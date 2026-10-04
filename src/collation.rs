//! Collations: the registry every expression of a stylesheet sees, and the
//! collations `xsl:sort` chooses (§13.1.3, §3.6.1; DESIGN §9.5).
//!
//! > \[§13.1.3\] A collation is identified by a URI, but the manner in which
//! > this URI is associated with an actual rule or algorithm is
//! > implementation-defined. The one collation URI that must be recognized by
//! > every implementation is
//! > `http://www.w3.org/2005/xpath-functions/collation/codepoint`.
//!
//! This processor recognizes, with no dependency:
//!
//! * the **codepoint** collation ([`CODEPOINT_COLLATION_URI`]), which the XPath
//!   engine implements itself and never looks up through a resolver;
//! * the **HTML ASCII case-insensitive** collation
//!   ([`HTML_ASCII_CASE_INSENSITIVE_URI`], [`HtmlAsciiCaseInsensitive`]);
//! * every collation a host registers with
//!   [`XsltFunctions::register_collation`](crate::XsltFunctions::register_collation).
//!
//! [`CollationRegistry`] holds them and implements the engine's
//! [`CollationResolver`]; the compiler hands it, together with the
//! `[xsl:]default-collation` in force, to every `XPathContext` it builds, so
//! `compare()`, `=`, `distinct-values()` and the rest see the same collations
//! as `xsl:sort`.
//!
//! The registry lives in [`XsltFunctions`](crate::XsltFunctions) because that is
//! the one caller-owned object every `XPathContext` of a stylesheet already
//! borrows (its function catalog); the resolver has to be borrowed for exactly
//! the same lifetime.
//!
//! `xsl:sort` without a `collation` attribute but with `lang` or `case-order`
//! uses [`CaseOrderCollation`], this processor's implementation-defined answer
//! to §13.1.3 in the absence of language-specific collation data (see there).
//! The UCA collations of F&O 3.0 (`http://www.w3.org/2013/collation/UCA?…`)
//! need locale data and are left to a later cargo feature.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

pub use xsd_schema::xpath::collation::{Collation, CollationResolver, CODEPOINT_COLLATION_URI};

/// `http://www.w3.org/2005/xpath-functions/collation/html-ascii-case-insensitive`.
pub const HTML_ASCII_CASE_INSENSITIVE_URI: &str =
    "http://www.w3.org/2005/xpath-functions/collation/html-ascii-case-insensitive";

/// The prefix of the UCA collation URIs of F&O 3.0, which need locale data
/// this processor does not carry yet.
pub(crate) const UCA_COLLATION_PREFIX: &str = "http://www.w3.org/2013/collation/UCA";

/// The owner of the UCA collations (`http://www.w3.org/2013/collation/UCA?…`),
/// for the `XSLT2-NOTIMPL` they raise: roadmap milestone M2k, "ICU/UCA
/// collations behind a cargo feature, after M2g" (DESIGN §9.5). Spelled as a
/// bare milestone code, the form `(M2k)` that the conformance harness
/// attributes to an owner.
pub(crate) const UCA_MILESTONE: &str = "M2k";

/// The HTML ASCII case-insensitive collation.
///
/// Strings are compared after mapping the ASCII letters `A`–`Z` to `a`–`z`,
/// codepoint by codepoint; every other character is compared as it is. Each
/// character is one collation unit, so the substring functions (`contains`,
/// `starts-with`, …) work under it. Because the fold maps one byte to one
/// byte, a byte offset into the folded string is a byte offset into the
/// original, which is what [`find`](Collation::find) has to return.
#[derive(Debug, Default, Clone, Copy)]
pub struct HtmlAsciiCaseInsensitive;

impl Collation for HtmlAsciiCaseInsensitive {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        // UTF-8 byte order is codepoint order, and the ASCII fold preserves it.
        a.bytes()
            .map(|c| c.to_ascii_lowercase())
            .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
    }

    fn equals(&self, a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }

    fn sort_key(&self, s: &str) -> Option<Vec<u8>> {
        Some(s.bytes().map(|c| c.to_ascii_lowercase()).collect())
    }

    fn find(&self, haystack: &str, needle: &str) -> Option<Option<(usize, usize)>> {
        let folded = haystack.to_ascii_lowercase();
        let needle = needle.to_ascii_lowercase();
        Some(
            folded
                .find(&needle)
                .map(|start| (start, start + needle.len())),
        )
    }

    fn starts_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        Some(
            haystack.len() >= needle.len()
                && haystack.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes()),
        )
    }

    fn ends_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        Some(
            haystack.len() >= needle.len()
                && haystack.as_bytes()[haystack.len() - needle.len()..]
                    .eq_ignore_ascii_case(needle.as_bytes()),
        )
    }
}

/// Which case `xsl:sort` puts first among strings that differ only in case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOrder {
    /// `case-order="upper-first"`.
    UpperFirst,
    /// `case-order="lower-first"`, and the default when only `lang` is given.
    LowerFirst,
}

/// The collation `xsl:sort` uses when it has `lang` or `case-order` but no
/// `collation` attribute — this processor's implementation-defined algorithm
/// (§13.1.3).
///
/// > \[§13.1.3\] The `lang` and `case-order` attributes are ignored if a
/// > `collation` attribute is present. But in the absence of a `collation`
/// > attribute, these attributes provide input to an implementation-defined
/// > algorithm to locate a suitable collation.
///
/// Without language-specific collation data (ICU is a later, optional
/// feature, milestone M2k) the same algorithm serves every `lang`:
///
/// 1. strings are ordered first by their Unicode lower-case mapping, compared
///    codepoint by codepoint — so `MacAndrew` precedes `macintosh`;
/// 2. strings with the same lower-case mapping are ordered by the first
///    character in which they differ, which then differs only in case, as
///    §13.1.3 asks: "when two strings differ only in the case of one or more
///    characters, then a string in which the first differing character is
///    lower-case should precede a string in which the corresponding character
///    is title-case, which should in turn precede a string in which the
///    corresponding character is upper-case" (`lower-first`; the mirror image
///    for `upper-first`);
/// 3. anything still equal is ordered by codepoints, so that only identical
///    strings compare equal and the order is total.
///
/// Accents and other secondary differences get no special treatment: `ä`
/// sorts after `z`, by codepoint.
#[derive(Debug, Clone, Copy)]
pub struct CaseOrderCollation {
    pub case_order: CaseOrder,
}

impl CaseOrderCollation {
    /// The rank §13.1.3 orders a character's case by: lower, title, upper.
    fn rank(&self, c: char) -> u8 {
        let natural = if c.is_lowercase() {
            0
        } else if c.is_uppercase() {
            2
        } else {
            // Title-case letters (Lt) are neither; so is every non-letter, for
            // which the rank never decides anything (they have no case pair).
            1
        };
        match self.case_order {
            CaseOrder::LowerFirst => natural,
            CaseOrder::UpperFirst => 2 - natural,
        }
    }
}

impl Collation for CaseOrderCollation {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        let folded = a
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(b.chars().flat_map(char::to_lowercase));
        if folded != Ordering::Equal {
            return folded;
        }
        for (x, y) in a.chars().zip(b.chars()) {
            if x != y {
                return (self.rank(x), x).cmp(&(self.rank(y), y));
            }
        }
        a.cmp(b)
    }
}

/// The collations a stylesheet's expressions and `xsl:sort` can name: the
/// built-in ones and the host's.
///
/// It implements [`CollationResolver`], which is how the XPath engine reaches
/// it; the codepoint collation is the engine's own and is never asked for.
pub struct CollationRegistry {
    host: HashMap<String, Rc<dyn Collation>>,
    html_ascii: Rc<dyn Collation>,
}

impl Default for CollationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CollationRegistry {
    /// The built-in collations only: codepoint and HTML ASCII case-insensitive.
    pub fn new() -> Self {
        Self {
            host: HashMap::new(),
            html_ascii: Rc::new(HtmlAsciiCaseInsensitive),
        }
    }

    /// Registers a host collation under an absolute URI.
    ///
    /// A host collation may replace a built-in one, except the codepoint
    /// collation: the XPath engine implements that itself and never consults a
    /// resolver for it, so registering [`CODEPOINT_COLLATION_URI`] would have no
    /// effect anywhere — the call refuses it and returns `false`. Every other
    /// registration returns `true`; a later registration of the same URI
    /// replaces an earlier one.
    pub fn register(&mut self, uri: impl Into<String>, collation: Rc<dyn Collation>) -> bool {
        let uri = uri.into();
        if uri == CODEPOINT_COLLATION_URI {
            return false;
        }
        self.host.insert(uri, collation);
        true
    }

    /// Whether `uri` (absolute) names a collation this processor can use.
    pub fn recognizes(&self, uri: &str) -> bool {
        uri == CODEPOINT_COLLATION_URI || self.resolve(uri).is_some()
    }
}

impl CollationResolver for CollationRegistry {
    fn resolve(&self, uri: &str) -> Option<Rc<dyn Collation>> {
        if let Some(collation) = self.host.get(uri) {
            return Some(Rc::clone(collation));
        }
        (uri == HTML_ASCII_CASE_INSENSITIVE_URI).then(|| Rc::clone(&self.html_ascii))
    }
}

impl fmt::Debug for CollationRegistry {
    /// A collation is opaque (the trait has no `Debug` bound), so the registry
    /// prints the URIs it recognizes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut host: Vec<&str> = self.host.keys().map(String::as_str).collect();
        host.sort_unstable();
        f.debug_struct("CollationRegistry")
            .field(
                "built_in",
                &[CODEPOINT_COLLATION_URI, HTML_ASCII_CASE_INSENSITIVE_URI],
            )
            .field("host", &host)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(words: &[&str], collation: &dyn Collation) -> Vec<String> {
        let mut out: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        out.sort_by(|a, b| collation.compare(a, b));
        out
    }

    #[test]
    fn html_ascii_folds_only_ascii_letters() {
        let c = HtmlAsciiCaseInsensitive;
        assert_eq!(c.compare("ABC", "abc"), Ordering::Equal);
        assert!(c.equals("Hello", "hELLO"));
        assert_ne!(c.compare("Ä", "ä"), Ordering::Equal);
        assert_eq!(c.find("xxHeLLo", "ll"), Some(Some((4, 6))));
        assert_eq!(c.starts_with("Hello", "HE"), Some(true));
        assert_eq!(c.ends_with("Hello", "LO"), Some(true));
        assert_eq!(c.sort_key("AbC"), Some(b"abc".to_vec()));
    }

    /// §13.1.3: "if lower-first is requested, then a sorted sequence might be
    /// 'MacAndrew, macintosh, macIntosh, Macintosh, MacIntosh, macintoshes,
    /// Macintoshes, McIntosh'. If upper-first is requested, the same sequence
    /// would sort as 'MacAndrew, MacIntosh, Macintosh, macIntosh, macintosh,
    /// MacIntoshes, macintoshes, McIntosh'".
    #[test]
    fn case_order_reproduces_the_spec_examples() {
        let words = [
            "MacIntosh",
            "MacAndrew",
            "macIntosh",
            "macintoshes",
            "Macintoshes",
            "macintosh",
            "McIntosh",
            "Macintosh",
        ];
        let lower = CaseOrderCollation {
            case_order: CaseOrder::LowerFirst,
        };
        assert_eq!(
            sorted(&words, &lower),
            [
                "MacAndrew",
                "macintosh",
                "macIntosh",
                "Macintosh",
                "MacIntosh",
                "macintoshes",
                "Macintoshes",
                "McIntosh"
            ]
        );
        let upper = CaseOrderCollation {
            case_order: CaseOrder::UpperFirst,
        };
        assert_eq!(
            sorted(&words, &upper),
            [
                "MacAndrew",
                "MacIntosh",
                "Macintosh",
                "macIntosh",
                "macintosh",
                "Macintoshes",
                "macintoshes",
                "McIntosh"
            ]
        );
        // "if lang="en", then A a B b are sorted with case-order="upper-first"
        // and a A b B are sorted with case-order="lower-first"".
        assert_eq!(sorted(&["b", "B", "a", "A"], &upper), ["A", "a", "B", "b"]);
        assert_eq!(sorted(&["B", "b", "A", "a"], &lower), ["a", "A", "b", "B"]);
    }

    #[test]
    fn the_registry_never_replaces_the_codepoint_collation() {
        let mut registry = CollationRegistry::new();
        assert!(registry.recognizes(CODEPOINT_COLLATION_URI));
        assert!(registry.recognizes(HTML_ASCII_CASE_INSENSITIVE_URI));
        assert!(!registry.recognizes("urn:host"));
        assert!(!registry.register(CODEPOINT_COLLATION_URI, Rc::new(HtmlAsciiCaseInsensitive)));
        assert!(registry.resolve(CODEPOINT_COLLATION_URI).is_none());
        assert!(registry.register("urn:host", Rc::new(HtmlAsciiCaseInsensitive)));
        assert!(registry.recognizes("urn:host"));
    }
}
