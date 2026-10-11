//! Regression tests for the independent review of M2k (`XSLT2_M2K_REVIEW.md`,
//! findings F1–F9), fixed in `XSLT2_M2K_REVIEW_FIXES.md`.
//!
//! * F1 — `xsl:key` and `group-by` under a collation without sort keys keep an
//!   index ordered by `compare` instead of one bucket compared pairwise. The
//!   counting tests use a host collation, so they run in the default build
//!   too; the UCA ones need the `icu-collation` feature.
//! * F2 — the substring functions under a UCA collation (F&O 2.0 §7.5): the
//!   leftmost minimal match of a collation-equal substring, at character
//!   boundaries, instead of FOCH0004.
//! * F3 `alternate=blanked`, F5 the `&` separator, F6 `version`, F7 the Unicode
//!   extension keywords of `lang`, F9 one collator per canonical tailoring.

use std::cell::Cell;
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::collation::Collation;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Runs a whole stylesheet module over `source` with `functions`.
fn run_with(functions: &XsltFunctions, sheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///m2k/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///m2k/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///m2k/in.xml", source)?;
    compiled.serialize_primary(&outcome)
}

/// Wraps template-level declarations in a `version="2.0"` stylesheet.
fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xs="http://www.w3.org/2001/XMLSchema" exclude-result-prefixes="xs">{body}</xsl:stylesheet>"#
    )
}

/// `<r><w>…</w>…</r>` with `n` distinct words.
fn words(n: usize, word: impl Fn(usize) -> String) -> String {
    let mut source = String::from("<r>");
    for i in 0..n {
        source.push_str(&format!("<w>{}</w>", word(i)));
    }
    source.push_str("</r>");
    source
}

// ═══ F1 — an ordered index for strings without sort keys ═══════════════════

/// ASCII case-insensitive, **without** a sort key, counting its comparisons.
#[derive(Default)]
struct CountingCaseless {
    compares: Cell<usize>,
}

impl Collation for CountingCaseless {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        self.compares.set(self.compares.get() + 1);
        a.bytes()
            .map(|c| c.to_ascii_lowercase())
            .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
    }
}

const COUNTING: &str = "urn:m2k:counting-caseless";

fn counting() -> (XsltFunctions, Rc<CountingCaseless>) {
    let collation = Rc::new(CountingCaseless::default());
    let mut functions = XsltFunctions::new();
    assert!(functions.register_collation(COUNTING, Rc::clone(&collation) as Rc<dyn Collation>));
    (functions, collation)
}

/// Muenchian grouping with `key()` over 2,000 distinct strings under a
/// collation without sort keys: before F1 every lookup compared the sought
/// value with every string of the index (about 2,000 × 2,000 comparisons);
/// with the ordered index each lookup and each insertion is a binary search.
#[test]
fn key_lookups_under_a_collation_without_sort_keys_are_logarithmic() {
    let (functions, collation) = counting();
    let n = 2_000;
    let result = run_with(
        &functions,
        &sheet(&format!(
            r#"<xsl:key name="k" match="w" use="." collation="{COUNTING}"/>
               <xsl:template match="/"><out><xsl:value-of select="count(r/w[generate-id() = generate-id(key('k', .)[1])])"/></out></xsl:template>"#
        )),
        &words(n, |i| format!("Word{i:05}")),
    )
    .unwrap();
    assert_eq!(result, format!("<out>{n}</out>"));
    let compares = collation.compares.get();
    eprintln!("key(): {compares} comparisons for {n} strings");
    assert!(
        compares < 64 * n,
        "{compares} comparisons for {n} lookups: not O(n log n)"
    );
}

/// The same for `group-by`: finding a key's group is a binary search over the
/// groups so far.
#[test]
fn group_by_under_a_collation_without_sort_keys_is_logarithmic() {
    let (functions, collation) = counting();
    let n = 2_000;
    let result = run_with(
        &functions,
        &sheet(&format!(
            r#"<xsl:template match="/"><out><xsl:for-each-group select="r/w" group-by="." collation="{COUNTING}"><xsl:if test="position() = last()"><xsl:value-of select="last()"/></xsl:if></xsl:for-each-group></out></xsl:template>"#
        )),
        &words(n, |i| format!("Word{i:05}")),
    )
    .unwrap();
    assert_eq!(result, format!("<out>{n}</out>"));
    let compares = collation.compares.get();
    eprintln!("group-by: {compares} comparisons for {n} keys");
    assert!(
        compares < 64 * n,
        "{compares} comparisons for {n} keys: not O(n log n)"
    );
}

/// The ordered index keeps `key()`'s typed-value rules (§16.3.2: equal "under
/// the rules appropriate to the XPath eq operator … No error is reported if
/// two values are encountered that are not comparable; they are regarded … as
/// being not equal"): untyped atomic and `xs:anyURI` values compare as strings
/// under the collation, a number never equals a string, and one key may mix
/// strings (ordered index) with numbers (hashed).
#[test]
fn the_ordered_key_index_keeps_the_typed_value_rules() {
    let (functions, _) = counting();
    let source =
        r#"<r><w n="1">Apple</w><w n="2">apple</w><w n="3">Pear</w><w n="1">APPLE</w></r>"#;
    let result = run_with(
        &functions,
        &sheet(&format!(
            r#"<xsl:key name="k" match="w" use="." collation="{COUNTING}"/>
               <xsl:key name="mixed" match="w" use="(., xs:integer(@n))" collation="{COUNTING}"/>
               <xsl:template match="/">
                 <out>
                   <string><xsl:value-of select="key('k', 'aPPle')/@n" separator=","/></string>
                   <untyped><xsl:value-of select="key('k', r/w[3])/@n" separator=","/></untyped>
                   <uri><xsl:value-of select="key('k', xs:anyURI('PEAR'))/@n"/></uri>
                   <number><xsl:value-of select="count(key('k', 1))"/></number>
                   <several><xsl:value-of select="key('k', ('pear', 'none', 'APPLE'))/@n" separator=","/></several>
                   <mixed-number><xsl:value-of select="key('mixed', 1)" separator=","/></mixed-number>
                   <mixed-string><xsl:value-of select="key('mixed', 'pear')"/></mixed-string>
                   <mixed-text><xsl:value-of select="count(key('mixed', '1'))"/></mixed-text>
                 </out>
               </xsl:template>"#
        )),
        source,
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><string>1,2,1</string><untyped>3</untyped><uri>3</uri><number>0</number>\
         <several>1,2,3,1</several><mixed-number>Apple,APPLE</mixed-number>\
         <mixed-string>Pear</mixed-string><mixed-text>0</mixed-text></out>"
    );
}

/// `group-by` with the ordered index: strings equal under the collation share
/// a group whose key is the initial item's (§14.3), untyped atomic values are
/// cast to `xs:string`, and a number in the same population groups apart.
#[test]
fn the_ordered_grouping_index_keeps_the_typed_value_rules() {
    let (functions, _) = counting();
    let source = r#"<r><w>Apple</w><w>pear</w><w>APPLE</w><w>apple</w><w>PEAR</w></r>"#;
    let result = run_with(
        &functions,
        &sheet(&format!(
            r#"<xsl:template match="/"><out><xsl:for-each-group select="(r/w, 1, 1.0, xs:anyURI('Pear'), 'one')" group-by="." collation="{COUNTING}"><g n="{{count(current-group())}}"><xsl:value-of select="current-grouping-key()"/></g></xsl:for-each-group></out></xsl:template>"#
        )),
        source,
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><g n=\"3\">Apple</g><g n=\"3\">pear</g><g n=\"2\">1</g><g n=\"1\">one</g></out>"
    );
}

#[cfg(feature = "icu-collation")]
mod uca {
    use std::time::{Duration, Instant};

    use xslt2::collation::CollationResolver;

    use super::*;

    const UCA: &str = "http://www.w3.org/2013/collation/UCA";

    fn run(sheet: &str, source: &str) -> Result<String, XsltError> {
        run_with(&XsltFunctions::new(), sheet, source)
    }

    /// The value of an XPath expression, as `xsl:value-of` writes it.
    fn xpath(expr: &str) -> String {
        let result = run(
            &super::sheet(&format!(
                r#"<xsl:template match="/"><out><xsl:value-of select="{expr}"/></out></xsl:template>"#
            )),
            "<doc/>",
        )
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
        result
            .strip_prefix("<out>")
            .and_then(|s| s.strip_suffix("</out>"))
            .map(str::to_string)
            .unwrap_or_default()
    }

    fn error_code(expr: &str) -> String {
        match run(
            &super::sheet(&format!(
                r#"<xsl:template match="/"><out><xsl:value-of select="{expr}"/></out></xsl:template>"#
            )),
            "<doc/>",
        ) {
            Ok(text) => panic!("{expr}: expected an error, got {text}"),
            Err(e) => e.code,
        }
    }

    fn cmp(a: &str, b: &str, query: &str) -> i32 {
        xpath(&format!("compare('{a}', '{b}', '{UCA}{query}')"))
            .parse()
            .unwrap_or_else(|e| panic!("compare('{a}', '{b}', {query}): {e}"))
    }

    // ─── F1 ───────────────────────────────────────────────────────────────

    /// The review's measurement (62.7 s for `key()`, 32.5 s for `group-by` at
    /// 20,000 strings in a release build) at 5,000 strings in whatever build
    /// runs the tests: quadratic took minutes in a debug build, the ordered
    /// index takes a second or two. The bound is generous on purpose.
    #[test]
    fn key_and_group_by_under_uca_are_not_quadratic() {
        let n = 5_000;
        let source = words(n, |i| format!("W{i:05}é"));
        let secondary = format!("{UCA}?strength=secondary");
        let started = Instant::now();
        let result = run(
            &super::sheet(&format!(
                r#"<xsl:key name="k" match="w" use="." collation="{secondary}"/>
                   <xsl:template match="/"><out><xsl:value-of select="count(r/w[generate-id() = generate-id(key('k', .)[1])])"/>,<xsl:for-each-group select="r/w" group-by="." collation="{secondary}"><xsl:if test="position() = last()"><xsl:value-of select="last()"/></xsl:if></xsl:for-each-group></out></xsl:template>"#
            )),
            &source,
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result, format!("<out>{n},{n}</out>"));
        assert!(
            elapsed < Duration::from_secs(20),
            "key() and group-by over {n} strings took {elapsed:?}"
        );
    }

    /// Strings equal under the collation land together — "a"/"A" from
    /// secondary strength down, accents from primary — and nothing else does.
    #[test]
    fn the_ordered_indexes_group_what_the_collation_equates() {
        let source = "<r><w>a</w><w>A</w><w>á</w><w>Á</w><w>b</w><w>ab</w></r>";
        for (strength, key_hits, groups) in [
            ("primary", "a,A,á,Á", "4:a 1:b 1:ab"),
            ("secondary", "a,A", "2:a 2:á 1:b 1:ab"),
            ("tertiary", "a", "1:a 1:A 1:á 1:Á 1:b 1:ab"),
        ] {
            let collation = format!("{UCA}?strength={strength}");
            let result = run(
                &super::sheet(&format!(
                    r#"<xsl:key name="k" match="w" use="." collation="{collation}"/>
                       <xsl:template match="/"><out><k><xsl:value-of select="key('k', 'a')" separator=","/></k><g><xsl:for-each-group select="r/w" group-by="." collation="{collation}"><xsl:value-of select="concat(count(current-group()), ':', current-grouping-key())"/><xsl:if test="position() != last()"><xsl:text> </xsl:text></xsl:if></xsl:for-each-group></g></out></xsl:template>"#
                )),
                source,
            )
            .unwrap();
            assert_eq!(
                result,
                format!("<out><k>{key_hits}</k><g>{groups}</g></out>"),
                "{strength}"
            );
        }
    }

    // ─── F2 — substring matching ──────────────────────────────────────────

    /// F&O 2.0 §7.5's examples use "CollationA … a collation in which both "-"
    /// and "*" are ignorable collation units". `alternate=shifted` at the
    /// default tertiary strength is one: both are punctuation, variable at
    /// `maxVariable=punct`, and shifted to the quaternary level.
    const COLLATION_A: &str = "http://www.w3.org/2013/collation/UCA?alternate=shifted";

    /// Every example of F&O 2.0 §7.5.1–§7.5.5, with CollationA where the
    /// example names it and the root collation otherwise.
    #[test]
    fn the_f_and_o_examples_hold() {
        let a = COLLATION_A;
        let root = UCA;
        for (call, expected) in [
            // 7.5.1 fn:contains
            (format!("contains('tattoo', 't', '{root}')"), "true"),
            (format!("contains('tattoo', 'ttt', '{root}')"), "false"),
            (format!("contains('', (), '{root}')"), "true"),
            (format!("contains('abcdefghi', '-d-e-f-', '{a}')"), "true"),
            (
                format!("contains('a*b*c*d*e*f*g*h*i*', 'd-ef-', '{a}')"),
                "true",
            ),
            (
                format!("contains('abcd***e---f*--*ghi', 'def', '{a}')"),
                "true",
            ),
            (format!("contains((), '--***-*---', '{a}')"), "true"),
            // 7.5.2 fn:starts-with
            (format!("starts-with('tattoo', 'tat', '{root}')"), "true"),
            (format!("starts-with('tattoo', 'att', '{root}')"), "false"),
            (format!("starts-with((), (), '{root}')"), "true"),
            (
                format!("starts-with('abcdefghi', '-a-b-c-', '{a}')"),
                "true",
            ),
            (
                format!("starts-with('a*b*c*d*e*f*g*h*i*', 'a-bc-', '{a}')"),
                "true",
            ),
            (
                format!("starts-with('abcd***e---f*--*ghi', 'abcdef', '{a}')"),
                "true",
            ),
            (format!("starts-with((), '--***-*---', '{a}')"), "true"),
            (format!("starts-with('-abcdefghi', '-abc', '{a}')"), "true"),
            // 7.5.3 fn:ends-with
            (format!("ends-with('tattoo', 'tattoo', '{root}')"), "true"),
            (format!("ends-with('tattoo', 'atto', '{root}')"), "false"),
            (format!("ends-with((), (), '{root}')"), "true"),
            (format!("ends-with('abcdefghi', '-g-h-i-', '{a}')"), "true"),
            (
                format!("ends-with('abcd***e---f*--*ghi', 'defghi', '{a}')"),
                "true",
            ),
            (format!("ends-with((), '--***-*---', '{a}')"), "true"),
            (format!("ends-with('abcdefghi', 'ghi-', '{a}')"), "true"),
            // 7.5.4 fn:substring-before
            (
                format!("substring-before('tattoo', 'attoo', '{root}')"),
                "t",
            ),
            (format!("substring-before('tattoo', 'tatto', '{root}')"), ""),
            (format!("substring-before((), (), '{root}')"), ""),
            (
                format!("substring-before('abcdefghi', '--d-e-', '{a}')"),
                "abc",
            ),
            (
                format!("substring-before('abc--d-e-fghi', '--d-e-', '{a}')"),
                "abc--",
            ),
            (
                format!("substring-before('a*b*c*d*e*f*g*h*i*', '***cde', '{a}')"),
                "a*b*",
            ),
            (
                format!("substring-before('Eureka!', '--***-*---', '{a}')"),
                "",
            ),
            // 7.5.5 fn:substring-after
            (format!("substring-after('tattoo', 'tat', '{root}')"), "too"),
            (format!("substring-after('tattoo', 'tattoo', '{root}')"), ""),
            (format!("substring-after((), (), '{root}')"), ""),
            (
                format!("substring-after('abcdefghi', '--d-e-', '{a}')"),
                "fghi",
            ),
            // The specification prints "-fghi " (with a stray space).
            (
                format!("substring-after('abc--d-e-fghi', '--d-e-', '{a}')"),
                "-fghi",
            ),
            (
                format!("substring-after('a*b*c*d*e*f*g*h*i*', '***cde***', '{a}')"),
                "*f*g*h*i*",
            ),
            (
                format!("substring-after('Eureka!', '--***-*---', '{a}')"),
                "Eureka!",
            ),
        ] {
            assert_eq!(xpath(&call), expected, "{call}");
        }
    }

    /// "If the value of $arg1 or $arg2 is the empty sequence, or contains only
    /// ignorable collation units, it is interpreted as the zero-length string.
    /// If the value of $arg2 is the zero-length string, then the function
    /// returns true. If the value of $arg1 is the zero-length string, the
    /// function returns false." — in that order, and before any search.
    #[test]
    fn the_zero_length_rules_come_first() {
        let a = COLLATION_A;
        for (call, expected) in [
            (format!("contains('abc', '', '{UCA}')"), "true"),
            (format!("contains('', '', '{UCA}')"), "true"),
            (format!("contains('', 'a', '{UCA}')"), "false"),
            (format!("contains('---', '-', '{a}')"), "true"),
            (format!("contains('---', 'a', '{a}')"), "false"),
            (format!("contains('', '*', '{a}')"), "true"),
            (format!("starts-with('', '-', '{a}')"), "true"),
            (format!("starts-with('-*-', 'a', '{a}')"), "false"),
            (format!("ends-with('abc', '*', '{a}')"), "true"),
            (format!("substring-before('abc', '-', '{a}')"), ""),
            (format!("substring-after('abc', '-', '{a}')"), "abc"),
            (format!("substring-after('', '', '{UCA}')"), ""),
            (format!("substring-before('-', 'a', '{a}')"), ""),
        ] {
            assert_eq!(xpath(&call), expected, "{call}");
        }
    }

    /// The strength decides what matches: primary ignores accents and case,
    /// secondary only case, tertiary neither.
    #[test]
    fn case_and_accents_match_by_strength() {
        for (strength, needle, expected) in [
            ("primary", "ap", "true"),
            ("secondary", "ap", "false"),
            ("secondary", "äp", "true"),
            ("tertiary", "äp", "false"),
            ("tertiary", "Äp", "true"),
        ] {
            let call = format!("contains('Äpfel', '{needle}', '{UCA}?strength={strength}')");
            assert_eq!(xpath(&call), expected, "{call}");
        }
        let primary = format!("{UCA}?strength=primary");
        assert_eq!(
            xpath(&format!("substring-after('XÄPFELy', 'apfel', '{primary}')")),
            "y"
        );
        assert_eq!(
            xpath(&format!("starts-with('Ärger', 'ar', '{primary}')")),
            "true"
        );
        assert_eq!(
            xpath(&format!("ends-with('CAFÉ', 'fe', '{primary}')")),
            "true"
        );
    }

    /// An expansion matches as a whole character: `ß` is `ss` at primary
    /// strength, so `ss` finds `ß` and `ß` finds `ss`; a match never splits
    /// `ß` itself (`uca.rs`, "What character boundaries cannot do").
    #[test]
    fn expansions_match_whole_characters() {
        let primary = format!("{UCA}?strength=primary");
        for (call, expected) in [
            (format!("contains('Straße', 'ss', '{primary}')"), "true"),
            (
                format!("substring-before('Straße', 'ss', '{primary}')"),
                "Stra",
            ),
            (format!("substring-after('Straße', 'ss', '{primary}')"), "e"),
            (format!("contains('Strasse', 'ß', '{primary}')"), "true"),
            (format!("substring-after('Strasse', 'ß', '{primary}')"), "e"),
            (format!("starts-with('ßx', 'ss', '{primary}')"), "true"),
            (format!("ends-with('Maß', 'ss', '{primary}')"), "true"),
            (format!("contains('ß', 's', '{primary}')"), "false"),
            // Tertiary strength tells them apart.
            (format!("contains('Straße', 'ss', '{UCA}')"), "false"),
            // A compatibility ligature equals its letters below tertiary.
            (
                format!("contains('ﬁle', 'fi', '{UCA}?strength=secondary')"),
                "true",
            ),
        ] {
            assert_eq!(xpath(&call), expected, "{call}");
        }
    }

    /// A contraction matches as a whole: Danish `aa` is a variant of `å`
    /// (tertiary difference), so at secondary strength `å` finds `Aa`. A match
    /// may begin inside a contraction, though (Slovak `ch`), and a combining
    /// sequence is two collation units, so `e` is found in `e` + U+0301.
    #[test]
    fn contractions_and_combining_sequences() {
        let danish = format!("{UCA}?lang=da;strength=secondary");
        for (call, expected) in [
            (format!("contains('Aarhus', 'å', '{danish}')"), "true"),
            (
                format!("substring-after('Aarhus', 'å', '{danish}')"),
                "rhus",
            ),
            (format!("starts-with('Aarhus', 'å', '{danish}')"), "true"),
            (format!("contains('xchy', 'ch', '{UCA}?lang=sk')"), "true"),
            (
                format!("substring-before('xchy', 'ch', '{UCA}?lang=sk')"),
                "x",
            ),
            // The documented approximation: `h` inside the contraction `ch`.
            (format!("contains('chata', 'h', '{UCA}?lang=sk')"), "true"),
            // Canonical equivalence: a precomposed needle finds the
            // decomposed sequence, and the other way round.
            (format!("contains('cafe\u{301}!', 'é', '{UCA}')"), "true"),
            (
                format!("substring-before('cafe\u{301}!', 'é', '{UCA}')"),
                "caf",
            ),
            (format!("contains('café', 'e\u{301}', '{UCA}')"), "true"),
            (format!("contains('café', 'e', '{UCA}')"), "false"),
            (format!("contains('cafe\u{301}', 'e', '{UCA}')"), "true"),
        ] {
            assert_eq!(xpath(&call), expected, "{call}");
        }
    }

    /// The review's F2(b): under a UCA default collation, a pattern predicate
    /// with a substring function raised FOCH0004, which the §5.5.4 recovery
    /// turned into a silent non-match. It matches now.
    #[test]
    fn a_pattern_with_a_substring_function_matches_under_a_uca_default() {
        let result = run(
            &format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" default-collation="{UCA}?lang=en">
                     <xsl:template match="/"><out><xsl:apply-templates select="r/w"/></out></xsl:template>
                     <xsl:template match="w[starts-with(., 'W0000')]"><hit><xsl:value-of select="."/></hit></xsl:template>
                     <xsl:template match="w"/>
                   </xsl:stylesheet>"#
            ),
            "<r><w>W00001</w><w>X</w><w>w00002</w></r>",
        )
        .unwrap();
        assert_eq!(result, "<out><hit>W00001</hit></out>");
        let result = run(
            &format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" default-collation="{UCA}?strength=secondary">
                     <xsl:template match="/"><out><xsl:value-of select="r/w[contains(., 'w0000')]" separator=","/></out></xsl:template>
                   </xsl:stylesheet>"#
            ),
            "<r><w>W00001</w><w>X</w><w>w00002</w></r>",
        )
        .unwrap();
        assert_eq!(result, "<out>W00001,w00002</out>");
    }

    // ─── F3 — alternate=blanked ───────────────────────────────────────────

    /// UTS #10: blanked "is equivalent to the Shifted option used with
    /// strength=tertiary" — the hyphen of `de-luge` is ignored, so `deluge`
    /// sorts after `delta` (non-ignorable puts the hyphen first: −1).
    #[test]
    fn blanked_ignores_variable_characters() {
        assert_eq!(cmp("de-luge", "delta", "?alternate=blanked"), 1);
        assert_eq!(cmp("de-luge", "delta", "?alternate=blanked;fallback=no"), 1);
        assert_eq!(cmp("de-luge", "delta", "?alternate=non-ignorable"), -1);
        // No quaternary level: `data-base` equals `database` even at
        // strength 4, where shifted tells them apart.
        assert_eq!(
            cmp("data-base", "database", "?alternate=blanked;strength=4"),
            0
        );
        assert_ne!(
            cmp("data-base", "database", "?alternate=shifted;strength=4"),
            0
        );
        // Identical strength cannot be honoured exactly.
        assert_eq!(
            error_code(&format!(
                "compare('a', 'b', '{UCA}?alternate=blanked;strength=identical;fallback=no')"
            )),
            "FOCH0002"
        );
    }

    // ─── F5 — separators ──────────────────────────────────────────────────

    /// F&O 3.1 §5.3.3: "a sequence of zero or more semicolon-separated
    /// parameters"; `&` as this processor's implementation-defined addition.
    #[test]
    fn semicolons_and_ampersands_separate_parameters() {
        for query in [
            "?lang=de;strength=primary;fallback=no",
            // `&amp;`: the URI is written inside an XML attribute.
            "?lang=de&amp;strength=primary&amp;fallback=no",
            "?fallback=no&amp;lang=de;strength=primary",
        ] {
            assert_eq!(cmp("A", "ä", query), 0, "{query}");
        }
        assert_eq!(
            error_code(&format!(
                "compare('a', 'b', '{UCA}?fallback=no&amp;version=99')"
            )),
            "FOCH0002"
        );
    }

    // ─── F6 — version ─────────────────────────────────────────────────────

    /// "version: The version number of the UCA to be used": the data is UCA
    /// 15.1.0; an earlier or equal version is honoured with it, a later one is
    /// refused with `fallback=no` and ignored otherwise.
    #[test]
    fn versions_up_to_15_1_are_honoured() {
        for version in ["15.1.0", "15.1", "6.2.0", "9"] {
            assert_eq!(
                cmp("a", "A", &format!("?version={version};fallback=no")),
                -1
            );
        }
        for version in ["15.1.1", "16.0", "nine"] {
            assert_eq!(
                error_code(&format!(
                    "compare('a', 'b', '{UCA}?version={version};fallback=no')"
                )),
                "FOCH0002",
                "{version}"
            );
            assert_eq!(cmp("a", "A", &format!("?version={version}")), -1);
        }
    }

    // ─── F7 — Unicode extensions in lang ──────────────────────────────────

    /// `-u-kf-` and `-u-kn-` (and the other collation keywords) are honoured;
    /// an explicit parameter wins over the same setting in `lang`, as in ICU
    /// and ECMA-402.
    #[test]
    fn lang_extension_keywords_are_honoured() {
        assert_eq!(cmp("A", "a", "?lang=en-u-kf-upper"), -1);
        assert_eq!(cmp("A", "a", "?lang=en-u-kf-upper;fallback=no"), -1);
        assert_eq!(cmp("A", "a", "?lang=en-u-kf-upper;caseFirst=lower"), 1);
        assert_eq!(cmp("A", "a", "?caseFirst=lower;lang=en-u-kf-upper"), 1);
        assert_eq!(cmp("A", "a", "?lang=da-u-kf-lower"), 1);
        assert_eq!(cmp("chap2", "chap12", "?lang=en-u-kn"), -1);
        assert_eq!(cmp("chap2", "chap12", "?lang=en-u-kn-true"), -1);
        assert_eq!(cmp("chap2", "chap12", "?lang=en-u-kn;numeric=no"), 1);
        assert_eq!(cmp("A", "a", "?lang=en-u-ks-level2"), 0);
        // A collation type the data has: German phone-book order (ä = ae).
        assert_eq!(
            cmp(
                "Ärger",
                "Aerger",
                "?lang=de-u-co-phonebk;strength=primary;fallback=no"
            ),
            0
        );
        assert_ne!(cmp("Ärger", "Aerger", "?lang=de;strength=primary"), 0);
        // Unsupported or invalid: refused with fallback=no, ignored otherwise.
        for lang in [
            "de-u-co-bogus",
            "en-u-kf-sideways",
            "en-u-kr-grek",
            "en-x-private",
        ] {
            assert_eq!(
                error_code(&format!(
                    "compare('a', 'b', '{UCA}?lang={lang};fallback=no')"
                )),
                "FOCH0002",
                "{lang}"
            );
            assert_eq!(cmp("a", "A", &format!("?lang={lang}")), -1, "{lang}");
        }
    }

    /// `xsl:sort lang` reads the keywords too, and `case-order` wins over
    /// `-u-kf-` as an explicit parameter does.
    #[test]
    fn sort_lang_extension_keywords() {
        let sort = |attrs: &str| {
            run(
                &super::sheet(&format!(
                    r#"<xsl:template match="/"><out><xsl:value-of separator=","><xsl:perform-sort select="('b', 'a', 'B', 'A', 'x10', 'x9')"><xsl:sort select="." {attrs}/></xsl:perform-sort></xsl:value-of></out></xsl:template>"#
                )),
                "<doc/>",
            )
            .unwrap()
        };
        assert_eq!(sort(r#"lang="en""#), "<out>a,A,b,B,x10,x9</out>");
        assert_eq!(
            sort(r#"lang="en-u-kf-upper-kn""#),
            "<out>A,a,B,b,x9,x10</out>"
        );
        assert_eq!(
            sort(r#"lang="en-u-kf-upper" case-order="lower-first""#),
            "<out>a,A,b,B,x10,x9</out>"
        );
    }

    // ─── F9 — one collator per canonical tailoring ────────────────────────

    #[test]
    fn regional_variants_without_their_own_tailoring_share_a_collator() {
        let functions = XsltFunctions::new();
        let registry = functions.collations();
        let root = registry.resolve(UCA).unwrap();
        for uri in [
            format!("{UCA}?lang=en"),
            format!("{UCA}?lang=en-US"),
            format!("{UCA}?lang=en-GB;fallback=no"),
            format!("{UCA}?lang=de-AT"),
        ] {
            assert!(Rc::ptr_eq(&root, &registry.resolve(&uri).unwrap()), "{uri}");
        }
        let sv = registry.resolve(&format!("{UCA}?lang=sv")).unwrap();
        assert!(!Rc::ptr_eq(&root, &sv));
        assert!(Rc::ptr_eq(
            &sv,
            &registry.resolve(&format!("{UCA}?lang=sv-FI")).unwrap()
        ));
    }
}

// ═══ F8 — the feature changes existing results ════════════════════════════

/// README / CLAUDE.md: "`('f', 'é', 'e')` with `lang="en"` sorts `e, f, é`
/// without the feature and `e, é, f` with it."
#[test]
fn the_feature_changes_sort_lang_results() {
    let result = run_with(
        &XsltFunctions::new(),
        &sheet(
            r#"<xsl:template match="/"><out><xsl:value-of separator=","><xsl:perform-sort select="('f', 'é', 'e')"><xsl:sort select="." lang="en"/></xsl:perform-sort></xsl:value-of></out></xsl:template>"#,
        ),
        "<doc/>",
    )
    .unwrap();
    let expected = if cfg!(feature = "icu-collation") {
        "<out>e,é,f</out>"
    } else {
        "<out>e,f,é</out>"
    };
    assert_eq!(result, expected);
}

// ═══ F4 — absent parameters take the language's defaults (documented) ═════

/// The defaults the README and `uca.rs` name: Thai is `alternate=shifted`
/// (spaces and punctuation ignored up to the tertiary level), Canadian French
/// compares accents backwards, Danish puts upper case first — and an explicit
/// parameter overrides each.
#[cfg(feature = "icu-collation")]
#[test]
fn absent_parameters_take_the_language_defaults() {
    let cmp = |a: &str, b: &str, query: &str| -> String {
        let result = run_with(
            &XsltFunctions::new(),
            &sheet(&format!(
                r#"<xsl:template match="/"><out><xsl:value-of select="compare('{a}', '{b}', 'http://www.w3.org/2013/collation/UCA{query}')"/></out></xsl:template>"#
            )),
            "<doc/>",
        )
        .unwrap();
        result[5..result.len() - 6].to_string()
    };
    assert_eq!(cmp("a b", "ab", "?lang=th"), "0");
    assert_eq!(cmp("data-base", "database", "?lang=th"), "0");
    assert_eq!(cmp("a b", "ab", "?lang=th;alternate=non-ignorable"), "-1");
    assert_eq!(cmp("côte", "coté", "?lang=fr-CA"), "-1");
    assert_eq!(cmp("côte", "coté", "?lang=fr-CA;backwards=no"), "1");
    assert_eq!(cmp("côte", "coté", "?lang=fr"), "1");
    assert_eq!(cmp("A", "a", "?lang=da"), "-1");
    assert_eq!(cmp("A", "a", "?lang=da;caseFirst=lower"), "1");
}
