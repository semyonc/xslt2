//! The compiler: stylesheet modules → declaration tables and `Instruction`
//! trees (§7).
//!
//! One compilation owns a `bumpalo::Bump` in which every module's
//! `BufferDocument` lives; the [`CompiledStylesheet`] it produces borrows only
//! the caller's [`NameTable`] and [`XsltFunctions`], so the arena is dropped as
//! soon as `compile_*` returns.

mod analyze;
mod attributes;
mod avt;
pub(crate) mod checks;
mod decimal_format;
mod declarations;
mod expressions;
mod functions;
mod grouping;
mod instructions;
mod keys;
mod literal;
mod modes;
mod number;
mod output;
mod patterns;
pub(crate) mod result_document;
mod tree;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::XPathContext;

use crate::error::XsltError;
use crate::functions::{StylesheetCatalog, StylesheetFunctionTable, XsltFunctions};
use crate::model::*;
use crate::names::QName;
use crate::output::OutputDefinition;
use crate::pattern::Pattern;
use crate::resolver::{self, FileResolver, Resolver};
use crate::schema::SchemaContext;
use crate::stylesheet::{static_context_of, CompatMode, Loader, ModuleSet, StaticContexts};

/// Compiles stylesheets against a caller-owned name table and function set.
///
/// Both are borrowed, not owned: a `NameTable` interns the names of every
/// document that takes part in a transformation (§6.6), and `XPathContext`
/// keeps its function catalog as a reference, so a `CompiledStylesheet` that
/// owned the set would be self-referential (M0b, Q35).
///
/// [`Compiler::new`] is a **basic** XSLT processor (§21.1: XTSE1650 for
/// `xsl:import-schema`, XTSE1660 for `[xsl:]type` and a non-`strip`
/// validation, XTDE1665 for typed input); [`Compiler::schema_aware`] is a
/// **schema-aware** one (§21.2), compiling against a caller-owned
/// [`SchemaContext`] (M4 plan D-1, D-6: a run-time choice, not a cargo
/// feature).
pub struct Compiler<'a> {
    names: &'a NameTable,
    functions: &'a XsltFunctions,
    resolver: Arc<dyn Resolver>,
    schema: Option<&'a SchemaContext>,
}

impl<'a> Compiler<'a> {
    /// A compiler that reads modules from the file system.
    pub fn new(names: &'a NameTable, functions: &'a XsltFunctions) -> Self {
        Self {
            names,
            functions,
            resolver: Arc::new(FileResolver),
            schema: None,
        }
    }

    /// A schema-aware compiler (§21.2) for stylesheets whose
    /// `xsl:import-schema` declarations `context` was built from
    /// ([`SchemaContext::builder`]).
    ///
    /// The stylesheet's names are the context's: its schema set owns the name
    /// table, which the XPath binder looks schema types up by and which every
    /// document of every transformation is built with (DESIGN §6.6). Every
    /// XPath expression sees the set's types and declarations (§3.13, M4 plan
    /// D-7). Compiling a stylesheet that imports something the context did not
    /// load is refused (`XSLT2-INTERNAL`).
    pub fn schema_aware(context: &'a SchemaContext, functions: &'a XsltFunctions) -> Self {
        Self {
            names: context.names(),
            functions,
            resolver: Arc::new(FileResolver),
            schema: Some(context),
        }
    }

    /// Whether this is a schema-aware compiler.
    pub fn is_schema_aware(&self) -> bool {
        self.schema.is_some()
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
            .schema_aware(self.schema.is_some())
            .load(uri, text)?;
        // §10.3: the stylesheet functions join the in-scope functions of every
        // expression. The catalog that adds them lives only as long as the
        // compilation (see `StylesheetCatalog`); the builder fills the table
        // before it compiles the first expression.
        let table = std::cell::OnceCell::new();
        let catalogs = CompileCatalogs {
            plain: StylesheetCatalog::new(self.functions, &table, false),
            backwards: StylesheetCatalog::new(self.functions, &table, true),
            table: &table,
        };
        let schema = self.schema.map(SchemaContext::schema_set);
        let mut builder = Builder::new(self.names, self.functions, &modules, &catalogs, schema);
        if let Some(context) = self.schema {
            // D-1: the context must have been built from this stylesheet.
            crate::schema::check_coverage(context, &modules)?;
        }
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

/// The two function catalogs expressions are **compiled** against — one per
/// compatibility mode — and the stylesheet-function table both consult
/// (§10.3, §3.8).
struct CompileCatalogs<'a, 'm> {
    plain: StylesheetCatalog<'a, 'm>,
    backwards: StylesheetCatalog<'a, 'm>,
    table: &'m std::cell::OnceCell<StylesheetFunctionTable>,
}

struct Builder<'a, 'm, 't> {
    names: &'a NameTable,
    functions: &'a XsltFunctions,
    /// The schema set of a schema-aware compilation (§21.2), in every XPath
    /// static context; `None` for a basic one.
    schema: Option<&'a xsd_schema::SchemaSet>,
    /// Each module's `default-validation` (§3.6), by `ModuleId`: the
    /// validation of a node constructor that has no `[xsl:]validation` or
    /// `[xsl:]type` of its own.
    module_validation: Vec<Validation>,
    modules: &'m ModuleSet<'t>,
    catalogs: &'m CompileCatalogs<'a, 'm>,
    static_contexts: StaticContexts,
    /// The static context of each [`StaticContextId`] as evaluation sees it,
    /// with the caller's [`XsltFunctions`] as its catalog; these are kept in
    /// the compiled stylesheet.
    contexts: Vec<XPathContext<'a>>,
    /// The same static contexts as the binder sees them, with the stylesheet
    /// functions in scope (§10.3). Every expression is compiled against one of
    /// these; they die with the compilation.
    compile_contexts: Vec<XPathContext<'m>>,
    exprs: Vec<CompiledExpr>,
    patterns: Vec<Pattern>,
    templates: Vec<TemplateRule>,
    named: HashMap<QName, (TemplateId, u32)>,
    globals: Vec<GlobalVar>,
    global_index: HashMap<QName, usize>,
    attribute_sets: HashMap<QName, AttributeSet>,
    /// The keys (§16.3.1) and their names.
    keys: Vec<Key>,
    key_index: HashMap<QName, KeyId>,
    space_rules: Vec<SpaceRule>,
    output: OutputDefinition,
    named_outputs: HashMap<QName, OutputDefinition>,
    /// Every `xsl:character-map` declaration (§20.1), resolved by
    /// `finish_output_definitions` into `expanded_character_maps`.
    character_maps: Vec<output::CharacterMapDecl>,
    expanded_character_maps: HashMap<QName, crate::serialize::CharacterMap>,
    /// Every `xsl:decimal-format` declaration (§16.4.1), by name (`None` for
    /// the unnamed decimal format), merged by `finish_decimal_formats` into
    /// `decimal_formats`.
    decimal_format_decls: HashMap<Option<QName>, decimal_format::DecimalFormatDecls>,
    decimal_formats: crate::model::DecimalFormats,
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
    /// The names every `xsl:result-document` refers to, for the static checks
    /// that need every declaration (§19.1).
    result_document_refs: Vec<result_document::ResultDocumentReferences>,
    /// Every `xsl:function` declaration, from the pre-pass (§10.3).
    function_decls: Vec<functions::FunctionDecl<'t>>,
    /// The compiled winners, indexed by `StylesheetFunctionId`; `None` until
    /// the body is compiled.
    functions_table: Vec<Option<StylesheetFunction>>,
    /// The bodies of the `xsl:function` declarations that lost to one of
    /// higher import precedence: never called, but checked (§2.9).
    losing_function_bodies: Vec<Body>,
    function_handles: HashMap<xsd_schema::xpath::functions::FunctionHandle, StylesheetFunctionId>,
    /// How many `xsl:number` instructions have been compiled: each gets the
    /// next slot of the run's numbering memo (`NumberInstruction::memo_slot`).
    number_instructions: usize,
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
    fn new(
        names: &'a NameTable,
        functions: &'a XsltFunctions,
        modules: &'m ModuleSet<'t>,
        catalogs: &'m CompileCatalogs<'a, 'm>,
        schema: Option<&'a xsd_schema::SchemaSet>,
    ) -> Self {
        Self {
            names,
            functions,
            schema,
            module_validation: modules
                .modules
                .iter()
                .map(attributes::module_default_validation)
                .collect(),
            modules,
            catalogs,
            static_contexts: StaticContexts::default(),
            contexts: Vec::new(),
            compile_contexts: Vec::new(),
            exprs: Vec::new(),
            patterns: Vec::new(),
            templates: Vec::new(),
            named: HashMap::new(),
            globals: Vec::new(),
            global_index: HashMap::new(),
            attribute_sets: HashMap::new(),
            keys: Vec::new(),
            key_index: HashMap::new(),
            space_rules: Vec::new(),
            output: OutputDefinition::default(),
            named_outputs: HashMap::new(),
            character_maps: Vec::new(),
            expanded_character_maps: HashMap::new(),
            decimal_format_decls: HashMap::new(),
            decimal_formats: crate::model::DecimalFormats::default(),
            scope: Vec::new(),
            frame_size: 0,
            decl_order: 0,
            aliases: HashMap::new(),
            attr_set_refs: Vec::new(),
            result_document_refs: Vec::new(),
            function_decls: Vec::new(),
            functions_table: Vec::new(),
            losing_function_bodies: Vec::new(),
            function_handles: HashMap::new(),
            number_instructions: 0,
        }
    }

    fn build(&mut self) -> Result<CompiledStylesheet<'a>, XsltError> {
        // §3.4: the element table first, over every module, so that a misplaced
        // element or a content model the spec does not allow is reported even
        // when it stands in a subtree nothing would compile (M2c).
        checks::check(self.modules, self.functions.collations(), self.schema)?;
        // §10.3: every stylesheet function is in the static context of every
        // expression, wherever it is declared — so they are all collected, and
        // their winners settled, before the first expression is compiled.
        self.collect_functions()?;
        self.build_declarations()?;
        // Needs every key: a `key()` call may reach any of them.
        self.settle_key_group_dependence();
        // §10.1.1's XTSE0680 needs the declaration tables, so it runs over the
        // compiled tree rather than over the stylesheet tree.
        checks::check_call_template_params(self)?;
        let modes = self.build_mode_table();
        let principal = self.modules.module(self.modules.principal);
        let base_uri = principal.uri.clone();
        let (principal_compat, principal_version_1_0) = {
            let mut nav = principal.doc.create_navigator();
            if crate::stylesheet::move_to_document_element(&mut nav) {
                // §20's default output method: "if the version attribute of the
                // xsl:stylesheet element of the principal stylesheet module has
                // the value 1.0" (`xsl:version` on a simplified stylesheet).
                let version = if xsd_schema::navigator::DomNavigator::namespace_uri(&nav)
                    == crate::names::XSLT_NS
                {
                    crate::stylesheet::attr_local(&nav, "version")
                } else {
                    crate::stylesheet::attr_ns(&nav, crate::names::XSLT_NS, "version")
                };
                let one = version
                    .as_deref()
                    .map(crate::whitespace::trim)
                    .and_then(|v| v.parse::<f64>().ok())
                    == Some(1.0);
                (
                    static_context_of(&nav, principal, self.functions.collations())?.compat,
                    one,
                )
            } else {
                (CompatMode::Xslt20, false)
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
            keys: std::mem::take(&mut self.keys),
            key_index: std::mem::take(&mut self.key_index),
            output: std::mem::take(&mut self.output),
            named_outputs: std::mem::take(&mut self.named_outputs),
            character_maps: std::mem::take(&mut self.expanded_character_maps),
            decimal_formats: std::mem::take(&mut self.decimal_formats),
            principal_version_1_0,
            space_rules: std::mem::take(&mut self.space_rules),
            functions_table: std::mem::take(&mut self.functions_table)
                .into_iter()
                .map(|f| f.expect("every winning xsl:function is compiled by build_declarations"))
                .collect(),
            function_handles: std::mem::take(&mut self.function_handles),
            module_uris: self.modules.modules.iter().map(|m| m.uri.clone()).collect(),
            base_uri,
            principal_compat,
            schema: self.schema,
            final_validation: self.module_validation[self.modules.principal.0 as usize].clone(),
        })
    }
}
