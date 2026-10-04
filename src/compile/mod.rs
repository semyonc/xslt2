//! The compiler: stylesheet modules → declaration tables and `Instruction`
//! trees (§7).
//!
//! One compilation owns a `bumpalo::Bump` in which every module's
//! `BufferDocument` lives; the [`CompiledStylesheet`] it produces borrows only
//! the caller's [`NameTable`] and [`XsltFunctions`], so the arena is dropped as
//! soon as `compile_*` returns.

mod attributes;
mod avt;
pub(crate) mod checks;
mod declarations;
mod expressions;
mod instructions;
mod literal;
mod modes;
mod patterns;
mod tree;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::XPathContext;

use crate::error::XsltError;
use crate::functions::XsltFunctions;
use crate::model::*;
use crate::names::QName;
use crate::output::OutputDefinition;
use crate::pattern::Pattern;
use crate::resolver::{self, FileResolver, Resolver};
use crate::stylesheet::{static_context_of, CompatMode, Loader, ModuleSet, StaticContexts};

/// Compiles stylesheets against a caller-owned name table and function set.
///
/// Both are borrowed, not owned: a `NameTable` interns the names of every
/// document that takes part in a transformation (§6.6), and `XPathContext`
/// keeps its function catalog as a reference, so a `CompiledStylesheet` that
/// owned the set would be self-referential (M0b, Q35).
pub struct Compiler<'a> {
    names: &'a NameTable,
    functions: &'a XsltFunctions,
    resolver: Arc<dyn Resolver>,
}

impl<'a> Compiler<'a> {
    /// A compiler that reads modules from the file system.
    pub fn new(names: &'a NameTable, functions: &'a XsltFunctions) -> Self {
        Self {
            names,
            functions,
            resolver: Arc::new(FileResolver),
        }
    }

    /// Where `xsl:include`, `xsl:import` and `document()` read from.
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// The resolver this compiler was given, for a transformation to reuse.
    pub fn resolver(&self) -> Arc<dyn Resolver> {
        Arc::clone(&self.resolver)
    }

    /// Compiles the stylesheet module at `path`.
    pub fn compile_file(&self, path: &Path) -> Result<CompiledStylesheet<'a>, XsltError> {
        let uri = resolver::path_to_uri(path);
        self.compile_uri(&uri)
    }

    /// Compiles the stylesheet module the resolver serves at `uri`.
    pub fn compile_uri(&self, uri: &str) -> Result<CompiledStylesheet<'a>, XsltError> {
        self.run(uri, None)
    }

    /// Compiles `source` as the module named `base_uri`.
    pub fn compile_str(
        &self,
        source: &str,
        base_uri: &str,
    ) -> Result<CompiledStylesheet<'a>, XsltError> {
        self.run(base_uri, Some(source.to_string()))
    }

    fn run(&self, uri: &str, text: Option<String>) -> Result<CompiledStylesheet<'a>, XsltError> {
        let arena = Bump::new();
        // SAFETY-free lifetime note: `modules` borrows `arena`, and every value
        // that escapes this function is owned, so the arena dies here.
        let modules = Loader::new(&arena, self.names, self.resolver.as_ref(), self.functions)
            .load(uri, text)?;
        let mut builder = Builder::new(self.names, self.functions, &modules);
        builder.build()
    }
}

// ── The scope of variable names an expression is compiled against ────────

/// One variable binding in scope.
///
/// A variable is identified by its **expanded** QName (§5.1: "the name of a
/// variable … is a QName … expanded using the in-scope namespaces of the
/// element on which it appears"), so only `name` decides which binding a
/// reference resolves to. The lexical spelling used at the declaration is
/// deliberately not kept: the upstream XPath parser resolves the names it is
/// given in the static context of the expression being compiled, which is a
/// different element with different prefixes, so the spelling is recomputed
/// per consuming context by [`Builder::visible_vars`].
struct ScopeEntry {
    name: QName,
    location: VarLocation,
}

// ── Builder ──────────────────────────────────────────────────────────────

struct Builder<'a, 'm, 't> {
    names: &'a NameTable,
    functions: &'a XsltFunctions,
    modules: &'m ModuleSet<'t>,
    static_contexts: StaticContexts,
    contexts: Vec<XPathContext<'a>>,
    exprs: Vec<CompiledExpr>,
    patterns: Vec<Pattern>,
    templates: Vec<TemplateRule>,
    named: HashMap<QName, (TemplateId, u32)>,
    globals: Vec<GlobalVar>,
    global_index: HashMap<QName, usize>,
    attribute_sets: HashMap<QName, AttributeSet>,
    space_rules: Vec<SpaceRule>,
    output: OutputDefinition,
    named_outputs: HashMap<QName, OutputDefinition>,
    /// Variables in scope at the point currently being compiled.
    scope: Vec<ScopeEntry>,
    /// Slots used by the frame currently being compiled.
    frame_size: usize,
    decl_order: u32,
    /// `xsl:namespace-alias` (§11.1.4).
    ///
    /// Keyed by the **literal namespace URI** — the URI the `stylesheet-prefix`
    /// is bound to, not the prefix — because §11.1.4 aliases the namespace, so
    /// any prefix bound to that URI in a literal result element is rewritten.
    aliases: HashMap<String, Alias>,
    /// Every name a `use-attribute-sets` / `xsl:use-attribute-sets` attribute
    /// mentions, for the static half of XTSE0710 (§10.2).
    attr_set_refs: Vec<QName>,
}

/// One winning `xsl:namespace-alias` declaration (§11.1.4).
struct Alias {
    /// The target namespace URI — what appears in the result tree.
    uri: String,
    /// The prefix the target namespace is written with: "the namespace prefix
    /// in that expanded-QName is replaced by the prefix indicated by the
    /// `result-prefix` attribute".
    prefix: String,
    /// The import precedence of the declaration that won. Kept because
    /// XTSE0810 is about two declarations at the **same** precedence, and
    /// "the declaration with the highest import precedence is used".
    precedence: u32,
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    fn new(names: &'a NameTable, functions: &'a XsltFunctions, modules: &'m ModuleSet<'t>) -> Self {
        Self {
            names,
            functions,
            modules,
            static_contexts: StaticContexts::default(),
            contexts: Vec::new(),
            exprs: Vec::new(),
            patterns: Vec::new(),
            templates: Vec::new(),
            named: HashMap::new(),
            globals: Vec::new(),
            global_index: HashMap::new(),
            attribute_sets: HashMap::new(),
            space_rules: Vec::new(),
            output: OutputDefinition::default(),
            named_outputs: HashMap::new(),
            scope: Vec::new(),
            frame_size: 0,
            decl_order: 0,
            aliases: HashMap::new(),
            attr_set_refs: Vec::new(),
        }
    }

    fn build(&mut self) -> Result<CompiledStylesheet<'a>, XsltError> {
        // §3.4: the element table first, over every module, so that a misplaced
        // element or a content model the spec does not allow is reported even
        // when it stands in a subtree nothing would compile (M2c).
        checks::check(self.modules, self.functions.collations())?;
        self.build_declarations()?;
        // §10.1.1's XTSE0680 needs the declaration tables, so it runs over the
        // compiled tree rather than over the stylesheet tree.
        checks::check_call_template_params(self)?;
        let modes = self.build_mode_table();
        let principal = self.modules.module(self.modules.principal);
        let base_uri = principal.uri.clone();
        let principal_compat = {
            let mut nav = principal.doc.create_navigator();
            if crate::stylesheet::move_to_document_element(&mut nav) {
                static_context_of(&nav, principal, self.functions.collations())?.compat
            } else {
                CompatMode::Xslt20
            }
        };
        Ok(CompiledStylesheet {
            names: self.names,
            functions: self.functions,
            contexts: std::mem::take(&mut self.contexts),
            static_contexts: std::mem::take(&mut self.static_contexts),
            exprs: std::mem::take(&mut self.exprs),
            patterns: std::mem::take(&mut self.patterns),
            templates: std::mem::take(&mut self.templates),
            named: self
                .named
                .iter()
                .map(|(k, (id, _))| (k.clone(), *id))
                .collect(),
            globals: std::mem::take(&mut self.globals),
            global_index: std::mem::take(&mut self.global_index),
            modes,
            attribute_sets: std::mem::take(&mut self.attribute_sets),
            output: std::mem::take(&mut self.output),
            named_outputs: std::mem::take(&mut self.named_outputs),
            space_rules: std::mem::take(&mut self.space_rules),
            module_uris: self.modules.modules.iter().map(|m| m.uri.clone()).collect(),
            base_uri,
            principal_compat,
        })
    }
}
