//! XML whitespace — the only whitespace XSLT knows (§4).
//!
//! > \[Definition: A **whitespace text node** is a text node whose content
//! > consists entirely of whitespace characters (that is, #x09, #x0A, #x0D, or
//! > #x20).\]
//!
//! Rust's [`char::is_whitespace`] follows the Unicode `White_Space` property
//! and so also matches U+00A0 NO-BREAK SPACE, U+2007 FIGURE SPACE, U+3000
//! IDEOGRAPHIC SPACE and a dozen others; [`str::trim`], [`str::trim_start`],
//! [`str::trim_end`] and [`str::split_whitespace`] all build on it. Using any
//! of them for an XSLT or XPath decision is a bug: it would strip a no-break
//! space out of a source document under `xsl:strip-space`, split a
//! whitespace-separated attribute list in the middle of a token, or accept a
//! name that is not an NCName.
//!
//! Everything in this crate that asks "is this whitespace?" goes through this
//! module. The same four characters are production `S` of XML 1.0 and of the
//! XPath grammar, so the helpers also serve for trimming expression text and
//! the values of QName-valued and enumerated attributes.

/// The four characters XML calls whitespace: tab, newline, carriage return and
/// space.
pub const XML_WHITESPACE: [char; 4] = ['\u{9}', '\u{A}', '\u{D}', '\u{20}'];

/// Whether `c` is one of the four XML whitespace characters.
#[inline]
pub fn is_xml_whitespace(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}')
}

/// Whether `text` consists **entirely** of XML whitespace — the test §4's
/// "whitespace text node" turns on. An empty string qualifies.
#[inline]
pub fn is_all_xml_whitespace(text: &str) -> bool {
    text.chars().all(is_xml_whitespace)
}

/// [`str::trim`] over XML whitespace only.
#[inline]
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_xml_whitespace)
}

/// [`str::trim_start`] over XML whitespace only.
#[inline]
pub fn trim_start(text: &str) -> &str {
    text.trim_start_matches(is_xml_whitespace)
}

/// [`str::trim_end`] over XML whitespace only.
#[inline]
pub fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_xml_whitespace)
}

/// The tokens of a whitespace-separated attribute value — `elements`, `mode`,
/// `use-attribute-sets`, `exclude-result-prefixes`, `default-collation`, … —
/// which §3.5 and the element syntax summaries all define over XML
/// whitespace.
///
/// Empty tokens are skipped, so leading, trailing and repeated separators are
/// harmless, exactly as [`str::split_whitespace`] would have them.
pub fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(XML_WHITESPACE).filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters Rust calls whitespace and XML does not.
    const NOT_XML_WHITESPACE: &[char] = &[
        '\u{A0}',   // NO-BREAK SPACE
        '\u{B}',    // LINE TABULATION
        '\u{C}',    // FORM FEED
        '\u{85}',   // NEXT LINE
        '\u{1680}', // OGHAM SPACE MARK
        '\u{2000}', // EN QUAD
        '\u{2028}', // LINE SEPARATOR
        '\u{3000}', // IDEOGRAPHIC SPACE
    ];

    #[test]
    fn only_the_four_xml_characters_are_whitespace() {
        for c in XML_WHITESPACE {
            assert!(is_xml_whitespace(c), "{c:?} is XML whitespace");
        }
        for &c in NOT_XML_WHITESPACE {
            assert!(!is_xml_whitespace(c), "{c:?} is not XML whitespace");
            // The std functions disagree, which is the whole point.
            assert!(c.is_whitespace(), "{c:?} is Unicode whitespace");
        }
    }

    #[test]
    fn a_whitespace_text_node_is_only_the_four() {
        assert!(is_all_xml_whitespace(""));
        assert!(is_all_xml_whitespace(" \t\r\n "));
        assert!(!is_all_xml_whitespace("\u{A0}"));
        assert!(!is_all_xml_whitespace(" \u{3000} "));
        assert!(!is_all_xml_whitespace("x"));
    }

    #[test]
    fn trimming_keeps_the_characters_xml_does_not_call_whitespace() {
        assert_eq!(trim("  a  "), "a");
        assert_eq!(trim("\t\r\na\n"), "a");
        assert_eq!(trim("\u{A0}a\u{A0}"), "\u{A0}a\u{A0}");
        assert_eq!(trim_start(" \u{A0}a "), "\u{A0}a ");
        assert_eq!(trim_end(" a\u{A0} "), " a\u{A0}");
        assert_eq!(trim(""), "");
        assert_eq!(trim("   "), "");
    }

    #[test]
    fn tokens_split_on_xml_whitespace_only() {
        assert_eq!(tokens("a b\tc\nd").collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        assert_eq!(tokens("  a   b  ").collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(tokens("").collect::<Vec<_>>(), Vec::<&str>::new());
        assert_eq!(tokens("   ").collect::<Vec<_>>(), Vec::<&str>::new());
        // One token, not two: a no-break space is part of the name.
        assert_eq!(tokens("a\u{A0}b").collect::<Vec<_>>(), vec!["a\u{A0}b"]);
    }
}
