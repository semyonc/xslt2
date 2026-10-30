//! Grouping: `xsl:for-each-group`, `current-group()` and `current-grouping-key()`
//! (§14).
//!
//! Every test quotes the sentence of XSLT 2.0 it checks. The sections: the four
//! grouping attributes and their static errors, the comparison of grouping keys
//! (typed values, NaN, collations), the order and sorting of the groups, the
//! dynamic context of the body and of the sort keys, the current group and
//! grouping key everywhere else, the dynamic errors, and linearity.

mod common;

use std::rc::Rc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::collation::{Collation, HTML_ASCII_CASE_INSENSITIVE_URI};
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

/// A stylesheet whose only template matches `/` and has `content` inside an
/// `out` element.
fn root(content: &str) -> String {
    sheet(&format!(
        r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
    ))
}

const TEST_STACK: usize = 256 * 1024 * 1024;

/// Compiles `main` against `functions` (built by `setup`) and runs it over
/// `source`.
fn run_with(
    setup: impl FnOnce(&mut XsltFunctions) + Send + 'static,
    main: &str,
    source: &str,
) -> Result<String, XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let mut functions = XsltFunctions::new();
        setup(&mut functions);
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

fn run(main: &str, source: &str) -> Result<String, XsltError> {
    run_with(|_| {}, main, source)
}

fn out(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => text,
        Err(e) => panic!("the transformation failed: {e}"),
    }
}

fn code_of(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => panic!("expected an error, got {text:?}"),
        Err(e) => e.code.to_string(),
    }
}

const CITIES: &str = r#"<cities>
  <city name="Milano" country="Italia" pop="5"/>
  <city name="Paris" country="France" pop="7"/>
  <city name="Muenchen" country="Deutschland" pop="4"/>
  <city name="Lyon" country="France" pop="2"/>
  <city name="Venezia" country="Italia" pop="1"/>
</cities>"#;

/// One `<g>` per group: the key, then the names of the group's members.
const PER_GROUP: &str =
    r#"<g k="{current-grouping-key()}"><xsl:value-of select="current-group()/@name"/></g>"#;

// ── §14.3 group-by ──────────────────────────────────────────────────────────

/// §14.3: "If the group-by attribute is present, the items in the population
/// are examined, in population order. For each item J, the expression in the
/// group-by attribute is evaluated to produce a sequence of zero or more
/// grouping key values. For each one of these grouping keys, if there is
/// already a group created to hold items having that grouping key value, J is
/// added to that group; otherwise a new group is created for items with that
/// grouping key value, and J becomes its first member." Without `xsl:sort`,
/// "the processing order of the groups is the order of first appearance".
#[test]
fn group_by_groups_in_order_of_first_appearance() {
    let main = root(&format!(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">{PER_GROUP}</xsl:for-each-group>"#
    ));
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g k="Italia">Milano Venezia</g><g k="France">Paris Lyon</g><g k="Deutschland">Muenchen</g></out>"#
    );
}

/// §14.3: "The item is included in as many groups as there are distinct
/// grouping keys (which may be zero)." … "An item will never be assigned more
/// than once to the same group; if two or more grouping keys for the same item
/// are equal, then the duplicates are ignored."
#[test]
fn group_by_keys_may_be_many_none_or_repeated() {
    let source = r#"<doc>
      <w id="1" t="a b a"/>
      <w id="2" t=""/>
      <w id="3" t="b c"/>
    </doc>"#;
    let main = root(
        r#"<xsl:for-each-group select="doc/w" group-by="tokenize(@t, ' ')[. ne '']">
             <g k="{current-grouping-key()}"><xsl:value-of select="current-group()/@id"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, source),
        r#"<out><g k="a">1</g><g k="b">1 3</g><g k="c">3</g></out>"#
    );
}

/// §14.3: "An item here means the item at a particular position within the
/// population—if the population contains the same node at several different
/// positions in the sequence then a group may indeed contain duplicate nodes."
#[test]
fn the_same_node_twice_in_the_population_is_two_members() {
    let main = root(
        r#"<xsl:variable name="c" select="cities/city[1]"/>
           <xsl:for-each-group select="($c, $c)" group-by="1"><n><xsl:value-of select="count(current-group())"/></n></xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, CITIES), "<out><n>2</n></out>");
}

/// §14.3: "When calculating grouping keys for an item in the population, the
/// expression contained in the group-by or group-adjacent attribute is
/// evaluated with that item as the context item, with its position in
/// population order as the context position, and with the size of the
/// population as the context size."
#[test]
fn the_key_expression_sees_the_population_focus() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="if (position() * 2 le last()) then 'first' else 'second'">
             <g k="{current-grouping-key()}"><xsl:value-of select="current-group()/@name"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g k="first">Milano Paris</g><g k="second">Muenchen Lyon Venezia</g></out>"#
    );
    // §16.6.1: "For an outermost expression … the current item is always the
    // same as the context item."
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="current()/@country">
             <xsl:value-of select="current-grouping-key()"/><xsl:text>;</xsl:text>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, CITIES), "<out>Italia;France;Deutschland;</out>");
}

// ── §14.3 comparison of grouping keys ──────────────────────────────────────

/// §14.3: "Grouping keys are compared using the rules for the eq operator
/// appropriate to their dynamic type." Numbers of different types that are
/// `eq` share a group; §14.2: the grouping key is "the grouping key of the
/// initial item in the group" — here an `xs:integer`.
#[test]
fn numbers_of_mixed_types_group_by_value() {
    let main = root(
        r#"<xsl:for-each-group select="(1, 2.0, xs:float(1), xs:double(2), 1.0, xs:double(3))" group-by=".">
             <g n="{count(current-group())}" integer="{current-grouping-key() instance of xs:integer}"><xsl:value-of select="current-grouping-key()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, "<r/>"),
        r#"<out><g n="3" integer="true">1</g><g n="2" integer="false">2</g><g n="1" integer="false">3</g></out>"#
    );
}

/// `eq` on `xs:dateTime` compares instants, so the same moment written in two
/// timezones is one grouping key (the §16.3.2 Note's point, for keys).
#[test]
fn date_times_in_different_timezones_share_a_group() {
    let main = root(
        r#"<xsl:for-each-group select="(xs:dateTime('2000-01-01T12:00:00Z'), xs:dateTime('2000-01-01T13:00:00+01:00'), xs:dateTime('2000-01-01T12:00:00+01:00'))" group-by=".">
             <g n="{count(current-group())}"><xsl:value-of select="current-grouping-key()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, "<r/>"),
        r#"<out><g n="2">2000-01-01T12:00:00Z</g><g n="1">2000-01-01T12:00:00+01:00</g></out>"#
    );
}

/// §14.3: "For the purposes of grouping, the value NaN is considered equal to
/// itself." (`eq` says the opposite.)
#[test]
fn nan_is_one_grouping_key() {
    let main = root(
        r#"<xsl:for-each-group select="(number('x'), 1, xs:double('NaN'), xs:float('NaN'))" group-by=".">
             <g n="{count(current-group())}"><xsl:value-of select="current-grouping-key()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, "<r/>"),
        r#"<out><g n="3">NaN</g><g n="1">1</g></out>"#
    );
}

/// §14.3: "The grouping keys are the items in the sequence obtained by
/// evaluating the expression …, atomizing the result, and then casting an
/// xs:untypedAtomic value to xs:string." … "Two items that are not comparable
/// using the eq operator are considered to be not equal, that is, they are
/// allocated to different groups." So the untyped `"5"` is a string, which is
/// not comparable with the integer `5`: two groups, and no error.
#[test]
fn untyped_keys_are_strings_and_incomparable_keys_differ() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city[1]/@pop, 5, '5', xs:untypedAtomic('5')" group-by=".">
             <g n="{count(current-group())}" string="{current-grouping-key() instance of xs:string}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g n="3" string="true"/><g n="1" string="false"/></out>"#
    );
}

/// §14.5: "the processor identifies those existing groups G such that the
/// grouping key of the initial item of G is equal to K" — the example's
/// `xs:float` and `xs:double` both equal the `xs:decimal` but not each other.
/// The decimal joins the float's group (the key of *its* initial item); the
/// double equals no group's key and starts its own.
#[test]
fn non_transitive_numeric_keys_follow_section_14_5() {
    let main = root(
        r#"<xsl:for-each-group select="xs:float('1.0'), xs:decimal('1.0000000000100000000001'), xs:double('1.00000000001')" group-by=".">
             <g n="{count(current-group())}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, "<r/>"), r#"<out><g n="2"/><g n="1"/></out>"#);
}

/// §14.3: "If the values are strings, or untyped atomic values, then if there
/// is a collation attribute the values are compared using the collation
/// specified as the effective value of the collation attribute".
#[test]
fn a_collation_attribute_compares_string_keys() {
    let source = r#"<doc><w>Apple</w><w>apple</w><w>APPLE</w><w>pear</w></doc>"#;
    let main = root(&format!(
        r#"<xsl:for-each-group select="doc/w" group-by="." collation="{HTML_ASCII_CASE_INSENSITIVE_URI}">
             <g k="{{current-grouping-key()}}"><xsl:value-of select="count(current-group())"/></g>
           </xsl:for-each-group>"#
    ));
    assert_eq!(
        out(&main, source),
        r#"<out><g k="Apple">3</g><g k="pear">1</g></out>"#
    );
    // The attribute is an attribute value template, and a relative URI is
    // "resolved if relative against the base URI of the xsl:for-each-group
    // element".
    let main = root(
        r#"<xsl:variable name="c" select="'html-ascii-case-insensitive'"/>
           <xsl:for-each-group select="doc/w" group-adjacent="." collation="{$c}" xml:base="http://www.w3.org/2005/xpath-functions/collation/">
             <g><xsl:value-of select="count(current-group())"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>3</g><g>1</g></out>");
}

/// §14.3: "If there is no collation attribute then the default collation is
/// used." §3.6.1: the `[xsl:]default-collation` attribute "also determines the
/// collation used by certain XSLT constructs (such as xsl:key and
/// xsl:for-each-group) within its scope."
#[test]
fn without_a_collation_attribute_the_default_collation_applies() {
    let source = r#"<doc><w>Apple</w><w>apple</w><w>pear</w></doc>"#;
    let main = root(&format!(
        r#"<xsl:for-each-group select="doc/w" group-by="." default-collation="{HTML_ASCII_CASE_INSENSITIVE_URI}">
             <g><xsl:value-of select="count(current-group())"/></g>
           </xsl:for-each-group>"#
    ));
    assert_eq!(out(&main, source), "<out><g>2</g><g>1</g></out>");
    // The codepoint collation otherwise.
    let main = root(
        r#"<xsl:for-each-group select="doc/w" group-by="."><g><xsl:value-of select="count(current-group())"/></g></xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>1</g><g>1</g><g>1</g></out>");
}

/// A collation a host registers works for grouping as for everything else,
/// including one that cannot produce sort keys (every string is then compared
/// with every group's key).
#[test]
fn a_host_collation_without_sort_keys_still_groups() {
    struct ByLength;
    impl Collation for ByLength {
        fn compare(&self, a: &str, b: &str) -> std::cmp::Ordering {
            a.chars().count().cmp(&b.chars().count())
        }
    }
    let source = r#"<doc><w>ab</w><w>xyz</w><w>cd</w><w>q</w><w>rst</w></doc>"#;
    let main = root(
        r#"<xsl:for-each-group select="doc/w" group-by="." collation="urn:grouping:by-length">
             <g k="{current-grouping-key()}"><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    let result = run_with(
        |functions| {
            functions.register_collation("urn:grouping:by-length", Rc::new(ByLength));
        },
        &main,
        source,
    )
    .unwrap();
    assert_eq!(
        result,
        r#"<out><g k="ab">ab cd</g><g k="xyz">xyz rst</g><g k="q">q</g></out>"#
    );
}

/// §14.3: "[ERR XTDE1110] It is a non-recoverable dynamic error if the
/// collation URI specified to xsl:for-each-group (after resolving against the
/// base URI) is a collation that is not recognized by the implementation."
#[test]
fn an_unknown_collation_is_xtde1110() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@country" collation="urn:no-such-collation"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, CITIES), "XTDE1110");
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-adjacent="@country" collation="{concat('urn:', 'nope')}"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, CITIES), "XTDE1110");
}

// ── §14.3 group-adjacent ───────────────────────────────────────────────────

/// §14.3: "If an item has the same value for the grouping key as its
/// preceding item within the population (in population order), then it is
/// assigned to the same group as its preceding item; otherwise a new group is
/// created and the item becomes its first member."
#[test]
fn group_adjacent_groups_runs_of_equal_keys() {
    let source = r#"<doc><p/><li>1</li><li>2</li><p/><p/><li>3</li></doc>"#;
    let main = root(
        r#"<xsl:for-each-group select="doc/*" group-adjacent="name()">
             <g k="{current-grouping-key()}" n="{count(current-group())}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, source),
        r#"<out><g k="p" n="1"/><g k="li" n="2"/><g k="p" n="2"/><g k="li" n="1"/></out>"#
    );
    // NaN is equal to itself here too, and incomparable keys simply differ.
    let main = root(
        r#"<xsl:for-each-group select="(number('a'), number('b'), 'x', 1, 1.0)" group-adjacent=".">
             <g n="{count(current-group())}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, "<r/>"),
        r#"<out><g n="2"/><g n="1"/><g n="2"/></out>"#
    );
}

/// §14.3: "[ERR XTTE1100] It is a type error if the grouping key evaluated
/// using the group-adjacent attribute is an empty sequence, or a sequence
/// containing more than one item."
#[test]
fn a_group_adjacent_key_must_be_one_item() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-adjacent="@missing"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, CITIES), "XTTE1100");
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-adjacent="(@name, @country)"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, CITIES), "XTTE1100");
    // group-by has no such rule: "which may be zero".
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@missing"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, CITIES), "<out/>");
}

// ── §14.3 group-starting-with, group-ending-with ───────────────────────────

const FLAT: &str = r#"<body><h1>A</h1><p>a1</p><p>a2</p><h1>B</h1><p>b1</p></body>"#;

/// §14.3: "The nodes in the population are examined in population order. If a
/// node matches the pattern, or is the first node in the population, then a
/// new group is created and the node becomes its first member. Otherwise, the
/// node is assigned to the same group as its preceding node within the
/// population."
#[test]
fn group_starting_with_starts_a_group_at_each_match() {
    let main = root(
        r#"<xsl:for-each-group select="body/*" group-starting-with="h1">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, FLAT), "<out><g>A a1 a2</g><g>B b1</g></out>");
    // The first node starts a group whether or not it matches.
    let source = r#"<body><p>x</p><h1>A</h1><p>a</p></body>"#;
    assert_eq!(out(&main, source), "<out><g>x</g><g>A a</g></out>");
}

/// The pattern is a pattern like any other (§5.5): its predicates are decided
/// for each candidate (`tests/pattern_predicates.rs`), and they may refer to local variables in scope
/// at the instruction (`for-each-group-071` uses a function parameter).
#[test]
fn group_starting_with_patterns_take_predicates_and_local_variables() {
    let source = r#"<body><p c="h">A</p><p>a1</p><p c="h">B</p><p c="x">b1</p><p>b2</p></body>"#;
    let main = root(
        r#"<xsl:for-each-group select="body/p" group-starting-with="p[@c = 'h']">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>A a1</g><g>B b1 b2</g></out>");
    let main = root(
        r#"<xsl:variable name="mark" select="'x'"/>
           <xsl:for-each-group select="body/p" group-starting-with="p[@c = $mark]">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>A a1 B</g><g>b1 b2</g></out>");
    // A positional predicate counts among the siblings, as in any pattern.
    let main = root(
        r#"<xsl:for-each-group select="body/p" group-starting-with="p[position() mod 2 = 1]">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, source),
        "<out><g>A a1</g><g>B b1</g><g>b2</g></out>"
    );
}

/// A stylesheet function parameter in the pattern (`for-each-group-071`).
#[test]
fn a_function_parameter_is_visible_in_the_pattern() {
    let source = r#"<doc><b l="1"/><b l="2"/><b l="1"/></doc>"#;
    let main = sheet(
        r#"<xsl:function name="f:split">
             <xsl:param name="nodes"/>
             <xsl:param name="level"/>
             <xsl:for-each-group select="$nodes" group-starting-with="b[@l = $level]">
               <g n="{count(current-group())}"/>
             </xsl:for-each-group>
           </xsl:function>
           <xsl:template match="/"><out><xsl:sequence select="f:split(doc/b, '1')"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, source), r#"<out><g n="2"/><g n="1"/></out>"#);
}

/// §14.3: "If a node is the first node in the population, or if the previous
/// node in the population matches the pattern, then a new group is created
/// and the node becomes its first member."
#[test]
fn group_ending_with_ends_a_group_at_each_match() {
    let source =
        r#"<doc><l>a</l><l cont="y">b</l><l>c</l><l cont="y">d</l><l cont="y">e</l><l>f</l></doc>"#;
    let main = root(
        r#"<xsl:for-each-group select="doc/l" group-ending-with="l[not(@cont)]">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, source),
        "<out><g>a</g><g>b c</g><g>d e f</g></out>"
    );
}

/// §14.2: "If the xsl:for-each-group instruction uses the group-starting-with
/// or group-ending-with attributes, then the current grouping key is the
/// empty sequence."
#[test]
fn positional_grouping_has_no_grouping_key() {
    let main = root(
        r#"<xsl:for-each-group select="body/*" group-starting-with="h1">
             <g empty="{empty(current-grouping-key())}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, FLAT),
        r#"<out><g empty="true"/><g empty="true"/></out>"#
    );
    // Nested inside value-based grouping, the inner instruction's key is the
    // empty sequence, not the outer one's (bug 18333, `for-each-group-015a`).
    let main = root(
        r#"<xsl:for-each-group select="body/*" group-by="name()">
             <xsl:for-each-group select="current-group()" group-starting-with="h1">
               <h key="{current-grouping-key()}"/>
             </xsl:for-each-group>
             <o key="{current-grouping-key()}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, FLAT),
        r#"<out><h key=""/><h key=""/><o key="h1"/><h key=""/><o key="p"/></out>"#
    );
}

/// §14.3: "[ERR XTTE1120] When the group-starting-with or group-ending-with
/// attribute is used, it is a type error if the result of evaluating the
/// select expression contains an item that is not a node."
#[test]
fn positional_grouping_of_atomic_values_is_xtte1120() {
    let main = root(
        r#"<xsl:for-each-group select="(body/*, 1)" group-starting-with="h1"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, FLAT), "XTTE1120");
    let main = root(
        r#"<xsl:for-each-group select="('a', 'b')" group-ending-with="*"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, FLAT), "XTTE1120");
}

// ── §14.3 static errors ─────────────────────────────────────────────────────

/// §14.3: "[ERR XTSE1080] These four attributes are mutually exclusive: it is
/// a static error if none of these four attributes is present, or if more than
/// one of them is present."
#[test]
fn exactly_one_grouping_attribute_is_xtse1080() {
    let none = root(r#"<xsl:for-each-group select="*"><g/></xsl:for-each-group>"#);
    assert_eq!(code_of(&none, FLAT), "XTSE1080");
    let two = root(
        r#"<xsl:for-each-group select="*" group-by="." group-adjacent="."><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&two, FLAT), "XTSE1080");
    let two = root(
        r#"<xsl:for-each-group select="*" group-starting-with="h1" group-ending-with="p"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&two, FLAT), "XTSE1080");
}

/// §14.3: "[ERR XTSE1090] It is an error to specify the collation attribute if
/// neither the group-by attribute nor group-adjacent attribute is specified."
#[test]
fn a_collation_with_positional_grouping_is_xtse1090() {
    let main = root(&format!(
        r#"<xsl:for-each-group select="body/*" group-starting-with="h1" collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"><g/></xsl:for-each-group>"#
    ));
    assert_eq!(code_of(&main, FLAT), "XTSE1090");
}

/// §14.1: "[ERR XTSE1060] It is a static error if the current-group function
/// is used within a pattern." §14.2: "[ERR XTSE1070] It is a static error if
/// the current-grouping-key function is used within a pattern." Every pattern:
/// a template's, a key's, and the grouping instruction's own.
#[test]
fn the_grouping_functions_in_a_pattern_are_static_errors() {
    let template = sheet(
        r#"<xsl:template match="p[current-group()]"/><xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&template, FLAT), "XTSE1060");
    let key = sheet(
        r#"<xsl:key name="k" match="p[current-grouping-key() = 1]" use="."/><xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&key, FLAT), "XTSE1070");
    let grouping = root(
        r#"<xsl:for-each-group select="body/*" group-starting-with="*[. is current-group()[1]]"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&grouping, FLAT), "XTSE1060");
    // Not a pattern: the same calls in an expression are fine.
    let expression = root(
        r#"<xsl:for-each-group select="body/*" group-by="name()"><xsl:if test="current-group()[current-grouping-key() = 'h1']"><h/></xsl:if></xsl:for-each-group>"#,
    );
    assert_eq!(out(&expression, FLAT), "<out><h/></out>");
}

/// The `count` and `from` patterns of `xsl:number` are patterns too
/// (`error-1060a`, `error-1070a`): the two rules hold for them (reported by the
/// pattern compiler).
#[test]
fn the_grouping_functions_in_an_xsl_number_pattern_are_static_errors() {
    let count = root(r#"<xsl:number count="p[exists(current-group())]"/>"#);
    assert_eq!(code_of(&count, FLAT), "XTSE1060");
    let from = root(r#"<xsl:number from="h1[. = current-grouping-key()]"/>"#);
    assert_eq!(code_of(&from, FLAT), "XTSE1070");
}

/// §5.5.4: "Any dynamic error or type error that occurs during the evaluation
/// of a pattern against a particular node is treated as a recoverable error …
/// The optional recovery action is to treat the pattern as not matching."
/// §16.6.1: "If the current function is used within a pattern, its value is
/// the node that is being matched against the pattern."
#[test]
fn a_grouping_pattern_is_matched_like_any_pattern() {
    let source = r#"<body><p n="x">a</p><p n="1">b</p><p n="y">c</p><p n="1">d</p></body>"#;
    let main = root(
        r#"<xsl:for-each-group select="body/p" group-starting-with="p[xs:integer(@n) = 1]">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>a</g><g>b c</g><g>d</g></out>");
    let main = root(
        r#"<xsl:for-each-group select="body/p" group-ending-with="p[current()/@n = 'x']">
             <g><xsl:value-of select="current-group()"/></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, source), "<out><g>a</g><g>b c d</g></out>");
}

// ── §14.3 sorting the groups ───────────────────────────────────────────────

/// §14.3 "Example: Sorting Groups": "you can sort the groups in order of their
/// grouping key by writing `<xsl:sort select="current-grouping-key()"/>`; or
/// you can sort the groups in order of size by writing `<xsl:sort
/// select="count(current-group())"/>`". The sort "do[es] not affect the order
/// of items within each group".
#[test]
fn groups_sort_by_key_and_by_size() {
    let main = root(&format!(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <xsl:sort select="current-grouping-key()"/>{PER_GROUP}</xsl:for-each-group>"#
    ));
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g k="Deutschland">Muenchen</g><g k="France">Paris Lyon</g><g k="Italia">Milano Venezia</g></out>"#
    );
    let main = root(&format!(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <xsl:sort select="count(current-group())" order="descending"/>
             <xsl:sort select="sum(current-group()/@pop)" data-type="number"/>{PER_GROUP}</xsl:for-each-group>"#
    ));
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g k="Italia">Milano Venezia</g><g k="France">Paris Lyon</g><g k="Deutschland">Muenchen</g></out>"#
    );
}

/// §14.3: "During this evaluation, the context item is the initial item of
/// the group, the context position is the position of this item within the
/// set of initial items (that is, one item for each group in the population)
/// in population order, the context size is the number of groups". The body's
/// position is then the position in the *sorted* order.
#[test]
fn sort_keys_see_the_initial_items_in_population_order() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <xsl:sort select="position()" order="descending"/>
             <g at="{position()}" of="{last()}" sortpos="{@name}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g at="1" of="3" sortpos="Muenchen"/><g at="2" of="3" sortpos="Paris"/><g at="3" of="3" sortpos="Milano"/></out>"#
    );
    // §13.1: the attribute value templates of xsl:sort "are evaluated using the
    // same focus as is used to evaluate the select attribute of the containing
    // instruction" — the instruction's own focus, the document node here.
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <xsl:sort select="current-grouping-key()" order="{if (self::document-node()) then 'descending' else 'ascending'}"/>
             <xsl:value-of select="current-grouping-key()"/><xsl:text>;</xsl:text>
           </xsl:for-each-group>"#,
    );
    assert_eq!(out(&main, CITIES), "<out>Italia;France;Deutschland;</out>");
}

// ── the dynamic context of the body ─────────────────────────────────────────

/// §14.3: "Within the sequence constructor, the context item is the initial
/// item of the relevant group, the context position is the position of this
/// item among the sequence of initial items (one item for each group) arranged
/// in processing order of the groups, the context size is the number of
/// groups … This has the effect that within the sequence constructor, a call
/// on position() takes successive values 1, 2, ... last()." §16.6.1:
/// `current()` is the context item of an outermost expression.
#[test]
fn the_body_focus_is_the_initial_item_and_the_group_position() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <g at="{position()}" of="{last()}" first="{@name}" current="{current()/@name}"/>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, CITIES),
        r#"<out><g at="1" of="3" first="Milano" current="Milano"/><g at="2" of="3" first="Paris" current="Paris"/><g at="3" of="3" first="Muenchen" current="Muenchen"/></out>"#
    );
}

/// §6.7: "When an xsl:for-each, xsl:for-each-group, or xsl:analyze-string
/// instruction is evaluated, … the current template rule becomes null for the
/// evaluation of that instruction" — so `xsl:next-match` inside the body is
/// "[ERR XTDE0560] … evaluated when the current template rule is null".
#[test]
fn the_current_template_rule_is_null_in_the_body() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="body"/></out></xsl:template>
           <xsl:template match="body">
             <xsl:for-each-group select="*" group-by="name()"><xsl:next-match/></xsl:for-each-group>
           </xsl:template>"#,
    );
    assert_eq!(code_of(&main, FLAT), "XTDE0560");
}

/// §6.5: "Whenever an xsl:apply-templates instruction is evaluated, the
/// current mode becomes the mode selected by this instruction. … No other
/// instruction changes the current mode."
#[test]
fn the_current_mode_is_unchanged_in_the_body() {
    let main = sheet(
        r##"<xsl:template match="/"><out><xsl:apply-templates select="body" mode="m"/></out></xsl:template>
           <xsl:template match="body" mode="m">
             <xsl:for-each-group select="*" group-by="name()"><xsl:apply-templates select="." mode="#current"/></xsl:for-each-group>
           </xsl:template>
           <xsl:template match="*" mode="m"><m><xsl:value-of select="name()"/></m></xsl:template>
           <xsl:template match="*"><d/></xsl:template>"##,
    );
    assert_eq!(out(&main, FLAT), "<out><m>h1</m><m>p</m></out>");
}

// ── §14.1, §14.2: the current group and key elsewhere ──────────────────────

/// §14.1: "While an xsl:for-each-group instruction is being evaluated, the
/// current group will be non-empty. At other times, it will be an empty
/// sequence." §14.2: the current grouping key "At other times … will be the
/// empty sequence". Not an error in XSLT 2.0 (`for-each-group-081a`).
#[test]
fn outside_grouping_both_functions_return_the_empty_sequence() {
    let main =
        root(r#"<a group="{empty(current-group())}" key="{empty(current-grouping-key())}"/>"#);
    assert_eq!(
        out(&main, FLAT),
        r#"<out><a group="true" key="true"/></out>"#
    );
    // Nor in a global variable, nor in the xsl:sort of an xsl:for-each that is
    // not inside a grouping instruction.
    let main = sheet(
        r#"<xsl:variable name="g" select="count(current-group())"/>
           <xsl:template match="/"><out g="{$g}">
             <xsl:for-each select="body/*"><xsl:sort select="count(current-group())"/><xsl:value-of select="count(current-group())"/></xsl:for-each>
           </out></xsl:template>"#,
    );
    assert_eq!(out(&main, FLAT), r#"<out g="0">00000</out>"#);
}

/// §14.3: "During the evaluation of a stylesheet function, the current group
/// and current grouping key are set to the empty sequence, and revert to their
/// previous values on completion of evaluation of the stylesheet function."
#[test]
fn a_stylesheet_function_sees_no_current_group() {
    let main = sheet(
        r#"<xsl:function name="f:probe"><xsl:sequence select="count(current-group()), count(current-grouping-key())"/></xsl:function>
           <xsl:template match="/"><out>
             <xsl:for-each-group select="body/*" group-by="name()">
               <g in="{f:probe()}" after="{count(current-group())}"/>
             </xsl:for-each-group>
           </out></xsl:template>"#,
    );
    assert_eq!(
        out(&main, FLAT),
        r#"<out><g in="0 0" after="2"/><g in="0 0" after="3"/></out>"#
    );
}

/// §5.4.4's table: the current group is set by "xsl:for-each-group" and
/// cleared by "calls on stylesheet functions" — nothing else. A template
/// called or applied from the body runs while the instruction "is being
/// evaluated" (§14.1), so it sees the group. (XSLT 3.0 changed this; the
/// suite's tests of the 3.0 rule are `XSLT30+`.)
#[test]
fn templates_invoked_from_the_body_see_the_current_group() {
    let main = sheet(
        r#"<xsl:template match="/"><out>
             <xsl:for-each-group select="body/*" group-by="name()">
               <xsl:call-template name="named"/>
               <xsl:apply-templates select="." mode="applied"/>
             </xsl:for-each-group>
           </out></xsl:template>
           <xsl:template name="named"><c k="{current-grouping-key()}" n="{count(current-group())}"/></xsl:template>
           <xsl:template match="*" mode="applied"><a k="{current-grouping-key()}" n="{count(current-group())}"/></xsl:template>"#,
    );
    assert_eq!(
        out(&main, FLAT),
        r#"<out><c k="h1" n="2"/><a k="h1" n="2"/><c k="p" n="3"/><a k="p" n="3"/></out>"#
    );
}

/// The sort keys of an `xsl:for-each` inside the body see the enclosing group
/// (§13.1.1 changes the focus, nothing else); an `xsl:for-each` changes the
/// focus but not the current group.
#[test]
fn an_inner_for_each_and_its_sort_keep_the_current_group() {
    let main = root(
        r#"<xsl:for-each-group select="body/*" group-by="name()">
             <g><xsl:for-each select="current-group()">
               <xsl:sort select="index-of(current-group()/string(), string(.))" order="descending"/>
               <xsl:value-of select="concat(., '/', count(current-group()))"/><xsl:text> </xsl:text>
             </xsl:for-each></g>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, FLAT),
        "<out><g>B/2 A/2 </g><g>b1/3 a2/3 a1/3 </g></out>"
    );
}

/// "Grouping is nestable to multiple levels" (§14); §14.3: "On completion of
/// the evaluation of the xsl:for-each-group instruction, the current group and
/// current grouping key revert to their previous value." The inner
/// instruction's `select` is evaluated in the outer group
/// (`for-each-group-088`).
#[test]
fn nested_grouping_restores_the_outer_group() {
    let main = root(
        r#"<xsl:for-each-group select="cities/city" group-by="@country">
             <c k="{current-grouping-key()}">
               <xsl:for-each-group select="current-group()" group-by="@pop &gt; 3">
                 <i k="{current-grouping-key()}"><xsl:value-of select="current-group()/@name"/></i>
               </xsl:for-each-group>
               <after n="{count(current-group())}" k="{current-grouping-key()}"/>
             </c>
           </xsl:for-each-group>"#,
    );
    assert_eq!(
        out(&main, CITIES),
        concat!(
            r#"<out><c k="Italia"><i k="true">Milano</i><i k="false">Venezia</i><after n="2" k="Italia"/></c>"#,
            r#"<c k="France"><i k="true">Paris</i><i k="false">Lyon</i><after n="2" k="France"/></c>"#,
            r#"<c k="Deutschland"><i k="true">Muenchen</i><after n="1" k="Deutschland"/></c></out>"#
        )
    );
}

/// The empty population: "If the population is empty, the number of groups
/// will be zero." The collation attribute is still evaluated.
#[test]
fn an_empty_population_has_no_groups() {
    let main = root(r#"<xsl:for-each-group select="()" group-by="."><g/></xsl:for-each-group>"#);
    assert_eq!(out(&main, FLAT), "<out/>");
    let main = root(
        r#"<xsl:for-each-group select="()" group-by="." collation="urn:unknown"><g/></xsl:for-each-group>"#,
    );
    assert_eq!(code_of(&main, FLAT), "XTDE1110");
}

/// The instruction also groups atomic values with `group-by` /
/// `group-adjacent` (only the pattern forms need nodes), and its result is a
/// sequence (§14.3: "The sequences that result are concatenated, in processing
/// order").
#[test]
fn grouping_returns_a_sequence_of_any_items() {
    let main = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="r" as="xs:integer*">
               <xsl:for-each-group select="(3, 1, 3, 2, 1)" group-by=".">
                 <xsl:sort select="."/>
                 <xsl:sequence select="current-grouping-key() * 10, count(current-group())"/>
               </xsl:for-each-group>
             </xsl:variable>
             <out><xsl:value-of select="$r"/></out>
           </xsl:template>"#,
    );
    assert_eq!(out(&main, FLAT), "<out>10 2 20 1 30 2</out>");
}

// ── Performance ─────────────────────────────────────────────────────────────

/// `group-by` hashes its keys, so allocating a population to groups is linear
/// in its size even when nearly every key is new. A lookup that compared each
/// key with every existing group would cost O(items × groups) — which is
/// linear too while the number of distinct keys is fixed, so the test makes it
/// grow with the population: *n* items, *n*/2 distinct keys, at 2,500 and
/// 20,000 items: linear work at 20,000 items takes about as long as eight runs
/// at 2,500, a compare-every-group lookup about eight times as long
/// (`common::scaling`).
#[test]
fn group_by_is_linear_in_the_population() {
    let main = root(
        r#"<xsl:variable name="g" as="xs:integer*">
             <xsl:for-each-group select="doc/i" group-by="@k"><xsl:sequence select="count(current-group())"/></xsl:for-each-group>
           </xsl:variable>
           <xsl:value-of select="count($g), sum($g), max($g)"/>"#,
    );
    common::scaling::assert_linear("group-by", 2_500, 8, |n| {
        let mut source = String::from("<doc>");
        for i in 0..n {
            source.push_str(&format!(r#"<i k="k{}" n="{}"/>"#, i % (n / 2), i));
        }
        source.push_str("</doc>");
        let mut result = String::new();
        let elapsed = common::scaling::timed(|| result = out(&main, &source));
        assert_eq!(result, format!("<out>{} {n} 2</out>", n / 2));
        elapsed
    });
}
