//! The compiler: stylesheet modules → declaration tables and `Instruction`
//! trees (§7).
//!
//! One compilation owns a `bumpalo::Bump` in which every module's
//! `BufferDocument` lives; the [`CompiledStylesheet`] it produces borrows only
//! the caller's [`NameTable`] and [`XsltFunctions`], so the arena is dropped as
//! soon as `compile_*` returns.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::{XPathContext, XPathExpr};

use crate::error::{codes, Location, XsltError};
use crate::functions::XsltFunctions;
use crate::model::*;
use crate::names::{parse_yes_no, resolve_qname, split_qname, QName, XSLT_NS};
use crate::output::{parse_method, OutputDefinition};
use crate::pattern::{self, IndexKey, Pattern, PatternSink};
use crate::receiver::{Name, Nav};
use crate::resolver::{self, FileResolver, Resolver};
use crate::stylesheet::{
    attr_local, attributes, in_scope_namespaces, is_stripped_stylesheet_text, static_context_of,
    CompatMode, DeclKind, Loader, Module, ModuleId, ModuleSet, StaticContextId, StaticContexts,
};
use crate::whitespace;

/// The standard attributes §3.5 allows on every XSLT element.
const STANDARD_ATTRS: &[&str] = &[
    "version",
    "exclude-result-prefixes",
    "extension-element-prefixes",
    "xpath-default-namespace",
    "default-collation",
    "use-when",
];

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
    /// Keyed by the namespace **URI** the `stylesheet-prefix` is bound to — not
    /// by a prefix — because §11.1.4 aliases the namespace, so any prefix bound
    /// to that URI in a literal result element is rewritten. The value is the
    /// result namespace URI and the preferred prefix to write it with.
    aliases: HashMap<String, (String, String)>,
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
        }
    }

    fn build(&mut self) -> Result<CompiledStylesheet<'a>, XsltError> {
        self.build_declarations()?;
        let modes = self.build_mode_table();
        let principal = self.modules.module(self.modules.principal);
        let base_uri = principal.uri.clone();
        let principal_compat = {
            let mut nav = principal.doc.create_navigator();
            if crate::stylesheet::move_to_document_element(&mut nav) {
                static_context_of(&nav, principal)?.compat
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

    /// §3.7: a simplified stylesheet module is one template rule matching `/`
    /// whose body is the literal result element.
    ///
    /// > The literal result element is treated as if it were the child of an
    /// > `xsl:template` element with a `match` attribute of `/`.
    fn simplified_template(
        &mut self,
        module_id: ModuleId,
        root: &Nav<'t>,
        precedence: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(root, module)?;
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let instruction = self.literal_element(root, self.modules.module(module_id), ctx)?;
        self.scope.truncate(scope_mark);
        let pattern = self.compile_pattern("/", ctx)?;
        self.decl_order += 1;
        let id = TemplateId(self.templates.len() as u32);
        let loc = self.modules.module(module_id).location(root);
        self.templates.push(TemplateRule {
            id,
            name: None,
            pattern: Some(pattern),
            modes: vec![ModeKey::Default],
            all_modes: false,
            explicit_priority: None,
            precedence,
            subtree_min: precedence,
            decl_order: self.decl_order,
            params: Vec::new(),
            body: Body(vec![instruction]),
            as_type: None,
            frame_size: self.frame_size,
            module: module_id,
            loc,
        });
        Ok(())
    }

    fn build_declarations(&mut self) -> Result<(), XsltError> {
        // Pass 1: the global variables and parameters, so that every
        // expression can be compiled against a complete global scope.
        let mut global_decls: Vec<(u32, ModuleId, Nav<'t>)> = Vec::new();
        for unit in &self.modules.units {
            for (module_id, nav, kind) in &unit.decls {
                if *kind != DeclKind::Declaration || nav.namespace_uri() != XSLT_NS {
                    continue;
                }
                if self.excluded(nav, *module_id) {
                    continue;
                }
                if matches!(nav.local_name(), "variable" | "param") {
                    global_decls.push((unit.precedence, *module_id, nav.clone()));
                }
            }
        }

        // A declaration M1 does not implement is reported before any template
        // body is compiled: otherwise a template calling a function that
        // `xsl:function` would have declared fails first, and the case is
        // classified as a wrong answer rather than a missing feature.
        for unit in &self.modules.units {
            for (module_id, nav, kind) in &unit.decls {
                if *kind != DeclKind::Declaration || nav.namespace_uri() != XSLT_NS {
                    continue;
                }
                if let Some(milestone) = unimplemented_declaration(nav.local_name()) {
                    return Err(XsltError::not_implemented(
                        format!("xsl:{}", nav.local_name()),
                        milestone,
                    )
                    .with_location(self.modules.module(*module_id).location(nav)));
                }
            }
        }
        self.declare_globals(&global_decls)?;

        // Pass 2: everything, now that names resolve.
        for unit_index in 0..self.modules.units.len() {
            let precedence = self.modules.units[unit_index].precedence;
            let subtree_min = self.modules.units[unit_index].subtree_min;
            let decls: Vec<(ModuleId, Nav<'t>, DeclKind)> =
                self.modules.units[unit_index].decls.clone();
            for (module_id, nav, kind) in decls {
                if self.excluded(&nav, module_id) {
                    continue;
                }
                let module = self.modules.module(module_id);
                if kind == DeclKind::SimplifiedRoot {
                    self.simplified_template(module_id, &nav, precedence)?;
                    continue;
                }
                if nav.namespace_uri() != XSLT_NS {
                    // §3.6.2: a top-level element in a non-null namespace that
                    // is not the XSLT namespace is user data and is ignored.
                    if nav.namespace_uri().is_empty() {
                        return Err(XsltError::new(
                            codes::XTSE0130,
                            format!(
                                "the top-level element {} is in no namespace; user-defined \
                                 top-level elements must be in a namespace",
                                nav.local_name()
                            ),
                        )
                        .with_location(module.location(&nav)));
                    }
                    continue;
                }
                self.declaration(&nav, module_id, precedence, subtree_min)?;
            }
        }
        Ok(())
    }

    /// Whether an element has any content that is not stripped whitespace
    /// (§4.2) and is not excluded by §3.12.
    fn has_content(&self, nav: &Nav<'t>, module_id: ModuleId) -> bool {
        let mut child = nav.clone();
        if !child.move_to_first_child() {
            return false;
        }
        loop {
            match child.node_type() {
                DomNodeType::Element if self.excluded(&child, module_id) => {}
                DomNodeType::Element => return true,
                kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => return true,
                _ => {}
            }
            if !child.move_to_next_sibling() {
                return false;
            }
        }
    }

    /// Whether §3.12 effectively excludes this element, and with it its whole
    /// subtree.
    ///
    /// The decision was made once, by the [`Loader`], before the module graph
    /// was even resolved; every traversal in the compiler consults it, so no
    /// instruction compiler has to remember `use-when`.
    fn excluded(&self, nav: &Nav<'t>, module_id: ModuleId) -> bool {
        self.modules.module(module_id).is_excluded(nav)
    }

    // ── Static contexts ──────────────────────────────────────────────────

    /// Interns the lexical static context of `nav` and builds its
    /// [`XPathContext`] the first time it is seen (§7.2).
    fn context_of(
        &mut self,
        nav: &Nav<'t>,
        module: &Module<'t>,
    ) -> Result<StaticContextId, XsltError> {
        let sc = static_context_of(nav, module)?;
        let before = self.static_contexts.len();
        let id = self.static_contexts.intern(sc);
        if self.static_contexts.len() > before {
            let sc = self.static_contexts.get(id);
            let mut ctx = XPathContext::new(self.names)
                .with_namespaces(sc.snapshot(self.names))
                .with_base_uri(sc.base_uri.clone())
                .with_function_catalog(self.functions.catalog())
                // §9.10 row 1: a `version < 2.0` region keeps XPath 2.0 syntax
                // and takes XPath 1.0 compatibility-mode semantics.
                .with_xpath10_compatibility(sc.compat.is_backwards());
            if !sc.xpath_default_namespace.is_empty() {
                ctx = ctx.with_default_element_ns(self.names.add(&sc.xpath_default_namespace));
            }
            self.contexts.push(ctx);
        }
        debug_assert_eq!(self.contexts.len(), self.static_contexts.len());
        Ok(id)
    }

    // ── Expressions ──────────────────────────────────────────────────────

    /// The variables an expression compiled in `ctx` can refer to, each under
    /// a lexical name that resolves to its expanded QName **in that context**.
    ///
    /// §5.1/§9.1 identify a variable by its expanded QName, but
    /// `XPathExpr::compile_with_vars` is handed *lexical* names and resolves
    /// each of them in the static context of the expression being compiled.
    /// Passing the spelling used at the declaration is therefore wrong twice
    /// over: a reference written with a different prefix bound to the same
    /// namespace would not find the variable, and a declaration prefix that is
    /// not bound at the reference site would make every expression there fail
    /// to compile with XPST0081 — even one that never mentions the variable.
    ///
    /// So the name is re-spelled here with **any** prefix the consuming
    /// context binds to the variable's namespace. A variable whose namespace
    /// that context cannot spell at all is left out: no expression written
    /// there could name it. A variable in no namespace goes by its local name.
    ///
    /// Shadowing is unaffected — it is decided on the expanded name, before
    /// any spelling is chosen.
    fn visible_vars(&self, ctx: StaticContextId) -> Vec<(String, VarLocation)> {
        let sc = self.static_contexts.get(ctx);
        let mut seen: Vec<&QName> = Vec::new();
        let mut out: Vec<(String, VarLocation)> = Vec::new();
        for entry in self.scope.iter().rev() {
            if seen.iter().any(|n| **n == entry.name) {
                continue;
            }
            seen.push(&entry.name);
            let lexical = if entry.name.uri.is_empty() {
                entry.name.local.clone()
            } else {
                match sc.prefix_for(&entry.name.uri) {
                    Some(prefix) if !prefix.is_empty() => {
                        format!("{prefix}:{}", entry.name.local)
                    }
                    _ => continue,
                }
            };
            out.push((lexical, entry.location));
        }
        out.reverse();
        out
    }

    fn compile_expr(&mut self, source: &str, ctx: StaticContextId) -> Result<ExprId, XsltError> {
        let visible = self.visible_vars(ctx);
        let names: Vec<&str> = visible.iter().map(|(n, _)| n.as_str()).collect();
        let vars: Vec<VarLocation> = visible.iter().map(|(_, l)| *l).collect();
        let compiled = XPathExpr::compile_with_vars(source, &self.contexts[ctx.0 as usize], &names);
        let (expr, unavailable) = match compiled {
            Ok(expr) => (expr, None),
            // §3.9 / §9.10 row 3: a call to a function the processor does not
            // have is an error only when the expression is evaluated, as long
            // as compatibility is in force here. The expression is replaced by
            // a stub that raises XTDE1425 if it is ever reached.
            Err(e) if e.error_code() == Some("XPST0017") && self.tolerates_unknown(ctx) => {
                let stub = XPathExpr::compile("()", &self.contexts[ctx.0 as usize])
                    .map_err(|e| XsltError::internal(e.to_string()))?;
                (stub, Some(format!("{e}")))
            }
            Err(e) => {
                return Err(XsltError::new(
                    e.error_code().unwrap_or(codes::XTSE0010),
                    format!("cannot compile the expression {source:?}: {e}"),
                ))
            }
        };
        self.exprs.push(CompiledExpr {
            expr,
            ctx,
            vars,
            unavailable,
        });
        Ok(ExprId(self.exprs.len() as u32 - 1))
    }

    /// Compiles the `as` attribute of a variable, parameter or template.
    fn compile_type(&mut self, text: &str, ctx: StaticContextId) -> Result<TypeCheck, XsltError> {
        let text = whitespace::trim(text).to_string();
        let test = self.compile_probe(&format!("$value instance of {text}"), ctx, &text)?;
        // The item type, without its occurrence indicator.
        let item = whitespace::trim(text.trim_end_matches(['?', '*', '+'])).to_string();
        let atomic = !item.contains('(') && !item.is_empty();
        let cast = if atomic && item != "xs:anyAtomicType" {
            Some(self.compile_probe(&format!("$value cast as {item}?"), ctx, &text)?)
        } else {
            None
        };
        Ok(TypeCheck {
            text,
            test,
            cast,
            atomic,
            item_type: item,
        })
    }

    /// Compiles a helper expression whose only variable is `$value`.
    fn compile_probe(
        &mut self,
        source: &str,
        ctx: StaticContextId,
        what: &str,
    ) -> Result<ExprId, XsltError> {
        let expr = XPathExpr::compile_with_vars(source, &self.contexts[ctx.0 as usize], &["value"])
            .map_err(|e| {
                XsltError::new(
                    e.error_code().unwrap_or(codes::XTSE0010),
                    format!("{what:?} is not a sequence type: {e}"),
                )
            })?;
        self.exprs.push(CompiledExpr {
            expr,
            ctx,
            vars: Vec::new(),
            unavailable: None,
        });
        Ok(ExprId(self.exprs.len() as u32 - 1))
    }

    /// Whether an unresolvable function call is tolerated here until it is
    /// evaluated (§3.9, §9.10 row 3).
    fn tolerates_unknown(&self, ctx: StaticContextId) -> bool {
        let compat = self.static_contexts.get(ctx).compat;
        compat.is_forwards() || compat.is_backwards()
    }

    // ── Patterns ─────────────────────────────────────────────────────────

    fn compile_pattern(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<PatternId, XsltError> {
        // Hand the whole pattern to the XPath parser first: a pattern is a
        // restriction of the path syntax, so anything the parser rejects is
        // XTSE0340 too.
        let visible = self.visible_vars(ctx);
        let names: Vec<&str> = visible.iter().map(|(n, _)| n.as_str()).collect();
        XPathExpr::compile_with_vars(source, &self.contexts[ctx.0 as usize], &names).map_err(
            |e| XsltError::new(codes::XTSE0340, format!("{source:?} is not a pattern: {e}")),
        )?;

        let sc = self.static_contexts.get(ctx).clone();
        let mut sink = Sink {
            builder: self,
            ctx,
            sc,
        };
        let pattern = pattern::parse(source, &mut sink)?;
        self.patterns.push(pattern);
        Ok(PatternId(self.patterns.len() as u32 - 1))
    }

    // ── Declarations (§7.3) ──────────────────────────────────────────────

    fn declare_globals(&mut self, decls: &[(u32, ModuleId, Nav<'t>)]) -> Result<(), XsltError> {
        // Highest precedence wins; two at the top precedence is XTSE0630.
        let mut best: HashMap<QName, (u32, usize)> = HashMap::new();
        let mut entries: Vec<(QName, u32, ModuleId, Nav<'t>)> = Vec::new();
        for (precedence, module_id, nav) in decls {
            let module = self.modules.module(*module_id);
            let ctx = self.context_of(nav, module)?;
            let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0010,
                    format!("xsl:{} has no name attribute", nav.local_name()),
                )
                .with_location(module.location(nav))
            })?;
            entries.push((name, *precedence, *module_id, nav.clone()));
        }
        for (index, (name, precedence, _, nav)) in entries.iter().enumerate() {
            match best.get(name) {
                Some((existing, _)) if *existing > *precedence => {}
                Some((existing, _)) if *existing == *precedence => {
                    return Err(XsltError::new(
                        codes::XTSE0630,
                        format!(
                            "the global variable {name} is declared twice with the same import \
                             precedence"
                        ),
                    )
                    .with_location(self.modules.module(self.modules.principal).location(nav)));
                }
                _ => {
                    best.insert(name.clone(), (*precedence, index));
                }
            }
        }

        // Bind the winning declarations, in declaration order, then compile
        // their values with every global already in scope.
        let mut winners: Vec<usize> = best.values().map(|(_, i)| *i).collect();
        winners.sort_unstable();
        for (slot, index) in winners.iter().enumerate() {
            let (name, _, _, _) = &entries[*index];
            self.global_index.insert(name.clone(), slot);
            self.scope.push(ScopeEntry {
                name: name.clone(),
                location: VarLocation::Global(slot),
            });
        }
        for index in winners {
            let (name, precedence, module_id, nav) = &entries[index];
            let module = self.modules.module(*module_id);
            let ctx = self.context_of(nav, module)?;
            let is_param = nav.local_name() == "param";
            self.frame_size = 0;
            let as_type = match attr_local(nav, "as") {
                Some(text) => Some(self.compile_type(&text, ctx)?),
                None => None,
            };
            let value = self.var_value(nav, *module_id, ctx, as_type.is_some())?;
            let required = attr_local(nav, "required")
                .map(|v| parse_yes_no(&v, "required"))
                .transpose()?
                .unwrap_or(false);
            self.globals.push(GlobalVar {
                name: name.clone(),
                is_param,
                required,
                value,
                as_type,
                precedence: *precedence,
                frame_size: self.frame_size,
                loc: module.location(nav),
            });
        }
        Ok(())
    }

    fn declaration(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        precedence: u32,
        subtree_min: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        let local = nav.local_name().to_string();
        if !self.static_contexts.get(ctx).compat.is_forwards() {
            check_attributes(nav, &local, &module.location(nav))?;
        }
        match local.as_str() {
            "template" => self.template(nav, module_id, ctx, precedence, subtree_min),
            "variable" | "param" => Ok(()), // handled in pass 1
            "output" => self.output_declaration(nav, ctx, precedence, module),
            "strip-space" | "preserve-space" => {
                self.space_declaration(nav, ctx, precedence, module)
            }
            "attribute-set" => self.attribute_set(nav, module_id, ctx, precedence),
            "namespace-alias" => self.namespace_alias(nav, ctx, module),
            name if unimplemented_declaration(name).is_some() => Err(XsltError::not_implemented(
                format!("xsl:{name}"),
                unimplemented_declaration(name).expect("checked"),
            )
            .with_location(module.location(nav))),
            other => {
                // §3.9: in forwards-compatible mode an unknown XSLT
                // declaration is ignored.
                if self.static_contexts.get(ctx).compat.is_forwards() {
                    Ok(())
                } else {
                    Err(XsltError::new(
                        codes::XTSE0010,
                        format!("xsl:{other} is not a stylesheet declaration"),
                    )
                    .with_location(module.location(nav)))
                }
            }
        }
    }

    fn output_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let name = self.qname_attr(nav, "name", ctx, module)?;
        let target = match &name {
            Some(name) => self.named_outputs.entry(name.clone()).or_default(),
            None => &mut self.output,
        };
        for (uri, local, _, value) in attributes(nav) {
            if !uri.is_empty() {
                continue;
            }
            match local.as_str() {
                "name" => {}
                "method" => target.method.set(parse_method(&value)?, precedence),
                "version" => target.version.set(value, precedence),
                "encoding" => target.encoding.set(value, precedence),
                "indent" => target
                    .indent
                    .set(parse_yes_no(&value, "indent")?, precedence),
                "omit-xml-declaration" => target
                    .omit_xml_declaration
                    .set(parse_yes_no(&value, "omit-xml-declaration")?, precedence),
                "standalone" => {
                    let value = match whitespace::trim(&value) {
                        "omit" => None,
                        other => Some(parse_yes_no(other, "standalone")?),
                    };
                    target.standalone.set(value, precedence);
                }
                "doctype-public" => target.doctype_public.set(value, precedence),
                "doctype-system" => target.doctype_system.set(value, precedence),
                "media-type" => target.media_type.set(value, precedence),
                // Serialization parameters M1 does not act on but must accept.
                "cdata-section-elements"
                | "escape-uri-attributes"
                | "include-content-type"
                | "normalization-form"
                | "undeclare-prefixes"
                | "use-character-maps"
                | "byte-order-mark" => {}
                other if STANDARD_ATTRS.contains(&other) => {}
                other => {
                    return Err(XsltError::new(
                        codes::XTSE0090,
                        format!("xsl:output has no attribute named {other:?}"),
                    )
                    .with_location(module.location(nav)))
                }
            }
        }
        Ok(())
    }

    fn space_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let strip = nav.local_name() == "strip-space";
        let elements = attr_local(nav, "elements").ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!("xsl:{} has no elements attribute", nav.local_name()),
            )
            .with_location(module.location(nav))
        })?;
        let sc = self.static_contexts.get(ctx).clone();
        for token in whitespace::tokens(&elements) {
            let test = if token == "*" {
                SpaceTest::Any
            } else if let Some(prefix) = token.strip_suffix(":*") {
                SpaceTest::Namespace(sc.resolve_prefix(prefix).ok_or_else(|| {
                    XsltError::new(
                        codes::XTSE0280,
                        format!("the prefix {prefix:?} in xsl:strip-space is not bound"),
                    )
                })?)
            } else if let Some(local) = token.strip_prefix("*:") {
                SpaceTest::LocalName(local.to_string())
            } else {
                let (prefix, local) = split_qname(token);
                let uri = if prefix.is_empty() {
                    // §4.4: an unprefixed name test uses the default namespace
                    // for elements, i.e. `xpath-default-namespace`.
                    sc.xpath_default_namespace.clone()
                } else {
                    sc.resolve_prefix(prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTSE0280,
                            format!("the prefix {prefix:?} in xsl:strip-space is not bound"),
                        )
                    })?
                };
                SpaceTest::Name(QName::new(uri, local))
            };
            let priority = test.priority();
            self.decl_order += 1;
            self.space_rules.push(SpaceRule {
                test,
                strip,
                precedence,
                priority,
                decl_order: self.decl_order,
            });
        }
        Ok(())
    }

    fn namespace_alias(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let sc = self.static_contexts.get(ctx).clone();
        let resolve = |token: Option<String>| -> Result<(String, String), XsltError> {
            let token = token.ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:namespace-alias needs both prefixes")
            })?;
            let prefix = if whitespace::trim(&token) == "#default" {
                ""
            } else {
                whitespace::trim(&token)
            };
            let uri = sc.resolve_prefix(prefix).ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0280,
                    format!("the prefix {prefix:?} in xsl:namespace-alias is not bound"),
                )
            })?;
            Ok((uri, prefix.to_string()))
        };
        let (from_uri, _) = resolve(attr_local(nav, "stylesheet-prefix"))?;
        let (to_uri, to_prefix) = resolve(attr_local(nav, "result-prefix"))?;
        let _ = module;
        self.aliases.insert(from_uri, (to_uri, to_prefix));
        Ok(())
    }

    fn attribute_set(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        precedence: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:attribute-set has no name attribute")
                    .with_location(module.location(nav))
            })?;
        let uses = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let mut attributes = Vec::new();
        let mut child = nav.clone();
        if child.move_to_first_child() {
            loop {
                if child.node_type() == DomNodeType::Element && !self.excluded(&child, module_id) {
                    if child.namespace_uri() == XSLT_NS && child.local_name() == "attribute" {
                        attributes.push(self.instruction(&child, module_id)?);
                    } else {
                        return Err(XsltError::new(
                            codes::XTSE0010,
                            "the content of xsl:attribute-set must be xsl:attribute elements only",
                        )
                        .with_location(self.modules.module(module_id).location(&child)));
                    }
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        self.scope.truncate(scope_mark);
        let _ = precedence;
        let entry = self
            .attribute_sets
            .entry(name.clone())
            .or_insert_with(|| AttributeSet {
                name,
                use_attribute_sets: Vec::new(),
                attributes: Vec::new(),
            });
        entry.use_attribute_sets.extend(uses);
        entry.attributes.extend(attributes);
        Ok(())
    }

    fn template(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        precedence: u32,
        subtree_min: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        let name = self.declared_name(nav, "name", ctx, module)?;
        let match_text = attr_local(nav, "match");
        if name.is_none() && match_text.is_none() {
            return Err(XsltError::new(
                codes::XTSE0500,
                "xsl:template must have a match attribute, a name attribute, or both",
            )
            .with_location(loc));
        }

        self.frame_size = 0;
        let scope_mark = self.scope.len();

        // The pattern is compiled with globals only in scope (§9.7).
        let pattern = match &match_text {
            Some(text) => Some(self.compile_pattern(text, ctx)?),
            None => None,
        };

        let (modes, all_modes) = self.modes_of(nav, ctx, module, pattern.is_some())?;
        // §6.4: the `priority` attribute belongs to the declaration and so
        // applies to every alternative of the pattern. Without it, each
        // alternative keeps the default priority `pattern::PathPattern`
        // computed from its own syntax, and conflict resolution treats the
        // alternatives as separate rules.
        let explicit_priority = match attr_local(nav, "priority") {
            Some(text) => Some(whitespace::trim(&text).parse::<f64>().map_err(|_| {
                XsltError::new(
                    codes::XTSE0530,
                    format!("the priority {text:?} is not a number"),
                )
                .with_location(loc.clone())
            })?),
            None => None,
        };
        let as_type = match attr_local(nav, "as") {
            Some(text) => Some(self.compile_type(&text, ctx)?),
            None => None,
        };

        let (params, body) = self.template_body(nav, module_id)?;
        self.scope.truncate(scope_mark);

        self.decl_order += 1;
        let id = TemplateId(self.templates.len() as u32);
        if let Some(name) = &name {
            match self.named.get(name) {
                Some((_, existing)) if *existing > precedence => {}
                Some((_, existing)) if *existing == precedence => {
                    return Err(XsltError::new(
                        codes::XTSE0660,
                        format!("two templates are named {name} with the same import precedence"),
                    )
                    .with_location(loc));
                }
                _ => {
                    self.named.insert(name.clone(), (id, precedence));
                }
            }
        }
        self.templates.push(TemplateRule {
            id,
            name,
            pattern,
            modes,
            all_modes,
            explicit_priority,
            precedence,
            subtree_min,
            decl_order: self.decl_order,
            params,
            body,
            as_type,
            frame_size: self.frame_size,
            module: module_id,
            loc,
        });
        Ok(())
    }

    /// The `mode` attribute of `xsl:template` (§6.5).
    fn modes_of(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        module: &Module<'t>,
        has_match: bool,
    ) -> Result<(Vec<ModeKey>, bool), XsltError> {
        let Some(text) = attr_local(nav, "mode") else {
            return Ok((
                if has_match {
                    vec![ModeKey::Default]
                } else {
                    Vec::new()
                },
                false,
            ));
        };
        if !has_match {
            return Err(XsltError::new(
                codes::XTSE0500,
                "xsl:template has a mode attribute but no match attribute",
            )
            .with_location(module.location(nav)));
        }
        let mut modes = Vec::new();
        let mut all = false;
        for token in whitespace::tokens(&text) {
            match token {
                "#all" => all = true,
                "#default" => modes.push(ModeKey::Default),
                name => modes.push(ModeKey::Named(self.resolve(name, ctx, codes::XTSE0550)?)),
            }
        }
        if all && modes.len() > 1 {
            return Err(XsltError::new(
                codes::XTSE0550,
                "mode=\"#all\" cannot be combined with other mode names",
            )
            .with_location(module.location(nav)));
        }
        if modes.is_empty() && !all {
            modes.push(ModeKey::Default);
        }
        Ok((modes, all))
    }

    /// The `xsl:param` children of a template, then its sequence constructor.
    fn template_body(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<(Vec<ParamDecl>, Body), XsltError> {
        let mut params = Vec::new();
        let mut child = nav.clone();
        let mut first_non_param: Option<Nav<'t>> = None;
        if child.move_to_first_child() {
            loop {
                match child.node_type() {
                    DomNodeType::Element if self.excluded(&child, module_id) => {}
                    DomNodeType::Element => {
                        let is_param =
                            child.namespace_uri() == XSLT_NS && child.local_name() == "param";
                        if is_param && first_non_param.is_none() {
                            params.push(self.param_decl(&child, module_id, false)?);
                        } else {
                            if first_non_param.is_none() {
                                first_non_param = Some(child.clone());
                            }
                            if is_param {
                                let module = self.modules.module(module_id);
                                return Err(XsltError::new(
                                    codes::XTSE0010,
                                    "xsl:param must come before any other content of a template",
                                )
                                .with_location(module.location(&child)));
                            }
                        }
                    }
                    // Whitespace before the first instruction is stripped;
                    // significant text opens the sequence constructor.
                    kind if kind.is_text_like()
                        && first_non_param.is_none()
                        && !is_stripped_stylesheet_text(&child) =>
                    {
                        first_non_param = Some(child.clone());
                    }
                    _ => {}
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        let body = self.sequence_constructor_skipping(nav, module_id, "param")?;
        Ok((params, body))
    }

    fn param_decl(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        global: bool,
    ) -> Result<ParamDecl, XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
            XsltError::new(codes::XTSE0010, "xsl:param has no name attribute")
                .with_location(module.location(nav))
        })?;
        let as_type = match attr_local(nav, "as") {
            Some(text) => Some(self.compile_type(&text, ctx)?),
            None => None,
        };
        let required = attr_local(nav, "required")
            .map(|v| parse_yes_no(&v, "required"))
            .transpose()?
            .unwrap_or(false);
        let tunnel = attr_local(nav, "tunnel")
            .map(|v| parse_yes_no(&v, "tunnel"))
            .transpose()?
            .unwrap_or(false);
        let value = self.var_value(nav, module_id, ctx, as_type.is_some())?;
        // §10.1.1: "It is a static error if a parameter that is declared as
        // required also specifies a default value."
        if required && !matches!(value, VarValue::Empty) {
            return Err(XsltError::new(
                codes::XTSE0010,
                format!("the required parameter {name} must not have a default value"),
            )
            .with_location(module.location(nav)));
        }
        let slot = self.frame_size;
        self.frame_size += 1;
        if !global {
            self.scope.push(ScopeEntry {
                name: name.clone(),
                location: VarLocation::Local(slot),
            });
        }
        Ok(ParamDecl {
            name,
            slot,
            value,
            as_type,
            required,
            tunnel,
            loc: module.location(nav),
        })
    }

    /// `select` xor content, the shape every variable-binding element shares.
    ///
    /// > \[ERR XTSE0620\] It is a static error if a variable-binding element
    /// > has a select attribute and has non-empty content.
    fn var_value(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        has_as: bool,
    ) -> Result<VarValue, XsltError> {
        let select = attr_local(nav, "select");
        let has_content = self.has_content(nav, module_id);
        if select.is_some() && has_content {
            let module = self.modules.module(module_id);
            return Err(XsltError::new(
                codes::XTSE0620,
                format!(
                    "xsl:{} has both a select attribute and content",
                    nav.local_name()
                ),
            )
            .with_location(module.location(nav)));
        }
        Ok(match select {
            Some(source) => VarValue::Select(self.compile_expr(&source, ctx)?),
            None if has_content => {
                let body = self.sequence_constructor(nav, module_id)?;
                if has_as {
                    VarValue::Sequence(body)
                } else {
                    VarValue::Tree { body, ctx }
                }
            }
            None => VarValue::Empty,
        })
    }

    // ── Sequence constructors (§5.7) ─────────────────────────────────────

    fn sequence_constructor(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Body, XsltError> {
        self.sequence_constructor_skipping(parent, module_id, "")
    }

    /// Compiles the children of `parent`, skipping XSLT elements named `skip`
    /// (used for the `xsl:param` children of a template).
    fn sequence_constructor_skipping(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
        skip: &str,
    ) -> Result<Body, XsltError> {
        let scope_mark = self.scope.len();
        let mut out = Vec::new();
        let mut child = parent.clone();
        if child.move_to_first_child() {
            loop {
                match child.node_type() {
                    DomNodeType::Element => {
                        let skipped = !skip.is_empty()
                            && child.namespace_uri() == XSLT_NS
                            && child.local_name() == skip;
                        if !skipped && !self.excluded(&child, module_id) {
                            out.push(self.instruction(&child, module_id)?);
                        }
                    }
                    // §4.2: whitespace-only text nodes are stripped.
                    kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => {
                        let module = self.modules.module(module_id);
                        let ctx = self.context_of(parent, module)?;
                        out.push(self.wrap(
                            InstructionKind::Text {
                                text: child.value(),
                                disable_output_escaping: false,
                            },
                            ctx,
                            module_id,
                            &child,
                        ));
                    }
                    // Comments and processing instructions in a stylesheet are
                    // not part of the result (§4.1).
                    _ => {}
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        self.scope.truncate(scope_mark);
        Ok(Body(out))
    }

    fn wrap(
        &self,
        kind: InstructionKind,
        ctx: StaticContextId,
        module_id: ModuleId,
        nav: &Nav<'t>,
    ) -> Instruction {
        let loc = self.modules.module(module_id).location(nav);
        Instruction {
            kind,
            ctx,
            module: module_id,
            line: loc.line,
            column: loc.column,
        }
    }

    fn instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Instruction, XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        if nav.namespace_uri() != XSLT_NS {
            let sc = self.static_contexts.get(ctx);
            if sc.is_extension_namespace(nav.namespace_uri()) {
                // §18.2.2: an extension instruction this processor does not
                // know falls back to its xsl:fallback children.
                return self.extension_instruction(nav, module_id, ctx);
            }
            let kind = self.literal_element_kind(nav, module_id, ctx)?;
            return Ok(self.wrap(kind, ctx, module_id, nav));
        }
        let kind = self.xslt_instruction(nav, module_id, ctx)?;
        Ok(self.wrap(kind, ctx, module_id, nav))
    }

    fn extension_instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<Instruction, XsltError> {
        match self.fallback_bodies(nav, module_id)? {
            Some(body) => Ok(self.wrap(
                InstructionKind::Choose {
                    whens: Vec::new(),
                    otherwise: Some(body),
                },
                ctx,
                module_id,
                nav,
            )),
            None => Ok(self.wrap(
                InstructionKind::NotImplemented {
                    feature: format!("the extension instruction {}", nav.name()),
                    milestone: "M3",
                },
                ctx,
                module_id,
                nav,
            )),
        }
    }

    /// The contents of **every** `xsl:fallback` child, concatenated.
    ///
    /// > \[§18.2.2\] If the instruction is not available, then the
    /// > `xsl:fallback` children of the instruction are evaluated (there may
    /// > be more than one).
    fn fallback_bodies(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Option<Body>, XsltError> {
        let mut found = false;
        let mut out = Vec::new();
        let mut child = nav.clone();
        if child.move_to_first_child() {
            loop {
                if child.node_type() == DomNodeType::Element
                    && child.namespace_uri() == XSLT_NS
                    && child.local_name() == "fallback"
                    && !self.excluded(&child, module_id)
                {
                    found = true;
                    out.extend(self.sequence_constructor(&child, module_id)?.0);
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        Ok(found.then_some(Body(out)))
    }

    fn xslt_instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<InstructionKind, XsltError> {
        let local = nav.local_name().to_string();
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        if !self.static_contexts.get(ctx).compat.is_forwards() {
            check_attributes(nav, &local, &loc)?;
        }
        match local.as_str() {
            "apply-templates" => {
                self.reject_sort(nav, module_id)?;
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                let mode = self.mode_ref(nav, ctx)?;
                let params = self.with_params(nav, module_id)?;
                Ok(InstructionKind::ApplyTemplates {
                    select,
                    mode,
                    params,
                })
            }
            "apply-imports" => Ok(InstructionKind::ApplyImports {
                params: self.with_params(nav, module_id)?,
            }),
            "next-match" => Ok(InstructionKind::NextMatch {
                params: self.with_params(nav, module_id)?,
            }),
            "call-template" => {
                let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                    XsltError::new(codes::XTSE0010, "xsl:call-template has no name attribute")
                        .with_location(loc.clone())
                })?;
                let params = self.with_params(nav, module_id)?;
                Ok(InstructionKind::CallTemplate { name, params })
            }
            "for-each" => {
                self.reject_sort(nav, module_id)?;
                let select = self.required_expr(nav, "select", ctx, &loc)?;
                let body = self.sequence_constructor_skipping(nav, module_id, "sort")?;
                Ok(InstructionKind::ForEach { select, body })
            }
            "if" => {
                let test = self.required_expr(nav, "test", ctx, &loc)?;
                let then = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::If { test, then })
            }
            "choose" => self.choose(nav, module_id, &loc),
            "value-of" => {
                let separator = match attr_local(nav, "separator") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let has_select = attr_local(nav, "select").is_some();
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0870)?;
                let doe = attr_local(nav, "disable-output-escaping")
                    .map(|v| parse_yes_no(&v, "disable-output-escaping"))
                    .transpose()?
                    .unwrap_or(false);
                // §9.10 row 5: with backwards-compatible behaviour and no
                // separator, all items after the first are ignored.
                let first_item_only = separator.is_none()
                    && has_select
                    && self.static_contexts.get(ctx).compat.is_backwards();
                Ok(InstructionKind::ValueOf {
                    value,
                    separator,
                    disable_output_escaping: doe,
                    first_item_only,
                })
            }
            "text" => {
                let doe = attr_local(nav, "disable-output-escaping")
                    .map(|v| parse_yes_no(&v, "disable-output-escaping"))
                    .transpose()?
                    .unwrap_or(false);
                Ok(InstructionKind::Text {
                    text: text_content(nav),
                    disable_output_escaping: doe,
                })
            }
            "sequence" => {
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                Ok(InstructionKind::Sequence { select })
            }
            "element" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let namespace = match attr_local(nav, "namespace") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let attr_sets = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
                let inherit = attr_local(nav, "inherit-namespaces")
                    .map(|v| parse_yes_no(&v, "inherit-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Element {
                    name,
                    namespace,
                    attr_sets,
                    inherit_namespaces: inherit,
                    body,
                })
            }
            "attribute" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let namespace = match attr_local(nav, "namespace") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let separator = match attr_local(nav, "separator") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0840)?;
                Ok(InstructionKind::Attribute {
                    name,
                    namespace,
                    separator,
                    value,
                })
            }
            "namespace" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0910)?;
                Ok(InstructionKind::Namespace { name, value })
            }
            "comment" => {
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0940)?;
                Ok(InstructionKind::Comment { value })
            }
            "processing-instruction" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0880)?;
                Ok(InstructionKind::ProcessingInstruction { name, value })
            }
            "document" => Ok(InstructionKind::Document {
                body: self.sequence_constructor(nav, module_id)?,
            }),
            "copy" => {
                let copy_ns = attr_local(nav, "copy-namespaces")
                    .map(|v| parse_yes_no(&v, "copy-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let inherit = attr_local(nav, "inherit-namespaces")
                    .map(|v| parse_yes_no(&v, "inherit-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let attr_sets = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Copy {
                    copy_namespaces: copy_ns,
                    inherit_namespaces: inherit,
                    attr_sets,
                    body,
                })
            }
            "copy-of" => {
                let select = self.required_expr(nav, "select", ctx, &loc)?;
                let copy_ns = attr_local(nav, "copy-namespaces")
                    .map(|v| parse_yes_no(&v, "copy-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                Ok(InstructionKind::CopyOf {
                    select,
                    copy_namespaces: copy_ns,
                })
            }
            "variable" => {
                let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                    XsltError::new(codes::XTSE0010, "xsl:variable has no name attribute")
                        .with_location(loc.clone())
                })?;
                let as_type = match attr_local(nav, "as") {
                    Some(text) => Some(self.compile_type(&text, ctx)?),
                    None => None,
                };
                let value = self.var_value(nav, module_id, ctx, as_type.is_some())?;
                let slot = self.frame_size;
                self.frame_size += 1;
                self.scope.push(ScopeEntry {
                    name: name.clone(),
                    location: VarLocation::Local(slot),
                });
                Ok(InstructionKind::Variable {
                    slot,
                    value,
                    as_type,
                    name,
                })
            }
            "param" => Err(XsltError::new(
                codes::XTSE0010,
                "xsl:param is only allowed at the start of a template, function or stylesheet",
            )
            .with_location(loc)),
            "message" => {
                let terminate = match attr_local(nav, "terminate") {
                    Some(text) => self.avt(&text, ctx)?,
                    None => Avt::literal("no"),
                };
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Message {
                    select,
                    body,
                    terminate,
                })
            }
            "fallback" => Ok(InstructionKind::Fallback),
            "number" | "for-each-group" | "analyze-string" | "perform-sort" | "result-document"
            | "sort" | "function" | "key" => Ok(InstructionKind::NotImplemented {
                feature: format!("xsl:{local}"),
                milestone: match local.as_str() {
                    "number" | "result-document" => "M3",
                    _ => "M2",
                },
            }),
            other => {
                // §3.9: in a forwards-compatible region an unknown XSLT
                // instruction becomes its xsl:fallback children, or an error
                // only if it is evaluated.
                if self.static_contexts.get(ctx).compat.is_forwards() {
                    return Ok(self.fallback_or_error(nav, module_id, other));
                }
                Err(XsltError::new(
                    codes::XTSE0010,
                    format!("xsl:{other} is not an XSLT instruction"),
                )
                .with_location(loc))
            }
        }
    }

    fn fallback_or_error(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        name: &str,
    ) -> InstructionKind {
        match self.fallback_bodies(nav, module_id) {
            Ok(Some(body)) => InstructionKind::Choose {
                whens: Vec::new(),
                otherwise: Some(body),
            },
            // §3.9: without a fallback, the error is signalled only if the
            // instruction is evaluated.
            _ => InstructionKind::NotImplemented {
                feature: format!("the unknown XSLT instruction xsl:{name}"),
                milestone: "M3",
            },
        }
    }

    fn choose(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let mut whens = Vec::new();
        let mut otherwise = None;
        let mut child = nav.clone();
        if child.move_to_first_child() {
            loop {
                if child.node_type() == DomNodeType::Element && !self.excluded(&child, module_id) {
                    let module = self.modules.module(module_id);
                    let child_loc = module.location(&child);
                    if child.namespace_uri() != XSLT_NS {
                        return Err(XsltError::new(
                            codes::XTSE0010,
                            "only xsl:when and xsl:otherwise may appear inside xsl:choose",
                        )
                        .with_location(child_loc));
                    }
                    match child.local_name() {
                        "when" => {
                            if otherwise.is_some() {
                                return Err(XsltError::new(
                                    codes::XTSE0010,
                                    "xsl:when must not follow xsl:otherwise",
                                )
                                .with_location(child_loc));
                            }
                            let child_ctx =
                                self.context_of(&child, self.modules.module(module_id))?;
                            let test = self.required_expr(&child, "test", child_ctx, &child_loc)?;
                            let body = self.sequence_constructor(&child, module_id)?;
                            whens.push((test, body));
                        }
                        "otherwise" => {
                            if otherwise.is_some() {
                                return Err(XsltError::new(
                                    codes::XTSE0010,
                                    "xsl:choose may have only one xsl:otherwise child",
                                )
                                .with_location(child_loc));
                            }
                            otherwise = Some(self.sequence_constructor(&child, module_id)?);
                        }
                        other => {
                            return Err(XsltError::new(
                                codes::XTSE0010,
                                format!("xsl:{other} may not appear inside xsl:choose"),
                            )
                            .with_location(child_loc))
                        }
                    }
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        if whens.is_empty() {
            return Err(XsltError::new(
                codes::XTSE0010,
                "xsl:choose must have at least one xsl:when child",
            )
            .with_location(loc.clone()));
        }
        Ok(InstructionKind::Choose { whens, otherwise })
    }

    /// The literal result element of §11.1.
    fn literal_element(
        &mut self,
        nav: &Nav<'t>,
        module: &Module<'t>,
        ctx: StaticContextId,
    ) -> Result<Instruction, XsltError> {
        let module_id = module.id;
        let kind = self.literal_element_kind(nav, module_id, ctx)?;
        Ok(self.wrap(kind, ctx, module_id, nav))
    }

    fn literal_element_kind(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<InstructionKind, XsltError> {
        let module = self.modules.module(module_id);
        let sc = self.static_contexts.get(ctx).clone();

        let (uri, prefix) = self.alias_of(nav.namespace_uri(), nav.prefix());
        let name = Name::qualified(&prefix, nav.local_name(), &uri);

        // §11.1.2: every in-scope namespace of the literal result element
        // becomes a namespace node of the result element, except the XSLT
        // namespace, the excluded namespaces and the extension namespaces.
        let mut namespaces = Vec::new();
        for (prefix, uri) in in_scope_namespaces(nav) {
            if uri == XSLT_NS || sc.is_excluded(&uri) || sc.is_extension_namespace(&uri) {
                continue;
            }
            let (aliased_uri, aliased_prefix) = self.alias_of(&uri, &prefix);
            namespaces.push((aliased_prefix, aliased_uri));
        }

        let mut attrs = Vec::new();
        let mut attr_sets = Vec::new();
        for (attr_uri, local, attr_prefix, value) in attributes(nav) {
            if attr_uri == XSLT_NS {
                match local.as_str() {
                    "use-attribute-sets" => {
                        attr_sets = self.qname_list_value(&value, ctx)?;
                    }
                    other if STANDARD_ATTRS.contains(&other) => {}
                    kind @ ("type" | "validation") => {
                        return Err(XsltError::not_implemented(
                            format!("xsl:{kind} on a literal result element"),
                            "M4",
                        )
                        .with_location(module.location(nav)))
                    }
                    other => {
                        return Err(XsltError::new(
                            codes::XTSE0805,
                            format!("xsl:{other} is not allowed on a literal result element"),
                        )
                        .with_location(module.location(nav)))
                    }
                }
                continue;
            }
            let (aliased_uri, aliased_prefix) = if attr_uri.is_empty() {
                (String::new(), String::new())
            } else {
                self.alias_of(&attr_uri, &attr_prefix)
            };
            attrs.push((
                Name::qualified(&aliased_prefix, &local, &aliased_uri),
                self.avt(&value, ctx)?,
            ));
        }

        let body = self.sequence_constructor(nav, module_id)?;
        Ok(InstructionKind::LiteralElement {
            name,
            namespaces,
            attrs,
            attr_sets,
            body,
        })
    }

    /// §11.1.4: `xsl:namespace-alias` rewrites a literal result element's
    /// namespace on the way out.
    fn alias_of(&self, uri: &str, prefix: &str) -> (String, String) {
        match self.aliases.get(uri) {
            Some((to_uri, to_prefix)) => (to_uri.clone(), to_prefix.clone()),
            None => (uri.to_string(), prefix.to_string()),
        }
    }

    // ── Small helpers ────────────────────────────────────────────────────

    fn reject_sort(&self, nav: &Nav<'t>, module_id: ModuleId) -> Result<(), XsltError> {
        let mut child = nav.clone();
        if child.move_to_first_child() {
            loop {
                if child.node_type() == DomNodeType::Element
                    && child.namespace_uri() == XSLT_NS
                    && child.local_name() == "sort"
                    && !self.excluded(&child, module_id)
                {
                    return Err(XsltError::not_implemented("xsl:sort", "M2")
                        .with_location(self.modules.module(module_id).location(&child)));
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        Ok(())
    }

    fn with_params(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Vec<WithParam>, XsltError> {
        let mut out = Vec::new();
        let mut child = nav.clone();
        if child.move_to_first_child() {
            loop {
                if child.node_type() == DomNodeType::Element
                    && child.namespace_uri() == XSLT_NS
                    && child.local_name() == "with-param"
                    && !self.excluded(&child, module_id)
                {
                    let module = self.modules.module(module_id);
                    let ctx = self.context_of(&child, module)?;
                    let name = self
                        .qname_attr(&child, "name", ctx, module)?
                        .ok_or_else(|| {
                            XsltError::new(codes::XTSE0010, "xsl:with-param has no name attribute")
                                .with_location(module.location(&child))
                        })?;
                    let as_type = match attr_local(&child, "as") {
                        Some(text) => Some(self.compile_type(&text, ctx)?),
                        None => None,
                    };
                    let tunnel = attr_local(&child, "tunnel")
                        .map(|v| parse_yes_no(&v, "tunnel"))
                        .transpose()?
                        .unwrap_or(false);
                    let value = self.var_value(&child, module_id, ctx, as_type.is_some())?;
                    out.push(WithParam {
                        name,
                        value,
                        as_type,
                        tunnel,
                        loc: self.modules.module(module_id).location(&child),
                    });
                }
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn mode_ref(&mut self, nav: &Nav<'t>, ctx: StaticContextId) -> Result<ModeRef, XsltError> {
        let _ = &self.names;
        Ok(match attr_local(nav, "mode") {
            None => ModeRef::Default,
            Some(text) => match whitespace::trim(&text) {
                "#default" => ModeRef::Default,
                "#current" => ModeRef::Current,
                name => ModeRef::Named(self.resolve(name, ctx, codes::XTSE0550)?),
            },
        })
    }

    fn value_source(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        exclusivity_code: &'static str,
    ) -> Result<ValueSource, XsltError> {
        let select = attr_local(nav, "select");
        let has_content = self.has_content(nav, module_id);
        let module = self.modules.module(module_id);
        match (select, has_content) {
            (Some(_), true) => Err(XsltError::new(
                exclusivity_code,
                format!(
                    "xsl:{} has both a select attribute and content",
                    nav.local_name()
                ),
            )
            .with_location(module.location(nav))),
            (Some(source), false) => Ok(ValueSource::Select(self.compile_expr(&source, ctx)?)),
            (None, _) => Ok(ValueSource::Content(
                self.sequence_constructor(nav, module_id)?,
            )),
        }
    }

    fn required_expr(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<ExprId, XsltError> {
        let source = attr_local(nav, attr).ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!("xsl:{} has no {attr} attribute", nav.local_name()),
            )
            .with_location(loc.clone())
        })?;
        self.compile_expr(&source, ctx)
    }

    fn required_avt(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<Avt, XsltError> {
        let text = attr_local(nav, attr).ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!("xsl:{} has no {attr} attribute", nav.local_name()),
            )
            .with_location(loc.clone())
        })?;
        self.avt(&text, ctx)
    }

    /// §5.6: an attribute value template.
    fn avt(&mut self, value: &str, ctx: StaticContextId) -> Result<Avt, XsltError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let chars: Vec<char> = value.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            match chars[i] {
                '{' if chars.get(i + 1) == Some(&'{') => {
                    literal.push('{');
                    i += 2;
                }
                '}' if chars.get(i + 1) == Some(&'}') => {
                    literal.push('}');
                    i += 2;
                }
                '}' => {
                    // > [ERR XTSE0370] It is a static error if an unescaped
                    // > right curly bracket occurs in a fixed part of an
                    // > attribute value template.
                    return Err(XsltError::new(
                        codes::XTSE0370,
                        format!("an unescaped `}}` in the attribute value template {value:?}"),
                    ));
                }
                '{' => {
                    if !literal.is_empty() {
                        parts.push(AvtPart::Literal(std::mem::take(&mut literal)));
                    }
                    let (source, next) = avt_expression(&chars, i + 1, value)?;
                    parts.push(AvtPart::Expr(self.compile_expr(&source, ctx)?));
                    i = next;
                }
                c => {
                    literal.push(c);
                    i += 1;
                }
            }
        }
        if !literal.is_empty() {
            parts.push(AvtPart::Literal(literal));
        }
        Ok(Avt { parts })
    }

    fn qname_attr(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<Option<QName>, XsltError> {
        let Some(text) = attr_local(nav, attr) else {
            return Ok(None);
        };
        let name = self
            .resolve(&text, ctx, codes::XTSE0020)
            .map_err(|e| e.or_location(&module.location(nav)))?;
        Ok(Some(name))
    }

    fn qname_list(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<Vec<QName>, XsltError> {
        let Some(text) = attr_local(nav, attr) else {
            return Ok(Vec::new());
        };
        self.qname_list_value(&text, ctx)
            .map_err(|e| e.or_location(&module.location(nav)))
    }

    fn qname_list_value(
        &mut self,
        text: &str,
        ctx: StaticContextId,
    ) -> Result<Vec<QName>, XsltError> {
        whitespace::tokens(text)
            .map(|token| self.resolve(token, ctx, codes::XTSE0020))
            .collect()
    }

    fn resolve(&self, name: &str, ctx: StaticContextId, code: &str) -> Result<QName, XsltError> {
        let sc = self.static_contexts.get(ctx);
        resolve_qname(name, |prefix| sc.resolve_prefix(prefix), code)
    }

    /// The name a declaration gives itself, rejecting the reserved namespaces.
    ///
    /// > \[ERR XTSE0080\] It is a static error if a name occurring in the
    /// > stylesheet that is required to be a QName uses a reserved namespace.
    fn declared_name(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<Option<QName>, XsltError> {
        let Some(name) = self.qname_attr(nav, attr, ctx, module)? else {
            return Ok(None);
        };
        // `xsl:initial-template` is the conventional default entry point and
        // is the one name in the XSLT namespace a stylesheet may declare.
        let is_entry_point = name.uri == XSLT_NS && name.local == "initial-template";
        if !is_entry_point && RESERVED_NAMESPACES.contains(&name.uri.as_str()) {
            return Err(XsltError::new(
                codes::XTSE0080,
                format!("{name} is in a reserved namespace"),
            )
            .with_location(module.location(nav)));
        }
        Ok(Some(name))
    }

    // ── The dispatch index (§8.5) ────────────────────────────────────────

    fn build_mode_table(&mut self) -> ModeTable {
        let mut table = ModeTable::default();
        // Every mode a declaration's `mode` attribute names must exist, plus
        // the unnamed one. `#all` contributes no name — §6.5 defines it by
        // reference to the modes named elsewhere — and its rules go into a
        // single index every mode consults instead (see `ModeTable::all_modes`),
        // so a mode named only by an `xsl:apply-templates` still sees them.
        let mut mode_names: Vec<ModeKey> = vec![ModeKey::Default];
        for rule in &self.templates {
            for mode in &rule.modes {
                if !mode_names.contains(mode) {
                    mode_names.push(mode.clone());
                }
            }
        }
        for mode in &mode_names {
            table.modes.insert(mode.clone(), ModeIndex::default());
        }
        for rule in &self.templates {
            let Some(pattern_id) = rule.pattern else {
                continue;
            };
            let pattern = &self.patterns[pattern_id.0 as usize];
            for (index, alternative) in pattern.alternatives.iter().enumerate() {
                // §6.4: each alternative is a template rule of its own, so it
                // gets its own index key.
                let id = RuleId {
                    template: rule.id,
                    alternative: index as u32,
                };
                let key = alternative.index_key();
                if rule.all_modes {
                    push_into(&mut table.all_modes, key, id);
                    continue;
                }
                for mode in &rule.modes {
                    let bucket = table.modes.entry(mode.clone()).or_default();
                    push_into(bucket, alternative.index_key(), id);
                }
            }
        }
        // §6.4: candidates are tried in (precedence, priority, declaration
        // order) order, all descending, so the first match wins.
        let key = |id: &RuleId| {
            let rule = &self.templates[id.template.0 as usize];
            let priority = match rule.explicit_priority {
                Some(p) => p,
                None => match rule.pattern {
                    Some(pattern) => {
                        self.patterns[pattern.0 as usize].alternatives[id.alternative as usize]
                            .priority
                    }
                    None => 0.0,
                },
            };
            (rule.precedence, priority, rule.decl_order)
        };
        let sort = |list: &mut Vec<RuleId>| {
            list.sort_by(|a, b| {
                let (pa, ra, da) = key(a);
                let (pb, rb, db) = key(b);
                pb.cmp(&pa)
                    .then(rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal))
                    .then(db.cmp(&da))
            });
        };
        let sort_index = |index: &mut ModeIndex| {
            for list in index.by_element.values_mut() {
                sort(list);
            }
            for list in index.by_attribute.values_mut() {
                sort(list);
            }
            for list in index.by_namespace.values_mut() {
                sort(list);
            }
            for list in index.by_local.values_mut() {
                sort(list);
            }
            sort(&mut index.other);
        };
        for index in table.modes.values_mut() {
            sort_index(index);
        }
        sort_index(&mut table.all_modes);
        table
    }
}

/// Files one rule into the bucket its index key names (§8.5).
fn push_into(index: &mut ModeIndex, key: IndexKey, id: RuleId) {
    let bucket = match key {
        IndexKey::Element(name) => index.by_element.entry(name).or_default(),
        IndexKey::Attribute(name) => index.by_attribute.entry(name).or_default(),
        IndexKey::Namespace(uri) => index.by_namespace.entry(uri).or_default(),
        IndexKey::Local(local) => index.by_local.entry(local).or_default(),
        IndexKey::Other => &mut index.other,
    };
    if !bucket.contains(&id) {
        bucket.push(id);
    }
}

/// The [`PatternSink`] a pattern is read through: it resolves prefixes in the
/// pattern's own static context and compiles each predicate as an expression.
struct Sink<'b, 'a, 'm, 't> {
    builder: &'b mut Builder<'a, 'm, 't>,
    ctx: StaticContextId,
    sc: crate::stylesheet::StaticContext,
}

impl PatternSink for Sink<'_, '_, '_, '_> {
    fn resolve_prefix(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            return Some(self.sc.xpath_default_namespace.clone());
        }
        self.sc.resolve_prefix(prefix)
    }

    fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError> {
        self.builder.compile_expr(source, self.ctx)
    }
}

// ── Free helpers ─────────────────────────────────────────────────────────

/// The concatenated text of an element's children (`xsl:text`).
fn text_content(nav: &Nav<'_>) -> String {
    let mut out = String::new();
    let mut child = nav.clone();
    if child.move_to_first_child() {
        loop {
            if child.node_type().is_text_like() {
                out.push_str(&child.value());
            }
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    out
}

/// The namespaces §3.2 reserves; a stylesheet may not declare a name in one.
const RESERVED_NAMESPACES: &[&str] = &[
    XSLT_NS,
    "http://www.w3.org/2005/xpath-functions",
    "http://www.w3.org/2001/XMLSchema",
    "http://www.w3.org/2001/XMLSchema-instance",
];

/// The stylesheet declarations M1 parses but does not implement, and the
/// milestone that owns each.
fn unimplemented_declaration(local: &str) -> Option<&'static str> {
    Some(match local {
        "key" | "function" => "M2",
        "decimal-format" | "character-map" => "M3",
        "import-schema" => "M4",
        _ => return None,
    })
}

/// The attributes in no namespace each XSLT element allows, beyond the
/// standard attributes of §3.5.
///
/// > \[ERR XTSE0090\] It is a static error if an element from the XSLT
/// > namespace has an attribute whose name is in no namespace and that is not
/// > one of the attributes defined for that element in this specification.
fn allowed_attributes(local: &str) -> Option<&'static [&'static str]> {
    Some(match local {
        "stylesheet" | "transform" => &["id", "default-validation", "input-type-annotations"],
        "include" | "import" => &["href"],
        "import-schema" => &["namespace", "schema-location"],
        "template" => &["match", "name", "priority", "mode", "as"],
        "param" => &["name", "select", "as", "required", "tunnel", "static"],
        "variable" => &["name", "select", "as", "static"],
        "with-param" => &["name", "select", "as", "tunnel"],
        "apply-templates" => &["select", "mode"],
        "apply-imports" | "next-match" | "choose" | "otherwise" | "fallback" => &[],
        "call-template" => &["name"],
        "for-each" => &["select"],
        "if" | "when" => &["test"],
        "value-of" => &["select", "separator", "disable-output-escaping"],
        "text" => &["disable-output-escaping"],
        "sequence" => &["select"],
        "element" => &[
            "name",
            "namespace",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        "attribute" => &[
            "name",
            "namespace",
            "select",
            "separator",
            "type",
            "validation",
        ],
        "namespace" => &["name", "select"],
        "comment" => &["select"],
        "processing-instruction" => &["name", "select"],
        "document" => &["validation", "type"],
        "copy" => &[
            "copy-namespaces",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        "copy-of" => &["select", "copy-namespaces", "type", "validation"],
        "message" => &["select", "terminate"],
        "strip-space" | "preserve-space" => &["elements"],
        "attribute-set" => &["name", "use-attribute-sets"],
        "namespace-alias" => &["stylesheet-prefix", "result-prefix"],
        "sort" => &[
            "select",
            "lang",
            "order",
            "collation",
            "stable",
            "case-order",
            "data-type",
        ],
        "key" => &["name", "match", "use", "collation"],
        "function" => &["name", "as", "override"],
        // Everything else (xsl:output, xsl:number, xsl:for-each-group, …) is
        // checked where it is compiled, or not at all in M1.
        _ => return None,
    })
}

/// Raises XTSE0090 for an attribute the element does not define.
fn check_attributes(nav: &Nav<'_>, local: &str, loc: &Location) -> Result<(), XsltError> {
    for (uri, name, _, _) in attributes(nav) {
        // §3.5: the standard attributes are spelled without a prefix on an
        // XSLT element, so an attribute that *is* in the XSLT namespace is
        // never one of them.
        if uri == XSLT_NS {
            return Err(XsltError::new(
                codes::XTSE0090,
                format!("xsl:{local} has an attribute xsl:{name} in the XSLT namespace"),
            )
            .with_location(loc.clone()));
        }
    }
    let Some(allowed) = allowed_attributes(local) else {
        return Ok(());
    };
    for (uri, name, _, _) in attributes(nav) {
        if !uri.is_empty() {
            continue;
        }
        if allowed.contains(&name.as_str()) || STANDARD_ATTRS.contains(&name.as_str()) {
            continue;
        }
        return Err(XsltError::new(
            codes::XTSE0090,
            format!("xsl:{local} has no attribute named {name:?}"),
        )
        .with_location(loc.clone()));
    }
    Ok(())
}

/// Reads the expression between `{` and its matching `}` in an AVT.
///
/// > \[ERR XTSE0350\] It is a static error if an unescaped left curly bracket
/// > appears in a fixed part of an attribute value template without a matching
/// > right curly bracket.
fn avt_expression(chars: &[char], mut i: usize, value: &str) -> Result<(String, usize), XsltError> {
    let mut source = String::new();
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                source.push(c);
            }
            (Some(_), c) => source.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                source.push(c);
            }
            (None, '}') => return Ok((source, i + 1)),
            (None, c) => source.push(c),
        }
        i += 1;
    }
    Err(XsltError::new(
        codes::XTSE0350,
        format!("an unmatched `{{` in the attribute value template {value:?}"),
    ))
}
