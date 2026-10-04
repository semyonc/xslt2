//! Calling a stylesheet function (§10.3) from an XPath expression.
//!
//! The XPath engine binds the call to the handle the compiler minted for the
//! winning `xsl:function` and hands it, with the evaluated arguments, to
//! [`XsltEvaluator`](super::XsltEvaluator); this is where the body runs.

use xsd_schema::xpath::functions::XPathValue;

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::{Nav, SequenceCollector};

/// How much of [`TransformOptions::max_recursion_depth`] one stylesheet-function
/// call uses, where a template invocation uses 1.
///
/// A function call nests deeper on the host stack than a template call: the
/// XPath engine's own evaluation of the calling expression sits between the
/// two instruction levels. Measured in a release build (Q33, M2e): a
/// self-recursive function whose body is one `xsl:sequence` (`if ($n = 0) then
/// 0 else f:down($n - 1) + 1`) uses about 16.5 KB of stack per call, a
/// recursive named template whose `xsl:call-template` sits in an `xsl:choose`
/// about 7.5 KB. Counting each call as 8 stops a runaway recursion at 5 000
/// nested calls by default, which light and moderate bodies reach well inside
/// the default 256 MiB stack.
///
/// The count does **not** guarantee that the stack lasts: a heavy body — the
/// recursive call inside an `xsl:variable` holding ten nested literal result
/// elements, or inside ten nested `xsl:if`/`xsl:for-each`/`xsl:choose` levels —
/// uses so much stack per call that it would overflow before 5 000 calls. What
/// guarantees the depth error instead of an overflow is the stack guard
/// checked on the same entry ([`TransformOptions::stack_limit`]), provided one
/// body's own stack use between two calls stays inside the reserve above that
/// limit (32 MiB on the default stack).
///
/// [`TransformOptions::max_recursion_depth`]: super::TransformOptions::max_recursion_depth
/// [`TransformOptions::stack_limit`]: super::TransformOptions::stack_limit
pub(super) const FUNCTION_CALL_DEPTH: usize = 8;

impl<'a, 't> Transformation<'a, 't> {
    /// Evaluates a call to the stylesheet function `id` with `args`, the
    /// evaluated arguments in order.
    pub(super) fn call_stylesheet_function(
        &self,
        id: StylesheetFunctionId,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let depth = self.enter(FUNCTION_CALL_DEPTH)?;
        let result = self.stylesheet_function_body(id, args);
        self.depth.set(depth - FUNCTION_CALL_DEPTH);
        result
    }

    fn stylesheet_function_body(
        &self,
        id: StylesheetFunctionId,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let function = self.sheet.stylesheet_function(id);
        // §9.3/§10.3 "It is not possible within the body of the stylesheet
        // function to access the values of local variables that were in scope
        // in the place where the function call was written": a frame of its
        // own, holding the arguments and the body's local variables.
        let mut frame = Frame::with_slots(function.frame_size);
        // §10.3: "These are interpreted positionally. … The rules for converting
        // the values of the actual arguments supplied in the function call to
        // the types required by each xsl:param element are defined in [XPath
        // 2.0]. The rules that apply are those for the case where XPath 1.0
        // compatibility mode is set to false."
        //
        // > \[ERR XTTE0790\] If the value of a parameter to a stylesheet
        // > function cannot be converted to the required type, a type error is
        // > signaled.
        for (param, value) in function.params.iter().zip(args) {
            let value = self
                .convert(value, param.as_type.as_ref(), codes::XTTE0790)
                .map_err(|e| {
                    with_context(
                        e,
                        format!("the argument for ${} of {}", param.name, function.name),
                    )
                    .or_location(&param.loc)
                })?;
            frame.set(param.slot, value);
        }
        // §9.1 / DESIGN §9.1, the stylesheet-function row:
        // - focus absent — §10.3 "Within the body of a stylesheet function, the
        //   focus is initially undefined; this means that any attempt to
        //   reference the context item, context position, or context size is a
        //   non-recoverable dynamic error. [XPDY0002]", and with it `current()`
        //   (§16.6.1, XTDE1360 when there is none);
        // - current template rule null — §6.7 "when a stylesheet function is
        //   called … the current template rule becomes null", so
        //   `xsl:apply-imports` / `xsl:next-match` inside are XTDE0560;
        // - current mode the default mode — §6.5 "When a stylesheet function is
        //   called, the current mode is set to the default mode";
        // - tunnel parameters none — §10.1.2 "Tunnel parameters are not passed
        //   in calls to stylesheet functions", and a template called from the
        //   body starts from an empty base set;
        // - current group, grouping key and captured substrings empty (§14.1,
        //   §15.2): this context has none of them by construction.
        let cx = XsltContext::empty();
        // §10.3: "a sequence constructor that defines the value to be returned
        // by the function" — a sequence, not a tree.
        let mut collector = SequenceCollector::new(&self.store);
        self.eval_body(&function.body, &mut frame, &cx, &mut collector)?;
        let value = XPathValue::from_sequence(collector.into_items());
        // > \[ERR XTTE0780\] If the `as` attribute is specified, then the result
        // > evaluated by the sequence constructor (see 5.7 Sequence
        // > Constructors) is converted to the required type, using the function
        // > conversion rules. It is a type error if this conversion fails. If
        // > the `as` attribute is omitted, the calculated result is used as
        // > supplied, and no conversion takes place.
        self.convert(value, function.as_type.as_ref(), codes::XTTE0780)
            .map_err(|e| {
                with_context(e, format!("the result of {}", function.name))
                    .or_location(&function.loc)
            })
    }
}

/// Says which value a conversion error is about, without touching its code.
fn with_context(mut e: XsltError, what: String) -> XsltError {
    e.message = format!("{what}: {}", e.message);
    e
}
