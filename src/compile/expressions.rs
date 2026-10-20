//! Static contexts (§5.4.1, §7.2), the variable scope an expression is
//! compiled against (§5.1) and the QName attributes of a declaration (§2.4).

use xsd_schema::navigator::DomNavigator;
use xsd_schema::types::XmlTypeCode;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::names::{resolve_qname, split_qname, QName, XSD_NS, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, static_context_of, Module, StaticContextId};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── Static contexts ──────────────────────────────────────────────────

    /// Interns the lexical static context of `nav` and builds its
    /// [`XPathContext`] the first time it is seen (§7.2).
    pub(super) fn context_of(
        &mut self,
        nav: &Nav<'t>,
        module: &Module<'t>,
    ) -> Result<StaticContextId, XsltError> {
        let sc = static_context_of(nav, module, self.functions.collations())?;
        let before = self.static_contexts.len();
        let id = self.static_contexts.intern(sc);
        if self.static_contexts.len() > before {
            let sc = self.static_contexts.get(id);
            // §3.8 / §9.10 row 3: with backwards-compatible behaviour a call to
            // a function with no implementation (outside the standard function
            // namespace) binds to the fallback error function; everywhere else
            // — forwards-compatible behaviour included (§3.9 Note) — it is
            // XPath's static XPST0017.
            let catalog = if sc.compat.is_backwards() {
                self.functions.backwards_compatible_catalog()
            } else {
                self.functions.catalog()
            };
            let mut ctx = XPathContext::new(self.names)
                .with_namespaces(sc.snapshot(self.names))
                .with_base_uri(sc.base_uri.clone())
                .with_function_catalog(catalog)
                // §9.10 row 1: a `version < 2.0` region keeps XPath 2.0 syntax
                // and takes XPath 1.0 compatibility-mode semantics.
                .with_xpath10_compatibility(sc.compat.is_backwards())
                // DESIGN §9.5: every expression sees the collation registry and
                // the `[xsl:]default-collation` in force (§3.6.1: "the default
                // collation used by all XPath expressions appearing in the
                // attributes of this element, or attributes of descendant
                // elements").
                .with_collation_resolver(self.functions.collations())
                .with_default_collation(sc.default_collation.clone());
            if !sc.xpath_default_namespace.is_empty() {
                ctx = ctx.with_default_element_ns(self.names.add(&sc.xpath_default_namespace));
            }
            // §3.13 / §3.14: in schema-aware mode the in-scope schema
            // components — the built-in types and everything the context's set
            // holds (M4 plan D-7) — are those of the caller's schema set,
            // looked up by the ids of its own name table, which is
            // `self.names` (D-1).
            if let Some(set) = self.schema {
                ctx = ctx.with_schema_set(set);
            }
            // §10.3: the binder also sees the stylesheet functions — before
            // the processor's own functions for `override="yes"`, after them
            // for `override="no"`, and before the §3.8 fallback in either case.
            let compile_catalog = if sc.compat.is_backwards() {
                &self.catalogs.backwards
            } else {
                &self.catalogs.plain
            };
            let compile_ctx = ctx.clone().with_function_catalog(compile_catalog);
            self.contexts.push(ctx);
            self.compile_contexts.push(compile_ctx);
        }
        debug_assert_eq!(self.contexts.len(), self.static_contexts.len());
        debug_assert_eq!(self.compile_contexts.len(), self.static_contexts.len());
        Ok(id)
    }

    /// The context an expression in static context `ctx` is compiled against
    /// (see `Builder::compile_contexts`).
    pub(super) fn compile_context(&self, ctx: StaticContextId) -> &XPathContext<'m> {
        &self.compile_contexts[ctx.0 as usize]
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
    pub(super) fn visible_vars(&self, ctx: StaticContextId) -> Vec<(String, VarLocation)> {
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

    /// Compiles an XPath expression written in an attribute or in an attribute
    /// value template.
    ///
    /// > \[§5.3\] Except where forwards-compatible behavior is enabled (see 3.9
    /// > Forwards-Compatible Processing), it is a static error if the value of
    /// > such an attribute, or the text between curly brackets in an attribute
    /// > value template, does not match the XPath production Expr, or if it
    /// > fails to satisfy other static constraints defined in the XPath
    /// > specification, for example that all variable references must refer
    /// > to variables that are in scope.
    ///
    /// So where forwards-compatible behaviour is enabled, such an error is
    /// not reported when the stylesheet is compiled: the expression becomes
    /// one that raises it when — and only if — it is evaluated
    /// ([`Self::deferred_static_error`]). The one exception is a call to an
    /// unknown function, which §18.1.1's note keeps static: "In XSLT 2.0
    /// (without backwards compatibility enabled) a static error occurs when an
    /// XPath expression references a function that is not available. This is
    /// true even in a part of the stylesheet that uses forwards-compatible
    /// behavior." Every call of an expression that parses is checked
    /// ([`Self::unknown_function_call`]), so another static error the engine
    /// meets first cannot hide one.
    ///
    /// Patterns are not expressions (§5.5.2 has no such exception for
    /// XTSE0340), so a pattern's predicates go through
    /// [`Self::compile_expr_strictly`].
    pub(super) fn compile_expr(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<ExprId, XsltError> {
        match self.compile_expr_strictly(source, ctx) {
            Err(error)
                if self.static_contexts.get(ctx).compat.is_forwards()
                    && error.code != "XPST0017" =>
            {
                // The engine reports the first static error it meets, so an
                // undeclared variable or prefix can hide a call to an unknown
                // function in the same expression; that one stays static.
                if let Some(unknown) = self.unknown_function_call(source, ctx) {
                    return Err(unknown);
                }
                self.deferred_static_error(error, ctx)
            }
            other => other,
        }
    }

    /// The static error of the first function call in `source` whose name
    /// does not resolve here — XPST0017 for no function of that name and
    /// arity, XPST0081 for a prefix that is not bound — or `None` when every
    /// call resolves, or when `source` does not parse (then nothing is known
    /// about its function calls, and the syntax error is deferred whole).
    ///
    /// Each call found in the parsed expression is compiled on its own, with
    /// its arguments replaced by `()`: only the name and the arity are looked
    /// up, so only those two codes are taken from the probe.
    fn unknown_function_call(&self, source: &str, ctx: StaticContextId) -> Option<XsltError> {
        let parsed = xsd_schema::xpath::parse(source).ok()?;
        for (_, node) in parsed.arena.iter() {
            let xsd_schema::xpath::ast::AstNode::FunctionCall(call) = node else {
                continue;
            };
            let name = if call.prefix.is_empty() {
                call.local_name.clone()
            } else {
                format!("{}:{}", call.prefix, call.local_name)
            };
            let probe = format!("{name}({})", vec!["()"; call.args.len()].join(", "));
            if let Err(e) = XPathExpr::compile_with_vars(&probe, self.compile_context(ctx), &[]) {
                if let Some(code @ ("XPST0017" | "XPST0081")) = e.error_code() {
                    return Some(XsltError::new(
                        code,
                        format!(
                            "cannot compile the expression {source:?}: {e} (a call to a function \
                             that is not available is a static error even with forwards-compatible \
                             behavior, §18.1.1)"
                        ),
                    ));
                }
            }
        }
        None
    }

    /// Compiles an XPath expression; any static error is reported now.
    pub(super) fn compile_expr_strictly(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<ExprId, XsltError> {
        let visible = self.visible_vars(ctx);
        let names: Vec<&str> = visible.iter().map(|(n, _)| n.as_str()).collect();
        let vars: Vec<VarLocation> = visible.iter().map(|(_, l)| *l).collect();
        // A call to a function with no implementation needs no special case
        // here: with backwards-compatible behaviour the context's catalog
        // binds *the call* to §3.8's fallback error function (see
        // `context_of`), and anywhere else it is the static XPST0017.
        let expr = XPathExpr::compile_with_vars(source, self.compile_context(ctx), &names)
            .map_err(|e| {
                XsltError::new(
                    e.error_code().unwrap_or(codes::XTSE0010),
                    format!("cannot compile the expression {source:?}: {e}"),
                )
            })?;
        self.exprs.push(CompiledExpr {
            expr,
            ctx,
            vars,
            deferred: None,
        });
        Ok(ExprId(self.exprs.len() as u32 - 1))
    }

    /// An expression that fails with `error` when — and only if — it is
    /// evaluated (§5.3, forwards-compatible behaviour): the error rides in
    /// `CompiledExpr::deferred`, which `Transformation::eval_expr` returns
    /// before evaluating anything, so the run fails with the code the compiler
    /// would have reported, as the processor's error. The expression itself is
    /// the empty sequence, so the analyses that read a compiled expression
    /// (variable references, focus, function calls) see nothing.
    fn deferred_static_error(
        &mut self,
        error: XsltError,
        ctx: StaticContextId,
    ) -> Result<ExprId, XsltError> {
        let expr = XPathExpr::compile_with_vars("()", self.compile_context(ctx), &[])
            .map_err(|e| XsltError::internal(format!("cannot compile `()`: {e}")))?;
        let message = format!(
            "{} (a static error in a region with forwards-compatible behavior, deferred to \
             evaluation by §5.3)",
            error.message
        );
        self.exprs.push(CompiledExpr {
            expr,
            ctx,
            vars: Vec::new(),
            deferred: Some(XsltError::new(error.code, message)),
        });
        Ok(ExprId(self.exprs.len() as u32 - 1))
    }

    /// Compiles the `as` attribute of a variable, parameter or template.
    pub(super) fn compile_type(
        &mut self,
        text: &str,
        ctx: StaticContextId,
    ) -> Result<TypeCheck, XsltError> {
        let text = whitespace::trim(text).to_string();
        let test = self.compile_probe(&format!("$value instance of {text}"), ctx, &text)?;
        // The item type, without its occurrence indicator.
        let item = whitespace::trim(text.trim_end_matches(['?', '*', '+'])).to_string();
        let atomic = !item.contains('(') && !item.is_empty();
        // The expanded name decides which built-in type this is, never the
        // spelling: XPath 2.0 resolves a prefixed type name through the
        // in-scope namespaces, and an unprefixed one through the "default
        // element/type namespace … used for any unprefixed QName appearing in
        // a position where an element or type name is expected".
        let target = if atomic {
            let sc = self.static_contexts.get(ctx);
            let (prefix, local) = split_qname(&item);
            let uri = if prefix.is_empty() {
                Some(sc.xpath_default_namespace.clone())
            } else {
                sc.resolve_prefix(prefix)
            };
            match uri {
                Some(uri) if uri == XSD_NS => XmlTypeCode::from_local_name(local),
                _ => None,
            }
        } else {
            None
        };
        let cast = if atomic && target != Some(XmlTypeCode::AnyAtomicType) {
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
            target,
        })
    }

    /// Compiles a helper expression whose only variable is `$value`.
    fn compile_probe(
        &mut self,
        source: &str,
        ctx: StaticContextId,
        what: &str,
    ) -> Result<ExprId, XsltError> {
        let expr = XPathExpr::compile_with_vars(source, self.compile_context(ctx), &["value"])
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
            deferred: None,
        });
        Ok(ExprId(self.exprs.len() as u32 - 1))
    }

    pub(super) fn required_expr(
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
    pub(super) fn qname_attr(
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

    /// The `use-attribute-sets` attribute of `xsl:element`, `xsl:copy` or
    /// `xsl:attribute-set`.
    pub(super) fn qname_list(
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

    /// One `use-attribute-sets` / `xsl:use-attribute-sets` value (§10.2).
    ///
    /// > \[ERR XTSE0710\] It is a static error if the value of the
    /// > `use-attribute-sets` attribute of an `xsl:copy`, `xsl:element`, or
    /// > `xsl:attribute-set` element, or the `xsl:use-attribute-sets` attribute
    /// > of a literal result element, is not a whitespace-separated sequence of
    /// > EQNames, or if it contains a QName that does not match the `name`
    /// > attribute of any `xsl:attribute-set` declaration in the stylesheet.
    ///
    /// Both halves of that rule are XTSE0710, and both are **static**: every
    /// name is remembered here and checked against the declared sets once the
    /// declaration walk is over ([`Builder::check_attribute_set_references`]),
    /// so an attribute set that is never used at run time is validated too.
    pub(super) fn qname_list_value(
        &mut self,
        text: &str,
        ctx: StaticContextId,
    ) -> Result<Vec<QName>, XsltError> {
        let names: Vec<QName> = whitespace::tokens(text)
            .map(|token| self.resolve(token, ctx, codes::XTSE0710))
            .collect::<Result<_, _>>()?;
        self.attr_set_refs.extend(names.iter().cloned());
        Ok(names)
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        ctx: StaticContextId,
        code: &str,
    ) -> Result<QName, XsltError> {
        let sc = self.static_contexts.get(ctx);
        resolve_qname(name, |prefix| sc.resolve_prefix(prefix), code)
    }

    /// The name a declaration gives itself, rejecting the reserved namespaces.
    ///
    /// > \[ERR XTSE0080\] It is a static error to use a reserved namespace in
    /// > the name of a named template, a mode, an attribute set, a key, a
    /// > decimal-format, a variable or parameter, a stylesheet function, a named
    /// > output definition, or a character map.
    pub(super) fn declared_name(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<Option<QName>, XsltError> {
        self.checked_declared_name(nav, attr, ctx, module, false)
    }

    /// [`Self::declared_name`] for `xsl:template`: the same check, except that a
    /// template may be named `xsl:initial-template`, the conventional default
    /// entry point. The exemption is for templates only — a stylesheet
    /// function or an attribute set of that name is XTSE0080 like any other
    /// name in the XSLT namespace.
    pub(super) fn declared_template_name(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        module: &Module<'t>,
    ) -> Result<Option<QName>, XsltError> {
        self.checked_declared_name(nav, "name", ctx, module, true)
    }

    fn checked_declared_name(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        module: &Module<'t>,
        entry_point_allowed: bool,
    ) -> Result<Option<QName>, XsltError> {
        let Some(name) = self.qname_attr(nav, attr, ctx, module)? else {
            return Ok(None);
        };
        let is_entry_point =
            entry_point_allowed && name.uri == XSLT_NS && name.local == "initial-template";
        if !is_entry_point {
            self.reject_reserved(&name, nav, module)?;
        }
        Ok(Some(name))
    }

    /// XTSE0080 for a name that is not read through [`Self::declared_name`]:
    /// a mode, which a `mode` attribute names rather than declares.
    pub(super) fn reject_reserved(
        &self,
        name: &QName,
        nav: &Nav<'t>,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        if RESERVED_NAMESPACES.contains(&name.uri.as_str()) {
            return Err(XsltError::new(
                codes::XTSE0080,
                format!("{name} is in a reserved namespace"),
            )
            .with_location(module.location(nav)));
        }
        Ok(())
    }
}

/// The namespaces §3.2 reserves ("The reserved namespaces are those listed
/// below"); a stylesheet may not declare a name in one: the XSLT namespace, the
/// standard function namespace, the XML namespace, the schema namespace, the
/// schema instance namespace, and "The namespace `http://www.w3.org/2000/xmlns/`
/// is reserved for use as described in \[Namespaces in XML 1.0\]".
const RESERVED_NAMESPACES: &[&str] = &[
    XSLT_NS,
    "http://www.w3.org/2005/xpath-functions",
    crate::names::XML_NS,
    "http://www.w3.org/2001/XMLSchema",
    "http://www.w3.org/2001/XMLSchema-instance",
    "http://www.w3.org/2000/xmlns/",
];
