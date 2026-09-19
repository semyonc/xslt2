//! Compiling a `match` pattern (§5.5.2) against its own static context.

use xsd_schema::xpath::XPathExpr;

use super::Builder;
use crate::error::{codes, XsltError};
use crate::model::*;
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
