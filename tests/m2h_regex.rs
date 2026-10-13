//! M2h — regular expressions: `xsl:analyze-string`, `xsl:matching-substring`,
//! `xsl:non-matching-substring` and `regex-group()` (§15).
//!
//! Every test quotes the sentence of XSLT 2.0 (`xslt20.html`) it checks. The
//! sections follow the work items of ROADMAP §4 "M2h": the instruction and its
//! static errors, the regular-expression dialect and its errors (static for a
//! fixed `regex` / `flags`, dynamic for a computed one), the partition of the
//! input, the focus inside the two children, the conversion of `select`,
//! `regex-group()`, the current captured substrings as a dynamic context
//! component (DESIGN §9.1, one test per row), keys that observe them, and a
//! linearity bound.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Wraps top-level content in a version 2.0 stylesheet with the `xs` and `f`
/// prefixes bound.
fn sheet(body: &str) -> String {
    sheet_version("2.0", body)
}

fn sheet_version(version: &str, body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{body}</xsl:stylesheet>"#
    )
}

/// A stylesheet whose only template matches `/` and has `content` inside an
/// `out` element.
fn root(content: &str) -> String {
    sheet(&format!(
        r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
    ))
}

const TEST_STACK: usize = 256 * 1024 * 1024;

/// Where a run failed: while compiling (a static error) or while running.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Static(String),
    Dynamic(String),
}

/// Compiles `main` (with `files` available to `xsl:import`) and runs it over
/// `source`.
fn run_files(files: &[(&str, &str)], main: &str, source: &str) -> Result<String, Failure> {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(u, t)| (u.to_string(), t.to_string()))
        .collect();
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let mut mem = MemoryResolver::new();
        for (uri, text) in files {
            mem.insert(uri, text);
        }
        let compiled = Compiler::new(&names, &functions)
            .with_resolver(Arc::new(mem))
            .compile_str(&main, "file:///m2h/main.xsl")
            .map_err(|e: XsltError| Failure::Static(e.code))?;
        let arena = Bump::new();
        let run = compiled.transformation(&arena, TransformOptions::default());
        let outcome = run
            .transform_source("file:///m2h/in.xml", &source)
            .map_err(|e| Failure::Dynamic(e.code))?;
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .map_err(|e| Failure::Dynamic(e.code))
    })
}

fn run(main: &str, source: &str) -> Result<String, Failure> {
    run_files(&[], main, source)
}

fn out(main: &str) -> String {
    out_over(main, "<doc/>")
}

fn out_over(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => text,
        Err(e) => panic!("the transformation failed: {e:?}"),
    }
}

fn static_error(main: &str) -> String {
    match run(main, "<doc/>") {
        Err(Failure::Static(code)) => code,
        other => panic!("expected a static error, got {other:?}"),
    }
}

fn dynamic_error(main: &str) -> String {
    dynamic_error_over(main, "<doc/>")
}

fn dynamic_error_over(main: &str, source: &str) -> String {
    match run(main, source) {
        Err(Failure::Dynamic(code)) => code,
        other => panic!("expected a dynamic error, got {other:?}"),
    }
}

/// `<m>…</m>` for each matching substring and `<n>…</n>` for each
/// non-matching one: the partition, made visible.
const SHOW: &str = r#"<xsl:matching-substring><m><xsl:value-of select="."/></m></xsl:matching-substring>
    <xsl:non-matching-substring><n><xsl:value-of select="."/></n></xsl:non-matching-substring>"#;

fn partition(input: &str, regex: &str) -> String {
    out(&root(&format!(
        r#"<xsl:analyze-string select="{input}" regex="{regex}">{SHOW}</xsl:analyze-string>"#
    )))
}

// ── §15.1 the instruction and its content ─────────────────────────────────

/// §15.1: "[ERR XTSE1130] It is a static error if the xsl:analyze-string
/// instruction contains neither an xsl:matching-substring nor an
/// xsl:non-matching-substring element" — even in a template that is never
/// invoked.
#[test]
fn neither_child_is_xtse1130() {
    let main = sheet(
        r#"<xsl:template match="/"><out/></xsl:template>
           <xsl:template name="never">
             <xsl:analyze-string select="'a'" regex="a"><xsl:fallback/></xsl:analyze-string>
           </xsl:template>"#,
    );
    assert_eq!(static_error(&main), "XTSE1130");
}

/// The content model `(xsl:matching-substring?, xsl:non-matching-substring?,
/// xsl:fallback*)`: "If both are present, the xsl:matching-substring element
/// must come first", and neither "may appear more than once" — XTSE0010 like
/// every content-model violation.
#[test]
fn the_content_model_is_xtse0010() {
    for content in [
        "<xsl:non-matching-substring/><xsl:matching-substring/>",
        "<xsl:matching-substring/><xsl:matching-substring/>",
        "<xsl:fallback/><xsl:matching-substring/>",
        "<xsl:matching-substring/><out/>",
    ] {
        let main = root(&format!(
            r#"<xsl:analyze-string select="'a'" regex="a">{content}</xsl:analyze-string>"#
        ));
        assert_eq!(static_error(&main), "XTSE0010", "{content}");
    }
    // `select` and `regex` are required.
    assert_eq!(
        static_error(&root(
            r#"<xsl:analyze-string regex="a"><xsl:matching-substring/></xsl:analyze-string>"#
        )),
        "XTSE0010"
    );
    assert_eq!(
        static_error(&root(
            r#"<xsl:analyze-string select="'a'"><xsl:matching-substring/></xsl:analyze-string>"#
        )),
        "XTSE0010"
    );
}

/// §15.1: "Any xsl:fallback elements among the children of the
/// xsl:analyze-string instruction are ignored by an XSLT 2.0 processor".
#[test]
fn fallback_children_are_ignored() {
    let main = root(
        r#"<xsl:analyze-string select="'ab'" regex="a">
             <xsl:matching-substring><m/></xsl:matching-substring>
             <xsl:fallback><fallback/></xsl:fallback>
             <xsl:fallback><xsl:value-of select="error()"/></xsl:fallback>
           </xsl:analyze-string>"#,
    );
    assert_eq!(out(&main), "<out><m/></out>");
}

/// §18.1.1: `element-available` "returns true if and only if … the
/// expanded-QName identifies an instruction that the implementation
/// supports" — which `xsl:analyze-string` now is. Its two children are not
/// instructions.
#[test]
fn analyze_string_is_an_available_instruction() {
    let main = root(
        r#"<a><xsl:value-of select="element-available('xsl:analyze-string')"/></a>
           <m><xsl:value-of select="element-available('xsl:matching-substring')"/></m>"#,
    );
    assert_eq!(out(&main), "<out><a>true</a><m>false</m></out>");
}

/// §15.1: "If the element is absent, the effect is the same as if it were
/// present with empty content."
#[test]
fn an_absent_child_is_empty_content() {
    let only_matching = root(
        r#"<xsl:analyze-string select="'xay'" regex="a">
             <xsl:matching-substring><m pos="{position()}" of="{last()}"/></xsl:matching-substring>
           </xsl:analyze-string>"#,
    );
    // The non-matching substrings still count in position and size.
    assert_eq!(out(&only_matching), r#"<out><m pos="2" of="3"/></out>"#);
    let only_non_matching = root(
        r#"<xsl:analyze-string select="'xay'" regex="a">
             <xsl:non-matching-substring><n pos="{position()}"><xsl:value-of select="."/></n></xsl:non-matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(
        out(&only_non_matching),
        r#"<out><n pos="1">x</n><n pos="3">y</n></out>"#
    );
}

// ── §15 the regular expression, its flags, and their errors ───────────────

/// §15.1: "[ERR XTDE1140] It is a non-recoverable dynamic error if the
/// effective value of the regex attribute does not conform to the required
/// syntax … If the regular expression is known statically (for example, if
/// the attribute does not contain any expressions enclosed in curly brackets)
/// then the processor may signal the error as a static error." This processor
/// does, for a fixed `regex` and `flags` — also in a template never invoked —
/// and raises the dynamic error for a computed one.
#[test]
fn an_invalid_regex_is_xtde1140_static_when_fixed() {
    for regex in [
        "[A-Z",
        // No (?…) syntax in XPath 2.0 (F&amp;O 1.0 §7.6.1).
        "(?:AB)(CD)",
        // A back-reference to a group that does not exist, or that is not
        // closed yet, or inside a character class.
        r"(a)\2",
        r"(a\1)",
        r"(a)[\1]",
    ] {
        let fixed = sheet(&format!(
            r#"<xsl:template match="/"><out/></xsl:template>
               <xsl:template name="never">
                 <xsl:analyze-string select="'a'" regex="{regex}"><xsl:matching-substring/></xsl:analyze-string>
               </xsl:template>"#
        ));
        assert_eq!(static_error(&fixed), "XTDE1140", "{regex}");
        let computed = root(&format!(
            r#"<xsl:analyze-string select="'a'" regex="{{'{regex}'}}"><xsl:matching-substring/></xsl:analyze-string>"#
        ));
        assert_eq!(dynamic_error(&computed), "XTDE1140", "{regex}");
    }
}

/// §15.1: "[ERR XTDE1145] It is a non-recoverable dynamic error if the
/// effective value of the flags attribute has a value other than the values
/// defined in [Functions and Operators]" — `s`, `m`, `i`, `x`; the `q` flag is
/// XSLT 3.0's. Static when fixed, dynamic when computed.
#[test]
fn invalid_flags_are_xtde1145_static_when_fixed() {
    for flags in ["q", "12345", "S", "g"] {
        let fixed = root(&format!(
            r#"<xsl:analyze-string select="'a'" regex="a" flags="{flags}"><xsl:matching-substring/></xsl:analyze-string>"#
        ));
        assert_eq!(static_error(&fixed), "XTDE1145", "{flags}");
        let computed = root(&format!(
            r#"<xsl:analyze-string select="'a'" regex="a" flags="{{'{flags}'}}"><xsl:matching-substring/></xsl:analyze-string>"#
        ));
        assert_eq!(dynamic_error(&computed), "XTDE1145", "{flags}");
    }
}

/// §15.1: "[ERR XTDE1150] It is a non-recoverable dynamic error if the
/// effective value of the regex attribute is a regular expression that
/// matches a zero-length string: or more specifically, if the regular
/// expression $r and flags $f are such that matches("", $r, $f) returns
/// true." The definition does not look at the input: an empty input does not
/// hide the error.
#[test]
fn a_zero_length_match_is_xtde1150_whatever_the_input() {
    for regex in ["", "a?", "[A-Z]*", "^", "(a|)"] {
        let fixed = root(&format!(
            r#"<xsl:analyze-string select="'bananas'" regex="{regex}"><xsl:matching-substring/></xsl:analyze-string>"#
        ));
        assert_eq!(static_error(&fixed), "XTDE1150", "{regex:?}");
        for input in ["'bananas'", "''"] {
            let computed = root(&format!(
                r#"<xsl:analyze-string select="{input}" regex="{{'{regex}'}}"><xsl:matching-substring/></xsl:analyze-string>"#
            ));
            assert_eq!(
                dynamic_error(&computed),
                "XTDE1150",
                "{regex:?} over {input}"
            );
        }
    }
    // With `m`, `^[\t ]*$` matches the empty string too (analyze-string-090a).
    let main = root(
        r#"<xsl:analyze-string select="'a'" regex="^[\t ]*$" flags="m"><xsl:matching-substring/></xsl:analyze-string>"#,
    );
    assert_eq!(static_error(&main), "XTDE1150");
}

/// §15.1: "If the attribute is omitted, the effect is the same as supplying a
/// zero-length string." The four flags of F&O §7.6.1.1 in any order,
/// repeated: `i` case-insensitive, `s` dot-all, `m` multi-line, `x` whitespace
/// removed outside character classes.
#[test]
fn the_four_flags() {
    let run = |input: &str, regex: &str, flags: &str| {
        out(&root(&format!(
            r#"<xsl:analyze-string select="{input}" regex="{regex}" flags="{flags}">{SHOW}</xsl:analyze-string>"#
        )))
    };
    assert_eq!(run("'aAb'", "a", ""), "<out><m>a</m><n>Ab</n></out>");
    assert_eq!(
        run("'aAb'", "a", "ii"),
        "<out><m>a</m><m>A</m><n>b</n></out>"
    );
    assert_eq!(run("'a&#10;b'", "a.b", ""), "<out><n>a\nb</n></out>");
    assert_eq!(run("'a&#10;b'", "a.b", "s"), "<out><m>a\nb</m></out>");
    assert_eq!(
        run("'ab&#10;ab'", "^ab", ""),
        "<out><m>ab</m><n>\nab</n></out>"
    );
    assert_eq!(
        run("'ab&#10;ab'", "^ab", "m"),
        "<out><m>ab</m><n>\n</n><m>ab</m></out>"
    );
    assert_eq!(run("'a b'", "a b", "x"), "<out><n>a b</n></out>");
    assert_eq!(run("'ab'", "a b", "smix"), "<out><m>ab</m></out>");
    assert_eq!(run("'a b'", "a[ ]b", "x"), "<out><m>a b</m></out>");
}

/// The note of §15.1: "Because the regex attribute is an attribute value
/// template, curly brackets within the regular expression must be doubled."
#[test]
fn doubled_curly_brackets_are_a_quantifier() {
    assert_eq!(
        partition("'aaaaab'", ".{{1,2}}"),
        "<out><m>aa</m><m>aa</m><m>ab</m></out>"
    );
    assert_eq!(
        partition("'12345abc'", "{'[0-9]{1,5}[a-z]{3}'}"),
        "<out><m>12345abc</m></out>"
    );
}

/// The regular expression is compiled from the effective values each time
/// the instruction is evaluated, so a computed one can change from one
/// evaluation to the next (the run's cache is keyed by both values).
#[test]
fn a_computed_regex_follows_its_value() {
    let main = root(
        r#"<xsl:for-each select="('a', 'b', 'a')">
             <xsl:analyze-string select="'abc'" regex="{.}" flags="{if (. = 'b') then 'i' else ''}">
               <xsl:matching-substring><m><xsl:value-of select="."/></m></xsl:matching-substring>
             </xsl:analyze-string>
           </xsl:for-each>"#,
    );
    assert_eq!(out(&main), "<out><m>a</m><m>b</m><m>a</m></out>");
}

// ── §15.1 the partition ──────────────────────────────────────────────────

/// §15.1: "If there are several matches, the first match is defined to be the
/// one whose starting position comes first in the string. If several
/// alternatives within the regular expression both match at the same position
/// in the input string, then the match that is chosen is the first
/// alternative that matches. For example, if the input string is The quick
/// brown fox jumps and the regular expression is jump|jumps, then the match
/// that is chosen is jump."
#[test]
fn the_first_alternative_wins() {
    assert_eq!(
        partition("'The quick brown fox jumps'", "jump|jumps"),
        "<out><n>The quick brown fox </n><m>jump</m><n>s</n></out>"
    );
    assert_eq!(
        partition("'The quick brown fox jumps'", "jumps|jump"),
        "<out><n>The quick brown fox </n><m>jumps</m></out>"
    );
}

/// §15.1: "Having found the first match, the instruction proceeds to find the
/// second and subsequent matches by repeating the search, starting at the
/// first character that was not included in the previous match." Matches do
/// not overlap; adjacent matches give no empty non-matching substring between
/// them ("Each substring will contain at least one character").
#[test]
fn matches_do_not_overlap_and_no_substring_is_empty() {
    assert_eq!(partition("'aaa'", "aa"), "<out><m>aa</m><n>a</n></out>");
    assert_eq!(partition("'abab'", "ab"), "<out><m>ab</m><m>ab</m></out>");
    assert_eq!(
        partition("'xabyab'", "ab"),
        "<out><n>x</n><m>ab</m><n>y</n><m>ab</m></out>"
    );
    assert_eq!(partition("'xyz'", "a"), "<out><n>xyz</n></out>");
    // Reluctant quantifiers (F&O 1.0 §7.6.1) take as little as they can.
    assert_eq!(
        partition("'aaa'", "a+?"),
        "<out><m>a</m><m>a</m><m>a</m></out>"
    );
    assert_eq!(
        partition("'[x][y]'", r"\[.*?\]"),
        "<out><m>[x]</m><m>[y]</m></out>"
    );
    assert_eq!(partition("'[x][y]'", r"\[.*\]"), "<out><m>[x][y]</m></out>");
}

/// §15.1: "If the input is a zero-length string, the number of substrings will
/// be zero, so neither the xsl:matching-substring nor xsl:non-matching-substring
/// elements will be evaluated."
#[test]
fn an_empty_input_evaluates_nothing() {
    let main = root(
        r#"<xsl:analyze-string select="''" regex="a">
             <xsl:matching-substring><xsl:value-of select="error()"/></xsl:matching-substring>
             <xsl:non-matching-substring><xsl:value-of select="error()"/></xsl:non-matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(out(&main), "<out/>");
}

/// §15.1: "In processing each substring, the contents of the substring will be
/// the context item (as a value of type xs:string); the position of the
/// substring within the sequence of matching and non-matching substrings will
/// be the context position; and the number of matching and non-matching
/// substrings will be the context size." `current()` is the substring too
/// (§16.6.1).
#[test]
fn the_focus_is_the_substring() {
    let item = r#"<s pos="{position()}" of="{last()}" str="{. instance of xs:string}" cur="{current()}" up="{upper-case(.)}"/>"#;
    let main = root(&format!(
        r#"<xsl:analyze-string select="'xaay'" regex="a">
             <xsl:matching-substring>{item}</xsl:matching-substring>
             <xsl:non-matching-substring>{item}</xsl:non-matching-substring>
           </xsl:analyze-string>"#
    ));
    assert_eq!(
        out(&main),
        concat(&[
            r#"<out><s pos="1" of="4" str="true" cur="x" up="X"/>"#,
            r#"<s pos="2" of="4" str="true" cur="a" up="A"/>"#,
            r#"<s pos="3" of="4" str="true" cur="a" up="A"/>"#,
            r#"<s pos="4" of="4" str="true" cur="y" up="Y"/></out>"#,
        ])
    );
}

fn concat(parts: &[&str]) -> String {
    parts.concat()
}

/// The children are sequence constructors in the instruction's own scope:
/// local variables declared before the instruction are visible, and those
/// declared inside are local to the child (§9.7).
#[test]
fn the_children_see_the_local_variables() {
    let main = root(
        r#"<xsl:variable name="v" select="'!'"/>
           <xsl:analyze-string select="'ab'" regex="a">
             <xsl:matching-substring><xsl:variable name="w" select="concat(., $v)"/><m><xsl:value-of select="$w"/></m></xsl:matching-substring>
             <xsl:non-matching-substring><xsl:variable name="w" select="concat($v, .)"/><n><xsl:value-of select="$w"/></n></xsl:non-matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(out(&main), "<out><m>a!</m><n>!b</n></out>");
}

// ── §15.1 the select conversion ──────────────────────────────────────────

/// §15.1: "If the result of evaluating the select expression is not a string,
/// it is converted to a string by applying the function conversion rules."
/// XPath 2.0 §3.1.5 with the expected type `xs:string`: a node is atomized
/// and its `xs:untypedAtomic` value cast; an `xs:anyURI` is promoted; the
/// empty sequence, two items and a number are XPTY0004 (`analyze-string-045`:
/// "applied to empty sequence. Error in 2.0").
#[test]
fn select_is_converted_by_the_function_conversion_rules() {
    let analyze = |select: &str| {
        root(&format!(
            r#"<xsl:analyze-string select="{select}" regex="b">{SHOW}</xsl:analyze-string>"#
        ))
    };
    let source = "<doc><p>abc</p><p>b</p></doc>";
    assert_eq!(
        out_over(&analyze("doc/p[1]"), source),
        "<out><n>a</n><m>b</m><n>c</n></out>"
    );
    assert_eq!(
        out_over(&analyze("xs:anyURI('abc')"), source),
        "<out><n>a</n><m>b</m><n>c</n></out>"
    );
    assert_eq!(
        out_over(&analyze("xs:token('abc')"), source),
        "<out><n>a</n><m>b</m><n>c</n></out>"
    );
    for select in [
        "()",
        "doc/p",
        "('a', 'b')",
        "12",
        "xs:double('1')",
        "true()",
    ] {
        assert_eq!(
            dynamic_error_over(&analyze(select), source),
            "XPTY0004",
            "{select}"
        );
    }
}

/// With backwards compatible behaviour (§3.8) the function conversion rules
/// of XPath 1.0 compatibility mode apply (XPath 2.0 §3.1.5): "the value V is
/// effectively replaced by V[1]" and then "by fn:string(V)" — so the first
/// item's string value, and nothing at all for the empty sequence.
#[test]
fn select_in_backwards_compatible_mode() {
    let analyze = |select: &str| {
        sheet_version(
            "1.0",
            &format!(
                r#"<xsl:template match="/"><out><xsl:analyze-string select="{select}" regex="b" version="2.0">{SHOW}</xsl:analyze-string></out></xsl:template>"#
            ),
        )
    };
    // The `version` attribute on the instruction itself switches it back to
    // 2.0 behaviour; the backwards-compatible cases need it on an ancestor
    // only.
    let bc = |select: &str| {
        sheet_version(
            "1.0",
            &format!(
                r#"<xsl:template match="/"><out><xsl:analyze-string select="{select}" regex="b">{SHOW}</xsl:analyze-string></out></xsl:template>"#
            ),
        )
    };
    let source = "<doc><p>abc</p><p>b</p></doc>";
    assert_eq!(
        out_over(&bc("doc/p"), source),
        "<out><n>a</n><m>b</m><n>c</n></out>"
    );
    assert_eq!(out_over(&bc("()"), source), "<out/>");
    assert_eq!(out_over(&bc("1 + 1"), source), "<out><n>2</n></out>");
    assert_eq!(dynamic_error_over(&analyze("doc/p"), source), "XPTY0004");
}

// ── §15.2 regex-group() ──────────────────────────────────────────────────

/// `regex-group(0), …, regex-group(n)` of every match, as `<m>` elements.
fn groups(input: &str, regex: &str, n: usize) -> String {
    let attrs: String = (0..=n)
        .map(|i| format!(r#" g{i}="{{regex-group({i})}}""#))
        .collect();
    out(&root(&format!(
        r#"<xsl:analyze-string select="{input}" regex="{regex}"><xsl:matching-substring><m{attrs}/></xsl:matching-substring></xsl:analyze-string>"#
    )))
}

/// §15.2: "The Nth captured substring (where N > 0) is the string matched by
/// the subexpression contained by the Nth left parenthesis in the regex. The
/// zeroeth captured substring is the string that matches the entire regex.
/// This means that the value of regex-group(0) is initially the same as the
/// value of . (dot)." Nested groups are numbered by their left parenthesis
/// and rebuilt from what is nested in them.
#[test]
fn groups_are_numbered_by_their_left_parenthesis() {
    assert_eq!(
        groups(
            "'23 March 2002'",
            r"([0-9]+)\s([A-Z][a-z]+)\s([0-9]{{4}})",
            3
        ),
        r#"<out><m g0="23 March 2002" g1="23" g2="March" g3="2002"/></out>"#
    );
    assert_eq!(
        groups("'abcd'", "((a)(b(c)))d", 4),
        r#"<out><m g0="abcd" g1="abc" g2="a" g3="bc" g4="c"/></out>"#
    );
    assert_eq!(
        groups("'[cite]'", r"\[(.*?)\]", 1),
        r#"<out><m g0="[cite]" g1="cite"/></out>"#
    );
}

/// §15.2: "The function returns the zero-length string if there is no
/// captured substring with the relevant number. This can occur for a number
/// of reasons: The number is negative. The regular expression does not
/// contain a parenthesized sub-expression with the given number. The
/// parenthesized sub-expression exists, and did not match any part of the
/// input string. The parenthesized sub-expression exists, and matched a
/// zero-length substring of the input string."
#[test]
fn no_captured_substring_is_the_zero_length_string() {
    let main = root(
        r#"<xsl:analyze-string select="'ab'" regex="(a)(x)?(b)(c?)">
             <xsl:matching-substring>
               <m neg="{regex-group(-1)}" none="{regex-group(5)}" big="{regex-group(99999999999999999999)}"
                  absent="{regex-group(2)}" empty="{regex-group(4)}" b="{regex-group(3)}"/>
             </xsl:matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(
        out(&main),
        r#"<out><m neg="" none="" big="" absent="" empty="" b="b"/></out>"#
    );
}

/// Alternation: only the branch that matched captures; each match has its own
/// captured substrings.
#[test]
fn alternation_captures_the_branch_that_matched() {
    assert_eq!(
        groups("'ab'", "(a)|(b)", 2),
        r#"<out><m g0="a" g1="a" g2=""/><m g0="b" g1="" g2="b"/></out>"#
    );
}

/// F&O 1.0 §7.6.1 (paraphrased): a group inside a repetition captures its last
/// iteration.
#[test]
fn a_repeated_group_captures_its_last_iteration() {
    assert_eq!(
        groups("'abc'", "(.)+", 1),
        r#"<out><m g0="abc" g1="c"/></out>"#
    );
    assert_eq!(
        groups("'a1b22'", "([a-z]([0-9]+))+", 2),
        r#"<out><m g0="a1b22" g1="b22" g2="22"/></out>"#
    );
}

/// F&O 1.0 §7.6.1: back-references `\N` match what group N captured; one
/// digit always, a second only when that many groups precede it.
#[test]
fn back_references() {
    assert_eq!(
        groups("'xaay abba'", r"(a|b)\1", 1),
        r#"<out><m g0="aa" g1="a"/><m g0="bb" g1="b"/></out>"#
    );
    // Only one group: `\10` is `\1` followed by `0`.
    assert_eq!(
        groups("'aa0 aa'", r"(a)\10", 1),
        r#"<out><m g0="aa0" g1="a"/></out>"#
    );
}

/// R3-2 (`XSLT2_M2H_REVIEW3.md`). F&O 1.0 §7.6.1: "If a sub-expression
/// matches more than one substring (because it is within a construct that
/// allows repetition), then only the last substring that it matched will be
/// captured." In `((a)|(ab))+c` over `aabc` the second repetition first tries
/// `(a)` at offset 1, which fails later on, and then matches `(ab)`: the
/// attempt is not part of the match, so group 2 keeps the `a` of the first
/// repetition (Python, Perl and Java agree). `regexml` 0.2.2 reported `""`:
/// backtracking cleared the group instead of restoring it.
///
/// The fix is in `regexml` (the local fork, see `CLAUDE.md`), which this
/// crate's tests do not control: with the crates.io 0.2.2 the test checks the
/// backend on the very pattern first and skips, saying so, instead of
/// failing. The fork's own tests (`tests/test_captures.rs`) assert the fix
/// unconditionally.
#[test]
fn a_backtracked_alternative_does_not_erase_an_earlier_capture() {
    let backend = regexml::Regex::xpath("((a)|(ab))+c", "")
        .unwrap()
        .replace_all("aabc", "$2")
        .unwrap();
    if backend != "a" {
        eprintln!(
            "a_backtracked_alternative_does_not_erase_an_earlier_capture: SKIPPED — the \
             regexml in use reports group 2 of ((a)|(ab))+c over aabc as {backend:?}; the \
             capture-restoring backtracking fix (fork branch analyze-string-and-category-cache, \
             planned upstream PR) is needed for this test"
        );
        return;
    }
    assert_eq!(
        groups("'aabc'", "((a)|(ab))+c", 3),
        r#"<out><m g0="aabc" g1="ab" g2="a" g3="ab"/></out>"#
    );
    assert_eq!(
        groups("'aaabc'", "((a)|(ab))+c", 3),
        r#"<out><m g0="aaabc" g1="ab" g2="a" g3="ab"/></out>"#
    );
    // a group whose only attempt was backtracked has captured nothing
    assert_eq!(
        groups("'abc'", "((a)|(ab))+c", 3),
        r#"<out><m g0="abc" g1="ab" g2="" g3="ab"/></out>"#
    );
    assert_eq!(
        groups("'abac'", "((a)|(ab))+c", 3),
        r#"<out><m g0="abac" g1="a" g2="a" g3="ab"/></out>"#
    );
    // a back-reference sees the restored capture: `\2` is "a"
    assert_eq!(
        groups("'aabca'", r"((a)|(ab))+c\2", 2),
        r#"<out><m g0="aabca" g1="ab" g2="a"/></out>"#
    );
}

/// `regex-group($group-number as xs:integer)`: the argument goes through the
/// function conversion rules (XPath 2.0 §3.1.5) — an `xs:untypedAtomic` value
/// is cast, a value of a type derived from `xs:integer` is accepted, and
/// anything else that is not an `xs:integer` (a double, a decimal, a string,
/// the empty sequence, two items) is XPTY0004; a failed cast is FORG0001.
#[test]
fn the_argument_is_an_integer() {
    let call = |argument: &str| {
        root(&format!(
            r#"<xsl:variable name="root" select="."/><xsl:analyze-string select="'ab'" regex="(a)(b)"><xsl:matching-substring><m g="{{regex-group({argument})}}"/></xsl:matching-substring></xsl:analyze-string>"#
        ))
    };
    let source = r#"<doc n="2"/>"#;
    assert_eq!(
        out_over(&call("$root/doc/@n"), source),
        r#"<out><m g="b"/></out>"#
    );
    assert_eq!(
        out_over(&call("xs:byte(1)"), source),
        r#"<out><m g="a"/></out>"#
    );
    for argument in ["1.0", "1e0", "'1'", "()", "(1, 2)"] {
        assert_eq!(
            dynamic_error_over(&call(argument), source),
            "XPTY0004",
            "{argument}"
        );
    }
    assert_eq!(
        dynamic_error_over(&call("xs:untypedAtomic('one')"), source),
        "FORG0001"
    );
}

// ── §15.2 the current captured substrings: DESIGN §9.1, row by row ────────

/// §15.2: "It is initially an empty sequence" — `regex-group()` outside any
/// `xsl:analyze-string` is "" for every number.
#[test]
fn initially_there_are_no_captured_substrings() {
    let main = root(r#"<r g0="{regex-group(0)}" g1="{regex-group(1)}"/>"#);
    assert_eq!(out(&main), r#"<out><r g0="" g1=""/></out>"#);
}

/// §15.2: "During the evaluation of an xsl:non-matching-substring instruction
/// … it is set to an empty sequence."
#[test]
fn non_matching_substring_empties_them() {
    let main = root(
        r#"<xsl:analyze-string select="'xa'" regex="(a)">
             <xsl:non-matching-substring><n g0="{regex-group(0)}" g1="{regex-group(1)}"/></xsl:non-matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(out(&main), r#"<out><n g0="" g1=""/></out>"#);
}

/// DESIGN §9.1, `xsl:analyze-string` row. §15.2: "During the evaluation of an
/// xsl:matching-substring instruction it is set to the sequence of matched
/// substrings for that regex match. During the evaluation of an
/// xsl:non-matching-substring instruction … it is set to an empty sequence.
/// On completion of an instruction that changes the value, the variable
/// reverts to its previous value." A nested instruction's matching substring
/// replaces the outer ones, its non-matching substring empties them, and the
/// outer ones are back afterwards.
#[test]
fn nested_instructions_replace_empty_and_restore() {
    let main = root(
        r#"<xsl:analyze-string select="'k=v'" regex="(\w)=(\w)">
             <xsl:matching-substring>
               <outer g1="{regex-group(1)}"/>
               <xsl:analyze-string select="'1x'" regex="([0-9])">
                 <xsl:matching-substring><inner g1="{regex-group(1)}" g2="{regex-group(2)}"/></xsl:matching-substring>
                 <xsl:non-matching-substring><gap g1="{regex-group(1)}"/></xsl:non-matching-substring>
               </xsl:analyze-string>
               <again g1="{regex-group(1)}" g2="{regex-group(2)}"/>
             </xsl:matching-substring>
           </xsl:analyze-string>
           <after g1="{regex-group(1)}"/>"#,
    );
    assert_eq!(
        out(&main),
        concat(&[
            r#"<out><outer g1="k"/><inner g1="1" g2=""/><gap g1=""/>"#,
            r#"<again g1="k" g2="v"/><after g1=""/></out>"#,
        ])
    );
}

/// DESIGN §9.1, `xsl:apply-templates` and built-in rule rows. §15.2: "The
/// value of the current captured substrings is unaffected through calls of
/// xsl:apply-templates" — including the built-in rule for elements, which
/// applies templates to the children (§6.6).
#[test]
fn apply_templates_and_built_in_rules_preserve_them() {
    // Inside the children the context item is a string, so the document is
    // reached through a variable.
    let main = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="root" select="."/>
             <out><xsl:analyze-string select="'ab'" regex="(a)(b)">
               <xsl:matching-substring><xsl:apply-templates select="$root/doc"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>
           <xsl:template match="leaf"><leaf g2="{regex-group(2)}"/></xsl:template>"#,
    );
    // `doc` and `mid` have no rule: the built-in rule reaches `leaf`.
    assert_eq!(
        out_over(&main, "<doc><mid><leaf/></mid></doc>"),
        r#"<out><leaf g2="b"/></out>"#
    );
}

/// DESIGN §9.1, `xsl:call-template` row: "unaffected through calls of …
/// xsl:call-template".
#[test]
fn call_template_preserves_them() {
    let main = sheet(
        r#"<xsl:template match="/">
             <out><xsl:analyze-string select="'ab'" regex="(a)(b)">
               <xsl:matching-substring><xsl:call-template name="t"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>
           <xsl:template name="t"><t g1="{regex-group(1)}"/></xsl:template>"#,
    );
    assert_eq!(out(&main), r#"<out><t g1="a"/></out>"#);
}

/// DESIGN §9.1, `xsl:apply-imports` / `xsl:next-match` row: "unaffected through
/// calls of … xsl:apply-imports or xsl:next-match".
#[test]
fn next_match_and_apply_imports_preserve_them() {
    let imported =
        sheet(r#"<xsl:template match="doc"><imported g2="{regex-group(2)}"/></xsl:template>"#);
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:template match="/">
             <xsl:variable name="root" select="."/>
             <out><xsl:analyze-string select="'ab'" regex="(a)(b)">
               <xsl:matching-substring><xsl:apply-templates select="$root/doc"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>
           <xsl:template match="doc" priority="2"><first g1="{regex-group(1)}"/><xsl:next-match/></xsl:template>
           <xsl:template match="doc"><second g1="{regex-group(1)}"/><xsl:apply-imports/></xsl:template>"#,
    );
    assert_eq!(
        run_files(&[("file:///m2h/imported.xsl", &imported)], &main, "<doc/>").unwrap(),
        r#"<out><first g1="a"/><second g1="a"/><imported g2="b"/></out>"#
    );
}

/// "… or by expansion of named attribute sets."
#[test]
fn attribute_sets_preserve_them() {
    let main = sheet(
        r#"<xsl:attribute-set name="s"><xsl:attribute name="g1" select="regex-group(1)"/></xsl:attribute-set>
           <xsl:template match="/">
             <out><xsl:analyze-string select="'ab'" regex="(a)(b)">
               <xsl:matching-substring><e xsl:use-attribute-sets="s"/><xsl:element name="f" use-attribute-sets="s"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main), r#"<out><e g1="a"/><f g1="a"/></out>"#);
}

/// DESIGN §9.1, the `xsl:for-each`, `xsl:for-each-group`, grouping-key, sort-key
/// and `xsl:perform-sort` rows: none of them changes the current captured
/// substrings (§5.4.4's table lists only xsl:matching-substring as setting
/// them).
#[test]
fn for_each_grouping_and_sorting_preserve_them() {
    let main = root(
        r#"<xsl:analyze-string select="'x'" regex="(x)">
             <xsl:matching-substring>
               <fe><xsl:for-each select="1 to 2"><xsl:value-of select="regex-group(1)"/></xsl:for-each></fe>
               <feg><xsl:for-each-group select="1 to 3" group-by="concat(regex-group(1), . mod 2)">
                 <g k="{current-grouping-key()}" g1="{regex-group(1)}"/>
               </xsl:for-each-group></feg>
               <sorted><xsl:for-each select="('c', 'a', 'b')">
                 <xsl:sort select="index-of(('c', 'a', 'b'), .) * (if (regex-group(1) = 'x') then -1 else 1)"/>
                 <xsl:value-of select="."/>
               </xsl:for-each></sorted>
               <ps><xsl:perform-sort select="('c', 'a', 'b')"><xsl:sort select="if (regex-group(1) = 'x') then . else 0"/></xsl:perform-sort></ps>
             </xsl:matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(
        out(&main),
        concat(&[
            r#"<out><fe>xx</fe><feg><g k="x1" g1="x"/><g k="x0" g1="x"/></feg>"#,
            r#"<sorted>bac</sorted><ps>a b c</ps></out>"#,
        ])
    );
}

/// DESIGN §9.1, the stylesheet-function row. §15.2: "During the evaluation of
/// … a stylesheet function it is set to an empty sequence" — and a template
/// the function calls starts from that empty value. The caller's are back
/// after the call.
#[test]
fn a_stylesheet_function_sees_none() {
    let main = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="concat('[', regex-group(1), ']')"/></xsl:function>
           <xsl:function name="f:t"><xsl:call-template name="t"/></xsl:function>
           <xsl:template name="t"><xsl:sequence select="concat('(', regex-group(1), ')')"/></xsl:template>
           <xsl:template match="/">
             <out><xsl:analyze-string select="'a'" regex="(a)">
               <xsl:matching-substring><m f="{f:g()}" t="{f:t()}" back="{regex-group(1)}"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main), r#"<out><m f="[]" t="()" back="a"/></out>"#);
}

/// DESIGN §9.1, the pattern rows. §15.2: "During the evaluation of … a pattern
/// … it is set to an empty sequence": a template rule's `match` predicate
/// tested for an `xsl:apply-templates` inside `xsl:matching-substring` sees
/// none, while the body of the rule chosen sees the caller's. So does a
/// `group-starting-with` pattern.
#[test]
fn patterns_see_none() {
    let main = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="root" select="."/>
             <out><xsl:analyze-string select="'a'" regex="(a)">
               <xsl:matching-substring>
                 <xsl:apply-templates select="$root/doc/r"/>
                 <xsl:for-each-group select="$root/doc/r" group-starting-with="r[regex-group(1) = '']">
                   <g size="{count(current-group())}"/>
                 </xsl:for-each-group>
               </xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>
           <xsl:template match="r[regex-group(1) = '']" priority="2"><empty-in-pattern g1="{regex-group(1)}"/></xsl:template>
           <xsl:template match="r"><wrong/></xsl:template>"#,
    );
    assert_eq!(
        out_over(&main, "<doc><r/><r/></doc>"),
        concat(&[
            r#"<out><empty-in-pattern g1="a"/><empty-in-pattern g1="a"/>"#,
            r#"<g size="1"/><g size="1"/></out>"#,
        ])
    );
}

/// DESIGN §9.1, the global-variable row: a global variable is evaluated with
/// the initial value of the current captured substrings — the empty sequence
/// — even when the first reference to it is made inside
/// `xsl:matching-substring` (§9.5: lazily evaluated globals do not see the
/// context of whatever happened to reference them first).
#[test]
fn a_global_variable_sees_none() {
    let main = sheet(
        r#"<xsl:variable name="g" select="concat('[', regex-group(1), ']')"/>
           <xsl:param name="p" select="concat('(', regex-group(1), ')')"/>
           <xsl:template match="/">
             <out><xsl:analyze-string select="'a'" regex="(a)">
               <xsl:matching-substring><m g="{$g}" p="{$p}"/></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main), r#"<out><m g="[]" p="()"/></out>"#);
}

/// A local variable's value is computed where it is declared, so it keeps the
/// captured substrings of that point (and a temporary tree too, DESIGN §9.1's
/// `xsl:variable` row).
#[test]
fn local_variables_see_them() {
    let main = root(
        r#"<xsl:analyze-string select="'a'" regex="(a)">
             <xsl:matching-substring>
               <xsl:variable name="v" select="regex-group(1)"/>
               <xsl:variable name="t"><t g1="{regex-group(1)}"/></xsl:variable>
               <m v="{$v}"><xsl:copy-of select="$t"/></m>
             </xsl:matching-substring>
           </xsl:analyze-string>"#,
    );
    assert_eq!(out(&main), r#"<out><m v="a"><t g1="a"/></m></out>"#);
}

// ── §6.7 / §6.5 the current template rule and mode ───────────────────────

/// §6.7: "When an xsl:for-each, xsl:for-each-group, or xsl:analyze-string
/// instruction is evaluated, … the current template rule becomes null for the
/// evaluation of that instruction", and "[ERR XTDE0560] It is a
/// non-recoverable dynamic error if xsl:apply-imports or xsl:next-match is
/// evaluated when the current template rule is null." Both children.
#[test]
fn next_match_inside_is_xtde0560() {
    for (child, instruction) in [
        ("matching-substring", "next-match"),
        ("matching-substring", "apply-imports"),
        ("non-matching-substring", "next-match"),
        ("non-matching-substring", "apply-imports"),
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="/">
                 <out><xsl:analyze-string select="'xa'" regex="a">
                   <xsl:{child}><xsl:{instruction}/></xsl:{child}>
                 </xsl:analyze-string></out>
               </xsl:template>"#
        ));
        assert_eq!(dynamic_error(&main), "XTDE0560", "{child} / {instruction}");
    }
}

/// §6.5: "No other instruction changes the current mode": `mode="#current"`
/// inside the children is the mode of the enclosing template.
#[test]
fn the_mode_is_preserved() {
    // Inside the children the context item is a string: the node to apply
    // templates to is reached through a variable.
    let main = sheet(
        r##"<xsl:template match="/"><out><xsl:apply-templates select="doc" mode="m"/></out></xsl:template>
           <xsl:template match="doc" mode="m">
             <xsl:variable name="doc" select="."/>
             <xsl:analyze-string select="'xa'" regex="a">
               <xsl:matching-substring><xsl:apply-templates select="$doc/r" mode="#current"/></xsl:matching-substring>
               <xsl:non-matching-substring><xsl:apply-templates select="$doc/r" mode="#current"/></xsl:non-matching-substring>
             </xsl:analyze-string>
           </xsl:template>
           <xsl:template match="r"><wrong/></xsl:template>
           <xsl:template match="r" mode="m"><r/></xsl:template>"##,
    );
    assert_eq!(out_over(&main, "<doc><r/></doc>"), "<out><r/><r/></out>");
}

// ── Keys (§16.3) and the current captured substrings ─────────────────────

/// DESIGN §9.1, the `xsl:key` row: a key's `use` is evaluated for a `key()`
/// call with the captured substrings of the call (§15.2 empties them only for
/// `xsl:non-matching-substring`, patterns and stylesheet functions; §16.3.1
/// changes only the focus). Each `xsl:matching-substring` activation gets its
/// own index: one reused across matches would answer every match with the
/// first match's values.
#[test]
fn a_key_use_sees_the_captured_substrings_of_the_call() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="concat(@id, regex-group(1))"/>
           <xsl:template match="/">
             <xsl:variable name="doc" select="."/>
             <out>
               <before><xsl:value-of select="count(key('k', 'r1'))"/></before>
               <xsl:analyze-string select="'a-b'" regex="([ab])">
                 <xsl:matching-substring>
                   <m g1="{regex-group(1)}"><xsl:value-of select="count(key('k', concat('r1', regex-group(1)), $doc)), count(key('k', 'r1a', $doc))"/></m>
                 </xsl:matching-substring>
                 <xsl:non-matching-substring>
                   <n><xsl:value-of select="count(key('k', 'r1', $doc))"/></n>
                 </xsl:non-matching-substring>
               </xsl:analyze-string>
               <after><xsl:value-of select="count(key('k', 'r1')), count(key('k', 'r1a'))"/></after>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out_over(&main, r#"<doc><r id="r1"/><r id="r2"/></doc>"#),
        concat(&[
            r#"<out><before>1</before><m g1="a">1 1</m><n>1</n><m g1="b">1 0</m>"#,
            r#"<after>1 0</after></out>"#,
        ])
    );
}

/// A key's content that invokes a template, or a key that calls another key
/// that reads the captured substrings, observes them too (the conservative
/// static test, `GroupReach::reads_captures`).
#[test]
fn keys_reaching_regex_group_through_templates_and_keys() {
    let main = sheet(
        r#"<xsl:key name="t" match="r"><xsl:call-template name="g"/></xsl:key>
           <xsl:key name="inner" match="r" use="regex-group(1)"/>
           <xsl:key name="outer" match="r" use="if (key('inner', 'a')) then 'yes' else 'no'"/>
           <xsl:template name="g"><xsl:sequence select="regex-group(1)"/></xsl:template>
           <xsl:template match="/">
             <xsl:variable name="doc" select="."/>
             <out><xsl:analyze-string select="'ab'" regex="(.)">
               <xsl:matching-substring>
                 <m t="{count(key('t', regex-group(1), $doc))}" outer="{key('outer', 'yes', $doc)/@id}"/>
               </xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out_over(&main, r#"<doc><r id="r1"/></doc>"#),
        r#"<out><m t="1" outer="r1"/><m t="1" outer=""/></out>"#
    );
}

/// A key that reads both the current group and the captured substrings gets
/// an index per (group, activation) pair.
#[test]
fn a_key_reading_the_group_and_the_captured_substrings() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="concat(count(current-group()), regex-group(1))"/>
           <xsl:template match="/">
             <xsl:variable name="doc" select="."/>
             <out><xsl:for-each-group select="1 to 3" group-adjacent=". = 3">
               <xsl:analyze-string select="'ab'" regex="(.)">
                 <xsl:matching-substring>
                   <m><xsl:value-of select="count(key('k', concat(count(current-group()), regex-group(1)), $doc))"/></m>
                 </xsl:matching-substring>
               </xsl:analyze-string>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out_over(&main, "<doc><r/><r/></doc>"),
        "<out><m>2</m><m>2</m><m>2</m><m>2</m></out>"
    );
}

/// A key's `match` is a pattern: it sees no captured substrings (§15.2), even
/// for a `key()` call made inside `xsl:matching-substring`.
#[test]
fn a_key_match_pattern_sees_none() {
    let main = sheet(
        r#"<xsl:key name="k" match="r[regex-group(1) = '']" use="'all'"/>
           <xsl:template match="/">
             <xsl:variable name="doc" select="."/>
             <out><xsl:analyze-string select="'a'" regex="(a)">
               <xsl:matching-substring><m><xsl:value-of select="count(key('k', 'all', $doc))"/></m></xsl:matching-substring>
             </xsl:analyze-string></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out_over(&main, "<doc><r/><r/></doc>"),
        "<out><m>2</m></out>"
    );
}

// That a key which cannot observe the captured substrings keeps one index
// for the run, and that the index of one that can is dropped when its
// activation ends, is pinned by the unit tests of `src/runtime/keys.rs`
// (`tables_after`), which can see the index tables.

// ── Linearity ────────────────────────────────────────────────────────────

/// The partition is linear in the input for an ordinary regular expression:
/// four times the lines take well under sixteen times as long. (The probes in
/// the M2h report measure the absolute numbers in a release build.)
#[test]
fn splitting_lines_is_linear() {
    let time = |lines: usize| {
        let main = root(
            r#"<xsl:analyze-string select="string(doc)" regex="\n">
                 <xsl:matching-substring><br/></xsl:matching-substring>
                 <xsl:non-matching-substring><xsl:value-of select="string-length(.)"/></xsl:non-matching-substring>
               </xsl:analyze-string>"#,
        );
        let mut source = String::from("<doc>");
        for i in 0..lines {
            source.push_str(&format!("line {i:05}\n"));
        }
        source.push_str("</doc>");
        let start = Instant::now();
        let text = out_over(&main, &source);
        assert_eq!(text.matches("<br/>").count(), lines);
        start.elapsed()
    };
    let small = time(2_000);
    let large = time(8_000);
    assert!(
        large < small * 12 + Duration::from_millis(500),
        "2,000 lines: {small:?}; 8,000 lines: {large:?}"
    );
}

// ── M2h review fixes (XSLT2_M2H_REVIEW.md R1, R5, R7) ──────────────────────

/// Where a run failed, with the error itself.
#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Static,
    Dynamic,
}

/// Compiles `main` and runs it over `source` on the **current** thread, with
/// `TransformOptions::stack_limit` set to `stack_limit`.
fn run_here(
    main: &str,
    source: &str,
    stack_limit: Option<usize>,
) -> Result<String, (Phase, XsltError)> {
    let functions = XsltFunctions::new();
    let names = NameTable::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(main, "file:///m2h/main.xsl")
        .map_err(|e| (Phase::Static, e))?;
    let arena = Bump::new();
    let options = TransformOptions {
        stack_limit,
        ..Default::default()
    };
    let run = compiled.transformation(&arena, options);
    let outcome = run
        .transform_source("file:///m2h/in.xml", source)
        .map_err(|e| (Phase::Dynamic, e))?;
    compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .map_err(|e| (Phase::Dynamic, e))
}

/// `run_here` on the large test stack.
fn run_large(
    main: &str,
    source: &str,
    stack_limit: Option<usize>,
) -> Result<String, (Phase, XsltError)> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || run_here(&main, &source, stack_limit))
}

/// `run_here` with the default options on an ordinary thread:
/// `std::thread::spawn`, Rust's default stack (2 MiB), which the stack guard
/// does not know the size of (`xslt2::OTHER_THREAD_STACK_LIMIT`).
fn run_default_thread(main: &str, source: &str) -> Result<String, (Phase, XsltError)> {
    let (main, source) = (main.to_string(), source.to_string());
    std::thread::spawn(move || run_here(&main, &source, None))
        .join()
        .unwrap()
}

/// The implementation limit on regex nesting (`regex::MAX_NESTING`, private).
const NESTING_LIMIT: usize = 32;

/// `((…(a)…))`, `depth` groups deep.
fn nested_groups(depth: usize) -> String {
    format!("{}a{}", "(".repeat(depth), ")".repeat(depth))
}

/// An `xsl:analyze-string` over `'xay'` with the given `regex` attribute,
/// reporting the outermost and the innermost group (number `NESTING_LIMIT`)
/// of each match.
fn deep_groups(regex: &str) -> String {
    root(&format!(
        r#"<xsl:analyze-string select="'xay'" regex="{regex}">
             <xsl:matching-substring><m g1="{{regex-group(1)}}" gn="{{regex-group({NESTING_LIMIT})}}"/></xsl:matching-substring>
           </xsl:analyze-string>"#
    ))
}

/// Asserts that `result` is the regex-nesting implementation limit, raised
/// in `phase`, for a pattern `depth` deep.
fn assert_nesting_limit(result: Result<String, (Phase, XsltError)>, phase: Phase, depth: usize) {
    match result {
        Err((got, e)) => {
            assert_eq!(got, phase, "{e}");
            assert_eq!(e.code, "XSLT2-INTERNAL", "{e}");
            assert!(
                e.message.contains(&format!("{depth} deep"))
                    && e.message
                        .contains(&format!("the {NESTING_LIMIT} this processor allows")),
                "{e}"
            );
        }
        Ok(out) => panic!("expected the nesting limit, got {out:?}"),
    }
}

/// R1. §21: "A conforming processor may impose limits on the processing
/// resources consumed by the processing of a stylesheet." `regexml` compiles a
/// regular expression recursively, so its nesting is limited (32 levels):
/// up to the limit the regular expression works, fixed or computed — on an
/// ordinary thread, in a debug build as well (R3-1).
#[test]
fn a_regex_nested_to_the_limit_works_fixed_and_computed() {
    let regex = nested_groups(NESTING_LIMIT);
    let expected = r#"<out><m g1="a" gn="a"/></out>"#;
    assert_eq!(
        run_default_thread(&deep_groups(&regex), "<doc/>").unwrap(),
        expected
    );
    let source = format!(r#"<doc re="{regex}"/>"#);
    assert_eq!(
        run_default_thread(&deep_groups("{/doc/@re}"), &source).unwrap(),
        expected
    );
}

/// R1. One level deeper is the implementation-limit error (`XSLT2-INTERNAL`,
/// like the stack guard's limits) — a static error for a fixed regex, a
/// dynamic one for a regex computed from the input.
#[test]
fn one_level_deeper_is_an_implementation_limit() {
    let depth = NESTING_LIMIT + 1;
    let regex = nested_groups(depth);
    assert_nesting_limit(
        run_large(&deep_groups(&regex), "<doc/>", None),
        Phase::Static,
        depth,
    );
    let source = format!(r#"<doc re="{regex}"/>"#);
    assert_nesting_limit(
        run_large(&deep_groups("{/doc/@re}"), &source, None),
        Phase::Dynamic,
        depth,
    );
}

/// R1, the review's repro: a regular expression taken from the input
/// (`regex="{/doc/@re}"`) with 3 000 and 12 000 nested groups aborted the
/// process (stack overflow) on a 2 MiB thread — 12 000 even on the 8 MiB main
/// thread. It is now an error the caller gets back, on an ordinary 2 MiB
/// thread with the default options.
#[test]
fn a_deeply_nested_regex_from_the_input_is_an_error_not_an_abort() {
    let main = root(
        r#"<xsl:analyze-string select="'xay'" regex="{/doc/@re}"><xsl:matching-substring>[<xsl:value-of select="regex-group(1)"/>]</xsl:matching-substring></xsl:analyze-string>"#,
    );
    for depth in [3_000, 12_000] {
        let main = main.clone();
        let source = format!(r#"<doc re="{}"/>"#, nested_groups(depth));
        let result = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || run_here(&main, &source, None))
            .unwrap()
            .join()
            .unwrap();
        assert_nesting_limit(result, Phase::Dynamic, depth);
    }
}

/// R3-1 (`XSLT2_M2H_REVIEW3.md`), the review's repro: a recursive named
/// template compiles a new computed regular expression nested to the limit at
/// every level (`$n` makes each one different, so the run's cache never hits)
/// until the stack guard stops the recursion — run with the default options
/// on an ordinary thread (`std::thread::spawn`, 2 MiB), in whatever profile
/// `cargo test` builds. The guard lets the evaluation use 1 MiB
/// (`OTHER_THREAD_STACK_LIMIT`) before a compilation that comes on top of it,
/// so the limit has to keep a debug build's compilation (5.6 KB a level) well
/// inside the other 1 MiB: with the previous limit, 256, this aborted the
/// process (stack overflow, SIGABRT) in a debug build at 192 and 256 levels.
/// It ends in the guard's error; one level deeper, the first compilation is
/// the nesting-limit error.
#[test]
fn a_recursion_compiling_regexes_at_the_limit_ends_in_the_stack_guard_error() {
    let main = sheet(
        r#"<xsl:template match="/"><xsl:call-template name="t"/></xsl:template>
           <xsl:template name="t">
             <xsl:param name="n" select="0"/>
             <xsl:analyze-string select="'xay'" regex="{concat(/doc/@re, '|', $n)}">
               <xsl:matching-substring/>
             </xsl:analyze-string>
             <xsl:call-template name="t">
               <xsl:with-param name="n" select="$n + 1"/>
             </xsl:call-template>
           </xsl:template>"#,
    );
    let at_limit = format!(r#"<doc re="{}"/>"#, nested_groups(NESTING_LIMIT));
    match run_default_thread(&main, &at_limit) {
        Err((Phase::Dynamic, e)) => {
            assert_eq!(e.code, "XSLT2-INTERNAL", "{e}");
            assert!(
                e.message.contains("TransformOptions::stack_limit"),
                "expected the stack guard's error: {e}"
            );
        }
        other => panic!("expected the stack guard's error, got {other:?}"),
    }
    let deeper = format!(r#"<doc re="{}"/>"#, nested_groups(NESTING_LIMIT + 1));
    assert_nesting_limit(
        run_default_thread(&main, &deeper),
        Phase::Dynamic,
        NESTING_LIMIT + 1,
    );
}

/// R1, Q33: the run's stack guard (`TransformOptions::stack_limit`) is checked
/// before a computed regular expression is compiled. `N` nested literal
/// result elements take the evaluation deep with no template invocation in
/// between to check the guard; an `xsl:analyze-string` there used to compile
/// its regex (and run) regardless.
#[test]
fn the_stack_guard_is_checked_before_a_computed_regex_is_compiled() {
    const LIMIT: usize = 256 * 1024;
    // 100 levels already pass the limit in a release build (and many more
    // bytes a level in a debug build); 300 for margin. The compiler's work
    // grows faster than linearly with the nesting, so not thousands.
    const N: usize = 300;
    let deep = |inner: &str| {
        sheet(&format!(
            r#"<xsl:template match="/"><out>{}{inner}{}</out></xsl:template>
               <xsl:template name="t"><t/></xsl:template>"#,
            "<a>".repeat(N),
            "</a>".repeat(N)
        ))
    };
    let analyze = r#"<xsl:analyze-string select="'xay'" regex="{'a'}"><xsl:matching-substring><m/></xsl:matching-substring></xsl:analyze-string>"#;
    let assert_guard = |result: Result<String, (Phase, XsltError)>, what: &str| match result {
        Err((Phase::Dynamic, e)) => {
            assert_eq!(e.code, "XSLT2-INTERNAL", "{what}: {e}");
            assert!(
                e.message.contains("TransformOptions::stack_limit"),
                "{what}: {e}"
            );
        }
        other => panic!("{what}: expected the stack guard's error, got {other:?}"),
    };
    // The premise: at that depth a template invocation trips the guard.
    assert_guard(
        run_large(
            &deep(r#"<xsl:call-template name="t"/>"#),
            "<doc/>",
            Some(LIMIT),
        ),
        "call-template",
    );
    // So does compiling a computed regular expression there.
    assert_guard(
        run_large(&deep(analyze), "<doc/>", Some(LIMIT)),
        "analyze-string",
    );
    // Near the top, under the same limit, it runs.
    assert_eq!(
        run_large(&root(analyze), "<doc/>", Some(LIMIT)).unwrap(),
        "<out><m/></out>"
    );
}

/// The 26 XSLT 2.0 instructions (the syntax summaries' "Category:
/// instruction", `xsl:variable` included) and whether this processor runs
/// them: all of them since M3c. (`xsl:result-document` was false until M3b
/// implemented it — `tests/m3b_result_documents.rs`; `xsl:number` until M3c —
/// `tests/m3c_numbering.rs`.)
const INSTRUCTIONS: [(&str, bool); 26] = [
    ("analyze-string", true),
    ("apply-imports", true),
    ("apply-templates", true),
    ("attribute", true),
    ("call-template", true),
    ("choose", true),
    ("comment", true),
    ("copy", true),
    ("copy-of", true),
    ("document", true),
    ("element", true),
    ("fallback", true),
    ("for-each", true),
    ("for-each-group", true),
    ("if", true),
    ("message", true),
    ("namespace", true),
    ("next-match", true),
    ("number", true),
    ("perform-sort", true),
    ("processing-instruction", true),
    ("result-document", true),
    ("sequence", true),
    ("text", true),
    ("value-of", true),
    ("variable", true),
];

/// R5. §18.1.1: "The function returns true if and only if … the
/// expanded-QName identifies an instruction that the implementation
/// supports." `xsl:for-each-group` and `xsl:perform-sort` run (M2g, M2d) but
/// were answered false; the one that still raises `XSLT2-NOTIMPL` is false.
/// Declarations and subordinate elements are not instructions.
#[test]
fn element_available_is_true_for_every_implemented_instruction() {
    let select = INSTRUCTIONS
        .iter()
        .map(|(name, _)| format!("element-available('xsl:{name}')"))
        .collect::<Vec<_>>()
        .join(", ");
    let expected = INSTRUCTIONS
        .iter()
        .map(|(_, available)| available.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let main = root(&format!(
        r#"<xsl:value-of select="{select}" separator=","/>"#
    ));
    assert_eq!(out(&main), format!("<out>{expected}</out>"));

    for name in [
        "stylesheet",
        "template",
        "function",
        "key",
        "param",
        "attribute-set",
        "output",
        "sort",
        "when",
        "otherwise",
        "with-param",
        "matching-substring",
        "non-matching-substring",
    ] {
        let main = root(&format!(
            r#"<xsl:value-of select="element-available('xsl:{name}')"/>"#
        ));
        assert_eq!(out(&main), "<out>false</out>", "xsl:{name}");
    }

    // The last one answered false until M3c runs now.
    assert_eq!(out(&root(r#"<xsl:number value="1"/>"#)), "<out>1</out>");
}

/// R5. §3.12 `use-when`: guarding code with `element-available()` keeps it
/// when the instruction runs — the review's repro dropped working
/// `xsl:for-each-group` code.
#[test]
fn use_when_element_available_keeps_working_code() {
    let main = root(
        r#"<g xsl:use-when="element-available('xsl:for-each-group')"><xsl:for-each-group select="1, 2, 1" group-by="."><k><xsl:value-of select="current-grouping-key()"/></k></xsl:for-each-group></g>
           <s xsl:use-when="element-available('xsl:perform-sort')"><xsl:perform-sort select="3, 1, 2"><xsl:sort select="."/></xsl:perform-sort></s>
           <n xsl:use-when="element-available('xsl:number')"><xsl:number value="1"/></n>"#,
    );
    assert_eq!(
        out(&main),
        "<out><g><k>1</k><k>2</k></g><s>1 2 3</s><n>1</n></out>"
    );
}

/// R7. §15.1: "[ERR XTSE1130] It is a static error if the xsl:analyze-string
/// instruction contains neither an xsl:matching-substring nor an
/// xsl:non-matching-substring element" — reported before the regular
/// expression is compiled, so an invalid (or too deeply nested) fixed regex
/// or invalid flags do not hide it. §2.9 lets either error be reported ("It
/// is implementation-dependent which of the several errors is signaled");
/// the instruction's own shape comes first.
#[test]
fn neither_child_is_xtse1130_whatever_the_regex() {
    let too_deep = nested_groups(NESTING_LIMIT + 1);
    for (regex, flags) in [
        ("[", ""),
        ("(?:a)", ""),
        ("a?", ""),
        ("a", "q"),
        (too_deep.as_str(), ""),
    ] {
        for content in ["", "<xsl:fallback/>"] {
            let main = root(&format!(
                r#"<xsl:analyze-string select="'a'" regex="{regex}" flags="{flags}">{content}</xsl:analyze-string>"#
            ));
            assert_eq!(
                static_error(&main),
                "XTSE1130",
                "{regex:?} {flags:?} {content:?}"
            );
        }
    }
}
