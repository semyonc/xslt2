//! compatibility and residue: backwards-compatible attribute value
//! templates (XSLT 2.0 §5.6, §9.10 row 6 of the design), the default output
//! method of a 1.0 stylesheet (§20, row 11), §5.7.2's merging of adjacent text
//! nodes, extension instructions and `xsl:fallback` (§18.2), and
//! forwards-compatible processing (§3.9, §5.3).
//!
//! Every test goes through the public API — compile, transform,
//! `serialize_primary` — and cites the rule it checks.

mod common;

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, QName, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const XHTML: &str = "http://www.w3.org/1999/xhtml";

fn sheet_versioned(version: &str, body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"{version}\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:f=\"urn:f\" \
           exclude-result-prefixes=\"xs f\">{body}</xsl:stylesheet>"
    )
}

/// Where a run failed: at compile time or at run time, with the code.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Static(String),
    Dynamic(String),
}

/// Compiles `main` (with `modules` beside it), runs it over `source` from the
/// template named `main` when `initial` says so, and returns the serialized
/// primary result without the default XML declaration.
fn run_full(
    main: &str,
    modules: &[(&str, &str)],
    source: &str,
    initial: Option<&str>,
) -> Result<String, Failure> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    for (uri, text) in modules {
        mem.insert(*uri, *text);
    }
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::new(mem))
        .compile_str(main, "file:///compat/main.xsl")
        .map_err(|e| Failure::Static(e.code))?;
    let arena = bumpalo::Bump::new();
    let options = TransformOptions {
        initial_template: initial.map(QName::local),
        ..Default::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .transform_source("file:///compat/in.xml", source)
        .map_err(|e| Failure::Dynamic(e.code))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| Failure::Dynamic(e.code))?;
    Ok(common::body(text))
}

fn run_modules(main: &str, modules: &[(&str, &str)]) -> Result<String, Failure> {
    run_full(main, modules, "<doc><p>a<b/>c</p></doc>", None)
}

fn run(text: &str) -> Result<String, Failure> {
    run_modules(text, &[])
}

/// A stylesheet of `version` whose template matching `/` holds `body`.
fn template(version: &str, body: &str) -> String {
    sheet_versioned(
        version,
        &format!(r#"<xsl:template match="/">{body}</xsl:template>"#),
    )
}

fn ok(text: &str) -> String {
    run(text).unwrap_or_else(|f| panic!("{f:?}\n{text}"))
}

fn static_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Static(code)) => code,
        other => panic!("expected a static error, got {other:?}\n{text}"),
    }
}

fn dynamic_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Dynamic(code)) => code,
        other => panic!("expected a dynamic error, got {other:?}\n{text}"),
    }
}

/// The run-time error of `text` over `<doc><p>a<b/>c</p></doc>` (it must
/// compile), with its origin.
fn run_time_error(text: &str) -> XsltError {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, "file:///compat/main.xsl")
        .unwrap_or_else(|e| panic!("expected to compile, got {e}\n{text}"));
    let arena = bumpalo::Bump::new();
    let result = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///compat/in.xml", "<doc><p>a<b/>c</p></doc>")
        .map(|_| ());
    match result {
        Err(e) => e,
        Ok(()) => panic!("expected a run-time error\n{text}"),
    }
}

// ═══ §5.6 / J.1.3: attribute value templates with backwards-compatible behaviour ═══

/// §5.6: "If backwards compatible behavior is enabled for the attribute, the
/// rules for converting the value of the expression to a string are modified as
/// follows. After atomizing the result of the expression, all items other than
/// the first item in the resulting sequence are discarded, and the effective
/// value is obtained by converting the first item in the sequence to a string.
/// If the atomized sequence is empty, the result is a zero-length string."
/// (`avt-1203`.)
#[test]
fn a_backwards_compatible_avt_takes_the_first_item() {
    assert_eq!(
        ok(&template(
            "1.0",
            r#"<out test="{1, 2, 3, 4, 5}" empty="{()}" mixed="[{('a', 'b')}|{(7, 8)}]"/>"#
        )),
        r#"<out test="1" empty="" mixed="[a|7]"/>"#
    );
    // J.1.3: "under XSLT 2.0 all items in the sequence will be output, space
    // separated" (`avt-1204`).
    assert_eq!(
        ok(&template(
            "2.0",
            r#"<out test="{1, 2, 3, 4, 5}" empty="{()}"/>"#
        )),
        r#"<out test="1 2 3 4 5" empty=""/>"#
    );
}

/// §3.8: "An element that has an `[xsl:]version` attribute whose value is
/// greater than or equal to 2.0 disables backwards-compatible behavior for
/// itself, its attributes, its descendants and their attributes. The
/// compatibility behavior established by an element overrides any
/// compatibility behavior established by an ancestor element." The attribute
/// follows its own element (`backwards-010`, `xslt-compat-005`).
#[test]
fn a_nested_version_switches_the_rule_on_and_off() {
    // A 2.0 stylesheet; one literal result element enables it, one disables it.
    assert_eq!(
        ok(&template(
            "2.0",
            r#"<out><one xsl:version="1.0" a="{1 to 5}"><in a="{1 to 3}"/></one><two xsl:version="2.0" a="{1 to 5}"/></out>"#
        )),
        r#"<out><one a="1"><in a="1"/></one><two a="1 2 3 4 5"/></out>"#
    );
    // A 1.0 stylesheet with a 2.0 template, and a 2.0 element inside a 1.0 one.
    let text = sheet_versioned(
        "1.0",
        r#"<xsl:template match="/"><out><a v="{1 to 3}"/><xsl:call-template name="t"/><b xsl:version="2.0" v="{1 to 3}"><c xsl:version="1.0" v="{1 to 3}"/></b></out></xsl:template>
           <xsl:template name="t" version="2.0"><t v="{1 to 3}"/></xsl:template>"#,
    );
    assert_eq!(
        ok(&text),
        r#"<out><a v="1"/><t v="1 2 3"/><b v="1 2 3"><c v="1"/></b></out>"#
    );
}

/// The rule is about every attribute value template in the region (§5.6 "If
/// backwards compatible behavior is enabled for the attribute"), not only
/// those of literal result elements: `xsl:element` / `xsl:attribute`
/// `name`, `xsl:processing-instruction` `name`, `xsl:sort` `order`, a
/// `separator`.
#[test]
fn every_avt_of_the_region_takes_the_first_item() {
    let body = r#"<out>
        <xsl:element name="{('e1', 'e2')}"><xsl:attribute name="{('a1', 'a2')}" select="'v'"/></xsl:element>
        <xsl:processing-instruction name="{('pi', 'x')}">d</xsl:processing-instruction>
        <s><xsl:for-each select="3, 1, 2"><xsl:sort select="." order="{('descending', 'ascending')}"/><xsl:value-of select="."/></xsl:for-each></s>
        <v><xsl:value-of select="1 to 3" separator="{(',', ';')}"/></v>
      </out>"#;
    assert_eq!(
        ok(&template("1.0", body)),
        r#"<out><e1 a1="v"/><?pi d?><s>321</s><v>1,2,3</v></out>"#
    );
    // Without the rule `{('e1', 'e2')}` is the name `e1 e2`, which is not a
    // QName (XTDE0820).
    assert_eq!(
        dynamic_error(&template("2.0", r#"<xsl:element name="{('e1', 'e2')}"/>"#)),
        "XTDE0820"
    );
}

/// The first item is the first *atomized* item: §5.6's modified rule starts
/// "After atomizing the result of the expression", so two text nodes are not
/// merged first (XSLT 1.0 took the string value of the first node of a
/// node-set). Without the rule, §5.7.2 merges the adjacent text nodes, and an
/// element between them is an item of its own (its string value here is
/// empty, hence two spaces).
#[test]
fn the_first_item_is_taken_after_atomization() {
    // <p>a<b/>c</p>: two text nodes.
    assert_eq!(
        ok(&template(
            "1.0",
            r#"<out t="{doc/p/text()}" n="{doc/p/node()}"/>"#
        )),
        r#"<out t="a" n="a"/>"#
    );
    assert_eq!(
        ok(&template(
            "2.0",
            r#"<out t="{doc/p/text()}" n="{doc/p/node()}"/>"#
        )),
        r#"<out t="ac" n="a  c"/>"#
    );
}

// ═══ §5.7.2 Constructing simple content: adjacent text nodes ═══════════════

/// §5.7.2, which §5.6 applies to an AVT and §11.4.3 to `xsl:value-of`:
/// "Zero-length text nodes in the sequence are discarded. Adjacent text nodes
/// in the sequence are merged into a single text node. The sequence is
/// atomized." So a function returning the text nodes `[`, `0`, `]` makes `[0]`
/// (`avt-1205`), and the two text nodes of `<p>a<b/>c</p>`, adjacent in the
/// sequence, make `ac` — in an AVT, in `xsl:value-of select` and in
/// `xsl:attribute select`.
#[test]
fn adjacent_text_nodes_are_merged_before_atomization() {
    let text = sheet_versioned(
        "2.0",
        r#"<xsl:function name="f:t" as="text()*">
             <xsl:param name="p"/>
             <xsl:text/>[<xsl:value-of select="$p"/>]<xsl:text/>
           </xsl:function>
           <xsl:template match="/">
             <out avt="{f:t(0)}{f:t(1)}{f:t(2)}" mixed="{f:t(0), 7, f:t(1)}">
               <xsl:attribute name="sel" select="doc/p/text()"/>
               <v><xsl:value-of select="doc/p/text()"/></v>
               <w><xsl:value-of select="doc/p/text(), 'x', doc/p/text()" separator="|"/></w>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        ok(&text),
        r#"<out avt="[0][1][2]" mixed="[0] 7 [1]" sel="ac"><v>ac</v><w>ac|x|ac</w></out>"#
    );
}

/// §11.4.3: "Special rules apply when backwards compatible behavior is enabled
/// for the instruction. If no separator attribute is present, and if the
/// select attribute is present, then all items in the atomized result sequence
/// other than the first are ignored." With a `separator` the ordinary §5.7.2
/// rules apply, adjacent text nodes merged.
#[test]
fn backwards_compatible_value_of_takes_the_first_atomized_item() {
    assert_eq!(
        ok(&template(
            "1.0",
            r#"<out><v><xsl:value-of select="doc/p/text()"/></v><s><xsl:value-of select="doc/p/text(), 'x'" separator="|"/></s></out>"#
        )),
        "<out><v>a</v><s>ac|x</s></out>"
    );
}

// ═══ §20 / J.1.3: the default output method of a 1.0 stylesheet ═══════════

/// §20: "If the expanded-QName of this first element child has local part
/// `html` (in lower case), and namespace URI `http://www.w3.org/1999/xhtml`,
/// then the default output method is normally xhtml. However, if the version
/// attribute of the xsl:stylesheet element of the principal stylesheet module
/// has the value 1.0, and if the result tree is generated implicitly (rather
/// than by an explicit xsl:result-document instruction), then the default
/// output method in this situation is xml." J.1.3: "with backwards
/// compatibility enabled, the XML output method will be used."
///
/// The `xml` method writes `<br/>`; the `xhtml` method writes `<br />` and
/// adds the `meta` element.
#[test]
fn an_xhtml_result_of_a_1_0_principal_module_is_written_as_xml() {
    let xhtml = format!(r#"<html xmlns="{XHTML}"><head/><body><br/></body></html>"#);
    let xml_form = format!(r#"<html xmlns="{XHTML}"><head/><body><br/></body></html>"#);
    // The principal module is 1.0: `xml`, even from a 2.0 template.
    let one = sheet_versioned(
        "1.0",
        &format!(r#"<xsl:template match="/" version="2.0">{xhtml}</xsl:template>"#),
    );
    assert_eq!(ok(&one), xml_form);
    // A 2.0 principal module: `xhtml`, even from a 1.0 template.
    let two = sheet_versioned(
        "2.0",
        &format!(r#"<xsl:template match="/" version="1.0">{xhtml}</xsl:template>"#),
    );
    let written = ok(&two);
    assert!(written.contains("<br />"), "{written}");
    assert!(written.contains("<meta"), "{written}");
    // A 1.0 module included by a 2.0 principal module does not decide it.
    let included = sheet_versioned(
        "1.0",
        &format!(r#"<xsl:template match="/">{xhtml}</xsl:template>"#),
    );
    let main = sheet_versioned("2.0", r#"<xsl:include href="inc.xsl"/>"#);
    let written = run_modules(&main, &[("file:///compat/inc.xsl", &included)]).unwrap();
    assert!(written.contains("<br />"), "{written}");
    // An explicit xsl:result-document is not "generated implicitly".
    let explicit = sheet_versioned(
        "1.0",
        &format!(
            r#"<xsl:template match="/"><xsl:result-document version="2.0">{xhtml}</xsl:result-document></xsl:template>"#
        ),
    );
    let written = ok(&explicit);
    assert!(written.contains("<br />"), "{written}");
}

// ═══ §18.2 Extension instructions ══════════════════════════════════════════

/// §18.2.3: "When a processor performs fallback for an extension instruction
/// that is not recognized, if the instruction element has one or more
/// xsl:fallback children, then the content of each of the xsl:fallback children
/// must be evaluated" — in document order; §3.9's "Siblings of the xsl:fallback
/// elements are ignored" holds for the extension's own content too, which this
/// processor does not interpret (not even as XSLT).
#[test]
fn an_extension_instruction_performs_fallback() {
    let body = r#"<out xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext">
        <ext:magic select="$undeclared" any-attribute="{{">
          <lit>ignored</lit>
          <xsl:fallback><one/></xsl:fallback>
          <xsl:value-of select="$also-undeclared"/>
          <xsl:fallback><two/><xsl:value-of select="1 + 1"/></xsl:fallback>
        </ext:magic>
      </out>"#;
    assert_eq!(ok(&template("2.0", body)), "<out><one/><two/>2</out>");
}

/// §18.2.3: "it is a non-recoverable dynamic error if it has no xsl:fallback
/// children" (XTDE1450) — dynamic, so only when it is evaluated; §18.2.2: "An
/// implementation must not signal an error merely because the stylesheet
/// contains an extension instruction for which no implementation is
/// available." (`error-1450a`, `error-1450b`.)
#[test]
fn an_extension_instruction_without_fallback_fails_only_when_evaluated() {
    let never = r#"<out xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext">
        <xsl:if test="false()"><ext:magic/></xsl:if>ok</out>"#;
    assert_eq!(ok(&template("2.0", never)), "<out>ok</out>");
    let always =
        r#"<out xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext"><ext:magic/></out>"#;
    assert_eq!(dynamic_error(&template("2.0", always)), "XTDE1450");
    // Designated on xsl:stylesheet, reached through a named template.
    let text = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:my="urn:my"
             extension-element-prefixes="my">
             <xsl:template match="/"><xsl:call-template name="t"/></xsl:template>
             <xsl:template name="t"><my:inst/></xsl:template>
           </xsl:stylesheet>"#
    );
    assert_eq!(dynamic_error(&text), "XTDE1450");
}

/// §18.2.1: "The designation of a namespace as an extension namespace is
/// effective for the element bearing the `[xsl:]extension-element-prefixes`
/// attribute and for all descendants of that element within the same
/// stylesheet module." Outside it the same element is a literal result
/// element; `#default` designates the default namespace.
#[test]
fn the_designation_is_scoped() {
    let body = r#"<out xmlns:ext="urn:ext">
        <a xsl:extension-element-prefixes="ext"><ext:e><xsl:fallback>fb</xsl:fallback></ext:e></a>
        <b><ext:e><xsl:fallback>fb</xsl:fallback>lit</ext:e></b>
      </out>"#;
    assert_eq!(
        ok(&template("2.0", body)),
        r#"<out xmlns:ext="urn:ext"><a>fb</a><b><ext:e>lit</ext:e></b></out>"#
    );
    // An included module does not inherit the designation.
    let included = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:ext="urn:ext">
             <xsl:template name="t"><ext:e>lit</ext:e></xsl:template>
           </xsl:stylesheet>"#
    );
    let main = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:ext="urn:ext"
             extension-element-prefixes="ext" exclude-result-prefixes="ext">
             <xsl:include href="inc.xsl"/>
             <xsl:template match="/"><out><xsl:call-template name="t"/></out></xsl:template>
           </xsl:stylesheet>"#
    );
    assert_eq!(
        run_modules(&main, &[("file:///compat/inc.xsl", &included)]).unwrap(),
        r#"<out><ext:e xmlns:ext="urn:ext">lit</ext:e></out>"#
    );
    // `#default`.
    let default = r##"<out><e xmlns="urn:d" xsl:extension-element-prefixes="#default"><xsl:fallback>fb</xsl:fallback></e></out>"##;
    assert_eq!(ok(&template("2.0", default)), "<out>fb</out>");
}

/// §18.2.2: "If the expanded-QName is in the XSLT namespace, the function
/// returns true if and only if the expanded QName is the name of an XSLT
/// instruction"; otherwise "true if and only if the processor has an
/// implementation available of an extension instruction with the given
/// expanded QName. This applies whether or not the namespace has been
/// designated as an extension namespace." "If the expanded-QName has a null
/// namespace URI, the element-available function will return false."
#[test]
fn element_available_answers_for_instructions_only() {
    let body = r#"<out xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext"><xsl:value-of select="
        element-available('xsl:if'), element-available('xsl:fallback'),
        element-available('xsl:result-document'), element-available('xsl:number'),
        element-available('xsl:template'), element-available('xsl:sort'),
        element-available('xsl:when'), element-available('xsl:exciting-new-feature'),
        element-available('ext:magic'), element-available('out')"/></out>"#;
    assert_eq!(
        ok(&template("2.0", body)),
        "<out>true true true true false false false false false false</out>"
    );
    // In use-when, deciding between an extension instruction and its stand-in.
    let use_when = r#"<out xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext">
        <ext:magic xsl:use-when="element-available('ext:magic')"/>
        <alt xsl:use-when="not(element-available('ext:magic'))"/>
      </out>"#;
    assert_eq!(ok(&template("2.0", use_when)), "<out><alt/></out>");
}

// ═══ §3.9 Forwards-compatible processing ═══════════════════════════════════

/// §3.9: "if an element in the XSLT namespace appears as a child of the
/// xsl:stylesheet element, and XSLT 2.0 does not allow such elements to occur
/// as children of the xsl:stylesheet element, then the element and its content
/// must be ignored" — an unknown name, and an XSLT 2.0 instruction, whose own
/// `version` can enable the behaviour ("An element enables forwards-compatible
/// behavior for itself").
#[test]
fn an_unknown_top_level_element_is_ignored() {
    let declarations = r#"
        <xsl:exciting-new-declaration select="this is not XPath"><xsl:bogus/></xsl:exciting-new-declaration>
        <xsl:template match="/"><out>ok</out></xsl:template>"#;
    assert_eq!(ok(&sheet_versioned("3.0", declarations)), "<out>ok</out>");
    assert_eq!(
        static_error(&sheet_versioned("2.0", declarations)),
        "XTSE0010"
    );
    // An instruction where a declaration is expected, with its own version.
    let instruction = r#"<xsl:template match="/"><out>ok</out></xsl:template>
        <xsl:value-of select="$x" version="4.0"/>"#;
    assert_eq!(ok(&sheet_versioned("2.0", instruction)), "<out>ok</out>");
    assert_eq!(ok(&sheet_versioned("3.0", instruction)), "<out>ok</out>");
    // A version of 2.0 on the element disables it again.
    let disabled = r#"<xsl:template match="/"><out>ok</out></xsl:template>
        <xsl:value-of select="1" version="2.0"/>"#;
    assert_eq!(static_error(&sheet_versioned("3.0", disabled)), "XTSE0010");
}

/// §3.9: "if an element has an attribute that XSLT 2.0 does not allow the
/// element to have, then the attribute must be ignored" — on an XSLT element
/// and on a literal result element (`xsl:` attributes, XTSE0805 otherwise).
#[test]
fn an_unknown_attribute_is_ignored() {
    let text = r#"<xsl:template match="/" banana="custard"><out xsl:new-attribute="x"><xsl:value-of select="'ok'" flavour="vanilla"/></out></xsl:template>"#;
    assert_eq!(ok(&sheet_versioned("3.0", text)), "<out>ok</out>");
    assert_eq!(static_error(&sheet_versioned("2.0", text)), "XTSE0090");
    let lre = r#"<xsl:template match="/"><out xsl:new-attribute="x">ok</out></xsl:template>"#;
    assert_eq!(static_error(&sheet_versioned("2.0", lre)), "XTSE0805");
    // An attribute in the XSLT namespace on an XSLT element (XTSE0090, §3.5).
    let prefixed = r#"<xsl:template match="/" xsl:priority="1"><out>ok</out></xsl:template>"#;
    assert_eq!(ok(&sheet_versioned("3.0", prefixed)), "<out>ok</out>");
    assert_eq!(static_error(&sheet_versioned("2.0", prefixed)), "XTSE0090");
}

/// §3.9: "If the element has one or more xsl:fallback children, then no error
/// is reported either statically or dynamically, and the result of evaluating
/// the instruction is the concatenation of the sequences formed by evaluating
/// the sequence constructors within its xsl:fallback children, in document
/// order. Siblings of the xsl:fallback elements are ignored, even if they are
/// valid XSLT 2.0 instructions. If the element has no xsl:fallback children,
/// then a static error is reported in the same way as if forwards-compatible
/// behavior were not enabled."
#[test]
fn an_unknown_instruction_falls_back_or_is_a_static_error() {
    let with_fallback = r#"<out><xsl:future version="4.0">
        <wrong/><xsl:fallback><ok1/></xsl:fallback><xsl:value-of select="'wrong'"/><xsl:fallback><ok2/></xsl:fallback>
      </xsl:future></out>"#;
    assert_eq!(
        ok(&template("2.0", with_fallback)),
        "<out><ok1/><ok2/></out>"
    );
    // Without a fallback child: the static error, even in a branch that is
    // never evaluated.
    let without = r#"<out><xsl:if test="false()"><xsl:future version="4.0"/></xsl:if></out>"#;
    assert_eq!(static_error(&template("2.0", without)), "XTSE0010");
    // Without forwards-compatible behaviour the fallback does not help.
    let not_enabled = r#"<out><xsl:future><xsl:fallback/></xsl:future></out>"#;
    assert_eq!(static_error(&template("2.0", not_enabled)), "XTSE0010");
    // "XSLT 2.0 does not allow such elements to appear as part of a sequence
    // constructor" covers XSLT 2.0 elements that are not instructions too.
    let misplaced = r#"<out><a/><xsl:param name="p"><xsl:fallback>p</xsl:fallback></xsl:param><xsl:sort><xsl:fallback>s</xsl:fallback></xsl:sort><xsl:when test="true()"><xsl:fallback>w</xsl:fallback></xsl:when></out>"#;
    assert_eq!(ok(&template("3.0", misplaced)), "<out><a/>psw</out>");
    assert_eq!(static_error(&template("2.0", misplaced)), "XTSE0010");
}

/// §5.3: "Except where forwards-compatible behavior is enabled …, it is a
/// static error if the value of such an attribute, or the text between curly
/// brackets in an attribute value template, does not match the XPath
/// production Expr, or if it fails to satisfy other static constraints". So in
/// a forwards-compatible region such an expression fails only if it is
/// evaluated — with the error the compiler would have reported. A call to an
/// unknown function stays static (§18.1.1 note: "This is true even in a part
/// of the stylesheet that uses forwards-compatible behavior").
#[test]
fn an_xpath_static_error_is_deferred_in_a_forwards_compatible_region() {
    let never = r#"<out><xsl:if test="false()"><xsl:value-of select="1 +"/><a b="{$undeclared}"/></xsl:if>ok</out>"#;
    assert_eq!(ok(&template("3.0", never)), "<out>ok</out>");
    assert_eq!(static_error(&template("2.0", never)), "XPST0003");
    assert_eq!(
        dynamic_error(&template(
            "3.0",
            r#"<out><xsl:value-of select="1 +"/></out>"#
        )),
        "XPST0003"
    );
    assert_eq!(
        dynamic_error(&template("3.0", r#"<out a="{$undeclared}"/>"#)),
        "XPST0008"
    );
    // The region is lexical: a 2.0 element inside it reports statically.
    assert_eq!(
        static_error(&template(
            "3.0",
            r#"<out><xsl:if test="false()" version="2.0"><xsl:value-of select="1 +"/></xsl:if></out>"#
        )),
        "XPST0003"
    );
    // An unknown function is static wherever it is written.
    assert_eq!(
        static_error(&template(
            "3.0",
            r#"<out><xsl:if test="false()"><xsl:value-of select="f:nonexistent()"/></xsl:if></out>"#
        )),
        "XPST0017"
    );
}

/// J.1.4: "At XSLT 2.0 it returns "2.0" as a string" — whatever the region's
/// `version`.
#[test]
fn xsl_version_is_the_string_2_0_everywhere() {
    let body = r#"<out a="{system-property('xsl:version')}" n="{system-property('xsl:version') instance of xs:string}"/>"#;
    for version in ["1.0", "2.0", "3.0"] {
        assert_eq!(
            ok(&template(version, body)),
            r#"<out a="2.0" n="true"/>"#,
            "version {version}"
        );
    }
}

/// A named initial template: `error-1450b` starts from `main`.
#[test]
fn an_extension_instruction_in_the_initial_template() {
    let text = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:my="urn:my" extension-element-prefixes="my">
             <xsl:template name="main"><out><my:inst><xsl:fallback>fb</xsl:fallback></my:inst></out></xsl:template>
           </xsl:stylesheet>"#
    );
    assert_eq!(
        run_full(&text, &[], "<doc/>", Some("main")).unwrap(),
        "<out>fb</out>"
    );
}

// ═══ Deferred errors, misplaced elements, forwards-compatible processing ═══

/// A deferred static error (§5.3) is the processor's, and nothing a
/// stylesheet writes can produce one. §5.5.4: "Any dynamic error or type error
/// that occurs during the evaluation of a pattern against a particular node is
/// treated as a recoverable error … The optional recovery action is to treat
/// the pattern as not matching" — an `fn:error` the stylesheet raises is such an
/// error whatever its QName, even the namespace the processor once used for
/// deferred errors.
#[test]
fn a_stylesheet_cannot_forge_a_processor_error() {
    let forged = |code: &str| {
        format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:template match="/"><out><xsl:apply-templates select="doc/p"/></out></xsl:template>
                 <xsl:template match="p[error(QName('urn:x-xslt2:deferred-static-error', '{code}'), 'forged')]">matched</xsl:template>
                 <xsl:template match="p">fallback</xsl:template>
               </xsl:stylesheet>"#
        )
    };
    for code in ["XSLT2-NOTIMPL", "XSLT2-INTERNAL", "XTMM9000", "XPST0003"] {
        assert_eq!(ok(&forged(code)), "<out>fallback</out>", "{code}");
    }
    // Outside a pattern the error is the stylesheet's own.
    let raised = run_time_error(&template(
        "2.0",
        "<out><xsl:value-of select=\"error(QName('urn:x-xslt2:deferred-static-error', 'XSLT2-NOTIMPL'), 'forged')\"/></out>",
    ));
    assert!(raised.is_raised(), "{raised}");
    assert!(!raised.is_not_implemented(), "{raised}");
    // A deferred error is the processor's, with its own code, when evaluated.
    for (body, code) in [
        (r#"<out><xsl:value-of select="1 +"/></out>"#, "XPST0003"),
        (r#"<out a="{$undeclared}"/>"#, "XPST0008"),
    ] {
        let deferred = run_time_error(&template("3.0", body));
        assert_eq!(deferred.code, code);
        assert!(!deferred.is_raised(), "{deferred}");
    }
}

/// §3.9 for an `xsl:param` or `xsl:sort` that is a *direct* child of the
/// element whose leading `xsl:param` / `xsl:sort` children it would otherwise
/// be taken for. Only the leading run declares parameters (§9.2: "The
/// xsl:param elements must come before any other children") or sort keys
/// (§13: "xsl:sort elements must occur before any other children"); a later
/// one is part of the sequence constructor, where "If the element has one or
/// more xsl:fallback children, then no error is reported either statically or
/// dynamically, and the result of evaluating the instruction is the
/// concatenation of the sequences formed by evaluating the sequence
/// constructors within its xsl:fallback children … If the element has no
/// xsl:fallback children, then a static error is reported in the same way as if
/// forwards-compatible behavior were not enabled."
#[test]
fn a_misplaced_param_or_sort_falls_back() {
    // A template (`fc1`): leading params still declare, later ones fall back.
    let templates = r#"<xsl:template match="/">
          <out>A</out>
          <xsl:param name="p"><xsl:fallback>FB-param</xsl:fallback></xsl:param>
          <xsl:call-template name="t"/>
        </xsl:template>
        <xsl:template name="t">
          <xsl:param name="q" select="'Q'"/>
          <xsl:value-of select="$q"/>
          <xsl:param name="r"><xsl:fallback>FB-r</xsl:fallback></xsl:param>
        </xsl:template>"#;
    assert_eq!(
        ok(&sheet_versioned("3.0", templates)),
        "<out>A</out>FB-paramQFB-r"
    );
    assert_eq!(static_error(&sheet_versioned("2.0", templates)), "XTSE0010");
    // The misplaced element declares nothing: a caller passing `r` to a 2.0
    // template whose only `r` falls back is XTSE0680 (`fc2`).
    let undeclared = r#"<xsl:template match="/"><xsl:call-template name="t"><xsl:with-param name="r" select="'R'"/></xsl:call-template></xsl:template>
        <xsl:template name="t" version="3.0"><xsl:value-of select="'x'"/><xsl:param name="r"><xsl:fallback>FB-r</xsl:fallback></xsl:param></xsl:template>"#;
    assert_eq!(
        static_error(&sheet_versioned("2.0", undeclared)),
        "XTSE0680"
    );
    // A function (`fc7`).
    let function = r#"<xsl:function name="f:g">
          <xsl:sequence select="'G'"/>
          <xsl:param name="late"><xsl:fallback>FB-fparam</xsl:fallback></xsl:param>
        </xsl:function>
        <xsl:template match="/"><out><xsl:value-of select="f:g()" separator="+"/></out></xsl:template>"#;
    assert_eq!(
        ok(&sheet_versioned("3.0", function)),
        "<out>G+FB-fparam</out>"
    );
    // xsl:for-each, with and without `select` on the misplaced xsl:sort
    // (`fc6`, `fc3`), and xsl:for-each-group (`fc9`).
    for (body, expected) in [
        (
            r#"<out><xsl:for-each select="1,2,3"><xsl:value-of select="."/><xsl:sort><xsl:fallback>FB</xsl:fallback></xsl:sort></xsl:for-each></out>"#,
            "<out>1FB2FB3FB</out>",
        ),
        (
            r#"<out><xsl:for-each select="2,1"><xsl:sort select="."/><xsl:value-of select="."/><xsl:sort select="."><xsl:fallback>F</xsl:fallback></xsl:sort></xsl:for-each></out>"#,
            "<out>1F2F</out>",
        ),
        (
            r#"<out><xsl:for-each-group select="1,2,3" group-by="."><xsl:value-of select="."/><xsl:sort select="."><xsl:fallback>F</xsl:fallback></xsl:sort></xsl:for-each-group></out>"#,
            "<out>1F2F3F</out>",
        ),
        (
            r#"<out><xsl:perform-sort><xsl:sort select="."/><xsl:sequence select="'c', 'a', 'b'"/><xsl:sort><xsl:fallback>FB</xsl:fallback></xsl:sort></xsl:perform-sort></out>"#,
            // The fallback's text node sorts first ("FB" < "a"); §5.7.1 puts a
            // space only between adjacent atomic values.
            "<out>FBa b c</out>",
        ),
    ] {
        assert_eq!(ok(&template("3.0", body)), expected, "{body}");
        assert_eq!(static_error(&template("2.0", body)), "XTSE0010", "{body}");
    }
    // Without an xsl:fallback child: the static error, forwards-compatible or
    // not (`fc4`, `fc5`).
    let bare = r#"<out><xsl:for-each select="2,1"><xsl:value-of select="."/><xsl:sort select="."/></xsl:for-each></out>"#;
    assert_eq!(static_error(&template("3.0", bare)), "XTSE0010");
    let bare_param = r#"<xsl:template match="/"><out/><xsl:param name="p"/></xsl:template>"#;
    assert_eq!(
        static_error(&sheet_versioned("3.0", bare_param)),
        "XTSE0010"
    );
}

/// a call to a function that is not available stays a static error with
/// forwards-compatible behaviour (§18.1.1 note) even when the engine meets
/// another static error of the expression first — an undeclared variable or
/// prefix — which alone would be deferred (§5.3).
#[test]
fn an_unknown_function_is_not_hidden_by_a_deferred_error() {
    let unexecuted = |select: &str| {
        template(
            "3.0",
            &format!(
                r#"<out xmlns:u="urn:u"><xsl:if test="false()"><xsl:value-of select="{select}"/></xsl:if></out>"#
            ),
        )
    };
    assert_eq!(
        static_error(&unexecuted("u:unknown($undeclared)")),
        "XPST0017"
    );
    assert_eq!(static_error(&unexecuted("nope:unknown()")), "XPST0081");
    assert_eq!(static_error(&unexecuted("concat($undeclared)")), "XPST0017");
    // Every function resolves: the other error is deferred.
    assert_eq!(
        ok(&unexecuted("concat($undeclared, 'x')")),
        r#"<out xmlns:u="urn:u"/>"#
    );
    assert_eq!(ok(&unexecuted("nope:element")), r#"<out xmlns:u="urn:u"/>"#);
    // An expression that does not parse says nothing about its calls: deferred.
    assert_eq!(
        ok(&unexecuted("u:unknown(1 +)")),
        r#"<out xmlns:u="urn:u"/>"#
    );
}

/// §3.9 "the element and its content must be ignored" — so the standard
/// attributes of an ignored top-level element are not interpreted
/// (`exclude-result-prefixes`, `default-collation`, `extension-element-prefixes`,
/// `xpath-default-namespace`; a `version` that is not a number cannot enable
/// the behaviour, but one inherited from the stylesheet still does).
#[test]
fn an_ignored_top_level_element_is_not_read() {
    for element in ["value-of", "exciting-new-declaration"] {
        for attributes in [
            r#"exclude-result-prefixes="nope""#,
            r#"default-collation="urn:no-such-collation""#,
            r#"version="banana""#,
            r#"extension-element-prefixes="nope""#,
            r#"xpath-default-namespace="urn:x" select="1 +""#,
        ] {
            let body = format!(
                r#"<xsl:{element} {attributes}/><xsl:template match="/"><out/></xsl:template>"#
            );
            assert_eq!(ok(&sheet_versioned("3.0", &body)), "<out/>", "{body}");
        }
    }
    // Not ignored without forwards-compatible behaviour: the static error.
    let read = r#"<xsl:exciting-new-declaration exclude-result-prefixes="nope"/><xsl:template match="/"><out/></xsl:template>"#;
    assert_eq!(static_error(&sheet_versioned("2.0", read)), "XTSE0010");
}
