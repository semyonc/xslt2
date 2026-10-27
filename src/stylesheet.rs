//! Loading stylesheet modules (§3.10, §4.2) and the lexical static context
//! (§5.4.1, §3.5).
//!
//! A module is parsed into a [`BufferDocument`] in the compiler's arena and
//! walked with a navigator, which is what gives error locations, simplified
//! stylesheets and `xsl:include` for free (design sketch §7.1). The arena is
//! local to one compilation: the module trees are discarded once the
//! [`CompiledStylesheet`](crate::CompiledStylesheet) exists.

use std::collections::HashSet;

use bumpalo::Bump;
use xsd_schema::document::{BufferDocument, BufferDocumentOptions};
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};
use xsd_schema::xpath::functions::effective_boolean_value;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use crate::collation::{uca_not_implemented, CollationRegistry};
use crate::error::{codes, Location, XsltError};
use crate::functions::{StaticEvaluator, XsltFunctions};
use crate::names::{CODEPOINT_COLLATION, XML_NS, XSLT_NS};
use crate::receiver::Nav;
use crate::resolver::{self, Resolver};
use crate::whitespace;

/// Identifies one loaded stylesheet module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub u32);

/// XSLT 1.0 compatibility, forwards compatibility, or neither (§3.8, §3.9, §9.10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CompatMode {
    /// `[xsl:]version` is 2.0.
    #[default]
    Xslt20,
    /// `[xsl:]version` is less than 2.0 — backwards-compatible behaviour.
    BackwardsCompatible,
    /// `[xsl:]version` is greater than 2.0 — forwards-compatible processing.
    ForwardsCompatible,
}

impl CompatMode {
    /// §3.8: "backwards compatible behavior is enabled for an element if the
    /// element has an `[xsl:]version` attribute whose value is less than 2.0".
    /// §3.9 makes the symmetric statement for a value greater than 2.0.
    pub fn of_version(version: f64) -> Self {
        if version < 2.0 {
            Self::BackwardsCompatible
        } else if version > 2.0 {
            Self::ForwardsCompatible
        } else {
            Self::Xslt20
        }
    }

    /// Whether XSLT 1.0 behaviours apply (§9.10).
    pub fn is_backwards(self) -> bool {
        self == Self::BackwardsCompatible
    }

    /// Whether unknown XSLT elements are tolerated (§3.9).
    pub fn is_forwards(self) -> bool {
        self == Self::ForwardsCompatible
    }
}

/// One interned lexical static context (§5.4.1).
///
/// The standard attributes of §3.5 may appear on *any* stylesheet element and
/// apply to its subtree, so the context belongs to an element, not to a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticContext {
    pub module: ModuleId,
    /// In-scope namespace bindings, prefix → URI, sorted by prefix.
    pub namespaces: Vec<(String, String)>,
    /// Default namespace for unprefixed element names *in XPath expressions and
    /// patterns* (`[xsl:]xpath-default-namespace`).
    pub xpath_default_namespace: String,
    /// The default collation (§3.6.1): the first URI of the nearest
    /// `[xsl:]default-collation` that the
    /// [`CollationRegistry`] recognizes,
    /// absolute; the codepoint collation when no ancestor has the attribute.
    /// It becomes the default collation of every `XPathContext` built for this
    /// static context. §3.6.1: it "does not affect the collation used by
    /// xsl:sort".
    pub default_collation: String,
    /// Base URI after `xml:base`.
    pub base_uri: String,
    /// The `[xsl:]version` that applies here.
    pub compat: CompatMode,
    /// Namespace **URIs** excluded from literal result elements
    /// (`[xsl:]exclude-result-prefixes`).
    ///
    /// §11.1.2 designates the *namespace bound to* each listed prefix, so the
    /// prefix is resolved where the attribute is written — which is not
    /// necessarily where the literal result element is — and the URI is what
    /// travels down the subtree.
    pub excluded_namespaces: Vec<String>,
    /// Namespace URIs naming extension instructions
    /// (`[xsl:]extension-element-prefixes`).
    pub extension_namespaces: Vec<String>,
}

impl StaticContext {
    /// The namespace URI a prefix is bound to here.
    pub fn resolve_prefix(&self, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(XML_NS.to_string());
        }
        self.namespaces
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.clone())
    }

    /// A prefix bound to `uri` here, preferring a non-empty one. `None` when
    /// the namespace cannot be spelled in this context.
    pub fn prefix_for(&self, uri: &str) -> Option<String> {
        if uri.is_empty() {
            return Some(String::new());
        }
        self.namespaces
            .iter()
            .filter(|(p, u)| u == uri && !p.is_empty())
            .map(|(p, _)| p.clone())
            .next()
    }

    /// The bindings a compiled [`XPathContext`] needs.
    ///
    /// [`XPathContext`]: xsd_schema::xpath::XPathContext
    pub fn snapshot(&self, names: &NameTable) -> NamespaceContextSnapshot {
        NamespaceContextSnapshot {
            default_ns: None,
            bindings: self
                .namespaces
                .iter()
                .filter(|(p, _)| !p.is_empty())
                .map(|(p, u)| (names.add(p), names.add(u)))
                .collect(),
        }
    }

    /// Whether a namespace is excluded from literal result elements.
    ///
    /// §11.1.2: "the `[xsl:]exclude-result-prefixes` attribute … the value is
    /// a whitespace-separated list of namespace prefixes. The namespace bound
    /// to each of the prefixes is designated as an excluded namespace." The
    /// test is therefore on the **URI**: a different prefix bound to the same
    /// URI is excluded too, and a prefix rebound to another URI is not.
    pub fn is_excluded(&self, uri: &str) -> bool {
        uri == XSLT_NS || self.excluded_namespaces.iter().any(|u| u == uri)
    }

    /// Whether `uri` is an extension-element namespace (§18.2).
    pub fn is_extension_namespace(&self, uri: &str) -> bool {
        self.extension_namespaces.iter().any(|u| u == uri)
    }
}

/// Interns [`StaticContext`]s so that identical ones share an id (§7.2).
#[derive(Default)]
pub struct StaticContexts {
    contexts: Vec<StaticContext>,
}

/// Index of an interned [`StaticContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticContextId(pub u32);

impl StaticContexts {
    /// Interns `ctx`, returning the id it shares with any identical one.
    pub fn intern(&mut self, ctx: StaticContext) -> StaticContextId {
        if let Some(i) = self.contexts.iter().position(|c| *c == ctx) {
            return StaticContextId(i as u32);
        }
        self.contexts.push(ctx);
        StaticContextId(self.contexts.len() as u32 - 1)
    }

    /// The context with this id.
    pub fn get(&self, id: StaticContextId) -> &StaticContext {
        &self.contexts[id.0 as usize]
    }

    /// How many distinct contexts the stylesheet has.
    pub fn len(&self) -> usize {
        self.contexts.len()
    }

    /// Whether nothing has been interned.
    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    /// Every interned context, in id order.
    pub fn all(&self) -> &[StaticContext] {
        &self.contexts
    }
}

// ── Modules ──────────────────────────────────────────────────────────────

/// One parsed stylesheet module.
pub struct Module<'t> {
    pub id: ModuleId,
    pub uri: String,
    pub doc: &'t BufferDocument<'t>,
    /// The element the module starts at: the document element, or the element
    /// a fragment identifier picked out of a host document (§3.11).
    pub root: Option<u32>,
    /// Byte offset of the start of every line, for turning a source span into
    /// a line and column.
    line_starts: Vec<usize>,
    /// The module's text, as parsed: the source spans of `doc` are byte ranges
    /// of it. An inline schema of `xsl:import-schema` is cut out of it
    /// (`crate::schema`, M4 plan D-9).
    pub(crate) text: String,
    /// The element node references §3.12 effectively excludes from this module.
    ///
    /// Computed once, by [`Loader`], **before** `xsl:include` and `xsl:import`
    /// are resolved and before anything else looks at the tree, so that no part
    /// of the compiler has to remember `use-when`: every walk over a module's
    /// children goes through [`Module::children`], and an excluded element —
    /// with its whole subtree — is simply not yielded.
    excluded: HashSet<u32>,
}

impl<'t> Module<'t> {
    /// The location of a node, for an error message.
    ///
    /// The byte range comes from upstream `BufferDocument::source_span`, which
    /// the loader enables with `BufferDocumentOptions::track_source_locations`;
    /// the line and column are computed here because the document keeps only
    /// offsets, not the text.
    pub fn location(&self, nav: &Nav<'t>) -> Location {
        let (line, column) = match self.doc.source_span(nav.current_ref()) {
            Some(span) => self.line_column(span.start),
            None => (0, 0),
        };
        Location {
            module_uri: Some(self.uri.clone()),
            line,
            column,
        }
    }

    /// The children of `nav` that §3.12 leaves in the stylesheet module.
    ///
    /// > If the attribute is present and the effective boolean value of the
    /// > expression is false, then the element, together with all the nodes
    /// > having that element as an ancestor, is effectively excluded from the
    /// > stylesheet module. … the stylesheet module has the same effect as if
    /// > the node were not there.
    ///
    /// This is the **only** way anything walks the children of a stylesheet
    /// element after loading: an element `use-when` excluded — and with it its
    /// whole subtree, which the walk never descends into — is simply not
    /// yielded, so a traversal cannot forget the check by omitting a guard.
    /// Text, comments and processing instructions pass through untouched;
    /// §3.12 puts `use-when` on elements only.
    ///
    /// The one traversal that cannot use this is the §3.12 pass that *computes*
    /// the exclusion set (`use_when_exclusions`) together with the §3.11
    /// fragment lookup that runs beside it in `Loader::parse`: both run before
    /// the set exists. They are in this module, next to the set itself.
    pub fn children(&self, nav: &Nav<'t>) -> Children<'_, 't> {
        let mut first = nav.clone();
        Children {
            module: self,
            next: first.move_to_first_child().then_some(first),
        }
    }

    /// Whether §3.12 effectively excludes this element of **this** module.
    ///
    /// Only the excluded element itself is recorded; because every traversal
    /// starts at the module root and stops at an excluded element, its
    /// descendants are never reached.
    ///
    /// Prefer [`Module::children`]: a hand-written sibling walk plus this
    /// predicate is what §3.12 exclusion used to be applied by, and what one
    /// forgotten call made wrong.
    pub fn is_excluded(&self, nav: &Nav<'t>) -> bool {
        !self.excluded.is_empty() && self.excluded.contains(&nav.current_ref())
    }

    /// 1-based line and column of a byte offset.
    pub(crate) fn line_column(&self, offset: usize) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let column = offset - self.line_starts[line] + 1;
        (line as u32 + 1, column as u32)
    }
}

/// The §3.12-filtered child sequence of one stylesheet element, in document
/// order (see [`Module::children`]).
pub struct Children<'m, 't> {
    module: &'m Module<'t>,
    /// The next candidate, already positioned; `None` once the siblings run out.
    next: Option<Nav<'t>>,
}

impl<'t> Iterator for Children<'_, 't> {
    type Item = Nav<'t>;

    fn next(&mut self) -> Option<Nav<'t>> {
        loop {
            let candidate = self.next.take()?;
            let mut following = candidate.clone();
            self.next = following.move_to_next_sibling().then_some(following);
            if candidate.node_type() == DomNodeType::Element && self.module.is_excluded(&candidate)
            {
                continue;
            }
            return Some(candidate);
        }
    }
}

/// The byte offset of the start of every line of `text`, starting with 0.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// How a module was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inclusion {
    Import,
    Include,
}

/// What a top-level entry of a unit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    /// An ordinary top-level declaration.
    Declaration,
    /// The literal result element of a simplified stylesheet module (§3.7),
    /// which stands for one template rule matching `/`.
    SimplifiedRoot,
}

/// One import-precedence level: the top-level declarations of a module and of
/// everything it includes (§3.10.2 — an included module's declarations become
/// the includer's).
pub struct Unit<'t> {
    pub precedence: u32,
    /// Lowest precedence among the modules this one imports, transitively
    /// (§7.2). The imported subtree is `[subtree_min, precedence)`.
    pub subtree_min: u32,
    /// Top-level declarations, in declaration order, each with its module.
    pub decls: Vec<(ModuleId, Nav<'t>, DeclKind)>,
}

/// Every module of one compilation, and the precedence levels they form.
pub struct ModuleSet<'t> {
    pub modules: Vec<Module<'t>>,
    pub units: Vec<Unit<'t>>,
    /// The principal module: the one the compilation started from.
    pub principal: ModuleId,
    /// Whether the principal module is a simplified stylesheet (§3.7).
    pub simplified: bool,
    /// §4.3: "Stripping of type annotations takes place if at least one stylesheet module in the
    /// stylesheet specifies input-type-annotations="strip"".
    pub strip_input_annotations: bool,
}

impl<'t> ModuleSet<'t> {
    pub fn module(&self, id: ModuleId) -> &Module<'t> {
        &self.modules[id.0 as usize]
    }
}

/// Loads a module graph into `arena`.
pub struct Loader<'a, 't> {
    arena: &'t Bump,
    names: &'t NameTable,
    resolver: &'a dyn Resolver,
    /// The function catalog a `use-when` expression is compiled against (§3.12).
    functions: &'a XsltFunctions,
    modules: Vec<Module<'t>>,
    units: Vec<Unit<'t>>,
    /// The modules on the current load path, by
    /// [`resolver::resource_identity`] (so that `d//m.xsl` and `d/m.xsl` are
    /// one module), with the edge that reached them.
    stack: Vec<(String, Inclusion)>,
    next_precedence: u32,
    /// The `input-type-annotations` values the modules of this stylesheet ask
    /// for, for §3.6's XTSE0265 across the whole module graph.
    input_type_annotations: HashSet<String>,
    /// Whether the processor is schema-aware (§21.2), which a `use-when`
    /// expression can observe through `system-property('xsl:is-schema-aware')`
    /// and `type-available()` (§3.12).
    schema_aware: bool,
}

impl<'a, 't> Loader<'a, 't> {
    pub fn new(
        arena: &'t Bump,
        names: &'t NameTable,
        resolver: &'a dyn Resolver,
        functions: &'a XsltFunctions,
    ) -> Self {
        Self {
            arena,
            names,
            resolver,
            functions,
            modules: Vec::new(),
            units: Vec::new(),
            stack: Vec::new(),
            next_precedence: 0,
            input_type_annotations: HashSet::new(),
            schema_aware: false,
        }
    }

    /// Loads as a schema-aware processor (§21.2): `use-when` sees
    /// `system-property('xsl:is-schema-aware') = 'yes'` and every built-in
    /// type (§3.12: "The type definitions that would be available in the
    /// absence of any `xsl:import-schema` declaration").
    pub fn schema_aware(mut self, schema_aware: bool) -> Self {
        self.schema_aware = schema_aware;
        self
    }

    /// Loads the principal module at `uri` and everything it imports or
    /// includes.
    pub fn load(mut self, uri: &str, text: Option<String>) -> Result<ModuleSet<'t>, XsltError> {
        let (principal, simplified) = self.visit(uri, text, Inclusion::Import, true)?;
        // > [ERR XTSE0265] It is a static error if there is a stylesheet module
        // > in the stylesheet that specifies input-type-annotations="strip" and
        // > another stylesheet module that specifies
        // > input-type-annotations="preserve".
        if self.input_type_annotations.contains("strip")
            && self.input_type_annotations.contains("preserve")
        {
            return Err(XsltError::new(
                codes::XTSE0265,
                "one stylesheet module specifies input-type-annotations=\"strip\" and another \
                 specifies input-type-annotations=\"preserve\"",
            ));
        }
        Ok(ModuleSet {
            modules: self.modules,
            units: self.units,
            principal,
            simplified,
            strip_input_annotations: self.input_type_annotations.contains("strip"),
        })
    }

    /// Parses one module, processes its imports (post-order precedence), and
    /// records the resulting unit.
    fn visit(
        &mut self,
        uri: &str,
        text: Option<String>,
        edge: Inclusion,
        principal: bool,
    ) -> Result<(ModuleId, bool), XsltError> {
        let identity = resolver::resource_identity(uri);
        if self.stack.iter().any(|(u, _)| *u == identity) {
            // §3.10.2 / §3.10.3: an include cycle is XTSE0180, an import cycle
            // XTSE0210. The edge that closes the cycle names it.
            let code = if edge == Inclusion::Include {
                codes::XTSE0180
            } else {
                codes::XTSE0210
            };
            return Err(XsltError::new(
                code,
                format!(
                    "the stylesheet module {uri:?} directly or indirectly {} itself",
                    if edge == Inclusion::Include {
                        "includes"
                    } else {
                        "imports"
                    }
                ),
            ));
        }
        self.stack.push((identity, edge));
        let result = self.visit_inner(uri, text, principal);
        self.stack.pop();
        result
    }

    fn visit_inner(
        &mut self,
        uri: &str,
        text: Option<String>,
        principal: bool,
    ) -> Result<(ModuleId, bool), XsltError> {
        let id = self.parse(uri, text)?;
        let subtree_min = self.next_precedence;
        let mut decls: Vec<(ModuleId, Nav<'t>, DeclKind)> = Vec::new();
        let simplified = self.collect(id, &mut decls)?;
        let _ = principal;
        let precedence = self.next_precedence;
        self.next_precedence += 1;
        self.units.push(Unit {
            precedence,
            subtree_min,
            decls,
        });
        Ok((id, simplified))
    }

    /// Parses one module into the arena and records its line starts.
    fn parse(&mut self, uri: &str, text: Option<String>) -> Result<ModuleId, XsltError> {
        // §3.11: a fragment identifier picks an embedded stylesheet module out
        // of a host document by its ID.
        let (document_uri, fragment) = match uri.split_once('#') {
            Some((base, fragment)) => (base, Some(fragment.to_string())),
            None => (uri, None),
        };
        let text = match text {
            Some(t) => t,
            None => self
                .resolver
                .resolve_text(document_uri)
                .map_err(|e| resolver::load_failed(uri, &e))?,
        };
        let doc = BufferDocument::from_reader(
            text.as_bytes(),
            self.arena,
            self.names,
            BufferDocumentOptions {
                track_source_locations: true,
                ..Default::default()
            },
            None,
        )
        .map_err(|e| resolver::load_failed(uri, &e.to_string()))?;
        let doc: &'t BufferDocument<'t> = self.arena.alloc(doc);
        let root = match &fragment {
            None => None,
            Some(fragment) => Some(find_by_id(doc, fragment).ok_or_else(|| {
                resolver::load_failed(uri, &format!("no element has the id {fragment:?}"))
            })?),
        };
        let id = ModuleId(self.modules.len() as u32);
        self.modules.push(Module {
            id,
            uri: uri.to_string(),
            doc,
            root,
            line_starts: line_starts(&text),
            text,
            excluded: HashSet::new(),
        });
        // §3.12, as early as it can be: before `xsl:include`/`xsl:import` are
        // resolved (an excluded one must not load its module at all) and before
        // any static validation (§3.12: "no static or dynamic errors will be
        // reported in respect of the element and its contents").
        let excluded = use_when_exclusions(
            &self.modules[id.0 as usize],
            self.names,
            self.functions,
            self.schema_aware,
        )?;
        self.modules[id.0 as usize].excluded = excluded;
        Ok(id)
    }

    /// Collects the top-level declarations of a module, expanding
    /// `xsl:include` in place and recursing into `xsl:import`.
    ///
    /// Returns whether the module is a simplified stylesheet (§3.7), which only
    /// the principal module may be.
    fn collect(
        &mut self,
        id: ModuleId,
        out: &mut Vec<(ModuleId, Nav<'t>, DeclKind)>,
    ) -> Result<bool, XsltError> {
        let module_uri = self.modules[id.0 as usize].uri.clone();
        let root = {
            let module = &self.modules[id.0 as usize];
            match module.root {
                Some(node) => module.doc.create_navigator_at(node),
                None => {
                    let mut nav = module.doc.create_navigator();
                    if !move_to_document_element(&mut nav) {
                        return Err(XsltError::new(
                            codes::XTSE0010,
                            format!("the stylesheet module {module_uri:?} has no document element"),
                        ));
                    }
                    nav
                }
            }
        };

        let is_stylesheet = root.namespace_uri() == XSLT_NS
            && matches!(root.local_name(), "stylesheet" | "transform");
        if !is_stylesheet {
            // §3.12: an excluded simplified stylesheet module contributes
            // nothing at all, and nothing about it is checked.
            if self.modules[id.0 as usize].is_excluded(&root) {
                return Ok(true);
            }
            // §3.7 Simplified Stylesheet Modules: a literal result element with
            // an `xsl:version` attribute is a stylesheet whose single template
            // matches "/" and whose body is that element.
            return match attr_ns(&root, XSLT_NS, "version") {
                // §3.7: the literal result element stands for one template
                // rule matching `/`. An included or imported module may be
                // simplified too, so this is not restricted to the principal
                // module.
                Some(version) => {
                    check_version(&version, &module_uri)?;
                    out.push((id, root, DeclKind::SimplifiedRoot));
                    Ok(true)
                }
                None if root.namespace_uri() == XSLT_NS => Err(XsltError::new(
                    codes::XTSE0010,
                    format!(
                        "the document element of {module_uri:?} is xsl:{}, not xsl:stylesheet \
                         or xsl:transform",
                        root.local_name()
                    ),
                )),
                // §3.7: the document element is then a literal result element,
                // and "[ERR XTSE0150] A literal result element that is used as
                // the outermost element of a simplified stylesheet module must
                // have an xsl:version attribute." §3.10.1 also allows XTSE0165
                // when the module was reached through xsl:include/xsl:import,
                // because "the resource that is retrieved does not contain a
                // stylesheet module conforming to this specification"; the
                // suite accepts either, so the more specific code is used.
                None => Err(XsltError::new(
                    codes::XTSE0150,
                    format!(
                        "{module_uri:?} is neither an xsl:stylesheet nor a simplified stylesheet \
                         (its document element {} has no xsl:version attribute)",
                        root.local_name()
                    ),
                )),
            };
        }

        // §3.6: "The xsl:stylesheet element must have a version attribute".
        let version = attr_local(&root, "version").ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!(
                    "xsl:{} in {module_uri:?} has no version attribute",
                    root.local_name()
                ),
            )
        })?;
        check_version(&version, &module_uri)?;
        // §3.6: the three values of `input-type-annotations`, collected across
        // the module graph for XTSE0265 (checked once, in `load`).
        if let Some(value) = attr_local(&root, "input-type-annotations") {
            match whitespace::trim(&value) {
                v @ ("strip" | "preserve" | "unspecified") => {
                    self.input_type_annotations.insert(v.to_string());
                }
                other => {
                    return Err(XsltError::new(
                        codes::XTSE0020,
                        format!(
                            "input-type-annotations must be \"strip\", \"preserve\" or \
                             \"unspecified\", not {other:?}"
                        ),
                    ))
                }
            }
        }

        let mut seen_non_import = false;
        // §3.12: an excluded declaration is not there — in particular an
        // excluded `xsl:include`/`xsl:import` never loads its module. That is
        // `Module::children`'s job, so the walk here cannot forget it.
        //
        // The children are materialized first because the loop body loads more
        // modules, which both borrows `self` mutably and can reallocate
        // `self.modules`; the exclusion set of *this* module is fixed by
        // `parse` before `collect` runs, so the list cannot go stale.
        let children: Vec<Nav<'t>> = self.modules[id.0 as usize].children(&root).collect();
        for child in children {
            if child.node_type() != DomNodeType::Element {
                continue;
            }
            let xslt = child.namespace_uri() == XSLT_NS;
            let local = child.local_name().to_string();
            match (xslt, local.as_str()) {
                (true, "import") => {
                    if seen_non_import {
                        // §3.10.2: "The xsl:import element children must
                        // precede all other element children of an
                        // xsl:stylesheet element".
                        return Err(XsltError::new(
                            codes::XTSE0200,
                            "xsl:import must precede every other declaration of a module",
                        ));
                    }
                    let href = require_href(&child, "xsl:import")?;
                    let target = resolver::resolve_uri(&base_uri_of(&child, &module_uri), &href);
                    self.visit(&target, None, Inclusion::Import, false)?;
                }
                (true, "include") => {
                    seen_non_import = true;
                    let href = require_href(&child, "xsl:include")?;
                    let target = resolver::resolve_uri(&base_uri_of(&child, &module_uri), &href);
                    self.visit_include(&target, out)?;
                }
                _ => {
                    seen_non_import = true;
                    out.push((id, child.clone(), DeclKind::Declaration));
                }
            }
        }
        Ok(false)
    }

    /// Splices an included module's declarations into `out` (§3.10.1: "the
    /// effect is the same as if the included stylesheet module's declarations
    /// … were copied into the including stylesheet module").
    ///
    /// The module keeps no precedence of its own, but it *does* stay on the
    /// load path for the whole of its own `collect`, which is what makes an
    /// indirect include cycle (`a` → `b` → `c` → `b`) an error rather than a
    /// hang.
    fn visit_include(
        &mut self,
        uri: &str,
        out: &mut Vec<(ModuleId, Nav<'t>, DeclKind)>,
    ) -> Result<(), XsltError> {
        let identity = resolver::resource_identity(uri);
        if self.stack.iter().any(|(u, _)| *u == identity) {
            return Err(XsltError::new(
                codes::XTSE0180,
                format!("the stylesheet module {uri:?} directly or indirectly includes itself"),
            ));
        }
        self.stack.push((identity, Inclusion::Include));
        let result = self
            .parse(uri, None)
            .and_then(|id| self.collect(id, out).map(|_| ()));
        self.stack.pop();
        result
    }
}

// ── §3.12 Conditional Element Inclusion ──────────────────────────────────

/// The elements of `module` that §3.12 effectively excludes.
///
/// > Any element in the XSLT namespace may have a `use-when` attribute whose
/// > value is an XPath expression that can be evaluated statically. If the
/// > attribute is present and the effective boolean value of the expression is
/// > false, then the element, together with all the nodes having that element
/// > as an ancestor, is effectively excluded from the stylesheet module. …
/// > A literal result element, or any other element within a stylesheet module
/// > that is not in the XSLT namespace, may similarly carry an `xsl:use-when`
/// > attribute.
///
/// The walk is top-down and stops at an excluded element, so a `use-when`
/// inside an excluded subtree is never evaluated — §3.12 promises that "no
/// static or dynamic errors will be reported in respect of the element and its
/// contents, other than errors in the `use-when` attribute itself".
///
/// **The escape hatch.** This is the one walk over a stylesheet element's
/// children that must *not* go through [`Module::children`], because it is what
/// builds the set `Module::children` filters by: at this point
/// `module.excluded` is still empty, so the filtered iterator would yield
/// everything and the recursion would be the same walk with an extra indirection
/// — while `Module::children` would silently start returning different nodes
/// halfway through the pass as the set grows under it. It walks raw, top-down,
/// and never descends into an element it has just excluded, which is what makes
/// the set complete for every later traversal.
fn use_when_exclusions(
    module: &Module<'_>,
    names: &NameTable,
    functions: &XsltFunctions,
    schema_aware: bool,
) -> Result<HashSet<u32>, XsltError> {
    let mut excluded = HashSet::new();
    let root = match module.root {
        Some(node) => module.doc.create_navigator_at(node),
        None => {
            let mut nav = module.doc.create_navigator();
            if !move_to_document_element(&mut nav) {
                return Ok(excluded);
            }
            nav
        }
    };

    if !keeps(&root, module, names, functions, schema_aware)? {
        let is_stylesheet = root.namespace_uri() == XSLT_NS
            && matches!(root.local_name(), "stylesheet" | "transform");
        if is_stylesheet {
            // > If the xsl:stylesheet or xsl:transform element itself is
            // > effectively excluded, the effect is to exclude all the children
            // > of the xsl:stylesheet or xsl:transform element, but not the
            // > xsl:stylesheet or xsl:transform element or its attributes.
            let mut child = root.clone();
            if child.move_to_first_child() {
                loop {
                    if child.node_type() == DomNodeType::Element {
                        excluded.insert(child.current_ref());
                    }
                    if !child.move_to_next_sibling() {
                        break;
                    }
                }
            }
        } else {
            // A simplified stylesheet module's literal result element, which is
            // the module's only declaration.
            excluded.insert(root.current_ref());
        }
        return Ok(excluded);
    }

    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        let mut child = parent.clone();
        if !child.move_to_first_child() {
            continue;
        }
        loop {
            if child.node_type() == DomNodeType::Element {
                if keeps(&child, module, names, functions, schema_aware)? {
                    stack.push(child.clone());
                } else {
                    excluded.insert(child.current_ref());
                }
            }
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    Ok(excluded)
}

/// Whether an element survives its own `[xsl:]use-when` (§3.12).
fn keeps(
    nav: &Nav<'_>,
    module: &Module<'_>,
    names: &NameTable,
    functions: &XsltFunctions,
    schema_aware: bool,
) -> Result<bool, XsltError> {
    // §3.5 spells a standard attribute `use-when` on an XSLT element and
    // `xsl:use-when` on any other.
    let Some(source) = standard_attr(nav, "use-when") else {
        return Ok(true);
    };
    evaluate_use_when(&source, nav, module, names, functions, schema_aware)
}

/// Evaluates one `use-when` expression in the static and dynamic context §3.12
/// prescribes.
///
/// > XPath 1.0 compatibility mode: false. In scope namespaces: determined by
/// > the in-scope namespaces for the containing element in the stylesheet.
/// > Default element/type namespace: determined by the `xpath-default-namespace`
/// > attribute if present … In scope variables: None. In scope functions: The
/// > core functions defined in \[Functions and Operators\], together with the
/// > functions `element-available`, `function-available`, `type-available`, and
/// > `system-property` … Default collation: The Unicode Codepoint Collation.
/// > Base URI: The base URI of the containing element in the stylesheet. …
/// > Context item, position, and size: Undefined.
///
/// Compatibility mode is deliberately *not* taken from `[xsl:]version`: §3.12
/// fixes it at false however the surrounding module is versioned.
fn evaluate_use_when(
    source: &str,
    nav: &Nav<'_>,
    module: &Module<'_>,
    names: &NameTable,
    functions: &XsltFunctions,
    schema_aware: bool,
) -> Result<bool, XsltError> {
    let sc = use_when_static_context(nav, module);
    // §3.12's "In scope functions" row: the F&O core plus `element-available`,
    // `function-available`, `type-available` and `system-property`, and nothing
    // else — so a call to an XSLT-specific function such as `generate-id()` or
    // `key()` does not bind, which is XPST0017.
    let use_when_catalog = functions.use_when_catalog();
    // "In scope collations: Implementation-defined" — the same registry as
    // every other expression; "Default collation: The Unicode Codepoint
    // Collation", which is what an `XPathContext` defaults to.
    let mut ctx = XPathContext::new(names)
        .with_namespaces(sc.snapshot(names))
        .with_base_uri(sc.base_uri.clone())
        .with_function_catalog(&use_when_catalog)
        .with_collation_resolver(functions.collations());
    if !sc.xpath_default_namespace.is_empty() {
        ctx = ctx.with_default_element_ns(names.add(&sc.xpath_default_namespace));
    }
    let fail = |e: xsd_schema::xpath::XPathError| {
        // An error the engine identifies by an error QName — `fn:error` —
        // keeps that identity; anything else that has no code of its own is an
        // error in the `use-when` attribute itself, which §3.12 leaves as
        // XTSE0010. An error one of the name-answering functions raised
        // (XTDE1390, XTDE1400, XTDE1428, XTDE1440, and `fn:doc`'s FODC0002)
        // comes back as the `XsltError` it was, with the processor as its
        // origin: §3.12 excludes an element with "no static or dynamic errors
        // … other than errors in the use-when attribute itself", and names no
        // code of its own for those, so the error is reported as it is, while
        // the stylesheet is compiled.
        crate::error::raised_error_to_xslt(&e)
            .unwrap_or_else(|| {
                XsltError::new(
                    e.error_code().unwrap_or(codes::XTSE0010),
                    format!("the use-when expression {source:?} cannot be evaluated: {e}"),
                )
            })
            .or_location(&module.location(nav))
    };
    let expr = XPathExpr::compile(source, &ctx).map_err(fail)?;
    let evaluator = StaticEvaluator::new(
        functions,
        sc.namespaces.clone(),
        sc.xpath_default_namespace.clone(),
    )
    .schema_aware(schema_aware);
    let outcome = expr
        .evaluator(&ctx)
        .run_with_node_and_setup::<Nav<'_>, _>(None, |ev| {
            ev.context().set_function_evaluator(Some(&evaluator));
        });
    let value = outcome.map_err(fail)?;
    effective_boolean_value(&value).map_err(fail)
}

/// The parts of an element's static context that its `use-when` expression
/// sees (§3.12's table): its in-scope namespaces, the nearest
/// `[xsl:]xpath-default-namespace` and its base URI.
///
/// Not [`static_context_of`]: that one also interprets `[xsl:]version`,
/// `[xsl:]default-collation`, `[xsl:]exclude-result-prefixes` and
/// `[xsl:]extension-element-prefixes`, and reports their errors — but the
/// element may be about to be excluded, and §3.12 says "When a node is
/// effectively excluded from a stylesheet module the stylesheet module has the
/// same effect as if the node were not there. Among other things this means
/// that no static or dynamic errors will be reported in respect of the element
/// and its contents, other than errors in the use-when attribute itself." (Its
/// Note keeps the errors of "attributes that are necessarily processed before
/// \[xsl:\]use-when, examples being xml:space and
/// \[xsl:\]xpath-default-namespace"; neither can raise one here.) The
/// attributes of an element that is *not* excluded are interpreted, and their
/// errors reported, when it is checked and compiled.
fn use_when_static_context(nav: &Nav<'_>, module: &Module<'_>) -> StaticContext {
    let mut xpath_default_namespace = String::new();
    let mut cursor = nav.clone();
    loop {
        if cursor.node_type() == DomNodeType::Element {
            if let Some(v) = standard_attr(&cursor, "xpath-default-namespace") {
                xpath_default_namespace = v;
                break;
            }
        }
        if !cursor.move_to_parent() {
            break;
        }
    }
    StaticContext {
        module: module.id,
        namespaces: in_scope_namespaces(nav),
        xpath_default_namespace,
        // "Default collation: The Unicode Codepoint Collation".
        default_collation: CODEPOINT_COLLATION.to_string(),
        base_uri: base_uri_of(nav, &module.uri),
        // "XPath 1.0 compatibility mode: false".
        compat: CompatMode::Xslt20,
        excluded_namespaces: Vec::new(),
        extension_namespaces: Vec::new(),
    }
}

/// §3.6: the `version` attribute must be a number.
///
/// > \[ERR XTSE0110\] The value of the `version` attribute must be a number:
/// > specifically, it must be a valid instance of the type `xs:decimal` as
/// > defined in \[XML Schema Part 2\].
fn check_version(version: &str, module_uri: &str) -> Result<(), XsltError> {
    if crate::compile::checks::is_xs_decimal(version) {
        return Ok(());
    }
    Err(XsltError::new(
        codes::XTSE0110,
        format!(
            "the version attribute of {module_uri:?} is {version:?}, which is not an xs:decimal"
        ),
    ))
}

fn require_href(nav: &Nav<'_>, what: &str) -> Result<String, XsltError> {
    attr_local(nav, "href")
        .ok_or_else(|| XsltError::new(codes::XTSE0010, format!("{what} has no href attribute")))
}

// ── Navigation helpers ───────────────────────────────────────────────────

/// The element whose `id` or `xml:id` attribute is `id` (§3.11).
///
/// `BufferDocument` only indexes `xml:id`, because nothing tells it which
/// attributes an external DTD declared to be of type ID; embedded stylesheet
/// modules are conventionally marked with a plain `id` attribute, so both are
/// accepted.
///
/// Walks raw rather than through [`Module::children`]: this runs inside
/// [`Loader::parse`], which needs the module root before there is a `Module` to
/// compute a §3.12 exclusion set for.
fn find_by_id(doc: &BufferDocument<'_>, id: &str) -> Option<u32> {
    if let Some(node) = doc.get_element_by_id(id) {
        return Some(node);
    }
    let mut nav = doc.create_navigator();
    let mut stack = Vec::new();
    if nav.move_to_first_child() {
        stack.push(nav);
    }
    while let Some(mut cursor) = stack.pop() {
        loop {
            if cursor.node_type() == DomNodeType::Element {
                let matches = attr_local(&cursor, "id").as_deref() == Some(id)
                    || attr_ns(&cursor, XML_NS, "id").as_deref() == Some(id);
                if matches {
                    return Some(cursor.current_ref());
                }
                let mut child = cursor.clone();
                if child.move_to_first_child() {
                    let mut rest = cursor.clone();
                    if rest.move_to_next_sibling() {
                        stack.push(rest);
                    }
                    cursor = child;
                    continue;
                }
            }
            if !cursor.move_to_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Moves a document-node navigator to the document element.
pub fn move_to_document_element(nav: &mut Nav<'_>) -> bool {
    if !nav.move_to_first_child() {
        return false;
    }
    loop {
        if nav.node_type() == DomNodeType::Element {
            return true;
        }
        if !nav.move_to_next_sibling() {
            return false;
        }
    }
}

/// The value of an attribute in no namespace.
pub fn attr_local(nav: &Nav<'_>, local: &str) -> Option<String> {
    attr_ns(nav, "", local)
}

/// The value of an attribute with an explicit namespace.
pub fn attr_ns(nav: &Nav<'_>, uri: &str, local: &str) -> Option<String> {
    let mut a = nav.clone();
    if !a.move_to_first_attribute() {
        return None;
    }
    loop {
        if a.local_name() == local && a.namespace_uri() == uri {
            return Some(a.value());
        }
        if !a.move_to_next_attribute() {
            return None;
        }
    }
}

/// Every attribute of an element as `(uri, local, prefix, value)`.
pub fn attributes(nav: &Nav<'_>) -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    let mut a = nav.clone();
    if !a.move_to_first_attribute() {
        return out;
    }
    loop {
        out.push((
            a.namespace_uri().to_string(),
            a.local_name().to_string(),
            a.prefix().to_string(),
            a.value(),
        ));
        if !a.move_to_next_attribute() {
            return out;
        }
    }
}

/// A standard attribute of §3.5, spelled `name` on an XSLT element and
/// `xsl:name` on any other.
pub fn standard_attr(nav: &Nav<'_>, name: &str) -> Option<String> {
    if nav.namespace_uri() == XSLT_NS {
        attr_local(nav, name)
    } else {
        attr_ns(nav, XSLT_NS, name)
    }
}

/// The base URI in effect at a node: `xml:base` on it or an ancestor, resolved
/// against the module URI (§5.8).
pub fn base_uri_of(nav: &Nav<'_>, module_uri: &str) -> String {
    let mut chain: Vec<String> = Vec::new();
    let mut cursor = nav.clone();
    loop {
        if cursor.node_type() == DomNodeType::Element {
            if let Some(base) = attr_ns(&cursor, XML_NS, "base") {
                chain.push(base);
            }
        }
        if !cursor.move_to_parent() {
            break;
        }
    }
    let mut uri = module_uri.to_string();
    for base in chain.iter().rev() {
        uri = resolver::resolve_uri(&uri, base);
    }
    uri
}

/// The base URI of a node of a tree — the XDM `base-uri` accessor, as `fn:base-uri` computes it:
/// the `xml:base` values of its ancestor-or-self elements (an attribute, text, comment or processing
/// instruction takes its parent element's), resolved outermost first against the base URI of the
/// document the node lives in, which itself resolves against `fallback` (the static base URI; it is
/// the whole answer when the document has none).
///
/// A parentless element lives in a hidden document whose base URI is the element's own
/// (`SequenceCollector`). When it carries an `xml:base` of its own, that document base is not used
/// and the chain resolves against `fallback`, as upstream's `fn:base-uri` does (it reads the
/// parentless element's `xml:base` where the document's base URI would be, `DomNavigator::base_uri`)
/// — so that `document()` resolves a relative URI in such a node against exactly what `base-uri()`
/// reports, and the element's `xml:base`, already in a base URI recorded for it (`xsl:copy`), is not
/// applied twice.
///
/// Not `DomNavigator::base_uri`, which is **unresolved** (upstream's written contract, `wave-b`
/// batch (x), C7): `BufferDocNavigator` answers it with the raw value of the nearest `xml:base`, or
/// the document's base URI when there is none — "the consumer resolves" (M4 review R1).
pub(crate) fn node_base_uri(nav: &Nav<'_>, fallback: &str) -> String {
    let mut top = nav.clone();
    while top.move_to_parent() {}
    let own_base_on_a_parentless_element =
        top.node_type() == DomNodeType::Element && attr_ns(&top, XML_NS, "base").is_some();
    let document = match nav.document().document_base_uri() {
        Some(uri) if !uri.is_empty() && !own_base_on_a_parentless_element => {
            resolver::resolve_uri(fallback, uri)
        }
        _ => fallback.to_string(),
    };
    base_uri_of(nav, &document)
}

/// The in-scope namespace bindings of an element, prefix → URI, sorted.
///
/// The `xml` prefix is left out: it is implicit everywhere and never declared.
///
/// A namespace **undeclaration** — `xmlns=""`, or the XML 1.1 `xmlns:p=""` —
/// is *not* a binding: it cancels one. The axis this walks
/// ([`NamespaceAxisScope::ExcludeXml`]) is upstream's view of the
/// *declarations*, which keeps an undeclaration so that serialization and
/// shallow copy can see it; the dedup by prefix therefore does the shadowing
/// for us — the innermost declaration for a prefix is the one that survives —
/// and dropping the zero-length URIs afterwards leaves exactly the bindings
/// that are in scope. An outer default namespace cancelled by an inner
/// `xmlns=""` is already gone at that point, so it cannot leak back.
///
/// Every caller wants bindings, not declarations: §11.1.2's namespace nodes of
/// a literal result element, §5.4.1's statically known namespaces, and the
/// `[xsl:]exclude-result-prefixes` / `[xsl:]extension-element-prefixes` prefix
/// lists — where an undeclared default namespace must make `#default`
/// XTSE0809 (§11.1.3), not resolve to the zero-length URI.
pub fn in_scope_namespaces(nav: &Nav<'_>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut ns = nav.clone();
    if ns.move_to_first_namespace(NamespaceAxisScope::ExcludeXml) {
        loop {
            // `local_name()` of a namespace node is its prefix.
            let prefix = ns.local_name().to_string();
            let uri = ns.value();
            if !out.iter().any(|(p, _)| *p == prefix) {
                out.push((prefix, uri));
            }
            if !ns.move_to_next_namespace(NamespaceAxisScope::ExcludeXml) {
                break;
            }
        }
    }
    out.retain(|(_, uri)| !uri.is_empty());
    out.sort();
    out
}

/// Builds the lexical static context of an element (§5.4.1, §3.5).
///
/// The standard attributes accumulate down the ancestor chain: the nearest
/// `[xsl:]version`, `[xsl:]xpath-default-namespace`, `[xsl:]default-collation`
/// and `xml:base` win, while `[xsl:]exclude-result-prefixes` and
/// `[xsl:]extension-element-prefixes` are unioned — §11.1.2: "the set of
/// excluded namespaces for a literal result element … is the union of the sets
/// of excluded namespaces of its ancestors".
pub fn static_context_of(
    nav: &Nav<'_>,
    module: &Module<'_>,
    collations: &CollationRegistry,
) -> Result<StaticContext, XsltError> {
    let mut ancestors: Vec<Nav<'_>> = Vec::new();
    let mut cursor = nav.clone();
    loop {
        if cursor.node_type() == DomNodeType::Element {
            ancestors.push(cursor.clone());
        }
        if !cursor.move_to_parent() {
            break;
        }
    }
    ancestors.reverse();

    let namespaces = in_scope_namespaces(nav);
    let mut ctx = StaticContext {
        module: module.id,
        namespaces,
        xpath_default_namespace: String::new(),
        default_collation: CODEPOINT_COLLATION.to_string(),
        base_uri: module.uri.clone(),
        compat: CompatMode::Xslt20,
        excluded_namespaces: Vec::new(),
        extension_namespaces: Vec::new(),
    };
    for a in &ancestors {
        // The prefix lists resolve in the scope of the element that carries
        // them, not in the scope of the element the context is being built
        // for, so each ancestor's own in-scope namespaces are needed.
        let in_scope = in_scope_namespaces(a);
        if let Some(base) = attr_ns(a, XML_NS, "base") {
            ctx.base_uri = resolver::resolve_uri(&ctx.base_uri, &base);
        }
        // §3.9: "These rules do not apply to the version attribute of the
        // xsl:output element, which has an entirely different purpose: it is
        // used to define the version of the output method to be used for
        // serialization."
        let is_output = a.namespace_uri() == XSLT_NS && a.local_name() == "output";
        if let Some(v) = standard_attr(a, "version").filter(|_| !is_output) {
            if !crate::compile::checks::is_xs_decimal(&v) {
                return Err(XsltError::new(
                    codes::XTSE0110,
                    format!("version {v:?} is not an xs:decimal"),
                ));
            }
            let n = whitespace::trim(&v).parse::<f64>().map_err(|_| {
                XsltError::new(codes::XTSE0110, format!("version {v:?} is not a number"))
            })?;
            ctx.compat = CompatMode::of_version(n);
        }
        if let Some(v) = standard_attr(a, "xpath-default-namespace") {
            ctx.xpath_default_namespace = v;
        }
        if let Some(v) = standard_attr(a, "default-collation") {
            // §5.4.1: a whitespace-separated list of collation URIs, each
            // resolved against the base URI of the attribute's parent element;
            // the first one the processor recognizes is used.
            //
            // > [ERR XTSE0125] It is a static error if the value of an
            // > [xsl:]default-collation attribute, after resolving against the
            // > base URI, contains no URI that the implementation recognizes as
            // > a collation URI.
            let base = base_uri_of(a, &module.uri);
            let candidates: Vec<String> = whitespace::tokens(&v)
                .map(|token| resolver::resolve_uri(&base, token))
                .collect();
            ctx.default_collation = match candidates.iter().find(|uri| collations.recognizes(uri)) {
                Some(uri) => uri.clone(),
                // A UCA collation of F&O 3.1 is a missing feature, not a
                // broken stylesheet, in a build without the `icu-collation`
                // cargo feature (M2k, DESIGN §9.5), which brings its locale
                // data.
                None => match candidates.iter().find_map(|uri| uca_not_implemented(uri)) {
                    Some(missing) => return Err(missing.with_location(module.location(a))),
                    None => {
                        return Err(XsltError::new(
                            codes::XTSE0125,
                            format!(
                                "no collation URI in default-collation={v:?} is one this processor \
                                 recognizes"
                            ),
                        )
                        .with_location(module.location(a)))
                    }
                },
            };
        }
        if let Some(v) = standard_attr(a, "exclude-result-prefixes") {
            for token in whitespace::tokens(&v) {
                push_namespace(
                    &mut ctx.excluded_namespaces,
                    token,
                    &in_scope,
                    PrefixList::ExcludeResult,
                )?;
            }
        }
        if let Some(v) = standard_attr(a, "extension-element-prefixes") {
            for token in whitespace::tokens(&v) {
                push_namespace(
                    &mut ctx.extension_namespaces,
                    token,
                    &in_scope,
                    PrefixList::ExtensionElement,
                )?;
            }
        }
    }
    Ok(ctx)
}

/// Which attribute a prefix list serves — and so which error an unusable token
/// raises. The two lists look alike and are spelled alike, but §11.1.3 and
/// §18.2.1 give them different codes, and §11.1.3 gives `#default` a code of
/// its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrefixList {
    /// `[xsl:]exclude-result-prefixes` (§11.1.3): XTSE0808 for a prefix with no
    /// binding in scope, XTSE0809 for `#default` with no default namespace.
    ExcludeResult,
    /// `[xsl:]extension-element-prefixes` (§18.2.1): XTSE1430 for both.
    ExtensionElement,
}

impl PrefixList {
    /// The attribute name, for the message.
    fn attribute(self) -> &'static str {
        match self {
            Self::ExcludeResult => "exclude-result-prefixes",
            Self::ExtensionElement => "extension-element-prefixes",
        }
    }

    /// The code for a token other than `#default` that names no in-scope
    /// prefix.
    fn unbound_code(self) -> &'static str {
        match self {
            Self::ExcludeResult => codes::XTSE0808,
            Self::ExtensionElement => codes::XTSE1430,
        }
    }

    /// The code for `#default` where no default namespace is in force.
    fn no_default_code(self) -> &'static str {
        match self {
            Self::ExcludeResult => codes::XTSE0809,
            Self::ExtensionElement => codes::XTSE1430,
        }
    }
}

/// One token of `exclude-result-prefixes` / `extension-element-prefixes`:
/// an NCName, `#default`, or `#all`, resolved to the namespace URI (or URIs)
/// it designates in `in_scope`.
///
/// > \[ERR XTSE0808\] It is a static error if a namespace prefix is used within
/// > the `[xsl:]exclude-result-prefixes` attribute and there is no namespace
/// > binding in scope for that prefix.
/// >
/// > \[ERR XTSE0809\] It is a static error if the value `#default` is used
/// > within the `[xsl:]exclude-result-prefixes` attribute and the parent
/// > element of the `[xsl:]exclude-result-prefixes` attribute has no default
/// > namespace.
/// >
/// > \[ERR XTSE1430\] It is a static error if there is no namespace bound to
/// > the prefix on the element bearing the `[xsl:]extension-element-prefixes`
/// > attribute or, when `#default` is specified, if there is no default
/// > namespace.
///
/// XTSE1430 is scoped to `extension-element-prefixes`; the engine used to raise
/// it for both lists. The tokens are separated by *whitespace* only — a comma
/// is part of the token, and so is simply not a prefix in scope
/// (`error-0808b`).
fn push_namespace(
    out: &mut Vec<String>,
    token: &str,
    in_scope: &[(String, String)],
    list: PrefixList,
) -> Result<(), XsltError> {
    let mut push = |uri: &str| {
        if !out.iter().any(|u| u == uri) {
            out.push(uri.to_string());
        }
    };
    match token {
        "#all" => {
            for (_, uri) in in_scope {
                push(uri);
            }
        }
        "#default" => match in_scope.iter().find(|(p, _)| p.is_empty()) {
            Some((_, uri)) => push(uri),
            // An `xmlns=""` undeclaration leaves no default-namespace binding,
            // so `#default` designates nothing — see `in_scope_namespaces`.
            None => {
                return Err(XsltError::new(
                    list.no_default_code(),
                    format!(
                        "#default is listed in {} but no default namespace is in force",
                        list.attribute()
                    ),
                ))
            }
        },
        other => match in_scope.iter().find(|(p, _)| p == other) {
            Some((_, uri)) => push(uri),
            None => {
                return Err(XsltError::new(
                    list.unbound_code(),
                    format!(
                        "the prefix {other:?} listed in {} is not bound to a namespace",
                        list.attribute()
                    ),
                ))
            }
        },
    }
    Ok(())
}

// ── Whitespace (§4.2) ────────────────────────────────────────────────────

/// Whether a text node of the stylesheet is stripped (§4.2).
///
/// > The XSLT processor must strip from a stylesheet module any text node
/// > whose parent is not an `xsl:text` element and whose content consists
/// > entirely of whitespace characters, unless … an `xml:space` attribute with
/// > the value `preserve` is in scope.
///
/// The test is made on the node's whole [text run](text_run_value): §4.2
/// removes comments and processing instructions first and merges the text
/// nodes that become adjacent, so `   h<!--c-->   ` is one text node `   h   `
/// and neither half is whitespace-only on its own.
pub fn is_stripped_stylesheet_text(nav: &Nav<'_>) -> bool {
    if !nav.node_type().is_text_like() {
        return false;
    }
    if !whitespace::is_all_xml_whitespace(&text_run_value(nav)) {
        return false;
    }
    let mut parent = nav.clone();
    if !parent.move_to_parent() {
        return true;
    }
    if parent.namespace_uri() == XSLT_NS && parent.local_name() == "text" {
        return false;
    }
    !xml_space_preserves(&parent)
}

/// The value of the text node §4.2 makes out of `nav` and its neighbours.
///
/// > The tree representing the stylesheet is preprocessed as follows: All
/// > comments and processing instructions are removed. Any text nodes that are
/// > now adjacent to each other are merged. Any whitespace text node that
/// > satisfies both the following conditions is removed from the tree: …
///
/// A *text run* is a maximal sequence of sibling text nodes separated only by
/// comments and processing instructions; its value is their concatenation.
/// Elements end a run — including one that `use-when` excludes, since §3.12:
/// "Conditional element exclusion happens after stripping of whitespace text
/// nodes from the stylesheet". The parser already merged directly adjacent
/// text, so a run of one node is the common case and costs one sibling probe
/// in each direction.
pub fn text_run_value(nav: &Nav<'_>) -> String {
    let mut first = nav.clone();
    let mut cursor = nav.clone();
    while cursor.move_to_prev_sibling() {
        match cursor.node_type() {
            DomNodeType::Comment | DomNodeType::ProcessingInstruction => {}
            kind if kind.is_text_like() => first = cursor.clone(),
            _ => break,
        }
    }
    let mut value = first.value();
    let mut cursor = first;
    while cursor.move_to_next_sibling() {
        match cursor.node_type() {
            DomNodeType::Comment | DomNodeType::ProcessingInstruction => {}
            kind if kind.is_text_like() => value.push_str(&cursor.value()),
            _ => break,
        }
    }
    value
}

/// Whether `nav` is the first text node of its [text run](text_run_value) —
/// the one that stands for the merged node; the others are part of it.
pub fn starts_text_run(nav: &Nav<'_>) -> bool {
    let mut cursor = nav.clone();
    while cursor.move_to_prev_sibling() {
        match cursor.node_type() {
            DomNodeType::Comment | DomNodeType::ProcessingInstruction => {}
            kind => return !kind.is_text_like(),
        }
    }
    true
}

/// Whether `xml:space="preserve"` is in scope on an element (nearest wins).
pub fn xml_space_preserves(nav: &Nav<'_>) -> bool {
    let mut cursor = nav.clone();
    loop {
        if cursor.node_type() == DomNodeType::Element {
            if let Some(v) = attr_ns(&cursor, XML_NS, "space") {
                return whitespace::trim(&v) == "preserve";
            }
        }
        if !cursor.move_to_parent() {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_offset_becomes_a_line_and_column() {
        let text = "<a>\n  <b/>\n</a>";
        let starts = line_starts(text);
        assert_eq!(starts, vec![0, 4, 11]);
        let at = |offset: usize| {
            let line = match starts.binary_search(&offset) {
                Ok(i) => i,
                Err(i) => i - 1,
            };
            (line as u32 + 1, (offset - starts[line]) as u32 + 1)
        };
        assert_eq!(at(0), (1, 1));
        assert_eq!(at(4), (2, 1));
        assert_eq!(at(6), (2, 3));
        assert_eq!(at(11), (3, 1));
    }

    #[test]
    fn compat_mode_follows_the_version_number() {
        assert_eq!(CompatMode::of_version(1.0), CompatMode::BackwardsCompatible);
        assert_eq!(CompatMode::of_version(2.0), CompatMode::Xslt20);
        assert_eq!(CompatMode::of_version(3.0), CompatMode::ForwardsCompatible);
    }

    #[test]
    fn identical_static_contexts_share_an_id() {
        let mut interner = StaticContexts::default();
        let ctx = StaticContext {
            module: ModuleId(0),
            namespaces: vec![("p".into(), "urn:p".into())],
            xpath_default_namespace: String::new(),
            default_collation: CODEPOINT_COLLATION.into(),
            base_uri: "urn:m".into(),
            compat: CompatMode::Xslt20,
            excluded_namespaces: Vec::new(),
            extension_namespaces: Vec::new(),
        };
        let a = interner.intern(ctx.clone());
        let b = interner.intern(ctx.clone());
        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
        let other = StaticContext {
            compat: CompatMode::BackwardsCompatible,
            ..ctx
        };
        assert_ne!(interner.intern(other), a);
        assert_eq!(interner.len(), 2);
    }
}
