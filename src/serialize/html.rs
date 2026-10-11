//! The HTML 4.01 and XHTML 1.0 tables the `html` and `xhtml` methods consult
//! (Serialization §6, §7 and Appendix C). Names are compared in lower case:
//! the HTML method "MUST recognize the names of HTML elements regardless of
//! case" (§7.1), so its names are folded first; XHTML names are lower case by
//! definition and XML names are case-sensitive, so the `xhtml` method passes
//! them as they are (`markup.rs`, `vocabulary_name`).

use unicode_normalization::UnicodeNormalization;

/// §7.1: "For HTML 4.0, the element types that have an empty content model are
/// area, base, basefont, br, col, frame, hr, img, input, isindex, link, meta
/// and param." §6 lists the same thirteen for XHTML.
pub(crate) fn is_empty_element(lower: &str) -> bool {
    matches!(
        lower,
        "area"
            | "base"
            | "basefont"
            | "br"
            | "col"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "isindex"
            | "link"
            | "meta"
            | "param"
    )
}

/// The elements of HTML 4.01 (Strict, Transitional and Frameset). §7.1: an
/// element in no namespace whose name "is not recognized as the name of an
/// HTML element … MUST be output in the same way as a non-empty, inline element
/// such as span".
pub(crate) fn is_html_element(lower: &str) -> bool {
    is_inline_element(lower)
        || is_empty_element(lower)
        || matches!(
            lower,
            "address"
                | "blockquote"
                | "body"
                | "caption"
                | "center"
                | "colgroup"
                | "dd"
                | "del"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "form"
                | "frameset"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "head"
                | "html"
                | "ins"
                | "legend"
                | "li"
                | "menu"
                | "noframes"
                | "noscript"
                | "ol"
                | "optgroup"
                | "option"
                | "p"
                | "pre"
                | "style"
                | "table"
                | "tbody"
                | "td"
                | "textarea"
                | "tfoot"
                | "th"
                | "thead"
                | "title"
                | "tr"
                | "ul"
        )
}

/// The `%inline` category of the HTML 4.01 DTDs (§7.4.3, §6.1.3: "Whitespace
/// MUST NOT be added or removed adjacent to an inline element"): `%fontstyle`,
/// `%phrase`, `%special` and `%formctrl`. `ins` and `del` count "if they are
/// used as inline elements (i.e., if they do not contain element children)",
/// which the caller decides.
pub(crate) fn is_inline_element(lower: &str) -> bool {
    matches!(
        lower,
        "tt" | "i"
            | "b"
            | "u"
            | "s"
            | "strike"
            | "big"
            | "small"
            | "em"
            | "strong"
            | "dfn"
            | "code"
            | "samp"
            | "kbd"
            | "var"
            | "cite"
            | "abbr"
            | "acronym"
            | "a"
            | "img"
            | "applet"
            | "object"
            | "font"
            | "basefont"
            | "br"
            | "script"
            | "map"
            | "q"
            | "sub"
            | "sup"
            | "span"
            | "bdo"
            | "iframe"
            | "input"
            | "select"
            | "textarea"
            | "label"
            | "button"
    )
}

/// The formatted elements: "Whitespace MUST NOT be added or removed inside a
/// formatted element, the formatted elements being pre, script, style, and
/// textarea" (§7.4.3, §6.1.3).
pub(crate) fn is_formatted_element(lower: &str) -> bool {
    matches!(lower, "pre" | "script" | "style" | "textarea")
}

/// The boolean attributes of HTML 4.01 — "attributes with only a single allowed
/// value that is equal to the name of the attribute" — which §7.2 says the
/// HTML method "MUST output … in minimized form".
pub(crate) fn is_boolean_attribute(lower: &str) -> bool {
    matches!(
        lower,
        "checked"
            | "compact"
            | "declare"
            | "defer"
            | "disabled"
            | "ismap"
            | "multiple"
            | "nohref"
            | "noresize"
            | "noshade"
            | "nowrap"
            | "readonly"
            | "selected"
    )
}

/// Serialization Appendix C, "List of URI Attributes": "The following list of
/// attributes are declared as type %URI or %UriList for a given HTML or XHTML
/// element, with the exception of the name attribute for element A which is not
/// a URI type." The `name` of `a` is in the list all the same, and is escaped
/// as Appendix B.2.1 of HTML 4.01 recommends.
pub(crate) fn is_uri_attribute(element: &str, attribute: &str) -> bool {
    match attribute {
        "action" => element == "form",
        "archive" | "classid" | "data" => element == "object",
        "background" => element == "body",
        "cite" => matches!(element, "blockquote" | "del" | "ins" | "q"),
        "codebase" => matches!(element, "applet" | "object"),
        "datasrc" => matches!(
            element,
            "button" | "div" | "input" | "object" | "select" | "span" | "table" | "textarea"
        ),
        "for" => element == "script",
        "href" => matches!(element, "a" | "area" | "base" | "link"),
        "longdesc" => matches!(element, "frame" | "iframe" | "img"),
        "name" => element == "a",
        "profile" => element == "head",
        "src" => matches!(element, "frame" | "iframe" | "img" | "input" | "script"),
        "usemap" => matches!(element, "img" | "input" | "object"),
        _ => false,
    }
}

/// The first two steps of URI escaping (Serialization §4, rule a): "normalize
/// to NFC using the method defined in Section 7.4.6 fn:normalize-unicode",
/// then "percent-encode any special characters in the URI using the method
/// defined in Section 7.4.12 fn:escape-html-uri" — F&O 2.0 §7.4.12: "escapes all
/// characters except printable characters of the US-ASCII coded character set,
/// specifically the octets ranging from 32 to 126 (decimal)", each byte of the
/// UTF-8 encoding as `%HH` with upper-case hex digits. The third step, escaping
/// for HTML or XML, is the caller's.
pub(crate) fn escape_html_uri(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.nfc() {
        if (' '..='~').contains(&c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_escaping_keeps_printable_ascii_and_encodes_the_rest() {
        assert_eq!(
            escape_html_uri("http://iri.example.org/\u{FB4F}/\u{E5}r?x=y z"),
            "http://iri.example.org/%EF%AD%8F/%C3%A5r?x=y z"
        );
        // NFC first: a + combining ring is å.
        assert_eq!(escape_html_uri("a\u{30A}"), "%C3%A5");
        assert_eq!(escape_html_uri("\u{96}"), "%C2%96");
    }

    #[test]
    fn tables() {
        assert!(is_uri_attribute("a", "href"));
        assert!(!is_uri_attribute("h2", "style"));
        assert!(is_boolean_attribute("selected"));
        assert!(is_empty_element("br"));
        assert!(is_inline_element("span"));
        assert!(!is_inline_element("div"));
        assert!(is_html_element("table"));
        assert!(!is_html_element("out"));
    }
}
