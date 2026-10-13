//! `xsl:number` (§12): the place marker's source, the `count` / `from`
//! patterns, and the six conversion attributes of §12.3.

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::numbering::{self, NumberFormat};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, StaticContextId};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:number` instruction.
    ///
    /// The element table ([`checks`](super::checks)) has already checked which
    /// attributes it has. What it cannot express:
    ///
    /// > \[ERR XTSE0975\] It is a static error if the value attribute of
    /// > xsl:number is present unless the select, level, count, and from
    /// > attributes are all absent.
    ///
    /// and the permitted values of `level` and of the attribute value templates
    /// written without an expression (XTSE0020, §2.2: "It is a static error if
    /// an attribute (other than an attribute written using curly brackets in a
    /// position where an attribute value template is permitted) contains a
    /// value that is not one of the permitted values for that attribute").
    /// The `count` and `from` patterns are patterns like any other: XTSE0340,
    /// and §14's XTSE1060 / XTSE1070.
    pub(super) fn number(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let value = attr_local(nav, "value");
        let select = attr_local(nav, "select");
        let level = attr_local(nav, "level");
        let count = attr_local(nav, "count");
        let from = attr_local(nav, "from");
        let at = |e: XsltError| e.or_location(loc);

        let source = match value {
            Some(value) => {
                let present: Vec<&str> = [
                    ("select", &select),
                    ("level", &level),
                    ("count", &count),
                    ("from", &from),
                ]
                .iter()
                .filter(|(_, attr)| attr.is_some())
                .map(|(name, _)| *name)
                .collect();
                if !present.is_empty() {
                    return Err(XsltError::new(
                        codes::XTSE0975,
                        format!(
                            "xsl:number has a value attribute, so it may not have {}",
                            present.join(", ")
                        ),
                    )
                    .with_location(loc.clone()));
                }
                NumberSource::Value(self.compile_expr(&value, ctx).map_err(at)?)
            }
            None => {
                let level = match level.as_deref().map(whitespace::trim) {
                    None | Some("single") => NumberLevel::Single,
                    Some("multiple") => NumberLevel::Multiple,
                    Some("any") => NumberLevel::Any,
                    Some(other) => {
                        return Err(XsltError::new(
                            codes::XTSE0020,
                            format!(
                                "the level attribute of xsl:number must be single, multiple or \
                                 any, not {other:?}"
                            ),
                        )
                        .with_location(loc.clone()))
                    }
                };
                let select = match select {
                    Some(source) => Some(self.compile_expr(&source, ctx).map_err(at)?),
                    None => None,
                };
                let count = match count {
                    Some(source) => Some(self.compile_pattern(&source, ctx).map_err(at)?),
                    None => None,
                };
                let from = match from {
                    Some(source) => Some(self.compile_pattern(&source, ctx).map_err(at)?),
                    None => None,
                };
                NumberSource::Node {
                    select,
                    level,
                    count,
                    from,
                }
            }
        };

        let avt = |builder: &mut Self, name: &str| -> Result<Option<Avt>, XsltError> {
            match attr_local(nav, name) {
                Some(text) => Ok(Some(builder.avt(&text, ctx).map_err(at)?)),
                None => Ok(None),
            }
        };
        let format = avt(self, "format")?;
        let lang = avt(self, "lang")?;
        let letter_value = avt(self, "letter-value")?;
        let ordinal = avt(self, "ordinal")?;
        let grouping_separator = avt(self, "grouping-separator")?;
        let grouping_size = avt(self, "grouping-size")?;

        // The values written without an expression are checked now.
        let fixed = [
            ("lang", &lang),
            ("letter-value", &letter_value),
            ("grouping-separator", &grouping_separator),
            ("grouping-size", &grouping_size),
        ];
        for (name, avt) in fixed {
            if let Some(text) = avt.as_ref().and_then(Avt::as_literal) {
                if let Err(problem) = numbering::check_attribute(name, text) {
                    return Err(XsltError::new(codes::XTSE0020, problem).with_location(loc.clone()));
                }
            }
        }

        let format = format.map(|avt| match avt.as_literal() {
            Some(text) => NumberFormatSource::Fixed(NumberFormat::parse(text)),
            None => NumberFormatSource::Computed(avt),
        });
        let memo_slot = self.number_instructions;
        self.number_instructions += 1;
        Ok(InstructionKind::Number(Box::new(NumberInstruction {
            source,
            format,
            lang,
            letter_value,
            ordinal,
            grouping_separator,
            grouping_size,
            backwards_compatible: self.static_contexts.get(ctx).compat.is_backwards(),
            memo_slot,
        })))
    }
}
