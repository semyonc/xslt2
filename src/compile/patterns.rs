//! Compiling a `match` pattern (§5.5.2) against its own static context.

use xsd_schema::xpath::XPathExpr;

use super::Builder;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::FN_NS;
use crate::pattern::{self, PatternSink};
use crate::stylesheet::StaticContextId;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── Patterns ─────────────────────────────────────────────────────────

    pub(super) fn compile_pattern(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<PatternId, XsltError> {
        // Hand the whole pattern to the XPath parser first: a pattern is a
        // restriction of the path syntax, so anything the parser rejects as
        // *syntax* is XTSE0340 too — §5.5.2: "it is a static error if the
        // pattern does not match the production Pattern".
        //
        // A name the expression cannot resolve is a different error, and
        // keeps the XPath code. §5.3: it is a static error if an expression
        // "fails to satisfy other static constraints defined in the XPath
        // specification, for example that all variable references must refer
        // to variables that are in scope. Error codes are defined in
        // [XPath 2.0]" — and every pattern is an expression. That reading
        // needs the string to *be* a pattern, though: `$x` or `foo()` is not
        // one, whatever else is wrong with it, so it stays XTSE0340.
        //
        // An unknown function is XPST0017 here like anywhere else; only with
        // backwards-compatible behaviour, and only outside the standard
        // function namespace, does the context's catalog bind the call to
        // §3.8's fallback error function instead (`context_of`), so that it
        // compiles and raises XTDE1425 only if it is evaluated.
        let visible = self.visible_vars(ctx);
        let names: Vec<&str> = visible.iter().map(|(n, _)| n.as_str()).collect();
        match XPathExpr::compile_with_vars(source, self.compile_context(ctx), &names) {
            Err(e) => {
                return Err(match e.error_code() {
                    Some(code @ ("XPST0008" | "XPST0017" | "XPST0051" | "XPST0081"))
                        if has_pattern_shape(source) =>
                    {
                        XsltError::new(code, format!("cannot compile the pattern {source:?}: {e}"))
                    }
                    _ => {
                        XsltError::new(codes::XTSE0340, format!("{source:?} is not a pattern: {e}"))
                    }
                });
            }
            Ok(whole) => reject_grouping_functions(&whole, source)?,
        }

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
}

/// The two §14 functions that have no meaning in a pattern.
///
/// > \[§14.1\] \[ERR XTSE1060\] It is a static error if the current-group
/// > function is used within a pattern.
///
/// > \[§14.2\] \[ERR XTSE1070\] It is a static error if the
/// > current-grouping-key function is used within a pattern.
///
/// `whole` is the pattern compiled as one expression; its function calls are
/// every call anywhere in the pattern (`XPathExpr::function_calls`), in the
/// predicates of any step and in the argument of an `id()` / `key()` anchor.
/// A call inside the body of a stylesheet function the pattern calls is not
/// "used within a pattern": it is in the function's body.
fn reject_grouping_functions(whole: &XPathExpr, source: &str) -> Result<(), XsltError> {
    for call in whole.function_calls() {
        if call.namespace != FN_NS {
            continue;
        }
        let code = match call.local_name.as_str() {
            "current-group" => codes::XTSE1060,
            "current-grouping-key" => codes::XTSE1070,
            _ => continue,
        };
        return Err(XsltError::new(
            code,
            format!(
                "{}() is not allowed in the pattern {source:?}",
                call.local_name
            ),
        ));
    }
    Ok(())
}

/// Whether `source` matches the production Pattern, judged on its shape alone.
///
/// Asked only after the whole string has parsed as an XPath expression and
/// failed on a *name*, so the expressions inside its predicates are known to be
/// syntactically sound; what is left to decide is whether the outer structure
/// is a Pattern (§5.5.2). No prefix is resolved and no predicate is compiled.
/// A construct of the Pattern grammar that this processor does not implement
/// yet (an `id()`/`key()` pattern, a schema-aware node test) is still a
/// Pattern.
fn has_pattern_shape(source: &str) -> bool {
    struct ShapeOnly;
    impl PatternSink for ShapeOnly {
        fn resolve_prefix(&self, _prefix: &str) -> Option<String> {
            Some(String::new())
        }
        fn compile_predicate(&mut self, _source: &str) -> Result<ExprId, XsltError> {
            Ok(ExprId(0))
        }
    }
    match pattern::parse(source, &mut ShapeOnly) {
        Ok(_) => true,
        Err(e) => e.is_not_implemented(),
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
        // §5.3's forwards-compatible exception is for expressions; a pattern
        // that does not match the production Pattern is XTSE0340 wherever it
        // is written (§5.5.2).
        self.builder.compile_expr_strictly(source, self.ctx)
    }
}
