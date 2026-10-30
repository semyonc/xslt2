//! `xsl:sort case-order` with a strength in `lang`, and the substring functions under a UCA
//! collation.
//!
//! * `xsl:sort case-order` with a `lang` whose Unicode extension asks
//!   for primary or secondary strength (`-u-ks-level1`, `-u-ks-level2`): the
//!   UCA collation keeps a case level, so upper and lower case are ordered as
//!   asked while accents (level 1) stay ignored. Without the feature
//!   `CaseOrderCollation` gives the same orders, so the case-order
//!   tests run in both builds.
//! * The substring functions under a UCA collation are not cubic
//!   (`collation/uca/search.rs`): 2,000 × `a` searched for 1,000 × `a` + `b`
//!   took 21.5 s in a release build with a cubic search.

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
    mem.insert("file:///collation/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///collation/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///collation/in.xml", source)?;
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

// ═══ case-order with a weak strength in lang ══════════════════════════════

/// A reproduction: `lang="en-u-ks-level2" case-order="upper-first"`
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

// ═══ the substring functions are not cubic ════════════════════════════════

#[cfg(feature = "icu-collation")]
mod substrings {
    use std::time::Duration;

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
        let mut answers = (None, None, None);
        let elapsed = crate::common::scaling::timed(|| {
            answers = (
                collation.find(haystack, needle),
                collation.starts_with(haystack, needle),
                collation.ends_with(haystack, needle),
            );
        });
        (answers.0, answers.1, answers.2, elapsed)
    }

    /// A haystack of 2 n `fill`s searched for n `fill`s and `last`, and the
    /// same shape with the characters each path of the search serves: the
    /// primary filter (plain letters, accents precomposed or not), the count
    /// filter (`ß`: two primary weights; a Hangul syllable: two jamo). None is
    /// cubic, from n = 125 to n = 1,000 (`common::scaling`); the cubic search
    /// took 21.5 s at n = 1,000 in a release build. Every one but the Hangul
    /// syllable is linear; the Hangul syllable is quadratic (about 0.9 s at
    /// n = 1,000 in a debug build).
    #[test]
    fn long_common_prefixes_are_not_cubic() {
        for (query, fill, last, quadratic) in [
            ("?strength=secondary", "a", "b", false),
            ("?strength=secondary", "á", "b", false),
            ("?strength=secondary", "e\u{301}", "b", false),
            ("?strength=primary", "ß", "b", false),
            ("", "가", "나", true),
            ("?lang=sv", "å", "b", false),
        ] {
            let what = format!("{query} {fill:?}");
            let check = if quadratic {
                crate::common::scaling::assert_quadratic
            } else {
                crate::common::scaling::assert_linear
            };
            check(&what, 125, 8, &mut |n| {
                let haystack = fill.repeat(2 * n);
                let needle = fill.repeat(n) + last;
                let (found, starts, ends, elapsed) = search(query, &haystack, &needle);
                assert_eq!(
                    (found, starts, ends),
                    (Some(None), Some(false), Some(false)),
                    "{what}"
                );
                elapsed
            });
        }
    }

    /// The same sizes with a match: the needle is found where it is, with the
    /// minimal match's exact bounds, and the search is linear in the length
    /// (`common::scaling`).
    #[test]
    fn long_matches_are_found_exactly() {
        crate::common::scaling::assert_linear("a match after a long prefix", 128, 8, |n| {
            let haystack = "a".repeat(3 * n / 2) + "b" + &"a".repeat(n / 2 - 1);
            let needle = "A".repeat(n) + "B";
            let (found, starts, ends, elapsed) = search("?strength=secondary", &haystack, &needle);
            assert_eq!(found, Some(Some((n / 2, 3 * n / 2 + 1))));
            assert_eq!((starts, ends), (Some(false), Some(false)));
            elapsed
        });
        // `ß` against `ss`: the count filter compares the right ends only.
        crate::common::scaling::assert_linear("ß against ss", 250, 8, |n| {
            let haystack = "ß".repeat(n);
            let needle = "ss".repeat(n / 2);
            let (found, starts, ends, elapsed) = search("?strength=primary", &haystack, &needle);
            assert_eq!(found, Some(Some((0, n))));
            assert_eq!((starts, ends), (Some(true), Some(true)));
            elapsed
        });
    }

    /// Ordinary text of 2,500 and 20,000 characters, in languages whose
    /// characters take each path: absent, present and trailing needles all
    /// answer in time linear in the text (`common::scaling`).
    #[test]
    fn long_texts_take_linear_time() {
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
            crate::common::scaling::assert_linear(query, 2_500, 8, |length| {
                let mut text = String::new();
                let mut k = 0;
                while text.chars().count() < length {
                    text.push_str(words[(k * 7 + k / 3) % words.len()]);
                    text.push(' ');
                    k += 1;
                }
                let tail: String = text.chars().skip(text.chars().count() - 200).collect();
                let mut elapsed = Duration::ZERO;
                for (needle, expected) in [("qqqzzz", false), (tail.as_str(), true)] {
                    let (found, _, ends, took) = search(query, &text, needle);
                    assert_eq!(found.unwrap().is_some(), expected, "{query} {needle:?}");
                    assert_eq!(ends, Some(expected), "{query} {needle:?}");
                    elapsed += took;
                }
                elapsed
            });
        }
    }
}
