//! The schema-aware mode: the schema context and `xsl:import-schema` (§3.14, §2.8, §3.13, §21.2).
//!
//! A schema-aware compilation is two steps (M4 plan D-1, adopted by the user 2026-10-19):
//!
//! 1. The caller builds a [`SchemaContext`] — it owns the `SchemaSet` — from a **pre-scan** of the
//!    stylesheet: [`SchemaContextBuilder::stylesheet_file`] (or `_uri`, `_str`) loads the module graph
//!    with the compiler's own [`Loader`] (so `use-when` and the include / import precedence are exactly
//!    the compiler's), collects every `xsl:import-schema`, keeps for each namespace only the
//!    declarations of the highest import precedence, and [`SchemaContextBuilder::build`] loads them
//!    into one schema set.
//! 2. `Compiler::schema_aware(&context, &functions)` compiles the same stylesheet with the set's own
//!    `NameTable` as its names (the XPath binder looks schema types up by the ids of its context's table)
//!    and the set in every XPath static context. It parses the module graph a second time — the "double
//!    parse" of the plan — and checks that every import it finds is one the context loaded.
//!
//! **Why the set first.** A `SchemaSet` owns its `NameTable` and is mutated while schemas load, so a
//! compiler that runs first cannot borrow it; the compiled stylesheet, every document of every run and
//! the set must share one table (DESIGN §6.6).
//!
//! **§3.14's semantics**, defined by a synthetic schema document of `xs:import`s:
//!
//! * by location — `schema-location` resolved against the declaration's base URI, read through the
//!   compiler's [`Resolver`]; a document that cannot be read is no error ("it is not intrinsically an
//!   error if no schema document can be located"), one that is not a valid schema is XTSE0220, and so
//!   is one whose target namespace is not the declaration's (the synthetic `xs:import` names it,
//!   src-import); a location of the XML or XLink namespace that cannot be read falls back to the
//!   copy of its schema this processor knows (below);
//! * by namespace only — the XML namespace's schema (`xml.xsd`) and XLink's, which `xsd-schema`
//!   embeds; the XML Schema instance namespace's built-in attribute declarations; any other
//!   namespace contributes nothing (host schemas are M4b's; the Schema for Schemas is not
//!   available, so the XML Schema namespace is not "found");
//! * inline — the `xs:schema` element is cut out of the module text by its source span (D-9, "the
//!   slice approach", proven on 517 WSDLs by SOAP S0), the in-scope namespace bindings it does not
//!   declare itself are written into its start tag on the same line, the elements of it that §3.12
//!   `use-when` excludes are blanked out, and it is loaded under the location `<base URI>#schema-N`
//!   (N from 1 in each module; a module reached twice has its inline schemas loaded once). Line
//!   numbers inside the slice are the module's.
//!
//! Every component of the set is in scope for the whole stylesheet (D-7: lenient — the engine has no
//! namespace filter, §3.13 allows implementation-defined additions); XTSE1520 and `type-available` read
//! the same set.
//!
//! **Locations.** `xsd-schema`'s resolver treats a `file:` base URI as a plain path (WAVE_B C15a), so a
//! `file:` URI is handed to it as the file-system path it names, and `ResolverLoader` turns a path
//! back into a `file:` URI before asking the compiler's resolver — C15a is avoided for `file:` URIs
//! only. Under any other base `xsd-schema` keeps the dot segments of a resolved directive
//! (`http://h/s/../x.xsd`); `ResolverLoader` removes them before asking the resolver, and a relative
//! location (a stylesheet named by a relative URI) is passed on relative. What stays upstream (C15a):
//! `xsd-schema` identifies a document by its own spelling of the location, so one non-`file:`
//! document reached under two spellings (`s/../x.xsd` and `x.xsd`) is loaded twice.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};
use xsd_schema::parser::resolver::{fixup_composition_edges, resolve_all_directives};
use xsd_schema::parser::SourceLocation;
use xsd_schema::{
    get_embedded_schema, has_embedded_schema, parse_schema_only, process_loaded_schemas,
    DocumentId, EmbeddedLoader, ResolverConfig, SchemaError, SchemaLoader, SchemaResolver,
    SchemaResult, SchemaSet, TypeKey,
};

use crate::error::{codes, Location, XsltError};
use crate::functions::XsltFunctions;
use crate::names::{XSD_NS, XSLT_NS};
use crate::receiver::Nav;
use crate::resolver::{self, FileResolver, Resolver};
use crate::stylesheet::{
    attr_local, base_uri_of, in_scope_namespaces, is_stripped_stylesheet_text, DeclKind, Loader,
    Module, ModuleSet,
};

/// The XML Schema instance namespace, whose four attributes are built into every schema set.
const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

// ── The context ──────────────────────────────────────────────────────────

/// The schema components a schema-aware stylesheet is compiled against (§3.13's in-scope schema
/// components), owned by the caller (M4 plan D-1).
///
/// The [`CompiledStylesheet`](crate::CompiledStylesheet) made from it borrows it, the same way it
/// borrows the name table in basic mode: the set's own [`NameTable`] *is* the stylesheet's table, so
/// every document a transformation builds uses it too.
///
/// ```
/// use std::sync::Arc;
/// use xslt2::{Compiler, MemoryResolver, SchemaContext, TransformOptions, XsltFunctions};
///
/// let sheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
///                    xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:h="urn:hats"
///                    exclude-result-prefixes="xs h">
///   <xsl:import-schema namespace="urn:hats">
///     <xs:schema targetNamespace="urn:hats">
///       <xs:simpleType name="size">
///         <xs:restriction base="xs:integer"><xs:maxInclusive value="10"/></xs:restriction>
///       </xs:simpleType>
///     </xs:schema>
///   </xsl:import-schema>
///   <xsl:template match="/"><out><xsl:value-of select="11 castable as h:size"/></out></xsl:template>
/// </xsl:stylesheet>"#;
/// let resolver = Arc::new(MemoryResolver::new().with("urn:sheet", sheet));
/// let functions = XsltFunctions::new();
/// let context = SchemaContext::builder()
///     .with_resolver(resolver.clone())
///     .stylesheet_uri("urn:sheet", &functions)?
///     .build()?;
/// let compiled = Compiler::schema_aware(&context, &functions)
///     .with_resolver(resolver)
///     .compile_uri("urn:sheet")?;
/// let arena = bumpalo::Bump::new();
/// let outcome = compiled
///     .transformation(&arena, TransformOptions::default())
///     .transform_source("urn:in", "<doc/>")?;
/// assert!(compiled.serialize_primary(&outcome)?.ends_with("<out>false</out>"));
/// # Ok::<(), xslt2::XsltError>(())
/// ```
pub struct SchemaContext {
    set: SchemaSet,
    imports: Vec<SchemaImport>,
}

impl std::fmt::Debug for SchemaContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaContext")
            .field("imports", &self.imports)
            .finish_non_exhaustive()
    }
}

impl Default for SchemaContext {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaContext {
    /// A context with the built-in types only (§3.13: "A schema-aware XSLT processor additionally
    /// supports: All other built-in types defined in \[XML Schema Part 2\]"), XSD 1.0 (M4 plan D-3).
    /// Enough for a stylesheet without `xsl:import-schema`.
    pub fn new() -> Self {
        Self {
            set: SchemaSet::new(),
            imports: Vec::new(),
        }
    }

    /// A builder that pre-scans a stylesheet for its `xsl:import-schema` declarations.
    pub fn builder() -> SchemaContextBuilder {
        SchemaContextBuilder {
            resolver: Arc::new(FileResolver),
            plans: Vec::new(),
        }
    }

    /// The compiled schema set.
    pub fn schema_set(&self) -> &SchemaSet {
        &self.set
    }

    /// The name table of the set — the one a schema-aware stylesheet, and every document of its
    /// transformations, uses.
    pub fn names(&self) -> &NameTable {
        &self.set.name_table
    }

    /// The `xsl:import-schema` declarations the context loaded: those that won their namespace by
    /// import precedence (§3.14), in the order of the module graph's precedence levels.
    pub fn imports(&self) -> &[SchemaImport] {
        &self.imports
    }

    /// Whether the context satisfies `planned`: it loaded an identical import (same namespace, same
    /// source), or `planned` names its namespace only and the context holds that namespace's
    /// components.
    ///
    /// §3.14: "The `namespace` attribute indicates that a schema for the given namespace is required
    /// by the stylesheet. This information may be enough on its own to enable an implementation to
    /// locate the required schema components" — the context is where this processor locates them
    /// (review R5). An import by location or an inline schema the context did not load is not
    /// satisfied by other documents of its namespace: they may define other components.
    fn covers(&self, planned: &PlannedImport) -> bool {
        let key = planned.key();
        self.imports.iter().any(|import| import.key == key)
            || (matches!(planned.source, Planned::NamespaceOnly)
                && self.holds_namespace(planned.namespace.as_deref()))
    }

    /// Whether the set has components in `namespace`: the built-in ones of the XML Schema and XML
    /// Schema instance namespaces, or a loaded schema document with that target namespace.
    fn holds_namespace(&self, namespace: Option<&str>) -> bool {
        if matches!(namespace, Some(XSD_NS | XSI_NS)) {
            return true;
        }
        let id = match namespace {
            Some(ns) => match self.set.name_table.get(ns) {
                Some(id) => Some(id),
                None => return false,
            },
            None => None,
        };
        self.set
            .documents
            .iter()
            .any(|document| document.target_namespace == id)
    }
}

/// One `xsl:import-schema` declaration a [`SchemaContext`] loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaImport {
    namespace: Option<String>,
    source: ImportSource,
    declared_at: Location,
    found: bool,
    key: ImportKey,
}

impl SchemaImport {
    /// The namespace imported: the `namespace` attribute, or the target namespace of the inline
    /// schema (§3.14's third combination); `None` for no namespace.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// Where the schema document came from.
    pub fn source(&self) -> &ImportSource {
        &self.source
    }

    /// The location of the declaration in the stylesheet.
    pub fn declared_at(&self) -> &Location {
        &self.declared_at
    }

    /// Whether a schema document was found: always for an inline schema; for a location, whether the
    /// resolver could read it (or, for the XML and XLink namespaces, the known copy was used); for a
    /// namespace-only import, whether this processor knows a schema for the namespace — the XML and
    /// XLink namespaces' and the XML Schema instance namespace's, not the Schema for Schemas. §3.14:
    /// "it is not intrinsically an error if no schema document can be located".
    pub fn found(&self) -> bool {
        self.found
    }
}

/// Where the schema document of an import comes from (§3.14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportSource {
    /// `schema-location`, resolved against the base URI of the declaration.
    Location(String),
    /// An inline `xs:schema`, loaded under `system_id` (`<base URI>#schema-N`, N counted from 1 in
    /// each module, in document order; M4 plan D-9).
    Inline { system_id: String },
    /// Neither: the namespace alone.
    NamespaceOnly,
}

/// What identifies an import for [`SchemaContext::covers`]: the namespace and the source, an inline
/// schema by its text as well.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ImportKey {
    namespace: Option<String>,
    source: ImportSource,
    inline_text: Option<String>,
}

/// Pre-scans stylesheets and builds a [`SchemaContext`] (see the module documentation).
pub struct SchemaContextBuilder {
    resolver: Arc<dyn Resolver>,
    plans: Vec<PlannedImport>,
}

impl std::fmt::Debug for SchemaContextBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaContextBuilder")
            .field("imports", &self.plans.len())
            .finish_non_exhaustive()
    }
}

impl SchemaContextBuilder {
    /// Where stylesheet modules and schema documents are read from — the resolver the stylesheet
    /// will be compiled with (default: [`FileResolver`]).
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Pre-scans the stylesheet whose principal module is at `path`.
    pub fn stylesheet_file(
        self,
        path: &Path,
        functions: &XsltFunctions,
    ) -> Result<Self, XsltError> {
        let uri = resolver::path_to_uri(path);
        self.stylesheet_uri(&uri, functions)
    }

    /// Pre-scans the stylesheet whose principal module the resolver serves at `uri`.
    pub fn stylesheet_uri(self, uri: &str, functions: &XsltFunctions) -> Result<Self, XsltError> {
        self.prescan(uri, None, functions)
    }

    /// Pre-scans the stylesheet whose principal module is `text`, named `base_uri` — as
    /// `Compiler::compile_str` will compile it.
    pub fn stylesheet_str(
        self,
        text: &str,
        base_uri: &str,
        functions: &XsltFunctions,
    ) -> Result<Self, XsltError> {
        self.prescan(base_uri, Some(text.to_string()), functions)
    }

    fn prescan(
        mut self,
        uri: &str,
        text: Option<String>,
        functions: &XsltFunctions,
    ) -> Result<Self, XsltError> {
        // The pre-scan's own arena and name table die here: only owned data leaves.
        let arena = Bump::new();
        let names = NameTable::new();
        let modules = Loader::new(&arena, &names, self.resolver.as_ref(), functions)
            .schema_aware(true)
            .load(uri, text)?;
        self.plans.extend(plan(&modules)?);
        Ok(self)
    }

    /// Loads the imports into one schema set (XSD 1.0, D-3) and compiles it.
    ///
    /// > \[ERR XTSE0220\] It is a static error if the synthetic schema document does not satisfy the
    /// > constraints described in \[XML Schema Part 1\] (section 5.1, Errors in Schema Construction
    /// > and Structure). This includes, without loss of generality, conflicts such as multiple
    /// > definitions of the same name.
    pub fn build(self) -> Result<SchemaContext, XsltError> {
        Loading::new(Arc::clone(&self.resolver)).run(self.plans)
    }
}

// ── The pre-scan (§3.14) ─────────────────────────────────────────────────

/// One `xsl:import-schema` declaration that won its namespace.
pub(crate) struct PlannedImport {
    namespace: Option<String>,
    source: Planned,
    declared_at: Location,
    precedence: u32,
}

enum Planned {
    Location(String),
    Inline(InlineSchema),
    NamespaceOnly,
}

/// An inline `xs:schema`, cut out of the module text (D-9).
struct InlineSchema {
    /// `<base URI>#schema-N`, as the caller sees it.
    system_id: String,
    /// The slice with the injected bindings.
    text: String,
    /// The module and the position of the `<` of the `xs:schema` start tag.
    module_uri: String,
    line: u32,
    column: u32,
    /// Characters of the slice's first line before the injection point, and characters injected.
    inject_after: u32,
    injected: u32,
}

impl PlannedImport {
    fn key(&self) -> ImportKey {
        let (source, inline_text) = match &self.source {
            Planned::Location(uri) => (ImportSource::Location(uri.clone()), None),
            Planned::Inline(inline) => (
                ImportSource::Inline {
                    system_id: inline.system_id.clone(),
                },
                Some(inline.text.clone()),
            ),
            Planned::NamespaceOnly => (ImportSource::NamespaceOnly, None),
        };
        ImportKey {
            namespace: self.namespace.clone(),
            source,
            inline_text,
        }
    }
}

/// The `xsl:import-schema` declarations of a module graph that §3.14 uses:
///
/// > If two `xsl:import-schema` declarations specify the same namespace, or if both specify no
/// > namespace, then only the one with highest import precedence is used. If this leaves more than
/// > one, then all the declarations at the highest import precedence are used.
///
/// The namespace an inline schema specifies without a `namespace` attribute is its target namespace.
/// The order is the module graph's (precedence levels, lowest first; declaration order within).
///
/// A module reached twice — two `xsl:include`s of it, or an include and an import (§3.10.2 forbids
/// only a module including itself) — is two modules of the graph, but its declarations are the same
/// elements of the same resource: each is used once (review R4; loading one inline schema twice
/// would be "multiple definitions of the same name", XTSE0220).
pub(crate) fn plan(modules: &ModuleSet<'_>) -> Result<Vec<PlannedImport>, XsltError> {
    let mut found: Vec<(Option<DeclarationId>, PlannedImport)> = Vec::new();
    let mut numbering = InlineNumbering::default();
    for unit in &modules.units {
        for (module_id, nav, kind) in &unit.decls {
            if *kind != DeclKind::Declaration
                || nav.namespace_uri() != XSLT_NS
                || nav.local_name() != "import-schema"
            {
                continue;
            }
            let module = modules.module(*module_id);
            let id = DeclarationId::of(module, nav);
            found.push((
                id.clone(),
                declaration(module, nav, unit.precedence, id, &mut numbering)?,
            ));
        }
    }
    let mut highest: HashMap<Option<String>, u32> = HashMap::new();
    for (_, import) in &found {
        let entry = highest.entry(import.namespace.clone()).or_insert(0);
        *entry = (*entry).max(import.precedence);
    }
    found.retain(|(_, import)| highest[&import.namespace] == import.precedence);
    // The same declaration reached through two copies of its module, once.
    let mut seen: HashSet<DeclarationId> = HashSet::new();
    found.retain(|(id, _)| id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    Ok(found.into_iter().map(|(_, import)| import).collect())
}

/// What identifies one `xsl:import-schema` element whichever copy of its module it was reached
/// through: the module's resource (its URI, as module identity compares it) and the element's byte
/// offset in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DeclarationId {
    module: String,
    offset: usize,
}

impl DeclarationId {
    fn of(module: &Module<'_>, nav: &Nav<'_>) -> Option<Self> {
        let span = module.doc.source_span(nav.current_ref())?;
        Some(Self {
            module: resolver::resource_identity(&module.uri),
            offset: span.start,
        })
    }
}

/// The `N` of `#schema-N` (D-9): the inline schemas of each module are numbered from 1 in document
/// order, and one reached again (another copy of its module) keeps its number.
#[derive(Default)]
struct InlineNumbering {
    /// The last number given in each module, by module identity.
    counters: HashMap<String, u32>,
    /// The system id given to each declaration.
    given: HashMap<DeclarationId, String>,
    /// Every system id given, so that two modules whose inline schemas share a base URI
    /// (`xml:base`) still get distinct ones.
    used: HashSet<String>,
}

impl InlineNumbering {
    fn system_id(
        &mut self,
        declaration: Option<&DeclarationId>,
        module: &str,
        base: &str,
    ) -> String {
        if let Some(id) = declaration.and_then(|d| self.given.get(d)) {
            return id.clone();
        }
        let counter = self
            .counters
            .entry(resolver::resource_identity(module))
            .or_insert(0);
        *counter += 1;
        let mut n = *counter;
        let mut system_id = format!("{base}#schema-{n}");
        while !self.used.insert(system_id.clone()) {
            n += 1;
            system_id = format!("{base}#schema-{n}");
        }
        if let Some(declaration) = declaration {
            self.given.insert(declaration.clone(), system_id.clone());
        }
        system_id
    }
}

/// Reads one `xsl:import-schema` declaration: its attributes, its content, the slice of an inline
/// schema.
fn declaration(
    module: &Module<'_>,
    nav: &Nav<'_>,
    precedence: u32,
    id: Option<DeclarationId>,
    numbering: &mut InlineNumbering,
) -> Result<PlannedImport, XsltError> {
    let at = module.location(nav);
    let error = |code: &'static str, message: String| {
        XsltError::new(code, message).with_location(at.clone())
    };
    let namespace = attr_local(nav, "namespace").map(|v| crate::whitespace::trim(&v).to_string());
    // §3.14: "The zero-length string is not a valid namespace URI, and is therefore not a valid
    // value for the namespace attribute."
    if namespace.as_deref() == Some("") {
        return Err(error(
            codes::XTSE0020,
            "the namespace attribute of xsl:import-schema is a zero-length string, which is not a \
             namespace URI; omit it to import names in no namespace"
                .to_string(),
        ));
    }
    let location = attr_local(nav, "schema-location");
    let schema = inline_schema_element(module, nav)?;

    let (namespace, source) = match schema {
        Some(schema) => {
            if location.is_some() {
                return Err(error(
                    codes::XTSE0215,
                    "an xsl:import-schema that contains an xs:schema must not have a \
                     schema-location attribute"
                        .to_string(),
                ));
            }
            let target = attr_local(&schema, "targetNamespace");
            // §3.14's three permitted combinations: both absent, both present and equal, or the
            // namespace attribute absent and the target namespace present.
            let namespace = match (&namespace, &target) {
                (None, None) => None,
                (None, Some(target)) => Some(target.clone()),
                (Some(ns), Some(target)) if ns == target => Some(ns.clone()),
                (Some(ns), _) => {
                    return Err(error(
                        codes::XTSE0215,
                        format!(
                        "the namespace attribute {ns:?} of xsl:import-schema conflicts with the \
                             target namespace of the contained schema ({})",
                        match &target {
                            Some(t) => format!("{t:?}"),
                            None => "none".to_string(),
                        }
                    ),
                    ))
                }
            };
            let base = base_uri_of(&schema, &module.uri);
            let base = base.split('#').next().unwrap_or(&base).to_string();
            let system_id = numbering.system_id(id.as_ref(), &module.uri, &base);
            (
                namespace,
                Planned::Inline(slice(module, &schema, system_id)?),
            )
        }
        None => match location {
            Some(href) => {
                let base = base_uri_of(nav, &module.uri);
                let uri = resolver::resolve_uri(&base, crate::whitespace::trim(&href));
                (namespace, Planned::Location(uri))
            }
            None => (namespace, Planned::NamespaceOnly),
        },
    };
    Ok(PlannedImport {
        namespace,
        source,
        declared_at: at,
        precedence,
    })
}

/// The `xs:schema` child of an `xsl:import-schema`, if it has one.
///
/// §3.14's syntax box: `<!-- Content: xs:schema? -->`. Anything else — a second schema, another
/// element, an element named `schema` in another namespace, text that is not stripped (§4.2) — is
/// XTSE0010 (§3.4: "if the content of the element does not correspond to the content that is allowed
/// for the element").
pub(crate) fn inline_schema_element<'t>(
    module: &Module<'t>,
    nav: &Nav<'t>,
) -> Result<Option<Nav<'t>>, XsltError> {
    let mut schema = None;
    for child in module.children(nav) {
        match child.node_type() {
            DomNodeType::Element => {
                let is_schema = child.namespace_uri() == XSD_NS && child.local_name() == "schema";
                if !is_schema || schema.is_some() {
                    return Err(XsltError::new(
                        codes::XTSE0010,
                        format!(
                            "xsl:import-schema may contain one xs:schema element and nothing else, \
                             not {}",
                            if is_schema {
                                "a second xs:schema".to_string()
                            } else {
                                format!("the element {{{}}}{}", child.namespace_uri(), child.local_name())
                            }
                        ),
                    )
                    .with_location(module.location(&child)));
                }
                schema = Some(child);
            }
            kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => {
                return Err(XsltError::new(
                    codes::XTSE0010,
                    "xsl:import-schema may contain one xs:schema element and nothing else, not text",
                )
                .with_location(module.location(nav)));
            }
            _ => {}
        }
    }
    Ok(schema)
}

/// Cuts an inline `xs:schema` out of its module's text (D-9): the element's bytes, with the in-scope
/// namespace bindings it does not declare itself written into its start tag right after the element
/// name, on the same line — §3.14's "a document containing a copy of the `xs:schema` element", whose
/// in-scope namespaces a copy keeps.
///
/// §3.12: "A literal result element, or any other element within a stylesheet module that is not in
/// the XSLT namespace, may similarly carry an `xsl:use-when` attribute", and "When a node is
/// effectively excluded from a stylesheet module the stylesheet module has the same effect as if the
/// node were not there" — so every element of the schema that `use-when` excludes (the loader has
/// computed them, `Module::is_excluded`) is blanked out of the copy: each of its bytes but line ends
/// becomes a space, so that what follows keeps its line and column (review R1).
fn slice(
    module: &Module<'_>,
    schema: &Nav<'_>,
    system_id: String,
) -> Result<InlineSchema, XsltError> {
    let span = module
        .doc
        .source_span(schema.current_ref())
        .ok_or_else(|| XsltError::internal("an inline xs:schema has no source span"))?;
    let original = module.text.get(span.start..span.end).ok_or_else(|| {
        XsltError::internal("the source span of an inline xs:schema is outside its module")
    })?;
    let blanked = blank_excluded(module, schema, span.start, original)?;
    let raw = blanked.as_str();
    // The prefixes the element declares itself (the default namespace as "").
    let mut local: Vec<String> = Vec::new();
    let mut ns = schema.clone();
    if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
        loop {
            local.push(ns.local_name().to_string());
            if !ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                break;
            }
        }
    }
    let mut inject = String::new();
    for (prefix, uri) in in_scope_namespaces(schema) {
        if local.contains(&prefix) {
            continue;
        }
        let value = escape_attribute(&uri);
        if prefix.is_empty() {
            inject.push_str(&format!(" xmlns=\"{value}\""));
        } else {
            inject.push_str(&format!(" xmlns:{prefix}=\"{value}\""));
        }
    }
    // `<` and the element name: up to the first whitespace, `>` or `/`.
    let name_end = raw[1..]
        .find(|c: char| crate::whitespace::is_xml_whitespace(c) || c == '>' || c == '/')
        .map_or(raw.len(), |i| i + 1);
    let text = format!("{}{}{}", &raw[..name_end], inject, &raw[name_end..]);
    let (line, column) = module.line_column(span.start);
    Ok(InlineSchema {
        system_id,
        text,
        module_uri: module.uri.clone(),
        line,
        column,
        inject_after: raw[..name_end].chars().count() as u32,
        injected: inject.chars().count() as u32,
    })
}

/// `text` (the bytes of `schema`, which start at `start` in the module) with every descendant
/// element §3.12 excludes replaced by spaces, line ends kept.
fn blank_excluded(
    module: &Module<'_>,
    schema: &Nav<'_>,
    start: usize,
    text: &str,
) -> Result<String, XsltError> {
    let mut cuts: Vec<std::ops::Range<usize>> = Vec::new();
    // A raw walk: `Module::children` would skip exactly the elements wanted here. It never
    // descends into an excluded element, whose descendants go with it.
    let mut stack = vec![schema.clone()];
    while let Some(parent) = stack.pop() {
        let mut child = parent.clone();
        if !child.move_to_first_child() {
            continue;
        }
        loop {
            if child.node_type() == DomNodeType::Element {
                if module.is_excluded(&child) {
                    let cut = module
                        .doc
                        .source_span(child.current_ref())
                        .and_then(|span| {
                            Some(span.start.checked_sub(start)?..span.end.checked_sub(start)?)
                        })
                        .ok_or_else(|| {
                            XsltError::internal(
                                "an element of an inline xs:schema has no source span inside it",
                            )
                        })?;
                    cuts.push(cut);
                } else {
                    stack.push(child.clone());
                }
            }
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    if cuts.is_empty() {
        return Ok(text.to_string());
    }
    let mut bytes = text.as_bytes().to_vec();
    for cut in cuts {
        let range = bytes.get_mut(cut).ok_or_else(|| {
            XsltError::internal("an excluded element lies outside its inline xs:schema")
        })?;
        for byte in range {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    // Every byte of a multi-byte character is replaced, and a span starts and ends at a character
    // boundary (`<` … `>`), so the result is UTF-8.
    String::from_utf8(bytes)
        .map_err(|_| XsltError::internal("a blanked inline xs:schema is not UTF-8"))
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Checks that a schema context covers the imports of a module graph being compiled — that the
/// context was built from this stylesheet (D-1).
pub(crate) fn check_coverage(
    context: &SchemaContext,
    modules: &ModuleSet<'_>,
) -> Result<(), XsltError> {
    for planned in plan(modules)? {
        if !context.covers(&planned) {
            let what = match &planned.source {
                Planned::Location(uri) => format!("schema-location {uri:?}"),
                Planned::Inline(inline) => format!("the inline schema {}", inline.system_id),
                Planned::NamespaceOnly => "no schema document".to_string(),
            };
            return Err(XsltError::internal(format!(
                "the schema context was not built from this stylesheet: it has not loaded the \
                 xsl:import-schema of {} ({what}); build it with \
                 SchemaContext::builder().stylesheet_…() from the stylesheet being compiled",
                match &planned.namespace {
                    Some(ns) => format!("the namespace {ns:?}"),
                    None => "no namespace".to_string(),
                }
            ))
            .with_location(planned.declared_at.clone()));
        }
    }
    Ok(())
}

// ── Loading ──────────────────────────────────────────────────────────────

/// The location `xsd-schema` loads a URI under: the path a `file:` URI names (WAVE_B C15a), the URI
/// itself otherwise.
fn schema_location(uri: &str) -> String {
    let is_file = uri.starts_with("file:") || !resolver::has_scheme(uri);
    match resolver::uri_to_path(uri).filter(|_| is_file) {
        Some(path) => {
            let fragment = uri.split_once('#').map(|(_, f)| f);
            let path = path.to_string_lossy().into_owned();
            match fragment {
                Some(fragment) => format!("{path}#{fragment}"),
                None => path,
            }
        }
        None => uri.to_string(),
    }
}

/// A [`SchemaLoader`] over the compiler's [`Resolver`]: what `xsd-schema` reads for the directives
/// (`xs:include`, `xs:import`, `xs:redefine`) of the schemas the context loads.
#[derive(Debug)]
struct ResolverLoader {
    resolver: Arc<dyn Resolver>,
}

impl ResolverLoader {
    /// The URI to ask the resolver for, for a location `xsd-schema` resolved a directive to.
    ///
    /// The [`Resolver`] contract: "`uri` is always absolute: \[`resolve_uri`\] has already been
    /// applied". `xsd-schema` resolves a directive against its document's location itself
    /// (`resolve_relative_uri`), which is RFC 3986 for none of the three forms it can produce (review
    /// R3; WAVE_B C15a):
    ///
    /// * a URI with a scheme (an `http:` base, or any other non-`file:` one): its dot segments are
    ///   kept (`http://h/s/../x.xsd`) — removed here, RFC 3986 §5.2.4, as `resolve_uri` removes them;
    /// * an absolute path (a `file:` base, which this module hands `xsd-schema` as the path it names):
    ///   lexically normalized already — turned back into its `file:` URI;
    /// * a relative path (a stylesheet named by a relative URI, `resolve_uri`'s "base that is itself
    ///   relative"): already merged and normalized as `resolve_uri` would merge it — passed on as it
    ///   is, never made absolute against the process's working directory.
    fn request(location: &str) -> String {
        if resolver::has_scheme(location) {
            resolver::normalize_uri(location)
        } else if Path::new(location).is_absolute() {
            resolver::path_to_uri(Path::new(location))
        } else {
            location.to_string()
        }
    }
}

impl SchemaLoader for ResolverLoader {
    fn load(&self, location: &str) -> SchemaResult<String> {
        if location.starts_with("embedded://") {
            return EmbeddedLoader::new().load(location);
        }
        let uri = Self::request(location);
        self.resolver
            .resolve_text(&uri)
            .map_err(|why| SchemaError::resolution(format!("cannot read {uri}: {why}")))
    }

    fn can_load(&self, _location: &str) -> bool {
        true
    }
}

/// One run of [`SchemaContextBuilder::build`].
struct Loading {
    resolver: Arc<dyn Resolver>,
    set: SchemaSet,
    /// Where each inline schema sits in its module, by the location it was loaded under.
    inline: Vec<(String, InlineMap)>,
    /// The documents loaded for the declarations, by the location they were loaded under (what a
    /// schema error's location names), with the declaration that loaded each.
    loaded: HashMap<String, (DocumentId, Location)>,
}

/// How a line of an inline slice maps back to its module.
#[derive(Clone)]
struct InlineMap {
    module_uri: String,
    line: u32,
    column: u32,
    inject_after: u32,
    injected: u32,
}

impl InlineMap {
    fn map(&self, location: &SourceLocation) -> Location {
        let line = location.line as u32;
        let column = location.column as u32;
        let (line, column) = if line <= 1 {
            let column = if column > self.inject_after {
                column.saturating_sub(self.injected).max(self.inject_after)
            } else {
                column
            };
            (self.line, self.column + column.saturating_sub(1))
        } else {
            (self.line + line - 1, column)
        };
        Location {
            module_uri: Some(self.module_uri.clone()),
            line,
            column,
        }
    }
}

impl Loading {
    fn new(resolver: Arc<dyn Resolver>) -> Self {
        Self {
            resolver,
            set: SchemaSet::new(),
            inline: Vec::new(),
            loaded: HashMap::new(),
        }
    }

    fn run(mut self, plans: Vec<PlannedImport>) -> Result<SchemaContext, XsltError> {
        let mut imports = Vec::with_capacity(plans.len());
        let mut pending: Vec<DocumentId> = Vec::new();
        for planned in plans {
            let key = planned.key();
            let (source, found) = match planned.source {
                Planned::Location(ref uri) => {
                    let found = self.load_location(
                        uri,
                        &planned.namespace,
                        &planned.declared_at,
                        &mut pending,
                    )?;
                    (ImportSource::Location(uri.clone()), found)
                }
                Planned::Inline(ref inline) => {
                    self.load_inline(inline, &planned.declared_at, &mut pending)?;
                    (
                        ImportSource::Inline {
                            system_id: inline.system_id.clone(),
                        },
                        true,
                    )
                }
                Planned::NamespaceOnly => {
                    let found = self.load_namespace(
                        &planned.namespace,
                        &planned.declared_at,
                        &mut pending,
                    )?;
                    (ImportSource::NamespaceOnly, found)
                }
            };
            imports.push(SchemaImport {
                namespace: planned.namespace.clone(),
                source,
                declared_at: planned.declared_at.clone(),
                found,
                key,
            });
        }

        // What `SchemaSetBuilder::compile` does, with the set in hand: resolve the directives of
        // every document (depth first), fix the composition edges, and process the set. A directive
        // that cannot be read is not an error (it is a hint, as a location is); a document that is
        // read and is not a valid schema is.
        // Every location goes to the compiler's resolver, which decides what can be read — the
        // network included (`FileResolver` reads no `http:` URI); `xsd-schema`'s own network
        // switch would refuse an `http:` location before asking it.
        let config = ResolverConfig {
            allow_network: true,
            ..Default::default()
        };
        let mut resolver = SchemaResolver::with_config_and_loader(
            config,
            Box::new(ResolverLoader {
                resolver: Arc::clone(&self.resolver),
            }),
        );
        resolver.catalog_mut().add_xml_catalog();
        let mut errors: Vec<SchemaError> = Vec::new();
        for id in pending {
            self.resolve_directives(id, &mut resolver, &mut errors);
        }
        fixup_composition_edges(&mut self.set);
        if let Some(error) = errors
            .into_iter()
            .find(SchemaError::is_schema_content_error)
        {
            return Err(self.xtse0220(&error, None));
        }
        if let Err(error) = process_loaded_schemas(&mut self.set) {
            return Err(self.xtse0220(&error, None));
        }
        Ok(SchemaContext {
            set: self.set,
            imports,
        })
    }

    fn resolve_directives(
        &mut self,
        id: DocumentId,
        resolver: &mut SchemaResolver,
        errors: &mut Vec<SchemaError>,
    ) {
        let result = resolve_all_directives(id, resolver, &mut self.set);
        for loaded in result.loaded {
            self.resolve_directives(loaded, resolver, errors);
        }
        errors.extend(result.errors);
        errors.extend(result.import_errors);
    }

    /// An import by location: read through the resolver, parse, check the target namespace.
    fn load_location(
        &mut self,
        uri: &str,
        namespace: &Option<String>,
        at: &Location,
        pending: &mut Vec<DocumentId>,
    ) -> Result<bool, XsltError> {
        let location = schema_location(uri);
        let id = match self.loaded.get(&location).map(|(id, _)| *id) {
            Some(id) => id,
            None => {
                let text = match self.resolver.resolve_text(uri) {
                    Ok(text) => text,
                    // A namespace whose schema this processor knows (the XML namespace, XLink):
                    // the known copy, as for the namespace-only form and as `xsd-schema` does for an
                    // `xs:import` of it whose location cannot be read (its catalog) — §3.14: "The
                    // namespace attribute indicates that a schema for the given namespace is
                    // required by the stylesheet. This information may be enough on its own to
                    // enable an implementation to locate the required schema components" (review
                    // R6; `http://www.w3.org/2001/xml.xsd` is not read from the network).
                    Err(_) if namespace.as_deref().is_some_and(has_embedded_schema) => {
                        return self.load_namespace(namespace, at, pending);
                    }
                    // §3.14: "it is not intrinsically an error if no schema document can be located
                    // for a namespace identified in an xsl:import-schema declaration".
                    Err(_) => return Ok(false),
                };
                let id = parse_schema_only(text.as_bytes(), &location, &mut self.set)
                    .map_err(|e| self.xtse0220(&e, Some(at)))?;
                self.set.mark_loaded(location.clone(), id);
                self.loaded.insert(location, (id, at.clone()));
                pending.push(id);
                id
            }
        };
        // The synthetic schema document's `xs:import` carries the declaration's namespace, so the
        // document found must have that target namespace (src-import; XML Schema 1.0 §4.2.3, 1.1
        // §4.2.6.2), and none when the declaration names none.
        let target = self.set.documents[id as usize]
            .target_namespace
            .map(|ns| self.set.name_table.resolve(ns));
        if target != *namespace {
            let describe = |ns: &Option<String>| match ns {
                Some(ns) => format!("the namespace {ns:?}"),
                None => "no namespace".to_string(),
            };
            return Err(XsltError::new(
                codes::XTSE0220,
                format!(
                    "the schema document {uri} has {} as its target namespace, but the \
                     xsl:import-schema that imports it names {}",
                    describe(&target),
                    describe(namespace)
                ),
            )
            .with_location(at.clone()));
        }
        Ok(true)
    }

    /// An inline schema: the slice, under its own location.
    fn load_inline(
        &mut self,
        inline: &InlineSchema,
        at: &Location,
        pending: &mut Vec<DocumentId>,
    ) -> Result<(), XsltError> {
        let location = schema_location(&inline.system_id);
        self.inline.push((
            location.clone(),
            InlineMap {
                module_uri: inline.module_uri.clone(),
                line: inline.line,
                column: inline.column,
                inject_after: inline.inject_after,
                injected: inline.injected,
            },
        ));
        let id = parse_schema_only(inline.text.as_bytes(), &location, &mut self.set)
            .map_err(|e| self.xtse0220(&e, Some(at)))?;
        self.set.mark_loaded(location.clone(), id);
        self.loaded.insert(location, (id, at.clone()));
        pending.push(id);
        Ok(())
    }

    /// A namespace-only import: the schemas `xsd-schema` embeds (the XML namespace, XLink); the
    /// built-in attribute declarations of the XML Schema instance namespace; nothing for any other
    /// namespace (host schemas are M4b's).
    ///
    /// The XML Schema namespace is "found" only with the Schema for Schemas (§3.13's Note: "A
    /// stylesheet that is designed to process XML Schema documents as its input or output may import
    /// the Schema for Schemas"), which this processor does not have: its built-in types are in every
    /// set whether imported or not, its element declarations (`schema-element(xs:schema)`) in none
    /// (review R6).
    fn load_namespace(
        &mut self,
        namespace: &Option<String>,
        at: &Location,
        pending: &mut Vec<DocumentId>,
    ) -> Result<bool, XsltError> {
        let Some(namespace) = namespace else {
            return Ok(false);
        };
        if namespace == XSD_NS {
            return Ok(false);
        }
        if namespace == XSI_NS {
            return Ok(true);
        }
        let Some(bytes) = get_embedded_schema(namespace) else {
            return Ok(false);
        };
        let location = match namespace.as_str() {
            xsd_schema::XML_NAMESPACE => "embedded://xml.xsd",
            _ => "embedded://xlink.xsd",
        };
        if self.loaded.contains_key(location) {
            return Ok(true);
        }
        let id = parse_schema_only(bytes, location, &mut self.set)
            .map_err(|e| self.xtse0220(&e, Some(at)))?;
        self.set.mark_loaded(location.to_string(), id);
        self.loaded.insert(location.to_string(), (id, at.clone()));
        pending.push(id);
        Ok(true)
    }

    /// XTSE0220 for a schema error, at the line of an inline schema in its module when the error is
    /// in one, else at the declaration that loaded the document (or `at`).
    fn xtse0220(&self, error: &SchemaError, at: Option<&Location>) -> XsltError {
        let located = schema_error_location(error);
        let mapped = located.as_ref().and_then(|l| {
            self.inline
                .iter()
                .find(|(location, _)| *location == l.base_uri)
                .map(|(_, map)| map.map(l))
        });
        let message = format!("the imported schema is not valid: {error}");
        let error = XsltError::new(codes::XTSE0220, message);
        match mapped {
            Some(location) => error.with_location(location),
            None => {
                let declared = located
                    .as_ref()
                    .and_then(|l| self.loaded.get(&l.base_uri))
                    .map(|(_, at)| at);
                match declared.or(at) {
                    Some(location) => error.with_location(location.clone()),
                    None => error,
                }
            }
        }
    }
}

/// The source location a schema error carries, if any.
fn schema_error_location(error: &SchemaError) -> Option<SourceLocation> {
    match error {
        SchemaError::XmlError { location, .. }
        | SchemaError::StructuralError { location, .. }
        | SchemaError::NamespaceError { location, .. }
        | SchemaError::FeatureError { location, .. } => location.clone(),
        _ => None,
    }
}

// ── Types in scope (§3.13, §18.1.4, §19.2.1.2) ───────────────────────────

/// What kind of type definition a name denotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeKind {
    Simple,
    Complex,
}

/// The XSD built-in types a schema-aware processor has (§3.13, "All other built-in types defined in
/// \[XML Schema Part 2\]"), with the five \[XPath 2.0\] adds and whether each is simple; `None` for a
/// name that is not one. XSD 1.0's list (D-3).
pub(crate) fn built_in_type(local: &str) -> Option<TypeKind> {
    Some(match local {
        "anyType" | "untyped" => TypeKind::Complex,
        "anySimpleType" | "anyAtomicType" | "untypedAtomic" | "yearMonthDuration"
        | "dayTimeDuration" | "string" | "normalizedString" | "token" | "language" | "NMTOKEN"
        | "NMTOKENS" | "Name" | "NCName" | "ID" | "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES"
        | "boolean" | "decimal" | "float" | "double" | "integer" | "nonPositiveInteger"
        | "negativeInteger" | "long" | "int" | "short" | "byte" | "nonNegativeInteger"
        | "unsignedLong" | "unsignedInt" | "unsignedShort" | "unsignedByte" | "positiveInteger"
        | "duration" | "dateTime" | "time" | "date" | "gYearMonth" | "gYear" | "gMonthDay"
        | "gDay" | "gMonth" | "hexBinary" | "base64Binary" | "anyURI" | "QName" | "NOTATION" => {
            TypeKind::Simple
        }
        _ => return None,
    })
}

/// Whether a built-in type is atomic and instantiable, so that it has a constructor function (XPath
/// 2.0 §3.10.4: "For every atomic type in the in-scope schema types (except `xs:NOTATION` and
/// `xs:anyAtomicType`, which are not instantiable), a constructor function is implicitly defined").
pub(crate) fn built_in_has_constructor(local: &str) -> bool {
    built_in_type(local) == Some(TypeKind::Simple)
        && !matches!(
            local,
            "anySimpleType" | "anyAtomicType" | "NOTATION" | "NMTOKENS" | "IDREFS" | "ENTITIES"
        )
}

/// The type the expanded name `{uri}local` denotes in the in-scope schema types of a schema-aware
/// stylesheet compiled against `set`: a built-in one, or one of the set (D-7: the whole set).
pub(crate) fn in_scope_type(set: &SchemaSet, uri: &str, local: &str) -> Option<TypeKind> {
    if uri == XSD_NS {
        return built_in_type(local);
    }
    let key = lookup_type(set, uri, local)?;
    Some(match key {
        TypeKey::Simple(_) => TypeKind::Simple,
        TypeKey::Complex(_) => TypeKind::Complex,
    })
}

/// Whether `{uri}local` names an atomic type of `set` outside the XML Schema namespace — one with a
/// constructor function (XPath 2.0 §3.10.4).
pub(crate) fn has_user_constructor(set: &SchemaSet, uri: &str, local: &str) -> bool {
    match lookup_type(set, uri, local) {
        Some(TypeKey::Simple(key)) => set.arenas.simple_types.get(key).is_some_and(|def| {
            def.variety == xsd_schema::parser::frames::SimpleTypeVariety::Atomic
        }),
        _ => false,
    }
}

fn lookup_type(set: &SchemaSet, uri: &str, local: &str) -> Option<TypeKey> {
    let names = &set.name_table;
    let ns = if uri.is_empty() {
        None
    } else {
        Some(names.get(uri)?)
    };
    set.lookup_type(ns, names.get(local)?)
}
