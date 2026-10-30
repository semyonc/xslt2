//! Grouping patterns that read local variables, the group sort key in XTTE1020, and keys whose
//! specifier sees the current group.
//!
//! * A positional predicate after one that reads a local variable would make a
//!   `group-starting-with` / `group-ending-with` pattern quadratic if its
//!   sequence were rebuilt for every population node: the run-wide placement
//!   cache refuses a sequence that depends on a local. Within one evaluation of
//!   the instruction the frame cannot change, so the matcher remembers such
//!   sequences for that evaluation (`runtime/patterns.rs` `EvaluationScope`). The
//!   tests pin the speed, the answers (against the per-candidate path and against
//!   a reference), and that nothing is carried from one evaluation to the next.
//! * XTTE1020 for a group sort key names the group, not "item N".
//! * A key's `use` (or sequence constructor) is evaluated with the current group
//!   of the `key()` call, and a key whose specifier may observe the group
//!   (decided at compile time, `Key::observes_group`) keeps an index per group,
//!   dropped when the group's evaluation ends; every other key keeps one per
//!   document for the run.
//!
//! (The linearity test of `group-by` is in `grouping.rs`.)

mod common;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Wraps top-level content in a version 2.0 stylesheet with the `xs` and `f`
/// prefixes bound.
fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{body}</xsl:stylesheet>"#
    )
}

const TEST_STACK: usize = 256 * 1024 * 1024;

fn run(main: &str, source: &str) -> Result<String, XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///grouping/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///grouping/in.xml", &source)?;
        compiled.serialize_primary(&outcome).map(common::body)
    })
}

fn out(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => text,
        Err(e) => panic!("the transformation failed: {e}"),
    }
}

/// A flat `<doc>` of `n` `r` elements whose `k` cycles through 0, 1, 2.
fn flat(n: usize) -> String {
    let mut source = String::from("<doc>");
    for i in 0..n {
        source.push_str(&format!(r#"<r k="{}"/>"#, i % 3));
    }
    source.push_str("</doc>");
    source
}

/// The sizes of the groups `group-starting-with` (`starting`) or
/// `group-ending-with` forms over `n` nodes of which those at `matches` match
/// the pattern — §14.3, written out: "If a node matches the pattern, or is the
/// first node in the population, then a new group is created"; "If a node is
/// the first node in the population, or if the previous node in the
/// population matches the pattern, then a new group is created".
fn reference_sizes(n: usize, matches: impl Fn(usize) -> bool, starting: bool) -> Vec<usize> {
    let mut sizes: Vec<usize> = Vec::new();
    for i in 0..n {
        let starts = i == 0 || if starting { matches(i) } else { matches(i - 1) };
        if starts {
            sizes.push(1);
        } else {
            *sizes.last_mut().expect("the first node starts a group") += 1;
        }
    }
    sizes
}

fn joined(sizes: &[usize]) -> String {
    sizes
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

// ── positional predicates after a local-variable predicate ─────────────────

/// The shape `r[@k = $v][position() mod 100 = 1]`, over 625 and 5,000
/// siblings: each parent's sequence S1 (the `r` with `@k = $v`) is built once
/// per evaluation of the instruction, so the match is linear
/// (`common::scaling`). Rebuilt for every candidate, it took 8 s for 5,000
/// siblings in a release build (126 s at 20,000), minutes in a debug build.
///
/// XPath 2.0 §3.2.2: "the predicates are applied from left to right, and the
/// result of applying each predicate serves as the input sequence for the
/// following predicate" — so the 1st, 101st, 201st … `r` with `k = 0` match:
/// the `r` at 0, 300, 600, … in the flat document.
#[test]
fn a_positional_predicate_after_a_local_variable_is_linear() {
    let matches = |i: usize| i % 3 == 0 && (i / 3) % 100 == 0;
    for (attribute, starting) in [("group-starting-with", true), ("group-ending-with", false)] {
        let main = sheet(&format!(
            r#"<xsl:template match="/">
                 <xsl:variable name="v" select="'0'"/>
                 <xsl:variable name="sizes" as="xs:integer*">
                   <xsl:for-each-group select="doc/r" {attribute}="r[@k = $v][position() mod 100 = 1]">
                     <xsl:sequence select="count(current-group())"/>
                   </xsl:for-each-group>
                 </xsl:variable>
                 <out><xsl:value-of select="$sizes"/></out>
               </xsl:template>"#
        ));
        common::scaling::assert_linear(attribute, 625, 8, |n| {
            let source = flat(n);
            let mut result = String::new();
            let elapsed = common::scaling::timed(|| result = out(&main, &source));
            assert_eq!(
                result,
                format!(
                    "<out>{}</out>",
                    joined(&reference_sizes(n, matches, starting))
                ),
                "{attribute}"
            );
            elapsed
        });
    }
}

/// An `id(…)` / `key(…)` anchor whose argument is a local variable had the
/// same cost when its selection was computed again for every candidate (0.7 s
/// at 5,000, 11 s at 20,000, release). It is kept for the evaluation, as a
/// literal argument's is kept for the run, so the match is linear from 2,500
/// to 20,000 siblings. §5.5.2: `key` "with a StringLiteral … and a KeyValue …
/// a Literal or a VarRef".
#[test]
fn a_key_pattern_with_a_local_argument_is_linear() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="@k"/>
           <xsl:template match="/">
             <xsl:variable name="v" select="'1'"/>
             <xsl:variable name="sizes" as="xs:integer*">
               <xsl:for-each-group select="doc/r" group-starting-with="key('k', $v)">
                 <xsl:sequence select="count(current-group())"/>
               </xsl:for-each-group>
             </xsl:variable>
             <out><xsl:value-of select="count($sizes), sum($sizes), $sizes[1], $sizes[2]"/></out>
           </xsl:template>"#,
    );
    common::scaling::assert_linear("key('k', $v)", 2_500, 8, |n| {
        let source = flat(n);
        let mut result = String::new();
        let elapsed = common::scaling::timed(|| result = out(&main, &source));
        // The first node starts a group; then every r with k = 1 starts one.
        let sizes = reference_sizes(n, |i| i % 3 == 1, true);
        assert_eq!(
            result,
            format!("<out>{} {n} {} {}</out>", sizes.len(), sizes[0], sizes[1])
        );
        elapsed
    });
}

/// The shapes the evaluation scope remembers, compared with the same pattern
/// forced onto the per-candidate path. `exists(current())` is true for every
/// candidate (§16.6.1: in a pattern `current()` "is the node that is being
/// matched against the pattern"), so `[P and exists(current())]` selects what
/// `[P]` selects — but a predicate that calls `current()` is never remembered,
/// in any scope, so the twin rebuilds every sequence for every candidate,
/// which is what the matcher did for every one of these patterns before.
///
/// The population spans several parents and is processed in reverse
/// document order, so sequences are per parent and the population order is
/// not the sibling order; every group is identified by its size and its
/// initial node. And the instruction runs once per value of `$v`, rebound by
/// `xsl:for-each` **in the same frame slot**: a scope that outlived one
/// evaluation would hand the next one the sequences of the previous value.
#[test]
fn the_evaluation_scope_agrees_with_the_per_candidate_path() {
    // (remembered for the evaluation, twin on the per-candidate path)
    let shapes = [
        (
            "r[@k = $v][position() mod 3 = 1]",
            "r[@k = $v and exists(current())][position() mod 3 = 1]",
        ),
        (
            "r[@k = $v][position() = last() - 1]",
            "r[@k = $v and exists(current())][position() = last() - 1]",
        ),
        ("r[@k = $v][2]", "r[@k = $v and exists(current())][2]"),
        (
            "r[@k = $v][position() > 1][1]",
            "r[@k = $v and exists(current())][position() > 1][1]",
        ),
        (
            "r[@k = $v][@m = '1'][position() mod 2 = 0]",
            "r[@k = $v and exists(current())][@m = '1'][position() mod 2 = 0]",
        ),
        (
            "r[@m = '1'][@k != $v][last()]",
            "r[@m = '1' and exists(current())][@k != $v][last()]",
        ),
        // A step above the candidate: the `sec` sequence among `doc`'s
        // children, for a candidate `r`.
        (
            "sec[@t = $v][2]/r",
            "sec[@t = $v and exists(current())][2]/r",
        ),
        (
            "sec[@t = $v][1]//r[@k = $v][1]",
            "sec[@t = $v and exists(current())][1]//r[@k = $v and exists(current())][1]",
        ),
        // The anchors: kept for the evaluation, against the equivalent step.
        ("key('k', $v)", "r[@k = $v and exists(current())]"),
        ("key('k', $v)/x", "r[@k = $v and exists(current())]/x"),
        // A stylesheet function is not remembered in either form: the same
        // path twice, to show the refusal leaves the answer alone.
        (
            "r[f:eq(@k, $v)][position() mod 2 = 1]",
            "r[f:eq(@k, $v) and exists(current())][position() mod 2 = 1]",
        ),
    ];
    let mut source = String::from("<doc>");
    for (s, size) in [7usize, 1, 12, 5, 9, 4].into_iter().enumerate() {
        source.push_str(&format!(r#"<sec t="{}">"#, s % 3));
        for i in 0..size {
            source.push_str(&format!(
                r#"<r k="{}" m="{}"><x/></r>"#,
                (i + s) % 3,
                (i / 2) % 2
            ));
        }
        source.push_str("</sec>");
    }
    source.push_str("</doc>");

    let mut body = String::new();
    for (index, (remembered, per_candidate)) in shapes.iter().enumerate() {
        // `key(…)/x` and `…/x` patterns group the `x`; the others the `r`.
        let select = if remembered.ends_with("/x") {
            "reverse(//x)"
        } else {
            "reverse(//r)"
        };
        for attribute in ["group-starting-with", "group-ending-with"] {
            for (tag, pattern) in [("a", remembered), ("b", per_candidate)] {
                body.push_str(&format!(
                    r#"<{tag} shape="{index}" form="{attribute}"><xsl:for-each-group select="{select}" {attribute}="{pattern}"><xsl:value-of select="count(current-group()), generate-id(current-group()[1])"/><xsl:text>;</xsl:text></xsl:for-each-group></{tag}>"#
                ));
            }
        }
    }
    let main = sheet(&format!(
        r#"<xsl:key name="k" match="r" use="@k"/>
           <xsl:function name="f:eq" as="xs:boolean">
             <xsl:param name="a"/><xsl:param name="b"/>
             <xsl:sequence select="string($a) = string($b)"/>
           </xsl:function>
           <xsl:template match="/">
             <out><xsl:variable name="doc" select="."/>
               <xsl:for-each select="'0', '1', '2', 'none'">
                 <xsl:variable name="v" select="."/>
                 <case v="{{$v}}"><xsl:for-each select="$doc">{body}</xsl:for-each></case>
               </xsl:for-each>
             </out>
           </xsl:template>"#
    ));
    let result = out(&main, &source);

    let between = |text: &str, open: &str, close: &str| -> Vec<String> {
        text.split(open)
            .skip(1)
            .map(|rest| rest.split(close).next().unwrap_or("").to_string())
            .collect()
    };
    let remembered = between(&result, "<a ", "</a>");
    let per_candidate = between(&result, "<b ", "</b>");
    assert_eq!(remembered.len(), shapes.len() * 2 * 4, "{result}");
    assert_eq!(remembered, per_candidate);
    // Not vacuous: the answers depend on `$v`, and most shapes split the
    // population into several groups.
    let several = remembered
        .iter()
        .filter(|groups| groups.matches(';').count() > 1)
        .count();
    assert!(several > remembered.len() / 2, "{result}");
    let cases = between(&result, "<case ", "</case>");
    assert_eq!(cases.len(), 4);
    assert!(cases[0] != cases[1] && cases[1] != cases[2], "{result}");
}

/// The same instruction evaluated in frames of its own: a recursive template
/// whose parameter changes at every level, the grouping run at each level
/// before and after the recursive call. Each evaluation must see its own
/// `$v` — 1, 2, 3, then back to 2 and 1 on the way out.
#[test]
fn every_evaluation_sees_its_own_frame() {
    let source = flat(30);
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:call-template name="level"><xsl:with-param name="v" select="1"/></xsl:call-template></out></xsl:template>
           <xsl:template name="level">
             <xsl:param name="v" as="xs:integer"/>
             <xsl:variable name="groups" as="xs:integer*">
               <xsl:for-each-group select="/doc/r" group-starting-with="r[@k = string($v mod 3)][position() mod 2 = 1]">
                 <xsl:sequence select="count(current-group())"/>
               </xsl:for-each-group>
             </xsl:variable>
             <in v="{$v}"><xsl:value-of select="$groups"/></in>
             <xsl:if test="$v lt 3">
               <xsl:call-template name="level"><xsl:with-param name="v" select="$v + 1"/></xsl:call-template>
             </xsl:if>
             <xsl:for-each-group select="/doc/r" group-starting-with="r[@k = string($v mod 3)][position() mod 2 = 1]">
               <xsl:if test="position() = 1"><back v="{$v}"><xsl:value-of select="count(current-group())"/></back></xsl:if>
             </xsl:for-each-group>
           </xsl:template>"#,
    );
    let sizes = |k: usize| reference_sizes(30, move |i| i % 3 == k && (i / 3) % 2 == 0, true);
    let expected = format!(
        r#"<out><in v="1">{}</in><in v="2">{}</in><in v="3">{}</in><back v="3">{}</back><back v="2">{}</back><back v="1">{}</back></out>"#,
        joined(&sizes(1)),
        joined(&sizes(2)),
        joined(&sizes(0)),
        sizes(0)[0],
        sizes(2)[0],
        sizes(1)[0]
    );
    assert_eq!(out(&main, &source), expected);
}

/// What the scope must not remember: a predicate that reads `current()`.
/// §16.6.1: "If the current function is used within a pattern, its value is
/// the node that is being matched against the pattern" — a different node for
/// every population node of one evaluation. In `r[. is current()][1]` the
/// sequence S1 is the candidate alone, so every `r` matches and starts a
/// group; a sequence remembered from the first candidate would have matched
/// only that one.
#[test]
fn a_predicate_reading_current_is_decided_per_candidate() {
    let source = flat(6);
    let main = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="v" select="'0'"/>
             <xsl:variable name="sizes" as="xs:integer*">
               <xsl:for-each-group select="doc/r" group-starting-with="r[. is current()][1]">
                 <xsl:sequence select="count(current-group())"/>
               </xsl:for-each-group>
               <xsl:sequence select="0"/>
               <xsl:for-each-group select="doc/r" group-starting-with="r[@k = $v or . is current()][last()]">
                 <xsl:sequence select="count(current-group())"/>
               </xsl:for-each-group>
             </xsl:variable>
             <out><xsl:value-of select="$sizes"/></out>
           </xsl:template>"#,
    );
    // Second pattern: S1 = the r with k = 0 (the 1st and 4th) plus the
    // candidate, so the candidate is last in it from the 4th r on: groups
    // [1 2 3] [4] [5] [6]. Remembered from the first candidate tried (the
    // 2nd r), S1 would hold the 1st, 2nd and 4th, and only the 4th would
    // match: [1 2 3] [4 5 6].
    assert_eq!(out(&main, &source), "<out>1 1 1 1 1 1 0 3 1 1 1</out>");
}

// ── XTTE1020 names the group ───────────────────────────────────────────────

/// §13.1.2 \[ERR XTTE1020\]: "If any sort key value, after atomization and any
/// type conversion required by the data-type attribute, is a sequence
/// containing more than one item, then … this is a type error." For the groups
/// of `xsl:for-each-group` the value is computed per group (§14.3: "The select
/// expression of an xsl:sort element is evaluated once for each group"), and
/// the message says which group; for an ordinary sort, which item.
#[test]
fn xtte1020_names_a_group_or_an_item() {
    let source = "<doc/>";
    let grouping = sheet(
        r#"<xsl:template match="/"><out>
             <xsl:for-each-group select="1 to 4" group-by=". mod 2">
               <xsl:sort select="if (current-grouping-key() = 0) then (1, 2) else 1"/>
               <g/>
             </xsl:for-each-group>
           </out></xsl:template>"#,
    );
    let e = run(&grouping, source).expect_err("a two-item group sort key");
    assert_eq!(e.code, "XTTE1020");
    assert!(
        e.message.contains("of group 2 is a sequence of 2 items"),
        "{}",
        e.message
    );

    let items = sheet(
        r#"<xsl:template match="/"><out>
             <xsl:for-each select="1 to 3">
               <xsl:sort select="if (. = 2) then (1, 2) else ."/>
               <i/>
             </xsl:for-each>
           </out></xsl:template>"#,
    );
    let e = run(&items, source).expect_err("a two-item sort key");
    assert_eq!(e.code, "XTTE1020");
    assert!(
        e.message.contains("of item 2 is a sequence of 2 items"),
        "{}",
        e.message
    );
}

// ── a key's specifier sees the current group of the call ───────────────────
//
// §16.3.1: "If the use attribute is present, its value is an expression
// specifying the values of the key. The expression will be evaluated with the
// node that matches the pattern as the context node." — nothing else of the
// context is changed. §5.4.4's table lists, for the current group and the
// current grouping key, "Set by: xsl:for-each-group" and "Cleared by: calls on
// stylesheet functions" (where the current template rule and the current mode
// are also cleared for "the sequence constructors contained in xsl:key"), and
// §14.1: "While an xsl:for-each-group instruction is being evaluated, the
// current group will be non-empty. At other times, it will be an empty
// sequence." So the key specifier evaluated for a `key()` call made inside a
// group sees that group and its grouping key — and an index built for one
// group must not answer a call made in another group, or outside any.

/// Runs `main` over `source`; the serialized result and the `xsl:message`
/// texts, in order.
fn run_with_messages(main: &str, source: &str) -> Result<(String, Vec<String>), XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///grouping/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///grouping/in.xml", &source)?;
        let messages = outcome.messages.iter().map(|m| m.content.clone()).collect();
        Ok((
            compiled.serialize_primary(&outcome).map(common::body)?,
            messages,
        ))
    })
}

/// One group holding both `r`,
/// so `count(current-group())` is 2 for each of them while the key is used
/// inside that group. A specifier that saw no group would give every `r` the
/// key value 0, and the call would return nothing (Saxon: 2).
#[test]
fn a_key_use_sees_the_current_group_of_the_call() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="count(current-group())"/>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="1">
               <xsl:value-of select="count(key('k', count(current-group())))"/>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main, "<doc><r/><r/></doc>"), "<out>2</out>");
}

/// One key, called from two groups of different sizes: each call is answered
/// with the values its own group gives. In the group `a` (two members) every
/// `r` has the key value 2; in the group `b` (one member) every `r` has 1. An
/// index built for `a` and reused for `b` would answer `0 3` there instead of
/// `3 0`. The groups are also sorted by a key that calls `key()`, which is
/// evaluated with the group as the current group too (§14.3: "the current
/// group is the group whose sort key value is being determined").
#[test]
fn each_group_gets_its_own_index() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="count(current-group())"/>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="@g">
               <xsl:sort select="count(key('k', 1))"/>
               <g name="{current-grouping-key()}"><xsl:value-of select="count(key('k', 1)), count(key('k', 2))"/></g>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(&main, r#"<doc><r g="a"/><r g="a"/><r g="b"/></doc>"#),
        r#"<out><g name="a">0 3</g><g name="b">3 0</g></out>"#
    );
}

/// Outside any group the current group is the empty sequence (§14.1: "At
/// other times, it will be an empty sequence"), so every `r` has the key value
/// 0 — before the grouping, and again after it: neither the index built
/// outside the group answers inside it, nor the group's index outside.
#[test]
fn outside_any_group_the_current_group_is_empty() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="count(current-group())"/>
           <xsl:template match="/">
             <out>
               <before><xsl:value-of select="count(key('k', 0)), count(key('k', 2))"/></before>
               <xsl:for-each-group select="doc/r" group-by="1">
                 <in><xsl:value-of select="count(key('k', 0)), count(key('k', 2))"/></in>
               </xsl:for-each-group>
               <after><xsl:value-of select="count(key('k', 0)), count(key('k', 2))"/></after>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(&main, "<doc><r/><r/></doc>"),
        "<out><before>2 0</before><in>0 2</in><after>2 0</after></out>"
    );
}

/// `current-grouping-key()` in `use`: §14.2 "While an xsl:for-each-group
/// instruction with a group-by or group-adjacent attribute is being evaluated,
/// the current grouping key will be a single atomic value" — every `r` has the
/// key value of the group the call is made in; and with
/// `group-starting-with` "the current grouping key is the empty sequence"
/// (§14.3), so no `r` has any key value.
#[test]
fn current_grouping_key_in_use() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="current-grouping-key()"/>
           <xsl:template match="/">
             <out>
               <xsl:for-each-group select="doc/r" group-by="@g">
                 <g name="{current-grouping-key()}"><xsl:value-of select="count(key('k', 'x')), count(key('k', 'y'))"/></g>
               </xsl:for-each-group>
               <xsl:for-each-group select="doc/r" group-starting-with="r">
                 <s><xsl:value-of select="count(key('k', 'x')), count(key('k', 'y'))"/></s>
               </xsl:for-each-group>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(&main, r#"<doc><r g="x"/><r g="y"/></doc>"#),
        r#"<out><g name="x">2 0</g><g name="y">0 2</g><s>0 0</s><s>0 0</s></out>"#
    );
}

/// A key whose sequence constructor invokes a template: §5.4.4 clears the
/// current group only for "calls on stylesheet functions", so a template
/// called (`xsl:call-template`) or applied (`xsl:apply-templates`) from the
/// key specifier sees the group of the `key()` call.
#[test]
fn a_template_called_from_the_key_content_sees_the_group() {
    let main = sheet(
        r#"<xsl:key name="called" match="r"><xsl:call-template name="size"/></xsl:key>
           <xsl:key name="applied" match="r"><xsl:apply-templates select="." mode="size"/></xsl:key>
           <xsl:template name="size"><xsl:sequence select="count(current-group())"/></xsl:template>
           <xsl:template match="r" mode="size"><xsl:sequence select="count(current-group())"/></xsl:template>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="1">
               <xsl:value-of select="count(key('called', 2)), count(key('applied', 2))"/>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main, "<doc><r/><r/></doc>"), "<out>2 2</out>");
}

/// An attribute set expanded inside the key's sequence constructor is
/// evaluated with the same context (§10.2's attribute sets are not a call on a
/// stylesheet function, so §5.4.4 leaves the current group in place), so it
/// too sees the group of the `key()` call.
#[test]
fn an_attribute_set_used_in_the_key_content_sees_the_group() {
    let main = sheet(
        r#"<xsl:attribute-set name="s"><xsl:attribute name="n" select="count(current-group())"/></xsl:attribute-set>
           <xsl:key name="k" match="r">
             <xsl:variable name="e"><e xsl:use-attribute-sets="s"/></xsl:variable>
             <xsl:sequence select="string($e/e/@n)"/>
           </xsl:key>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="1">
               <xsl:value-of select="count(key('k', '2')), count(key('k', '0'))"/>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main, "<doc><r/><r/></doc>"), "<out>2 0</out>");
}

/// A key whose specifier cannot observe the group keeps one index per
/// document for the whole run: called from three groups, its `xsl:message`
/// runs once per `r` (3), not once per `r` per group (9). The key that reads
/// `current-group()` is built once per group: 3 × 3 messages.
#[test]
fn a_group_independent_key_is_built_once() {
    let main = sheet(
        r#"<xsl:key name="plain" match="r"><xsl:message>plain</xsl:message><xsl:sequence select="string(@g)"/></xsl:key>
           <xsl:key name="grouped" match="r"><xsl:message>grouped</xsl:message><xsl:sequence select="count(current-group())"/></xsl:key>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="@g">
               <xsl:value-of select="count(key('plain', current-grouping-key())), count(key('grouped', 1))"/>
               <xsl:text>;</xsl:text>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    let (result, messages) =
        run_with_messages(&main, r#"<doc><r g="a"/><r g="b"/><r g="c"/></doc>"#)
            .expect("the transformation runs");
    assert_eq!(result, "<out>1 3;1 3;1 3;</out>");
    let count = |text: &str| messages.iter().filter(|m| m.as_str() == text).count();
    assert_eq!(count("plain"), 3, "{messages:?}");
    assert_eq!(count("grouped"), 9, "{messages:?}");
}

/// A stylesheet function called from `use` does not see the group: §14.3
/// "During the evaluation of a stylesheet function, the current group and
/// current grouping key are set to the empty sequence". Every `r` has the key
/// value 0 in every group, and the key is built once (the function's
/// `xsl:message` runs once per `r`, not once per `r` per group).
#[test]
fn a_stylesheet_function_in_use_sees_no_group() {
    let main = sheet(
        r#"<xsl:function name="f:size" as="xs:integer">
             <xsl:message>f</xsl:message>
             <xsl:sequence select="count(current-group())"/>
           </xsl:function>
           <xsl:key name="k" match="r" use="f:size()"/>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="@g">
               <xsl:value-of select="count(key('k', 0)), count(key('k', 1))"/>
               <xsl:text>;</xsl:text>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    let (result, messages) = run_with_messages(&main, r#"<doc><r g="a"/><r g="b"/></doc>"#)
        .expect("the transformation runs");
    assert_eq!(result, "<out>2 0;2 0;</out>");
    assert_eq!(messages.len(), 2, "{messages:?}");
}

/// A key whose specifier calls `key()` for a key that reads the group depends
/// on the group too: the inner call is made with the same current group. In
/// the one group of two members `inner` gives every `r` the value 2, so
/// `outer` gives each the value 2; in groups of one member, 0.
#[test]
fn a_key_calling_a_group_dependent_key_depends_on_the_group() {
    let main = sheet(
        r#"<xsl:key name="inner" match="r" use="count(current-group())"/>
           <xsl:key name="outer" match="r" use="count(key('inner', 2))"/>
           <xsl:template match="/">
             <out>
               <xsl:for-each-group select="doc/r" group-by="1">
                 <one><xsl:value-of select="count(key('outer', 2)), count(key('outer', 0))"/></one>
               </xsl:for-each-group>
               <xsl:for-each-group select="doc/r" group-by="@id">
                 <each><xsl:value-of select="count(key('outer', 2)), count(key('outer', 0))"/></each>
               </xsl:for-each-group>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(&main, r#"<doc><r id="a"/><r id="b"/></doc>"#),
        "<out><one>2 0</one><each>0 2</each><each>0 2</each></out>"
    );
}

/// §9.8: "[ERR XTDE0640] In general, a circularity in a stylesheet is a
/// non-recoverable dynamic error" — still detected for a key that reads the
/// group, whose index is kept per group.
#[test]
fn a_group_dependent_key_defined_in_terms_of_itself_is_xtde0640() {
    let main = sheet(
        r#"<xsl:key name="k" match="r" use="count(current-group()) + count(key('k', 1))"/>
           <xsl:template match="/">
             <out><xsl:for-each-group select="doc/r" group-by="1">
               <xsl:value-of select="count(key('k', 1))"/>
             </xsl:for-each-group></out>
           </xsl:template>"#,
    );
    let e = run(&main, "<doc><r/><r/></doc>").expect_err("a circular key");
    assert_eq!(e.code, "XTDE0640", "{e}");
}
