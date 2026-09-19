//! Attribute value templates (§5.6).

use super::Builder;
use xsd_schema::navigator::DomNavigator;

use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, StaticContextId};

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    pub(super) fn required_avt(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<Avt, XsltError> {
        let text = attr_local(nav, attr).ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!("xsl:{} has no {attr} attribute", nav.local_name()),
            )
            .with_location(loc.clone())
        })?;
        self.avt(&text, ctx)
    }

    /// §5.6: an attribute value template.
    pub(super) fn avt(&mut self, value: &str, ctx: StaticContextId) -> Result<Avt, XsltError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let chars: Vec<char> = value.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            match chars[i] {
                '{' if chars.get(i + 1) == Some(&'{') => {
                    literal.push('{');
                    i += 2;
                }
                '}' if chars.get(i + 1) == Some(&'}') => {
                    literal.push('}');
                    i += 2;
                }
                '}' => {
                    // > [ERR XTSE0370] It is a static error if an unescaped
                    // > right curly bracket occurs in a fixed part of an
                    // > attribute value template.
                    return Err(XsltError::new(
                        codes::XTSE0370,
                        format!("an unescaped `}}` in the attribute value template {value:?}"),
                    ));
                }
                '{' => {
                    if !literal.is_empty() {
                        parts.push(AvtPart::Literal(std::mem::take(&mut literal)));
                    }
                    let (source, next) = avt_expression(&chars, i + 1, value)?;
                    parts.push(AvtPart::Expr(self.compile_expr(&source, ctx)?));
                    i = next;
                }
                c => {
                    literal.push(c);
                    i += 1;
                }
            }
        }
        if !literal.is_empty() {
            parts.push(AvtPart::Literal(literal));
        }
        Ok(Avt { parts })
    }
}

/// Reads the expression between `{` and its matching `}` in an AVT.
///
/// > \[ERR XTSE0350\] It is a static error if an unescaped left curly bracket
/// > appears in a fixed part of an attribute value template without a matching
/// > right curly bracket.
fn avt_expression(chars: &[char], mut i: usize, value: &str) -> Result<(String, usize), XsltError> {
    let mut source = String::new();
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                source.push(c);
            }
            (Some(_), c) => source.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                source.push(c);
            }
            (None, '}') => return Ok((source, i + 1)),
            (None, c) => source.push(c),
        }
        i += 1;
    }
    Err(XsltError::new(
        codes::XTSE0350,
        format!("an unmatched `{{` in the attribute value template {value:?}"),
    ))
}
