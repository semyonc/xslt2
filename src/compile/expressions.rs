//! Static contexts (§5.4.1, §7.2), the variable scope an expression is
//! compiled against (§5.1) and the QName attributes of a declaration (§2.4).

use xsd_schema::navigator::DomNavigator;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::names::{resolve_qname, QName, XSLT_NS};
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

    pub(super) fn compile_expr(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<ExprId, XsltError> {
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
    /// > \[ERR XTSE0080\] It is a static error if a name occurring in the
    /// > stylesheet that is required to be a QName uses a reserved namespace.
    pub(super) fn declared_name(
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
}

/// The namespaces §3.2 reserves; a stylesheet may not declare a name in one.
const RESERVED_NAMESPACES: &[&str] = &[
    XSLT_NS,
    "http://www.w3.org/2005/xpath-functions",
    "http://www.w3.org/2001/XMLSchema",
    "http://www.w3.org/2001/XMLSchema-instance",
];
