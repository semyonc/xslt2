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
    match sheet.output().effective_method() {
        OutputMethod::Text => {
            let text = text_value(doc);
            if let Some(limit) = limit {
                // Serialization §8.1.2: "If the instance of the data model
                // contains a character that cannot be represented in the
                // encoding that the serializer is using for output, the
                // serializer MUST signal a serialization error [err:SERE0008]."
                // The text method has no character references to fall back on.
                if let Some(c) = text.chars().find(|&c| c > limit) {
                    return Err(unrepresentable(c, "the text output method"));
                }
            }
            Ok(text)
        }
        _ => {
            let options = sheet.output().serialize_options()?;
            let serialized =
                xsd_schema::document::serialize::to_string(&doc.create_navigator(), &options)
                    .map_err(|e| XsltError::internal(format!("could not serialize: {e}")))?;
            match limit {
                None => Ok(serialized),
                Some(limit) => escape_above(&serialized, limit),
            }
        }
    }
}

/// SERE0008 for a character `c` the output encoding cannot represent, found
/// where no character reference can stand in for it.
fn unrepresentable(c: char, context: &str) -> XsltError {
    XsltError::new(
        SERE0008,
        format!(
            "U+{:04X} cannot be represented in the output encoding and occurs in {context}, where \
             a character reference is not allowed",
            c as u32
        ),
    )
}

/// Serialization's "character not representable in a context where character
/// references are not allowed" (§5.1.2, §8.1.2); a serialization code, so it is
/// not among Appendix E's constants.
const SERE0008: &str = "SERE0008";

/// Where [`escape_above`] is in the serialized XML.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Markup {
    /// Character data between tags: a text node.
    Content,
    /// Inside a start or end tag, outside any attribute value: names.
    Tag,
    /// Inside the attribute value delimited by this quote character.
    AttributeValue(char),
    /// `<!-- … -->`.
    Comment,
    /// `<? … ?>`, the XML declaration included.
    ProcessingInstruction,
    /// `<![CDATA[ … ]]>`.
    CData,
}

/// Applies the `encoding` parameter's rule for a character above `limit` to
/// serialized XML.
///
/// XSLT 2.0 and XQuery 1.0 Serialization (Second Edition), §5.1.2 "XML Output
/// Method: the encoding Parameter" (not local; read at w3.org):
///
/// > … if the character occurs in a context where XML recognizes character
/// > references (that is, in the value of an attribute node or text node), then
/// > the character MUST be output as a character reference. A serialization
/// > error \[err:SERE0008\] occurs if such a character appears in a context
/// > where character references are not allowed (for example, if the character
/// > occurs in the name of an element). The serializer MUST signal the error.
/// > … If a comment node contains the same character, a serialization error
/// > \[err:SERE0008\] results.
///
/// and §5.1.4 for a CDATA section: "the currently open CDATA section MUST be
/// closed before such characters, the characters MUST be output using character
/// references or entity references, and a new CDATA section MUST be opened".
///
/// This processor writes UTF-8 whatever the encoding says, so this is the part
/// of the parameter it honours: the one the result's content depends on.
///
/// The input is what the upstream serializer wrote, which is well formed and
/// escapes `<` in text and attribute values and `"` in (always `"`-quoted)
/// attribute values, so a `<` can only open markup, and the markup kind is known
/// from the characters that follow it. Comments and processing instructions are
/// scanned to their own terminators, so a quote or a `>` in their data cannot
/// move the scan out of step.
fn escape_above(text: &str, limit: char) -> Result<String, XsltError> {
    let mut out = String::with_capacity(text.len());
    let mut state = Markup::Content;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let tail = &rest[c.len_utf8()..];
        // Markup delimiters: copy the whole delimiter and change state.
        let delimiter = match state {
            Markup::Content if rest.starts_with("<!--") => Some(("<!--", Markup::Comment)),
            Markup::Content if rest.starts_with("<![CDATA[") => Some(("<![CDATA[", Markup::CData)),
            Markup::Content if rest.starts_with("<?") => {
                Some(("<?", Markup::ProcessingInstruction))
            }
            Markup::Content if c == '<' => Some(("<", Markup::Tag)),
            Markup::Tag if c == '>' => Some((">", Markup::Content)),
            Markup::Tag if c == '"' || c == '\'' => Some((&rest[..1], Markup::AttributeValue(c))),
            Markup::AttributeValue(q) if c == q => Some((&rest[..1], Markup::Tag)),
            Markup::Comment if rest.starts_with("-->") => Some(("-->", Markup::Content)),
            Markup::ProcessingInstruction if rest.starts_with("?>") => {
                Some(("?>", Markup::Content))
            }
            Markup::CData if rest.starts_with("]]>") => Some(("]]>", Markup::Content)),
            _ => None,
        };
        if let Some((delimiter, next)) = delimiter {
            out.push_str(delimiter);
            rest = &rest[delimiter.len()..];
            state = next;
            continue;
        }
        if c > limit {
            match state {
                Markup::Content | Markup::AttributeValue(_) => {
                    out.push_str(&format!("&#{};", c as u32));
                }
                Markup::CData => out.push_str(&format!("]]>&#{};<![CDATA[", c as u32)),
                Markup::Tag => return Err(unrepresentable(c, "a name")),
                Markup::Comment => return Err(unrepresentable(c, "a comment")),
                Markup::ProcessingInstruction => {
                    return Err(unrepresentable(c, "a processing instruction"))
                }
            }
        } else {
            out.push(c);
        }
        rest = tail;
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::escape_above;

    const ASCII: char = '\u{7F}';

    fn code(text: &str) -> String {
        escape_above(text, ASCII).unwrap_err().code
    }

    #[test]
    fn content_and_attribute_values_take_character_references() {
        assert_eq!(
            escape_above("<a x=\"\u{E9}\" y='\u{E9}'>\u{E9}</a>", ASCII).unwrap(),
            "<a x=\"&#233;\" y='&#233;'>&#233;</a>"
        );
        // A quote of the other kind inside a value does not close it.
        assert_eq!(
            escape_above("<a x=\"it's \u{E9}\"/>t\u{E9}", ASCII).unwrap(),
            "<a x=\"it's &#233;\"/>t&#233;"
        );
        // Nothing above the limit: the text is returned as it stands.
        let plain = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><a k=\"v\"><!--c--><?p d?>t</a>";
        assert_eq!(escape_above(plain, ASCII).unwrap(), plain);
    }

    #[test]
    fn names_comments_and_processing_instructions_are_sere0008() {
        assert_eq!(code("<\u{E9}/>"), "SERE0008");
        assert_eq!(code("<a \u{E9}=\"v\"/>"), "SERE0008");
        assert_eq!(code("<a></\u{E9}>"), "SERE0008");
        assert_eq!(code("<a><!--\u{E9}--></a>"), "SERE0008");
        assert_eq!(code("<a><?p \u{E9}?></a>"), "SERE0008");
        assert_eq!(code("<a><?\u{E9} d?></a>"), "SERE0008");
        // A quote or `>` earlier in the comment used to switch escaping on.
        assert_eq!(code("<a><!--it's > \u{E9}--></a>"), "SERE0008");
        assert_eq!(code("<a><?p it's > \u{E9}?></a>"), "SERE0008");
    }

    /// Data in a comment or PI never moves the scan out of step with the markup.
    #[test]
    fn comment_and_pi_data_do_not_change_the_state_after_them() {
        assert_eq!(
            escape_above(
                "<a><!--it's \"x\" >--><?p a > 'b?>t\u{E9}<b c=\"\u{E9}\"/></a>",
                ASCII
            )
            .unwrap(),
            "<a><!--it's \"x\" >--><?p a > 'b?>t&#233;<b c=\"&#233;\"/></a>"
        );
        assert_eq!(code("<a><!--it's--><\u{E9}/></a>"), "SERE0008");
    }

    /// Serialization §5.1.4: "the currently open CDATA section MUST be closed
    /// before such characters, the characters MUST be output using character
    /// references or entity references, and a new CDATA section MUST be opened
    /// for any further characters in the text node."
    #[test]
    fn a_cdata_section_is_split_around_the_reference() {
        assert_eq!(
            escape_above("<a><![CDATA[x\u{E9}y]]></a>", ASCII).unwrap(),
            "<a><![CDATA[x]]>&#233;<![CDATA[y]]></a>"
        );
    }
}
