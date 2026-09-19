//! The dynamic-context transition table of the design sketch §9.1, one test per
//! row, for the constructs M1 implements.
//!
//! Each row says which parts of the dynamic context a construct **preserves**,
//! **replaces** or **clears**. A stylesheet is the cheapest way to observe
//! them: the focus is `.`, `position()` and `last()`; `current()` is the item
//! of the innermost instruction, not of the expression being evaluated; the
//! current template rule is what `xsl:next-match` continues from; the variable
//! frame is what a local `xsl:variable` lands in; tunnel parameters are what an
//! `xsl:param tunnel="yes"` sees; and the output state is which destination the
//! events reach.

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">{body}</xsl:stylesheet>"#
    )
}

fn transform(sheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(std::sync::Arc::new(MemoryResolver::new()))
        .compile_str(sheet, "urn:test:sheet.xsl")?;
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run.transform_source("urn:test:in.xml", source)?;
    compiled.serialize_primary(&outcome)
}

fn messages(sheet: &str, source: &str) -> Vec<String> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(sheet, "urn:test:sheet.xsl")
        .expect("compile");
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run.transform_source("urn:test:in.xml", source).expect("run");
    outcome.messages.into_iter().map(|m| m.content).collect()
}

const THREE: &str = "<doc><i>a</i><i>b</i><i>c</i></doc>";

/// Row 1 — `xsl:apply-templates` → matched rule: the focus is **replaced**
/// with the selected item, its position in the selection and the size of the
/// selection; `current()` becomes the focus item; the frame is new; tunnel
/// parameters are inherited.
#[test]
fn apply_templates_replaces_the_focus_and_current() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i"/></out></xsl:template>
           <xsl:template match="i">
             <at p="{position()}" l="{last()}" v="{.}" c="{current()}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><at p="1" l="3" v="a" c="a"/><at p="2" l="3" v="b" c="b"/>"#.to_owned()
            + r#"<at p="3" l="3" v="c" c="c"/></out>"#
    );
}

/// Row 1, second half — a new frame: a local variable of the calling template
/// is not visible in the matched one, and the matched template's own locals do
/// not leak back.
#[test]
fn apply_templates_opens_a_new_frame() {
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="caller" select="'outer'"/>
             <out c="{$caller}"><xsl:apply-templates select="doc/i[1]"/></out>
           </xsl:template>
           <xsl:template match="i">
             <xsl:variable name="callee" select="'inner'"/>
             <at v="{$callee}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out c="outer"><at v="inner"/></out>"#
    );
}

/// Row 2 — the built-in rule (§6.6): the focus is replaced as for any matched
/// rule, the **mode is unchanged**, and the parameters are passed through.
#[test]
fn a_built_in_rule_keeps_the_mode_and_passes_parameters_through() {
    let s = sheet(
        r#"<xsl:template match="/">
             <out><xsl:apply-templates select="doc" mode="m">
               <xsl:with-param name="t" select="'kept'" tunnel="yes"/>
             </xsl:apply-templates></out>
           </xsl:template>
           <xsl:template match="i" mode="m">
             <xsl:param name="t" tunnel="yes"/>
             <in mode-still="m" t="{$t}" p="{position()}" l="{last()}"/>
           </xsl:template>"#,
    );
    // `doc` has no rule in mode `m`, so the built-in rule applies templates to
    // its children in the same mode, and the tunnel parameter survives.
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><in mode-still="m" t="kept" p="1" l="3"/>"#.to_owned()
            + r#"<in mode-still="m" t="kept" p="2" l="3"/>"#
            + r#"<in mode-still="m" t="kept" p="3" l="3"/></out>"#
    );
}

/// Row 3 — `xsl:call-template`: the focus is **preserved**, `current()` is
/// preserved, and only the frame is new.
#[test]
fn call_template_preserves_the_focus_and_opens_a_new_frame() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i"/></out></xsl:template>
           <xsl:template match="i">
             <xsl:if test="position() = 2">
               <xsl:call-template name="report"/>
             </xsl:if>
           </xsl:template>
           <xsl:template name="report">
             <at p="{position()}" l="{last()}" v="{.}" c="{current()}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><at p="2" l="3" v="b" c="b"/></out>"#
    );
}

/// Row 3, second half — the current template rule is **preserved** across
/// `xsl:call-template`, so an `xsl:next-match` inside a named template
/// continues from the caller's rule.
#[test]
fn call_template_preserves_the_current_template_rule() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i[1]"/></out></xsl:template>
           <xsl:template match="i" priority="2">
             <high/><xsl:call-template name="go"/>
           </xsl:template>
           <xsl:template match="i" priority="1"><low/></xsl:template>
           <xsl:template name="go"><xsl:next-match/></xsl:template>"#,
    );
    assert_eq!(transform(&s, THREE).unwrap(), "<out><high/><low/></out>");
}

/// Row 4 — `xsl:next-match`: the focus is preserved and the current rule is
/// **replaced** by the newly selected one, so a chain of `next-match` walks
/// down the candidate list exactly once each.
#[test]
fn next_match_replaces_the_current_rule_and_preserves_the_focus() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i[2]"/></out></xsl:template>
           <xsl:template match="i" priority="3">(3 p={position()})<xsl:next-match/></xsl:template>
           <xsl:template match="i" priority="2">(2 p={position()})<xsl:next-match/></xsl:template>
           <xsl:template match="i" priority="1">(1 v=<xsl:value-of select="."/>)</xsl:template>"#,
    );
    // The AVT braces are literal text here, so only the last row's value-of
    // interpolates; what matters is that each rule runs once, in order.
    assert_eq!(
        transform(&s, THREE).unwrap(),
        "<out>(3 p={position()})(2 p={position()})(1 v=b)</out>"
    );
}

/// Row 5 — `xsl:for-each`: the focus is replaced, `current()` follows it, and
/// the frame is the **same** one (its slots are re-assigned per iteration).
#[test]
fn for_each_replaces_the_focus_and_reuses_the_frame() {
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="outer" select="'o'"/>
             <out><xsl:for-each select="doc/i">
               <xsl:variable name="inner" select="concat($outer, position())"/>
               <at p="{position()}" l="{last()}" v="{.}" c="{current()}" x="{$inner}"/>
             </xsl:for-each></out>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><at p="1" l="3" v="a" c="a" x="o1"/><at p="2" l="3" v="b" c="b" x="o2"/>"#
            .to_owned()
            + r#"<at p="3" l="3" v="c" c="c" x="o3"/></out>"#
    );
}

/// Row 5, correction — `xsl:for-each` **clears** the current template rule.
///
/// §5.4: "When an `xsl:for-each`, `xsl:for-each-group`, or
/// `xsl:analyze-string` instruction is evaluated … the current template rule
/// becomes null for the evaluation of that instruction", so an
/// `xsl:next-match` inside one is XTDE0560. (The design sketch's §9.1 table
/// says "preserved" for this row; the spec says otherwise.)
#[test]
fn for_each_clears_the_current_template_rule() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i[1]"/></out></xsl:template>
           <xsl:template match="i" priority="2">
             <xsl:for-each select="."><xsl:next-match/></xsl:for-each>
           </xsl:template>
           <xsl:template match="i" priority="1"><low/></xsl:template>"#,
    );
    let e = transform(&s, THREE).expect_err("next-match inside for-each has no current rule");
    assert_eq!(e.code, "XTDE0560");
}

/// Row 9 — a pattern predicate: the candidate node is the focus, its position
/// is its position **among the siblings the step selects**, and `current()` is
/// the candidate, not the node `xsl:apply-templates` was looking at.
#[test]
fn a_pattern_predicate_sees_the_candidate_as_its_focus_and_current() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/i"/></out></xsl:template>
           <xsl:template match="i[current() = 'b']"><chosen/></xsl:template>
           <xsl:template match="i"/>"#,
    );
    assert_eq!(transform(&s, THREE).unwrap(), "<out><chosen/></out>");
}

/// Row 12 — a global variable: the focus is a singleton on the **root of the
/// tree containing the initial context node**, whatever the focus was where it
/// is first referenced.
#[test]
fn a_global_variable_is_evaluated_with_the_root_as_its_focus() {
    let s = sheet(
        r#"<xsl:variable name="where" select="name(.)"/>
           <xsl:variable name="size" select="last()"/>
           <xsl:template match="/"><out><xsl:for-each select="doc/i">
             <xsl:if test="position() = 2">
               <at here="{name(.)}" global="{$where}" gsize="{$size}"/>
             </xsl:if>
           </xsl:for-each></out></xsl:template>"#,
    );
    // `name()` of a document node is the empty string, and its singleton focus
    // has size 1 — neither is the `i` the reference was made from.
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><at here="i" global="" gsize="1"/></out>"#
    );
}

/// Row 13 — `xsl:variable` with content: everything is preserved except the
/// **output state**, which becomes a temporary tree, so the content does not
/// reach the result.
#[test]
fn a_variable_with_content_diverts_the_output_but_nothing_else() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:for-each select="doc/i">
             <xsl:variable name="t"><kept p="{position()}" v="{.}"/></xsl:variable>
             <xsl:if test="position() = 1">
               <xsl:copy-of select="$t"/><after p="{position()}" v="{.}"/>
             </xsl:if>
           </xsl:for-each></out></xsl:template>"#,
    );
    // The `kept` element appears once, because it was built into `$t` and only
    // the first iteration copies it out — not three times, which is what would
    // happen if the variable's content had gone straight to the result.
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><kept p="1" v="a"/><after p="1" v="a"/></out>"#
    );
}

/// Row 15 — `xsl:message`: the focus, the frame and the tunnel parameters are
/// all preserved, and the output state is a temporary tree of its own, so the
/// message content does not reach the result.
#[test]
fn a_message_preserves_the_context_and_writes_elsewhere() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:for-each select="doc/i">
             <xsl:message><m p="{position()}" v="{.}"/></xsl:message>
           </xsl:for-each></out></xsl:template>"#,
    );
    assert_eq!(transform(&s, THREE).unwrap(), "<out/>");
    assert_eq!(
        messages(&s, THREE),
        vec![
            r#"<m p="1" v="a"/>"#.to_string(),
            r#"<m p="2" v="b"/>"#.to_string(),
            r#"<m p="3" v="c"/>"#.to_string(),
        ]
    );
}

/// Tunnel parameters travel through constructs that do not mention them, and
/// an ordinary parameter does not (§10.1.2).
#[test]
fn tunnel_parameters_travel_and_ordinary_ones_do_not() {
    let s = sheet(
        r#"<xsl:template match="/">
             <out><xsl:apply-templates select="doc">
               <xsl:with-param name="t" select="'tunnel'" tunnel="yes"/>
               <xsl:with-param name="p" select="'plain'"/>
             </xsl:apply-templates></out>
           </xsl:template>
           <xsl:template match="doc">
             <xsl:param name="p" select="'default-at-doc'"/>
             <doc p="{$p}"><xsl:apply-templates select="i[1]"/></doc>
           </xsl:template>
           <xsl:template match="i">
             <xsl:param name="t" tunnel="yes"/>
             <xsl:param name="p" select="'default-at-i'"/>
             <i t="{$t}" p="{$p}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><doc p="plain"><i t="tunnel" p="default-at-i"/></doc></out>"#
    );
}

/// `current()` is the focus of the innermost *instruction*, which is what
/// makes it differ from `.` inside a predicate (§16.6.2).
#[test]
fn current_is_the_instruction_focus_not_the_predicate_focus() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:for-each select="doc/i">
             <at v="{.}" picked="{../i[. = current()]/name()}"/>
           </xsl:for-each></out></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, THREE).unwrap(),
        r#"<out><at v="a" picked="i"/><at v="b" picked="i"/><at v="c" picked="i"/></out>"#
    );
}
