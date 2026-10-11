//! Regression tests for the second review of M2k (`XSLT2_M2K_REVIEW2.md`,
//! findings R1 and R2), fixed in `XSLT2_M2K_REVIEW2_FIXES.md`.
//!
//! * R1 — `xsl:sort case-order` with a `lang` whose Unicode extension asks
//!   for primary or secondary strength (`-u-ks-level1`, `-u-ks-level2`): the
//!   UCA collation keeps a case level, so upper and lower case are ordered as
//!   asked while accents (level 1) stay ignored. Without the feature
//!   `CaseOrderCollation` already gives the same orders, so the case-order
//!   tests run in both builds.
//! * R2 — the substring functions under a UCA collation are no longer cubic
//!   (`collation/uca/search.rs`): the review's case, 2,000 × `a` searched for
//!   1,000 × `a` + `b`, took 21.5 s in a release build.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn run(sheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///m2k/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///m2k/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///m2k/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// The items of `select` sorted by one `xsl:sort` with `attributes`, joined.
fn sorted(select: &str, attributes: &str) -> String {
    let result = run(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:output method="text"/>
                 <xsl:template match="/"><xsl:for-each select="{select}"><xsl:sort {attributes}/><xsl:value-of select="."/></xsl:for-each></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
    )
    .unwrap_or_else(|e| panic!("{select} {attributes}: {e}"));
    result
}

// ═══ R1 — case-order with a weak strength in lang ═════════════════════════

/// The review's reproduction: `lang="en-u-ks-level2" case-order="upper-first"`
/// sorted `('a', 'A', 'b', 'B')` as `aAbB` — secondary strength ignores case,
/// and `caseFirst` alone cannot order what the collation calls equal. §13.1.3:
/// "The case-order attribute indicates whether the desired collation should
/// sort upper-case letters before lower-case or vice versa."
#[test]
fn case_order_holds_with_a_weak_lang_strength() {
    for level in ["level1", "level2"] {
        let lang = format!(r#"lang="en-u-ks-{level}""#);
        assert_eq!(
            sorted(
                "('a', 'A', 'b', 'B')",
                &format!(r#"{lang} case-order="upper-first""#)
            ),
            "AaBb",
            "{level} upper-first"
        );
        // The opposite input order, so that a stable sort of equal items
        // could not pass for the right answer.
        assert_eq!(
            sorted(
                "('A', 'a', 'B', 'b')",
                &format!(r#"{lang} case-order="lower-first""#)
            ),
            "aAbB",
            "{level} lower-first"
        );
        assert_eq!(
            sorted(
                "('a', 'A', 'b', 'B')",
                &format!(r#"{lang} case-order="upper-first""#)
            ),
            sorted(
                "('A', 'a', 'B', 'b')",
                &format!(r#"{lang} case-order="upper-first""#)
            ),
            "{level}: the order does not depend on the input order"
        );
    }
    // Tertiary strength, with and without `lang`: case is a difference there
    // already.
    for attributes in [r#"lang="en""#, r#"lang="en-u-ks-level3""#, ""] {
        assert_eq!(
            sorted(
                "('a', 'A', 'b', 'B')",
                &format!(r#"{attributes} case-order="upper-first""#)
            ),
            "AaBb",
            "{attributes}"
        );
        assert_eq!(
            sorted(
                "('A', 'a', 'B', 'b')",
                &format!(r#"{attributes} case-order="lower-first""#)
            ),
            "aAbB",
            "{attributes}"
        );
    }
}

/// The case level adds case and nothing else: at level 1 accents are still
/// ignored (`á` and `a` keep their input order), at level 2 they still count
/// before case. Raising the strength to tertiary instead would have sorted
/// `á` after `a` at level 1.
#[cfg(feature = "icu-collation")]
#[test]
fn the_case_level_keeps_the_language_s_strength() {
    assert_eq!(
        sorted(
            "('á', 'a', 'A')",
            r#"lang="en-u-ks-level1" case-order="upper-first""#
        ),
        "Aáa"
    );
    assert_eq!(
        sorted(
            "('A', 'á', 'a')",
            r#"lang="en-u-ks-level1" case-order="lower-first""#
        ),
        "áaA"
    );
    assert_eq!(
        sorted(
            "('á', 'a', 'Á', 'A')",
            r#"lang="en-u-ks-level2" case-order="upper-first""#
        ),
        "AaÁá"
    );
    assert_eq!(
        sorted(
            "('Á', 'A', 'á', 'a')",
            r#"lang="en-u-ks-level2" case-order="lower-first""#
        ),
        "aAáÁ"
    );
    // An explicit case-order wins over `-u-kc-false` as it wins over `-u-kf-`.
    assert_eq!(
        sorted(
            "('a', 'A', 'b', 'B')",
            r#"lang="en-u-ks-level2-kc-false" case-order="upper-first""#
        ),
        "AaBb"
    );
    // Without case-order a weak `lang` strength still ignores case: equal
    // items keep their input order.
    assert_eq!(
        sorted("('a', 'A', 'B', 'b')", r#"lang="en-u-ks-level2""#),
        "aABb"
    );
}

/// "The lang and case-order attributes are ignored if a collation attribute
/// is present" (§13.1.3): a secondary-strength collation URI keeps `a` and `A`
/// equal whatever `case-order` says.
#[cfg(feature = "icu-collation")]
#[test]
fn case_order_is_ignored_with_a_collation_attribute() {
    for collation in [
        "http://www.w3.org/2013/collation/UCA?strength=secondary",
        "http://www.w3.org/2013/collation/UCA?lang=en-u-ks-level2",
    ] {
        for (input, expected) in [
            ("('a', 'A', 'b', 'B')", "aAbB"),
            ("('A', 'a', 'B', 'b')", "AaBb"),
        ] {
            assert_eq!(
                sorted(
                    input,
                    &format!(r#"collation="{collation}" case-order="upper-first" lang="de""#)
                ),
                expected,
                "{collation}"
            );
        }
    }
}

// ═══ R2 — the substring functions are not cubic ═══════════════════════════

#[cfg(feature = "icu-collation")]
mod substrings {
    use std::time::{Duration, Instant};

    use xslt2::collation::CollationResolver;
    use xslt2::XsltFunctions;

    const UCA: &str = "http://www.w3.org/2013/collation/UCA";

    /// What `find`, `starts_with` and `ends_with` answered, and the time they
    /// took together.
    type Answers = (
        Option<Option<(usize, usize)>>,
        Option<bool>,
        Option<bool>,
        Duration,
    );

    /// `find`, `starts_with` and `ends_with` of `haystack` for `needle`.
    fn search(query: &str, haystack: &str, needle: &str) -> Answers {
        let functions = XsltFunctions::new();
        let collation = functions
            .collations()
            .resolve(&format!("{UCA}{query}"))
            .unwrap();
        let started = Instant::now();
        let found = collation.find(haystack, needle);
        let starts = collation.starts_with(haystack, needle);
        let ends = collation.ends_with(haystack, needle);
        (found, starts, ends, started.elapsed())
    }

    /// The review's probe, and the same shape with the characters each
    /// path of the search serves: the primary filter (plain letters, accents
    /// precomposed or not), the count filter (`ß`: two primary weights; a
    /// Hangul syllable: two jamo). Before the fix 2,000 characters took
    /// 21.5 s in a release build; now each takes milliseconds there. The
    /// bound is generous so that a debug build passes it too.
    #[test]
    fn long_common_prefixes_are_not_cubic() {
        for (query, fill, last) in [
            ("?strength=secondary", "a", "b"),
            ("?strength=secondary", "á", "b"),
            ("?strength=secondary", "e\u{301}", "b"),
            ("?strength=primary", "ß", "b"),
            ("", "가", "나"),
            ("?lang=sv", "å", "b"),
        ] {
            let haystack = fill.repeat(2_000);
            let needle = fill.repeat(1_000) + last;
            let (found, starts, ends, elapsed) = search(query, &haystack, &needle);
            assert_eq!(
                (found, starts, ends),
                (Some(None), Some(false), Some(false)),
                "{query} {fill:?}"
            );
            assert!(
                elapsed < Duration::from_secs(10),
                "{query} {fill:?}: {elapsed:?}"
            );
        }
    }

    /// The same sizes with a match: the needle is found where it is, with the
    /// minimal match's exact bounds.
    #[test]
    fn long_matches_are_found_exactly() {
        let haystack = "a".repeat(1_500) + "b" + &"a".repeat(499);
        let needle = "A".repeat(1_000) + "B";
        let (found, starts, ends, elapsed) = search("?strength=secondary", &haystack, &needle);
        assert_eq!(found, Some(Some((500, 1_501))));
        assert_eq!((starts, ends), (Some(false), Some(false)));
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        // `ß` against `ss`: the count filter compares the right ends only.
        let haystack = "ß".repeat(2_000);
        let needle = "ss".repeat(1_000);
        let (found, starts, ends, elapsed) = search("?strength=primary", &haystack, &needle);
        assert_eq!(found, Some(Some((0, 2_000))));
        assert_eq!((starts, ends), (Some(true), Some(true)));
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    /// Ordinary text of 20,000 characters, in languages whose characters take
    /// each path: absent, present and trailing needles all answer at once.
    #[test]
    fn long_texts_take_milliseconds() {
        for (query, words) in [
            (
                "?strength=secondary",
                &["the", "quick", "brown", "fox", "jumps"][..],
            ),
            ("?lang=fr", &["élève", "été", "café", "crème", "déjà"][..]),
            ("?lang=de", &["Straße", "groß", "Fuß", "weiß", "Maß"][..]),
            (
                "?lang=ko",
                &["한국어", "사랑", "안녕하세요", "감사합니다"][..],
            ),
            (
                "?lang=ja",
                &["コーヒー", "カード", "ケーキ", "スーパー"][..],
            ),
            ("?lang=da", &["Aarhus", "haar", "naar", "paa", "saa"][..]),
            ("?numeric=yes", &["item12", "v2", "page300", "no7"][..]),
        ] {
            let mut text = String::new();
            let mut k = 0;
            while text.chars().count() < 20_000 {
                text.push_str(words[(k * 7 + k / 3) % words.len()]);
                text.push(' ');
                k += 1;
            }
            let tail: String = text.chars().skip(text.chars().count() - 200).collect();
            for (needle, expected) in [("qqqzzz", false), (tail.as_str(), true)] {
                let (found, _, ends, elapsed) = search(query, &text, needle);
                assert_eq!(found.unwrap().is_some(), expected, "{query} {needle:?}");
                assert_eq!(ends, Some(expected), "{query} {needle:?}");
                assert!(elapsed < Duration::from_secs(10), "{query}: {elapsed:?}");
            }
        }
    }
}
