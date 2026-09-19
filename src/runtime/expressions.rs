//! The bridge to the XPath engine: evaluating a compiled expression with the
//! focus and the variable bindings of §5.4, and recovering an XSLT error code
//! from an `XPathError` on the way out.

use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::XPathError;

use super::context::XsltContext;
use super::functions::XsltEvaluator;
use super::{Frame, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::Nav;

impl<'a, 't> Transformation<'a, 't> {
    // ── Expressions ──────────────────────────────────────────────────────

    pub(super) fn ebv(
        &self,
        id: ExprId,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<bool, XsltError> {
        let value = self.eval_expr(id, frame, cx)?;
        effective_boolean_value(&value).map_err(|e| self.xpath_error(e))
    }

    pub(super) fn eval_expr(
        &self,
        id: ExprId,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let compiled = self.sheet.expr(id);
        if let Some(why) = &compiled.unavailable {
            // §18.1.2: "When backwards compatible behavior is enabled, it is a
            // non-recoverable dynamic error to evaluate an extension function
            // call if no implementation of the extension function is
            // available."
            return Err(XsltError::new(codes::XTDE1425, why.clone()));
        }
        let ctx = self.sheet.context(compiled.ctx);
        // A parked error belongs to the evaluation that parked it; one that
        // was swallowed (a pattern predicate treated as a non-match, §5.5.4)
        // must not surface from the next unrelated XPath error.
        self.parked_error.borrow_mut().take();

        // Bind only the slots the expression actually references (§7.5).
        let mut bindings: Vec<(u32, XPathValue<Nav<'t>>)> = Vec::with_capacity(compiled.vars.len());
        for (slot, location) in compiled.vars.iter().enumerate() {
            if !compiled.expr.references_external_var(slot as u32) {
                continue;
            }
            let value = match location {
                VarLocation::Local(index) => {
                    frame.get(*index).cloned().unwrap_or_else(XPathValue::empty)
                }
                VarLocation::Global(index) => self.global(*index)?,
            };
            bindings.push((slot as u32, value));
        }

        let evaluator = XsltEvaluator {
            run: self,
            cx,
            sc: self.sheet.static_contexts.get(compiled.ctx),
        };
        let item = cx.item.clone();
        let (position, size) = (cx.position, cx.size);
        let result = compiled
            .expr
            .evaluator(ctx)
            .run_with_node_and_setup::<Nav<'t>, _>(None, |ev| {
                for (slot, value) in bindings {
                    ev.set_variable(slot, value);
                }
                let dynamic = ev.context();
                dynamic.context_item = item;
                dynamic.context_position = position;
                dynamic.context_size = size;
                dynamic.set_function_evaluator(Some(&evaluator));
            });
        result.map_err(|e| self.xpath_error(e))
    }

    /// Turns an `XPathError` back into an `XsltError`, recovering an XSLT code
    /// that the evaluator parked on the way out.
    ///
    /// The `None` arm assigns [`codes_internal::INTERNAL`], and since
    /// [`XsltError::is_recoverable_in_pattern`] refuses to recover from that
    /// code, the arm has to mean what it says. Two of the engine's uncoded
    /// variants are not internal failures at all but ordinary type errors of
    /// the stylesheet, and XPath 2.0 §B.2 gives them a code:
    ///
    /// > If the operator is not defined for the types of its operands, a type
    /// > error is raised \[err:XPTY0004\].
    ///
    /// They are mapped to it here, so that everything left in the `None` arm is
    /// genuinely a broken invariant of the engine or of this crate.
    ///
    /// [`codes_internal::INTERNAL`]: crate::error::codes_internal::INTERNAL
    pub(super) fn xpath_error(&self, e: XPathError) -> XsltError {
        if let Some(parked) = self.parked_error.borrow_mut().take() {
            return parked;
        }
        match e.error_code() {
            Some(code) => XsltError::new(code, e.to_string()),
            None => match e {
                XPathError::BinaryOperatorNotDefined { .. }
                | XPathError::UnaryOperatorNotDefined { .. } => {
                    XsltError::new("XPTY0004", e.to_string())
                }
                _ => XsltError::internal(e.to_string()),
            },
        }
    }
}
