//! The schema-aware mode: the schema context and `xsl:import-schema` (§3.14, §2.8, §3.13, §21.2).
//!
//! A schema-aware compilation is two steps (M4 plan D-1, adopted by the user 2026-10-19):
//!
//! 1. The caller builds a [`SchemaContext`] — it owns the `SchemaSet` — from a **pre-scan** of the
//!    stylesheet: [`SchemaContextBuilder::stylesheet_file`] (or `_uri`, `_str`) loads the module graph
//!    with the compiler's own [`Loader`] (so `use-when` and the include / import precedence are exactly
//!    the compiler's), collects every `xsl:import-schema`, keeps for each namespace only the
//!    declarations of the highest import precedence, and [`SchemaContextBuilder::build`] loads them
//!    into one schema set — together with the schemas the host adds
//!    ([`SchemaContextBuilder::schema_uri`], `schema_file`, `schema_str`; §3.13: "An implementation
//!    may define mechanisms that allow additional schema components to be added to the in-scope
//!    schema components for the stylesheet"), against which source documents are validated (M4b,
//!    D-2: one set per compiled stylesheet).
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
//!   src-import) — unless the set already holds a schema document of the declared namespace (a
//!   host schema's, or an earlier import's): the location is then a hint that did not lead to the
//!   namespace's schema and is passed over, `found()` false (M4g, decision D-g11; XML Schema 1.1
//!   Part 1 §4.2.6.2: "it is open to applications to ignore all but the first `<import>` for a given
//!   namespace, regardless of the ·actual value· of schemaLocation"). The pass-over is
//!   order-dependent — an earlier import counts only when it was loaded first — and a document an
//!   earlier import's own `xs:import` brings in does not count (directives are resolved after every
//!   declaration is loaded); each location passed over, and each that cannot be read, is a
//!   [`SchemaContext::warnings`] entry. A location of the XML or XLink namespace that cannot be read
//!   falls back to the copy of its schema this processor knows (below);
//! * by namespace only — the XML namespace's schema (`xml.xsd`) and XLink's, which `xsd-schema`
//!   embeds; the XML Schema instance namespace's built-in attribute declarations; any other
//!   namespace is found when a host schema (or a document one of the set's directives loaded) has it
//!   as its target namespace, and contributes nothing otherwise (the Schema for Schemas is not
//!   available, so the XML Schema namespace is not "found" unless a host gives it);
//! * inline — the `xs:schema` element is cut out of the module text by its source span (D-9, "the
//!   slice approach", proven on 517 WSDLs by SOAP S0), the in-scope namespace bindings it does not
//!   declare itself are written into its start tag on the same line, the elements of it that §3.12
//!   `use-when` excludes are blanked out, and it is loaded under the location `<base URI>#schema-N`
//!   (N from 1 in each module; a module reached twice has its inline schemas loaded once). Line
//!   numbers inside the slice are the module's.
//!
//! Every component of the set is in scope for the whole stylesheet (D-7: lenient — the engine has no
//! namespace filter, §3.13 allows implementation-defined additions); XTSE1520 and `type-available` read
//! the same set. So the transitive-import rule of §3.14 ("These rules do not cause names to be
//! imported transitively … The stylesheet must import all the namespaces containing names that it
//! actually references") is not enforced: a name a schema document of the set defines is usable
//! although no `xsl:import-schema` names its namespace — an implementation-defined addition to the
//! in-scope schema components (§3.13), documented, never an error a conforming stylesheet meets.
//!
//! **XML Schema version** (M4g, D-3): 1.0 unless [`SchemaContextBuilder::xsd_version`] asks for 1.1,
//! for every schema document of the context alike.
//!
//! **Locations.** A `file:` URI is handed to `xsd-schema` as the file-system path it names
//! (`schema_location`), so that every location the context reports — in a schema error, a warning,
//! an inline schema's `<base URI>#schema-N` — is spelled one way, as a path; `ResolverLoader` turns a
//! path back into a `file:` URI before asking the compiler's resolver. Since `wave-b` `98ab11a`
//! (C15a) `xsd-schema` resolves every directive by RFC 3986 and identifies a document however its
//! location is spelled (`s/../x.xsd` and `x.xsd` are one document); it reads `file:` URIs too, so the
//! path form is a choice of spelling, no longer a workaround. A location with a scheme reaches
//! `ResolverLoader` resolved and normalized, and a relative one (a stylesheet named by a relative
//! URI) relative.

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
    SchemaResult, SchemaSet, TypeKey, XsdVersion,
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
    /// The locations of the host schemas, in the order they were given.
    hosts: Vec<String>,
    /// See [`SchemaContext::warnings`].
    warnings: Vec<String>,
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
        Self::with_version(XsdVersion::V1_0)
    }

    /// A context with the built-in types of XML Schema `version` only — see
    /// [`SchemaContextBuilder::xsd_version`] for what XML Schema 1.1 changes.
    pub fn with_version(version: XsdVersion) -> Self {
        Self {
            set: SchemaSet::with_version(version),
            imports: Vec::new(),
            hosts: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// A builder that pre-scans a stylesheet for its `xsl:import-schema` declarations.
    pub fn builder() -> SchemaContextBuilder {
        SchemaContextBuilder {
            resolver: Arc::new(FileResolver),
            plans: Vec::new(),
            hosts: Vec::new(),
            version: XsdVersion::V1_0,
        }
    }

    /// The version of XML Schema the context's set implements: 1.0 unless the builder was asked for
    /// 1.1 ([`SchemaContextBuilder::xsd_version`]).
    pub fn xsd_version(&self) -> XsdVersion {
        self.set.xsd_version
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

    /// The locations of the schema documents the host added to the context
    /// ([`SchemaContextBuilder::schema_uri`], `schema_file`, `schema_str`), in the order first given,
    /// each once.
    pub fn host_schemas(&self) -> &[String] {
        &self.hosts
    }

    /// The `xsl:import-schema` declarations the context loaded: those that won their namespace by
    /// import precedence (§3.14), in the order of the module graph's precedence levels.
    pub fn imports(&self) -> &[SchemaImport] {
        &self.imports
    }

    /// What building the context did on its own that the host may want to know, in the order it
    /// happened: each `schema-location` it did not load from — one that could not be read (§3.14:
    /// "it is not intrinsically an error if no schema document can be located"), and one passed
    /// over because the set already held a schema document of the declared namespace and the
    /// document at the location has another target namespace (decision D-g11; Structures 1.1
    /// §4.2.6.2 permits ignoring such a hint, "but such a strategy risks missing useful information
    /// when new schemaLocations are offered"). The import's [`SchemaImport::found`] is then false.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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
        matches!(namespace, Some(XSD_NS | XSI_NS)) || self.holds_loaded_namespace(namespace)
    }

    /// Whether a schema document with the target namespace `namespace` is loaded in the set.
    fn holds_loaded_namespace(&self, namespace: Option<&str>) -> bool {
        has_document_of(&self.set, namespace)
    }
}

/// The `targetNamespace` of the schema document `text` (`Some(None)`: none), read off its document
/// element without loading it into a set; `None` when it does not parse.
fn target_namespace_of(text: &str) -> Option<Option<String>> {
    let arena = Bump::new();
    let names = NameTable::new();
    let doc =
        xsd_schema::document::BufferDocument::from_reader_default(text.as_bytes(), &arena, &names)
            .ok()?;
    let mut nav = doc.create_navigator();
    if !nav.move_to_first_child() {
        return None;
    }
    while nav.node_type() != DomNodeType::Element {
        if !nav.move_to_next_sibling() {
            return None;
        }
    }
    if nav.move_to_first_attribute() {
        loop {
            if nav.local_name() == "targetNamespace" && nav.namespace_uri().is_empty() {
                return Some(Some(nav.value()).filter(|ns| !ns.is_empty()));
            }
            if !nav.move_to_next_attribute() {
                break;
            }
        }
    }
    Some(None)
}

/// ` at <module>:<line>:<column>` for a warning about the declaration at `at`.
fn declared_at(at: &Location) -> String {
    match &at.module_uri {
        Some(module) => format!(" at {module}:{}:{}", at.line, at.column),
        None => String::new(),
    }
}

/// Whether `set` holds a parsed schema document whose target namespace is `namespace`.
fn has_document_of(set: &SchemaSet, namespace: Option<&str>) -> bool {
    let id = match namespace {
        Some(ns) => match set.name_table.get(ns) {
            Some(id) => Some(id),
            None => return false,
        },
        None => None,
    };
    set.documents
        .iter()
        .any(|document| document.target_namespace == id)
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
    hosts: Vec<HostSchema>,
    version: XsdVersion,
}

/// A schema document the host adds to the context (§3.13's implementation-defined mechanism).
enum HostSchema {
    /// Read through the resolver at this (absolute) URI.
    Uri(String),
    /// Given as text, loaded under `uri`.
    Text { text: String, uri: String },
}

impl HostSchema {
    fn location(&self) -> &str {
        match self {
            HostSchema::Uri(uri) | HostSchema::Text { uri, .. } => uri,
        }
    }
}

impl std::fmt::Debug for SchemaContextBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaContextBuilder")
            .field("imports", &self.plans.len())
            .field("host_schemas", &self.hosts.len())
            .field("xsd_version", &self.version)
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

    /// The version of XML Schema the context's one set implements (M4 plan D-3: one version per
    /// context; default [`XsdVersion::V1_0`]).
    ///
    /// XSLT 2.0 refers to XML Schema 1.0 — its normative references name "XML Schema Part 1:
    /// Structures Second Edition" and "XML Schema Part 2: Datatypes Second Edition" —, so 1.0 is the
    /// default. [`XsdVersion::V1_1`] is an implementation-defined extension, one of the mechanisms
    /// §3.13 allows ("An implementation may define mechanisms that allow additional schema
    /// components to be added to the in-scope schema components for the stylesheet"): the set is
    /// built as an XML Schema 1.1 set (`SchemaSet::xsd11()`), so
    ///
    /// - every schema document the context loads — imported by location, by namespace, inline, or
    ///   a host schema — is read as an XML Schema 1.1 document (`xs:assert`, `xs:alternative`,
    ///   `xs:openContent`, … are accepted), and a 1.0 document as 1.1 reads it;
    /// - validation, of source documents (`TransformOptions::source_validation`) and of constructed
    ///   nodes (`[xsl:]validation`, `[xsl:]type`), is XML Schema 1.1 assessment, assertions
    ///   included (`ValidationFlags::PROCESS_ASSERTIONS`);
    /// - the built-in types are XML Schema 1.1's, which define three of the XPath 2.0 types of
    ///   §3.13 (`xs:anyAtomicType`, `xs:dayTimeDuration`, `xs:yearMonthDuration`) and add
    ///   `xs:dateTimeStamp`. (`[xsl:]type` may name all four XPath 2.0 types in either version:
    ///   upstream has them, `xs:untypedAtomic` included, as components of every set —
    ///   `SchemaSet::xpath_type`.)
    ///
    /// The XPath engine is the same in both versions.
    pub fn xsd_version(mut self, version: XsdVersion) -> Self {
        self.version = version;
        self
    }

    /// Adds the schema document the resolver serves at `uri` (an absolute URI) to the context.
    ///
    /// §3.13: "An implementation may define mechanisms that allow additional schema components to be
    /// added to the in-scope schema components for the stylesheet" — this is that mechanism (M4 plan
    /// D-2): the host's schemas join the stylesheet's imports in the **one** schema set of the
    /// context, against which the stylesheet is compiled and every source document is validated
    /// (`TransformOptions::source_validation`). Their components are in scope for the whole
    /// stylesheet (D-7). Host schemas are loaded before the stylesheet's imports: an
    /// `xsl:import-schema` by namespace only of a namespace a host schema supplies is `found()`, and
    /// one by location of a document a host schema already loaded (the same URI) loads it once.
    ///
    /// The document is read when the context is built ([`build`](Self::build)): one that cannot be
    /// read, or is not a valid schema, is XTSE0220 there (the in-scope schema components cannot be
    /// assembled). Its `xs:include` / `xs:import` / `xs:redefine` directives are followed through
    /// the resolver, as for an imported schema.
    pub fn schema_uri(mut self, uri: &str) -> Self {
        self.hosts
            .push(HostSchema::Uri(resolver::normalize_uri(uri)));
        self
    }

    /// Adds the schema document in the file at `path` (see [`schema_uri`](Self::schema_uri)).
    pub fn schema_file(self, path: &Path) -> Self {
        let uri = resolver::path_to_uri(path);
        self.schema_uri(&uri)
    }

    /// Adds the schema document `text`, loaded under the location `uri` — the base its directives
    /// resolve against (see [`schema_uri`](Self::schema_uri)).
    pub fn schema_str(mut self, text: &str, uri: &str) -> Self {
        self.hosts.push(HostSchema::Text {
            text: text.to_string(),
            uri: resolver::normalize_uri(uri),
        });
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

    /// Loads the imports into one schema set (XSD 1.0 unless [`xsd_version`](Self::xsd_version)
    /// says otherwise, D-3) and compiles it.
    ///
    /// > \[ERR XTSE0220\] It is a static error if the synthetic schema document does not satisfy the
    /// > constraints described in \[XML Schema Part 1\] (section 5.1, Errors in Schema Construction
    /// > and Structure). This includes, without loss of generality, conflicts such as multiple
    /// > definitions of the same name.
    pub fn build(self) -> Result<SchemaContext, XsltError> {
        Loading::new(Arc::clone(&self.resolver), self.version).run(self.hosts, self.plans)
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

/// The location `xsd-schema` loads a URI under: the path a `file:` URI names, the URI itself
/// otherwise — the one spelling of the context's locations in its diagnostics (see the module
/// docs; until `wave-b` `98ab11a` it also kept `xsd-schema` from treating a `file:` base as a
/// path, C15a).
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
    /// applied". `xsd-schema` resolves a directive against its document's location itself, by RFC
    /// 3986 §5.2 since `wave-b` `98ab11a` (C15a; review R3 of M4a found it was not), in one of three
    /// forms:
    ///
    /// * a URI with a scheme (an `http:` base, or any other non-`file:` one): resolved and
    ///   normalized (§5.2.4's dot segments removed, §6.2.2 / §6.2.3) — passed on as it is (until
    ///   the Wave B follow-up this crate removed the dot segments itself);
    /// * an absolute path (a `file:` base, which this module hands `xsd-schema` as the path it names,
    ///   [`schema_location`]): lexically normalized — turned back into its `file:` URI;
    /// * a relative path (a stylesheet named by a relative URI, `resolve_uri`'s "base that is itself
    ///   relative"): merged and normalized as `resolve_uri` would merge it — passed on as it is
    ///   (`xsd-schema` no longer makes it absolute against the process's working directory either).
    fn request(location: &str) -> String {
        if resolver::has_scheme(location) {
            location.to_string()
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
    /// The documents loaded for the declarations and the host schemas, by the location they were
    /// loaded under (what a schema error's location names), with the declaration that loaded each
    /// (`None` for a host schema).
    loaded: HashMap<String, (DocumentId, Option<Location>)>,
    /// [`SchemaContext::warnings`].
    warnings: Vec<String>,
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
    fn new(resolver: Arc<dyn Resolver>, version: XsdVersion) -> Self {
        Self {
            resolver,
            set: SchemaSet::with_version(version),
            inline: Vec::new(),
            loaded: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    fn run(
        mut self,
        hosts: Vec<HostSchema>,
        plans: Vec<PlannedImport>,
    ) -> Result<SchemaContext, XsltError> {
        let mut imports = Vec::with_capacity(plans.len());
        let mut pending: Vec<DocumentId> = Vec::new();
        // The host's schemas first (§3.13's implementation-defined mechanism), so that the
        // stylesheet's imports find what they supply.
        let mut host_locations = Vec::with_capacity(hosts.len());
        for host in &hosts {
            self.load_host(host, &mut pending)?;
            // A schema given twice is loaded once (`load_host`) and listed once.
            if !host_locations.iter().any(|known| known == host.location()) {
                host_locations.push(host.location().to_string());
            }
        }
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
        // A namespace-only import is found when the set holds a schema document of its namespace by
        // now — a host schema's, or one a host schema's (or another import's) directives loaded.
        let mut context = SchemaContext {
            set: self.set,
            imports,
            hosts: host_locations,
            warnings: self.warnings,
        };
        for index in 0..context.imports.len() {
            let import = &context.imports[index];
            if import.found || import.source != ImportSource::NamespaceOnly {
                continue;
            }
            let held = context.holds_loaded_namespace(import.namespace.as_deref());
            context.imports[index].found = held;
        }
        Ok(context)
    }

    /// A host schema: read (or given), parsed, its directives resolved with the rest.
    fn load_host(
        &mut self,
        host: &HostSchema,
        pending: &mut Vec<DocumentId>,
    ) -> Result<(), XsltError> {
        let location = schema_location(host.location());
        if self.loaded.contains_key(&location) {
            return Ok(());
        }
        let text = match host {
            HostSchema::Text { text, .. } => text.clone(),
            HostSchema::Uri(uri) => self.resolver.resolve_text(uri).map_err(|why| {
                XsltError::new(
                    codes::XTSE0220,
                    format!("cannot read the schema document {uri} the host added: {why}"),
                )
            })?,
        };
        let id = parse_schema_only(text.as_bytes(), &location, &mut self.set)
            .map_err(|e| self.xtse0220(&e, None))?;
        self.set.mark_loaded(location.clone(), id);
        self.loaded.insert(location, (id, None));
        pending.push(id);
        Ok(())
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
                    // for a namespace identified in an xsl:import-schema declaration" — said, though.
                    Err(why) => {
                        self.warnings.push(format!(
                            "xsl:import-schema{}: the schema document at {uri} could not be read \
                             ({why}), so nothing was loaded from it",
                            declared_at(at)
                        ));
                        return Ok(false);
                    }
                };
                // A hint that does not lead to the namespace's schema (M4g, decision D-g11): when the
                // set already holds a schema document of the declared namespace — a host schema's
                // (§3.13's mechanism, loaded first), or an earlier import's — a document of another
                // target namespace at the location is passed over (`found()` false), not XTSE0220.
                // §3.14: "The schema-location attribute is a URI Reference that gives a hint
                // indicating where a schema document … may be found"; XML Schema 1.1 Part 1
                // §4.2.6.2: "It is not an error for the application schema component reference
                // strategy to fail", and "Given that the schemaLocation [attribute] is only a
                // hint, it is open to applications to ignore all but the first <import> for a
                // given namespace, regardless of the ·actual value· of schemaLocation". Where
                // nothing else supplies the namespace the document is the import's referent, and
                // its other target namespace stays XTSE0220 (clause 3.1, below).
                //
                // What this leaves out (documented, R8): the rule is order-dependent — an earlier
                // import counts only when it was loaded first —, and a document an earlier import's
                // own `xs:import` brings in does not count (the directives are resolved after every
                // declaration is loaded). The pass-over is said in a warning.
                if has_document_of(&self.set, namespace.as_deref()) {
                    if let Some(found) = target_namespace_of(&text) {
                        if found != *namespace {
                            let describe = |ns: &Option<String>| match ns {
                                Some(ns) => format!("the namespace {ns:?}"),
                                None => "no namespace".to_string(),
                            };
                            self.warnings.push(format!(
                                "xsl:import-schema{}: the document at {uri} has {} as its target \
                                 namespace, but the declaration names {}, of which the schema set \
                                 already holds a schema document; the location was passed over, \
                                 and nothing was loaded from it",
                                declared_at(at),
                                describe(&found),
                                describe(namespace)
                            ));
                            return Ok(false);
                        }
                    }
                }
                let id = parse_schema_only(text.as_bytes(), &location, &mut self.set)
                    .map_err(|e| self.xtse0220(&e, Some(at)))?;
                self.set.mark_loaded(location.clone(), id);
                self.loaded.insert(location, (id, Some(at.clone())));
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
        self.loaded.insert(location, (id, Some(at.clone())));
        pending.push(id);
        Ok(())
    }

    /// A namespace-only import: a host schema of the namespace, loaded before the imports (M4b);
    /// else the schemas `xsd-schema` embeds (the XML namespace, XLink); the built-in attribute
    /// declarations of the XML Schema instance namespace; nothing for any other namespace (a
    /// document a directive loads later still makes it `found()`, `Loading::run`).
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
        // A host schema of the namespace is what this processor locates first.
        if has_document_of(&self.set, Some(namespace)) {
            return Ok(true);
        }
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
        self.loaded
            .insert(location.to_string(), (id, Some(at.clone())));
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
                    .and_then(|(_, at)| at.as_ref());
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

/// The built-in types XML Schema 1.1 Part 2 defines beyond XSLT 2.0's list ([`built_in_type`]):
/// Datatypes 1.1 §3.4.28 `dateTimeStamp` (its other additions — `anyAtomicType`, `dayTimeDuration`,
/// `yearMonthDuration` — are XPath 2.0's types, on the list already). `xs:error` is not one: it is
/// a Structures type (Structures 1.1 §3.16.7.3: "A Simple Type Definition for ·xs:error· is present
/// in every schema by definition"), and §3.13 adds only "All other built-in types defined in \[XML
/// Schema Part 2\]".
const XSD11_PART_2_ADDITIONS: [&str; 1] = ["dateTimeStamp"];

/// The type the expanded name `{uri}local` denotes in the in-scope schema types of a schema-aware
/// stylesheet compiled against `set`: a built-in one, or one of the set (D-7: the whole set).
///
/// §3.13: "A schema-aware XSLT processor additionally supports: All other built-in types defined in
/// \[XML Schema Part 2\]" — in an XML Schema 1.1 context (M4g, an implementation-defined mechanism
/// of the same section) that Part 2 is 1.1's, whose built-in types the set has: `xs:dateTimeStamp`
/// beyond XSLT 2.0's list ([`XSD11_PART_2_ADDITIONS`]; not `xs:error`, a Structures type).
pub(crate) fn in_scope_type(set: &SchemaSet, uri: &str, local: &str) -> Option<TypeKind> {
    if uri == XSD_NS {
        return built_in_type(local).or_else(|| {
            xsd11_part_2_addition(set, local)?;
            Some(TypeKind::Simple)
        });
    }
    let key = lookup_type(set, uri, local)?;
    Some(match key {
        TypeKey::Simple(_) => TypeKind::Simple,
        TypeKey::Complex(_) => TypeKind::Complex,
    })
}

/// The built-in type `xs:{local}` of an XML Schema 1.1 `set` that Part 2 1.1 adds to XSLT 2.0's
/// list ([`XSD11_PART_2_ADDITIONS`]): all are atomic and instantiable, so each has a constructor
/// function (XPath 2.0 §3.10.4).
pub(crate) fn xsd11_part_2_addition(set: &SchemaSet, local: &str) -> Option<TypeKey> {
    if !set.is_xsd11() || !XSD11_PART_2_ADDITIONS.contains(&local) {
        return None;
    }
    let names = &set.name_table;
    set.get_built_in_type_by_qname(names.get(XSD_NS), names.get(local)?)
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
