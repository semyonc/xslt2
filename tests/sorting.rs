//! Sorting (§13) and the collation registry.
//!
//! Every test quotes the sentence of XSLT 2.0 it checks. The sections: the
//! collation registry and its public API, the static and dynamic errors, the
//! dynamic context of a sort key, the comparison rules of §13.1.2 / §13.1.3,
//! `xsl:perform-sort`, backwards-compatible behaviour, mixed sort keys,
//! whitespace in `xsl:perform-sort`, QName-valued `data-type`, and the UCA
//! collations without the `icu-collation` feature.

mod common;

use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::collation::{
    Collation, CollationResolver, CODEPOINT_COLLATION_URI, HTML_ASCII_CASE_INSENSITIVE_URI,
};
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const HTML_ASCII: &str = HTML_ASCII_CASE_INSENSITIVE_URI;

/// A host collation for the tests: orders strings by length, then by
/// codepoints.
struct ByLength;

impl Collation for ByLength {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        a.chars()
            .count()
            .cmp(&b.chars().count())
            .then_with(|| a.cmp(b))
    }
}

/// Wraps template-level content in a `version="2.0"` stylesheet.
fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" exclude-result-prefixes="xs">{body}</xsl:stylesheet>"#
    )
}

/// Runs a whole stylesheet module over `source` with `functions`.
fn run_with(functions: &XsltFunctions, sheet: &str, source: &str) -> Result<String, XsltError> {
    run_with_docs(functions, sheet, source, &[])
}

/// Runs a whole stylesheet module over `source` with `functions`, with the
/// extra documents `docs` (URI, text) available to `document()`.
fn run_with_docs(
    functions: &XsltFunctions,
    sheet: &str,
    source: &str,
    docs: &[(&str, &str)],
) -> Result<String, XsltError> {
    let names = NameTable::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///sorting/in.xml", source);
    for (uri, text) in docs {
        mem.insert(*uri, *text);
    }
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///sorting/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///sorting/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// Runs a whole stylesheet module over `source` with the default functions.
fn run(sheet: &str, source: &str) -> Result<String, XsltError> {
    run_with(&XsltFunctions::new(), sheet, source)
}

/// Runs template-level `body` over `source`.
fn run_body(body: &str, source: &str) -> Result<String, XsltError> {
    run(&sheet(body), source)
}

/// Runs a `match="/"` template whose content is `content`, over `<doc/>`.
fn eval(content: &str) -> Result<String, XsltError> {
    run_body(
        &format!(r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#),
        "<doc/>",
    )
}

fn out(text: &str) -> String {
    format!("<out>{text}</out>")
}

fn code(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => panic!("expected an error, got {text}"),
        Err(e) => e.code.to_string(),
    }
}

const WORDS: &str = "<r><w>banana</w><w>Apple</w><w>cherry</w><w>apple</w><w>Banana</w></r>";

// ═══ The collation registry and its public API ══════════════════════════════

/// §13.1.3: "The one collation URI that must be recognized by every
/// implementation is http://www.w3.org/2005/xpath-functions/collation/codepoint"
/// — and the engine answers it itself, so the registry refuses to shadow it.
#[test]
fn the_registry_knows_the_built_in_collations_and_refuses_to_replace_codepoint() {
    let mut functions = XsltFunctions::new();
    assert!(functions.collations().recognizes(CODEPOINT_COLLATION_URI));
    assert!(functions.collations().recognizes(HTML_ASCII));
    assert!(!functions.collations().recognizes("urn:sorting:by-length"));
    assert!(!functions.register_collation(CODEPOINT_COLLATION_URI, Rc::new(ByLength)));
    assert!(functions.register_collation("urn:sorting:by-length", Rc::new(ByLength)));
    assert!(functions.collations().recognizes("urn:sorting:by-length"));
    assert!(functions
        .collations()
        .resolve("urn:sorting:by-length")
        .is_some());
    // The codepoint collation never goes through the resolver.
    assert!(functions
        .collations()
        .resolve(CODEPOINT_COLLATION_URI)
        .is_none());
}

/// A registered host collation is visible to `xsl:sort collation="…"`, to the
/// `$collation` argument of `compare()` and, as the `[xsl:]default-collation`,
/// to the comparison operators — the one registry reaches every
/// `XPathContext` of the stylesheet. §13.1.3: "If the xsl:sort element has a
/// collation attribute, then the strings are compared according to the rules
/// for the named collation".
#[test]
fn a_host_collation_reaches_xsl_sort_functions_and_operators() {
    let mut functions = XsltFunctions::new();
    functions.register_collation("urn:sorting:by-length", Rc::new(ByLength));
    let result = run_with(
        &functions,
        &sheet(
            r#"<xsl:template match="/" default-collation="urn:sorting:by-length">
                 <out>
                   <s><xsl:for-each select="r/w"><xsl:sort select="." collation="urn:sorting:by-length"/><xsl:value-of select="."/>,</xsl:for-each></s>
                   <c><xsl:value-of select="compare('zz', 'aaa', 'urn:sorting:by-length')"/></c>
                   <lt><xsl:value-of select="'zz' lt 'aaa'"/></lt>
                   <d><xsl:value-of select="default-collation()"/></d>
                 </out>
               </xsl:template>"#,
        ),
        "<r><w>ccc</w><w>a</w><w>bb</w></r>",
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><s>a,bb,ccc,</s><c>-1</c><lt>true</lt><d>urn:sorting:by-length</d></out>"
    );
}

/// §3.6.1: "The attribute is used to specify the default collation used by all
/// XPath expressions appearing in the attributes of this element, or attributes
/// of descendant elements" — with the HTML ASCII case-insensitive collation,
/// `eq`, `=`, `distinct-values()` and the substring functions all fold ASCII
/// case.
#[test]
fn the_default_collation_reaches_ordinary_expressions() {
    let result = run_body(
        &format!(
            r#"<xsl:template match="/" default-collation="{HTML_ASCII}">
                 <out>
                   <eq><xsl:value-of select="'ABC' eq 'abc'"/></eq>
                   <gen><xsl:value-of select="('x', 'Y') = 'y'"/></gen>
                   <dv><xsl:value-of select="count(distinct-values(('a', 'A', 'b')))"/></dv>
                   <sw><xsl:value-of select="starts-with('Hello', 'hE')"/></sw>
                   <ct><xsl:value-of select="contains('Hello', 'ELL', '{HTML_ASCII}')"/></ct>
                   <cp><xsl:value-of select="compare('a', 'B')"/></cp>
                 </out>
               </xsl:template>"#
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(
        result,
        "<out><eq>true</eq><gen>true</gen><dv>2</dv><sw>true</sw><ct>true</ct><cp>-1</cp></out>"
    );
    // Without the attribute the default is the codepoint collation.
    assert_eq!(
        eval("<xsl:value-of select=\"'ABC' eq 'abc'\"/>").unwrap(),
        out("false")
    );
}

/// §3.6.1: "The [xsl:]default-collation attribute does not affect the collation
/// used by xsl:sort." Under a case-blind default collation a plain `xsl:sort`
/// still uses the codepoint collation (upper case first).
#[test]
fn the_default_collation_does_not_affect_xsl_sort() {
    let result = run_body(
        &format!(
            r#"<xsl:template match="/" default-collation="{HTML_ASCII}">
                 <out><xsl:for-each select="r/w"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each></out>
               </xsl:template>"#
        ),
        WORDS,
    )
    .unwrap();
    assert_eq!(result, out("Apple,Banana,apple,banana,cherry,"));
}

/// §13.1.3: "If the effective value of the collation attribute of xsl:sort is
/// a relative URI, then it is resolved against the base URI of the xsl:sort
/// element."
#[test]
fn a_relative_collation_uri_is_resolved_against_the_base_uri_of_xsl_sort() {
    let mut functions = XsltFunctions::new();
    functions.register_collation("http://example.com/coll/by-length", Rc::new(ByLength));
    let result = run_with(
        &functions,
        &sheet(
            r#"<xsl:template match="/">
                 <out><xsl:for-each select="r/w"><xsl:sort select="." collation="by-length" xml:base="http://example.com/coll/"/><xsl:value-of select="."/>,</xsl:for-each></out>
               </xsl:template>"#,
        ),
        "<r><w>ccc</w><w>a</w><w>bb</w></r>",
    )
    .unwrap();
    assert_eq!(result, out("a,bb,ccc,"));
}

/// The HTML ASCII case-insensitive collation sorts without regard to ASCII
/// case, and the sort is stable among the strings it considers equal.
#[test]
fn xsl_sort_with_the_html_ascii_collation_is_case_blind_and_stable() {
    let result = run_body(
        &format!(
            r#"<xsl:template match="/">
                 <out><xsl:for-each select="r/w"><xsl:sort select="." collation="{HTML_ASCII}"/><xsl:value-of select="."/>,</xsl:for-each></out>
               </xsl:template>"#
        ),
        WORDS,
    )
    .unwrap();
    assert_eq!(result, out("Apple,apple,banana,Banana,cherry,"));
}

/// §3.12's static context of a `use-when` expression: "In scope collations:
/// Implementation-defined" — the same registry; "Default collation: The Unicode
/// Codepoint Collation".
#[test]
fn use_when_sees_the_registry_but_keeps_the_codepoint_default() {
    let result = run_body(
        &format!(
            r#"<xsl:template match="/" default-collation="{HTML_ASCII}">
                 <out>
                   <a xsl:use-when="compare('A', 'a', '{HTML_ASCII}') eq 0">registry</a>
                   <b xsl:use-when="'A' eq 'a'">default</b>
                 </out>
               </xsl:template>"#
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(result, "<out><a>registry</a></out>");
}

/// §3.6.1: "[ERR XTSE0125] It is a static error if the value of an
/// [xsl:]default-collation attribute, after resolving against the base URI,
/// contains no URI that the implementation recognizes as a collation URI." A
/// registered host collation is recognized, and the first recognized URI of
/// the list wins.
#[test]
fn default_collation_recognizes_host_collations_and_takes_the_first_known() {
    let mut functions = XsltFunctions::new();
    functions.register_collation("urn:sorting:by-length", Rc::new(ByLength));
    let result = run_with(
        &functions,
        &sheet(
            r#"<xsl:template match="/" default-collation="urn:unknown urn:sorting:by-length http://www.w3.org/2005/xpath-functions/collation/codepoint">
                 <out><xsl:value-of select="default-collation()"/></out>
               </xsl:template>"#,
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(result, out("urn:sorting:by-length"));
    let e = run_body(
        r#"<xsl:template match="/" default-collation="urn:sorting:by-length"><out/></xsl:template>"#,
        "<doc/>",
    )
    .unwrap_err();
    assert_eq!(e.code, "XTSE0125");
}

// ═══ Static errors ═══════════════════════════════════════════════════════

/// §13.1: "[ERR XTSE1015] It is a static error if an xsl:sort element with a
/// select attribute has non-empty content."
#[test]
fn xtse1015_select_and_content() {
    assert_eq!(
        code(eval(
            r#"<xsl:for-each select="1 to 3"><xsl:sort select="."><xsl:sequence select="."/></xsl:sort></xsl:for-each>"#
        )),
        "XTSE1015"
    );
}

/// §13.1: "[ERR XTSE1017] It is a static error if an xsl:sort element other
/// than the first in a sequence of sibling xsl:sort elements has a stable
/// attribute."
#[test]
fn xtse1017_stable_on_a_later_sort() {
    assert_eq!(
        code(eval(
            r#"<xsl:for-each select="1 to 3"><xsl:sort select="."/><xsl:sort select="." stable="yes"/></xsl:for-each>"#
        )),
        "XTSE1017"
    );
    // On the first one it is allowed.
    assert_eq!(
        eval(r#"<xsl:for-each select="(3, 1, 2)"><xsl:sort select="." stable="no"/><xsl:value-of select="."/></xsl:for-each>"#)
            .unwrap(),
        out("123")
    );
}

/// §3.4: "[ERR XTSE0020] It is a static error if an attribute (other than an
/// attribute written using curly brackets in a position where an attribute
/// value template is permitted) contains a value that is not one of the
/// permitted values for that attribute." (`sort-078`: `stable="YES"`.)
#[test]
fn xtse0020_a_fixed_attribute_value_that_is_not_permitted() {
    for attrs in [
        r#"stable="YES""#,
        r#"order="up""#,
        r#"data-type="integer""#,
        r#"case-order="upper""#,
        r#"lang="'de'""#,
    ] {
        assert_eq!(
            code(eval(&format!(
                r#"<xsl:for-each select="1 to 3"><xsl:sort select="." {attrs}/></xsl:for-each>"#
            ))),
            "XTSE0020",
            "{attrs}"
        );
    }
}

/// §13.2: "[ERR XTSE1040] It is a static error if an xsl:perform-sort
/// instruction with a select attribute has any content other than xsl:sort and
/// xsl:fallback instructions."
#[test]
fn xtse1040_perform_sort_with_select_and_content() {
    assert_eq!(
        code(eval(
            r#"<xsl:perform-sort select="1 to 3"><xsl:sort select="."/><x/></xsl:perform-sort>"#
        )),
        "XTSE1040"
    );
    // xsl:fallback is allowed, and ignored.
    assert_eq!(
        eval(r#"<xsl:perform-sort select="(2, 1)"><xsl:sort select="."/><xsl:fallback>F</xsl:fallback></xsl:perform-sort>"#)
            .unwrap(),
        out("1 2")
    );
}

// ═══ Dynamic errors ══════════════════════════════════════════════════════

/// §5.6 / §3.4: a computed attribute value that is not permitted is XTDE0030
/// ("It is a non-recoverable dynamic error if the effective value of an
/// attribute written using curly brackets, in a position where an attribute
/// value template is permitted, is a value that is not one of the permitted
/// values for that attribute").
#[test]
fn xtde0030_a_computed_attribute_value_that_is_not_permitted() {
    for attrs in [
        r#"order="{'up'}""#,
        r#"data-type="{'integer'}""#,
        r#"case-order="{'upper'}""#,
        r#"lang="{'''de'''}""#,
        r#"stable="{'YES'}""#,
    ] {
        assert_eq!(
            code(eval(&format!(
                r#"<xsl:for-each select="1 to 3"><xsl:sort select="." {attrs}/></xsl:for-each>"#
            ))),
            "XTDE0030",
            "{attrs}"
        );
    }
}

/// §13.1.2: "[ERR XTTE1020] If any sort key value, after atomization and any
/// type conversion required by the data-type attribute, is a sequence
/// containing more than one item, then the effect depends on whether the
/// xsl:sort element is evaluated with backwards compatible behavior. With
/// backwards compatible behavior, the effective sort key value is the first
/// item in the sequence. In other cases, this is a type error."
#[test]
fn xtte1020_and_the_first_item_rule_of_backwards_compatible_behaviour() {
    let source = "<r><i><k>2</k><k>9</k></i><i><k>1</k><k>0</k></i></r>";
    assert_eq!(
        code(run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="k"/><xsl:value-of select="k[1]"/></xsl:for-each></out></xsl:template>"#,
            source,
        )),
        "XTTE1020"
    );
    // version="1.0" on the xsl:sort element itself (§3.8: the nearest
    // [xsl:]version wins): the first item, 2 and 1, is the key.
    assert_eq!(
        run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="k" version="1.0"/><xsl:value-of select="k[1]"/></xsl:for-each></out></xsl:template>"#,
            source,
        )
        .unwrap(),
        out("12")
    );
}

/// §13.1.2: "[ERR XTDE1030] It is a non-recoverable dynamic error if, for any
/// sort key component, the set of sort key values evaluated for all the items
/// in the initial sequence, after any type conversion requested, contains a
/// pair of ordinary values for which the result of the XPath lt operator is an
/// error." Strings against numbers, and `xs:QName` values, which have no order.
#[test]
fn xtde1030_incomparable_sort_key_values() {
    assert_eq!(
        code(eval(
            r#"<xsl:for-each select="(1, 'a', 2)"><xsl:sort select="."/><xsl:value-of select="."/></xsl:for-each>"#
        )),
        "XTDE1030"
    );
    assert_eq!(
        code(eval(
            r#"<xsl:for-each select="(xs:QName('a'), xs:QName('b'))"><xsl:sort select="."/></xsl:for-each>"#
        )),
        "XTDE1030"
    );
    // data-type="text" converts first, so the same values sort as strings.
    assert_eq!(
        eval(r#"<xsl:for-each select="(1, 'a', 2)"><xsl:sort select="." data-type="text"/><xsl:value-of select="."/></xsl:for-each>"#)
            .unwrap(),
        out("12a")
    );
}

/// §13.1.3: "[ERR XTDE1035] It is a non-recoverable dynamic error if the
/// collation attribute of xsl:sort (after resolving against the base URI) is
/// not a URI that is recognized by the implementation as referring to a
/// collation." The attribute is evaluated whenever the instruction is, even
/// for an empty initial sequence (`sort-027`).
#[test]
fn xtde1035_an_unknown_collation() {
    for select in ["(2, 1)", "()"] {
        assert_eq!(
            code(eval(&format!(
                r#"<xsl:for-each select="{select}"><xsl:sort select="." collation="http://unknown.collation.uri/"/></xsl:for-each>"#
            ))),
            "XTDE1035",
            "{select}"
        );
    }
}

// ═══ The dynamic context of a sort key (§13.1.1, §6.5, §6.7) ═════════════

/// §13.1.1: "The context item is the item in the initial sequence whose sort
/// key value is being computed. The context position is the position of that
/// item in the initial sequence. The context size is the size of the initial
/// sequence." Sorting on `last() - position()` reverses the sequence; the body
/// then sees positions in the *sorted* sequence (§13.3).
#[test]
fn the_focus_of_a_sort_key_is_the_unsorted_position_and_size() {
    assert_eq!(
        eval(
            r#"<xsl:for-each select="('a', 'b', 'c')"><xsl:sort select="last() - position()"/><xsl:value-of select="concat(., position(), last())"/>,</xsl:for-each>"#
        )
        .unwrap(),
        out("c13,b23,a33,")
    );
}

/// §13.1.1 Note: "the current function may be used within the select
/// expression of xsl:sort to refer to the item that is the context item for
/// the expression as a whole; that is, the item whose sort key value is being
/// computed."
#[test]
fn current_in_a_sort_key_is_the_item_being_sorted() {
    let result = run_body(
        r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="/r/rank[@of = current()/@n]" data-type="number"/><xsl:value-of select="@n"/></xsl:for-each></out></xsl:template>"#,
        r#"<r><i n="x"/><i n="y"/><i n="z"/><rank of="x">3</rank><rank of="y">1</rank><rank of="z">2</rank></r>"#,
    )
    .unwrap();
    assert_eq!(result, out("yzx"));
}

/// §6.7 (the definition of *current template rule*): "When an xsl:for-each,
/// xsl:for-each-group, or xsl:analyze-string instruction is evaluated, or when
/// evaluating a sequence constructor contained in an xsl:sort or xsl:key
/// element, or when a stylesheet function is called (see 10.3 Stylesheet
/// Functions), the current template rule becomes null for the evaluation of
/// that instruction or function." An
/// `xsl:next-match` there is XTDE0560 ("It is a non-recoverable dynamic error
/// if xsl:apply-imports or xsl:next-match is evaluated when the current
/// template rule is null"), even inside a template rule, where it is fine
/// outside the sort key.
#[test]
fn the_current_template_rule_is_cleared_inside_a_sort_key() {
    let body = |content: &str| {
        format!(
            r#"<xsl:template match="r"><out><xsl:apply-templates select="i">{content}</xsl:apply-templates></out></xsl:template>
               <xsl:template match="i" priority="2"><xsl:next-match/></xsl:template>
               <xsl:template match="i"><xsl:value-of select="@k"/></xsl:template>"#
        )
    };
    let source = r#"<r><i k="b"/><i k="a"/></r>"#;
    assert_eq!(
        run_body(&body(r#"<xsl:sort select="@k"/>"#), source).unwrap(),
        out("ab")
    );
    let result = run_body(
        &format!(
            r#"<xsl:template match="r" priority="2"><xsl:for-each select="i"><xsl:sort><xsl:next-match/></xsl:sort></xsl:for-each></xsl:template>
               {}"#,
            body("")
        ),
        source,
    );
    assert_eq!(code(result), "XTDE0560");
}

/// §6.5: "While evaluating global variables and parameters, and the sequence
/// constructor contained in xsl:key or xsl:sort, the current mode is set to the
/// default mode." (§5.4.4's non-normative table says the same.) So
/// `mode="#current"` inside a sort key applies the default-mode rules.
#[test]
fn the_current_mode_is_the_default_mode_inside_a_sort_key() {
    let result = run_body(
        r##"<xsl:template match="/"><out><xsl:apply-templates select="r" mode="m"/></out></xsl:template>
           <xsl:template match="r" mode="m">
             <xsl:for-each select="i">
               <xsl:sort><xsl:apply-templates select="." mode="#current"/></xsl:sort>
               <xsl:value-of select="@n"/>
             </xsl:for-each>
           </xsl:template>
           <xsl:template match="i"><xsl:value-of select="@key"/></xsl:template>
           <xsl:template match="i" mode="m">WRONG</xsl:template>"##,
        r#"<r><i n="1" key="c"/><i n="2" key="a"/><i n="3" key="b"/></r>"#,
    )
    .unwrap();
    assert_eq!(result, out("231"));
}

/// The sort key sequence constructor shares the frame of the instruction: its
/// own variables, and the variables in scope around it, are visible.
#[test]
fn a_sort_key_sequence_constructor_sees_the_variables_in_scope() {
    assert_eq!(
        eval(
            r#"<xsl:variable name="offset" select="10"/><xsl:for-each select="(3, 1, 2)"><xsl:sort data-type="number"><xsl:variable name="v" select=". * -1"/><xsl:sequence select="$v + $offset"/></xsl:sort><xsl:value-of select="."/></xsl:for-each>"#
        )
        .unwrap(),
        out("321")
    );
}

/// §13.3: "When used within xsl:for-each or xsl:apply-templates, a sort key
/// specification indicates that the sequence of items selected by that
/// instruction is to be processed in sorted order". The matched template sees
/// its position in the sorted sequence; `xsl:with-param` may be mixed with
/// `xsl:sort` (`(xsl:sort | xsl:with-param)*`).
#[test]
fn apply_templates_processes_the_sorted_sequence() {
    let result = run_body(
        r#"<xsl:template match="r"><out><xsl:apply-templates select="i"><xsl:with-param name="p" select="'-'"/><xsl:sort select="." order="descending"/></xsl:apply-templates></out></xsl:template>
           <xsl:template match="i"><xsl:param name="p"/><xsl:value-of select="concat(position(), ., $p)"/></xsl:template>"#,
        "<r><i>a</i><i>c</i><i>b</i></r>",
    )
    .unwrap();
    assert_eq!(result, out("1c-2b-3a-"));
}

// ═══ Comparing sort key values (§13.1.1, §13.1.2) ════════════════════════

/// §13.1.1: "If all the sort key values compare equal, and the sort key
/// specification is stable, then A will precede B in the sorted sequence if and
/// only if A preceded B in the initial sequence." and its Note: "regardless of
/// whether order="descending" was specified". This processor always sorts
/// stably, `stable="no"` included (it is only a permission).
#[test]
fn equal_keys_keep_their_initial_order_even_descending() {
    let source = r#"<r><i k="1" n="a"/><i k="2" n="b"/><i k="1" n="c"/><i k="2" n="d"/></r>"#;
    for stable in ["", r#" stable="yes""#, r#" stable="no""#] {
        let result = run_body(
            &format!(
                r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="@k" order="descending"{stable}/><xsl:value-of select="@n"/></xsl:for-each></out></xsl:template>"#
            ),
            source,
        )
        .unwrap();
        assert_eq!(result, out("bdac"), "{stable}");
    }
}

/// §13.1.1: "If, however, the relevant sort key values compare equal, then the
/// second sort key value of A is compared with the second sort key value of B,
/// according to the rules of the second sort key component."
#[test]
fn several_sort_keys_are_major_to_minor() {
    let result = run_body(
        r#"<xsl:template match="/"><out><xsl:for-each select="r/p"><xsl:sort select="@last"/><xsl:sort select="@first" order="descending"/><xsl:value-of select="concat(@first, ' ', @last)"/>;</xsl:for-each></out></xsl:template>"#,
        r#"<r><p first="Ann" last="Lee"/><p first="Bob" last="Kay"/><p first="Cid" last="Lee"/><p first="Dan" last="Kay"/></r>"#,
    )
    .unwrap();
    assert_eq!(result, out("Dan Kay;Bob Kay;Cid Lee;Ann Lee;"));
}

/// §13.1.2: "If it has the effective value number, the atomized sort key values
/// are converted to doubles before being compared" (with `number()`, so a
/// non-numeric value is NaN) and "NaN values, for sorting purposes, are
/// considered to be equal to each other, and less than any other numeric
/// value."
#[test]
fn data_type_number_compares_numerically_with_nan_first() {
    let source = "<r><v>10</v><v>x</v><v>9</v><v>-1.5</v><v>y</v></r>";
    let result = run_body(
        r#"<xsl:template match="/"><out><xsl:for-each select="r/v"><xsl:sort select="." data-type="number"/><xsl:value-of select="."/>,</xsl:for-each></out></xsl:template>"#,
        source,
    )
    .unwrap();
    assert_eq!(result, out("x,y,-1.5,9,10,"));
    let result = run_body(
        r#"<xsl:template match="/"><out><xsl:for-each select="r/v"><xsl:sort select="." data-type="number" order="descending"/><xsl:value-of select="."/>,</xsl:for-each></out></xsl:template>"#,
        source,
    )
    .unwrap();
    assert_eq!(result, out("10,9,-1.5,x,y,"));
}

/// §13.1.2: "If this has the effective value text, the atomized sort key values
/// are converted to strings before being compared." Numbers then sort as
/// strings.
#[test]
fn data_type_text_compares_strings() {
    assert_eq!(
        eval(r#"<xsl:for-each select="(10, 9, 100)"><xsl:sort select="." data-type="text"/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("10,100,9,")
    );
}

/// §13.1.2: "The conversion is done by using the string or number function as
/// appropriate", and only then is "the set of sort key values (after any
/// conversion) … divided into two categories: empty values, and ordinary
/// values". `string(())` is `""` and `number(())` is NaN, so with a `data-type`
/// a missing key is an ordinary value equal to an empty or non-numeric one,
/// and the two keep their original order. Without `data-type` the missing key
/// stays empty and sorts first.
#[test]
fn data_type_converts_an_empty_key_before_classifying_it() {
    let source = r#"<r><i id="A" k=""/><i id="B"/><i id="C" k="1"/></r>"#;
    let sorted = |sort: &str| {
        run_body(
            &format!(
                r#"<xsl:template match="/"><out><xsl:for-each select="r/i">{sort}<xsl:value-of select="@id"/></xsl:for-each></out></xsl:template>"#
            ),
            source,
        )
        .unwrap()
    };
    assert_eq!(
        sorted(r#"<xsl:sort select="@k" data-type="text"/>"#),
        out("ABC")
    );
    assert_eq!(
        sorted(r#"<xsl:sort select="@k" data-type="number"/>"#),
        out("ABC")
    );
    assert_eq!(sorted(r#"<xsl:sort select="@k"/>"#), out("BAC"));
}

/// §13.1.2, the XSLT 2.0 rule without `data-type`: "In the absence of such a
/// cast, the sort key values are compared using the rules appropriate to their
/// data type. Any values of type xs:untypedAtomic are cast to xs:string." Typed
/// numbers compare numerically; untyped attribute values compare as strings;
/// a cast in the key forces a type (`xs:date(@d)`).
#[test]
fn without_data_type_values_compare_by_their_own_type() {
    assert_eq!(
        eval(r#"<xsl:for-each select="(10, 9, 100)"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("9,10,100,")
    );
    let source = r#"<r><i n="10"/><i n="9"/><i n="100"/></r>"#;
    assert_eq!(
        run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="@n"/><xsl:value-of select="@n"/>,</xsl:for-each></out></xsl:template>"#,
            source,
        )
        .unwrap(),
        out("10,100,9,")
    );
    assert_eq!(
        run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="xs:integer(@n)"/><xsl:value-of select="@n"/>,</xsl:for-each></out></xsl:template>"#,
            source,
        )
        .unwrap(),
        out("9,10,100,")
    );
}

/// §13.1.2: "The empty sort key values represent those items where the sort key
/// value is an empty sequence. These values are considered for sorting purposes
/// to be equal to each other, but less than any other value."
#[test]
fn empty_sort_keys_sort_first() {
    let source = r#"<r><i n="b" k="2"/><i n="a"/><i n="c" k="1"/><i n="d"/></r>"#;
    assert_eq!(
        run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="@k"/><xsl:value-of select="@n"/></xsl:for-each></out></xsl:template>"#,
            source,
        )
        .unwrap(),
        out("adcb")
    );
    assert_eq!(
        run_body(
            r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="@k" order="descending"/><xsl:value-of select="@n"/></xsl:for-each></out></xsl:template>"#,
            source,
        )
        .unwrap(),
        out("bcad")
    );
}

/// §13.1.2: "To ensure a total ordering, the same implementation of the lt
/// operator must be used for all the comparisons: the one that is chosen is the
/// one appropriate to the most specific type to which all the values can be
/// converted by subtype substitution and/or type promotion. For example, if the
/// sequence contains both xs:decimal and xs:double values, then the values are
/// compared using xs:double comparison, even when comparing two xs:decimal
/// values." — mixed numeric types sort as one numeric order.
#[test]
fn mixed_numeric_types_sort_together() {
    assert_eq!(
        eval(r#"<xsl:for-each select="(2.5, xs:double(1e0), 3, xs:float(0.5))"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("0.5,1,2.5,3,")
    );
    // The discriminating case: the two decimals differ as decimals but are
    // the same xs:double (1.0), so with the column compared as doubles they
    // are equal and keep their initial order; compared pairwise as decimals
    // they would swap.
    assert_eq!(
        eval(r#"<xsl:for-each select="(1.00000000000000002, 1.00000000000000001, 0.5e0)"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("0.5,1.00000000000000002,1.00000000000000001,")
    );
    // The same with xs:float as the widest type: the decimals are compared as
    // floats, where 1.00000001 and 1.00000002 are both 1.0.
    assert_eq!(
        eval(r#"<xsl:for-each select="(1.00000002, 1.00000001, xs:float(0.5))"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("0.5,1.00000002,1.00000001,")
    );
    // With no float or double, decimals and integers compare exactly.
    assert_eq!(
        eval(r#"<xsl:for-each select="(1.00000000000000002, 1.00000000000000001, 1)"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each>"#)
            .unwrap(),
        out("1,1.00000000000000001,1.00000000000000002,")
    );
}

// ═══ Collations chosen by lang and case-order (§13.1.3) ══════════════════

/// §13.1.3: "if lang="en", then A a B b are sorted with case-order="upper-first"
/// and a A b B are sorted with case-order="lower-first"." This processor's
/// implementation-defined algorithm (no language data): case-blind first, then
/// the first character differing in case decides.
#[test]
fn case_order_without_a_collation() {
    let sort = |attrs: &str| {
        eval(&format!(
            r#"<xsl:for-each select="('b', 'A', 'B', 'a')"><xsl:sort select="." {attrs}/><xsl:value-of select="."/></xsl:for-each>"#
        ))
        .unwrap()
    };
    assert_eq!(sort(r#"lang="en" case-order="upper-first""#), out("AaBb"));
    assert_eq!(sort(r#"lang="en" case-order="lower-first""#), out("aAbB"));
    assert_eq!(sort(r#"case-order="{'upper-first'}""#), out("AaBb"));
    // `lang` alone: the same algorithm, lower-first.
    assert_eq!(sort(r#"lang="en""#), out("aAbB"));
    // §13.1.3: "if a collation URI is specified, other attributes such as
    // case-order and lang are ignored."
    assert_eq!(
        sort(&format!(
            r#"lang="en" case-order="lower-first" collation="{CODEPOINT_COLLATION_URI}""#
        )),
        out("ABab")
    );
}

// ═══ xsl:perform-sort (§13.2) ═════════════════════════════════════════════

/// §13.2: "The initial sequence is obtained either by evaluating the select
/// attribute or by evaluating the contained sequence constructor (but not
/// both). If there is no select attribute and no sequence constructor then the
/// initial sequence (and therefore, the sorted sequence) is an empty sequence."
#[test]
fn perform_sort_from_select_from_content_and_from_nothing() {
    assert_eq!(
        eval(r#"<xsl:perform-sort select="(3, 1, 2)"><xsl:sort select="."/></xsl:perform-sort>"#)
            .unwrap(),
        out("1 2 3")
    );
    assert_eq!(
        eval(r#"<xsl:perform-sort><xsl:sort select="." order="descending"/><xsl:sequence select="(1, 3)"/><xsl:sequence select="2"/></xsl:perform-sort>"#)
            .unwrap(),
        out("3 2 1")
    );
    assert_eq!(
        eval(r#"<xsl:value-of select="count(())"/><xsl:perform-sort><xsl:sort select="."/></xsl:perform-sort>"#)
            .unwrap(),
        out("0")
    );
    // The result is a sequence, usable as a variable's value.
    assert_eq!(
        eval(r#"<xsl:variable name="s" as="xs:string*"><xsl:perform-sort select="('b', 'c', 'a')"><xsl:sort select="."/></xsl:perform-sort></xsl:variable><xsl:value-of select="$s" separator="|"/>"#)
            .unwrap(),
        out("a|b|c")
    );
}

/// §4.2: "Any whitespace text node whose following-sibling node is an
/// xsl:param or xsl:sort element is removed from the tree, regardless of any
/// xml:space attributes." (`whitespace-015`.)
#[test]
fn whitespace_before_xsl_sort_is_stripped_even_when_preserved() {
    let result = eval(
        "<xsl:for-each select=\"(2, 1)\" xml:space=\"preserve\">\n\t<xsl:sort select=\".\"/>  <xsl:sort select=\".\"/>[<xsl:value-of select=\".\"/>]</xsl:for-each>",
    )
    .unwrap();
    assert_eq!(result, out("[1][2]"));
}

// ── XTDE1030 is decided before sorting; the sort never panics ────────────

/// A reproduction: 21 integers with two strings among them. The
/// two- and three-item XTDE1030 tests above never reach the part of the
/// standard library's sort that checks its comparator; this one did, and the
/// process panicked ("user-provided comparison function does not correctly
/// implement a total order") instead of raising the error.
///
/// §13.1.2: "[ERR XTDE1030] It is a non-recoverable dynamic error if, for any
/// sort key component, the set of sort key values evaluated for all the items
/// in the initial sequence, after any type conversion requested, contains a
/// pair of ordinary values for which the result of the XPath lt operator is an
/// error."
#[test]
fn xtde1030_for_a_long_heterogeneous_sequence_is_an_error_not_a_panic() {
    let result = eval(
        r#"<xsl:perform-sort select="(74, 96, 59, 's219', 246, 434, 579, 228, 596, 50, 's47', 136, 147,
                                      584, 835, 105, 654, 99, 64, 633, 696)">
             <xsl:sort select="."/>
           </xsl:perform-sort>"#,
    );
    assert_eq!(code(result), "XTDE1030");
}

/// §13.1.2's Note: "The above error condition may occur if the values to be
/// sorted are of a type that does not support ordering (for example, xs:QName)
/// or if the sequence is heterogeneous (for example, if it contains both
/// strings and numbers)." XPath 2.0 B.2 lists the pairs `lt` is defined for —
/// numeric, `xs:boolean`, `xs:string`, `xs:date`, `xs:time`, `xs:dateTime`,
/// `xs:yearMonthDuration`, `xs:dayTimeDuration`, `xs:anyURI` — each against
/// itself (and `xs:anyURI` promotes to `xs:string`). Every other mixture, at
/// any length, is XTDE1030.
#[test]
fn xtde1030_for_every_kind_of_incomparable_column() {
    let many = |items: &str| {
        format!(r#"<xsl:perform-sort select="({items})"><xsl:sort select="."/></xsl:perform-sort>"#)
    };
    let dates: Vec<String> = (1..=24)
        .map(|d| format!("xs:date('2020-01-{d:02}')"))
        .collect();
    let dates = dates.join(", ");
    for (what, items) in [
        (
            "date and dateTime",
            format!("{dates}, xs:dateTime('2020-01-05T00:00:00')"),
        ),
        ("date and string", format!("'2020-01-03', {dates}")),
        (
            "two QNames among nothing else",
            "xs:QName('a'), xs:QName('b')".to_string(),
        ),
        (
            "QNames in a long column",
            (0..25)
                .map(|i| format!("xs:QName('q{i}')"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "xs:duration has no order",
            "xs:duration('P1D'), xs:duration('P2D')".to_string(),
        ),
        (
            "yearMonthDuration and dayTimeDuration",
            (0..22)
                .map(|i| {
                    if i == 11 {
                        "xs:dayTimeDuration('P1D')".to_string()
                    } else {
                        format!("xs:yearMonthDuration('P{i}M')")
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "boolean and number",
            (0..30)
                .map(|i| {
                    if i % 7 == 3 {
                        "true()".to_string()
                    } else {
                        i.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "gYear has no order",
            "xs:gYear('2001'), xs:gYear('2000')".to_string(),
        ),
    ] {
        assert_eq!(code(eval(&many(&items))), "XTDE1030", "{what}");
    }
    // One value of an unordered type is no *pair*: it sorts, alone or among
    // empty keys.
    assert_eq!(
        eval(r#"<xsl:value-of select="count(())"/><xsl:for-each select="(1, 2, 3)"><xsl:sort select="if (. = 2) then xs:QName('q') else ()"/><xsl:value-of select="."/></xsl:for-each>"#)
            .unwrap(),
        out("0132")
    );
}

/// The comparable kinds sort at length: 30 dates with and without timezones,
/// booleans, durations of one kind, strings with `xs:anyURI` values.
#[test]
fn comparable_columns_sort_at_length() {
    let sorted = |items: &str| {
        eval(&format!(
            r#"<xsl:variable name="s" as="item()*"><xsl:perform-sort select="({items})"><xsl:sort select="."/></xsl:perform-sort></xsl:variable><xsl:value-of select="string-join(for $x in $s return string($x), ',')"/>"#
        ))
        .unwrap()
    };
    let days: Vec<u32> = (0..30).map(|i| (i * 17) % 28 + 1).collect();
    let items: Vec<String> = days
        .iter()
        .map(|d| format!("xs:date('2020-02-{d:02}Z')"))
        .collect();
    let mut expected: Vec<String> = days.iter().map(|d| format!("2020-02-{d:02}Z")).collect();
    expected.sort();
    assert_eq!(sorted(&items.join(", ")), out(&expected.join(",")));

    let booleans: Vec<&str> = (0..25)
        .map(|i| if i % 3 == 0 { "true()" } else { "false()" })
        .collect();
    let expected = format!("{},{}", ["false"; 16].join(","), ["true"; 9].join(","));
    assert_eq!(sorted(&booleans.join(", ")), out(&expected));

    let months: Vec<u32> = (0..24).map(|i| (i * 7) % 24).collect();
    let items: Vec<String> = months
        .iter()
        .map(|m| format!("xs:yearMonthDuration('P{m}M')"))
        .collect();
    let mut sorted_months = months.clone();
    sorted_months.sort();
    let expected: Vec<String> = sorted_months
        .iter()
        .map(|m| match (m / 12, m % 12) {
            (0, 0) => "P0M".to_string(),
            (0, n) => format!("P{n}M"),
            (y, 0) => format!("P{y}Y"),
            (y, n) => format!("P{y}Y{n}M"),
        })
        .collect();
    assert_eq!(sorted(&items.join(", ")), out(&expected.join(",")));

    let words: Vec<String> = (0..26)
        .map(|i| {
            let w = format!("w{:02}", (i * 11) % 26);
            if i % 4 == 0 {
                format!("xs:anyURI('{w}')")
            } else {
                format!("'{w}'")
            }
        })
        .collect();
    let mut expected: Vec<String> = (0..26).map(|i| format!("w{:02}", (i * 11) % 26)).collect();
    expected.sort();
    assert_eq!(sorted(&words.join(", ")), out(&expected.join(",")));
}

/// §13.1.2: "NaN values, for sorting purposes, are considered to be equal to
/// each other, and less than any other numeric value." At length, with
/// integers, decimals and doubles mixed, ascending and descending, with and
/// without `data-type="number"`.
#[test]
fn nan_sorts_below_every_number_at_length() {
    let items = "3, xs:double('NaN'), 2.5e0, 10, 0.25, xs:double('NaN'), -1, 7, 4.5e0, 1, \
                 xs:double('NaN'), 9, 8, -2.75, 6, 5, xs:double('-INF'), 12, 11, xs:double('INF'), \
                 0, 13";
    let sort = |attrs: &str| {
        eval(&format!(
            r#"<xsl:variable name="s" as="item()*"><xsl:perform-sort select="({items})"><xsl:sort select="." {attrs}/></xsl:perform-sort></xsl:variable><xsl:value-of select="string-join(for $x in $s return string($x), ',')"/>"#
        ))
        .unwrap()
    };
    let ascending = "NaN,NaN,NaN,-INF,-2.75,-1,0,0.25,1,2.5,3,4.5,5,6,7,8,9,10,11,12,13,INF";
    let descending = "INF,13,12,11,10,9,8,7,6,5,4.5,3,2.5,1,0.25,0,-1,-2.75,-INF,NaN,NaN,NaN";
    assert_eq!(sort(""), out(ascending));
    assert_eq!(sort(r#"data-type="number""#), out(ascending));
    assert_eq!(sort(r#"order="descending""#), out(descending));
    assert_eq!(
        sort(r#"data-type="number" order="descending""#),
        out(descending)
    );
}

/// §13.1.2: "The empty sort key values represent those items where the sort key
/// value is an empty sequence. These values are considered for sorting purposes
/// to be equal to each other, but less than any other value." At length, among
/// numbers and among strings, ascending and descending, stably.
#[test]
fn empty_keys_mixed_with_values_at_length() {
    let mut source = String::from("<r>");
    for i in 0..24 {
        if i % 5 == 2 {
            source.push_str(&format!(r#"<i n="e{i}"/>"#));
        } else {
            source.push_str(&format!(r#"<i n="v{i}" k="{}"/>"#, (i * 7) % 24));
        }
    }
    source.push_str("</r>");
    let sort = |key: &str, attrs: &str| {
        run_body(
            &format!(
                r#"<xsl:template match="/"><out><xsl:for-each select="r/i"><xsl:sort select="{key}" {attrs}/><xsl:value-of select="@n"/>,</xsl:for-each></out></xsl:template>"#
            ),
            &source,
        )
        .unwrap()
    };
    let empties = "e2,e7,e12,e17,e22,";
    let mut numbered: Vec<(u32, String)> = (0..24u32)
        .filter(|i| i % 5 != 2)
        .map(|i| ((i * 7) % 24, format!("v{i}")))
        .collect();
    numbered.sort_by_key(|(k, _)| *k);
    let values: String = numbered.iter().map(|(_, n)| format!("{n},")).collect();
    assert_eq!(
        sort("@k/xs:integer(.)", ""),
        out(&format!("{empties}{values}"))
    );
    let values_down: String = numbered
        .iter()
        .rev()
        .map(|(_, n)| format!("{n},"))
        .collect();
    assert_eq!(
        sort("@k/xs:integer(.)", r#"order="descending""#),
        out(&format!("{values_down}{empties}"))
    );
    // Untyped attribute values are compared as strings: "10" < "2".
    let mut texts: Vec<(String, String)> = (0..24u32)
        .filter(|i| i % 5 != 2)
        .map(|i| (((i * 7) % 24).to_string(), format!("v{i}")))
        .collect();
    texts.sort_by(|a, b| a.0.cmp(&b.0));
    let values: String = texts.iter().map(|(_, n)| format!("{n},")).collect();
    assert_eq!(sort("@k", ""), out(&format!("{empties}{values}")));
}

/// A tiny deterministic generator (xorshift64*), so that the randomized test
/// needs no dependency and fails the same way every time.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One generated item: its XPath literal, its string value, and what the
/// reference order needs to know about its sort key.
struct Generated {
    literal: String,
    shown: String,
    key: RefKey,
}

enum RefKey {
    Empty,
    Number(f64),
    Text(String),
}

fn reference_order(a: &RefKey, b: &RefKey) -> Ordering {
    match (a, b) {
        (RefKey::Empty, RefKey::Empty) => Ordering::Equal,
        (RefKey::Empty, _) => Ordering::Less,
        (_, RefKey::Empty) => Ordering::Greater,
        (RefKey::Number(x), RefKey::Number(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x.partial_cmp(y).unwrap(),
        },
        (RefKey::Text(x), RefKey::Text(y)) => x.cmp(y),
        _ => unreachable!("the reference only orders homogeneous columns"),
    }
}

/// Property: for any mixture of integers, decimals, doubles (NaN included),
/// strings and empty keys, 20–100 items long, `xsl:perform-sort` either returns
/// the stable sorted order or raises XTDE1030 — exactly when strings and numbers
/// meet (§13.1.2) — and never panics. 300 sequences from a fixed seed.
#[test]
fn randomized_sorts_are_sorted_or_xtde1030_and_never_panic() {
    let mut rng = XorShift(0x5EED_2D0D_2026_1004);
    let mut panics = Vec::new();
    let mut outcomes = [0usize; 2];
    for case in 0..300 {
        let len = 20 + rng.below(81) as usize;
        // Per-case mixture, so that both homogeneous and heterogeneous columns
        // occur often: string share 0 %, ~3 %, ~15 % or 100 %.
        let string_share = [0, 3, 15, 100][rng.below(4) as usize];
        let empty_share = [0, 10][rng.below(2) as usize];
        let items: Vec<Generated> = (0..len)
            .map(|_| {
                let n = rng.below(1000) as i64 - 500;
                let roll = rng.below(100);
                if roll < empty_share {
                    return Generated {
                        literal: "''".into(),
                        shown: String::new(),
                        key: RefKey::Empty,
                    };
                }
                if rng.below(100) < string_share {
                    let w = format!("s{}", n + 500);
                    return Generated {
                        literal: format!("'{w}'"),
                        shown: w.clone(),
                        key: RefKey::Text(w),
                    };
                }
                match rng.below(10) {
                    0 => Generated {
                        literal: "xs:double('NaN')".into(),
                        shown: "NaN".into(),
                        key: RefKey::Number(f64::NAN),
                    },
                    1..=2 => {
                        let v = n as f64 + if n < 0 { -0.5 } else { 0.5 };
                        Generated {
                            literal: format!("{v}e0"),
                            shown: format!("{v}"),
                            key: RefKey::Number(v),
                        }
                    }
                    3..=4 => {
                        let v = n as f64 + if n < 0 { -0.25 } else { 0.25 };
                        Generated {
                            literal: format!("{v}"),
                            shown: format!("{v}"),
                            key: RefKey::Number(v),
                        }
                    }
                    _ => Generated {
                        literal: n.to_string(),
                        shown: n.to_string(),
                        key: RefKey::Number(n as f64),
                    },
                }
            })
            .collect();
        let descending = rng.below(2) == 1;
        let literals: Vec<&str> = items.iter().map(|g| g.literal.as_str()).collect();
        let content = format!(
            r#"<xsl:variable name="s" as="item()*"><xsl:perform-sort select="({})"><xsl:sort select="if (. instance of xs:string) then (if (. = '') then () else .) else ." order="{}"/></xsl:perform-sort></xsl:variable><xsl:value-of select="string-join(for $x in $s return string($x), ',')"/>"#,
            literals.join(", "),
            if descending {
                "descending"
            } else {
                "ascending"
            }
        );
        let has_number = items.iter().any(|g| matches!(g.key, RefKey::Number(_)));
        let has_text = items.iter().any(|g| matches!(g.key, RefKey::Text(_)));
        let expected = if has_number && has_text {
            None
        } else {
            let mut order: Vec<usize> = (0..items.len()).collect();
            order.sort_by(|&a, &b| {
                let o = reference_order(&items[a].key, &items[b].key);
                if descending {
                    o.reverse()
                } else {
                    o
                }
            });
            let shown: Vec<&str> = order.iter().map(|&i| items[i].shown.as_str()).collect();
            Some(out(&shown.join(",")))
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| eval(&content))) {
            Err(_) => panics.push(case),
            Ok(result) => match expected {
                None => {
                    assert_eq!(code(result), "XTDE1030", "case {case}: {content}");
                    outcomes[1] += 1;
                }
                Some(expected) => {
                    assert_eq!(result.unwrap(), expected, "case {case}: {content}");
                    outcomes[0] += 1;
                }
            },
        }
    }
    assert!(
        panics.is_empty(),
        "{} of 300 sorts panicked (cases {panics:?})",
        panics.len()
    );
    // Both branches of the property were exercised.
    assert!(outcomes[0] > 50 && outcomes[1] > 50, "{outcomes:?}");
}

// ── §4.2 whitespace inside xsl:perform-sort with select ──────────────────

/// §4.2: "Any whitespace text node whose following-sibling node is an xsl:param
/// or xsl:sort element is removed from the tree, regardless of any xml:space
/// attributes." So after §4.2 a `select`-ed `xsl:perform-sort` whose
/// whitespace is preserved has no content but its `xsl:sort` children, and
/// §13.2's "[ERR XTSE1040] It is a static error if an xsl:perform-sort
/// instruction with a select attribute has any content other than xsl:sort and
/// xsl:fallback instructions" does not apply.
#[test]
fn preserved_whitespace_before_xsl_sort_in_perform_sort_is_not_content() {
    let source = r#"<r><w k="3">c</w><w k="1">a</w><w k="2">b</w></r>"#;
    let sorted = r#"<w k="1">a</w><w k="2">b</w><w k="3">c</w>"#;
    for (what, template, expected) in [
        (
            "xml:space on the instruction",
            r#"<xsl:template match="/"><out><xsl:perform-sort select="r/w" xml:space="preserve"> <xsl:sort select="@k"/></xsl:perform-sort></out></xsl:template>"#,
            format!("<out>{sorted}</out>"),
        ),
        (
            "xml:space on an ancestor XSLT element",
            r#"<xsl:template match="/" xml:space="preserve"><out><xsl:perform-sort select="r/w">
  <xsl:sort select="@k"/></xsl:perform-sort></out></xsl:template>"#,
            format!("<out>{sorted}</out>"),
        ),
        (
            // The literal result element copies its xml:space attribute (§11.1.2).
            "xml:space on the enclosing literal result element",
            r#"<xsl:template match="/"><out xml:space="preserve"><xsl:perform-sort select="r/w">
  <xsl:sort select="@k"/></xsl:perform-sort></out></xsl:template>"#,
            format!(r#"<out xml:space="preserve">{sorted}</out>"#),
        ),
        (
            "whitespace between two xsl:sort elements",
            r#"<xsl:template match="/"><out><xsl:perform-sort select="r/w" xml:space="preserve"><xsl:sort select="@k"/>
   <xsl:sort select="."/></xsl:perform-sort></out></xsl:template>"#,
            format!("<out>{sorted}</out>"),
        ),
        (
            "a comment before the whitespace",
            r#"<xsl:template match="/"><out><xsl:perform-sort select="r/w" xml:space="preserve"><!-- c -->  <xsl:sort select="@k"/></xsl:perform-sort></out></xsl:template>"#,
            format!("<out>{sorted}</out>"),
        ),
    ] {
        let result = run_body(template, source);
        assert_eq!(result.map_err(|e| e.to_string()), Ok(expected), "{what}");
    }
    // Text that is not whitespace is content, whatever xml:space says.
    for content in [
        r#"<xsl:perform-sort select="(2, 1)"><xsl:sort select="."/>x</xsl:perform-sort>"#,
        r#"<xsl:perform-sort select="(2, 1)" xml:space="preserve"><xsl:sort select="."/> x </xsl:perform-sort>"#,
    ] {
        assert_eq!(code(eval(content)), "XTSE1040", "{content}");
    }
}

// ── the prefix of a QName-valued data-type must be declared ──────────────

/// §5.1: "[ERR XTSE0280] In the case of a prefixed QName used as the value of
/// an attribute in the stylesheet, or appearing within an XPath expression in
/// the stylesheet, it is a static error if the defining element has no
/// namespace node whose name matches the prefix of the QName." and "[ERR
/// XTDE0290] Where the result of evaluating an XPath expression (or an
/// attribute value template) is required to be a lexical QName, then unless
/// otherwise specified it is a non-recoverable dynamic error if the defining
/// element has no namespace node whose name matches the prefix of the lexical
/// QName." §13.1.2 requires the `data-type` value to be "a lexical QName with a
/// non-empty prefix" when it is neither `text` nor `number`.
#[test]
fn a_data_type_qname_needs_a_declared_prefix() {
    let sort = |attrs: &str| {
        eval(&format!(
            r#"<xsl:for-each select="(10, 9)" xmlns:my="urn:sorting:types"><xsl:sort select="." {attrs}/><xsl:value-of select="."/></xsl:for-each>"#
        ))
    };
    assert_eq!(code(sort(r#"data-type="zz:t""#)), "XTSE0280");
    assert_eq!(code(sort(r#"data-type="{'zz:t'}""#)), "XTDE0290");
    // A declared prefix is accepted, fixed or computed (the effect is
    // implementation-defined: here, none).
    assert_eq!(sort(r#"data-type="my:t""#).unwrap(), out("910"));
    assert_eq!(sort(r#"data-type="{'my:t'}""#).unwrap(), out("910"));
    // The error is static even where the sort never runs.
    assert_eq!(
        code(eval(
            r#"<xsl:if test="false()"><xsl:for-each select="()"><xsl:sort select="." data-type="zz:t"/></xsl:for-each></xsl:if>"#
        )),
        "XTSE0280"
    );
}

// ── the UCA collations need the `icu-collation` feature ──────────────────

/// A construct the processor recognizes but does not implement raises
/// `XSLT2-NOTIMPL` naming what would provide it: the UCA collations, in a
/// build without the `icu-collation` feature, name the feature.
#[cfg(not(feature = "icu-collation"))]
#[test]
fn uca_collations_are_not_implemented_without_the_feature() {
    for content in [
        r#"<xsl:for-each select="(2, 1)"><xsl:sort select="." collation="http://www.w3.org/2013/collation/UCA?lang=en"/></xsl:for-each>"#,
        r#"<xsl:value-of select="1" default-collation="http://www.w3.org/2013/collation/UCA?strength=primary"/>"#,
    ] {
        let error = eval(content).expect_err(content);
        assert!(error.is_not_implemented(), "{error}");
        assert!(
            error
                .message
                .ends_with(" is not implemented (icu-collation)"),
            "{:?}",
            error.message
        );
    }
}

// ── document() returns nodes in document order, no duplicates ──

/// §16.1: "The sequence of nodes returned by the function is in document order,
/// with no duplicates. This order has no necessary relationship to the order in
/// which URIs were supplied in the $uri-sequence argument." (`document-0115`.)
/// The order between documents is the processor's, so it is checked against
/// the union operator, which sorts by the same document order.
#[test]
fn document_returns_document_order_without_duplicates() {
    let docs = [
        ("file:///sorting/a.xml", "<a/>"),
        ("file:///sorting/b.xml", "<b/>"),
        ("file:///sorting/c.xml", "<c/>"),
    ];
    let result = run_with_docs(
        &XsltFunctions::new(),
        &sheet(
            r#"<xsl:template match="/">
                 <xsl:variable name="first" select="document(('c.xml', 'a.xml', 'c.xml', 'b.xml', 'a.xml'))"/>
                 <xsl:variable name="again" select="document(('b.xml', 'a.xml', 'c.xml'))"/>
                 <xsl:variable name="union" select="document('a.xml') | document('b.xml') | document('c.xml')"/>
                 <out>
                   <xsl:value-of select="count($first)"/>
                   <xsl:text>|</xsl:text>
                   <xsl:value-of select="for $d in $first return name($d/*)" separator=","/>
                   <xsl:text>|</xsl:text>
                   <xsl:value-of select="for $d in $again return name($d/*)" separator=","/>
                   <xsl:text>|</xsl:text>
                   <xsl:value-of select="for $d in $union return name($d/*)" separator=","/>
                 </out>
               </xsl:template>"#,
        ),
        "<doc/>",
        &docs,
    )
    .unwrap();
    let inner = result
        .strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .unwrap_or_else(|| panic!("{result}"));
    let parts: Vec<&str> = inner.split('|').collect();
    assert_eq!(parts[0], "3", "{result}");
    assert_eq!(
        parts[1], parts[3],
        "document order, as the union has it: {result}"
    );
    assert_eq!(
        parts[2], parts[3],
        "the argument order does not matter: {result}"
    );
}

// ── An excluded element's default-collation is not checked ──

/// §3.12: "When a node is effectively excluded from a stylesheet module the
/// stylesheet module has the same effect as if the node were not there. Among
/// other things this means that no static or dynamic errors will be reported
/// in respect of the element and its contents, other than errors in the
/// use-when attribute itself." The same attribute on an element that is *not*
/// excluded is §3.6.1's "[ERR XTSE0125] It is a static error if the value of an
/// [xsl:]default-collation attribute, after resolving against the base URI,
/// contains no URI that the implementation recognizes as a collation URI." —
/// reported at the element.
#[test]
fn default_collation_on_an_excluded_element_is_not_checked() {
    let main = r#"<xsl:template match="/"><out>ok</out></xsl:template>"#;
    for excluded in [
        r#"<xsl:template name="x" use-when="false()" default-collation="urn:unknown"/>"#,
        r#"<xsl:template name="x" use-when="false()"><xsl:value-of select="1" default-collation="urn:unknown"/></xsl:template>"#,
        r#"<xsl:template name="x"><out xsl:use-when="false()" xsl:default-collation="urn:unknown"/></xsl:template>"#,
    ] {
        assert_eq!(
            run_body(&format!("{main}{excluded}"), "<doc/>").map_err(|e| e.to_string()),
            Ok(out("ok")),
            "{excluded}"
        );
    }
    let included = "\n<xsl:template name=\"x\"\n    default-collation=\"urn:unknown\"/>";
    let error = run_body(&format!("{main}{included}"), "<doc/>").expect_err("XTSE0125");
    assert_eq!(error.code, "XTSE0125", "{error}");
    let location = error.location.as_ref().expect("the error has a location");
    assert_eq!(
        location.module_uri.as_deref(),
        Some("file:///sorting/main.xsl")
    );
    assert!(location.line >= 3, "{location:?}");
}
