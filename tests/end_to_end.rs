//! Whole stylesheets, end to end, with the exact serialized output asserted: the
//! identity transform, a report, parameters and tunnel parameters, the module
//! graph, a simplified stylesheet and backwards-compatible behaviour — kept
//! separate from the conformance harness so that a regression here is readable
//! without the suite.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xslt2::{
    CompiledStylesheet, Compiler, MemoryResolver, QName, TransformOptions, XsltError, XsltFunctions,
};

/// Compiles `sheet`, runs it over `source` and returns the serialized result.
fn transform(sheet: &str, source: &str) -> Result<String, XsltError> {
    transform_with(sheet, source, MemoryResolver::new(), |run| run)
}

fn transform_with<F>(
    sheet: &str,
    source: &str,
    resolver: MemoryResolver,
    setup: F,
) -> Result<String, XsltError>
where
    F: for<'a, 't> FnOnce(xslt2::Transformation<'a, 't>) -> xslt2::Transformation<'a, 't>,
{
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let resolver = Arc::new(resolver);
    let compiler = Compiler::new(&names, &functions).with_resolver(resolver.clone());
    let compiled = compiler.compile_str(sheet, "urn:test:sheet.xsl")?;
    run_and_serialize(&compiled, source, resolver, setup)
}

fn run_and_serialize<F>(
    compiled: &CompiledStylesheet<'_>,
    source: &str,
    resolver: Arc<MemoryResolver>,
    setup: F,
) -> Result<String, XsltError>
where
    F: for<'a, 't> FnOnce(xslt2::Transformation<'a, 't>) -> xslt2::Transformation<'a, 't>,
{
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let run = setup(run);
    let outcome = run.transform_source("urn:test:in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// Wraps a body in an `xsl:stylesheet` element.
///
/// §5.4.1: "The statically known namespaces are the namespace declarations
/// that are in scope for the containing element" — nothing is predefined, so
/// a stylesheet that writes `xs:integer` or `p:name` must declare the prefix,
/// exactly as these do.
fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:p="urn:p" exclude-result-prefixes="xs p">{body}</xsl:stylesheet>"#
    )
}

#[test]
fn the_identity_transform_copies_a_document_unchanged() {
    // The spec's own example (§11.9.1 "Example: Identity Transformation").
    let s = sheet(
        r#"<xsl:template match="@*|node()">
             <xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy>
           </xsl:template>"#,
    );
    let source = r#"<doc n="1"><a>text</a><!--c--><?pi data?><b x="y"/></doc>"#;
    assert_eq!(transform(&s, source).unwrap(), source);
}

#[test]
fn the_identity_transform_can_be_overridden_for_one_element() {
    let s = sheet(
        r#"<xsl:template match="@*|node()">
             <xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy>
           </xsl:template>
           <xsl:template match="skip"/>"#,
    );
    assert_eq!(
        transform(&s, "<doc><a/><skip><deep/></skip><b/></doc>").unwrap(),
        "<doc><a/><b/></doc>"
    );
}

#[test]
fn a_report_uses_for_each_if_choose_value_of_and_attribute_value_templates() {
    let s = sheet(
        r#"<xsl:template match="/">
             <report count="{count(//item)}">
               <xsl:for-each select="//item">
                 <row n="{position()}" id="{@id}">
                   <xsl:choose>
                     <xsl:when test="@id = 'b'">chosen</xsl:when>
                     <xsl:otherwise><xsl:value-of select="."/></xsl:otherwise>
                   </xsl:choose>
                   <xsl:if test="position() = last()"><last/></xsl:if>
                 </row>
               </xsl:for-each>
             </report>
           </xsl:template>"#,
    );
    let source = r#"<doc><item id="a">A</item><item id="b">B</item><item id="c">C</item></doc>"#;
    assert_eq!(
        transform(&s, source).unwrap(),
        r#"<report count="3"><row n="1" id="a">A</row><row n="2" id="b">chosen</row>"#.to_owned()
            + r#"<row n="3" id="c">C<last/></row></report>"#
    );
}

#[test]
fn call_template_passes_ordinary_and_tunnel_parameters() {
    // §10.1.2: "A tunnel parameter is passed transparently through all the
    // template rules and named templates that are invoked."
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:call-template name="outer">
               <xsl:with-param name="plain" select="'p'"/>
               <xsl:with-param name="secret" select="'s'" tunnel="yes"/>
             </xsl:call-template>
           </xsl:template>
           <xsl:template name="outer">
             <xsl:param name="plain"/>
             <out plain="{$plain}">
               <xsl:call-template name="inner"/>
             </out>
           </xsl:template>
           <xsl:template name="inner">
             <xsl:param name="secret" tunnel="yes"/>
             <xsl:param name="missing" select="'default'"/>
             <in secret="{$secret}" missing="{$missing}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc/>").unwrap(),
        r#"<out plain="p"><in secret="s" missing="default"/></out>"#
    );
}

#[test]
fn a_missing_required_parameter_names_the_code_of_the_calling_construct() {
    // §10.1.1: xsl:call-template is a static error, xsl:apply-templates a
    // dynamic one.
    let called = sheet(
        r#"<xsl:template match="/"><xsl:call-template name="t"/></xsl:template>
           <xsl:template name="t"><xsl:param name="p" required="yes"/></xsl:template>"#,
    );
    assert_eq!(transform(&called, "<doc/>").unwrap_err().code, "XTSE0690");

    let applied = sheet(
        r#"<xsl:template match="/"><xsl:apply-templates select="doc"/></xsl:template>
           <xsl:template match="doc"><xsl:param name="p" required="yes"/></xsl:template>"#,
    );
    assert_eq!(transform(&applied, "<doc/>").unwrap_err().code, "XTDE0700");
}

#[test]
fn a_with_param_the_callee_does_not_declare_is_xtse0680() {
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:call-template name="t"><xsl:with-param name="q" select="1"/></xsl:call-template>
           </xsl:template>
           <xsl:template name="t"><xsl:param name="p" select="0"/></xsl:template>"#,
    );
    let e = transform(&s, "<doc/>").unwrap_err();
    assert_eq!(e.code, "XTSE0680");
}

#[test]
fn import_precedence_and_include_build_one_declaration_set() {
    // `main` imports `low` (precedence 0) and includes `mid`, whose
    // declarations get `main`'s own precedence. The imported rule loses.
    let mut resolver = MemoryResolver::new();
    resolver.insert(
        "urn:test:low.xsl",
        sheet(
            r#"<xsl:template match="a"><from-low/></xsl:template>
               <xsl:template match="b"><b-from-low/></xsl:template>"#,
        ),
    );
    resolver.insert(
        "urn:test:mid.xsl",
        sheet(r#"<xsl:template match="a"><from-mid/></xsl:template>"#),
    );
    let main = sheet(
        r#"<xsl:import href="urn:test:low.xsl"/>
           <xsl:include href="urn:test:mid.xsl"/>
           <xsl:template match="/"><out><xsl:apply-templates select="doc/*"/></out></xsl:template>"#,
    );
    assert_eq!(
        transform_with(&main, "<doc><a/><b/></doc>", resolver, |r| r).unwrap(),
        "<out><from-mid/><b-from-low/></out>"
    );
}

#[test]
fn an_import_after_another_declaration_is_xtse0200() {
    let s = sheet(r#"<xsl:template match="/"/><xsl:import href="urn:test:none.xsl"/>"#);
    let e = transform(&s, "<doc/>").unwrap_err();
    assert_eq!(e.code, "XTSE0200");
}

#[test]
fn an_include_cycle_is_xtse0180_and_does_not_hang() {
    // The indirect case: b includes c, c includes b.
    let mut resolver = MemoryResolver::new();
    resolver.insert(
        "urn:test:b.xsl",
        sheet(r#"<xsl:include href="urn:test:c.xsl"/>"#),
    );
    resolver.insert(
        "urn:test:c.xsl",
        sheet(r#"<xsl:include href="urn:test:b.xsl"/>"#),
    );
    let main = sheet(r#"<xsl:include href="urn:test:b.xsl"/>"#);
    let e = transform_with(&main, "<doc/>", resolver, |r| r).unwrap_err();
    assert_eq!(e.code, "XTSE0180");
}

#[test]
fn a_simplified_stylesheet_is_one_template_matching_the_root() {
    // §3.7: a literal result element with an xsl:version attribute.
    let s = r#"<out xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xsl:version="2.0"
                    n="{/doc/@n}"><xsl:value-of select="/doc"/></out>"#;
    assert_eq!(
        transform(s, r#"<doc n="3">body</doc>"#).unwrap(),
        r#"<out n="3">body</out>"#
    );
}

#[test]
fn a_version_1_0_region_takes_the_first_item_of_a_value_of() {
    // §9.10 row 5 / J.1.3: with backwards-compatible behaviour and no
    // separator, xsl:value-of discards every item after the first.
    let bc = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                  <xsl:template match="/"><out><xsl:value-of select="//n"/></out></xsl:template>
                </xsl:stylesheet>"#;
    let source = "<doc><n>1</n><n>2</n><n>3</n></doc>";
    assert_eq!(transform(bc, source).unwrap(), "<out>1</out>");

    // The same stylesheet at version 2.0 joins them with a single space.
    let two =
        sheet(r#"<xsl:template match="/"><out><xsl:value-of select="//n"/></out></xsl:template>"#);
    assert_eq!(transform(&two, source).unwrap(), "<out>1 2 3</out>");
}

#[test]
fn a_nested_version_2_0_element_turns_backwards_compatibility_off_again() {
    // §3.8: "the nearest [xsl:]version attribute wins".
    let s = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:template match="/">
                   <out>
                     <a><xsl:value-of select="//n"/></a>
                     <b xsl:version="2.0"><xsl:value-of select="//n"/></b>
                   </out>
                 </xsl:template>
               </xsl:stylesheet>"#;
    assert_eq!(
        transform(s, "<doc><n>1</n><n>2</n></doc>").unwrap(),
        "<out><a>1</a><b>1 2</b></out>"
    );
}

#[test]
fn built_in_rules_walk_the_tree_and_copy_text() {
    // §6.6: with no template rules at all, the built-in rules apply templates
    // to children and copy the string value of text nodes.
    let s = sheet("");
    assert_eq!(transform(&s, "<doc>one<a>two</a></doc>").unwrap(), "onetwo");
}

#[test]
fn built_in_rules_pass_parameters_through() {
    // §6.6: "the built-in template rule for element and document nodes …
    // parameters are passed through".
    let s = sheet(
        r#"<xsl:template match="/">
             <out><xsl:apply-templates select="doc">
               <xsl:with-param name="p" select="'through'" tunnel="yes"/>
             </xsl:apply-templates></out>
           </xsl:template>
           <xsl:template match="leaf">
             <xsl:param name="p" tunnel="yes"/>
             <leaf p="{$p}"/>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc><mid><leaf/></mid></doc>").unwrap(),
        r#"<out><leaf p="through"/></out>"#
    );
}

#[test]
fn conflict_resolution_prefers_the_higher_priority_then_the_later_rule() {
    // §6.4: highest import precedence, then highest priority, then the rule
    // that occurs last in declaration order.
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/*"/></out></xsl:template>
           <xsl:template match="element()"><any/></xsl:template>
           <xsl:template match="a"><named/></xsl:template>
           <xsl:template match="*" priority="5"><high/></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc><a/></doc>").unwrap(),
        "<out><high/></out>"
    );

    // Without the priority, `a` (priority 0) beats both wildcards (-0.5).
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/*"/></out></xsl:template>
           <xsl:template match="element()"><any/></xsl:template>
           <xsl:template match="a"><named/></xsl:template>
           <xsl:template match="*"><wild/></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc><a/></doc>").unwrap(),
        "<out><named/></out>"
    );

    // Equal precedence and priority: the rule declared last wins (XTRE0540's
    // recovery action).
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/*"/></out></xsl:template>
           <xsl:template match="a"><first/></xsl:template>
           <xsl:template match="a"><last/></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc><a/></doc>").unwrap(),
        "<out><last/></out>"
    );
}

#[test]
fn a_global_variable_is_evaluated_once_and_sees_the_source_root() {
    let s = sheet(
        r#"<xsl:variable name="all" select="count(//item)"/>
           <xsl:variable name="label" select="concat('n=', $all)"/>
           <xsl:template match="/"><out label="{$label}"/></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc><item/><item/></doc>").unwrap(),
        r#"<out label="n=2"/>"#
    );
}

#[test]
fn a_circular_global_variable_is_xtde0640() {
    let s = sheet(
        r#"<xsl:variable name="a" select="$b"/>
           <xsl:variable name="b" select="$a"/>
           <xsl:template match="/"><out><xsl:value-of select="$a"/></out></xsl:template>"#,
    );
    let e = transform(&s, "<doc/>").unwrap_err();
    assert_eq!(e.code, "XTDE0640");
}

#[test]
fn a_stylesheet_parameter_can_be_supplied_by_the_caller() {
    let s = sheet(
        r#"<xsl:param name="who" select="'nobody'"/>
           <xsl:template match="/"><out><xsl:value-of select="$who"/></out></xsl:template>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&s, "urn:test:sheet.xsl")
        .unwrap();
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_param(
            QName::local("who"),
            xsd_schema::xpath::functions::XPathValue::string("caller"),
        );
    let outcome = run.transform_source("urn:test:in.xml", "<doc/>").unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out>caller</out>"
    );
}

#[test]
fn a_local_variable_with_content_builds_a_temporary_tree() {
    // §9.4: a variable with a sequence constructor and no `as` is a document
    // node, whose string value is the concatenation of its text.
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="t"><a>x</a><b>y</b></xsl:variable>
             <out count="{count($t/*)}"><xsl:value-of select="$t"/></out>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc/>").unwrap(),
        r#"<out count="2">xy</out>"#
    );
}

#[test]
fn attribute_sets_are_applied_in_order_and_cycles_are_xtse0720() {
    let s = sheet(
        r#"<xsl:attribute-set name="base"><xsl:attribute name="a">1</xsl:attribute></xsl:attribute-set>
           <xsl:attribute-set name="more" use-attribute-sets="base">
             <xsl:attribute name="b">2</xsl:attribute>
           </xsl:attribute-set>
           <xsl:template match="/"><out xsl:use-attribute-sets="more"/></xsl:template>"#,
    );
    assert_eq!(transform(&s, "<doc/>").unwrap(), r#"<out a="1" b="2"/>"#);

    let cyclic = sheet(
        r#"<xsl:attribute-set name="x" use-attribute-sets="y"/>
           <xsl:attribute-set name="y" use-attribute-sets="x"/>
           <xsl:template match="/"><out xsl:use-attribute-sets="x"/></xsl:template>"#,
    );
    assert_eq!(transform(&cyclic, "<doc/>").unwrap_err().code, "XTSE0720");
}

#[test]
fn simple_content_separates_atomic_values_but_merges_text_nodes() {
    // The spec's own example (§5.7.2 "Example: Space Separators in Attribute
    // Content").
    let s = sheet(
        r#"<xsl:template match="/"><doc>
             <xsl:attribute name="e" select="1 to 5"/>
             <xsl:attribute name="f">
               <xsl:for-each select="1 to 5"><xsl:value-of select="."/></xsl:for-each>
             </xsl:attribute>
           </doc></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<in/>").unwrap(),
        r#"<doc e="1 2 3 4 5" f="12345"/>"#
    );
}

#[test]
fn constructed_elements_attributes_comments_and_pis_reach_the_result() {
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:element name="out">
               <xsl:attribute name="a">{{ not an avt }}</xsl:attribute>
               <xsl:comment>c</xsl:comment>
               <xsl:processing-instruction name="pi">data</xsl:processing-instruction>
               <xsl:text>  kept  </xsl:text>
             </xsl:element>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<in/>").unwrap(),
        r#"<out a="{{ not an avt }}"><!--c--><?pi data?>  kept  </out>"#
    );
}

#[test]
fn an_xsl_message_is_collected_and_terminate_yes_ends_the_run() {
    let s =
        sheet(r#"<xsl:template match="/"><xsl:message>hello</xsl:message><out/></xsl:template>"#);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&s, "urn:test:sheet.xsl")
        .unwrap();
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run.transform_source("urn:test:in.xml", "<doc/>").unwrap();
    assert_eq!(outcome.messages.len(), 1);
    assert_eq!(outcome.messages[0].content, "hello");
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out/>"
    );

    let terminating = sheet(
        r#"<xsl:template match="/"><xsl:message terminate="yes">stop</xsl:message></xsl:template>"#,
    );
    assert_eq!(
        transform(&terminating, "<doc/>").unwrap_err().code,
        "XTMM9000"
    );
}

#[test]
fn a_missing_version_attribute_is_xtse0010() {
    let s = r#"<xsl:stylesheet xmlns:xsl="http://www.w3.org/1999/XSL/Transform"/>"#;
    assert_eq!(transform(s, "<doc/>").unwrap_err().code, "XTSE0010");
}

#[test]
fn a_document_element_that_is_not_a_stylesheet_is_xtse0150() {
    // §3.7: a document element that is not xsl:stylesheet/xsl:transform is read
    // as the literal result element of a simplified stylesheet module, and
    // "[ERR XTSE0150] A literal result element that is used as the outermost
    // element of a simplified stylesheet module must have an xsl:version
    // attribute." Not the generic XTSE0010.
    assert_eq!(
        transform("<not-a-stylesheet/>", "<doc/>").unwrap_err().code,
        "XTSE0150"
    );
}

/// The construct is a UCA collation URI in a build without the `icu-collation` feature
/// (`tests/collations_without_icu.rs`), the only `XSLT2-NOTIMPL` this processor raises. With the
/// feature, nothing this build reaches is unimplemented.
#[cfg(not(feature = "icu-collation"))]
#[test]
fn a_not_implemented_construct_carries_the_internal_code_not_a_panic() {
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:for-each select="doc"><xsl:sort select="." collation="http://www.w3.org/2013/collation/UCA"/><xsl:value-of select="."/></xsl:for-each></out></xsl:template>"#,
    );
    let e = transform(&s, "<doc/>").unwrap_err();
    assert!(e.is_not_implemented(), "{e}");
    assert_eq!(e.code, "XSLT2-NOTIMPL");
}

#[test]
fn errors_carry_the_line_and_column_of_the_instruction() {
    let s = "<xsl:stylesheet version=\"2.0\" \
             xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\n\
             <xsl:template match=\"/\">\n\
             <xsl:for-each select=\"doc\"><xsl:number value=\"-1\"/></xsl:for-each>\n\
             </xsl:template>\n\
             </xsl:stylesheet>";
    let e = transform(s, "<doc/>").unwrap_err();
    let location = e.location.expect("a location");
    assert_eq!(location.line, 3);
    assert_eq!(location.module_uri.as_deref(), Some("urn:test:sheet.xsl"));
}

#[test]
fn xsl_output_controls_the_serialization() {
    let s = sheet(
        r#"<xsl:output method="xml" omit-xml-declaration="no" encoding="UTF-8" standalone="yes"/>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    // (`transform` drops only the default declaration, `common::DECLARATION`.)
    let result = transform(&s, "<doc/>").unwrap();
    assert_eq!(
        result,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><out/>"#
    );

    let text = sheet(
        r#"<xsl:output method="text"/>
           <xsl:template match="/"><out>hello</out></xsl:template>"#,
    );
    assert_eq!(transform(&text, "<doc/>").unwrap(), "hello");
}

#[test]
fn document_reads_a_second_source_through_the_resolver() {
    let mut resolver = MemoryResolver::new();
    resolver.insert("urn:test:other.xml", "<other><v>42</v></other>");
    let s = sheet(
        r#"<xsl:template match="/">
             <out><xsl:value-of select="document('urn:test:other.xml')/other/v"/></out>
           </xsl:template>"#,
    );
    assert_eq!(
        transform_with(&s, "<doc/>", resolver, |r| r).unwrap(),
        "<out>42</out>"
    );
}

#[test]
fn system_property_and_element_available_answer_per_16_6_5_and_18_1_1() {
    let s = sheet(
        r#"<xsl:template match="/"><out
             v="{system-property('xsl:version')}"
             sa="{system-property('xsl:is-schema-aware')}"
             p="{system-property('xsl:product-name')}"
             i="{element-available('xsl:if')}"
             n="{element-available('xsl:no-such-thing')}"
             f="{function-available('concat')}"/></xsl:template>"#,
    );
    assert_eq!(
        transform(&s, "<doc/>").unwrap(),
        r#"<out v="2.0" sa="no" p="xslt2" i="true" n="false" f="true"/>"#
    );
}

#[test]
fn copy_of_keeps_namespaces_by_default_and_drops_them_on_request() {
    // §11.9.2: copy-namespaces="yes" is the default.
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:copy-of select="//p:keep"/></out></xsl:template>"#,
    );
    let source = r#"<doc xmlns:p="urn:p" xmlns:unused="urn:u"><p:keep/></doc>"#;
    assert_eq!(
        transform(&s, source).unwrap(),
        r#"<out><p:keep xmlns:p="urn:p" xmlns:unused="urn:u"/></out>"#
    );

    let no = sheet(
        r#"<xsl:template match="/">
             <out><xsl:copy-of select="//p:keep" copy-namespaces="no"/></out>
           </xsl:template>"#,
    );
    assert_eq!(
        transform(&no, source).unwrap(),
        r#"<out><p:keep xmlns:p="urn:p"/></out>"#
    );
}

#[test]
fn exclude_result_prefixes_keeps_a_prefix_out_of_the_result() {
    // §11.1.2.
    let s = r#"<xsl:stylesheet version="2.0"
                  xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                  xmlns:x="urn:x" exclude-result-prefixes="x">
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#;
    assert_eq!(transform(s, "<doc/>").unwrap(), "<out/>");

    let kept = r#"<xsl:stylesheet version="2.0"
                    xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                    xmlns:x="urn:x">
                   <xsl:template match="/"><out/></xsl:template>
                 </xsl:stylesheet>"#;
    assert_eq!(
        transform(kept, "<doc/>").unwrap(),
        r#"<out xmlns:x="urn:x"/>"#
    );
}

#[test]
fn patterns_with_predicates_apply_them_successively() {
    // §5.5.3: `item[@enabled='yes'][1]` is the first *enabled* item, not "the
    // first item, if it is enabled".
    let s = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="doc/item"/></out></xsl:template>
           <xsl:template match="item[@on='yes'][1]"><first-on id="{@id}"/></xsl:template>
           <xsl:template match="item"/>"#,
    );
    let source =
        r#"<doc><item id="a" on="no"/><item id="b" on="yes"/><item id="c" on="yes"/></doc>"#;
    assert_eq!(
        transform(&s, source).unwrap(),
        r#"<out><first-on id="b"/></out>"#
    );
}

#[test]
fn a_named_template_can_be_the_entry_point() {
    let s = sheet(r#"<xsl:template name="main"><out/></xsl:template>"#);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&s, "urn:test:sheet.xsl")
        .unwrap();
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_initial_template(QName::local("main"));
    let outcome = run.transform_source("urn:test:in.xml", "<doc/>").unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out/>"
    );

    // §6: a name that is not declared is XTDE0040.
    let arena2 = Bump::new();
    let run = compiled
        .transformation(&arena2, TransformOptions::default())
        .with_initial_template(QName::local("nope"));
    let error = run
        .transform_source("urn:test:in.xml", "<doc/>")
        .expect_err("an undeclared initial template is an error");
    assert_eq!(error.code, "XTDE0040");
}

#[test]
fn a_typed_variable_applies_the_function_conversion_rules() {
    let s = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="n" as="xs:integer" select="/doc/@n"/>
             <out v="{$n + 1}"/>
           </xsl:template>"#,
    );
    // The attribute is untypedAtomic; the conversion rules cast it.
    assert_eq!(
        transform(&s, r#"<doc n="41"/>"#).unwrap(),
        r#"<out v="42"/>"#
    );

    let bad = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="n" as="xs:integer" select="'x'"/>
             <out v="{$n}"/>
           </xsl:template>"#,
    );
    assert_eq!(transform(&bad, "<doc/>").unwrap_err().code, "XTTE0570");
}

#[test]
fn a_deep_recursion_survives_on_the_large_stack() {
    // No tail-call elimination, so the depth that works is a
    // property of the stack the run gets.
    let depth = 1_000;
    let result = xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || {
        let s = sheet(
            r#"<xsl:template match="/">
                 <out><xsl:call-template name="down">
                   <xsl:with-param name="n" select="$LIMIT"/>
                 </xsl:call-template></out>
               </xsl:template>
               <xsl:template name="down">
                 <xsl:param name="n"/>
                 <xsl:if test="$n > 0">
                   <xsl:call-template name="down">
                     <xsl:with-param name="n" select="$n - 1"/>
                   </xsl:call-template>
                 </xsl:if>
                 <xsl:if test="$n = 0"><bottom/></xsl:if>
               </xsl:template>"#,
        )
        .replace("$LIMIT", &depth.to_string());
        transform(&s, "<doc/>")
    });
    assert_eq!(result.unwrap(), "<out><bottom/></out>");
}
