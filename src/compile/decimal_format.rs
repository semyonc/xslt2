//! `xsl:decimal-format` (§16.4.1): the declarations, merged attribute by
//! attribute by import precedence, and their static errors — XTSE0020 (a value
//! that is not one character), XTSE1290 (conflicting values at the same
//! precedence), XTSE1295 (`zero-digit`) and XTSE1300 (the picture characters
//! must be distinct).

use std::collections::HashMap;

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::format_number::{is_zero_digit, DecimalFormat};
use crate::model::DecimalFormats;
use crate::names::QName;
use crate::receiver::Nav;
use crate::stylesheet::{attributes, Module, StaticContextId};

/// The attributes that set a variable, in §16.4.1's order. All but `infinity`
/// and `NaN` take a `char`.
const VARIABLES: [&str; 10] = [
    "decimal-separator",
    "grouping-separator",
    "infinity",
    "minus-sign",
    "NaN",
    "percent",
    "per-mille",
    "zero-digit",
    "digit",
    "pattern-separator",
];

/// Every declaration of one decimal format, as written.
#[derive(Default)]
pub(super) struct DecimalFormatDecls {
    /// For each attribute, every explicit value: its import precedence, the
    /// value, and where it was written.
    values: HashMap<&'static str, Vec<(u32, String, Location)>>,
    /// The first declaration, for the errors about the merged format.
    location: Option<Location>,
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// One `xsl:decimal-format` declaration, recorded for
    /// [`Self::finish_decimal_formats`].
    ///
    /// > In each case the value must be a single character \[see ERR
    /// > XTSE0020\].
    ///
    /// A `char` value is not trimmed: §2.2, "Except where the set of allowed
    /// values of an attribute is specified using the italicized name *string*
    /// or *char*, leading and trailing whitespace in the attribute value is
    /// ignored" (so `grouping-separator=" "` is a space, format-number-024).
    pub(super) fn decimal_format_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let location = module.location(nav);
        // `name? = qname`: XTSE0020 if it is not a lexical QName (or its prefix
        // is unbound), XTSE0080 if it is in a reserved namespace.
        let name = self.declared_name(nav, "name", ctx, module)?;
        let decls = self.decimal_format_decls.entry(name).or_default();
        decls.location.get_or_insert_with(|| location.clone());
        for (uri, local, _, value) in attributes(nav) {
            if !uri.is_empty() {
                continue;
            }
            // `name` and the standard attributes are not variables; an unknown
            // attribute is the static-check table's (XTSE0090), or ignored with
            // forwards-compatible behaviour.
            let Some(&variable) = VARIABLES.iter().find(|&&v| v == local) else {
                continue;
            };
            if !matches!(variable, "infinity" | "NaN") && value.chars().count() != 1 {
                return Err(XsltError::new(
                    codes::XTSE0020,
                    format!(
                        "the {variable} attribute of xsl:decimal-format must be a single \
                         character, not {value:?}"
                    ),
                )
                .with_location(location));
            }
            decls
                .values
                .entry(variable)
                .or_default()
                .push((precedence, value, location.clone()));
        }
        Ok(())
    }

    /// Merges every decimal format and checks it (§16.4.1).
    ///
    /// > For any named decimal format, the effective value of each attribute is
    /// > taken from an `xsl:decimal-format` declaration that has that name, and
    /// > that specifies an explicit value for the required attribute. If there
    /// > is no such declaration, the default value of the attribute is used. If
    /// > there is more than one such declaration, the one with highest import
    /// > precedence is used.
    ///
    /// (and the same sentence for the unnamed one). Then:
    ///
    /// > \[ERR XTSE1290\] It is a static error if a named or unnamed decimal
    /// > format contains two conflicting values for the same attribute in
    /// > different `xsl:decimal-format` declarations having the same import
    /// > precedence, unless there is another definition of the same attribute
    /// > with higher import precedence.
    ///
    /// > \[ERR XTSE1295\] It is a static error if the character specified in the
    /// > zero-digit attribute is not a digit or is a digit that does not have
    /// > the numeric value zero.
    ///
    /// > \[ERR XTSE1300\] It is a static error if, for any named or unnamed
    /// > decimal format, the variables representing characters used in a
    /// > picture string do not each have distinct values.
    ///
    /// The last two are about the effective values: a value overridden by a
    /// declaration of higher precedence is not checked (format-number-046,
    /// -059).
    pub(super) fn finish_decimal_formats(&mut self) -> Result<DecimalFormats, XsltError> {
        let mut formats = DecimalFormats::default();
        let mut decls: Vec<(Option<QName>, DecimalFormatDecls)> =
            std::mem::take(&mut self.decimal_format_decls)
                .into_iter()
                .collect();
        // Report errors in a stable order: the unnamed format first, then by
        // name.
        decls.sort_by(|(a, _), (b, _)| {
            a.as_ref()
                .map(|q| q.to_string())
                .cmp(&b.as_ref().map(|q| q.to_string()))
        });
        for (name, decls) in decls {
            let described = match &name {
                Some(name) => format!("the decimal format {name}"),
                None => "the unnamed decimal format".to_string(),
            };
            let mut format = DecimalFormat::default();
            for variable in VARIABLES {
                let Some(values) = decls.values.get(variable) else {
                    continue;
                };
                let top = values.iter().map(|(p, _, _)| *p).max().unwrap_or(0);
                let mut winners = values.iter().filter(|(p, _, _)| *p == top);
                let Some((_, value, _)) = winners.next() else {
                    continue;
                };
                if let Some((_, other, location)) = winners.find(|(_, v, _)| v != value) {
                    return Err(XsltError::new(
                        codes::XTSE1290,
                        format!(
                            "{described} has two conflicting values for {variable}, {value:?} \
                             and {other:?}, in declarations of the same import precedence"
                        ),
                    )
                    .with_location(location.clone()));
                }
                set_variable(&mut format, variable, value);
            }
            let location = decls.location.clone().unwrap_or_default();
            if !is_zero_digit(format.zero_digit) {
                let at = decls
                    .values
                    .get("zero-digit")
                    .and_then(|v| v.iter().max_by_key(|(p, _, _)| *p))
                    .map_or(location.clone(), |(_, _, l)| l.clone());
                return Err(XsltError::new(
                    codes::XTSE1295,
                    format!(
                        "the zero-digit of {described}, {:?}, is not a digit with the numeric \
                         value zero",
                        format.zero_digit
                    ),
                )
                .with_location(at));
            }
            if let Some((first, second, c)) = format.clashing_picture_characters() {
                return Err(XsltError::new(
                    codes::XTSE1300,
                    format!(
                        "{described} uses {c:?} for both {first} and {second}; the characters \
                         used in a picture string must be distinct"
                    ),
                )
                .with_location(location));
            }
            match name {
                Some(name) => {
                    formats.named.insert(name, format);
                }
                None => formats.unnamed = format,
            }
        }
        Ok(formats)
    }
}

/// Sets the variable an attribute names to its (checked) value.
fn set_variable(format: &mut DecimalFormat, variable: &str, value: &str) {
    let c = value.chars().next().unwrap_or('\0');
    match variable {
        "decimal-separator" => format.decimal_separator = c,
        "grouping-separator" => format.grouping_separator = c,
        "infinity" => format.infinity = value.to_string(),
        "minus-sign" => format.minus_sign = c,
        "NaN" => format.nan = value.to_string(),
        "percent" => format.percent = c,
        "per-mille" => format.per_mille = c,
        "zero-digit" => format.zero_digit = c,
        "digit" => format.digit = c,
        _ => format.pattern_separator = c,
    }
}
