//! The stylesheet-level static errors, and the legal forms next door.
//!
//! Every check is pinned here twice: once with the stylesheet that must be
//! rejected, and once with the **neighbouring legal form** that must still
//! compile. That second half is the point of the file: the way a table-driven
//! pass goes wrong is by rejecting something the spec allows — a construct
//! relaxed by §3.9 forwards-compatible processing, a child excluded by §3.12
//! `use-when`, an extension attribute in another namespace, an `xsl:fallback`
//! body, or whitespace and comments inside an element "required to be empty".
//!
//! Spec citations are to XSLT 2.0 (Second Edition); the element table in
//! `compile::checks` is transcribed from the element syntax summaries that
//! Appendix D collects.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, ModeKey, QName, Resolver, TransformOptions, XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///static/main.xsl";
const SOURCE_URI: &str = "file:///static/in.xml";
const SOURCE: &str = "<doc><item/></doc>";

/// An includable module served beside the main one. Without it the loader fails
/// with XTSE0165 ("the resource that is retrieved does not contain a stylesheet
/// module") before any check on the `xsl:include` element itself is reached.
fn other_module() -> String {
    format!("<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\"/>")
}

/// The resolver every helper uses: the source document plus `other.xsl`.
fn resolver() -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(SOURCE_URI, SOURCE);
    mem.insert("file:///static/other.xsl", other_module());
    Arc::new(mem)
}

/// Wraps `body` in an `xsl:stylesheet` with the given `[xsl:]version`.
fn sheet_version(version: &str, body: &str) -> String {
    format!("<xsl:stylesheet version=\"{version}\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>")
}

fn sheet(body: &str) -> String {
    sheet_version("2.0", body)
}

/// Compiles one stylesheet module and reports only whether it compiled.
fn compile(module: &str) -> Result<(), XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    Compiler::new(&names, &functions)
        .with_resolver(resolver())
        .compile_str(module, MAIN)
        .map(|_| ())
}

/// Compiles and runs one stylesheet, returning the serialized primary result.
fn run(module: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let resolver = resolver();
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(module, MAIN)?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source(SOURCE_URI, SOURCE)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// Asserts that `module` is rejected with `code`, at compile time.
#[track_caller]
fn rejected(code: &str, module: &str) {
    match compile(module) {
        Ok(()) => panic!("expected {code}, but the stylesheet compiled:\n{module}"),
        Err(e) => assert_eq!(e.code, code, "wrong code for:\n{module}\n({e})"),
    }
}

/// Asserts that `module` compiles. Used for every negative control.
#[track_caller]
fn accepted(module: &str) {
    if let Err(e) = compile(module) {
        panic!("this stylesheet must compile, but {e}:\n{module}");
    }
}

/// Asserts that `module` compiles **or** fails only because a feature is
/// missing — never with a static error.
///
/// An element the table describes but the compiler does not implement must
/// report `XSLT2-NOTIMPL`, so that a missing feature is never mistaken for a
/// wrong answer.
#[track_caller]
fn accepted_or_not_implemented(module: &str) {
    if let Err(e) = compile(module) {
        assert!(
            e.is_not_implemented(),
            "this stylesheet must compile or be NOTIMPL, but {e}:\n{module}"
        );
    }
}

// ═══ §3.4 content models — XTSE0010 ══════════════════════════════════════
//
// > [ERR XTSE0010] It is a static error if an XSLT-defined element is used in a
// > context where it is not permitted, if a required attribute is omitted, or if
// > the content of the element does not correspond to the content that is
// > allowed for the element.

#[test]
fn apply_imports_takes_only_with_param() {
    // Appendix D: `xsl:apply-imports` content is `xsl:with-param*`.
    for child in [
        r#"<xsl:sort select="."/>"#,
        r#"<xsl:fallback/>"#,
        r#"<xsl:variable name="x" select="3"/>"#,
        r#"<out/>"#,
        r#"text"#,
    ] {
        rejected(
            codes::XTSE0010,
            &sheet(&format!(
                r#"<xsl:template match="doc"><xsl:apply-imports>{child}</xsl:apply-imports></xsl:template>"#
            )),
        );
    }
    // The legal neighbour.
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:apply-imports>
             <xsl:with-param name="p" select="1"/>
           </xsl:apply-imports></xsl:template>"#,
    ));
}

#[test]
fn next_match_takes_with_param_and_fallback() {
    // Appendix D: `xsl:next-match` content is `(xsl:with-param | xsl:fallback)*`
    // — the one place `xsl:fallback` is a named child rather than an instruction.
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:next-match>
             <xsl:with-param name="p" select="1"/><xsl:fallback/>
           </xsl:next-match></xsl:template>"#,
    ));
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:next-match>
                 <xsl:sort select="."/>
               </xsl:next-match></xsl:template>"#,
        ),
    );
}

#[test]
fn call_template_takes_only_with_param() {
    for child in [
        r#"<xsl:param name="y" select="3"/>"#,
        r#"<xsl:sort select="@id"/>"#,
        r#"<xsl:value-of select="@id"/>"#,
    ] {
        rejected(
            codes::XTSE0010,
            &sheet(&format!(
                r#"<xsl:template name="x"/>
                   <xsl:template match="doc"><xsl:call-template name="x">{child}</xsl:call-template></xsl:template>"#
            )),
        );
    }
    accepted(&sheet(
        r#"<xsl:template name="x"><xsl:param name="y"/></xsl:template>
           <xsl:template match="doc"><xsl:call-template name="x">
             <xsl:with-param name="y" select="3"/>
           </xsl:call-template></xsl:template>"#,
    ));
}

#[test]
fn apply_templates_takes_sort_and_with_param_in_any_order() {
    // `(xsl:sort | xsl:with-param)*` is a **single** group with two names, so
    // the two may interleave — unlike `(xsl:when+, xsl:otherwise?)`.
    accepted_or_not_implemented(&sheet(
        r#"<xsl:template match="doc"><xsl:apply-templates select="item">
             <xsl:with-param name="p" select="1"/>
             <xsl:sort select="."/>
             <xsl:with-param name="q" select="2"/>
           </xsl:apply-templates></xsl:template>"#,
    ));
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:apply-templates select="item">
                 <xsl:if test="true()"/>
               </xsl:apply-templates></xsl:template>"#,
        ),
    );
    // A misspelt `xsl:width-param` is an unknown XSLT element, not an unknown
    // literal result element, because it is in the XSLT namespace.
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:apply-templates select="item">
                 <xsl:width-param name="y" select="3"/>
               </xsl:apply-templates></xsl:template>"#,
        ),
    );
}

#[test]
fn choose_is_when_plus_then_at_most_one_otherwise() {
    let choose = |content: &str| {
        sheet(&format!(
            r#"<xsl:template match="doc"><xsl:choose>{content}</xsl:choose></xsl:template>"#
        ))
    };
    // §4.2 strips whitespace inside xsl:choose "regardless of any xml:space
    // attributes", so only real text is an error.
    rejected(
        codes::XTSE0010,
        &choose(r#"<xsl:when test="2=2">a</xsl:when><xsl:otherwise/>no text allowed here"#),
    );
    rejected(codes::XTSE0010, &choose(r#"<xsl:otherwise/>"#));
    rejected(
        codes::XTSE0010,
        &choose(r#"<xsl:when test="1"/><xsl:otherwise/><xsl:when test="2"/>"#),
    );
    rejected(
        codes::XTSE0010,
        &choose(r#"<xsl:when test="1"/><xsl:otherwise/><xsl:otherwise/>"#),
    );
    rejected(codes::XTSE0010, &choose(r#"<out/>"#));
    accepted(&choose(
        r#"<xsl:when test="1">a</xsl:when><xsl:when test="2">b</xsl:when><xsl:otherwise>c</xsl:otherwise>"#,
    ));
    accepted(&choose(r#"<xsl:when test="1">a</xsl:when>"#));
}

#[test]
fn whitespace_and_comments_survive_where_elements_may_not() {
    // §4.2: whitespace inside xsl:choose, xsl:apply-templates, xsl:call-template,
    // xsl:attribute-set, xsl:stylesheet … is removed "regardless of any
    // xml:space attributes", so `xml:space="preserve"` must not turn a legal
    // stylesheet into an XTSE0010. And §4.2's first bullet removes every comment
    // and processing instruction before any of this is looked at.
    accepted(&sheet(
        r#"<xsl:template match="doc" xml:space="preserve"><xsl:choose>
             <!-- a comment --><?a pi?>
             <xsl:when test="1">a</xsl:when>
           </xsl:choose></xsl:template>"#,
    ));
    accepted(&sheet(
        r#"<xsl:template match="doc" xml:space="preserve"><xsl:call-template name="x">
             <xsl:with-param name="p" select="1"/>
           </xsl:call-template></xsl:template>
           <xsl:template name="x"><xsl:param name="p"/></xsl:template>"#,
    ));
    // §4.2, last bullet: whitespace before an xsl:param is removed too, so the
    // params of an xml:space="preserve" template are still recognised as params.
    accepted(&sheet(
        r#"<xsl:template match="doc" xml:space="preserve">
             <xsl:param name="p" select="1"/>
             <out/>
           </xsl:template>"#,
    ));
}

#[test]
fn attribute_set_takes_only_xsl_attribute() {
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:attribute-set name="a"><xsl:attribute name="one">1</xsl:attribute>text</xsl:attribute-set>
               <xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:attribute-set name="a"><xsl:comment/></xsl:attribute-set>
               <xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:attribute-set name="a"><xsl:attribute name="one">1</xsl:attribute></xsl:attribute-set>
           <xsl:template match="doc"><out xsl:use-attribute-sets="a"/></xsl:template>"#,
    ));
}

#[test]
fn xsl_text_is_pcdata_only() {
    // Appendix D: `xsl:text` content is `#PCDATA`. XSLT 1.0 tolerated nested
    // instructions; 2.0 does not.
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:text><xsl:text>x</xsl:text></xsl:text></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:text><xsl:if test="true()">y</xsl:if></xsl:text></xsl:template>"#,
        ),
    );
    // Text, comments and processing instructions are all fine.
    assert_eq!(
        run(&sheet(
            r#"<xsl:template match="doc"><out><xsl:text>a<!--c--><?p i?>b</xsl:text></out></xsl:template>"#
        ))
        .unwrap(),
        "<out>ab</out>"
    );
}

#[test]
fn xsl_sequence_requires_select_and_takes_only_fallback() {
    // §11.10 (XSLT 2.0): `select` is required and the content is `xsl:fallback*`.
    // XSLT 3.0 relaxed both, which is why the suite's `sequence-24xx` cases say
    // "(OK in XSLT 3.0)".
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:sequence/></xsl:template>"#),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:sequence><foo/></xsl:sequence></xsl:template>"#),
    );
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:sequence select="."><xsl:element name="e"/></xsl:sequence></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:sequence select="1"><xsl:fallback/></xsl:sequence></xsl:template>"#,
    ));
}

#[test]
fn a_declaration_is_not_an_instruction_and_an_instruction_is_not_a_declaration() {
    // Appendix D, "Permitted parent elements": xsl:key, xsl:function,
    // xsl:attribute-set and the rest of the declarations may only be children of
    // xsl:stylesheet/xsl:transform.
    for declaration in [
        r#"<xsl:key name="k" match="doc" use="."/>"#,
        r#"<xsl:function name="f:g" xmlns:f="urn:f"/>"#,
        r#"<xsl:attribute-set name="a"/>"#,
        r#"<xsl:output method="xml"/>"#,
        r#"<xsl:strip-space elements="*"/>"#,
        r##"<xsl:namespace-alias stylesheet-prefix="#default" result-prefix="#default"/>"##,
    ] {
        rejected(
            codes::XTSE0010,
            &sheet(&format!(
                r#"<xsl:template match="doc"><xsl:variable name="v">{declaration}</xsl:variable></xsl:template>"#
            )),
        );
    }
    // And the other way round.
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:value-of select="1"/><xsl:template match="doc"><out/></xsl:template>"#),
    );
    // xsl:variable is the one element that is both (Appendix D: "Category:
    // declaration instruction").
    accepted(&sheet(
        r#"<xsl:variable name="g" select="1"/>
           <xsl:template match="doc"><xsl:variable name="l" select="2"/><out/></xsl:template>"#,
    ));
}

#[test]
fn xsl_include_and_xsl_import_keep_their_own_codes() {
    // §3.10.2: "It is a static error if the xsl:include element is not a
    // top-level element." [XTSE0170]; §3.10.3 says the same of xsl:import
    // [XTSE0190]. Neither is the generic XTSE0010.
    rejected(
        codes::XTSE0170,
        &sheet(r#"<xsl:template name="main"><xsl:include href="other.xsl"/></xsl:template>"#),
    );
    rejected(
        codes::XTSE0190,
        &sheet(r#"<xsl:template name="main"><xsl:import href="other.xsl"/></xsl:template>"#),
    );
}

#[test]
fn a_required_attribute_may_not_be_omitted() {
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:for-each><out/></xsl:for-each></xsl:template>"#),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:if>x</xsl:if></xsl:template>"#),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:element>x</xsl:element></xsl:template>"#),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:strip-space/><xsl:template match="doc"><out/></xsl:template>"#),
    );
}

// ═══ §3.5 / §3.9 attributes — XTSE0090 ═══════════════════════════════════

#[test]
fn an_attribute_the_element_does_not_define_is_xtse0090() {
    // > [ERR XTSE0090] It is a static error if an element from the XSLT
    // > namespace has an attribute whose name is in no namespace and that is not
    // > one of the attributes defined for that element in this specification.
    //
    // The stylesheet element and xsl:include/xsl:import are checked too,
    // although the module collector consumes them.
    rejected(
        codes::XTSE0090,
        &format!(
            "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" extra-attribute=\"2\">\
               <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
        ),
    );
    rejected(
        codes::XTSE0090,
        &sheet(r#"<xsl:include href="other.xsl" iref="x.xsl"/>"#),
    );
    rejected(
        codes::XTSE0090,
        &sheet(r#"<xsl:import href="other.xsl" iref="x.xsl"/>"#),
    );
    rejected(
        codes::XTSE0090,
        &sheet(r#"<xsl:template match="doc" nosuch="1"><out/></xsl:template>"#),
    );
}

#[test]
fn an_attribute_in_another_namespace_is_always_allowed() {
    // §3.4 leaves an attribute in a namespace other than the XSLT namespace to
    // the implementation, and `xml:space`/`xml:base`/`xml:lang` must go through.
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xmlns:v=\"urn:vendor\" \
           v:optimize=\"fast\">\
           <xsl:template match=\"doc\" v:hint=\"x\" xml:space=\"default\" xml:base=\"urn:b\">\
             <out/></xsl:template></xsl:stylesheet>"
    ));
    // §3.5 the other way round: on an XSLT element the standard attributes are
    // spelled without a prefix, so an attribute that *is* in the XSLT namespace
    // is never one of them.
    rejected(
        codes::XTSE0090,
        &sheet(r#"<xsl:template match="doc" xsl:version="2.0"><out/></xsl:template>"#),
    );
}

#[test]
fn forwards_compatible_mode_ignores_an_unknown_attribute() {
    // §3.9: "if an element has an attribute that XSLT 2.0 does not allow the
    // element to have, then the attribute must be ignored."
    accepted(&sheet_version(
        "3.0",
        r#"<xsl:template match="doc" nosuch="1"><out/></xsl:template>"#,
    ));
    // "The compatibility behavior established by an element overrides any
    // compatibility behavior established by an ancestor element", so a 2.0
    // island inside a 3.0 module is strict again.
    rejected(
        codes::XTSE0090,
        &sheet_version(
            "3.0",
            r#"<xsl:template match="doc" version="2.0" nosuch="1"><out/></xsl:template>"#,
        ),
    );
}

#[test]
fn forwards_compatible_mode_ignores_an_unknown_declaration_but_not_an_unknown_instruction() {
    // §3.9, first bullet: an unknown XSLT element among the children of
    // xsl:stylesheet "and its content must be ignored".
    accepted(&sheet_version(
        "3.0",
        r#"<xsl:brand-new-declaration><whatever/></xsl:brand-new-declaration>
           <xsl:template match="doc"><out/></xsl:template>"#,
    ));
    // §3.9, third bullet: in a sequence constructor, "If the element has no
    // xsl:fallback children, then a static error is reported in the same way as
    // if forwards-compatible behavior were not enabled." Not at evaluation,
    // which was the XSLT 1.0 rule.
    rejected(
        codes::XTSE0010,
        &sheet_version(
            "3.0",
            r#"<xsl:template match="doc"><xsl:exciting-new-feature/></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:exciting-new-feature/></xsl:template>"#),
    );
}

#[test]
fn a_fallback_child_makes_an_unknown_instruction_legal_in_forwards_compatible_mode() {
    // §3.9: "If the element has one or more xsl:fallback children, then no error
    // is reported either statically or dynamically, and the result of evaluating
    // the instruction is the concatenation of the sequences formed by evaluating
    // the sequence constructors within its xsl:fallback children … Siblings of
    // the xsl:fallback elements are ignored, even if they are valid XSLT 2.0
    // instructions."
    assert_eq!(
        run(&sheet_version(
            "17.0",
            r#"<xsl:template match="doc"><out><xsl:exciting-new-17.0-feature>
                 <xsl:fly-to-the-moon/>
                 <xsl:fallback>fell back</xsl:fallback>
               </xsl:exciting-new-17.0-feature></out></xsl:template>"#
        ))
        .unwrap(),
        "<out>fell back</out>"
    );
    // The fallback's own content is checked, because it *is* evaluated.
    rejected(
        codes::XTSE0010,
        &sheet_version(
            "17.0",
            r#"<xsl:template match="doc"><xsl:new-thing>
                 <xsl:fallback><xsl:choose/></xsl:fallback>
               </xsl:new-thing></xsl:template>"#,
        ),
    );
}

#[test]
fn use_when_removes_a_child_before_anything_is_checked() {
    // §3.12: "When a node is effectively excluded from a stylesheet module the
    // stylesheet module has the same effect as if the node were not there. Among
    // other things this means that no static or dynamic errors will be reported
    // in respect of the element and its contents."
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:apply-imports>
             <xsl:sort select="." use-when="false()"/>
           </xsl:apply-imports></xsl:template>"#,
    ));
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:choose use-when="false()">nonsense</xsl:choose>
           <out/></xsl:template>"#,
    ));
    accepted(&sheet(
        r#"<xsl:nonsense-declaration use-when="false()"/>
           <xsl:template match="doc"><out/></xsl:template>"#,
    ));
    // …and the same child with a true condition is rejected, which is what shows
    // the test above is exercising the exclusion and not a hole in the pass.
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:apply-imports>
                 <xsl:sort select="." use-when="true()"/>
               </xsl:apply-imports></xsl:template>"#,
        ),
    );
}

// ═══ §4.2 / XTSE0120 / XTSE0260 — text and emptiness ═════════════════════

#[test]
fn the_stylesheet_element_may_not_have_text_children() {
    // > [ERR XTSE0120] An xsl:stylesheet element must not have any text node
    // > children. (This rule applies after stripping of whitespace text nodes as
    // > described in §4.2.)
    rejected(
        codes::XTSE0120,
        &sheet(r#"<xsl:template match="doc"><out/></xsl:template>custard"#),
    );
    // §4.2 lists xsl:stylesheet among the elements whose whitespace children go
    // "regardless of any xml:space attributes", so this is not an error.
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xml:space=\"preserve\">\n   \n\
           <xsl:template match=\"doc\"><out/></xsl:template>\n</xsl:stylesheet>"
    ));
}

#[test]
fn an_element_required_to_be_empty_takes_comments_and_pis_only() {
    // > [ERR XTSE0260] Within an XSLT element that is required to be empty, any
    // > content other than comments or processing instructions, including any
    // > whitespace text node preserved using the xml:space="preserve" attribute,
    // > is a static error.
    rejected(
        codes::XTSE0260,
        &sheet(r#"<xsl:strip-space elements="*">*</xsl:strip-space>"#),
    );
    rejected(
        codes::XTSE0260,
        &sheet(r#"<xsl:include href="other.xsl">text</xsl:include>"#),
    );
    rejected(
        codes::XTSE0260,
        &sheet(r#"<xsl:import href="other.xsl">text</xsl:import>"#),
    );
    // xsl:strip-space is *not* in §4.2's unconditional list, so preserved
    // whitespace really does reach it — and is an error.
    rejected(
        codes::XTSE0260,
        &sheet("<xsl:strip-space elements=\"*\" xml:space=\"preserve\"> \n</xsl:strip-space>"),
    );
    rejected(
        codes::XTSE0260,
        &sheet(
            r#"<xsl:template match="doc"><xsl:copy-of select="."><out/></xsl:copy-of></xsl:template>"#,
        ),
    );
    // Comments, processing instructions and ordinary (stripped) whitespace are
    // all fine.
    accepted(&sheet(
        "<xsl:strip-space elements=\"*\">\n  <!-- why --><?tool hint?>\n</xsl:strip-space>\
         <xsl:template match=\"doc\"><out/></xsl:template>",
    ));
}

// ═══ xs:decimal lexical forms — XTSE0110 and XTSE0530 ════════════════════

#[test]
fn version_and_priority_are_xs_decimal_not_xs_double() {
    // §3.6: "The value of the version attribute must be a number: specifically,
    // it must be a valid instance of the type xs:decimal" [XTSE0110]; §6.4 says
    // the same of xsl:template/@priority [XTSE0530]. `f64::from_str` accepts an
    // exponent, which xs:decimal's lexical space does not have.
    rejected(
        codes::XTSE0110,
        &sheet_version(
            "2.0e3",
            r#"<xsl:template name="main"><out/></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0530,
        &sheet(r#"<xsl:template match="doc" priority="2.0e2"><out/></xsl:template>"#),
    );
    // Negative, signed and trailing-dot forms are all xs:decimal.
    accepted(&sheet(
        r#"<xsl:template match="doc" priority="-1.5"><out/></xsl:template>
           <xsl:template match="item" priority="+2."><out/></xsl:template>"#,
    ));
    // §3.9: "These rules do not apply to the version attribute of the xsl:output
    // element, which has an entirely different purpose."
    accepted(&sheet(
        r#"<xsl:output method="xml" version="1.1"/>
           <xsl:template match="doc"><out/></xsl:template>"#,
    ));
}

// ═══ §6.4 / §6.5 — XTSE0500 and XTSE0550 ═════════════════════════════════

#[test]
fn a_template_without_match_has_neither_mode_nor_priority() {
    // > [ERR XTSE0500] … An xsl:template element that has no match attribute
    // > must have no mode attribute and no priority attribute.
    rejected(
        codes::XTSE0500,
        &sheet(r#"<xsl:template name="sub" priority="2"/>"#),
    );
    rejected(
        codes::XTSE0500,
        &sheet(r#"<xsl:template name="sub" mode="m"/>"#),
    );
    rejected(codes::XTSE0500, &sheet(r#"<xsl:template/>"#));
    accepted(&sheet(
        r#"<xsl:template name="sub" match="doc" mode="m" priority="2"><out/></xsl:template>"#,
    ));
}

#[test]
fn the_mode_list_of_a_template_is_a_set_of_tokens() {
    // > [ERR XTSE0550] It is a static error if the list is empty, if the same
    // > token is included more than once in the list, if the list contains an
    // > invalid token, or if the token #all appears together with any other
    // > value.
    rejected(
        codes::XTSE0550,
        &sheet(r#"<xsl:template match="doc" mode=""><out/></xsl:template>"#),
    );
    rejected(
        codes::XTSE0550,
        &sheet(r#"<xsl:template match="doc" mode="a b c a"><out/></xsl:template>"#),
    );
    rejected(
        codes::XTSE0550,
        &sheet(r##"<xsl:template match="doc" mode="#default #default"><out/></xsl:template>"##),
    );
    rejected(
        codes::XTSE0550,
        &sheet(r##"<xsl:template match="doc" mode="#all a"><out/></xsl:template>"##),
    );
    // Two prefixes bound to one namespace are the *same* token, because §5.1
    // compares expanded names.
    rejected(
        codes::XTSE0550,
        &format!(
            "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xmlns:p=\"urn:m\" \
               xmlns:q=\"urn:m\">\
               <xsl:template match=\"doc\" mode=\"p:m q:m\"><out/></xsl:template>\
             </xsl:stylesheet>"
        ),
    );
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xmlns:p=\"urn:p\" \
           xmlns:q=\"urn:q\">\
           <xsl:template match=\"doc\" mode=\"#default p:m q:m\"><out/></xsl:template>\
         </xsl:stylesheet>"
    ));
    accepted(&sheet(
        r##"<xsl:template match="doc" mode="#all"><out/></xsl:template>"##,
    ));
}

#[test]
fn apply_templates_mode_is_a_token_not_an_attribute_value_template() {
    // Appendix D gives `xsl:apply-templates/@mode` the type `token`, not
    // `{ token }`, so `{$x}` is simply not one of the permitted values — §3.4's
    // XTSE0020, and not §6.5's XTSE0550, which is about the *list* on
    // xsl:template.
    rejected(
        codes::XTSE0020,
        &sheet(
            r#"<xsl:template match="doc"><xsl:apply-templates select="item" mode="{$x}"/></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r##"<xsl:template match="doc"><xsl:apply-templates select="item" mode="#current"/></xsl:template>
           <xsl:template match="item"><out/></xsl:template>"##,
    ));
}

// ═══ §9.2 / §10.1.1 — duplicate parameters, by expanded QName ════════════

#[test]
fn two_parameters_of_a_template_may_not_share_a_name() {
    // > [ERR XTSE0580] It is a static error if two parameters of a template or
    // > of a stylesheet function have the same name.
    rejected(
        codes::XTSE0580,
        &sheet(
            r#"<xsl:template match="doc">
                 <xsl:param name="p" select="1"/><xsl:param name="p" select="2"/>
               </xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:template match="doc">
             <xsl:param name="p" select="1"/><xsl:param name="q" select="2"/><out/>
           </xsl:template>"#,
    ));
}

#[test]
fn duplicate_with_param_is_compared_by_expanded_qname() {
    // > [ERR XTSE0670] It is a static error if a single xsl:call-template,
    // > xsl:apply-templates, xsl:apply-imports, or xsl:next-match element
    // > contains two or more xsl:with-param elements with matching name
    // > attributes.
    //
    // "matching name attributes" means matching *expanded* names (§5.1), so two
    // different prefixes bound to the same URI collide and the same prefix
    // rebound to a different URI does not — which is exactly what a lexical
    // comparison would get wrong in both directions.
    rejected(
        codes::XTSE0670,
        &sheet(
            r#"<xsl:template name="x"><xsl:param name="y"/></xsl:template>
               <xsl:template match="doc"><xsl:call-template name="x">
                 <xsl:with-param name="y" select="3"/>
                 <xsl:with-param name="y" select="4"/>
               </xsl:call-template></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0670,
        &sheet(
            r#"<xsl:template name="x"><xsl:param xmlns:x="urn:a" name="x:y"/></xsl:template>
               <xsl:template match="doc"><xsl:call-template name="x">
                 <xsl:with-param xmlns:a="urn:a" name="a:y" select="3"/>
                 <xsl:with-param xmlns:b="urn:a" name="b:y" select="4"/>
               </xsl:call-template></xsl:template>"#,
        ),
    );
    // The same *spelling* in two different namespaces is two different
    // parameters.
    accepted(&sheet(
        r#"<xsl:template name="x">
             <xsl:param xmlns:a="urn:a" name="a:y"/><xsl:param xmlns:b="urn:b" name="b:y"/>
           </xsl:template>
           <xsl:template match="doc"><xsl:call-template name="x">
             <xsl:with-param xmlns:a="urn:a" name="a:y" select="3"/>
             <xsl:with-param xmlns:a="urn:b" name="a:y" select="4"/>
           </xsl:call-template></xsl:template>"#,
    ));
}

#[test]
fn a_tunnel_parameter_does_not_satisfy_a_non_tunnel_with_param() {
    // > [ERR XTSE0680] In the case of xsl:call-template, it is a static error to
    // > pass a non-tunnel parameter named x to a template that does not have a
    // > template parameter named x …
    //
    // §10.1.2 keeps tunnel and non-tunnel parameters apart, so a `tunnel="yes"`
    // xsl:param is not the parameter a plain xsl:with-param is naming. The check
    // is static, so it fires although the template below is never invoked.
    rejected(
        codes::XTSE0680,
        &sheet(
            r#"<xsl:template match="doc"><out/></xsl:template>
               <xsl:template name="unreached"><xsl:call-template name="temp">
                 <xsl:with-param name="p" select="1"/>
               </xsl:call-template></xsl:template>
               <xsl:template name="temp"><xsl:param name="p" tunnel="yes"/></xsl:template>"#,
        ),
    );
    // A tunnel xsl:with-param needs no declaration at all, and a non-tunnel one
    // matching a non-tunnel xsl:param is fine.
    accepted(&sheet(
        r#"<xsl:template match="doc"><xsl:call-template name="temp">
             <xsl:with-param name="p" select="1"/>
             <xsl:with-param name="undeclared" select="2" tunnel="yes"/>
           </xsl:call-template></xsl:template>
           <xsl:template name="temp"><xsl:param name="p"/></xsl:template>"#,
    ));
}

#[test]
fn a_stylesheet_parameter_cannot_be_a_tunnel_parameter() {
    // §9.2: `tunnel` "the value yes may be specified only for template
    // parameters".
    rejected(
        codes::XTSE0020,
        &sheet(
            r#"<xsl:param name="p" tunnel="yes"/><xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:param name="p" tunnel="no"/><xsl:template match="doc"><out/></xsl:template>"#,
    ));
}

// ═══ §10.2 — XTSE0710 ════════════════════════════════════════════════════

#[test]
fn use_attribute_sets_is_checked_statically() {
    // > [ERR XTSE0710] It is a static error if the value of the
    // > use-attribute-sets attribute … is not a whitespace-separated sequence of
    // > EQNames, or if it contains a QName that does not match the name
    // > attribute of any xsl:attribute-set declaration in the stylesheet.
    //
    // Both halves, and both statically: an attribute set that is never used at
    // run time used to escape the check entirely.
    rejected(
        codes::XTSE0710,
        &sheet(
            r#"<xsl:attribute-set name="a" use-attribute-sets="missing">
                 <xsl:attribute name="one">1</xsl:attribute>
               </xsl:attribute-set>
               <xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0710,
        &sheet(
            r##"<xsl:template match="doc"><xsl:element name="e" use-attribute-sets="#z"/></xsl:template>"##,
        ),
    );
    rejected(
        codes::XTSE0710,
        &sheet(
            r#"<xsl:template match="doc"><out xsl:use-attribute-sets="missing"/></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:attribute-set name="base"><xsl:attribute name="one">1</xsl:attribute></xsl:attribute-set>
           <xsl:attribute-set name="a" use-attribute-sets="base"/>
           <xsl:template match="doc"><xsl:element name="e" use-attribute-sets="a base"/></xsl:template>"#,
    ));
}

// ═══ §20 — XTSE1560 ══════════════════════════════════════════════════════

#[test]
fn two_xsl_output_declarations_may_not_disagree_at_the_same_precedence() {
    // > [ERR XTSE1560] It is a static error if two xsl:output declarations within
    // > an output definition specify explicit values for the same attribute
    // > (other than cdata-section-elements and use-character-maps), with the
    // > values of the attributes being not equal, unless there is another
    // > xsl:output declaration within the same output definition that has higher
    // > import precedence and that specifies an explicit value for the same
    // > attribute.
    rejected(
        codes::XTSE1560,
        &sheet(
            r#"<xsl:output indent="no"/><xsl:output indent="yes"/>
               <xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE1560,
        &sheet(
            r#"<xsl:output name="a" method="xml"/><xsl:output name="a" method="html"/>
               <xsl:template match="doc"><out/></xsl:template>"#,
        ),
    );
    // Equal values are not a conflict, and the named and unnamed definitions are
    // separate output definitions.
    accepted(&sheet(
        r#"<xsl:output indent="yes"/><xsl:output indent="yes" encoding="utf-8"/>
           <xsl:output name="a" indent="no"/>
           <xsl:template match="doc"><out/></xsl:template>"#,
    ));
}

// ═══ §3.6 — XTSE0265 ═════════════════════════════════════════════════════

#[test]
fn input_type_annotations_must_agree_across_the_module_graph() {
    // > [ERR XTSE0265] It is a static error if there is a stylesheet module in
    // > the stylesheet that specifies input-type-annotations="strip" and another
    // > stylesheet module that specifies input-type-annotations="preserve".
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert(SOURCE_URI, SOURCE);
    mem.insert(
        "file:///static/preserving.xsl",
        format!(
            "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
               input-type-annotations=\"preserve\"/>"
        ),
    );
    let strip_and_preserve: Arc<dyn Resolver> = Arc::new(mem);
    let module = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" input-type-annotations=\"strip\">\
           <xsl:include href=\"preserving.xsl\"/>\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    );
    let e = Compiler::new(&names, &functions)
        .with_resolver(strip_and_preserve)
        .compile_str(&module, MAIN)
        .map(|_| ())
        .expect_err("strip and preserve in one stylesheet is XTSE0265");
    assert_eq!(e.code, codes::XTSE0265);
    // Only the three named values are allowed.
    rejected(
        codes::XTSE0020,
        &format!(
            "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
               input-type-annotations=\"maybe\">\
               <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
        ),
    );
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" input-type-annotations=\"strip\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    ));
}

// ═══ §5.4.1 — XTSE0125 ═══════════════════════════════════════════════════

#[test]
fn an_unrecognised_default_collation_is_a_static_error() {
    // > [ERR XTSE0125] It is a static error if the value of an
    // > [xsl:]default-collation attribute, after resolving against the base URI,
    // > contains no URI that the implementation recognizes as a collation URI.
    let e = compile(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           default-collation=\"mailto:nobody@nowhere.example\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    ))
    .expect_err("an unrecognised collation URI is XTSE0125");
    assert_eq!(e.code, codes::XTSE0125);
    // §5.4.1: "If the implementation recognizes one or more of the resulting
    // absolute collation URIs, then it uses the first one that it recognizes",
    // so a list ending in the codepoint collation is fine.
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           default-collation=\"urn:vendor:a \
             http://www.w3.org/2005/xpath-functions/collation/codepoint\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    ));
    // The registry recognizes the HTML ASCII case-insensitive collation.
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           default-collation=\"http://www.w3.org/2005/xpath-functions/collation/\
             html-ascii-case-insensitive\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    ));
    // A collation [Functions and Operators] defines but this processor has not
    // built is a **missing feature**, not a broken stylesheet: reporting it as
    // XTSE0125 would claim conformance the processor does not have. The UCA
    // collations need the `icu-collation` cargo feature; with it
    // the URI is accepted (`tests/uca_collations.rs`).
    let uca = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           default-collation=\"http://www.w3.org/2013/collation/UCA?lang=en\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    );
    #[cfg(not(feature = "icu-collation"))]
    {
        let e = compile(&uca).expect_err("an unimplemented standard collation is not implemented");
        assert!(e.is_not_implemented(), "{e}");
    }
    #[cfg(feature = "icu-collation")]
    accepted(&uca);
}

// ═══ §3.7 — XTSE0150 ═════════════════════════════════════════════════════

#[test]
fn a_simplified_stylesheet_needs_xsl_version() {
    // > [ERR XTSE0150] A literal result element that is used as the outermost
    // > element of a simplified stylesheet module must have an xsl:version
    // > attribute.
    let e = compile(&format!(
        "<lre xmlns:xsl=\"{NS}\" version=\"2.0\"><out/></lre>"
    ))
    .expect_err("a simplified module with no xsl:version is XTSE0150");
    assert_eq!(e.code, codes::XTSE0150);
    // A stylesheet element whose prefix is bound to the wrong URI is just a
    // literal result element with an unfortunate name.
    let e = compile("<xsl:stylesheet xmlns:xsl=\"http://wrong-namespace/\" version=\"2.0\"/>")
        .expect_err("the wrong XSLT namespace makes this a simplified module");
    assert_eq!(e.code, codes::XTSE0150);
    // With the attribute it is a simplified stylesheet, and it works.
    assert_eq!(
        run(&format!(
            "<out xmlns:xsl=\"{NS}\" xsl:version=\"2.0\"><xsl:value-of select=\"name(/*)\"/></out>"
        ))
        .unwrap(),
        "<out>doc</out>"
    );
}

// ═══ §21.1 — XTSE1660, the basic-processor restrictions ═══════════════════

#[test]
fn a_basic_processor_rejects_type_and_non_strip_validation() {
    // > [ERR XTSE1660] A basic XSLT processor must signal a static error if the
    // > stylesheet includes an [xsl:]type attribute, or an [xsl:]validation or
    // > default-validation attribute with a value other than strip.
    //
    // Under XSLT 2.0 that includes `preserve` and `lax`, which XSLT 3.0 later
    // relaxed — which is why the suite tags those cases XSLT20 only.
    for attribute in [
        r#"type="xs:double""#,
        r#"validation="strict""#,
        r#"validation="lax""#,
        r#"validation="preserve""#,
    ] {
        rejected(
            codes::XTSE1660,
            &format!(
                "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
                   xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\
                   <xsl:template match=\"doc\">\
                     <xsl:element name=\"e\" {attribute}>2.2</xsl:element>\
                   </xsl:template></xsl:stylesheet>"
            ),
        );
    }
    for value in ["strict", "preserve", "lax"] {
        rejected(
            codes::XTSE1660,
            &format!(
                "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
                   default-validation=\"{value}\">\
                   <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
            ),
        );
    }
    // The same rule for the spelling a literal result element uses (§11.1).
    rejected(
        codes::XTSE1660,
        &sheet(r#"<xsl:template match="doc"><out xsl:validation="strict"/></xsl:template>"#),
    );
    // `strip` is the one value a basic processor supports, on every element that
    // admits the attribute — and it is the default, so a stylesheet that spells
    // it out must still compile.
    accepted(&sheet(
        r#"<xsl:template match="doc">
             <xsl:element name="e" validation="strip">
               <xsl:attribute name="a" validation="strip">1</xsl:attribute>
               <xsl:copy-of select="item" validation="strip"/>
             </xsl:element>
           </xsl:template>"#,
    ));
    // On a *literal result element* the legal `xsl:validation="strip"` still
    // reaches the literal-result-element compiler's own `XSLT2-NOTIMPL` for the
    // attribute, whatever its value: this pins the current state rather than
    // the ideal one.
    accepted_or_not_implemented(&sheet(
        r#"<xsl:template match="doc"><out xsl:validation="strip"/></xsl:template>"#,
    ));
    accepted(&format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" default-validation=\"strip\">\
           <xsl:template match=\"doc\"><out/></xsl:template></xsl:stylesheet>"
    ));
}

// ═══ §11.4.3 — XTSE0870 ══════════════════════════════════════════════════

#[test]
fn value_of_needs_exactly_one_of_select_and_content() {
    // §11.4.3: "These are mutually exclusive, and one of them must be present.
    // [ERR XTSE0870]" — the "neither" half was never checked.
    rejected(
        codes::XTSE0870,
        &sheet(r#"<xsl:template match="doc"><out><xsl:value-of/></out></xsl:template>"#),
    );
    rejected(
        codes::XTSE0870,
        &sheet(
            r#"<xsl:template match="doc"><xsl:value-of select="1">2</xsl:value-of></xsl:template>"#,
        ),
    );
    accepted(&sheet(
        r#"<xsl:template match="doc"><out><xsl:value-of select="1"/></out></xsl:template>"#,
    ));
    accepted(&sheet(
        r#"<xsl:template match="doc"><out><xsl:value-of>2</xsl:value-of></out></xsl:template>"#,
    ));
}

// ═══ §2.3 — XTDE0047 ═════════════════════════════════════════════════════

#[test]
fn an_initial_mode_and_an_initial_template_together_are_xtde0047() {
    // > [ERR XTDE0047] It is a non-recoverable dynamic error if the invocation
    // > of the stylesheet specifies both an initial mode and an initial template.
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let resolver = resolver();
    let module = sheet(
        r#"<xsl:template name="main"><out/></xsl:template>
           <xsl:template match="doc" mode="m"><in/></xsl:template>"#,
    );
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&module, MAIN)
        .unwrap();

    let both = |template: bool, mode: bool| {
        let arena = Bump::new();
        let mut run = compiled
            .transformation(&arena, TransformOptions::default())
            .with_resolver(Arc::clone(&resolver));
        if template {
            run = run.with_initial_template(QName::local("main"));
        }
        if mode {
            run = run.with_initial_mode(ModeKey::Named(QName::local("m")));
        }
        run.transform_source(SOURCE_URI, SOURCE).map(|_| ())
    };
    assert_eq!(both(true, true).unwrap_err().code, codes::XTDE0047);
    // Either one alone is a perfectly ordinary invocation.
    assert!(both(true, false).is_ok());
    assert!(both(false, true).is_ok());
}

// ═══ §18.1.4 — type-available ════════════════════════════════════════════

#[test]
fn type_available_answers_for_the_built_in_types() {
    // §18.1.4 + §3.13 + §21.1: a basic processor's static context holds every
    // primitive atomic type except xs:NOTATION, xs:integer, xs:anyType,
    // xs:anySimpleType, and XPath 2.0's five additions. `type-available` used to
    // answer false for all of them, which also mis-steered `use-when`.
    let probe = |name: &str| {
        run(&format!(
            "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
               xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
               exclude-result-prefixes=\"xs\">\
               <xsl:template match=\"doc\">\
                 <out><xsl:value-of select=\"type-available('{name}')\"/></out>\
               </xsl:template></xsl:stylesheet>"
        ))
        .unwrap()
    };
    for available in [
        "xs:anyType",
        "xs:anySimpleType",
        "xs:integer",
        "xs:string",
        "xs:date",
        "xs:time",
        "xs:hexBinary",
        "xs:untyped",
        "xs:untypedAtomic",
        "xs:anyAtomicType",
        "xs:dayTimeDuration",
        "xs:yearMonthDuration",
    ] {
        assert_eq!(probe(available), "<out>true</out>", "{available}");
    }
    for absent in [
        // Not in a basic processor's static context (§21.1 names xs:NCName as
        // its example of an "unknown type").
        "xs:NCName",
        "xs:NOTATION",
        "xs:unknownType",
        // Unprefixed: §18.1.4 expands it in the default namespace, which is
        // absent here, so it is in no namespace and matches nothing.
        "unknownType",
        "string",
        // A prefix bound to a namespace that holds no types.
        "xsl:unknownType",
    ] {
        assert_eq!(probe(absent), "<out>false</out>", "{absent}");
    }
}

// ═══ §3.12 — the use-when static and dynamic context ═════════════════════

#[test]
fn an_xslt_specific_function_is_not_in_scope_in_use_when() {
    for absent in [
        "generate-id(())",
        "current()",
        "unparsed-text('x')",
        "regex-group(1)",
    ] {
        let e = compile(&sheet(&format!(
            r#"<xsl:template match="doc" use-when="exists({absent})"><out/></xsl:template>"#
        )))
        .expect_err("an XSLT-specific function does not bind in a use-when expression");
        assert_eq!(e.code, "XPST0017", "{absent}: {e}");
    }
    // The four §3.12 names it *does* have, plus an F&O core function. (`xs` is
    // bound here: an unbound prefix is XTDE1428, which is X1 item 2's test.)
    accepted(&sheet(
        r#"<xsl:template match="doc" xmlns:xs="http://www.w3.org/2001/XMLSchema"
             use-when="system-property('xsl:vendor') != '' and element-available('xsl:if')
                       and function-available('system-property') and type-available('xs:integer')
                       and string-length(concat('a','b')) = 2"><out/></xsl:template>"#,
    ));
}

#[test]
fn use_when_has_no_documents() {
    // §3.12: "Statically known documents: None" and "Available documents: None".
    // `fn:doc` is an F&O core function, so it *binds* — and then cannot find
    // anything, which F&O makes FODC0002.
    let e = compile(&sheet(
        r#"<xsl:template match="doc" use-when="doc('other.xsl')"><out/></xsl:template>"#,
    ))
    .expect_err("doc() in a use-when expression cannot resolve anything");
    assert_eq!(e.code, "FODC0002", "{e}");
    // `fn:doc-available` answers false rather than raising, so a stylesheet may
    // guard on it.
    accepted(&sheet(
        r#"<xsl:template match="doc" use-when="not(doc-available(''))"><out/></xsl:template>"#,
    ));
}

#[test]
fn function_available_in_use_when_answers_for_the_use_when_scope() {
    // §3.12: "The effect of this rule is to ensure that function-available
    // returns true in respect of functions that can be called within the scope
    // of the use-when attribute."
    let probe = |name: &str| -> bool {
        // The element survives only when function-available says true.
        run(&sheet(&format!(
            r#"<xsl:template match="doc"><out>
                 <in xsl:use-when="function-available('{name}')"/>
               </out></xsl:template>"#
        )))
        .unwrap()
        .contains("<in/>")
    };
    assert!(probe("system-property"));
    assert!(probe("element-available"));
    assert!(probe("type-available"));
    assert!(probe("concat"));
    assert!(!probe("current"));
    assert!(!probe("generate-id"));
    assert!(!probe("key"));
    assert!(!probe("unparsed-entity-uri"));
    // Outside a use-when expression the same calls answer for the stylesheet's
    // own function library, where all four are available.
    let outside = run(&sheet(
        r#"<xsl:template match="doc"><out>
             <xsl:value-of select="function-available('current')"/>
             <xsl:value-of select="function-available('generate-id')"/>
             <xsl:value-of select="function-available('key')"/>
             <xsl:value-of select="function-available('unparsed-entity-uri')"/>
           </out></xsl:template>"#,
    ))
    .unwrap();
    assert_eq!(outside, "<out>truetruetruetrue</out>");
}

#[test]
fn function_available_covers_every_arity_a_function_has() {
    // §18.1.2 asks whether a function of that name exists at **any** arity; the
    // helper used to scan 0 to 4 only, so the five-argument §16.5 signatures
    // answered false. (`format-date`'s five-argument form is the high arity.)
    let answers = run(&sheet(
        r#"<xsl:template match="doc"><out>
             <xsl:value-of select="function-available('round-half-to-even')"/>
             <xsl:value-of select="function-available('substring')"/>
             <xsl:value-of select="function-available('concat')"/>
             <xsl:value-of select="function-available('no-such-function')"/>
             <xsl:value-of select="function-available('format-date', 5)"/>
           </out></xsl:template>"#,
    ))
    .unwrap();
    assert_eq!(answers, "<out>truetruetruefalsetrue</out>");
}

// ═══ An element without an implementation is still not a static error ═════

#[test]
fn an_unimplemented_element_in_a_legal_position_is_never_a_static_error() {
    // A construct the processor recognizes but does not implement raises
    // XSLT2-NOTIMPL. The element table describes every XSLT 2.0 element, so a
    // correctly placed one the compiler did not implement would be classified
    // as a missing feature, never as a static error.
    for declaration in [
        r#"<xsl:key name="k" match="doc" use="."/>"#,
        // (`decimal-separator=","` alone, with the default
        // `grouping-separator`, is XTSE1300, §16.4.1.)
        r#"<xsl:decimal-format name="d" decimal-separator="," grouping-separator="."/>"#,
        r#"<xsl:character-map name="m"><xsl:output-character character="a" string="b"/></xsl:character-map>"#,
        r#"<xsl:function name="f:g" xmlns:f="urn:f"><xsl:param name="p"/><xsl:sequence select="$p"/></xsl:function>"#,
    ] {
        accepted_or_not_implemented(&sheet(&format!(
            r#"{declaration}<xsl:template match="doc"><out/></xsl:template>"#
        )));
    }
    for instruction in [
        r#"<xsl:number value="1"/>"#,
        r#"<xsl:perform-sort select="item"><xsl:sort select="."/></xsl:perform-sort>"#,
        r#"<xsl:for-each-group select="item" group-by="."><xsl:sort select="."/><out/></xsl:for-each-group>"#,
        r#"<xsl:analyze-string select="'a'" regex="a"><xsl:matching-substring>m</xsl:matching-substring><xsl:non-matching-substring>n</xsl:non-matching-substring></xsl:analyze-string>"#,
        r#"<xsl:result-document href="out.xml"><out/></xsl:result-document>"#,
    ] {
        accepted_or_not_implemented(&sheet(&format!(
            r#"<xsl:template match="doc">{instruction}</xsl:template>"#
        )));
    }
    // But the same elements in the wrong place, or malformed, are static errors.
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:analyze-string select="'a'" regex="a">
                 <xsl:non-matching-substring>n</xsl:non-matching-substring>
                 <xsl:matching-substring>m</xsl:matching-substring>
               </xsl:analyze-string></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0010,
        &sheet(
            r#"<xsl:template match="doc"><xsl:perform-sort select="item"><out/></xsl:perform-sort></xsl:template>"#,
        ),
    );
    rejected(
        codes::XTSE0010,
        &sheet(r#"<xsl:template match="doc"><xsl:matching-substring/></xsl:template>"#),
    );
}

// ═══ the default type namespace in `use-when` ═════════════════════════════
//
// §3.12's static-context table for a `use-when` expression:
//
// > Default element/type namespace — determined by the `xpath-default-namespace`
// > attribute if present (see 5.2 Unprefixed QNames in Expressions and
// > Patterns); otherwise the null namespace
//
// The `use-when` evaluator used to expand **every** unprefixed name in the null
// namespace, so `type-available('string')` was false where the run-time call was
// true, and the element the `use-when` guarded was silently excluded.
//
// The three `*-available` functions do not share one rule — §5.1 is explicit
// that they differ — so each is pinned here in both directions, static and
// dynamic, and the two paths now go through one `expand_available_name`.
//
// The probes below match `/` and answer with `xsl:text`, never with a literal
// result element and never with a name-test pattern: a default namespace — of
// either kind — would otherwise change what the template *matches* or what it
// *builds*, and the test would stop being about the function.

/// The two ways of asking one of these functions the same question: statically,
/// as a `use-when` guard that decides whether the template exists at all, and
/// dynamically, as the test of an `xsl:if` inside it. `"yes"` is true, `""` is
/// false (the built-in rules then produce the source's text, which is empty).
fn available_sheets(decls: &str, call: &str) -> (String, String) {
    let statically = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" {decls}>\
           <xsl:template match=\"/\" use-when=\"{call}\"><xsl:text>yes</xsl:text></xsl:template>\
         </xsl:stylesheet>"
    );
    let dynamically = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" {decls}>\
           <xsl:template match=\"/\">\
             <xsl:if test=\"{call}\"><xsl:text>yes</xsl:text></xsl:if>\
           </xsl:template>\
         </xsl:stylesheet>"
    );
    (statically, dynamically)
}

fn available_probes(decls: &str, call: &str) -> (String, String) {
    let (statically, dynamically) = available_sheets(decls, call);
    (
        run(&statically).unwrap_or_else(|e| panic!("use-when {call:?} with {decls:?}: {e}")),
        run(&dynamically).unwrap_or_else(|e| panic!("run-time {call:?} with {decls:?}: {e}")),
    )
}

#[track_caller]
fn both_paths_agree(decls: &str, call: &str, expected: &str) {
    let (statically, dynamically) = available_probes(decls, call);
    assert_eq!(statically, expected, "use-when {call:?} with {decls:?}");
    assert_eq!(dynamically, expected, "run-time {call:?} with {decls:?}");
}

/// `xpath-default-namespace` is the default **type** namespace, so an unprefixed
/// type name resolves in it — in `use-when` exactly as at run time (§18.1.4:
/// "If the lexical QName is unprefixed, then the default namespace is used in
/// the expanded QName").
#[test]
fn type_available_in_use_when_honours_xpath_default_namespace() {
    const XSD: &str = "http://www.w3.org/2001/XMLSchema";
    let xpath_default = format!("xpath-default-namespace=\"{XSD}\"");

    // The defect: with the XSD namespace as the default type namespace,
    // `type-available('string')` was true at run time and false in `use-when`,
    // so the guarded element was silently dropped.
    both_paths_agree(&xpath_default, "type-available('string')", "yes");
    both_paths_agree(&xpath_default, "type-available('anyAtomicType')", "yes");
    // An unknown local name is still unknown.
    both_paths_agree(&xpath_default, "type-available('unknownType')", "");
    // Without the attribute an unprefixed name is in no namespace (§5.2: "an
    // unprefixed element name or type name refers to a name that is in no
    // namespace") and matches nothing.
    both_paths_agree("", "type-available('string')", "");
    // A default namespace that holds no types.
    both_paths_agree(
        "xpath-default-namespace=\"http://example.com/\"",
        "type-available('string')",
        "",
    );
    // The `xmlns=` default is *not* what a type name uses (§5.2: the attribute
    // "does not affect … the default namespace of the parent element").
    both_paths_agree(&format!("xmlns=\"{XSD}\""), "type-available('string')", "");

    // A *prefixed* name ignores both defaults (§5.1: "If the QName has a prefix,
    // then the prefix is expanded into a URI reference using the namespace
    // declarations in effect on its defining element").
    let bound = format!("xmlns:xs=\"{XSD}\" xpath-default-namespace=\"http://example.com/\"");
    both_paths_agree(&bound, "type-available('xs:string')", "yes");
    both_paths_agree(&bound, "type-available('xs:NCName')", "");
    both_paths_agree(
        &format!("xmlns:xs=\"{XSD}\" {xpath_default}"),
        "type-available('xsl:string')",
        "",
    );
    // An unbound prefix is XTDE1428 on both paths (X1 item 2, below); the
    // default type namespace does not rescue it.
    both_paths_raise(
        &xpath_default,
        "type-available('nope:string')",
        codes::XTDE1428,
    );
}

/// §5.1 lists `element-available` as one of only **three** places where "the
/// default namespace of the defining element is used when expanding an
/// unprefixed QName", and §18.2.2 repeats it: "If there is a default namespace
/// in scope, then it is used to expand an unprefixed QName." That is the
/// `xmlns=` default — *not* `xpath-default-namespace`, which §5.2 says does not
/// affect "strings that are interpreted as lexical QNames during stylesheet
/// evaluation".
#[test]
fn element_available_uses_the_default_namespace_of_the_defining_element() {
    // The default namespace of the defining element is the XSLT namespace, so
    // the unprefixed name of an XSLT instruction is available.
    both_paths_agree(&format!("xmlns=\"{NS}\""), "element-available('if')", "yes");
    // ... but a *declaration* is not an instruction (§18.2.2: "the function
    // returns true if and only if the expanded QName is the name of an XSLT
    // instruction").
    both_paths_agree(
        &format!("xmlns=\"{NS}\""),
        "element-available('stylesheet')",
        "",
    );
    // No default namespace: §18.2.2, "If the expanded-QName has a null namespace
    // URI, the element-available function will return false."
    both_paths_agree("", "element-available('if')", "");
    // `xpath-default-namespace` is not what this function uses.
    both_paths_agree(
        &format!("xpath-default-namespace=\"{NS}\""),
        "element-available('if')",
        "",
    );
    // A prefixed name is unaffected by either.
    both_paths_agree("", "element-available('xsl:if')", "yes");
    both_paths_agree("", "element-available('xsl:stylesheet')", "");
}

/// §18.1.1: "If the lexical QName is unprefixed, then the standard function
/// namespace is used" — neither default namespace touches it.
#[test]
fn function_available_always_uses_the_standard_function_namespace() {
    for decls in [
        String::new(),
        format!("xmlns=\"{NS}\""),
        format!("xpath-default-namespace=\"{NS}\""),
        "xpath-default-namespace=\"http://example.com/\"".to_string(),
    ] {
        both_paths_agree(&decls, "function-available('concat')", "yes");
        both_paths_agree(&decls, "function-available('no-such-function')", "");
    }
}

// ═══ `doc(())` in a `use-when` expression ═════════════════════════════════

/// \[Functions and Operators\] §15.5.4: `fn:doc(())` is the empty sequence — no
/// URI is looked up, so §3.12's empty set of available documents is never
/// consulted and there is no FODC0002 to raise. (F&O is not local; XQTS 1.0.2
/// `fn-doc-4` is `fn:count(fn:doc(()))` with the expected result `0`, and
/// `K-DocumentURIFunc-3` is `empty(document-uri(()))` = `true`.)
#[test]
fn an_empty_argument_looks_up_no_document() {
    for expr in [
        "empty(doc(()))",
        "count(doc(())) = 0",
        "not(doc-available(()))",
        "empty(document-uri(()))",
        "count(document-uri(())) = 0",
    ] {
        both_paths_agree("", expr, "yes");
    }
    // `document()` is XSLT-specific, so §3.12 keeps it out of the `use-when`
    // scope entirely; only the run-time half of the rule applies to it.
    assert_eq!(
        run(&sheet(
            r#"<xsl:template match="/"><xsl:if test="empty(document(()))"><xsl:text>yes</xsl:text></xsl:if></xsl:template>"#
        ))
        .unwrap(),
        "yes",
        "run-time document(())"
    );
    // A *non-empty* argument still finds nothing in a use-when expression, and
    // that is still FODC0002: the empty argument must not swallow the real case.
    let e = compile(&sheet(
        r#"<xsl:template match="doc" use-when="exists(doc('other.xsl'))"><out/></xsl:template>"#,
    ))
    .expect_err("a real URI in a use-when expression still resolves to nothing");
    assert_eq!(e.code, "FODC0002", "{e}");
    // An empty *string* is a URI, not an empty sequence, so it is FODC0002 too.
    let e = compile(&sheet(
        r#"<xsl:template match="doc" use-when="exists(doc(''))"><out/></xsl:template>"#,
    ))
    .expect_err("the zero-length URI is still a URI");
    assert_eq!(e.code, "FODC0002", "{e}");
}

// ═══ an argument that cannot be expanded is an error ══════════════════════
//
// Each of the four name-answering functions makes the same demand of its
// argument and gives it its own code:
//
// > \[§16.6.5\] \[ERR XTDE1390\] It is a non-recoverable dynamic error if the value
// > is not a valid QName, or if there is no namespace declaration in scope for
// > the prefix of the QName.
// >
// > \[§18.1.1\] \[ERR XTDE1400\] It is a non-recoverable dynamic error if the
// > argument does not evaluate to a string that is a valid QName, or if there is
// > no namespace declaration in scope for the prefix of the QName.
// >
// > \[§18.1.4\] \[ERR XTDE1428\] (the same sentence, for `type-available`)
// >
// > \[§18.2.2\] \[ERR XTDE1440\] (the same sentence, for `element-available`)
//
// Each code covers **both** failures — not a lexical QName, and an unbound
// prefix. §3.12 does not exempt a `use-when` expression: it excludes an element
// with "no static or dynamic errors … other than errors in the use-when
// attribute itself", and gives no code of its own for such an error, so the
// function's own code is reported, at compile time (each definition also lets
// the processor "optionally signal this as a static error" when it can detect
// it statically). The three `*-available` functions used to answer `false` for
// both failures on both paths, and `system-property` did so in `use-when`.

/// Both paths fail with `code`: the `use-when` guard when the stylesheet is
/// compiled, the `xsl:if` when it runs.
#[track_caller]
fn both_paths_raise(decls: &str, call: &str, code: &str) {
    let (statically, dynamically) = available_sheets(decls, call);
    match compile(&statically) {
        Ok(()) => panic!("use-when {call:?} with {decls:?}: expected {code}, but it compiled"),
        Err(e) => assert_eq!(e.code, code, "use-when {call:?} with {decls:?}: {e}"),
    }
    compile(&dynamically)
        .unwrap_or_else(|e| panic!("run-time {call:?} with {decls:?} must compile: {e}"));
    match run(&dynamically) {
        Ok(out) => panic!("run-time {call:?} with {decls:?}: expected {code}, got {out:?}"),
        Err(e) => assert_eq!(e.code, code, "run-time {call:?} with {decls:?}: {e}"),
    }
}

/// The four functions and their codes.
const NAME_FUNCTIONS: [(&str, &str); 4] = [
    ("system-property", codes::XTDE1390),
    ("function-available", codes::XTDE1400),
    ("type-available", codes::XTDE1428),
    ("element-available", codes::XTDE1440),
];

#[test]
fn an_unbound_prefix_is_an_error_on_both_paths() {
    for (function, code) in NAME_FUNCTIONS {
        both_paths_raise("", &format!("{function}('nope:x')"), code);
        // A prefix bound on *another* element is still unbound here (§5.4.1:
        // the in-scope namespaces are those "for the containing element").
        both_paths_raise("", &format!("{function}(concat('no', 'pe', ':x'))"), code);
    }
}

#[test]
fn an_argument_that_is_not_a_lexical_qname_is_an_error_on_both_paths() {
    // `1234` and `c#` are the suite's own examples (`error-1400a`,
    // `error-1440a`); `:x` is an empty prefix (`error-1390c` builds it with
    // `concat('', ':prop')`); the rest break `NCName (":" NCName)?` in the
    // remaining ways.
    for argument in ["1234", "c#", ":x", "x:", "a:b:c", "", "xsl:1x", "a b"] {
        for (function, code) in NAME_FUNCTIONS {
            both_paths_raise("", &format!("{function}('{argument}')"), code);
        }
    }
}

/// The negative controls: every name that *can* be expanded still answers, on
/// both paths, and XML whitespace around it is still tolerated as before.
#[test]
fn an_expandable_name_still_answers() {
    both_paths_agree("", "system-property('xsl:version') = '2.0'", "yes");
    both_paths_agree("", "system-property('nope') = ''", "yes");
    both_paths_agree("xmlns:p=\"urn:p\"", "system-property('p:x') = ''", "yes");
    both_paths_agree("", "function-available('concat')", "yes");
    both_paths_agree("xmlns:p=\"urn:p\"", "function-available('p:f')", "");
    both_paths_agree(
        "xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"",
        "type-available('xs:string')",
        "yes",
    );
    both_paths_agree("xmlns:p=\"urn:p\"", "type-available('p:t')", "");
    both_paths_agree("", "element-available('xsl:if')", "yes");
    both_paths_agree("xmlns:p=\"urn:p\"", "element-available('p:e')", "");
    both_paths_agree("", "element-available(' xsl:if ')", "yes");
}

/// §3.12: an element that is excluded takes its descendants with it, and "no
/// static or dynamic errors will be reported in respect of the element and its
/// contents, other than errors in the use-when attribute itself" — so a bad
/// argument in an excluded element's own descendants is never evaluated. And at
/// run time a dynamic error is "signaled only if the XPath expression is
/// actually evaluated" (§2.9).
#[test]
fn a_bad_argument_that_is_never_evaluated_is_not_an_error() {
    assert_eq!(
        run(&sheet(
            r#"<xsl:template match="/"><out>
                 <in xsl:use-when="false()">
                   <deeper xsl:use-when="function-available('nope:x')"/>
                 </in>
               </out></xsl:template>"#
        ))
        .unwrap(),
        "<out/>"
    );
    assert_eq!(
        run(&sheet(
            r#"<xsl:template match="/"><out>
                 <xsl:if test="false()"><xsl:value-of select="type-available('1234')"/></xsl:if>
               </out></xsl:template>"#
        ))
        .unwrap(),
        "<out/>"
    );
}
