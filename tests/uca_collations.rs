//! The UCA collations and locale-tailored `xsl:sort`, with the `icu-collation`
//! cargo feature.
//!
//! Every test quotes the rule it checks: F&O 3.1 §5.3.3 "The Unicode Collation
//! Algorithm" (read at w3.org) for the collation URIs and their parameters,
//! XSLT 2.0 for the consumers — §3.6.1 `[xsl:]default-collation`, §13.1.3
//! `xsl:sort`, §16.3.1 `xsl:key`, §14 `xsl:for-each-group`. The feature-off
//! behaviour is pinned by `tests/collations_without_icu.rs`.

#![cfg(feature = "icu-collation")]

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::collation::CollationResolver;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const UCA: &str = "http://www.w3.org/2013/collation/UCA";

/// Runs a whole stylesheet module over `source`.
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

/// Wraps template-level declarations in a `version="2.0"` stylesheet.
fn sheet(body: &str) -> String {
    format!(r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">{body}</xsl:stylesheet>"#)
}

/// Runs a `match="/"` template whose `out` element holds `content`.
fn eval(content: &str) -> Result<String, XsltError> {
    run(
        &sheet(&format!(
            r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
        )),
        "<doc/>",
    )
}

/// The value of an XPath expression, as `xsl:value-of` writes it.
fn xpath(expr: &str) -> String {
    let result = eval(&format!(r#"<xsl:value-of select="{expr}"/>"#))
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    result
        .strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .map(str::to_string)
        .unwrap_or_default()
}

/// `compare($a, $b, $uri)` under the UCA collation with `query`.
fn cmp(a: &str, b: &str, query: &str) -> i32 {
    xpath(&format!("compare('{a}', '{b}', '{UCA}{query}')"))
        .parse()
        .unwrap_or_else(|e| panic!("compare('{a}', '{b}', {query}): {e}"))
}

fn code(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => panic!("expected an error, got {text}"),
        Err(e) => e.code,
    }
}

fn out(text: &str) -> String {
    format!("<out>{text}</out>")
}

// ═══ The URI family and its parameters (F&O 3.1 §5.3.3) ═══════════════════

/// "This family of URIs use the scheme and path
/// http://www.w3.org/2013/collation/UCA followed by an optional query part."
/// "All implementations must recognize URIs in this family in the collation
/// argument of functions that take a collation argument."
#[test]
fn the_registry_recognizes_the_family() {
    let functions = XsltFunctions::new();
    let registry = functions.collations();
    assert!(registry.recognizes(UCA));
    assert!(registry.recognizes(&format!("{UCA}?")));
    assert!(registry.recognizes(&format!("{UCA}?lang=de;strength=primary")));
    // Not the family: another path, or something other than a query part.
    assert!(!registry.recognizes(&format!("{UCA}X")));
    assert!(!registry.recognizes(&format!("{UCA}/de")));
    // The default (no parameter) is the root collation at tertiary strength:
    // case matters, lower case first.
    assert_eq!(cmp("a", "A", ""), -1);
    assert_eq!(cmp("a", "b", ""), -1);
    // Resolving the same URI twice gives the same collation (the resolver
    // contract).
    let a = registry.resolve(UCA).unwrap();
    let b = registry.resolve(UCA).unwrap();
    assert!(std::rc::Rc::ptr_eq(&a, &b));
}

/// "strength (default tertiary): primary | secondary | tertiary | quaternary |
/// identical, or 1|2|3|4|5 as synonyms … Primary strength takes only the base
/// form of the character into account (so A=a=Ä=ä); secondary strength
/// ignores case but considers accents and diacritics as significant (so A=a
/// and Ä=ä but ä≠a); tertiary considers case as significant (A≠a≠Ä≠ä);
/// quaternary considers spaces and punctuation that would otherwise be ignored
/// (for example data-base=database)."
#[test]
fn strength_levels() {
    for primary in ["?strength=primary", "?strength=1"] {
        assert_eq!(cmp("A", "a", primary), 0);
        assert_eq!(cmp("a", "ä", primary), 0);
        assert_eq!(cmp("A", "ä", primary), 0);
        assert_eq!(cmp("a", "b", primary), -1);
    }
    for secondary in ["?strength=secondary", "?strength=2"] {
        assert_eq!(cmp("A", "a", secondary), 0);
        assert_eq!(cmp("Ä", "ä", secondary), 0);
        assert_eq!(cmp("a", "ä", secondary), -1);
    }
    for tertiary in ["?strength=tertiary", "?strength=3", ""] {
        assert_ne!(cmp("A", "a", tertiary), 0);
        assert_ne!(cmp("a", "ä", tertiary), 0);
        assert_ne!(cmp("A", "Ä", tertiary), 0);
    }
    // "quaternary considers spaces and punctuation that would otherwise be
    // ignored": with `alternate=shifted` the hyphen is ignored up to the
    // tertiary level and counts at the quaternary level.
    assert_eq!(cmp("data-base", "database", "?alternate=shifted"), 0);
    assert_ne!(
        cmp(
            "data-base",
            "database",
            "?alternate=shifted;strength=quaternary"
        ),
        0
    );
    assert_ne!(
        cmp("data-base", "database", "?alternate=shifted;strength=4"),
        0
    );
    // `identical` adds the code points (NFD) after every level: a Hebrew
    // cantillation mark is ignored even at the quaternary level.
    let (plain, marked) = (
        "\u{05D3}\u{05D7}\u{05D9}",
        "\u{05D3}\u{05D7}\u{05D9}\u{05AD}",
    );
    assert_eq!(cmp(plain, marked, "?strength=quaternary"), 0);
    assert_eq!(cmp(plain, marked, "?strength=identical"), -1);
    assert_eq!(cmp(plain, marked, "?strength=5"), -1);
}

/// "lang: language code: a string in the lexical space of xs:language. The
/// language whose collation conventions are to be used."
#[test]
fn lang_selects_a_tailoring() {
    // Swedish sorts ä after z; the root collation sorts it with a.
    assert_eq!(cmp("ä", "z", ""), -1);
    assert_eq!(cmp("ä", "z", "?lang=sv"), 1);
    // A region falls back to its language's tailoring (ICU4X locale fallback).
    assert_eq!(cmp("ä", "z", "?lang=sv-SE"), 1);
    // German has no such tailoring.
    assert_eq!(cmp("ä", "z", "?lang=de"), -1);
    // A tailoring may change a default: Danish puts upper case first.
    assert_eq!(cmp("A", "a", "?lang=da"), -1);
    assert_eq!(cmp("A", "a", "?lang=en"), 1);
    // A language with no tailoring of its own uses the root collation.
    assert_eq!(cmp("ä", "z", "?lang=qq"), -1);
}

/// "caseFirst: upper | lower. Indicates whether upper-case precedes lower-case
/// or vice versa." "numeric: yes | no (default no). When numeric=yes is
/// specified, a sequence of consecutive digits is interpreted as a number, for
/// example chap2 sorts before chap12." "backwards: yes | no (default no). The
/// value backwards=yes indicates that the last accent in the string is the
/// most significant." "caseLevel: yes | no (default no). When used with
/// primary strength, setting caseLevel=yes has the effect of ignoring accents
/// while taking account of case."
#[test]
fn case_numeric_backwards_and_case_level() {
    assert_eq!(cmp("A", "a", "?caseFirst=upper"), -1);
    assert_eq!(cmp("A", "a", "?caseFirst=lower"), 1);
    assert_eq!(cmp("A", "a", "?lang=da;caseFirst=lower"), 1);

    assert_eq!(cmp("chap2", "chap12", ""), 1);
    assert_eq!(cmp("chap2", "chap12", "?numeric=no"), 1);
    assert_eq!(cmp("chap2", "chap12", "?numeric=yes"), -1);

    // The French accent order: côte / coté differ in two accents; forwards the
    // first decides, backwards the last.
    assert_eq!(cmp("côte", "coté", ""), 1);
    assert_eq!(cmp("côte", "coté", "?backwards=no"), 1);
    assert_eq!(cmp("côte", "coté", "?backwards=yes"), -1);

    assert_eq!(cmp("a", "á", "?strength=primary;caseLevel=yes"), 0);
    assert_ne!(cmp("a", "A", "?strength=primary;caseLevel=yes"), 0);
    assert_eq!(cmp("a", "A", "?strength=primary;caseLevel=no"), 0);
}

/// "alternate: non-ignorable | shifted | blanked (default non-ignorable) …
/// The value non-ignorable indicates that such characters are treated as
/// distinct at the primary level (so data base sorts before datatype);
/// shifted indicates that they are used to differentiate two strings only at
/// the quaternary level". "maxVariable: space | punct | symbol | currency
/// (default punct). Indicates that all characters in the specified group and
/// earlier groups are treated as "noise" characters".
#[test]
fn alternate_and_max_variable() {
    assert_eq!(cmp("data base", "datatype", ""), -1);
    assert_eq!(cmp("data base", "datatype", "?alternate=non-ignorable"), -1);
    assert_ne!(cmp("data base", "database", "?alternate=non-ignorable"), 0);
    assert_eq!(cmp("data base", "database", "?alternate=shifted"), 0);
    // `punct` (the default) shifts punctuation; `space` only spaces.
    assert_eq!(cmp("a-b", "ab", "?alternate=shifted"), 0);
    assert_eq!(cmp("a-b", "ab", "?alternate=shifted;maxVariable=punct"), 0);
    assert_ne!(cmp("a-b", "ab", "?alternate=shifted;maxVariable=space"), 0);
    assert_eq!(cmp("a b", "ab", "?alternate=shifted;maxVariable=space"), 0);
    // `symbol` shifts symbols as well, `punct` does not.
    assert_ne!(cmp("a+b", "ab", "?alternate=shifted;maxVariable=punct"), 0);
    assert_eq!(cmp("a+b", "ab", "?alternate=shifted;maxVariable=symbol"), 0);
}

/// "If two query parameters use the same keyword then the last one wins."
#[test]
fn the_last_parameter_wins() {
    assert_eq!(cmp("A", "a", "?strength=primary;strength=tertiary"), 1);
    assert_eq!(cmp("A", "a", "?strength=tertiary;strength=primary"), 0);
}

/// "If the fallback parameter is omitted or takes the value yes, and if the
/// collation URI is well-formed according to the rules in this section, then
/// the implementation must accept the collation URI" … "if it does not
/// recognize the keyword or value then if the fallback parameter is present
/// with the value no it should reject the collation as unsupported, otherwise
/// it should ignore the unrecognized parameter."
#[test]
fn unsupported_parameters_are_ignored_by_default() {
    for parameter in [
        // Parameters ICU4X 1.5 cannot honour: reordering, a UCA version later
        // than the data's (15.1.0), a `co` type the data lacks.
        "version=16.0",
        "reorder=Grek,Latn",
        "lang=en-u-co-phonebk",
        // Values and keywords the specification does not define.
        "strength=strong",
        "caseFirst=off",
        "numeric=maybe",
        "lang=x-klingon",
        "colour=blue",
    ] {
        for query in [
            format!("?{parameter}"),
            format!("?fallback=yes;{parameter}"),
        ] {
            // Ignored: the result is the root collation's (tertiary, so case
            // differs and accents differ).
            assert_eq!(cmp("a", "A", &query), -1, "{query}");
            assert_eq!(cmp("a", "a", &query), 0, "{query}");
        }
    }
    // An unrecognized value of `fallback` itself is ignored: the default yes.
    assert_eq!(cmp("a", "A", "?fallback=maybe;version=99"), -1);
}

/// "If the fallback parameter is present with the value no, then the
/// implementation must either use a collation that conforms with the rules in
/// the Unicode specifications for the requested tailoring, or fail with a
/// static or dynamic error indicating that it does not provide the collation
/// (the error code should be the same as if the collation URI were not
/// recognized)." For a function's `$collation` argument that is FOCH0002 (F&O
/// 2.0 §7.3.1).
#[test]
fn fallback_no_rejects_what_cannot_be_honoured() {
    for parameter in [
        "version=16.0",
        "reorder=Grek,Latn",
        "alternate=blanked;strength=identical",
        "strength=strong",
        "lang=x-klingon",
        "lang=de-u-co-bogus",
        "colour=blue",
    ] {
        for query in [
            format!("?fallback=no;{parameter}"),
            // `fallback` applies wherever it is written.
            format!("?{parameter};fallback=no"),
        ] {
            let result = eval(&format!(
                r#"<xsl:value-of select="compare('a', 'b', '{UCA}{query}')"/>"#
            ));
            assert_eq!(code(result), "FOCH0002", "{query}");
        }
    }
    // What can be honoured is accepted with `fallback=no`, `normalization`
    // either way (ICU4X always normalizes, which conforms to both), a UCA
    // version up to the data's, `blanked` as UTS #10's shifted-at-tertiary.
    assert_eq!(
        cmp(
            "A",
            "a",
            "?fallback=no;lang=sv;strength=primary;caseFirst=upper;numeric=yes;alternate=shifted;\
             maxVariable=space;backwards=no;caseLevel=no;normalization=no;version=15.1.0"
        ),
        0
    );
    assert_eq!(cmp("a", "A", "?fallback=no;normalization=yes"), -1);
    assert_eq!(cmp("a", "A", "?fallback=no;version=6.2.0"), -1);
    assert_eq!(cmp("a-b", "ab", "?fallback=no;alternate=blanked"), 0);
    // A URI that is not well-formed is never accepted, with or without
    // fallback (the obligation covers well-formed URIs only).
    for query in ["?strength", "?=primary"] {
        let result = eval(&format!(
            r#"<xsl:value-of select="compare('a', 'b', '{UCA}{query}')"/>"#
        ));
        assert_eq!(code(result), "FOCH0002", "{query}");
    }
}

// ═══ The default collation (§3.6.1, §5.4.1) ═══════════════════════════════

/// §3.6.1: "The default-collation attribute is a standard attribute that may
/// appear on any element in the XSLT namespace, or (as xsl:default-collation)
/// on a literal result element. The attribute is used to specify the default
/// collation used by all XPath expressions appearing in the attributes of this
/// element, or attributes of descendant elements, unless overridden by another
/// default-collation attribute on an inner element." XPath 2.0 §3.5.1 and F&O
/// §7.3.1 use it for `eq`, `=`, `compare()` without a collation,
/// `distinct-values()` and `index-of()`.
#[test]
fn a_uca_default_collation_reaches_every_comparison() {
    let secondary = format!("{UCA}?strength=secondary");
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/" default-collation="{secondary}">
                 <out>
                   <eq><xsl:value-of select="'GREEN23' eq 'green23'"/></eq>
                   <general><xsl:value-of select="('x', 'GREEN') = 'green'"/></general>
                   <compare><xsl:value-of select="compare('Äpfel', 'äpfel')"/></compare>
                   <accent><xsl:value-of select="compare('apfel', 'äpfel')"/></accent>
                   <distinct><xsl:value-of select="count(distinct-values(('a', 'A', 'b', 'B', 'á')))"/></distinct>
                   <index><xsl:value-of select="index-of(('A', 'b', 'a', 'á'), 'a')"/></index>
                   <min><xsl:value-of select="min(('b', 'A', 'a'))"/></min>
                 </out>
               </xsl:template>"#
        )),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><eq>true</eq><general>true</general><compare>0</compare><accent>-1</accent>\
         <distinct>3</distinct><index>1 3</index><min>A</min></out>"
    );
    // The same expressions outside the attribute's scope use the codepoint
    // collation.
    assert_eq!(xpath("'GREEN23' eq 'green23'"), "false");
}

/// §3.6.1: "If the implementation recognizes one or more of the resulting
/// absolute collation URIs, then it uses the first one that it recognizes as
/// the default collation."
/// \[ERR XTSE0125\]: "It is a static error if the value of an
/// \[xsl:\]default-collation attribute, after resolving against the base URI,
/// contains no URI that the implementation recognizes as a collation URI."
#[test]
fn default_collation_takes_the_first_supported_uca_uri() {
    let strict = format!("{UCA}?fallback=no;reorder=Grek");
    let primary = format!("{UCA}?strength=primary");
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/" default-collation="{strict} {primary}">
                 <out><xsl:value-of select="'a' eq 'Á'"/></out>
               </xsl:template>"#
        )),
        "<doc/>",
    );
    assert_eq!(result.unwrap(), out("true"));
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/" default-collation="{strict}"><out/></xsl:template>"#
        )),
        "<doc/>",
    );
    assert_eq!(code(result), "XTSE0125");
}

/// F&O 2.0 §7.5: the five substring functions work under a UCA collation —
/// by collation-equal substrings at character boundaries (`uca.rs`,
/// "Substring matching"; the detailed cases are in `tests/collation_indexes_and_uca_parameters.rs`) —
/// with an explicit `$collation` and under the default collation alike.
#[test]
fn substring_functions_match_collation_equal_substrings() {
    let secondary = format!("{UCA}?strength=secondary");
    for (call, expected) in [
        (format!("contains('abc', 'B', '{secondary}')"), "true"),
        (format!("starts-with('abc', 'A', '{secondary}')"), "true"),
        (format!("ends-with('abc', 'C', '{secondary}')"), "true"),
        (format!("substring-before('abc', 'B', '{secondary}')"), "a"),
        (format!("substring-after('abc', 'B', '{secondary}')"), "c"),
        (format!("contains('abc', 'á', '{secondary}')"), "false"),
    ] {
        assert_eq!(xpath(&call), expected, "{call}");
    }
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/" default-collation="{secondary}"><out><xsl:value-of select="contains('abc', 'B')"/></out></xsl:template>"#
        )),
        "<doc/>",
    );
    assert_eq!(result.unwrap(), out("true"));
}

// ═══ xsl:sort (§13.1.3) ═══════════════════════════════════════════════════

/// Sorts `words` with an `xsl:sort` carrying `attrs`; the result joined by
/// commas.
fn sorted(words: &[&str], attrs: &str) -> String {
    let items = words
        .iter()
        .map(|w| format!("'{w}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = eval(&format!(
        r#"<xsl:value-of separator=","><xsl:perform-sort select="({items})"><xsl:sort select="." {attrs}/></xsl:perform-sort></xsl:value-of>"#
    ))
    .unwrap_or_else(|e| panic!("{attrs}: {e}"));
    result
        .strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .unwrap()
        .to_string()
}

/// §13.1.3: "The lang attribute indicates that a collation suitable for a
/// particular natural language should be used." With the feature it is the
/// UCA tailored for that language.
#[test]
fn sort_lang_chooses_a_locale_tailored_collation() {
    let words = ["zebra", "ärger", "apple", "Zulu", "öl", "Apple"];
    assert_eq!(
        sorted(&words, r#"lang="sv""#),
        "apple,Apple,zebra,Zulu,ärger,öl"
    );
    assert_eq!(
        sorted(&words, r#"lang="de""#),
        "apple,Apple,ärger,öl,zebra,Zulu"
    );
    // An attribute value template, and a region.
    assert_eq!(
        sorted(&words, r#"lang="{'sv-SE'}""#),
        "apple,Apple,zebra,Zulu,ärger,öl"
    );
    // A language without a tailoring of its own: the root collation.
    assert_eq!(
        sorted(&words, r#"lang="qq""#),
        "apple,Apple,ärger,öl,zebra,Zulu"
    );
    // The language's own case default: Danish sorts upper case first.
    assert_eq!(sorted(&["a", "A", "b", "B"], r#"lang="da""#), "A,a,B,b");
}

/// §13.1.3: "When lower-first is requested, the returned collation should have
/// the property that when two strings differ only in the case of one or more
/// characters, then a string in which the first differing character is
/// lower-case should precede a string in which the corresponding character is
/// title-case, which should in turn precede a string in which the
/// corresponding character is upper-case." "if lower-first is requested, then
/// a sorted sequence might be "MacAndrew, macintosh, macIntosh, Macintosh,
/// MacIntosh, macintoshes, Macintoshes, McIntosh". If upper-first is
/// requested, the same sequence would sort as "MacAndrew, MacIntosh,
/// Macintosh, macIntosh, macintosh, MacIntoshes, macintoshes, McIntosh""
#[test]
fn sort_case_order_reproduces_the_spec_examples() {
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
    assert_eq!(
        sorted(&words, r#"lang="en" case-order="lower-first""#),
        "MacAndrew,macintosh,macIntosh,Macintosh,MacIntosh,macintoshes,Macintoshes,McIntosh"
    );
    assert_eq!(
        sorted(&words, r#"lang="en" case-order="upper-first""#),
        "MacAndrew,MacIntosh,Macintosh,macIntosh,macintosh,Macintoshes,macintoshes,McIntosh"
    );
    // "So, for example, if lang="en", then A a B b are sorted with
    // case-order="upper-first" and a A b B are sorted with
    // case-order="lower-first"." Without `lang`: the root collation.
    assert_eq!(
        sorted(&["b", "A", "B", "a"], r#"case-order="upper-first""#),
        "A,a,B,b"
    );
    assert_eq!(
        sorted(&["b", "A", "B", "a"], r#"case-order="lower-first""#),
        "a,A,b,B"
    );
    // `case-order` overrides the language's default.
    assert_eq!(
        sorted(
            &["a", "A", "b", "B"],
            r#"lang="da" case-order="lower-first""#
        ),
        "a,A,b,B"
    );
    // And the tailoring still applies under an explicit case order.
    assert_eq!(
        sorted(&["ä", "z", "Z"], r#"lang="sv" case-order="upper-first""#),
        "Z,z,ä"
    );
}

/// §13.1.3: "The lang and case-order attributes are ignored if a collation
/// attribute is present." "If the xsl:sort element has a collation attribute,
/// then the strings are compared according to the rules for the named
/// collation: that is, they are compared using the XPath function call
/// compare($a, $b, $collation)."
#[test]
fn sort_collation_attribute_wins_over_lang_and_case_order() {
    let words = ["zebra", "ärger", "apple"];
    assert_eq!(
        sorted(
            &words,
            &format!(r#"lang="de" case-order="upper-first" collation="{UCA}?lang=sv""#)
        ),
        "apple,zebra,ärger"
    );
    assert_eq!(
        sorted(
            &["b", "A", "B", "a"],
            &format!(r#"lang="en" case-order="lower-first" collation="{UCA}?caseFirst=upper""#)
        ),
        "A,a,B,b"
    );
    // [ERR XTDE1035] "if the collation attribute of xsl:sort (after resolving
    // against the base URI) is not a URI that is recognized by the
    // implementation as referring to a collation".
    let result = eval(&format!(
        r#"<xsl:for-each select="('b', 'a')"><xsl:sort select="." collation="{UCA}?fallback=no;version=99"/></xsl:for-each>"#
    ));
    assert_eq!(code(result), "XTDE1035");
}

// ═══ xsl:key (§16.3.1) ════════════════════════════════════════════════════

/// §16.3.1: "The optional collation attribute is used only when deciding
/// whether two strings are equal for the purposes of key matching.
/// Specifically, two values $a and $b are considered equal if the result of
/// the function call compare($a, $b, $collation) is zero. The effective
/// collation for an xsl:key declaration is the collation specified in its
/// collation attribute if present, resolved against the base URI of the
/// xsl:key element, or the default collation that is in scope for the xsl:key
/// declaration otherwise". \[ERR XTSE1210\] for an unrecognized collation.
#[test]
fn keys_under_a_uca_collation() {
    let source = "<r><w>apple</w><w>Apple</w><w>Äpfel</w><w>APPLE</w><w>pear</w></r>";
    let primary = format!("{UCA}?strength=primary");
    let secondary = format!("{UCA}?strength=secondary");
    let result = run(
        &sheet(&format!(
            r#"<xsl:key name="explicit" match="w" use="." collation="{secondary}"/>
               <xsl:key name="default" match="w" use="." default-collation="{primary}"/>
               <xsl:template match="/">
                 <out>
                   <a><xsl:value-of select="key('explicit', 'APPLE')" separator=","/></a>
                   <b><xsl:value-of select="key('default', 'apfel')" separator=","/></b>
                   <c><xsl:value-of select="count(key('explicit', 'Pear'))"/></c>
                 </out>
               </xsl:template>"#
        )),
        source,
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><a>apple,Apple,APPLE</a><b>Äpfel</b><c>1</c></out>"
    );
    let result = run(
        &sheet(&format!(
            r#"<xsl:key name="k" match="w" use="." collation="{UCA}?fallback=no;reorder=Latn"/>
               <xsl:template match="/"><out/></xsl:template>"#
        )),
        source,
    );
    assert_eq!(code(result), "XTSE1210");
}

// ═══ xsl:for-each-group (§14) ═════════════════════════════════════════════

/// §14.3: "If the values are strings, or untyped atomic values, then if there
/// is a collation attribute the values are compared using the collation
/// specified as the effective value of the collation attribute, resolved if
/// relative against the base URI of the xsl:for-each-group element. If there
/// is no collation attribute then the default collation is used." \[ERR
/// XTDE1110\] "if the collation URI specified to xsl:for-each-group (after
/// resolving against the base URI) is a collation that is not recognized by
/// the implementation". (The W3C case `for-each-group-086` groups adjacent
/// items under `UCA?strength=secondary`.)
#[test]
fn grouping_under_a_uca_collation() {
    let secondary = format!("{UCA}?strength=secondary");
    let source = "<r><l>English</l><l>english</l><l>Chinese</l><l>French</l>\
                  <l>FRENCH</l><l>Chinese</l><l>Frénch</l></r>";
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/">
                 <out>
                   <adjacent><xsl:for-each-group select="r/l" group-adjacent="." collation="{secondary}"><g n="{{count(current-group())}}"><xsl:value-of select="current-grouping-key()"/></g></xsl:for-each-group></adjacent>
                   <by><xsl:for-each-group select="r/l" group-by="." collation="{secondary}"><g n="{{count(current-group())}}"><xsl:value-of select="."/></g></xsl:for-each-group></by>
                   <default><xsl:for-each-group select="r/l" group-by="." default-collation="{UCA}?strength=primary"><g n="{{count(current-group())}}"><xsl:value-of select="."/></g></xsl:for-each-group></default>
                 </out>
               </xsl:template>"#
        )),
        source,
    )
    .unwrap();
    assert_eq!(
        result,
        "<out>\
           <adjacent><g n=\"2\">English</g><g n=\"1\">Chinese</g><g n=\"2\">French</g>\
             <g n=\"1\">Chinese</g><g n=\"1\">Frénch</g></adjacent>\
           <by><g n=\"2\">English</g><g n=\"2\">Chinese</g><g n=\"2\">French</g><g n=\"1\">Frénch</g></by>\
           <default><g n=\"2\">English</g><g n=\"2\">Chinese</g><g n=\"3\">French</g></default>\
         </out>"
    );
    let result = run(
        &sheet(&format!(
            r#"<xsl:template match="/"><out><xsl:for-each-group select="r/l" group-by="." collation="{UCA}?reorder=Grek;fallback=no"><x/></xsl:for-each-group></out></xsl:template>"#
        )),
        source,
    );
    assert_eq!(code(result), "XTDE1110");
}
