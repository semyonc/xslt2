//! M2i — core defects in what M1 delivered. One section per cluster of the
//! M2a triage (`XSLT2_M2A_TRIAGE.md`) that `XSLT2_M2I_REPORT.md` closes; every
//! test names the suite case(s) it reproduces and quotes the specification
//! sentence it checks.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, MemoryResolver, QName, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Runs `sheet` (a whole stylesheet module) over `source` and returns the
/// serialized primary result.
fn run_sheet(sheet: &str, source: &str) -> Result<String, XsltError> {
    run_sheet_with(sheet, source, &[])
}

/// The same, with extra in-memory resources visible to the resolver.
fn run_sheet_with(sheet: &str, source: &str, extra: &[(&str, &str)]) -> Result<String, XsltError> {
    run_sheet_with_options(sheet, source, extra, TransformOptions::default())
}

/// The same, with explicit [`TransformOptions`] (e.g. `strict_recoverable`).
fn run_sheet_with_options(
    sheet: &str,
    source: &str,
    extra: &[(&str, &str)],
    options: TransformOptions,
) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///m2i/in.xml", source);
    for (uri, text) in extra {
        mem.insert(*uri, *text);
    }
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///m2i/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, options)
        .with_resolver(resolver)
        .transform_source("file:///m2i/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// Runs a stylesheet body from the named template `main`, with no source.
fn run_main(body: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled =
        Compiler::new(&names, &functions).compile_str(&sheet(body), "file:///m2i/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_initial_template(QName::local("main"))
        .transform_without_source()?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// The same, for a stylesheet body wrapped in a plain `xsl:stylesheet`.
fn run(body: &str, source: &str) -> Result<String, XsltError> {
    run_sheet(&sheet(body), source)
}

fn sheet(body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" exclude-result-prefixes=\"xs\">\
         {body}</xsl:stylesheet>"
    )
}

/// The error code of a failed run, or a panic naming what succeeded.
fn code_of(result: Result<String, XsltError>) -> String {
    match result {
        Ok(out) => panic!("expected an error, the transformation produced {out:?}"),
        Err(e) => e.code.to_string(),
    }
}

// ═══ L-ATTR-AXIS-PATTERN — the default axis of an `attribute()` step ═══════

/// `match-089`, `match-098`, `accessor-001`/`-014`/`-015`/`-021`, and the
/// pattern half of `accessor-028`, `match-106`/`-114`/`-116`.
///
/// XPath 2.0 §3.2.4: "If the axis name is omitted from an axis step, the
/// default axis is child unless the axis step contains an AttributeTest or
/// SchemaAttributeTest; in that case, the default axis is attribute."
/// XSLT 2.0 §5.5.3 gives a pattern the meaning of its equivalent expression,
/// so `match="attribute()"` matches attributes.
#[test]
fn a_bare_attribute_test_in_a_pattern_takes_the_attribute_axis() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/@*"/></out></xsl:template>
           <xsl:template match="attribute()"><a><xsl:value-of select="name()"/></a></xsl:template>"#,
        r#"<doc x="1" y="2"/>"#,
    )
    .unwrap();
    assert_eq!(out, "<out><a>x</a><a>y</a></out>");
}

/// The named and the typed forms, and a bare `attribute()` as the last step of
/// a multi-step pattern (`match-089`: `document-node()/child::*/child::*/attribute()`).
#[test]
fn named_typed_and_multi_step_attribute_tests_take_the_attribute_axis() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/e/@*"/></out></xsl:template>
           <xsl:template match="attribute(x)" priority="1">X</xsl:template>
           <xsl:template match="attribute(*, xs:untypedAtomic)">T</xsl:template>
           <xsl:template match="document-node()/child::*/child::*/attribute(z)" priority="3">Z</xsl:template>"#,
        r#"<doc><e x="1" y="2" z="3"/></doc>"#,
    )
    .unwrap();
    assert_eq!(out, "<out>XTZ</out>");
}

/// The rule is about the *default*: an explicit `child::` axis with an
/// `attribute()` test is still the child axis, which never contains an
/// attribute, so the rule never fires (the built-in rule copies the value).
#[test]
fn an_explicit_child_axis_with_an_attribute_test_never_matches() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/@x"/></out></xsl:template>
           <xsl:template match="child::attribute()">WRONG</xsl:template>"#,
        r#"<doc x="v"/>"#,
    )
    .unwrap();
    assert_eq!(out, "<out>v</out>");
}

// ═══ L-PATTERN-CHILD-DOCNODE — `child::document-node()` ════════════════════

/// `match-048`. §5.5.3: "If the NodeTest in PS is `document-node()` (optionally
/// with arguments), and if no explicit axis is specified, then the axis in step
/// PS is taken as `self` rather than `child`." With an explicit `child::` the
/// step stays on the child axis, and a document node is never a child.
#[test]
fn an_explicit_child_axis_document_node_test_never_matches() {
    let out = run(
        r#"<xsl:template match="child::document-node()"><d>WRONG</d></xsl:template>
           <xsl:template match="*"><out>OK</out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>OK</out>");
}

/// The implicit axis keeps working, both alone and as the first step of a
/// longer RelativePathPattern.
#[test]
fn an_implicit_axis_document_node_test_still_matches() {
    let out = run(
        r#"<xsl:template match="document-node()"><out><xsl:apply-templates select="doc"/></out></xsl:template>
           <xsl:template match="document-node(element(doc))/doc">D</xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>D</out>");
}

// ═══ WS-42-MERGE — §4.2 merges text left adjacent by removed comments/PIs ══

/// `whitespace-012`, `whitespace-013`, `axes-090`, `type-0125`, and the engine
/// half of `expression-2101`. §4.2: "All comments and processing instructions
/// are removed. Any text nodes that are now adjacent to each other are merged.
/// Any whitespace text node that satisfies both the following conditions is
/// removed from the tree" — so the whitespace test sees the merged node.
#[test]
fn text_split_by_a_comment_or_pi_is_merged_before_stripping() {
    let out = run(
        r#"<xsl:template match="/"><out><e>   h<!--c-->   </e><e>   <!--c-->h   </e><e>   <?pi?>h</e><e>h<?pi?>   </e></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(
        out,
        "<out><e>   h   </e><e>   h   </e><e>   h</e><e>h   </e></out>"
    );
}

/// A run that is whitespace throughout is still stripped as a whole.
#[test]
fn a_whitespace_run_split_by_a_comment_is_still_stripped() {
    let out = run(
        r#"<xsl:template match="/"><out><e>  <!--c-->  <?pi?>  </e></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out><e/></out>");
}

/// The merged node is **one** text node: in a sequence (§6.3, no complex
/// content to merge it afterwards) it counts once.
#[test]
fn a_merged_run_is_a_single_text_node_in_a_sequence() {
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="text()*">a<!--c-->b<?pi?>c</xsl:variable>
             <out><xsl:value-of select="count($v), string($v[1])"/></out>
           </xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>1 abc</out>");
}

/// §3.12: "Conditional element exclusion happens after stripping of
/// whitespace text nodes from the stylesheet" — an element excluded by
/// `use-when` still separates two text nodes, so whitespace on one side of it
/// is stripped on its own.
#[test]
fn an_excluded_element_still_separates_text_runs() {
    let out = run(
        r#"<xsl:template match="/"><out><e>   <xsl:if use-when="false()" test="true()"/>h</e></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out><e>h</e></out>");
}

// ═══ D-E11-DYNAMIC-CODES — dynamic conditions with their own codes ═════════

/// `error-0030a`. §2.2: "\[ERR XTDE0030\] It is a non-recoverable dynamic
/// error if the effective value of an attribute written using curly brackets,
/// in a position where an attribute value template is permitted, is a value
/// that is not one of the permitted values for that attribute."
#[test]
fn a_bad_effective_value_of_an_avt_is_xtde0030() {
    let code = code_of(run_main(
        r#"<xsl:param name="x" select="'bananas'"/>
           <xsl:template name="main"><out><xsl:message terminate="{$x}"/></out></xsl:template>"#,
    ));
    assert_eq!(code, "XTDE0030");
}

/// `message-0316` (a pass that must stay one): the same bad value written
/// **without** curly brackets is the static XTSE0020 — "It is a static error if
/// an attribute (other than an attribute written using curly brackets in a
/// position where an attribute value template is permitted) contains a value
/// that is not one of the permitted values for that attribute" — and is
/// reported when the stylesheet is compiled, whether or not it is reached.
#[test]
fn a_bad_fixed_value_in_an_avt_position_is_xtse0020_at_compile_time() {
    let code = code_of(run_main(
        r#"<xsl:template name="main"><out/></xsl:template>
           <xsl:template name="never"><xsl:message terminate="NO"/></xsl:template>"#,
    ));
    assert_eq!(code, "XTSE0020");
}

/// `error-0510a`, `apply-templates-001`. §6.3: "\[ERR XTTE0510\] It is a type
/// error if an xsl:apply-templates instruction with no select attribute is
/// evaluated when the context item is not a node."
#[test]
fn apply_templates_with_an_atomic_context_item_is_xtte0510() {
    let code = code_of(run_main(
        r#"<xsl:template name="main"><out><xsl:for-each select="1 to 5"><xsl:apply-templates/></xsl:for-each></out></xsl:template>"#,
    ));
    assert_eq!(code, "XTTE0510");
}

/// `error-0600a`. §9.2: "\[ERR XTTE0600\] If a default value is given
/// explicitly, that is, if there is either a select attribute or a non-empty
/// sequence constructor, then it is a type error if the default value cannot
/// be converted to the required type" — XTTE0590 is for a *supplied* value.
#[test]
fn an_explicit_param_default_that_fails_conversion_is_xtte0600() {
    let template = run(
        r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
           <xsl:template match="doc"><xsl:param name="p" as="xs:integer" select="3.5"/><xsl:value-of select="$p"/></xsl:template>"#,
        "<doc/>",
    );
    assert_eq!(code_of(template), "XTTE0600");
    let global = run(
        r#"<xsl:param name="g" as="xs:integer"><xsl:sequence select="'x'"/></xsl:param>
           <xsl:template match="/"><out><xsl:value-of select="$g"/></out></xsl:template>"#,
        "<doc/>",
    );
    assert_eq!(code_of(global), "XTTE0600");
}

/// A supplied value that fails conversion keeps XTTE0590.
#[test]
fn a_supplied_param_value_that_fails_conversion_is_still_xtte0590() {
    let code = code_of(run(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc"><xsl:with-param name="p" select="'x'"/></xsl:apply-templates></out></xsl:template>
           <xsl:template match="doc"><xsl:param name="p" as="xs:integer" select="1"/><xsl:value-of select="$p"/></xsl:template>"#,
        "<doc/>",
    ));
    assert_eq!(code, "XTTE0590");
}

/// `error-0610a`. §9.2: "\[ERR XTDE0610\] If an optional parameter has no
/// select attribute and has an empty sequence constructor, and if there is an
/// as attribute, then the default value of the parameter is an empty sequence.
/// If the empty sequence is not a valid instance of the required type defined
/// in the as attribute, then the parameter is treated as a required parameter,
/// which means that it is a non-recoverable dynamic error if the caller
/// supplies no value for the parameter."
#[test]
fn an_implicitly_required_param_with_no_value_is_xtde0610() {
    let code = code_of(run(
        r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
           <xsl:template match="doc"><xsl:param name="p" as="xs:integer"/><xsl:value-of select="$p"/></xsl:template>"#,
        "<doc/>",
    ));
    assert_eq!(code, "XTDE0610");
    // An `as` that allows the empty sequence makes the empty default fine.
    let out = run(
        r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
           <xsl:template match="doc"><xsl:param name="p" as="xs:integer?"/><xsl:value-of select="count($p)"/></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>0</out>");
}

// ═══ F13-REQUIRED-GLOBAL-PARAM — XTDE0050 at the invocation ═══════════════

/// `initial-mode-003`. §2.3: "\[ERR XTDE0050\] It is a non-recoverable
/// dynamic error if the stylesheet that is invoked declares a visible
/// stylesheet parameter with required="yes" and no value for this parameter is
/// supplied during the invocation of the stylesheet." The parameter is never
/// referenced, so a lazy check would never fire.
#[test]
fn an_unreferenced_required_stylesheet_param_is_still_xtde0050() {
    let code = code_of(run(
        r#"<xsl:param name="limbo" required="yes"/>
           <xsl:template match="/"><out/></xsl:template>"#,
        "<doc/>",
    ));
    assert_eq!(code, "XTDE0050");
}

// ═══ C4-AS-LEXICAL-TYPE — `as` conversion keyed on the expanded type name ══

/// `as-0116`: URI promotion with the XML Schema namespace bound to `xsd`.
/// XPath 2.0 Appendix B.1: "A value of type xs:anyURI (or any type derived by
/// restriction from xs:anyURI) can be promoted to the type xs:string." The
/// conversion is decided by the expanded name, not by the `xs:` spelling.
#[test]
fn uri_promotion_works_under_any_prefix() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xsd="http://www.w3.org/2001/XMLSchema" exclude-result-prefixes="xsd">
                 <xsl:variable name="v" select="xsd:anyURI('urn:x')" as="xsd:string"/>
                 <xsl:template match="/"><out><xsl:value-of select="$v instance of xsd:anyURI, $v instance of xsd:string"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>false true</out>");
}

/// `xpath-default-namespace-0502`: an unprefixed type name resolves through
/// the default element/type namespace — XPath 2.0 §2.1.1, "used for any
/// unprefixed QName appearing in a position where an element or type name is
/// expected" — which `[xsl:]xpath-default-namespace` sets (§5.2).
#[test]
fn numeric_promotion_works_with_an_unprefixed_type_name() {
    let out = run(
        r#"<xsl:template match="/"><out xsl:xpath-default-namespace="http://www.w3.org/2001/XMLSchema">
             <xsl:variable name="v" select="2.0" as="double"/>
             <xsl:value-of select="$v instance of double"/></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>true</out>");
}

/// `type-0174`, `type-0175`. Appendix B.1 promotes `xs:decimal` to `xs:float`
/// or `xs:double` and `xs:float` to `xs:double` — never `xs:double` to
/// `xs:float`, so the value does not match and §9.3 gives XTTE0570.
#[test]
fn a_double_is_not_promoted_to_a_float() {
    let code = code_of(run(
        r#"<xsl:template match="/"><xsl:variable name="d" select="xs:double('1e4')"/>
             <xsl:variable name="f" select="$d" as="xs:float"/><out><xsl:value-of select="$f"/></out></xsl:template>"#,
        "<doc/>",
    ));
    assert_eq!(code, "XTTE0570");
    // The permitted promotions still happen.
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="f" select="1.5" as="xs:float"/>
             <xsl:variable name="d" select="xs:float(2)" as="xs:double"/>
             <out><xsl:value-of select="$f instance of xs:float, $d instance of xs:double"/></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>true true</out>");
}

// ═══ L-PI-COMMENT-REPAIR — silent repairs, not errors ══════════════════════

/// `construct-node-007`, `construct-node-017`. §11.8: "In the generated
/// comment node, the processor must insert a space after any occurrence of -
/// that is followed by another - or that ends the comment."
#[test]
fn a_constructed_comment_is_repaired() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:comment select="'--Valid comment--'"/><xsl:comment> a-b -</xsl:comment></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out><!--- -Valid comment- - --><!-- a-b - --></out>");
}

/// The repair is made to the node, not by the serializer: the string value of
/// the constructed comment already carries the spaces.
#[test]
fn the_comment_repair_is_in_the_node_value() {
    let out = run(
        r#"<xsl:template match="/"><xsl:variable name="c" as="comment()"><xsl:comment>x--</xsl:comment></xsl:variable>
             <out><xsl:value-of select="string-length(string($c))"/></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    // "x--" → "x- - ".
    assert_eq!(out, "<out>5</out>");
}

/// `construct-node-022`, `node-1906`. §11.6: "If the result of evaluating the
/// content of the xsl:processing-instruction contains the string ?>, this
/// string is modified by inserting a space between the ? and > characters."
#[test]
fn constructed_processing_instruction_data_is_repaired() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:processing-instruction name="p" select="'--?>?>-'"/>
             <xsl:variable name="pi" as="processing-instruction()"><xsl:processing-instruction name="q">a?>b</xsl:processing-instruction></xsl:variable>
             <xsl:value-of select="string($pi)"/></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out><?p --? >? >-?>a? &gt;b</out>");
}

// ═══ B9-RESOLVE-URI-RFC — `xml:base` and relative URIs per RFC 3986 ════════

/// `base-uri-014`: an absolute-path reference keeps the base's scheme and
/// authority (RFC 3986 §5.2.2), so `xml:base="/main/"` under
/// `http://www.example.org/` is `http://www.example.org/main/`.
#[test]
fn an_absolute_path_xml_base_keeps_the_authority() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xml:base="http://www.example.org/">
                 <xsl:template match="/" xml:base="/main/"><out><xsl:value-of select="static-base-uri()"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>http://www.example.org/main/</out>");
}

/// `base-uri-013`: `d:` is a scheme (RFC 3986 §3.1, `ALPHA *( ALPHA / DIGIT /
/// "+" / "-" / "." )`), so `d://tests/` is an absolute URI and is not joined to
/// the outer base.
#[test]
fn a_one_letter_scheme_is_a_scheme() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xml:base="http://www.baseuri.exmpl/tests/">
                 <xsl:variable name="t" xml:base="d://tests/"><a/></xsl:variable>
                 <xsl:template match="/"><out><xsl:value-of select="base-uri($t)"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>d://tests/</out>");
}

/// `document-2003`: RFC 3986 §5.2.4 leaves a trailing slash after a final
/// `..`, so `xml:base="d/.."` names the stylesheet's own *directory* and
/// `x.xml` resolves inside it — not beside it.
#[test]
fn a_final_dot_dot_keeps_the_trailing_slash() {
    let out = run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:template match="/"><out xml:base="d/.."><xsl:value-of select="static-base-uri()"/>|<xsl:copy-of select="document('x.xml')/*"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[("file:///m2i/x.xml", "<found/>")],
    );
    // The stylesheet is `file:///m2i/main.xsl`; `d/..` from it is
    // `file:///m2i/`, not `file:///m2i` (which would put `x.xml` at the root).
    assert_eq!(
        out.unwrap(),
        r#"<out xml:base="d/..">file:///m2i/|<found/></out>"#
    );
}

/// Review R7. RFC 3986 §5.2.3 merges a reference against a base with no
/// authority and no "/" in its path into the reference's own path (`../b`),
/// and §5.2.4 rule A: "If the input buffer begins with a prefix of "../" or
/// "./", then remove that prefix from the input buffer" — so a `..` with
/// nothing to remove disappears under a scheme: `urn:a` + `../b` is `urn:b`.
/// (A base with no scheme at all is outside RFC 3986; there the leading `..`
/// is still kept.)
#[test]
fn an_unmatched_dot_dot_is_removed_when_resolving_under_a_scheme() {
    use xslt2::resolver::resolve_uri;
    assert_eq!(resolve_uri("urn:a", "../b"), "urn:b");
    assert_eq!(resolve_uri("mailto:x", "../y"), "mailto:y");
    assert_eq!(resolve_uri("urn:a", "./../b"), "urn:b");
    // A reference with its own scheme: T.path = remove_dot_segments(R.path).
    assert_eq!(resolve_uri("http://a/b/c/d;p?q", "http:../g"), "http:g");
    // Outside RFC 3986 (no scheme anywhere) the leading `..` survives.
    assert_eq!(resolve_uri("a/b.xsl", "../../c"), "../c");
    // End to end: the static base URI of an element under `xml:base`.
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:template name="main"><out xml:base="../b"><xsl:value-of select="static-base-uri()"/></out></xsl:template>"#,
            ),
            "urn:a",
        )
        .unwrap();
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_initial_template(QName::local("main"))
        .transform_without_source()
        .unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        r#"<out xml:base="../b">urn:b</out>"#
    );
}

// ═══ Review R6 — module identity vs. kept empty path segments ═════════════

/// Review R6. §3.10.2: "\[ERR XTSE0180\] It is a static error if a stylesheet
/// module directly or indirectly includes itself." RFC 3986 rightly keeps
/// empty path segments, so `.//main.xsl` resolves to `file:///m2i//main.xsl`
/// — but for a `file:` URI that is the same file as `file:///m2i/main.xsl`,
/// and the cycle check must see it as the same module (before the fix it
/// loaded the `//` spelling, which with real files recursed until the path was
/// too long, and was reported as XTSE0165).
#[test]
fn a_self_include_spelled_with_an_empty_segment_is_xtse0180() {
    let code = code_of(run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:include href=".//main.xsl"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[],
    ));
    assert_eq!(code, "XTSE0180");
}

/// Review R6, a two-module cycle through such a spelling: `main.xsl`
/// includes `.//b.xsl` (= `file:///m2i//b.xsl`), which includes `main.xsl` —
/// resolved against `b`'s own `//` URI, `file:///m2i//main.xsl`. (The
/// in-memory entry for the `//` spelling stands in for `FileResolver`, which
/// reads the same file for both spellings.) The same for an import cycle,
/// XTSE0210 (§3.10.3: "It is a static error if a stylesheet module directly
/// or indirectly imports itself").
#[test]
fn a_two_module_cycle_through_an_empty_segment_is_detected() {
    let b = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:include href="main.xsl"/></xsl:stylesheet>"#
    );
    let code = code_of(run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:include href=".//b.xsl"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[("file:///m2i//b.xsl", &b)],
    ));
    assert_eq!(code, "XTSE0180");
    let b = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:import href="main.xsl"/></xsl:stylesheet>"#
    );
    let code = code_of(run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:import href=".//b.xsl"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[("file:///m2i//b.xsl", &b)],
    ));
    assert_eq!(code, "XTSE0210");
}

/// Review R6, the control: only *empty* segments are ignored. `a//b.xsl`
/// importing `main.xsl` reaches `file:///m2i/a//main.xsl`, which is the file
/// `a/main.xsl` — a different module, not a cycle (here it does not exist,
/// so the import fails to load: XTSE0165).
#[test]
fn an_empty_segment_in_another_directory_is_not_a_cycle() {
    let b = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:import href="main.xsl"/></xsl:stylesheet>"#
    );
    let code = code_of(run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:import href="a//b.xsl"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[("file:///m2i/a//b.xsl", &b)],
    ));
    assert_eq!(code, "XTSE0165");
}

// ═══ SIMPLE-CONTENT-52 — §5.7.2 over constructed nodes ════════════════════

/// `namespace-2602`. §5.7.2 step 3, "The sequence is atomized": an attribute
/// node built in the content of `xsl:namespace` contributes its value, so the
/// namespace URI is not zero-length.
#[test]
fn an_attribute_node_in_simple_content_contributes_its_value() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:namespace name="p"><xsl:attribute name="a">urn:p</xsl:attribute></xsl:namespace></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out xmlns:p="urn:p"/>"#);
}

/// `xsl-document-0601`. A document node built inside simple content is one
/// item whose atomized value is its string value: comments inside it
/// contribute nothing, adjacent atomic values in it were joined with a space
/// by §5.7.1, and it is not merged with neighbouring text.
#[test]
fn a_document_node_in_simple_content_contributes_its_string_value() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:value-of separator="|">
             <xsl:document><xsl:comment>c</xsl:comment><xsl:sequence select="1, 2"/>x</xsl:document>
             <xsl:text>t</xsl:text>
           </xsl:value-of></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>1 2x|t</out>");
}

/// The same §5.7.1 rules apply to an element built inside simple content:
/// adjacent atomic values in its content are separated by a space, and a
/// comment child is not part of its string value.
#[test]
fn an_element_in_simple_content_contributes_its_string_value() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:attribute name="a"><e><xsl:sequence select="1, 2"/><xsl:comment>c</xsl:comment>z</e></xsl:attribute></out></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out a="1 2z"/>"#);
}

// ═══ C6-DYNAMIC-CONTEXT-DATETIME — one clock per transformation ═══════════

/// `date-020`. §5.4.3.2: "The current date and time represents an
/// implementation-dependent point in time during processing of the
/// transformation; it does not change during the course of the
/// transformation." Separate expressions, separated by work, see one value;
/// and the implicit timezone is the one that value carries.
#[test]
fn current_date_time_is_stable_across_expressions() {
    let out = run(
        r#"<xsl:variable name="g" select="current-dateTime()"/>
           <xsl:template match="/">
             <xsl:variable name="now" select="current-dateTime()"/>
             <xsl:variable name="work" select="sum(for $i in 1 to 200000 return $i)"/>
             <out><xsl:value-of select="$work gt 0 and $now = current-dateTime() and $g = $now
                                        and implicit-timezone() = timezone-from-dateTime($now)
                                        and current-date() = xs:date($now)"/></out>
           </xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, "<out>true</out>");
}

// ═══ G11-AVT-COMMENT-SCAN — braces and quotes inside XPath comments ═══════

/// `avt-1202`. §5.6, Note: "An expression within a variable part may contain
/// an unescaped curly bracket within a StringLiteral or within a comment." A
/// quote inside a comment does not open a string literal, and comments nest
/// (XPath 2.0 §2.6).
#[test]
fn a_curly_bracket_inside_an_xpath_comment_does_not_end_the_avt_expression() {
    let out = run(
        r#"<xsl:template match="/"><out a="some{('exp', (: isn't }fun{ (: nested } :) :) 23)}stuff"/></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out a="someexp 23stuff"/>"#);
}

// ═══ G7-ATTRSET-MERGE-ORDER — each declaration expanded in turn ═══════════

/// `attribute-set-1512`. §10.2: "Each xsl:attribute-set declaration whose name
/// matches is processed as follows. … If the xsl:attribute-set declaration has
/// a use-attribute-sets attribute, the attribute is expanded by applying this
/// algorithm recursively. If the xsl:attribute-set declaration contains one or
/// more xsl:attribute instructions, these instructions are evaluated … These
/// attribute nodes are appended to the result sequence." So the second
/// declaration's `use-attribute-sets` comes *after* the first declaration's
/// own attributes, and wins over them.
#[test]
fn same_named_attribute_set_declarations_are_expanded_one_after_another() {
    let out = run(
        r#"<xsl:template match="/"><out xsl:use-attribute-sets="child"/></xsl:template>
           <xsl:attribute-set name="child" use-attribute-sets="alice">
             <xsl:attribute name="follow">yellowbrickroad</xsl:attribute>
             <xsl:attribute name="rabbithole">shallow</xsl:attribute>
           </xsl:attribute-set>
           <xsl:attribute-set name="child" use-attribute-sets="rabbit">
             <xsl:attribute name="follow">theleader</xsl:attribute>
             <xsl:attribute name="alice">intoxicated</xsl:attribute>
           </xsl:attribute-set>
           <xsl:attribute-set name="rabbit"><xsl:attribute name="rabbithole">deep</xsl:attribute></xsl:attribute-set>
           <xsl:attribute-set name="alice"><xsl:attribute name="alice">ondrugs</xsl:attribute></xsl:attribute-set>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(
        out,
        r#"<out alice="intoxicated" follow="theleader" rabbithole="deep"/>"#
    );
}

/// "Where two such declarations have different import precedence, the one
/// with lower import precedence is processed first" — whatever their order in
/// the module graph.
#[test]
fn an_imported_attribute_set_declaration_is_processed_first() {
    let imported = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:attribute-set name="s"><xsl:attribute name="a">imported</xsl:attribute><xsl:attribute name="b">imported</xsl:attribute></xsl:attribute-set>
           </xsl:stylesheet>"#
    );
    let out = run_sheet_with(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:import href="imp.xsl"/>
                 <xsl:attribute-set name="s"><xsl:attribute name="a">main</xsl:attribute></xsl:attribute-set>
                 <xsl:template match="/"><out xsl:use-attribute-sets="s"/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
        &[("file:///m2i/imp.xsl", &imported)],
    )
    .unwrap();
    assert_eq!(out, r#"<out a="main" b="imported"/>"#);
}

// ═══ B10-COPY-BASEURI — a parentless xsl:copy keeps the base URI ══════════

/// `base-uri-024`. §11.9.1: "The base URI of a node is copied, except in the
/// case of an element node having an xml:base attribute, in which case the
/// base URI of the new node is taken as the value of the xml:base attribute,
/// resolved if it is relative against the base URI of the xsl:copy
/// instruction."
#[test]
fn a_parentless_shallow_copy_keeps_the_base_uri_of_its_original() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xml:base="http://s.example/">
                 <xsl:template match="/"><out><xsl:apply-templates select="r/*"/></out></xsl:template>
                 <xsl:template match="*">
                   <xsl:variable name="c" as="element()"><xsl:copy/></xsl:variable>
                   <b><xsl:value-of select="base-uri($c)"/></b>
                 </xsl:template>
               </xsl:stylesheet>"#
        ),
        r#"<r xml:base="http://a.example/x/"><inherited/><own xml:base="sub/"/></r>"#,
    )
    .unwrap();
    // `inherited` copies its base URI; `own` has an xml:base of its own, which
    // resolves against the xsl:copy instruction's base URI.
    assert_eq!(
        out,
        "<out><b>http://a.example/x/</b><b>http://s.example/sub/</b></out>"
    );
}

/// Review R1. "The base URI of a node is copied" (§11.9.1) — the node's *base
/// URI*, i.e. what `fn:base-uri` reports for it, not the raw value of the
/// nearest `xml:base`. With a relative `xml:base` on an ancestor
/// (`sub/` under `http://a.example/x/`) the two differ: the copy of `f` must
/// have `http://a.example/x/sub/`, the same as `base-uri()` of the original;
/// a two-level chain (`../up/` under that) resolves step by step (RFC 3986
/// §5.2.4 removes the `..`).
#[test]
fn a_parentless_shallow_copy_resolves_a_relative_xml_base_chain() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xml:base="http://s.example/">
                 <xsl:template match="/">
                   <out><xsl:apply-templates select="r/e/f | r/e/g/h"/></out>
                 </xsl:template>
                 <xsl:template match="f">
                   <xsl:variable name="c" as="element()"><xsl:copy/></xsl:variable>
                   <b copy="{{base-uri($c)}}" original="{{base-uri(.)}}"/>
                 </xsl:template>
                 <xsl:template match="h">
                   <xsl:variable name="c" as="element()"><xsl:copy/></xsl:variable>
                   <b copy="{{base-uri($c)}}"/>
                 </xsl:template>
               </xsl:stylesheet>"#
        ),
        r#"<r xml:base="http://a.example/x/"><e xml:base="sub/"><f/><g xml:base="../up/"><h/></g></e></r>"#,
    )
    .unwrap();
    assert_eq!(
        out,
        r#"<out><b copy="http://a.example/x/sub/" original="http://a.example/x/sub/"/><b copy="http://a.example/x/up/"/></out>"#
    );
}

/// Review R1, the bottom of the chain: with no `xml:base` anywhere the copy
/// has the base URI of the document the original lives in (here the source
/// document's URI), and a relative `xml:base` resolves against that.
#[test]
fn a_parentless_shallow_copy_resolves_a_relative_xml_base_against_the_document_uri() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xml:base="http://s.example/">
                 <xsl:template match="/"><out><xsl:apply-templates select="r/*/*"/></out></xsl:template>
                 <xsl:template match="*">
                   <xsl:variable name="c" as="element()"><xsl:copy/></xsl:variable>
                   <b><xsl:value-of select="base-uri($c)"/></b>
                 </xsl:template>
               </xsl:stylesheet>"#
        ),
        r#"<r><plain><f/></plain><rel xml:base="sub/"><f/></rel></r>"#,
    )
    .unwrap();
    assert_eq!(
        out,
        "<out><b>file:///m2i/in.xml</b><b>file:///m2i/sub/</b></out>"
    );
}

// ═══ D-E12 (error-0420a) — XTDE0420 for a document node inside an element ═

/// `error-0420a`. §5.7.1: "\[ERR XTDE0420\] It is a non-recoverable dynamic
/// error if the result sequence used to construct the content of a document
/// node contains a namespace node or attribute node." The document node here
/// is the `xsl:copy` of the source root, built inside element content; its
/// content is still the content of a document node.
#[test]
fn an_attribute_in_a_nested_document_node_is_xtde0420() {
    let copy = run(
        r#"<xsl:template match="/"><a><xsl:copy><xsl:attribute name="x">5</xsl:attribute></xsl:copy></a></xsl:template>"#,
        "<doc/>",
    );
    assert_eq!(code_of(copy), "XTDE0420");
    let document = run(
        r#"<xsl:template match="/"><a><xsl:document><xsl:namespace name="p">urn:p</xsl:namespace></xsl:document></a></xsl:template>"#,
        "<doc/>",
    );
    assert_eq!(code_of(document), "XTDE0420");
    // An element inside the nested document may still have attributes.
    let out = run(
        r#"<xsl:template match="/"><a><xsl:document><b><xsl:attribute name="x">5</xsl:attribute></b></xsl:document></a></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, r#"<a><b x="5"/></a>"#);
}

// ═══ D-E13-DOCUMENT-CODE and error-1160a — document()'s own error codes ════

/// `error-FODC0002a`. §16.1: the URI "is passed to the doc function … If an
/// error occurs during evaluation of the doc function, the processor may
/// either signal this error in the normal way". F&O §15.5.4 (`fn:doc`): a
/// resource that cannot be retrieved raises FODC0002 — not XTDE1170, which is
/// `unparsed-text()`'s code (§16.2).
#[test]
fn a_missing_document_is_fodc0002() {
    for call in ["document('no-such.xml')", "doc('no-such.xml')"] {
        let code = code_of(run(
            &format!(
                r#"<xsl:template match="/"><out><xsl:copy-of select="{call}"/></out></xsl:template>"#
            ),
            "<doc/>",
        ));
        assert_eq!(code, "FODC0002", "{call}");
    }
    // A resource that is there but is not well-formed is FODC0002 too.
    let code = code_of(run_sheet_with(
        &sheet(
            r#"<xsl:template match="/"><out><xsl:copy-of select="document('bad.xml')"/></out></xsl:template>"#,
        ),
        "<doc/>",
        &[("file:///m2i/bad.xml", "<a></b>")],
    ));
    assert_eq!(code, "FODC0002");
}

/// `error-1160a`. §16.1: "\[ERR XTRE1160\] When a URI reference contains a
/// fragment identifier, it is a recoverable dynamic error if the media type is
/// not one that is recognized by the processor … The optional recovery action
/// is to ignore the fragment identifier and return the document node." This
/// processor recognizes no media type's fragment identifiers. Review R4:
/// `TransformOptions::strict_recoverable` decides — "Signal recoverable errors
/// instead of taking the spec's recovery action" — so it signals in strict
/// mode and recovers otherwise.
#[test]
fn a_fragment_identifier_in_document_is_xtre1160_in_strict_mode() {
    let strict = TransformOptions {
        strict_recoverable: true,
        ..TransformOptions::default()
    };
    let code = code_of(run_sheet_with_options(
        &sheet(
            r#"<xsl:template match="/"><out><xsl:copy-of select="document('x.xml#frag')"/></out></xsl:template>"#,
        ),
        "<doc/>",
        &[("file:///m2i/x.xml", "<x/>")],
        strict,
    ));
    assert_eq!(code, "XTRE1160");
}

/// Review R4, the default mode: the recovery action — "ignore the fragment
/// identifier and return the document node" (§16.1).
#[test]
fn a_fragment_identifier_in_document_is_ignored_by_default() {
    let out = run_sheet_with(
        &sheet(
            r#"<xsl:template match="/"><out><xsl:copy-of select="document('x.xml#frag')"/></out></xsl:template>"#,
        ),
        "<doc/>",
        &[("file:///m2i/x.xml", "<x/>")],
    )
    .unwrap();
    assert_eq!(out, "<out><x/></out>");
}

/// Review R4: when the recovery action itself cannot be completed — the
/// resource without its fragment identifier cannot be retrieved, as with
/// `error-1160a`'s `http:` URI here — two errors have arisen, and §2.9: "If
/// more than one error arises, an implementation is not required to signal any
/// errors other than the first one that it detects. It is
/// implementation-dependent which of the several errors is signaled." The
/// first one detected is XTRE1160.
#[test]
fn an_unrecoverable_fragment_identifier_is_still_xtre1160_by_default() {
    let code = code_of(run_sheet_with(
        &sheet(
            r#"<xsl:template match="/"><out><xsl:copy-of select="document('no-such.xml#frag')"/></out></xsl:template>"#,
        ),
        "<doc/>",
        &[],
    ));
    assert_eq!(code, "XTRE1160");
}

/// Review R3. F&O `fn:doc-available` returns true if and only if a call on
/// `fn:doc($uri)` would return a document node (F&O §15.5.5, paraphrased — F&O
/// is not local). `doc()` maps no URI with a fragment identifier (FODC0005),
/// so `doc-available()` must say false for it, even when the resolver can
/// serve the URI. (`FileResolver` maps `x.xml#f` to the file `x.xml`; the
/// in-memory entry for `x.xml#f` stands in for that here.)
#[test]
fn doc_available_agrees_with_doc_on_a_fragment_identifier() {
    let run_one = |expr: &str| {
        run_sheet_with(
            &sheet(&format!(
                r#"<xsl:template match="/"><out><xsl:value-of select="{expr}"/></out></xsl:template>"#
            )),
            "<doc/>",
            &[
                ("file:///m2i/x.xml", "<x>X</x>"),
                ("file:///m2i/x.xml#f", "<x>X</x>"),
            ],
        )
    };
    assert_eq!(
        run_one("doc-available('x.xml#f')").unwrap(),
        "<out>false</out>"
    );
    assert_eq!(code_of(run_one("doc('x.xml#f')")), "FODC0005");
    // Without the fragment both agree the other way.
    assert_eq!(
        run_one("doc-available('x.xml')").unwrap(),
        "<out>true</out>"
    );
    assert_eq!(run_one("doc('x.xml')").unwrap(), "<out>X</out>");
}

// ═══ carried — a pattern's XPath name errors keep their own codes ════════

/// §5.5.2: "[ERR XTSE0340] … it is a static error if the pattern does not
/// match the production Pattern" — a *syntax* error. §5.3: an expression that
/// "fails to satisfy other static constraints defined in the XPath
/// specification, for example that all variable references must refer to
/// variables that are in scope" is a static error whose "Error codes are
/// defined in [XPath 2.0]"; every pattern is an expression.
#[test]
fn a_name_error_in_a_pattern_keeps_its_xpath_code() {
    for (pattern, expected) in [
        ("a[$no-such-variable]", "XPST0008"),
        ("a[no-such-function()]", "XPST0017"),
        ("nope:a", "XPST0081"),
        ("element(a, nope:t)", "XPST0081"),
        // Not a Pattern at all: still XTSE0340.
        ("2 + 2", "XTSE0340"),
        ("a[", "XTSE0340"),
    ] {
        let code = code_of(run(
            &format!(
                r#"<xsl:template match="/"><out/></xsl:template><xsl:template match="{pattern}">X</xsl:template>"#
            ),
            "<doc/>",
        ));
        assert_eq!(code, expected, "{pattern}");
    }
}

/// Review R5. §5.5.2: "\[ERR XTSE0340\] Where an attribute is defined to
/// contain a pattern, it is a static error if the pattern does not match the
/// production Pattern." A string that is not a Pattern at all is XTSE0340
/// even when, read as an expression, it also has an unresolved name; the
/// XPath name codes are kept only for strings that *are* patterns.
#[test]
fn a_non_pattern_with_an_unresolved_name_is_still_xtse0340() {
    for (pattern, expected) in [
        ("$x", "XTSE0340"),
        ("foo()", "XTSE0340"),
        ("a[1] | 1 + $x", "XTSE0340"),
        ("a | $x", "XTSE0340"),
        ("nope:f()", "XTSE0340"),
        // Patterns, so the XPath code stands.
        ("a[1] | b[$x]", "XPST0008"),
        ("a/b[foo()]", "XPST0017"),
        ("a | nope:b", "XPST0081"),
    ] {
        let code = code_of(run(
            &format!(
                r#"<xsl:template match="/"><out/></xsl:template><xsl:template match="{pattern}">X</xsl:template>"#
            ),
            "<doc/>",
        ));
        assert_eq!(code, expected, "{pattern}");
    }
}

/// §3.8: with backwards-compatible behaviour "any function call for which no
/// implementation is available (unless it uses the standard function
/// namespace) is bound to a fallback error function whose effect when
/// evaluated is to raise a dynamic error \[see ERR XTDE1425\]". In a pattern
/// that dynamic error is recoverable — §5.5.4: "The optional recovery action
/// is to treat the pattern as not matching that node" — so the rule compiles
/// and never fires; the built-in rule copies the text instead. (Review R2:
/// the name must be an *extension* name — an unprefixed name is in the
/// standard function namespace, which §3.8 excludes.)
#[test]
fn an_unknown_function_in_a_backwards_compatible_pattern_is_not_a_static_error() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="1.0" xmlns:xsl="{NS}" xmlns:ext="urn:ext" exclude-result-prefixes="ext">
                 <xsl:template match="/"><out><xsl:apply-templates select="//a"/></out></xsl:template>
                 <xsl:template match="a[ext:no-such-function()]">X</xsl:template>
               </xsl:stylesheet>"#
        ),
        "<r><a>1</a></r>",
    )
    .unwrap();
    assert_eq!(out, "<out>1</out>");
}

/// Review R2, the negative cases. §3.8 binds to the fallback only names
/// outside the standard function namespace, and only with backwards-compatible
/// behaviour; the §3.9 Note: "In XSLT 2.0 (without backwards compatibility
/// enabled) a static error occurs when an XPath expression references a
/// function that is not available. This is true even in a part of the
/// stylesheet that uses forwards-compatible behavior." So all of these are
/// XPath's static XPST0017 — in a pattern and in an expression alike, even in
/// a rule or template that is never used.
#[test]
fn an_unknown_function_is_xpst0017_outside_the_backwards_compatible_extension_case() {
    for (version, pattern) in [
        // Unprefixed = the standard function namespace.
        ("1.0", "a[no-such-function()]"),
        ("1.0", "a[fn:no-such-function()]"),
        // A known standard function with the wrong arity.
        ("1.0", "a[contains(.)]"),
        // Forwards-compatible behaviour gets no tolerance.
        ("3.0", "a[ext:no-such-function()]"),
        // Neither does plain XSLT 2.0.
        ("2.0", "a[ext:no-such-function()]"),
    ] {
        let code = code_of(run_sheet(
            &format!(
                r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}" xmlns:ext="urn:ext"
                     xmlns:fn="http://www.w3.org/2005/xpath-functions">
                     <xsl:template match="/"><out/></xsl:template>
                     <xsl:template match="{pattern}">X</xsl:template>
                   </xsl:stylesheet>"#
            ),
            "<r><a>1</a></r>",
        ));
        assert_eq!(code, "XPST0017", "version {version}, pattern {pattern}");
    }
    for (version, expr) in [
        ("1.0", "no-such-function()"),
        ("1.0", "contains(.)"),
        ("3.0", "ext:no-such-function()"),
    ] {
        let code = code_of(run_sheet(
            &format!(
                r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}" xmlns:ext="urn:ext" exclude-result-prefixes="ext">
                     <xsl:template match="/"><out/></xsl:template>
                     <xsl:template name="never"><xsl:value-of select="{expr}"/></xsl:template>
                   </xsl:stylesheet>"#
            ),
            "<r/>",
        ));
        assert_eq!(code, "XPST0017", "version {version}, expression {expr}");
    }
}

/// Review R2, the root cause: §3.8 binds *the call* to the fallback error
/// function, not the whole expression, and XPath 2.0 §3.6: "If XPath 1.0
/// compatibility mode is true, the order in which the operands of a logical
/// expression are evaluated is effectively prescribed. Specifically, it is
/// defined that when there is no need to evaluate the second operand in order
/// to determine the result, then no error can occur as a result of evaluating
/// the second operand." So a call guarded by `function-available()` (§18.1.2)
/// is never evaluated and the rest of the expression works — in a pattern and
/// in an expression.
#[test]
fn a_guarded_extension_call_in_backwards_compatible_mode_is_not_evaluated() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="1.0" xmlns:xsl="{NS}" xmlns:ext="urn:ext" exclude-result-prefixes="ext">
                 <xsl:template match="/">
                   <out avt="{{not(function-available('ext:foo')) or ext:foo()}}">
                     <xsl:apply-templates select="//a"/>
                     <xsl:value-of select="concat('[', ext:foo() and false(), ']')"/>
                   </out>
                 </xsl:template>
                 <xsl:template match="a[not(function-available('ext:foo')) or ext:foo()]">GUARDED-MATCH</xsl:template>
               </xsl:stylesheet>"#
        ),
        "<r><a>1</a></r>",
    );
    // The `and` evaluates its first operand, `ext:foo()`, so that one call *is*
    // evaluated: XTDE1425, the fallback error function's dynamic error.
    assert_eq!(code_of(out), "XTDE1425");
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="1.0" xmlns:xsl="{NS}" xmlns:ext="urn:ext" exclude-result-prefixes="ext">
                 <xsl:template match="/">
                   <out avt="{{not(function-available('ext:foo')) or ext:foo()}}">
                     <xsl:apply-templates select="//a"/>
                     <xsl:value-of select="concat('[', false() and ext:foo(), ']')"/>
                   </out>
                 </xsl:template>
                 <xsl:template match="a[not(function-available('ext:foo')) or ext:foo()]">GUARDED-MATCH</xsl:template>
               </xsl:stylesheet>"#
        ),
        "<r><a>1</a></r>",
    )
    .unwrap();
    assert_eq!(out, r#"<out avt="true">GUARDED-MATCH[false]</out>"#);
}

// ═══ carried — FODC0002 in use-when is the processor's error ══════════════

/// `use-when-0128`. §3.12 gives a `use-when` expression "Available documents:
/// None", so `fn:doc` fails with F&O's FODC0002 — an error the **processor**
/// raises, not one the stylesheet raised through `fn:error`.
#[test]
fn fodc0002_in_use_when_has_processor_origin() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let error = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(r#"<xsl:template match="/" use-when="doc('x.xml')"><out/></xsl:template>"#),
            "file:///m2i/main.xsl",
        )
        .map(|_| ())
        .expect_err("doc() in use-when cannot succeed");
    assert_eq!(error.code, "FODC0002", "{error}");
    assert_eq!(error.origin(), xslt2::ErrorOrigin::Processor, "{error}");
    assert!(!error.is_raised(), "{error}");
}

// ═══ carried — a constructed xml:id is normalized ═════════════════════════

/// §11.1.2 / §11.3: "If the name of a constructed attribute is xml:id, the
/// processor must perform attribute value normalization by effectively
/// applying the normalize-space function to the value of the attribute, and
/// the resulting attribute node must be given the is-id property. This applies
/// whether the attribute is constructed using the xsl:attribute instruction or
/// whether it is constructed using an attribute of a literal result element."
#[test]
fn a_constructed_xml_id_is_normalized() {
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="t">
               <r><a xml:id="  lre&#9;one  "/><b><xsl:attribute name="xml:id">&#10; ins&#13;</xsl:attribute></b></r>
             </xsl:variable>
             <out><xsl:value-of select="string-join(($t//@xml:id), '|'), name(id('ins', $t))" separator=","/></out>
           </xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    // Internal runs collapse to one space, the ends are trimmed, and the
    // normalized value is what `id()` finds the element by.
    assert_eq!(out, "<out>lre one|ins,b</out>");
}

/// Only `xml:id`: any other attribute keeps its whitespace.
#[test]
fn other_constructed_attributes_are_not_normalized() {
    let out = run(
        r#"<xsl:template match="/"><out id="  a  b " xml:lang=" en "/></xsl:template>"#,
        "<doc/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out id="  a  b " xml:lang=" en "/>"#);
}
