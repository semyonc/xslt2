//! `xsl:number` (XSLT 2.0 §12): the place marker from `value` (§12.1)
//! or from a node's position (§12.2, every `level`, with and without `count`
//! and `from`), its conversion to a string (§12.3), the backwards-compatible
//! behaviour of `value` (§12.1), and the four error codes
//! of the section (XTSE0975, XTDE0980, XTTE0990, XTTE1000) plus the permitted
//! values of the conversion attributes (XTSE0020 / XTDE0030).
//!
//! Every test goes through the public API — compile, transform,
//! `serialize_primary` — and cites the rule it checks.

mod common;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, QName, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn sheet_versioned(version: &str, body: &str) -> String {
    format!("<xsl:stylesheet version=\"{version}\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>")
}

fn sheet(body: &str) -> String {
    sheet_versioned("2.0", body)
}

/// `content` inside `<out>` as the template matching `/`.
fn root(content: &str) -> String {
    sheet(&format!(
        r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
    ))
}

/// Where a run failed: at compile time or at run time, with the code.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Static(String),
    Dynamic(String),
}

fn run_over(text: &str, source: &str) -> Result<String, Failure> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, "urn:numbering:sheet.xsl")
        .map_err(|e| Failure::Static(e.code))?;
    let arena = bumpalo::Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run
        .transform_source("urn:numbering:in.xml", source)
        .map_err(|e| Failure::Dynamic(e.code))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| Failure::Dynamic(e.code))?;
    Ok(common::body(text))
}

fn out_over(text: &str, source: &str) -> String {
    run_over(text, source).unwrap_or_else(|f| panic!("{f:?}"))
}

fn out(text: &str) -> String {
    out_over(text, "<doc/>")
}

/// The text of the one `<out>` element (`out` without the wrapper).
fn inner(text: &str) -> String {
    let all = out(text);
    all.strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .map(str::to_string)
        .unwrap_or_else(|| if all == "<out/>" { String::new() } else { all })
}

/// `<xsl:number …/>` with `attributes`, formatted alone.
fn number(attributes: &str) -> String {
    inner(&root(&format!("<xsl:number {attributes}/>")))
}

fn static_error(text: &str) -> String {
    match run_over(text, "<doc/>") {
        Err(Failure::Static(code)) => code,
        other => panic!("expected a static error, got {other:?}"),
    }
}

fn dynamic_error_over(text: &str, source: &str) -> String {
    match run_over(text, source) {
        Err(Failure::Dynamic(code)) => code,
        other => panic!("expected a dynamic error, got {other:?}"),
    }
}

fn dynamic_error(text: &str) -> String {
    dynamic_error_over(text, "<doc/>")
}

// ═══ §12 the instruction ═════════════════════════════════════════════════

/// §18.1.1: `element-available` "returns true if and only if … the
/// expanded-QName identifies an instruction that the implementation supports".
#[test]
fn element_available_is_true_for_xsl_number() {
    assert_eq!(
        inner(&root(
            r#"<xsl:value-of select="element-available('xsl:number')"/>"#
        )),
        "true"
    );
}

/// §12: "The result of the instruction is a newly constructed text node
/// containing the formatted number as its string value."
#[test]
fn the_result_is_one_text_node() {
    let text = root(
        r#"<xsl:variable name="n" as="node()*"><xsl:number value="7"/></xsl:variable>
           <xsl:value-of select="count($n), $n instance of text(), string($n)" separator="|"/>"#,
    );
    assert_eq!(inner(&text), "1|true|7");
}

// ═══ §12.1 a supplied number ═════════════════════════════════════════════

/// §12.1: "each value $V in the atomized sequence is then converted to the
/// integer value returned by the XPath expression
/// `xs:integer(round(number($V)))`" — round half towards positive infinity,
/// `number()` of a string, a node atomized.
#[test]
fn value_is_atomized_rounded_and_converted() {
    assert_eq!(number(r#"value="2.5""#), "3");
    assert_eq!(number(r#"value="2.4999""#), "2");
    assert_eq!(number(r#"value="-0.4""#), "0");
    assert_eq!(number(r#"value="-0.5""#), "0");
    assert_eq!(number(r#"value="'  12 '""#), "12");
    assert_eq!(number(r#"value="true()""#), "1");
    assert_eq!(
        number(r#"value="xs:decimal('6.5')" xmlns:xs="http://www.w3.org/2001/XMLSchema""#),
        "7"
    );
    assert_eq!(
        out_over(
            &root(r#"<xsl:number value="doc/@n"/>"#),
            r#"<doc n="41.6"/>"#
        ),
        "<out>42</out>"
    );
    // The spec's example: "Numbering a Sorted List".
    let sorted = sheet(
        r#"<xsl:template match="items"><out><xsl:for-each select="item"><xsl:sort select="."/><p><xsl:number value="position()" format="1. "/><xsl:value-of select="."/></p></xsl:for-each></out></xsl:template>"#,
    );
    assert_eq!(
        out_over(
            &sorted,
            "<items><item>c</item><item>a</item><item>b</item></items>"
        ),
        "<out><p>1. a</p><p>2. b</p><p>3. c</p></out>"
    );
}

/// §12.1: "The resulting sequence of integers is used as the place marker to
/// be formatted" — every item, not only the first; an empty sequence is an
/// empty place marker, which keeps the prefix and the suffix (§12.3).
#[test]
fn every_item_of_value_is_formatted() {
    assert_eq!(number(r#"value="1 to 5""#), "1.2.3.4.5");
    assert_eq!(number(r#"value="(3, 1, 2)" format="A-1""#), "C-1-2");
    assert_eq!(number(r#"value="()""#), "");
    assert_eq!(number(r#"value="()" format="[1]""#), "[]");
}

/// §12.1: "\[ERR XTDE0980\] It is a non-recoverable dynamic error if any
/// undiscarded item in the atomized sequence supplied as the value of the
/// value attribute of xsl:number cannot be converted to an integer, or if the
/// resulting integer is less than 0 (zero)."
#[test]
fn xtde0980_for_a_value_that_is_not_a_non_negative_integer() {
    for value in [
        "-1",
        "-0.6",
        "'fizz'",
        "number('NaN')",
        "1e0 div 0",
        "-1e0 div 0",
        "(1, 'x')",
        "(2, -3)",
    ] {
        assert_eq!(
            dynamic_error(&root(&format!(r#"<xsl:number value="{value}"/>"#))),
            "XTDE0980",
            "{value}"
        );
    }
    // Zero is allowed: "The value zero does not arise when numbering nodes in a
    // source document, but it can arise in other numbering sequences".
    assert_eq!(number(r#"value="0""#), "0");
}

/// §12.1, "If backwards compatible behavior is enabled for the instruction":
/// "all items in the atomized sequence after the first are discarded; If the
/// atomized sequence is empty, it is replaced by a sequence containing the
/// xs:double value NaN as its only item; If any value in the sequence cannot
/// be converted to an integer (this includes the case where the sequence
/// contains a NaN value) then the string NaN is inserted into the formatted
/// result string in its proper position. The error described in the following
/// paragraph does not apply in this case."
#[test]
fn backwards_compatible_value() {
    let bc = |content: &str| {
        inner(&sheet_versioned(
            "1.0",
            &format!(r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#),
        ))
    };
    assert_eq!(bc(r#"<xsl:number value="1 to 5"/>"#), "1");
    assert_eq!(bc(r#"<xsl:number value="()"/>"#), "NaN");
    assert_eq!(bc(r#"<xsl:number value="('apples', 'oranges')"/>"#), "NaN");
    assert_eq!(bc(r#"<xsl:number value="0e0 div 0"/>"#), "NaN");
    assert_eq!(bc(r#"<xsl:number value="'x'" format="(1)"/>"#), "(NaN)");
    assert_eq!(bc(r#"<xsl:number value="(2.5, 'x')" format="i"/>"#), "iii");
    // The exemption covers a value that cannot be converted; a negative
    // number can be, and its integer is less than zero.
    assert_eq!(
        dynamic_error(&sheet_versioned(
            "1.0",
            r#"<xsl:template match="/"><out><xsl:number value="-2"/></out></xsl:template>"#
        )),
        "XTDE0980"
    );
    // The behaviour is the instruction's own: an xsl:version="2.0" region
    // inside a 1.0 stylesheet formats every item (backwards-011).
    let mixed = sheet_versioned(
        "1.0",
        r#"<xsl:template match="/"><out><one><xsl:number value="1 to 3"/></one><two xsl:version="2.0"><xsl:number value="1 to 3"/></two></out></xsl:template>"#,
    );
    assert_eq!(out(&mixed), "<out><one>1</one><two>1.2.3</two></out>");
}

/// §12: "\[ERR XTSE0975\] It is a static error if the value attribute of
/// xsl:number is present unless the select, level, count, and from attributes
/// are all absent."
#[test]
fn xtse0975_value_with_select_level_count_or_from() {
    for other in [
        r#"select=".""#,
        r#"level="any""#,
        r#"count="a""#,
        r#"from="*""#,
    ] {
        assert_eq!(
            static_error(&root(&format!(r#"<xsl:number value="3" {other}/>"#))),
            "XTSE0975",
            "{other}"
        );
    }
    // A static error even where the instruction is never evaluated.
    let never = sheet(
        r#"<xsl:template match="/"><out/></xsl:template><xsl:template name="t"><xsl:number value="1" level="single"/></xsl:template>"#,
    );
    assert_eq!(static_error(&never), "XTSE0975");
}

// ═══ §12.2 the selected node ═════════════════════════════════════════════

/// §12.2: "\[ERR XTTE0990\] It is a type error if the xsl:number instruction
/// is evaluated, with no value or select attribute, when the context item is
/// not a node." An absent context item — in a stylesheet function, where "the
/// focus is initially undefined" (§10.3) — is the same error (`number-0824`).
#[test]
fn xtte0990_when_the_context_item_is_not_a_node() {
    assert_eq!(
        dynamic_error(&root(
            r#"<xsl:for-each select="1 to 2"><xsl:number/></xsl:for-each>"#
        )),
        "XTTE0990"
    );
    let function = sheet(
        r#"<xsl:function name="f:n" xmlns:f="urn:f"><xsl:number/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:n()" xmlns:f="urn:f"/></out></xsl:template>"#,
    );
    assert_eq!(dynamic_error(&function), "XTTE0990");
    // With a select attribute the context item does not matter.
    assert_eq!(
        inner(&root(
            r#"<xsl:variable name="d" select="/"/><xsl:for-each select="1 to 2"><xsl:number select="$d/doc"/></xsl:for-each>"#
        )),
        "11"
    );
}

/// §12.2: "\[ERR XTTE1000\] It is a type error if the result of evaluating the
/// select attribute of the xsl:number instruction is anything other than a
/// single node."
#[test]
fn xtte1000_when_select_is_not_one_node() {
    for select in ["()", "17", "'a'", "doc/e"] {
        assert_eq!(
            dynamic_error_over(
                &root(&format!(r#"<xsl:number select="{select}"/>"#)),
                "<doc><e/><e/></doc>"
            ),
            "XTTE1000",
            "{select}"
        );
    }
    assert_eq!(
        out_over(
            &root(r#"<xsl:number select="doc/e[2]"/>"#),
            "<doc><e/><e/></doc>"
        ),
        "<out>2</out>"
    );
}

const BOOK: &str = "<book>\
    <chapter><title>c1</title><section><title>s11</title><note/><subsection><title>ss111</title><note/></subsection><subsection><title>ss112</title></subsection></section><section><title>s12</title><note/></section></chapter>\
    <chapter><title>c2</title><note/><section><title>s21</title><note/></section></chapter>\
    <appendix><title>a1</title><section><title>as11</title><subsection><title>ass111</title><note/></subsection></section></appendix>\
    <appendix><title>a2</title><section><title>as21</title></section><section><title>as22</title><subsection><title>ass221</title></subsection></section></appendix>\
    </book>";

/// §12.2, "Example: Multi-Level Numbering": "Chapters are numbered 1, 2, 3;
/// appendices are numbered A, B, C; sections in chapters are numbered 1.1,
/// 1.2, 1.3; sections in appendices are numbered A.1, A.2, A.3. Subsections
/// within a chapter are numbered 1.1.1, 1.1.2, 1.1.3; subsections within an
/// appendix are numbered A.1.1, A.1.2, A.1.3." — the spec's two template
/// rules, verbatim but for `fo:block`.
#[test]
fn the_multi_level_example() {
    let text = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//title"/></out></xsl:template>
           <xsl:template match="title"><b><xsl:number level="multiple" count="chapter|section|subsection" format="1.1 "/><xsl:apply-templates/></b></xsl:template>
           <xsl:template match="appendix//title" priority="1"><b><xsl:number level="multiple" count="appendix|section|subsection" format="A.1 "/><xsl:apply-templates/></b></xsl:template>"#,
    );
    assert_eq!(
        out_over(&text, BOOK),
        "<out><b>1 c1</b><b>1.1 s11</b><b>1.1.1 ss111</b><b>1.1.2 ss112</b><b>1.2 s12</b>\
         <b>2 c2</b><b>2.1 s21</b>\
         <b>A a1</b><b>A.1 as11</b><b>A.1.1 ass111</b>\
         <b>B a2</b><b>B.1 as21</b><b>B.2 as22</b><b>B.2.1 ass221</b></out>"
    );
}

/// §12.2, "Example: Numbering Notes within a Chapter": "This example numbers
/// notes sequentially within a chapter" — `level="any" from="chapter"`. $F is
/// "the last node in document order that matches the from pattern and that
/// precedes the selected node", which for a note in an appendix is the last
/// chapter, not an ancestor: it goes on counting from there.
#[test]
fn the_notes_example() {
    let text = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//note"/></out></xsl:template>
           <xsl:template match="note"><b><xsl:number level="any" from="chapter" format="(1) "/></b></xsl:template>"#,
    );
    assert_eq!(
        out_over(&text, BOOK),
        "<out><b>(1) </b><b>(2) </b><b>(3) </b><b>(1) </b><b>(2) </b><b>(3) </b></out>"
    );
}

/// §12.2, "Example: Numbering the Items in an Ordered List": `<xsl:number/>`
/// in a rule for `ol/item` — level `single`, the implied count pattern.
#[test]
fn the_ordered_list_example() {
    let text = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="ol/item"><b><xsl:number/><xsl:text>. </xsl:text><xsl:apply-templates/></b></xsl:template>"#,
    );
    assert_eq!(
        out_over(
            &text,
            "<doc><ol><item>a</item><x/><item>b</item></ol><ol><item>c</item></ol></doc>"
        ),
        "<out><b>1. a</b><b>2. b</b><b>1. c</b></out>"
    );
}

/// §12.2 `level="single"`: `$A` is the innermost ancestor-or-self matching
/// `count`, `$F` the innermost matching `from`, and `$AF` is "$A if it is in
/// the subtree rooted at $F, or the empty sequence otherwise".
#[test]
fn level_single_with_count_and_from() {
    let source = "<doc><s><p><i/></p><p><i/><i/></p></s><s><x><p><i/></p></x></s></doc>";
    let each = |number: &str| {
        out_over(
            &root(&format!(
                r#"<xsl:for-each select="//i"><n><xsl:number {number}/></n></xsl:for-each>"#
            )),
            source,
        )
    };
    // Implied count: the i elements among their siblings.
    assert_eq!(each(""), "<out><n>1</n><n>1</n><n>2</n><n>1</n></out>");
    // count="p": the enclosing p among its p siblings.
    assert_eq!(
        each(r#"count="p""#),
        "<out><n>1</n><n>2</n><n>2</n><n>1</n></out>"
    );
    // from="x": the last i's $A (its p) is inside $F (x): numbered.
    assert_eq!(
        each(r#"count="p" from="x""#),
        "<out><n>1</n><n>2</n><n>2</n><n>1</n></out>"
    );
    // count="s" from="x": the last i's $A (the second s) is above $F (x), so
    // not in its subtree: empty.
    assert_eq!(
        each(r#"count="s" from="x""#),
        "<out><n>1</n><n>1</n><n>1</n><n/></out>"
    );
    // from="p" is below every s: all empty.
    assert_eq!(each(r#"count="s" from="p""#), "<out><n/><n/><n/><n/></out>");
    // from matching $A itself keeps it ($A is in the subtree rooted at $F).
    assert_eq!(
        each(r#"count="p" from="p""#),
        "<out><n>1</n><n>2</n><n>2</n><n>1</n></out>"
    );
    // No ancestor-or-self matches: empty.
    assert_eq!(each(r#"count="q""#), "<out><n/><n/><n/><n/></out>");
}

/// §12.2 `level="multiple"`: "the nodes selected in the first step that are
/// in the subtree rooted at the node selected in the second step".
#[test]
fn level_multiple_with_count_and_from() {
    let source = "<doc><s><p><i/></p><p><i/><i/></p></s><s><x><p><i/></p></x></s></doc>";
    let each = |number: &str| {
        out_over(
            &root(&format!(
                r#"<xsl:for-each select="//i"><n><xsl:number level="multiple" {number}/></n></xsl:for-each>"#
            )),
            source,
        )
    };
    assert_eq!(
        each(r#"count="s|p|i""#),
        "<out><n>1.1.1</n><n>1.2.1</n><n>1.2.2</n><n>2.1.1</n></out>"
    );
    assert_eq!(
        each(r#"count="s|p|i" from="x""#),
        "<out><n>1.1.1</n><n>1.2.1</n><n>1.2.2</n><n>1.1</n></out>"
    );
    // from matching a counted node keeps it.
    assert_eq!(
        each(r#"count="s|p|i" from="s""#),
        "<out><n>1.1.1</n><n>1.2.1</n><n>1.2.2</n><n>2.1.1</n></out>"
    );
    // Implied count: only nodes named like the selected one.
    assert_eq!(each(""), "<out><n>1</n><n>1</n><n>2</n><n>1</n></out>");
    assert_eq!(
        out_over(
            &root(
                r#"<xsl:for-each select="//a"><n><xsl:number level="multiple"/></n></xsl:for-each>"#
            ),
            "<doc><a><a/><b/><a><a/></a></a></doc>"
        ),
        "<out><n>1</n><n>1.1</n><n>1.2</n><n>1.2.1</n></out>"
    );
}

/// §12.2 `level="any"`: `$S/(preceding::node()|ancestor-or-self::node())
/// [matches-count(.)]` from the last node matching `from` — ancestors and
/// preceding nodes alike, attributes and namespace nodes excluded.
#[test]
fn level_any_with_count_and_from() {
    let source = r#"<doc><h/><p a="1"/><d><p a="2"><p/></p><h/><p/></d><p/></doc>"#;
    let each = |number: &str| {
        out_over(
            &root(&format!(
                r#"<xsl:for-each select="//p"><n><xsl:number level="any" {number}/></n></xsl:for-each>"#
            )),
            source,
        )
    };
    assert_eq!(
        each(""),
        "<out><n>1</n><n>2</n><n>3</n><n>4</n><n>5</n></out>"
    );
    assert_eq!(
        each(r#"from="h""#),
        "<out><n>1</n><n>2</n><n>3</n><n>1</n><n>2</n></out>"
    );
    // A from-match that is itself counted is counted ("$A[. is $F or . >> $F]").
    assert_eq!(
        each(r#"count="p|h" from="h""#),
        "<out><n>2</n><n>3</n><n>4</n><n>2</n><n>3</n></out>"
    );
    // The attributes of earlier elements are not counted; the selected
    // attribute is ("the selected node together with all nodes, other than
    // attributes and namespaces, that precede the selected node").
    assert_eq!(
        out_over(
            &root(
                r#"<xsl:for-each select="//@a"><n><xsl:number level="any" count="p|@a"/></n></xsl:for-each>"#
            ),
            source
        ),
        "<out><n>2</n><n>3</n></out>"
    );
    // Nothing counted: empty.
    assert_eq!(each(r#"count="q""#), "<out><n/><n/><n/><n/><n/></out>");
}

/// §12.2: the implied count pattern "matches any node with the same node kind
/// as the selected node and, if the selected node has an expanded-QName, with
/// the same expanded-QName as the selected node" — for every node kind.
#[test]
fn the_implied_count_pattern_for_each_node_kind() {
    let source = r#"<doc xmlns:q="urn:q"><e/><q:e/><e/>t1<!--c1--><?a x?><f/>t2<!--c2--><?b y?><?a z?><g at="1" bt="2"/></doc>"#;
    let numbers = |select: &str| {
        out_over(
            &root(&format!(
                r#"<xsl:for-each select="{select}"><n><xsl:number/></n></xsl:for-each>"#
            )),
            source,
        )
    };
    // Elements: the same expanded name only (q:e is not e).
    assert_eq!(numbers("doc/*:e"), "<out><n>1</n><n>1</n><n>2</n></out>");
    // Text nodes: every text sibling.
    assert_eq!(numbers("doc/text()"), "<out><n>1</n><n>2</n></out>");
    // Comments: every comment sibling.
    assert_eq!(numbers("doc/comment()"), "<out><n>1</n><n>2</n></out>");
    // Processing instructions: the same target only.
    assert_eq!(
        numbers("doc/processing-instruction()"),
        "<out><n>1</n><n>1</n><n>2</n></out>"
    );
    // Attributes: no siblings, so always 1.
    assert_eq!(numbers("doc/g/@*"), "<out><n>1</n><n>1</n></out>");
    // The document node: the root of its tree.
    assert_eq!(numbers("/"), "<out><n>1</n></out>");
    // A namespace node: its parent element has no namespace siblings.
    assert_eq!(numbers("doc/namespace::q"), "<out><n>1</n></out>");
    // level="any" with the implied pattern counts nodes of the same kind and
    // name anywhere before.
    assert_eq!(
        out_over(
            &root(
                r#"<xsl:for-each select="//processing-instruction()"><n><xsl:number level="any"/></n></xsl:for-each>"#
            ),
            source
        ),
        "<out><n>1</n><n>1</n><n>2</n></out>"
    );
}

/// §12.2 matches-from: "or if $node is the root node of a tree" — a tree whose
/// root is an element (a parentless element built with `as="element()"`).
#[test]
fn a_tree_rooted_at_an_element_is_numbered_from_that_root() {
    let text = root(
        r#"<xsl:variable name="t" as="element()"><r><a/><b><a/><a/></b></r></xsl:variable>
           <xsl:for-each select="$t//a"><n><xsl:number level="any"/>/<xsl:number level="multiple" count="*"/></n></xsl:for-each>"#,
    );
    assert_eq!(inner(&text), "<n>1/1.1</n><n>2/1.2.1</n><n>3/1.2.2</n>");
}

/// §5.5.4: an error while matching a pattern is recoverable, "The optional
/// recovery action is to treat the pattern as not matching"; a `count`
/// pattern sees the local variables in scope (§9.7), and `current()` in it is
/// the node being matched (§16.6.1).
#[test]
fn count_and_from_are_patterns_like_any_other() {
    let source = r#"<doc><p k="a"/><p k="b"/><p k="a"/><p k="a"/></doc>"#;
    let text = root(
        r#"<xsl:variable name="key" select="'a'"/><xsl:for-each select="doc/p"><n><xsl:number count="p[@k = $key]"/></n></xsl:for-each>"#,
    );
    assert_eq!(
        out_over(&text, source),
        "<out><n>1</n><n/><n>2</n><n>3</n></out>"
    );
    let current = root(
        r#"<xsl:for-each select="doc/p"><n><xsl:number level="any" count="p[@k = current()/@k]"/></n></xsl:for-each>"#,
    );
    // current() is each candidate, so every p matches.
    assert_eq!(
        out_over(&current, source),
        "<out><n>1</n><n>2</n><n>3</n><n>4</n></out>"
    );
    // A predicate that raises an error does not match.
    let failing = root(
        r#"<xsl:for-each select="doc/p"><n><xsl:number count="p[xs:integer(@k) = 1]" xmlns:xs="http://www.w3.org/2001/XMLSchema"/></n></xsl:for-each>"#,
    );
    assert_eq!(out_over(&failing, source), "<out><n/><n/><n/><n/></out>");
    // XTSE0340: not a pattern.
    assert_eq!(
        static_error(&root(r#"<xsl:number count="2+2"/>"#)),
        "XTSE0340"
    );
    assert_eq!(
        static_error(&root(r#"<xsl:number from="$x"/>"#)),
        "XTSE0340"
    );
}

/// §2.2 / XTSE0020: `level` is not an attribute value template, and its value
/// must be one of the three.
#[test]
fn level_must_be_single_multiple_or_any() {
    assert_eq!(
        static_error(&root(r#"<xsl:number level="all"/>"#)),
        "XTSE0020"
    );
    assert_eq!(
        static_error(&root(r#"<xsl:number level="{'any'}"/>"#)),
        "XTSE0020"
    );
    // "leading and trailing whitespace in the attribute value is ignored".
    assert_eq!(
        out_over(
            &root(r#"<xsl:for-each select="//e"><xsl:number level=" any "/></xsl:for-each>"#),
            "<d><e/><f><e/></f></d>"
        ),
        "<out>12</out>"
    );
}

/// Every node numbered with every level, with and without `count` / `from`,
/// against the spec's own expressions (§12.2) evaluated in XPath — with the
/// patterns written as predicates — for elements, attributes, text, comments
/// and processing instructions. The `[$yes]` variants read a local variable,
/// so the instruction cannot reuse what an earlier evaluation counted (see
/// `runtime/number.rs`, `NumberMemo`); the others can. Both orders of
/// evaluation (document order and its reverse) are checked.
#[test]
fn every_level_agrees_with_the_spec_expressions() {
    let mut source = String::from(r#"<doc a="0">"#);
    for i in 0..6 {
        source.push_str(&format!(
            r#"<ch n="{i}"><t>x</t><!--c--><s a="2"><p>y</p><?pi a?><p b="3">z<note/></p></s><note/><s><note/><p/></s></ch><note/>text<p/>"#
        ));
    }
    source.push_str("</doc>");
    // (name, count pattern, from pattern, count predicate, from predicate)
    let cases = [
        ("p|note", "ch", "self::p or self::note", "self::ch"),
        ("*", "s", "self::*", "self::s"),
        // §5.5.3: "the pattern node(), when used without an explicit axis, does
        // not match document nodes, attribute nodes, or namespace nodes".
        (
            "node()",
            "ch|s",
            "self::node()[not(self::attribute() or self::document-node())]",
            "self::ch or self::s",
        ),
        (
            "p|@a|text()",
            "ch",
            "self::p or self::attribute(a) or self::text()",
            "self::ch",
        ),
    ];
    let mut checked = 0;
    for (count, from, count_pred, from_pred) in cases {
        for order in ["ascending", "descending"] {
            for (level, oracle) in [
                (
                    "single",
                    format!(
                        "for $A in ancestor-or-self::node()[{c}][1], $F in ancestor-or-self::node()[{f} or not(..)][1] \
                         return (for $AF in $A[ancestor-or-self::node()[. is $F]] return 1 + count($AF/preceding-sibling::node()[{c}]))",
                        c = count_pred,
                        f = from_pred
                    ),
                ),
                (
                    "multiple",
                    format!(
                        "for $F in ancestor-or-self::node()[{f} or not(..)][1] \
                         return string-join(for $af in ancestor-or-self::node()[{c}][ancestor-or-self::node()[. is $F]] \
                         return string(1 + count($af/preceding-sibling::node()[{c}])), '.')",
                        c = count_pred,
                        f = from_pred
                    ),
                ),
                (
                    "any",
                    format!(
                        "for $F in ((preceding::node()|ancestor-or-self::node())[{f} or not(..)])[last()] \
                         return (for $n in count((preceding::node()|ancestor-or-self::node())[{c}][. is $F or . >> $F]) return if ($n eq 0) then () else $n)",
                        c = count_pred,
                        f = from_pred
                    ),
                ),
            ] {
                // `count="a|b"` with `[$yes]` on every alternative.
                let slow_count = count
                    .split('|')
                    .map(|alt| format!("{alt}[$yes]"))
                    .collect::<Vec<_>>()
                    .join("|");
                let slow_from = from
                    .split('|')
                    .map(|alt| format!("{alt}[$yes]"))
                    .collect::<Vec<_>>()
                    .join("|");
                let text = root(&format!(
                    r#"<xsl:variable name="yes" select="true()"/>
                       <xsl:for-each select="//node() | //@*"><xsl:sort select="position()" data-type="number" order="{order}"/>
                         <r><xsl:number level="{level}" count="{count}" from="{from}"/>|<xsl:number level="{level}" count="{slow_count}" from="{slow_from}"/>|<xsl:value-of select="{oracle}"/></r>
                       </xsl:for-each>"#
                ));
                let result = out_over(&text, &source);
                let rows: Vec<&str> = result
                    .split("<r>")
                    .skip(1)
                    .map(|r| r.split("</r>").next().unwrap_or(""))
                    .collect();
                assert!(rows.len() > 100, "{level} {count}: {} rows", rows.len());
                let mut non_empty = 0;
                for (i, row) in rows.iter().enumerate() {
                    let parts: Vec<&str> = row.split('|').collect();
                    assert_eq!(parts.len(), 3, "{row}");
                    assert_eq!(
                        parts[0], parts[2],
                        "level={level} count={count} from={from} {order}: row {i}: {row}"
                    );
                    assert_eq!(
                        parts[1], parts[2],
                        "level={level} count={slow_count} from={slow_from} {order}: row {i}: {row}"
                    );
                    non_empty += usize::from(!parts[2].is_empty());
                    checked += 1;
                }
                assert!(non_empty > 20, "{level} {count}: only {non_empty} numbers");
            }
        }
    }
    assert!(checked > 3000, "{checked}");
}

/// The same expressions for the implied count pattern (no `count` attribute),
/// numbering every node: the pattern changes with the kind and name of each
/// selected node.
#[test]
fn the_implied_pattern_agrees_with_the_spec_expressions() {
    let source = r#"<doc a="0"><e a="1"><e/>t<f/><e><f/><?x a?><?y b?><?x c?></e></e><!--c--><f>u<e a="2"/></f>v<!--d--></doc>"#;
    // "the same node kind as the selected node and … the same expanded-QName"
    let same = "(($S instance of element() and self::element()) or ($S instance of attribute() and self::attribute()) \
                or ($S instance of text() and self::text()) or ($S instance of comment() and self::comment()) \
                or ($S instance of processing-instruction() and self::processing-instruction()) \
                or ($S instance of document-node() and self::document-node())) and deep-equal(node-name(.), node-name($S))";
    for level in ["single", "multiple", "any"] {
        let oracle = match level {
            "single" => format!(
                "for $A in ancestor-or-self::node()[{same}][1] return 1 + count($A/preceding-sibling::node()[{same}])"
            ),
            "multiple" => format!(
                "string-join(for $af in ancestor-or-self::node()[{same}] return string(1 + count($af/preceding-sibling::node()[{same}])), '.')"
            ),
            _ => format!(
                "for $n in count((preceding::node()|ancestor-or-self::node())[{same}]) return if ($n eq 0) then () else $n"
            ),
        };
        let text = root(&format!(
            r#"<xsl:for-each select="//node() | //@*"><xsl:variable name="S" select="."/><r><xsl:number level="{level}"/>|<xsl:value-of select="{oracle}"/></r></xsl:for-each>"#
        ));
        let result = out_over(&text, source);
        let rows: Vec<&str> = result
            .split("<r>")
            .skip(1)
            .map(|r| r.split("</r>").next().unwrap_or(""))
            .collect();
        assert!(rows.len() > 15, "{level}: {result}");
        for row in rows {
            let (ours, spec) = row.split_once('|').unwrap();
            assert_eq!(ours, spec, "level={level}: {row}");
        }
    }
}

/// What an evaluation remembers for the next is only reused when it applies:
/// nodes numbered out of document order, nodes of several trees, and a
/// pattern that changes with the selected node (the implied one) all give the
/// numbers a fresh evaluation gives.
#[test]
fn numbers_do_not_depend_on_the_order_of_evaluation() {
    let source = "<doc><a/><b/><a/><c><a/><b/></c><a/><b/></doc>";
    let text = root(
        r#"<xsl:variable name="other" as="document-node()"><xsl:document><z><a/><a/><a/><a/><a/></z></xsl:document></xsl:variable>
           <xsl:for-each select="reverse((//*, $other//a)), //*, $other//a">
             <n><xsl:number level="any"/>,<xsl:number/>,<xsl:number level="multiple" count="*"/></n>
           </xsl:for-each>"#,
    );
    let result = out_over(&text, source);
    let rows: Vec<&str> = result
        .split("<n>")
        .skip(1)
        .map(|r| r.split("</n>").next().unwrap_or(""))
        .collect();
    // 9 + 5 nodes in reverse order, then the same in document order.
    assert_eq!(rows.len(), 28);
    let reversed: Vec<&str> = rows[..14].iter().rev().copied().collect();
    assert_eq!(reversed, rows[14..].to_vec());
    assert_eq!(
        rows[14..].join(" "),
        "1,1,1 1,1,1.1 1,1,1.2 2,2,1.3 1,1,1.4 3,1,1.4.1 2,1,1.4.2 4,3,1.5 3,2,1.6 \
         1,1,1.1 2,2,1.2 3,3,1.3 4,4,1.4 5,5,1.5"
    );
}

/// A pattern that reads a local variable can match a node differently in each
/// evaluation of the instruction, so nothing counted by one evaluation is
/// reused by the next; one that reads only global variables gives the same
/// answers throughout the run, and its counts are reused.
#[test]
fn a_count_pattern_reading_a_local_variable_is_matched_afresh() {
    let source =
        r#"<doc><p k="a"/><p k="b"/><p k="a"/><q><p k="b"/></q><p k="a"/><p k="b"/></doc>"#;
    // (`q` counts among the children of `doc` for every key.)
    let local = root(
        r#"<xsl:for-each select="//p"><xsl:variable name="k" select="string(@k)"/><n><xsl:number level="any" count="p[@k = $k]"/>,<xsl:number count="p[@k = $k]"/>,<xsl:number level="multiple" count="*[@k = $k]|doc|q"/></n></xsl:for-each>"#,
    );
    assert_eq!(
        out_over(&local, source),
        "<out><n>1,1,1.1</n><n>1,1,1.1</n><n>2,2,1.2</n><n>2,1,1.2.1</n><n>3,3,1.4</n><n>3,2,1.3</n></out>"
    );
    // The same counts through a global variable, which every evaluation sees
    // alike: one pattern per key value.
    let global = sheet(
        r#"<xsl:param name="k" select="'b'"/>
           <xsl:template match="/"><out><xsl:for-each select="//p"><n><xsl:number level="any" count="p[@k = $k]"/></n></xsl:for-each></out></xsl:template>"#,
    );
    assert_eq!(
        out_over(&global, source),
        "<out><n/><n>1</n><n>1</n><n>2</n><n>2</n><n>3</n></out>"
    );
}

// ═══ §12.3 number to string conversion ═══════════════════════════════════

/// §12.3, "Example: Formatting a List of Numbers": "Given the sequence of
/// numbers 5, 13, 7 and the format token A-001(i), the output will be the
/// string E-013(vii)".
#[test]
fn the_format_example() {
    assert_eq!(
        number(r#"value="5, 13, 7" format="A-001(i)""#),
        "E-013(vii)"
    );
}

/// §12.3: prefix, separators and suffix; "If there are more numbers than
/// format tokens, then the last format token is used to format remaining
/// numbers"; "Each number after the first is separated from the preceding
/// number by the separator token preceding the format token used to format
/// that number, or, if that is the first format token, then by . (dot)"; "If
/// there is a non-alphanumeric token but no format token, then the single
/// non-alphanumeric token is used as both the prefix and the suffix"; the
/// default `format` is `1`.
#[test]
fn format_tokens_prefix_separators_suffix() {
    assert_eq!(number(r#"value="1 to 4" format="(1-a:i)""#), "(1-b:iii:iv)");
    assert_eq!(number(r#"value="1 to 3" format="[1]""#), "[1.2.3]");
    assert_eq!(number(r##"value="7" format="#""##), "#7#");
    assert_eq!(number(r#"value="7, 8" format="""#), "7.8");
    assert_eq!(number(r#"value="7, 8""#), "7.8");
    // `format` keeps its whitespace ("Except where the set of allowed values
    // of an attribute is specified using the italicized name string or char").
    assert_eq!(number(r#"value="3" format=" 1 ""#), " 3 ");
    // A computed format.
    assert_eq!(
        inner(&root(
            r#"<xsl:variable name="f" select="'i.'"/><xsl:number value="4" format="{$f}"/>"#
        )),
        "iv."
    );
}

/// §12.3: decimal-digit tokens "generate a decimal representation of the
/// number where each number is at least as long as the format token. The
/// digits used in the decimal representation are the set of digits containing
/// the digit character used in the format token."
#[test]
fn decimal_digit_families_and_padding() {
    assert_eq!(number(r#"value="5, 123" format="001""#), "005.123");
    assert_eq!(number(r#"value="1 to 3, 10" format="&#x661;""#), "١.٢.٣.١٠");
    assert_eq!(number(r#"value="7" format="&#x660;&#x661;""#), "٠٧");
    assert_eq!(number(r#"value="42" format="&#xFF11;""#), "４２");
    assert_eq!(number(r#"value="20" format="&#xE51;""#), "๒๐");
    assert_eq!(number(r#"value="9" format="&#x1D7CF;""#), "\u{1D7D7}");
    assert_eq!(number(r#"value="305" format="&#x967;""#), "३०५");
    // "a format token 1 generates the sequence 0 1 2 ..." (lower bound 0).
    assert_eq!(number(r#"value="0" format="01""#), "00");
}

/// §12.3: `A`, `a` ("A B C ... Z AA AB AC...."), `i`, `I`, and the bounds:
/// "Numbers that fall outside this range must be formatted using the format
/// token 1."
#[test]
fn alphabetic_and_roman_sequences_and_their_bounds() {
    assert_eq!(number(r#"value="1, 26, 27, 703" format="A""#), "A.Z.AA.AAA");
    assert_eq!(number(r#"value="13, 819" format="a""#), "m.aem");
    assert_eq!(
        number(r#"value="1999, 3999" format="I""#),
        "MCMXCIX.MMMCMXCIX"
    );
    assert_eq!(number(r#"value="13, 819" format="i""#), "xiii.dcccxix");
    assert_eq!(number(r#"value="4000, 0" format="i""#), "4000.0");
    assert_eq!(number(r#"value="0" format="A""#), "0");
}

/// §12.3: "A format token w generates numbers written as lower-case words …
/// W … upper-case words … Ww … title-case words" — English only.
#[test]
fn words() {
    assert_eq!(
        number(r#"value="0, 13, 21" format="w""#),
        "zero.thirteen.twenty one"
    );
    assert_eq!(
        number(r#"value="134" format="W""#),
        "ONE HUNDRED AND THIRTY FOUR"
    );
    assert_eq!(
        number(r#"value="588" format="Ww""#),
        "Five Hundred and Eighty Eight"
    );
    assert_eq!(number(r#"value="2002" format="w""#), "two thousand and two");
    assert_eq!(
        number(r#"value="2134816" format="Ww""#),
        "Two Million One Hundred and Thirty Four Thousand Eight Hundred and Sixteen"
    );
}

/// §12.3: "If the optional ordinal attribute is present, and if its value is
/// not a zero-length string, this indicates a request to output ordinal
/// numbers rather than cardinal numbers. For example, in English, the value
/// ordinal="yes" when used with the format token 1 outputs the sequence 1st
/// 2nd 3rd 4th ..., and when used with the format token w outputs the
/// sequence first second third fourth …"; "If ordinal numbering is not
/// supported for the combination of the format token, the language, and the
/// actual value of the ordinal attribute, the request is ignored and cardinal
/// numbers are generated instead."
#[test]
fn ordinals() {
    assert_eq!(
        number(r#"value="1, 2, 3, 4, 11, 12, 13, 21, 101, 111, 2002" format="1" ordinal="yes""#),
        "1st.2nd.3rd.4th.11th.12th.13th.21st.101st.111th.2002nd"
    );
    assert_eq!(
        number(r#"value="1, 2, 3, 4, 20, 21" format="w" ordinal="yes""#),
        "first.second.third.fourth.twentieth.twenty-first"
    );
    assert_eq!(
        number(r#"value="1100" format="Ww" ordinal="yes""#),
        "One Thousand One Hundredth"
    );
    assert_eq!(number(r#"value="3" format="i" ordinal="yes""#), "iii");
    assert_eq!(number(r#"value="3" format="1" ordinal="""#), "3");
    // Any non-empty value asks for English ordinals.
    assert_eq!(number(r#"value="3" format="1" ordinal="-e""#), "3rd");
    assert_eq!(
        inner(&root(
            r#"<xsl:variable name="o" select="'yes'"/><xsl:number value="2" ordinal="{$o}"/>"#
        )),
        "2nd"
    );
}

/// §12.3: "It is implementation-defined which numbering sequences, additional
/// to those listed above, are supported. If an implementation does not
/// support a numbering sequence represented by the given token, it must use a
/// format token of 1." (The supported sequences of consecutive code points —
/// circled digits and the like — are `tests/numbering_sequences_and_order.rs`'s.)
#[test]
fn unsupported_tokens_are_the_token_1() {
    for token in [
        "&#xB9;", "&#x3B1;", "&#x5D0;", "&#x30A2;", "x", "11", "2", "&#x24EA;",
    ] {
        assert_eq!(
            number(&format!(r#"value="1, 12" format="({token})""#)),
            "(1.12)",
            "{token}"
        );
    }
}

/// §12.3: "If a language is requested that is not supported, the processor
/// uses the language that it would use if the lang attribute were omitted" —
/// English. `lang` "has the same range of values as xml:lang": a language tag
/// (XTSE0020 when written, XTDE0030 when computed — `number-0825`,
/// `number-0826`).
#[test]
fn lang_falls_back_to_english_and_must_be_a_language_tag() {
    assert_eq!(number(r#"value="3" format="Ww" lang="de""#), "Three");
    assert_eq!(number(r#"value="3" format="w" lang="fr-CA""#), "three");
    assert_eq!(number(r#"value="3" format="w" lang="""#), "three");
    assert_eq!(
        static_error(&root(r######"<xsl:number value="2" lang="#####"/>"######)),
        "XTSE0020"
    );
    assert_eq!(
        dynamic_error(&root(
            r#"<xsl:variable name="l" select="42"/><xsl:number value="2" lang="{$l}"/>"#
        )),
        "XTDE0030"
    );
}

/// §12.3 `letter-value? = { "alphabetic" | "traditional" }`: checked, and in
/// English it changes nothing (the token alone names the sequence).
#[test]
fn letter_value_is_checked() {
    assert_eq!(
        number(r#"value="4" format="i" letter-value="traditional""#),
        "iv"
    );
    assert_eq!(
        number(r#"value="4" format="a" letter-value="alphabetic""#),
        "d"
    );
    assert_eq!(
        number(r#"value="4" format="a" letter-value=" alphabetic ""#),
        "d"
    );
    assert_eq!(
        static_error(&root(r#"<xsl:number value="2" letter-value="other"/>"#)),
        "XTSE0020"
    );
    assert_eq!(
        dynamic_error(&root(
            r#"<xsl:variable name="v" select="'x'"/><xsl:number value="2" letter-value="{$v}"/>"#
        )),
        "XTDE0030"
    );
}

/// §12.3: "grouping-separator="," and grouping-size="3" would produce numbers
/// of the form 1,000,000 while grouping-separator="." and grouping-size="2"
/// would produce numbers of the form 1.00.00.00. If only one of the
/// grouping-separator and grouping-size attributes is specified, then it is
/// ignored." `grouping-separator` is a `char`, `grouping-size` a `number`.
#[test]
fn grouping() {
    assert_eq!(
        number(r#"value="1000000" grouping-separator="," grouping-size="3""#),
        "1,000,000"
    );
    assert_eq!(
        number(r#"value="1000000" grouping-separator="." grouping-size="2""#),
        "1.00.00.00"
    );
    assert_eq!(
        number(r#"value="1000000" grouping-separator=",""#),
        "1000000"
    );
    assert_eq!(number(r#"value="1000000" grouping-size="3""#), "1000000");
    assert_eq!(
        number(
            r#"value="1000000" format="00000000000001" grouping-separator="/" grouping-size="2""#
        ),
        "00/00/00/01/00/00/00"
    );
    // A space is a character like any other.
    assert_eq!(
        number(r#"value="12345" grouping-separator=" " grouping-size="3""#),
        "12 345"
    );
    assert_eq!(
        inner(&root(
            r#"<xsl:variable name="s" select="'!'"/><xsl:variable name="g" select="2"/><xsl:number value="12345" grouping-separator="{$s}" grouping-size="{$g}"/>"#
        )),
        "1!23!45"
    );
    for bad in [
        r#"grouping-separator=",," grouping-size="3""#,
        r#"grouping-separator="" grouping-size="3""#,
        r#"grouping-separator="," grouping-size="three""#,
        r#"grouping-separator="," grouping-size="-3""#,
    ] {
        assert_eq!(
            static_error(&root(&format!(r#"<xsl:number value="1" {bad}/>"#))),
            "XTSE0020",
            "{bad}"
        );
    }
    assert_eq!(
        dynamic_error(&root(
            r#"<xsl:variable name="s" select="'ab'"/><xsl:number value="1" grouping-separator="{$s}" grouping-size="3"/>"#
        )),
        "XTDE0030"
    );
}

/// §12.3: every conversion attribute is an attribute value template, evaluated
/// with the instruction's focus.
#[test]
fn conversion_attributes_are_attribute_value_templates() {
    let text = root(
        r#"<xsl:for-each select="doc/e"><n><xsl:number format="{@f}" ordinal="{@o}"/></n></xsl:for-each>"#,
    );
    assert_eq!(
        out_over(
            &text,
            r#"<doc><e f="1" o="yes"/><e f="a" o=""/><e f="W" o="yes"/></doc>"#
        ),
        "<out><n>1st</n><n>b</n><n>THIRD</n></out>"
    );
}

/// An `xsl:number` in a named template run as the initial template has no
/// context item at all.
#[test]
fn an_initial_template_without_a_source_has_no_context_node() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let text = sheet(r#"<xsl:template name="main"><out><xsl:number/></out></xsl:template>"#);
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, "urn:numbering:sheet.xsl")
        .unwrap();
    let arena = bumpalo::Bump::new();
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_initial_template(QName::local("main"));
    match run.transform_without_source() {
        Err(e) => assert_eq!(e.code, "XTTE0990"),
        Ok(_) => panic!("expected XTTE0990"),
    }
}
