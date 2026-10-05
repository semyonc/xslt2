//! M2l — pattern predicates evaluated per candidate (ROADMAP §4 "M2l").
//!
//! The matcher used to evaluate every predicate of a step for **every**
//! sibling of the candidate, which made `match="item[@type='x']"` quadratic in
//! the number of siblings, for template rules and for `xsl:key` alike. It now
//! decides each predicate for the candidate alone and builds the sibling
//! sequence only for a positional predicate (and remembers it per parent when
//! it can). These tests pin the semantics down, then the speed.
//!
//! The semantics are XSLT 2.0 §5.5.3 (`xslt20.html`): "To determine whether a
//! node N matches the pattern, evaluate the expression `root(.)//(EE)` with a
//! singleton focus based on N. If the result is a sequence of nodes that
//! includes N, then node N matches the pattern; otherwise node N does not match
//! the pattern." Most tests therefore compare three things: the nodes a
//! template rule with the pattern matches, the nodes the equivalent expression
//! `root(.)//(EE)` selects (evaluated by the XPath engine, which filters whole
//! sequences), and the node set worked out by hand from XPath 2.0 §3.2.2.

use std::time::{Duration, Instant};

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const TEST_STACK: usize = 256 * 1024 * 1024;

/// The functions every test stylesheet shares: `f:label` names a node in the
/// output (an element by its `id`, an attribute as `owner@name`, a text node as
/// `t:` and its value).
const FUNCTIONS: &str = r#"
  <xsl:function name="f:label" as="xs:string">
    <xsl:param name="n" as="node()"/>
    <xsl:sequence select="if ($n instance of element()) then string($n/@id)
                          else if ($n instance of attribute()) then concat($n/../@id, '@', name($n))
                          else if ($n instance of text()) then concat('t:', $n)
                          else name($n)"/>
  </xsl:function>
  <xsl:function name="f:is-x" as="xs:boolean">
    <xsl:param name="t"/>
    <xsl:sequence select="$t = 'x'"/>
  </xsl:function>
  <xsl:function name="f:even" as="xs:boolean">
    <xsl:param name="p" as="xs:integer"/>
    <xsl:sequence select="$p mod 2 = 0"/>
  </xsl:function>
  <xsl:variable name="n" select="2"/>
  <xsl:variable name="s" select="'x'"/>
  <xsl:variable name="t" select="'x'"/>
  <xsl:variable name="doc-id" select="'d'"/>
  <xsl:key name="byid" match="*" use="@id"/>"#;

fn sheet_v(version: &str, body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{FUNCTIONS}{body}</xsl:stylesheet>"#
    )
}

fn sheet(body: &str) -> String {
    sheet_v("2.0", body)
}

fn run_with(main: &str, source: &str, strict: bool) -> Result<String, XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///m2l/main.xsl")?;
        let arena = Bump::new();
        let options = TransformOptions {
            strict_recoverable: strict,
            ..TransformOptions::default()
        };
        let outcome = compiled
            .transformation(&arena, options)
            .transform_source("file:///m2l/in.xml", &source)?;
        compiled.serialize_primary(&outcome)
    })
}

fn out(main: &str, source: &str) -> String {
    match run_with(main, source, false) {
        Ok(text) => text,
        Err(e) => panic!("the transformation failed: {e}"),
    }
}

/// The text of the single `<out>` element of a result, trimmed.
fn inner(result: &str) -> String {
    let body = result
        .strip_prefix("<out>")
        .and_then(|r| r.strip_suffix("</out>"))
        .unwrap_or(if result == "<out/>" { "" } else { result });
    body.trim().to_string()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// The nodes of `source` that `pattern` matches, as labels in document order:
/// every node and attribute is offered to a template rule with the pattern
/// in a mode of its own.
fn matched_v(
    version: &str,
    pattern: &str,
    source: &str,
    strict: bool,
) -> Result<String, XsltError> {
    let main = sheet_v(
        version,
        &format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="//node() | //@*" mode="m"/></out></xsl:template>
               <xsl:template match="{}" mode="m"><xsl:value-of select="f:label(.)"/><xsl:text> </xsl:text></xsl:template>
               <xsl:template match="node() | @*" mode="m" priority="-10"/>"#,
            escape(pattern)
        ),
    );
    run_with(&main, source, strict).map(|r| inner(&r))
}

fn matched(pattern: &str, source: &str) -> String {
    matched_v("2.0", pattern, source, false)
        .unwrap_or_else(|e| panic!("matching {pattern:?} failed: {e}"))
}

/// The nodes the expression `expr` selects from the document node of `source`,
/// as labels in document order.
fn selected(expr: &str, source: &str) -> String {
    let main = sheet(&format!(
        r#"<xsl:template match="/"><out><xsl:for-each select="{}"><xsl:value-of select="f:label(.)"/><xsl:text> </xsl:text></xsl:for-each></out></xsl:template>"#,
        escape(expr)
    ));
    inner(&out(&main, source))
}

/// `pattern` matches exactly `expected` in `source`, and so does its
/// equivalent expression `root(.)//(EE)` (§5.5.3) — the engine's own
/// whole-sequence evaluation of the same predicates.
fn check_in(source: &str, pattern: &str, expected: &str) {
    assert_eq!(matched(pattern, source), expected, "pattern {pattern:?}");
    assert_eq!(
        selected(&format!("root(.)//({pattern})"), source),
        expected,
        "the equivalent expression of {pattern:?}"
    );
}

fn check(pattern: &str, expected: &str) {
    check_in(SRC, pattern, expected);
}

/// Six `item`s: four under `doc`, two under `list`. Positions under `doc`:
/// i1 = 1, i2 = 2, i3 = 3, i4 = 4 (`other` is not an `item`); under `list`:
/// i5 = 1, i6 = 2.
const SRC: &str = r#"<doc id="d" xmlns:p="urn:p">
  <item id="i1" a="1" n="2" k="1" type="x"/>
  <item id="i2" n="1" k="2" type="y"><x id="x1"/></item>
  <item id="i3" a="1" n="3" k="3" type="x"><sub id="s1"><x id="x2"/><x id="x3"/></sub></item>
  <other id="o1" a="1"/>
  <item id="i4" a="1" n="4" k="1" type="y"/>
  <list id="l1"><item id="i5" n="1" a="1" k="1"/><item id="i6" n="1" k="2"/></list>
</doc>"#;

// ── Positional predicates: unchanged (XPath 2.0 §3.2.2) ─────────────────────

/// §5.5.3: `para[1]` "matches any node selected by the expression
/// `root(.)//(child-or-top::para[1])`: that is, any `para` element that is the
/// first `para` child of its parent". XPath 2.0 §2.1.2: "The context size is
/// the number of items in the sequence of items currently being processed."
#[test]
fn first_and_last_count_among_the_siblings_the_node_test_selects() {
    check("item[1]", "i1 i5");
    check("item[last()]", "i4 i6");
    check("item[position() mod 2 = 0]", "i2 i4 i6");
    check("item[position() = last() - 1]", "i3 i5");
}

/// XPath 2.0 §3.2.2: "In the case of multiple adjacent predicates, the
/// predicates are applied from left to right, and the result of applying each
/// predicate serves as the input sequence for the following predicate." So in
/// `item[@a][2]` the 2 counts among the siblings that have an `a`.
#[test]
fn a_positional_predicate_counts_among_the_survivors_of_the_ones_before_it() {
    check("item[@a][2]", "i3");
    check("item[@type = $t][2]", "i3");
    check("item[position() le 2][@a]", "i1 i5");
    check("item[position() gt 1][1]", "i2 i6");
    check("item[@a][last()]", "i4 i5");
    check("item[@a][number(@n) - 1]", "i1 i3 i4");
}

/// XPath 2.0 §3.2.2: "If the value of the predicate expression is a singleton
/// atomic value of a numeric type or derived from a numeric type, the
/// predicate truth value is true if the value of the predicate expression is
/// equal (by the eq operator) to the context position, and is false otherwise.
/// … Otherwise, the predicate truth value is the effective boolean value of
/// the predicate expression." Whether a predicate is numeric is a property of
/// its *value*, so none of these mentions `position()` and all are positional.
#[test]
fn a_numeric_value_is_compared_with_the_position_whatever_the_expression() {
    // A global number.
    check("item[$n]", "i2 i6");
    // A per-node number: i3 (n=3, 3rd), i4 (n=4, 4th), i5 (n=1, 1st).
    check("item[number(@n)]", "i3 i4 i5");
    check("item[xs:integer(@k)]", "i1 i2 i3 i5 i6");
}

/// The other half of the same rule: a value that is not a singleton number is
/// taken by its effective boolean value — a global string, and an attribute
/// node whose *content* looks like a number (`@n` is a node, not a number).
#[test]
fn a_value_that_is_not_a_singleton_number_is_taken_by_its_boolean_value() {
    check("item[$s]", "i1 i2 i3 i4 i5 i6");
    check("item[@n]", "i1 i2 i3 i4 i5 i6");
    check("item[@a]", "i1 i3 i4 i5");
}

/// A `position()` inside a *nested* predicate belongs to that predicate's own
/// focus (XPath 2.0 §3.2.2: "the predicate expression is evaluated using an
/// inner focus"), so `item[.//x[1]]` is not positional.
#[test]
fn a_nested_positional_predicate_does_not_make_the_outer_one_positional() {
    check("item[.//x[1]]", "i2 i3");
    check("item[.//x[2]]", "i3");
    check("*[*[last()][@id = 'x3']]", "s1");
}

/// `item[(1, 2)]`: a sequence of two numbers is not "a singleton atomic value
/// of a numeric type", so its effective boolean value is taken, and F&O §15.1.1
/// makes that FORG0006 for a sequence of more than one atomic value. §5.5.4:
/// "Any dynamic error or type error that occurs during the evaluation of a
/// pattern against a particular node is treated as a recoverable error … The
/// optional recovery action is to treat the pattern as not matching that
/// node" — no node matches, and with the error signalled it is FORG0006.
#[test]
fn a_sequence_of_numbers_is_not_a_numeric_predicate() {
    assert_eq!(matched("item[(1, 2)]", SRC), "");
    let e = matched_v("2.0", "item[(1, 2)]", SRC, true).expect_err("FORG0006");
    assert_eq!(e.code, "FORG0006");
}

// ── Other axes and node kinds ──────────────────────────────────────────────

/// §5.5.3: "If PS uses the attribute axis, then the axis in step PS is
/// replaced by `attribute-or-top`" — positions count among the attributes the
/// node test selects on the same element.
#[test]
fn attribute_steps_count_among_the_attributes_of_one_element() {
    check("@a[. = '1']", "i1@a i3@a o1@a i4@a i5@a");
    check("item/@*[3]", "i1@n i2@k i3@n i4@n i5@a i6@k");
    check("@*[. = '1'][2]", "i1@k i4@k i5@a");
    check("@k[last()][. = '2']", "i2@k i6@k");
}

/// §5.5.2 allows only the `child` and `attribute` axes in a pattern, so a
/// namespace node is never matched by a step; a predicate may still *use* the
/// namespace axis, positionally or not.
#[test]
fn a_predicate_may_use_the_namespace_axis() {
    let e = matched_v("2.0", "namespace::p", SRC, false).expect_err("not a pattern");
    assert_eq!(e.code, "XTSE0340");
    check("item[namespace::p][1]", "i1 i5");
    // The second element child of each parent: i2 under doc, x3 under sub,
    // i6 under list.
    check("*[namespace::p][2]", "i2 x3 i6");
}

/// Text nodes are on `child-or-top` too.
#[test]
fn text_steps_count_among_the_text_children() {
    let source = r#"<p id="p">one<b id="b">two</b>three</p>"#;
    check_in(source, "text()[1]", "t:one t:two");
    check_in(source, "text()[last()]", "t:two t:three");
    check_in(source, "b/text()[1]", "t:two");
}

/// §5.5.3: "If the context node is a parentless element, comment,
/// processing-instruction, or text node then the `child-or-top` axis selects
/// the context node" (and `attribute-or-top` a parentless attribute), so the
/// sequence is the node alone: position 1 of 1.
#[test]
fn a_parentless_node_is_its_own_sibling_sequence() {
    let main = sheet(
        r#"<xsl:variable name="e" as="element()"><item id="p1" a="1"/></xsl:variable>
           <xsl:variable name="att" as="attribute()"><xsl:attribute name="a" select="'1'"/></xsl:variable>
           <xsl:template match="/"><out><xsl:apply-templates select="$e" mode="m"/>|<xsl:apply-templates select="$att" mode="m"/></out></xsl:template>
           <xsl:template match="item[1][@a][last()][. is current()]" mode="m">first</xsl:template>
           <xsl:template match="item[2]" mode="m">second</xsl:template>
           <xsl:template match="@a[2]" mode="m">second</xsl:template>
           <xsl:template match="@a[position() = 1][. = '1']" mode="m">attr</xsl:template>
           <xsl:template match="node() | @*" mode="m" priority="-10">none</xsl:template>"#,
    );
    assert_eq!(out(&main, "<doc/>"), "<out>first|attr</out>");
}

/// §5.5.3: "If the NodeTest in PS is `document-node()` … then the axis in step
/// PS is taken as `self` rather than `child`" — a document node has no parent,
/// so its sequence is itself.
#[test]
fn a_document_node_step_is_its_own_sequence() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="." mode="m"/></out></xsl:template>
           <xsl:template match="document-node()[2]" mode="m">second</xsl:template>
           <xsl:template match="document-node(element(doc))[last()][doc/@id = 'd'][1]" mode="m">first</xsl:template>
           <xsl:template match="node()" mode="m" priority="-10">none</xsl:template>"#,
    );
    assert_eq!(out(&main, SRC), "<out>first</out>");
}

// ── Pattern shapes ─────────────────────────────────────────────────────────

/// §5.5.3: a pattern "can be examined from right to left": predicates on a
/// step to the left are evaluated against the ancestor that step is tried on.
#[test]
fn predicates_on_every_step_of_a_path() {
    check("doc/item[2]", "i2");
    check("list/item[1]", "i5");
    check("item[2]/x", "x1");
    check("sub/x[last()]", "x3");
    check("doc//item[1]", "i1 i5");
    check("item[@a][2]//x[1]", "x2");
    check("*[@id = 'l1'][1]/item", "i5 i6");
    check("*[@id = 'l1'][2]/item", "");
    check("*[@id = 'l1'][last()]/item[last()]", "i6");
}

/// The anchored forms: `/…` and `//…` (§5.5.2).
#[test]
fn rooted_patterns_with_predicates() {
    check("/doc/item[last()]", "i4");
    check("/doc[1]/item[1]", "i1");
    check("//item[2]", "i2 i6");
    check("//list/item[@a][1]", "i5");
}

/// §5.5.3: "In a pattern, `|` indicates alternatives; a pattern with one or
/// more `|` separated alternatives matches if any one of the alternatives
/// matches."
#[test]
fn unions_of_predicate_patterns() {
    check("item[1] | item[last()]", "i1 i4 i5 i6");
    check("item[@a][1] | @k[. = '2']", "i1 i2@k i5 i6@k");
}

/// §5.5.2 `IdKeyPattern`, followed by steps with predicates.
#[test]
fn key_and_id_anchored_patterns_with_predicates_on_later_steps() {
    check("key('byid', 'l1')/item[2]", "i6");
    check("key('byid', 'd')//item[@a][1]", "i1 i5");
    check("key('byid', $doc-id)/item[last()]", "i4");
    let source =
        r#"<doc><list xml:id="L"><item id="j1"/><item id="j2"/></list><item id="j3"/></doc>"#;
    check_in(source, "id('L')/item[last()]", "j2");
    check_in(source, "id('L')//*[position() = 1]", "j1");
}

/// Predicates that call stylesheet functions. §10.3: "Within the body of a
/// stylesheet function, the focus is initially undefined", so a function
/// cannot read the position itself; `position()` passed as an *argument* is
/// read in the predicate's own focus.
#[test]
fn predicates_that_call_stylesheet_functions() {
    check("item[f:is-x(@type)]", "i1 i3");
    check("item[f:is-x(@type)][2]", "i3");
    check("item[f:even(position())]", "i2 i4 i6");
}

/// §16.6.1: "If the current function is used within a pattern, its value is
/// the node that is being matched against the pattern." So the sequence a
/// positional predicate counts in may depend on the node being matched:
/// `item[. is current()][1]` keeps only the candidate, which is then first —
/// every item matches; `item[@type = current()/@type][1]` is the first item of
/// the candidate's own type among its siblings. (No equivalent-expression
/// check: in an expression `current()` is the instruction's focus.)
#[test]
fn a_predicate_that_reads_current_counts_in_a_sequence_of_its_own() {
    assert_eq!(matched("item[. is current()][1]", SRC), "i1 i2 i3 i4 i5 i6");
    assert_eq!(matched("item[@type = current()/@type][1]", SRC), "i1 i2");
    assert_eq!(matched("item[current()/@a][1]", SRC), "i1 i5");
    assert_eq!(
        matched("item[@k = current()/@k][last()]", SRC),
        "i2 i3 i4 i5 i6"
    );
    // Several positional predicates after one that reads `current()`.
    assert_eq!(
        matched("item[. is current()][1][1]", SRC),
        "i1 i2 i3 i4 i5 i6"
    );
    assert_eq!(
        matched("item[@k = current()/@k][1][last()]", SRC),
        "i1 i2 i3 i5 i6"
    );
    assert_eq!(matched("item[1][. is current()][last()]", SRC), "i1 i5");
}

/// The same rules with backwards-compatible behaviour (§3.8): the predicates
/// are compiled against the compatibility catalog, and `position()` must still
/// be recognised as reading the context position.
#[test]
fn positional_predicates_with_backwards_compatible_behaviour() {
    for (pattern, expected) in [
        ("item[position() = 2]", "i2 i6"),
        ("item[2]", "i2 i6"),
        ("item[@a][2]", "i3"),
        ("item[last()]", "i4 i6"),
        ("item[@type = 'x']", "i1 i3"),
    ] {
        assert_eq!(
            matched_v("1.0", pattern, SRC, false).unwrap(),
            expected,
            "version 1.0, {pattern:?}"
        );
    }
}

/// §16.3.1: "an `xsl:key` element applies to all nodes that match the pattern
/// specified in the `match` attribute" — the same matcher, on the key path.
#[test]
fn key_patterns_with_predicates() {
    let main = sheet(
        r#"<xsl:key name="kx" match="item[@type = 'x']" use="@k"/>
           <xsl:key name="kp" match="item[position() mod 2 = 0]" use="@k"/>
           <xsl:key name="ka" match="item[@a][2]" use="@k"/>
           <xsl:key name="katt" match="@*[. = '1'][2]" use="name()"/>
           <xsl:key name="kc" match="item[@type = current()/@type][1]" use="'first'"/>
           <xsl:template match="/"><out><xsl:value-of select="key('kx', '1')/@id"/>|<xsl:value-of select="key('kx', '3')/@id"/>|<xsl:value-of select="key('kp', '1')/@id"/>|<xsl:value-of select="key('kp', '2')/@id"/>|<xsl:value-of select="key('ka', '3')/@id"/>|<xsl:value-of select="for $a in key('katt', 'k') return f:label($a)"/>|<xsl:value-of select="key('kc', 'first')/@id"/></out></xsl:template>"#,
    );
    assert_eq!(
        out(&main, SRC),
        "<out>i1|i3|i4|i2 i6|i3|i1@k i4@k|i1 i2</out>"
    );
}

// ── The remembered sibling sequences ───────────────────────────────────────

/// A positional predicate's sibling sequence is remembered per parent for the
/// run. Two trees of the same shape have parents at the same node indexes; the
/// sequences must still be told apart (here the second `x` item is the third
/// child in the source and the second child in the temporary tree).
#[test]
fn trees_of_the_same_shape_keep_their_own_sequences() {
    let main = sheet(
        r#"<xsl:variable name="tree"><doc><item id="t1" type="x"/><item id="t2" type="x"/><item id="t3" type="y"/></doc></xsl:variable>
           <xsl:template match="/"><out><xsl:apply-templates select="doc/item"/>|<xsl:apply-templates select="$tree/doc/item"/>|<xsl:apply-templates select="doc/item"/></out></xsl:template>
           <xsl:template match="item[@type = 'x'][2]"><xsl:value-of select="@id"/></xsl:template>
           <xsl:template match="item"/>"#,
    );
    let source =
        r#"<doc><item id="s1" type="x"/><item id="s2" type="y"/><item id="s3" type="x"/></doc>"#;
    assert_eq!(out(&main, source), "<out>s3|t2|s3</out>");
}

/// More parents than the cache keeps (past 4,096 sequences it forgets the less
/// recently used half): every answer is still right. Parents are visited in
/// document order here, so an evicted sequence is never needed again; the M2l
/// review checked the rebuild-after-eviction path with a separate probe.
#[test]
fn many_parents_are_matched_correctly_past_the_cache_limit() {
    const PARENTS: usize = 5_000;
    let mut source = String::from("<doc>");
    for i in 0..PARENTS {
        source.push_str(&format!(
            r#"<list><item id="a{i}" type="x"/><item id="b{i}" type="y"/><item id="c{i}" type="x"/></list>"#
        ));
    }
    source.push_str("</doc>");
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="item[@type = 'x'][2] | item[last()]"><xsl:value-of select="@id"/><xsl:text> </xsl:text></xsl:template>
           <xsl:template match="item"/>"#,
    );
    // Every list has one match, `c<i>`, through both alternatives.
    let expected: String = (0..PARENTS).map(|i| format!("c{i} ")).collect();
    assert_eq!(out(&main, &source), format!("<out>{expected}</out>"));
}

// ── Errors: decided per candidate (XPath 2.0 §2.3.4, XSLT §5.5.4) ───────────

/// The second item's `k` is not an integer.
const ERR: &str = r#"<doc><item id="e1" k="1"/><item id="e2" k="x"/><item id="e3" k="3"/></doc>"#;

/// The behaviour change of M2l. Before, every predicate was evaluated for
/// every sibling, so the error `xs:integer('x')` raises on `e2` made the
/// pattern fail for `e1` and `e3` too. Now a non-positional predicate is
/// evaluated for the candidate alone. XPath 2.0 §2.3.4: "the processor is free
/// to stop examining further items in a sequence as soon as it can establish
/// that further items would not affect the result except possibly by causing
/// an error"; XSLT §5.5.4 scopes the error to "the evaluation of a pattern
/// against a particular node", whose recovery action is "to treat the pattern
/// as not matching that node". (Before M2l: `""` for all three.)
#[test]
fn an_error_on_another_sibling_no_longer_fails_the_candidate() {
    assert_eq!(matched("item[xs:integer(@k) gt 0]", ERR), "e1 e3");
    // A numeric predicate needs the position, not the other siblings' values:
    // e1 has k = 1 at position 1, e3 has k = 3 at position 3.
    assert_eq!(matched("item[xs:integer(@k)]", ERR), "e1 e3");
    // A positional predicate is also evaluated once, for the candidate, with
    // its position — never for the siblings.
    assert_eq!(matched("item[position() = xs:integer(@k)]", ERR), "e1 e3");
}

/// With the error signalled instead of recovered from (`strict_recoverable`),
/// matching only `e1` no longer raises the error that belongs to `e2`
/// (before M2l: FORG0001); matching `e2` still does.
#[test]
fn a_signalled_error_belongs_to_the_node_that_raised_it() {
    let only_first = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/item[1]"/></out></xsl:template>
           <xsl:template match="item[xs:integer(@k) gt 0]"><xsl:value-of select="@id"/></xsl:template>
           <xsl:template match="item"/>"#,
    );
    assert_eq!(run_with(&only_first, ERR, true).unwrap(), "<out>e1</out>");
    let e = matched_v("2.0", "item[xs:integer(@k) gt 0]", ERR, true).expect_err("FORG0001 on e2");
    assert_eq!(e.code, "FORG0001");
}

/// What did not change: a predicate *before* a positional one is evaluated
/// for every sibling, because the position counts among the siblings it
/// keeps — the error on `e2` decides the sequence, so no node matches (and the
/// error is signalled for `e1` already).
#[test]
fn an_error_before_a_positional_predicate_still_reaches_every_sibling() {
    assert_eq!(matched("item[xs:integer(@k) gt 0][1]", ERR), "");
    let e = matched_v("2.0", "item[xs:integer(@k) gt 0][1]", ERR, true).expect_err("FORG0001");
    assert_eq!(e.code, "FORG0001");
}

/// The same change on the key path: the index of a key whose `match` has a
/// failing predicate on one node still holds the others (before M2l: `0`).
#[test]
fn a_key_index_keeps_the_nodes_whose_predicate_did_not_fail() {
    let main = sheet(
        r#"<xsl:key name="ke" match="item[xs:integer(@k) gt 0]" use="@id"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('ke', ('e1', 'e2', 'e3')))"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ERR), "<out>2</out>");
}

// ── Performance: linear in the number of siblings ───────────────────────────

/// `n` sibling items under one element, alternately `type="y"` and `"x"`.
fn siblings(n: usize) -> String {
    let mut s = String::from("<doc>");
    for i in 0..n {
        let t = if i % 2 == 1 { "x" } else { "y" };
        s.push_str(&format!(r#"<item code="c{i}" type="{t}"/>"#));
    }
    s.push_str("</doc>");
    s
}

/// Fails the test if `f` took longer than `limit`; returns its result.
fn within<T>(limit: Duration, what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed();
    assert!(
        elapsed < limit,
        "{what} took {elapsed:?} (limit {limit:?}): the predicate is evaluated per sibling again"
    );
    result
}

/// The M2f review's probes (P1-2), at 5,000 siblings: a template rule and a
/// key whose pattern has a non-positional predicate, and the same with
/// positional ones. Before M2l each took tens of seconds in a release build
/// (the template rule 25.5 s at 5,000 siblings); now each takes milliseconds,
/// and the bound below is generous enough for a debug build on a loaded
/// machine.
#[test]
fn predicate_patterns_are_linear_in_the_number_of_siblings() {
    const N: usize = 5_000;
    let source = siblings(N);
    let limit = Duration::from_secs(5);

    let template = sheet(
        r#"<xsl:template match="/"><out><xsl:variable name="r"><xsl:apply-templates select="//item"/></xsl:variable><xsl:value-of select="string-length($r)"/></out></xsl:template>
           <xsl:template match="item[@type = 'x']">x</xsl:template>
           <xsl:template match="item"/>"#,
    );
    let result = within(limit, "a template rule item[@type = 'x']", || {
        out(&template, &source)
    });
    assert_eq!(result, format!("<out>{}</out>", N / 2));

    let key = sheet(
        r#"<xsl:key name="k" match="item[@type = 'x']" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'c1')), count(key('k', 'c2'))"/></out></xsl:template>"#,
    );
    let result = within(limit, "a key match=\"item[@type = 'x']\"", || {
        out(&key, &source)
    });
    assert_eq!(result, "<out>1 0</out>");

    let positional = sheet(
        r#"<xsl:key name="k" match="item[@type = 'x'][2]" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'c3'))"/>|<xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="item[position() mod 1000 = 0]">x</xsl:template>
           <xsl:template match="item"/>"#,
    );
    let result = within(limit, "positional predicates", || out(&positional, &source));
    assert_eq!(result, format!("<out>1|{}</out>", "x".repeat(N / 1000)));
}
