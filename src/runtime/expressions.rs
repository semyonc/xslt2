//! The bridge to the XPath engine: evaluating a compiled expression with the
//! focus and the variable bindings of §5.4, and recovering an XSLT error code
//! from an `XPathError` on the way out.

use xsd_schema::types::{DateTimeValue, TimezoneOffset, XmlAtomicValue, XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::datetime::current_datetime;
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{DynamicContext, XPathContext, XPathError, XmlItem};

use super::context::XsltContext;
use super::functions::XsltEvaluator;
use super::{Frame, Transformation};
use crate::error::XsltError;
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
        // §5.3: a static error that forwards-compatible behaviour deferred
        // fails the expression now that it is evaluated.
        if let Some(deferred) = &compiled.deferred {
            return Err(deferred.clone());
        }
        let ctx = self.sheet.context(compiled.ctx);

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
        let clock = self.clock(ctx).clone();
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
                if let Some((now, timezone)) = clock {
                    dynamic.current_datetime = Some(now);
                    dynamic.implicit_timezone = Some(timezone);
                }
                dynamic.set_function_evaluator(Some(&evaluator));
                // Compiled regular expressions outlive this evaluation.
                dynamic.set_regex_cache(Some(&self.xpath_regexes));
            });
        result.map_err(|e| self.xpath_error(e))
    }

    /// The current dateTime and implicit timezone of this transformation.
    ///
    /// §5.4.3.2: "The current date and time represents an
    /// implementation-dependent point in time during processing of the
    /// transformation; it does not change during the course of the
    /// transformation. The implicit timezone is implementation-defined." §16
    /// adds that "function implementations, current dateTime, implicit
    /// timezone, … must be stable" for the duration of the transformation.
    ///
    /// The engine fixes `current-dateTime()` only for one evaluation (its
    /// dynamic context caches it), so the transformation takes the value once,
    /// from the engine's own `current-dateTime()`, and hands it — and its
    /// timezone, the host's local offset, as the implicit timezone — to every
    /// dynamic context it builds. `None` only if the engine stops returning an
    /// `xs:dateTime`, in which case every evaluation keeps its own clock.
    fn clock(&self, ctx: &XPathContext<'_>) -> &Option<(DateTimeValue, TimezoneOffset)> {
        self.clock.get_or_init(|| {
            let mut dynamic = DynamicContext::<Nav<'t>>::new(ctx, 0);
            let value = current_datetime(&mut dynamic, Vec::new()).ok()?;
            match value.into_vec().into_iter().next()? {
                XmlItem::Atomic(XmlValue {
                    value: XmlValueKind::Atomic(XmlAtomicValue::DateTime(now)),
                    ..
                }) => {
                    let timezone = now.timezone?;
                    Some((now, timezone))
                }
                _ => None,
            }
        })
    }

    /// Turns an `XPathError` back into an `XsltError`: the error the evaluator
    /// raised inside a function call travels as the engine error's host
    /// payload and comes back unchanged.
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
        // An error the processor raised inside a function call is its host
        // payload; an error the engine identifies by an error QName (what
        // `fn:error` raises) is reported by that QName's local name.
        if let Some(raised) = crate::error::raised_error_to_xslt(&e) {
            return raised;
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
