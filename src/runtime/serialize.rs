//! Serializing a result tree per the stylesheet's merged `xsl:output` (§20).

use xsd_schema::document::{BufferDocument, BufferDocumentOptions};
use xsd_schema::navigator::DomNavigator;

use crate::error::{Location, XsltError};
use crate::model::CompiledStylesheet;
use crate::output::OutputMethod;

/// The options a run uses to serialize its primary result.
pub fn serialize_options(
    sheet: &CompiledStylesheet<'_>,
) -> Result<xsd_schema::document::serialize::SerializeOptions, XsltError> {
    sheet.output().serialize_options()
}

/// Serializes a result tree per the stylesheet's merged `xsl:output` (§20).
pub fn serialize_result(
    sheet: &CompiledStylesheet<'_>,
    doc: &BufferDocument<'_>,
) -> Result<String, XsltError> {
    let limit = sheet.output().character_escaping()?;
    let serialized = match sheet.output().effective_method() {
        OutputMethod::Text => text_value(doc),
        _ => {
            let options = sheet.output().serialize_options()?;
            xsd_schema::document::serialize::to_string(&doc.create_navigator(), &options)
                .map_err(|e| XsltError::internal(format!("could not serialize: {e}")))?
        }
    };
    Ok(match limit {
        None => serialized,
        Some(limit) => escape_above(&serialized, limit),
    })
}

/// Writes every character above `limit` as a numeric character reference.
///
/// §20: a character the output encoding cannot represent is serialized as a
/// character reference. M1 always writes UTF-8 bytes, so this is the only part
/// of the `encoding` parameter it can honour — and the part the result of a
/// transformation actually depends on, because the reference is in the result
/// text rather than in the byte encoding.
///
/// A character reference is only legal in content and in an attribute value,
/// never in a name, so the scan tracks whether it is inside a tag and, within
/// a tag, whether it is inside the quotes of an attribute value.
fn escape_above(text: &str, limit: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;
    for ch in text.chars() {
        let escapable = !in_tag || quote.is_some();
        match ch {
            '<' if !in_tag => {
                in_tag = true;
                out.push(ch);
            }
            '>' if in_tag && quote.is_none() => {
                in_tag = false;
                out.push(ch);
            }
            '"' | '\'' if in_tag => {
                quote = match quote {
                    Some(q) if q == ch => None,
                    Some(q) => Some(q),
                    None => Some(ch),
                };
                out.push(ch);
            }
            c if c > limit && escapable => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The `text` output method: the string values of the text nodes, in order.
fn text_value(doc: &BufferDocument<'_>) -> String {
    let mut out = String::new();
    let mut nav = doc.create_navigator();
    let root = nav.clone();
    if !nav.move_to_first_child() {
        return out;
    }
    let mut stack = vec![nav];
    while let Some(mut cursor) = stack.pop() {
        loop {
            if cursor.node_type().is_text_like() {
                out.push_str(&cursor.value());
            }
            let mut child = cursor.clone();
            if child.move_to_first_child() {
                let mut rest = cursor.clone();
                if rest.move_to_next_sibling() {
                    stack.push(rest);
                }
                cursor = child;
                continue;
            }
            if !cursor.move_to_next_sibling() {
                break;
            }
        }
    }
    let _ = root;
    out
}

/// A run's default `BufferDocumentOptions` for source documents.
pub fn source_options() -> BufferDocumentOptions {
    BufferDocumentOptions::default()
}

/// Where an error without its own location is reported.
pub fn unknown_location() -> Location {
    Location {
        module_uri: None,
        line: 0,
        column: 0,
    }
}
