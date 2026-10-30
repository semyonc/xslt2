//! `xsl:function` (§10.3): the declaration pre-pass that puts every stylesheet
//! function into the static context of every expression, and the compilation
//! of the winning declarations.
//!
//! > A stylesheet function is included in the in-scope functions of the static
//! > context for all XPath expressions used in the stylesheet, unless there is
//! > another stylesheet function with the same name and arity, and higher
//! > import precedence, or the `override` attribute has the value `no` and
//! > there is already a function with the same name and arity in the in-scope
//! > functions.
//!
//! "All XPath expressions" includes a global variable declared before the
//! function, a pattern's predicate and a function body calling a function
//! declared further down, so the names, arities and winners are settled in a
//! pass of their own, before the first expression is compiled; the bodies are
//! compiled once the global variables are in scope.

use std::collections::HashMap;

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::{Builder, ScopeEntry};
use crate::error::{codes, XsltError};
use crate::functions::StylesheetFunctionTable;
use crate::model::*;
use crate::names::{is_ncname, parse_yes_no, QName, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, DeclKind, ModuleId};
use crate::whitespace;

/// One `xsl:function` declaration, as the pre-pass found it.
pub(super) struct FunctionDecl<'t> {
    name: QName,
    arity: usize,
    precedence: u32,
    override_vendor: bool,
    module: ModuleId,
    nav: Nav<'t>,
    /// The slot of the declaration that won for its name and arity; `None` for
    /// one that lost to a declaration of higher import precedence.
    winner: Option<StylesheetFunctionId>,
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// The pre-pass: every `xsl:function` of every module, its winner per
    /// expanded QName and arity, and the stylesheet-function table the binder
    /// consults (`StylesheetCatalog`).
    ///
    /// > \[ERR XTSE0770\] It is a static error for a stylesheet to contain two
    /// > or more functions with the same expanded-QName, the same arity, and the
    /// > same import precedence, unless there is another function with the same
    /// > expanded-QName and arity, and a higher import precedence.
    ///
    /// In schema-aware mode also for a winning stylesheet function that has the
    /// name and arity of a user atomic type's constructor function.
    pub(super) fn collect_functions(&mut self) -> Result<(), XsltError> {
        let modules = self.modules;
        let mut decls: Vec<FunctionDecl<'t>> = Vec::new();
        for unit in &modules.units {
            for (module_id, nav, kind) in &unit.decls {
                if *kind == DeclKind::Declaration
                    && nav.namespace_uri() == XSLT_NS
                    && nav.local_name() == "function"
                {
                    decls.push(self.function_decl(nav, *module_id, unit.precedence)?);
                }
            }
        }

        // The maximum precedence has to be known before a duplicate can be
        // judged: two declarations at one precedence are legal when a third
        // one above them overrides both.
        let mut highest: HashMap<(QName, usize), u32> = HashMap::new();
        for decl in &decls {
            let slot = highest
                .entry((decl.name.clone(), decl.arity))
                .or_insert(decl.precedence);
            *slot = (*slot).max(decl.precedence);
        }
        let mut won: HashMap<(QName, usize), usize> = HashMap::new();
        for (index, decl) in decls.iter().enumerate() {
            let key = (decl.name.clone(), decl.arity);
            if highest[&key] != decl.precedence {
                continue;
            }
            if won.insert(key, index).is_some() {
                return Err(XsltError::new(
                    codes::XTSE0770,
                    format!(
                        "two stylesheet functions named {} with {} parameter(s) have the same \
                         import precedence",
                        decl.name, decl.arity
                    ),
                )
                .with_location(self.modules.module(decl.module).location(&decl.nav)));
            }
        }

        let mut winners: Vec<usize> = won.into_values().collect();
        winners.sort_unstable();

        // Decision D-A (user, 2026-10-20; `type-functions-0503`): a constructor function of a user
        // atomic type (XPath 2.0 §3.10.4: "For every atomic type in the in-scope schema types …, a
        // constructor function is implicitly defined") has arity 1 and the type's expanded name.
        // §10.3's `override` does not decide a clash with it — it "defines what happens if this
        // function has the same name and arity as a function provided by the implementer or made
        // available in the static context using an implementation-defined mechanism", and the
        // standard `xsl:import-schema` is neither — while XPath 2.0 §2.1.1 identifies a function by
        // "its expanded QName and its arity" and counts "the signatures of constructor functions"
        // among the function signatures. Two functions under one name and arity is the clash XTSE0770
        // names, whatever `override` says. The built-in constructors live in the XML Schema
        // namespace, which a stylesheet function cannot use (§3.2, XTSE0080).
        if let Some(set) = self.schema {
            for index in &winners {
                let decl = &decls[*index];
                if decl.arity == 1
                    && crate::schema::has_user_constructor(set, &decl.name.uri, &decl.name.local)
                {
                    return Err(XsltError::new(
                        codes::XTSE0770,
                        format!(
                            "the stylesheet function {} with 1 parameter has the name and arity of \
                             the constructor function of the imported atomic type {}",
                            decl.name, decl.name
                        ),
                    )
                    .with_location(self.modules.module(decl.module).location(&decl.nav)));
                }
            }
        }
        let handles = self.functions.mint_handles(winners.len());
        let mut table = StylesheetFunctionTable::default();
        for (slot, (index, handle)) in winners.iter().zip(handles).enumerate() {
            let decl = &mut decls[*index];
            let id = StylesheetFunctionId(slot as u32);
            decl.winner = Some(id);
            table.insert(
                &decl.name.uri,
                &decl.name.local,
                decl.arity,
                handle,
                decl.override_vendor,
            );
            self.function_handles.insert(handle, id);
        }
        self.functions_table = (0..winners.len()).map(|_| None).collect();
        self.function_decls = decls;
        self.catalogs
            .table
            .set(table)
            .map_err(|_| XsltError::internal("the stylesheet-function table was filled twice"))
    }

    /// The name, arity and `override` of one `xsl:function`, with the static
    /// errors §10.3 gives the declaration and its parameters.
    fn function_decl(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        precedence: u32,
    ) -> Result<FunctionDecl<'t>, XsltError> {
        let modules = self.modules;
        let module = modules.module(module_id);
        let loc = module.location(nav);
        let ctx = self.context_of(nav, module)?;
        let text = attr_local(nav, "name").ok_or_else(|| {
            XsltError::new(codes::XTSE0010, "xsl:function has no name attribute")
                .with_location(loc.clone())
        })?;
        // > \[ERR XTSE0740\] A stylesheet function must have a prefixed name,
        // > to remove any risk of a clash with a function in the default
        // > function namespace. It is a static error if the name has no prefix.
        //
        // Only a *name* without a prefix: a value that is no QName at all is
        // XTSE0020, from `declared_name` below.
        if is_ncname(whitespace::trim(&text)) {
            return Err(XsltError::new(
                codes::XTSE0740,
                format!("the stylesheet function name {text:?} has no prefix"),
            )
            .with_location(loc));
        }
        // §10.3: "The prefix must not refer to a reserved namespace: [see ERR
        // XTSE0080]".
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .expect("the name attribute is present");
        // §10.3: `override? = "yes" | "no"`, "The default value is yes."
        let override_vendor = attr_local(nav, "override")
            .map(|value| parse_yes_no(&value, "override"))
            .transpose()
            .map_err(|e| e.or_location(&loc))?
            .unwrap_or(true);
        let mut arity = 0usize;
        for child in module.children(nav) {
            if child.node_type() != DomNodeType::Element {
                continue;
            }
            if child.namespace_uri() != XSLT_NS || child.local_name() != "param" {
                // §10.3: the parameters come first ("zero or more xsl:param
                // elements … followed by a sequence constructor"); the element
                // table has already rejected one placed later.
                break;
            }
            self.check_function_param(&child, module_id)?;
            arity += 1;
        }
        Ok(FunctionDecl {
            name,
            arity,
            precedence,
            override_vendor,
            module: module_id,
            nav: nav.clone(),
            winner: None,
        })
    }

    /// The rules §9.2 and §10.3 give an `xsl:param` of a stylesheet function.
    fn check_function_param(&self, nav: &Nav<'t>, module_id: ModuleId) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        // > \[ERR XTSE0760\] Because arguments to a stylesheet function call
        // > must all be specified, the xsl:param elements within an
        // > xsl:function element must not specify a default value: this means
        // > they must be empty, and must not have a select attribute.
        if attr_local(nav, "select").is_some() || self.has_content(nav, module_id) {
            return Err(XsltError::new(
                codes::XTSE0760,
                "a parameter of a stylesheet function cannot have a default value: it must be \
                 empty and have no select attribute",
            )
            .with_location(loc));
        }
        // §9.2: "The optional required attribute … may be specified for
        // stylesheet parameters and for template parameters; it must not be
        // specified for function parameters, which are always mandatory." The
        // sentence names no code; for an attribute the element must not have
        // here, XTSE0090 (§3.5) is the one that fits, and it is the code the
        // W3C suite expects (`function-0119a`, `function-0120a`).
        if attr_local(nav, "required").is_some() {
            return Err(XsltError::new(
                codes::XTSE0090,
                "a parameter of a stylesheet function cannot have a required attribute; \
                 function parameters are always mandatory",
            )
            .with_location(loc));
        }
        // §9.2: "The default is no; the value yes may be specified only for
        // template parameters" — and §10.1.2: "Tunnel parameters are not passed
        // in calls to stylesheet functions." An attribute value the element does
        // not permit is XTSE0020, as for a stylesheet parameter.
        if let Some(tunnel) = attr_local(nav, "tunnel") {
            if parse_yes_no(&tunnel, "tunnel").map_err(|e| e.or_location(&loc))? {
                return Err(XsltError::new(
                    codes::XTSE0020,
                    "a parameter of a stylesheet function cannot be a tunnel parameter; \
                     tunnel=\"yes\" is allowed only on a template parameter",
                )
                .with_location(loc));
            }
        }
        Ok(())
    }

    /// Compiles every `xsl:function` body — the losing declarations too, so
    /// that a static error in one is reported — and keeps the winners (and,
    /// for the static checks only, the losers' bodies).
    ///
    /// Run once the global variables are in scope: §10.3 "It is not possible
    /// within the body of the stylesheet function to access the values of local
    /// variables that were in scope in the place where the function call was
    /// written. Global variables, however, remain available." Nothing else is
    /// in scope here, so the frame is the function's own.
    pub(super) fn compile_functions(&mut self) -> Result<(), XsltError> {
        let decls: Vec<(usize, ModuleId, Nav<'t>, Option<StylesheetFunctionId>)> = self
            .function_decls
            .iter()
            .enumerate()
            .map(|(i, d)| (i, d.module, d.nav.clone(), d.winner))
            .collect();
        for (index, module_id, nav, winner) in decls {
            let compiled = self.function_body(index, &nav, module_id)?;
            match winner {
                Some(id) => self.functions_table[id.0 as usize] = Some(compiled),
                // Kept until the checks over the compiled tree have run
                // (`checks::check_call_template_params`): §2.9 "A static error
                // must be signaled even if it occurs in a part of the
                // stylesheet that is never evaluated", and XTSE0680 is only
                // decidable once every named template is known.
                None => self.losing_function_bodies.push(compiled.body),
            }
        }
        Ok(())
    }

    fn function_body(
        &mut self,
        index: usize,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<StylesheetFunction, XsltError> {
        let modules = self.modules;
        let module = modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let as_type = match attr_local(nav, "as") {
            Some(text) => Some(self.compile_type(&text, ctx)?),
            None => None,
        };
        let mut params = Vec::new();
        // §10.3: "xsl:param*" first; a later `xsl:param` belongs to the
        // sequence constructor (§3.9's fallback with forwards-compatible
        // behaviour, see `sequence_constructor_skipping`).
        for child in module.children(nav) {
            if super::instructions::leading_run_ends(nav, &child, &["param"]) {
                break;
            }
            if child.node_type() != DomNodeType::Element {
                continue;
            }
            let param_ctx = self.context_of(&child, module)?;
            let name = self
                .declared_name(&child, "name", param_ctx, module)?
                .ok_or_else(|| {
                    XsltError::new(codes::XTSE0010, "xsl:param has no name attribute")
                        .with_location(module.location(&child))
                })?;
            let param_as = match attr_local(&child, "as") {
                Some(text) => Some(self.compile_type(&text, param_ctx)?),
                None => None,
            };
            let slot = self.frame_size;
            self.frame_size += 1;
            self.scope.push(ScopeEntry {
                name: name.clone(),
                location: VarLocation::Local(slot),
            });
            params.push(FunctionParam {
                name,
                slot,
                as_type: param_as,
                loc: module.location(&child),
            });
        }
        let body = self.sequence_constructor_skipping(nav, module_id, "param")?;
        self.scope.truncate(scope_mark);
        let decl = &self.function_decls[index];
        Ok(StylesheetFunction {
            name: decl.name.clone(),
            params,
            as_type,
            body,
            frame_size: self.frame_size,
            override_vendor: decl.override_vendor,
            precedence: decl.precedence,
            module: module_id,
            loc: module.location(nav),
        })
    }
}
