//! `xsl:analyze-string` (§15.1): the input, the regular expression and its
//! flags, and the two optional children.

use std::sync::Arc;

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::names::XSLT_NS;
use crate::receiver::Nav;
use crate::regex::CompiledRegex;
use crate::stylesheet::{attr_local, ModuleId, StaticContextId};

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:analyze-string` instruction.
    ///
    /// The element table ([`checks`](super::checks)) has already checked the
    /// attributes and the order of the children — `(xsl:matching-substring?,
    /// xsl:non-matching-substring?, xsl:fallback*)`. What it cannot express is
    /// the one co-occurrence rule:
    ///
    /// > \[ERR XTSE1130\] It is a static error if the xsl:analyze-string
    /// > instruction contains neither an xsl:matching-substring nor an
    /// > xsl:non-matching-substring element.
    ///
    /// `xsl:fallback` children are skipped: "Any xsl:fallback elements among
    /// the children of the xsl:analyze-string instruction are ignored by an
    /// XSLT 2.0 processor".
    ///
    /// When neither `regex` nor `flags` contains an expression, the regular
    /// expression is compiled here, once, and an error in it is reported as a
    /// **static** error, which §15.1 allows for each of the three codes: "If
    /// the regular expression is known statically (for example, if the
    /// attribute does not contain any expressions enclosed in curly brackets)
    /// then the processor may signal the error as a static error."
    ///
    /// XTSE1130 is checked before the regular expression is compiled: it is
    /// an error in the content of this very instruction, and an instruction
    /// with neither child is wrong whatever its regular expression is. (§2.9
    /// allows either: "It is implementation-dependent which of the several
    /// errors is signaled"; this order reports the one about the instruction's
    /// own shape.)
    pub(super) fn analyze_string(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let select = self.required_expr(nav, "select", ctx, loc)?;
        let regex_avt = self
            .required_avt(nav, "regex", ctx, loc)
            .map_err(|e| e.or_location(loc))?;
        let flags_avt = match attr_local(nav, "flags") {
            Some(text) => Some(self.avt(&text, ctx).map_err(|e| e.or_location(loc))?),
            None => None,
        };
        let fixed_flags = match &flags_avt {
            None => Some(""),
            Some(avt) => avt.as_literal(),
        };

        // The two children, found before anything is compiled (XTSE1130).
        let mut matching_child = None;
        let mut non_matching_child = None;
        for child in self.children(nav, module_id) {
            if child.node_type() != DomNodeType::Element || child.namespace_uri() != XSLT_NS {
                continue;
            }
            match child.local_name() {
                "matching-substring" => matching_child = Some(child),
                "non-matching-substring" => non_matching_child = Some(child),
                // `xsl:fallback`, and in a forwards-compatible region anything
                // the element table let through.
                _ => {}
            }
        }
        if matching_child.is_none() && non_matching_child.is_none() {
            return Err(XsltError::new(
                codes::XTSE1130,
                "xsl:analyze-string contains neither an xsl:matching-substring nor an \
                 xsl:non-matching-substring element",
            )
            .with_location(loc.clone()));
        }

        let regex = match (regex_avt.as_literal(), fixed_flags) {
            (Some(pattern), Some(flags)) => RegexSource::Fixed(Arc::new(
                CompiledRegex::new(pattern, flags).map_err(|e| e.with_location(loc.clone()))?,
            )),
            _ => RegexSource::Computed {
                regex: regex_avt,
                flags: flags_avt,
            },
        };

        let matching = match &matching_child {
            Some(child) => Some(self.sequence_constructor(child, module_id)?),
            None => None,
        };
        let non_matching = match &non_matching_child {
            Some(child) => Some(self.sequence_constructor(child, module_id)?),
            None => None,
        };
        Ok(InstructionKind::AnalyzeString {
            select,
            regex,
            matching,
            non_matching,
        })
    }
}
