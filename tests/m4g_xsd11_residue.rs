//! M4g — XML Schema 1.1 as an option of the schema context, and the host-side workarounds for two
//! upstream gaps (`xslt_doc/M4_PLAN.md` §4 M4g; decision D-3: one XML Schema version per context,
//! 1.0 by default, 1.1 opt-in).
//!
//! - **XML Schema 1.1** (`SchemaContextBuilder::xsd_version`): XSLT 2.0 refers to XML Schema 1.0
//!   (its references: "XML Schema Part 1: Structures Second Edition"), so 1.0 stays the default; a
//!   1.1 context is an implementation-defined mechanism of §3.13 ("An implementation may define
//!   mechanisms that allow additional schema components to be added to the in-scope schema
//!   components"). In a 1.1 context schema documents are XML Schema 1.1 documents (`xs:assert`,
//!   `xs:alternative`), assertions are evaluated on validated sources and constructed trees, and
//!   the XPath 2.0 types XML Schema 1.1 defines are the set's own.
//! - **Upstream C72** (a workaround in M4g, gone since the Wave B follow-up): the XPath 2.0 types
//!   §3.13 puts in scope — `xs:untypedAtomic`, `xs:anyAtomicType`, `xs:dayTimeDuration`,
//!   `xs:yearMonthDuration` — named by `[xsl:]type` were validated by this crate against their
//!   definitions (XDM 1.0 §2.6.2) and annotated with the nearest type of an XML Schema 1.0 set, with
//!   a warning, the durations fenced. Upstream now has them in every set (`SchemaSet::xpath_type`):
//!   the tests below check the exact annotations in both versions (also `tests/wave_b_followup.rs`).
//! - **Upstream C89**: an element's `default` / `fixed` value of type `xs:QName` /
//!   `xs:NOTATION` whose namespace no prefix in scope binds is written with a prefix declared on
//!   the element, which every validation outside the one that supplied it reads (M4g held every
//!   start tag that might need it; since the Wave B follow-up only those whose value does, read
//!   with upstream's `element_value_constraint_for_type`).
//!
//! Every test goes through the public API and cites the rule it checks.

mod common;

use std::sync::Arc;

use bumpalo::Bump;

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsdVersion, XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4g/main.xsl";
const SOURCE: &str = "file:///m4g/doc.xml";
const XSD_URI: &str = "file:///m4g/m4g.xsd";
const XSD11_URI: &str = "file:///m4g/m4g11.xsd";

/// `urn:m4g`, valid in both versions; the schema document binds `p` to `urn:p`. Global elements:
/// `qd` (`xs:QName`, default `p:dvalue`), `qf` (`xs:QName`, fixed `p:fvalue`), `ql` (a list of
/// `xs:QName`, default `p:a p:b`), `cq` (simple content `xs:QName` with an attribute `n`, default
/// `p:cvalue`), `sd` (`xs:string`, default `text`), `qi` (simple content `xs:QName`, default `p:ivalue`,
/// an `xs:ID` attribute `id`), `wrap` (any number of `qd`, by reference),
/// `loose` (a local `lq`, `xs:QName`, default `p:lvalue`; its type `looseType` is named), `row`
/// (three typed children, an `xs:ID` attribute — the M4d table row), `table` (rows).
const M4G_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:m4g"
    xmlns:p="urn:p" targetNamespace="urn:m4g" elementFormDefault="qualified">
  <xs:element name="qd" type="xs:QName" default="p:dvalue"/>
  <xs:element name="qf" type="xs:QName" fixed="p:fvalue"/>
  <xs:simpleType name="qlist"><xs:list itemType="xs:QName"/></xs:simpleType>
  <xs:element name="ql" type="qlist" default="p:a p:b"/>
  <xs:element name="cq" default="p:cvalue">
    <xs:complexType>
      <xs:simpleContent>
        <xs:extension base="xs:QName"><xs:attribute name="n" type="xs:integer"/></xs:extension>
      </xs:simpleContent>
    </xs:complexType>
  </xs:element>
  <xs:element name="sd" type="xs:string" default="text"/>
  <xs:element name="qi" default="p:ivalue">
    <xs:complexType>
      <xs:simpleContent>
        <xs:extension base="xs:QName"><xs:attribute name="id" type="xs:ID"/></xs:extension>
      </xs:simpleContent>
    </xs:complexType>
  </xs:element>
  <xs:element name="wrap">
    <xs:complexType>
      <xs:sequence><xs:element ref="qd" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:complexType name="looseType">
    <xs:sequence>
      <xs:element name="lq" type="xs:QName" default="p:lvalue" minOccurs="0"/>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="loose" type="looseType"/>
</xs:schema>"#;

/// `urn:m4g11`, XML Schema 1.1 only: `range` (integer attributes `lo`, `hi`, asserting `@lo le
/// @hi`), `pick` (type alternatives: `kind="n"` gives it `numberPick`, `xs:integer` content,
/// otherwise `textPick`, `xs:string` content).
const M4G11_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:m4g11"
    targetNamespace="urn:m4g11" elementFormDefault="qualified">
  <xs:element name="range">
    <xs:complexType>
      <xs:attribute name="lo" type="xs:integer"/>
      <xs:attribute name="hi" type="xs:integer"/>
      <xs:assert test="@lo le @hi"/>
    </xs:complexType>
  </xs:element>
  <xs:complexType name="numberPick">
    <xs:simpleContent>
      <xs:extension base="xs:integer"><xs:attribute name="kind" type="xs:string"/></xs:extension>
    </xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="textPick">
    <xs:simpleContent>
      <xs:extension base="xs:string"><xs:attribute name="kind" type="xs:string"/></xs:extension>
    </xs:simpleContent>
  </xs:complexType>
  <xs:element name="pick">
    <xs:alternative test="@kind = 'n'" type="numberPick"/>
    <xs:alternative type="textPick"/>
  </xs:element>
</xs:schema>"#;

/// A stylesheet importing `urn:m4g` (and, with `v11`, `urn:m4g11`), with `top` as its top-level
/// content. `g` is `urn:m4g`, `h` is `urn:m4g11`; `p` is not declared.
fn sheet(v11: bool, top: &str) -> String {
    let import11 = if v11 {
        r#"<xsl:import-schema namespace="urn:m4g11" schema-location="m4g11.xsd"/>"#
    } else {
        ""
    };
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:g=\"urn:m4g\" xmlns:h=\"urn:m4g11\" \
           xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
           exclude-result-prefixes=\"xs g h xsi\">\
           <xsl:import-schema namespace=\"urn:m4g\" schema-location=\"m4g.xsd\"/>{import11}\
           {top}</xsl:stylesheet>"
    )
}

fn resolver(main: &str) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, M4G_XSD);
    mem.insert(XSD11_URI, M4G11_XSD);
    Arc::new(mem)
}

/// What a run gave: the serialized primary result, or the error, and the run's warnings.
struct Ran {
    result: Result<String, XsltError>,
    warnings: Vec<String>,
}

/// Schema-aware, the context built as `version` says: compiles `main` and runs it over `source`,
/// validated as `validation` says.
fn run_with(version: XsdVersion, main: &str, source: &str, validation: SourceValidation) -> Ran {
    let resolver = resolver(main);
    let functions = XsltFunctions::new();
    let context = match SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .xsd_version(version)
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
    {
        Ok(context) => context,
        Err(e) => {
            return Ran {
                result: Err(e),
                warnings: Vec::new(),
            }
        }
    };
    let compiled = match Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
    {
        Ok(compiled) => compiled,
        Err(e) => {
            return Ran {
                result: Err(e),
                warnings: Vec::new(),
            }
        }
    };
    let arena = Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let result = transformation
        .transform_source_with(SOURCE, source, validation)
        .and_then(|outcome| compiled.serialize_primary(&outcome))
        .map(common::body);
    Ran {
        result,
        warnings: transformation.take_warnings(),
    }
}

fn v10() -> XsdVersion {
    XsdVersion::V1_0
}

fn v11() -> XsdVersion {
    XsdVersion::V1_1
}

/// The text of `<out>…</out>` the template matching `/` writes: the declarations `top` (variables)
/// in the template, then `<xsl:value-of select="{body}"/>`.
fn answers(version: XsdVersion, top: &str, body: &str) -> String {
    let main = sheet(
        version == v11(),
        &format!(
            r#"<xsl:template match="/">{top}<out><xsl:value-of select="{body}"/></out></xsl:template>"#
        ),
    );
    match run_with(version, &main, "<src/>", SourceValidation::Skip).result {
        Ok(text) => inner(&text),
        Err(e) => panic!("the run failed: {e}\n{top}\n{body}"),
    }
}

/// The serialized result of the template matching `/` with `content`.
fn result(version: XsdVersion, content: &str) -> String {
    let main = sheet(
        version == v11(),
        &format!(r#"<xsl:template match="/">{content}</xsl:template>"#),
    );
    match run_with(version, &main, "<src/>", SourceValidation::Skip).result {
        Ok(text) => text,
        Err(e) => panic!("the run failed: {e}\n{content}"),
    }
}

/// Every `(content, code)` raises `code`; the ones that do not are listed together.
fn assert_codes(version: XsdVersion, cases: &[(&str, &str)]) {
    let mut wrong = Vec::new();
    for (content, code) in cases {
        let main = sheet(
            version == v11(),
            &format!(r#"<xsl:template match="/">{content}</xsl:template>"#),
        );
        match run_with(version, &main, "<src/>", SourceValidation::Skip).result {
            Ok(text) => wrong.push(format!(
                "{content}\n    expected {code}, got the result {text}"
            )),
            Err(e) if e.code == *code => {}
            Err(e) => wrong.push(format!("{content}\n    expected {code}, got {e}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn inner(text: &str) -> String {
    let text = text.trim();
    if text == "<out/>" {
        return String::new();
    }
    text.strip_prefix("<out>")
        .and_then(|t| t.strip_suffix("</out>"))
        .unwrap_or_else(|| panic!("not an <out> element: {text}"))
        .trim()
        .to_string()
}

// ══ XML Schema 1.1 as an option of the context (D-3) ═══════════════════════════════════════════

/// D-3: one version per context, **1.0 by default** — XSLT 2.0's normative reference is "XML
/// Schema Part 1: Structures Second Edition" (XML Schema 1.0). A context built without the option
/// is 1.0, and an XML Schema 1.1 construct in an imported schema document is a schema error there:
/// XTSE0220, "the synthetic schema document does not satisfy the constraints described in [XML
/// Schema Part 1]".
#[test]
fn a_context_is_xml_schema_1_0_unless_asked() {
    let functions = XsltFunctions::new();
    let main = sheet(false, "");
    let resolver = resolver(&main);
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .expect("a 1.0 schema loads");
    assert_eq!(context.xsd_version(), XsdVersion::V1_0);
    assert!(!context.schema_set().is_xsd11());
    assert_eq!(SchemaContext::new().xsd_version(), XsdVersion::V1_0);
    let main11 = sheet(true, "");
    let e = run_with(v10(), &main11, "<src/>", SourceValidation::Skip)
        .result
        .expect_err("xs:assert in a 1.0 context");
    assert_eq!(e.code, codes::XTSE0220, "{e}");
}

/// The option: `SchemaContextBuilder::xsd_version(XsdVersion::V1_1)` builds the context's one set
/// as an XML Schema 1.1 set (`SchemaSet::xsd11()`): its schema documents are 1.1 documents, and its
/// built-in types are 1.1's, which include the XPath 2.0 types §3.13 lists (XML Schema 1.1 Part 2
/// §3.2.2 `anyAtomicType`, §3.4.26 `yearMonthDuration`, §3.4.27 `dayTimeDuration`).
#[test]
fn an_xsd11_context_loads_1_1_schema_documents() {
    let functions = XsltFunctions::new();
    let main = sheet(true, "");
    let resolver = resolver(&main);
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .xsd_version(XsdVersion::V1_1)
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .expect("a 1.1 schema loads in a 1.1 context");
    assert_eq!(context.xsd_version(), XsdVersion::V1_1);
    assert!(context.schema_set().is_xsd11());
    assert_eq!(
        answers(
            v11(),
            "",
            "xs:dayTimeDuration('PT1H') instance of xs:duration, \
             'PT1H' castable as xs:dayTimeDuration, type-available('xs:dateTimeStamp')"
        ),
        "true true true"
    );
    // §3.13's "All other built-in types defined in [XML Schema Part 2]": in a 1.1 context that is
    // XML Schema 1.1's Part 2, whose `xs:dateTimeStamp` XSLT 2.0's list (and a 1.0 context) lacks.
    assert_eq!(
        answers(v10(), "", "type-available('xs:dateTimeStamp')"),
        "false"
    );
    assert_eq!(
        answers(
            v11(),
            r#"<xsl:variable name="a" as="attribute()"><xsl:attribute name="t" type="xs:dateTimeStamp">2026-10-24T10:00:00Z</xsl:attribute></xsl:variable>"#,
            "$a instance of attribute(*, xs:dateTimeStamp)"
        ),
        "true"
    );
}

/// Assertions on a validated source (XML Schema 1.1 Part 1 §3.4.4.2 clause 6, "Assertion
/// Satisfied"; upstream B9: `ValidationFlags::PROCESS_ASSERTIONS`): in a 1.1 context a source whose
/// element breaks its type's `xs:assert` is invalid — the principal source's loader code, XTDE1170
/// —, and a valid one is typed.
#[test]
fn assertions_are_evaluated_on_validated_sources_in_1_1() {
    let main = sheet(
        true,
        r#"<xsl:template match="/"><out><xsl:value-of select="h:range/@lo instance of attribute(*, xs:integer)"/></out></xsl:template>"#,
    );
    let ok = run_with(
        v11(),
        &main,
        r#"<range xmlns="urn:m4g11" lo="1" hi="2"/>"#,
        SourceValidation::Strict,
    );
    assert_eq!(ok.result.expect("valid"), "<out>true</out>");
    let bad = run_with(
        v11(),
        &main,
        r#"<range xmlns="urn:m4g11" lo="3" hi="2"/>"#,
        SourceValidation::Strict,
    )
    .result
    .expect_err("the assertion fails");
    assert_eq!(bad.code, codes::XTDE1170, "{bad}");
}

/// Assertions on constructed elements (§19.2.1.3: the validation of a constructed element is
/// "equivalent to" serializing, parsing and validating it — in a 1.1 context, XML Schema 1.1
/// validation, assertions included): `strict` (XTTE1510) and `type` (XTTE1540) — the assertion
/// failing — and both valid when it holds; under an outer validation too.
#[test]
fn assertions_are_evaluated_on_constructed_elements_in_1_1() {
    assert_eq!(
        answers(
            v11(),
            r#"<xsl:variable name="a" as="element()"><h:range xsl:validation="strict" lo="1" hi="2"/></xsl:variable>
               <xsl:variable name="b" as="element()"><xsl:element name="h:range" validation="strict"><xsl:attribute name="lo">5</xsl:attribute><xsl:attribute name="hi">5</xsl:attribute></xsl:element></xsl:variable>"#,
            "data($a/@hi) - data($a/@lo), $b/@lo instance of attribute(*, xs:integer)"
        ),
        "1 true"
    );
    assert_codes(
        v11(),
        &[
            (
                r#"<out><h:range xsl:validation="strict" lo="3" hi="2"/></out>"#,
                codes::XTTE1510,
            ),
            (
                r#"<out><xsl:element name="h:range" validation="lax"><xsl:attribute name="lo">3</xsl:attribute><xsl:attribute name="hi">2</xsl:attribute></xsl:element></out>"#,
                codes::XTTE1515,
            ),
            (
                r#"<out><xsl:variable name="v"><h:range lo="3" hi="2"/></xsl:variable><xsl:copy-of select="$v/h:range" validation="strict"/></out>"#,
                codes::XTTE1510,
            ),
        ],
    );
}

/// Type alternatives (XML Schema 1.1 Part 1 §3.3.2.1, conditional type assignment) on a
/// constructed element, as `validation-1301` uses them: the governing type follows the attributes.
#[test]
fn type_alternatives_govern_constructed_elements_in_1_1() {
    assert_eq!(
        answers(
            v11(),
            r#"<xsl:variable name="n" as="element()"><h:pick xsl:validation="strict" kind="n"> 7 </h:pick></xsl:variable>
               <xsl:variable name="t" as="element()"><h:pick xsl:validation="strict" kind="t"> 7 </h:pick></xsl:variable>"#,
            "data($n) + 1, $n instance of element(*, h:numberPick), \
             $t instance of element(*, h:textPick), concat('[', string($t), ']')"
        ),
        "8 true true [ 7 ]"
    );
    assert_codes(
        v11(),
        &[(
            r#"<out><h:pick xsl:validation="strict" kind="n">seven</h:pick></out>"#,
            codes::XTTE1510,
        )],
    );
}

/// In a 1.1 context the XPath 2.0 types of §3.13 are the set's own: `[xsl:]type` naming them
/// validates against them and annotates with them (XDM 1.0 §2.6.2; XML Schema 1.1 Part 2 §3.4.26,
/// §3.4.27, §3.2.2), so `instance of`, `data()` and the typed value answer as XPath defines them —
/// no warning. `validation-0108` / `-0109`'s `xs:untypedAtomic` on an element included.
#[test]
fn xpath_types_are_the_sets_own_in_1_1() {
    let main = sheet(
        true,
        r#"<xsl:template match="/">
             <xsl:variable name="d" as="element()"><z xsl:type="xs:dayTimeDuration"> PT1H </z></xsl:variable>
             <xsl:variable name="y" as="element()"><z xsl:type="xs:yearMonthDuration">P1Y2M</z></xsl:variable>
             <xsl:variable name="a" as="element()"><z xsl:type="xs:anyAtomicType">12</z></xsl:variable>
             <xsl:variable name="u" as="node()*">
               <xsl:attribute name="a" select="'abcd'" type="xs:untypedAtomic"/>
               <z xsl:type="xs:untypedAtomic">abcd</z>
             </xsl:variable>
             <xsl:variable name="at" as="attribute()"><xsl:attribute name="t" type="xs:dayTimeDuration">PT2H</xsl:attribute></xsl:variable>
             <out><xsl:value-of select="$d instance of element(*, xs:dayTimeDuration), data($d) lt xs:dayTimeDuration('PT2H'),
               $y instance of element(*, xs:yearMonthDuration), data($y) instance of xs:yearMonthDuration,
               $a instance of element(*, xs:anyAtomicType), data($a) instance of xs:untypedAtomic,
               $u[1] instance of attribute(*, xs:untypedAtomic), $u[2] instance of element(*, xs:untypedAtomic),
               data($u[2]) instance of xs:untypedAtomic, string($u[2]),
               $at instance of attribute(*, xs:dayTimeDuration)"/></out>
           </xsl:template>"#,
    );
    let ran = run_with(v11(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(
        inner(&ran.result.expect("valid")),
        "true true true true true true true true true abcd true"
    );
    assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
    assert_codes(
        v11(),
        &[
            (
                r#"<z xsl:type="xs:untypedAtomic">abcd<a/>wxyz</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:dayTimeDuration">P1Y</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:yearMonthDuration">P1D</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<out><xsl:attribute name="t" type="xs:yearMonthDuration">PT1H</xsl:attribute></out>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

// ══ Upstream C72: XPath 2.0 types in an XML Schema 1.0 set (the set's own since Wave B) ════════

/// `xs:untypedAtomic` on an element in a 1.0 set (`validation-0108`, `-0109`). XDM 1.0 §2.6.2:
/// "The datatype xs:untypedAtomic denotes untyped atomic data, such as text that has not been
/// assigned a more specific type"; every string is a value of it (§2.6.4: "When xs:untypedAtomic
/// is specified as the type, no validation takes place"). As a simple type it admits no element
/// children and no attributes but `xsi:*` (XML Schema 1.0 Part 1 §3.3.4, Element Locally Valid
/// (Type) clause 3.1): those are XTTE1540. The element is annotated `xs:untypedAtomic` (upstream's
/// component of the 1.0 set, `SchemaSet::xpath_type`; M4g's workaround annotated it
/// `xs:anySimpleType`, with a warning): its typed value is its string value as `xs:untypedAtomic`,
/// it is no `xs:untyped` element, and it is an `xs:untypedAtomic`, `xs:anyAtomicType` and
/// `xs:anySimpleType` one (XDM 1.0 §2.6.2: "xs:untypedAtomic" is derived from `xs:anyAtomicType`,
/// whose "base type is xs:anySimpleType"). No warning.
#[test]
fn untyped_atomic_on_an_element_in_1_0() {
    let main = sheet(
        false,
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="node()*">
               <xsl:attribute name="a" select="'abcd'" type="xs:untypedAtomic"/>
               <z xsl:type="xs:untypedAtomic"> ab  cd </z>
             </xsl:variable>
             <out><xsl:value-of select="$v[1] instance of attribute(*, xs:untypedAtomic),
               $v[2] instance of element(*, xs:anySimpleType), $v[2] instance of element(*, xs:untyped),
               data($v[2]) instance of xs:untypedAtomic, concat('[', data($v[2]), ']'),
               $v[2] instance of element(*, xs:untypedAtomic), $v[2] instance of element(*, xs:anyAtomicType)"/></out>
           </xsl:template>"#,
    );
    let ran = run_with(v10(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(
        inner(&ran.result.expect("valid")),
        "true true false true [ ab  cd ] true true"
    );
    assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
    assert_codes(
        v10(),
        &[
            (
                r#"<z xsl:type="xs:untypedAtomic">abcd<a/>wxyz</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:untypedAtomic" a="1">abcd</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<out><xsl:element name="z" type="xs:untypedAtomic"><y/></xsl:element></out>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// `xs:dayTimeDuration` / `xs:yearMonthDuration` in a 1.0 context. M4g fenced them (`XSLT2-NOTIMPL`
/// naming upstream proposal C72; decision D-g2 as the user revised it, review R3): a 1.0 set had no
/// definition of either type, and the nearest one, `xs:duration`, would have given an unordered
/// typed value. Upstream's 1.0 set has both now (`SchemaSet::xpath_type`), so every construct the
/// fence covered — element, attribute, copy, document, and an `xsi:type` naming one under
/// `xs:anyAtomicType` — validates against the type itself; the typed values answer as XPath defines
/// them (`tests/wave_b_followup.rs`, `durations_answer_as_xpath_defines_them_in_1_0`).
#[test]
fn durations_are_the_sets_own_in_1_0() {
    let ns = r#"<xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>"#;
    let src = r#"<xsl:variable name="s"><z>PT1H</z><w t="PT1H"/></xsl:variable>"#;
    let cases = [
        r#"<z xsl:type="xs:dayTimeDuration">PT1H</z>"#.to_string(),
        r#"<z xsl:type="xs:yearMonthDuration">P1Y</z>"#.to_string(),
        r#"<xsl:element name="z" type="xs:dayTimeDuration">PT1H</xsl:element>"#.to_string(),
        r#"<out><xsl:attribute name="t" type="xs:dayTimeDuration">PT1H</xsl:attribute></out>"#
            .to_string(),
        r#"<out><xsl:attribute name="t" type="xs:yearMonthDuration">P1Y</xsl:attribute></out>"#
            .to_string(),
        format!(r#"{src}<out><xsl:copy-of select="$s/z" type="xs:dayTimeDuration"/></out>"#),
        format!(r#"{src}<out><xsl:copy-of select="$s/w/@t" type="xs:dayTimeDuration"/></out>"#),
        format!(
            r#"{src}<out><xsl:for-each select="$s/z"><xsl:copy type="xs:dayTimeDuration">PT1H</xsl:copy></xsl:for-each></out>"#
        ),
        r#"<out><xsl:document type="xs:yearMonthDuration"><z>P1Y</z></xsl:document></out>"#
            .to_string(),
        format!(r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:dayTimeDuration">{ns}PT1H</z>"#),
        format!(r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:yearMonthDuration">{ns}P1Y</z>"#),
    ];
    let mut wrong = Vec::new();
    for content in &cases {
        let main = sheet(
            false,
            &format!(r#"<xsl:template match="/">{content}</xsl:template>"#),
        );
        let ran = run_with(v10(), &main, "<src/>", SourceValidation::Skip);
        match ran.result {
            Ok(_) if ran.warnings.is_empty() => {}
            other => wrong.push(format!("{content}\n    got {other:?}, {:?}", ran.warnings)),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The working path for the two durations: an XML Schema 1.1 context, whose set has both types
/// (Datatypes 1.1 §3.4.26, §3.4.27). Every construct the 1.0 context fences validates there, with
/// the exact annotation and no warning, and the typed value answers as XPath 2.0 defines it —
/// ordered, with arithmetic, `sum` / `max` and `xsl:sort` (the answers the review's R3 table gives
/// as the right ones), and a typed pattern naming the type matches.
#[test]
fn durations_are_native_in_1_1() {
    let d = r#"<xsl:variable name="d" as="element()*"><z xsl:type="xs:dayTimeDuration">PT1H</z><z xsl:type="xs:dayTimeDuration">PT3H</z><z xsl:type="xs:dayTimeDuration">PT2H</z></xsl:variable>"#;
    let y = r#"<xsl:variable name="y" as="element()*"><z xsl:type="xs:yearMonthDuration">P1Y</z><z xsl:type="xs:yearMonthDuration">P2M</z></xsl:variable>"#;
    let rest = r#"<xsl:variable name="e" as="element()"><xsl:element name="z" type="xs:yearMonthDuration"> P1Y2M </xsl:element></xsl:variable>
                  <xsl:variable name="a" as="attribute()"><xsl:attribute name="t" type="xs:dayTimeDuration">P2D</xsl:attribute></xsl:variable>
                  <xsl:variable name="src"><z>PT3M</z><w t="PT5M"/></xsl:variable>
                  <xsl:variable name="c" as="element()"><xsl:copy-of select="$src/z" type="xs:dayTimeDuration"/></xsl:variable>
                  <xsl:variable name="ca" as="attribute()"><xsl:copy-of select="$src/w/@t" type="xs:dayTimeDuration"/></xsl:variable>
                  <xsl:variable name="doc" as="document-node()"><xsl:document type="xs:yearMonthDuration"><z>P3M</z></xsl:document></xsl:variable>"#;
    let top = format!("{d}{y}{rest}");
    let main = sheet(
        true,
        &format!(
            r#"<xsl:template match="/">{top}<out><xsl:value-of select="data($d[1]) instance of xs:dayTimeDuration,
                 data($d[1]) lt xs:dayTimeDuration('PT2H'), data($d[1]) + xs:dayTimeDuration('PT1H'),
                 data($y[1]) + data($y[2]), xs:date('2020-01-01') + data($y[1]), sum($d), max($d),
                 string($e), $e instance of element(*, xs:yearMonthDuration),
                 data($a) instance of xs:dayTimeDuration, data($a) gt xs:dayTimeDuration('PT1H'),
                 $c instance of element(*, xs:dayTimeDuration), $ca instance of attribute(*, xs:dayTimeDuration),
                 $doc/z instance of element(*, xs:yearMonthDuration)"/></out></xsl:template>"#
        ),
    );
    let ran = run_with(v11(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(
        inner(&ran.result.expect("valid")),
        "true true PT2H P1Y2M 2021-01-01 PT6H PT3H P1Y2M true true true true true true"
    );
    assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
    let sort = format!(
        r#"{d}<out><xsl:for-each select="$d"><xsl:sort select="."/><xsl:value-of select="."/><xsl:text> </xsl:text></xsl:for-each></out>"#
    );
    assert_eq!(inner(&result(v11(), &sort)), "PT1H PT2H PT3H");
    let patterns = sheet(
        true,
        &format!(
            r#"<xsl:template match="/">{d}{y}<out><xsl:apply-templates select="$d[1], $y[1]"/></out></xsl:template>
               <xsl:template match="element(*, xs:dayTimeDuration)" priority="2">[dt]</xsl:template>
               <xsl:template match="element(*, xs:yearMonthDuration)" priority="2">[ym]</xsl:template>
               <xsl:template match="element(*, xs:duration)">[duration]</xsl:template>"#
        ),
    );
    let ran = run_with(v11(), &patterns, "<src/>", SourceValidation::Skip);
    assert_eq!(inner(&ran.result.expect("runs")), "[dt][ym]");
    assert_codes(
        v11(),
        &[
            (
                r#"<z xsl:type="xs:dayTimeDuration">P1M</z>"#,
                codes::XTTE1540,
            ),
            (r#"<z xsl:type="xs:dayTimeDuration">P</z>"#, codes::XTTE1540),
            (
                r#"<z xsl:type="xs:yearMonthDuration">P1YT1H</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<xsl:variable name="s"><z t="P1Y"/></xsl:variable><out><xsl:copy-of select="$s/z/@t" type="xs:dayTimeDuration"/></out>"#,
                codes::XTTE1540,
            ),
            (
                r#"<out><xsl:document type="xs:dayTimeDuration"><z>P1Y</z></xsl:document></out>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// `xs:anyAtomicType` in a 1.0 set. XDM 1.0 §2.6.2: "an atomic type that includes all atomic
/// values (and no values that are not atomic)"; XML Schema 1.1 Part 2 §3.2.2.2: its lexical space
/// is every string of XML characters. Every string is valid, an element child or an attribute is
/// not (a simple type); annotated `xs:anyAtomicType` (upstream's component of the 1.0 set; M4g
/// annotated its stand-in `xs:anySimpleType`, from which it derives, so the `xs:anySimpleType` tests
/// below are true either way), typed value `xs:untypedAtomic` (XDM 1.0 §3.3.1.2). An `xsi:type`
/// naming an atomic type is honoured (Structures clause 1.2.1.2.4: an `xsi:type` validly derived
/// from the stipulated type replaces it), one naming a list type is XTTE1540. (`s` is bound by
/// `xsl:namespace`, as in the next test.)
#[test]
fn any_atomic_type_in_1_0() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="e" as="element()"><z xsl:type="xs:anyAtomicType">12</z></xsl:variable>
               <xsl:variable name="i" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:integer"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace> 12 </z></xsl:variable>
               <xsl:variable name="a" as="attribute()"><xsl:attribute name="t" type="xs:anyAtomicType">x</xsl:attribute></xsl:variable>"#,
            "$e instance of element(*, xs:anySimpleType), data($e) instance of xs:untypedAtomic, \
             $i instance of element(*, xs:integer), data($i) + 1, \
             $a instance of attribute(*, xs:anySimpleType), data($a) instance of xs:untypedAtomic"
        ),
        "true true true 13 true true"
    );
    assert_codes(
        v10(),
        &[
            (
                r#"<z xsl:type="xs:anyAtomicType"><y/></z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:NMTOKENS"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>a b</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:integer"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>x</z>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// An `xsi:type` on an element validated against `xs:untypedAtomic` in a 1.0 set (upstream's
/// component since the Wave B follow-up, M4g's stand-in before): Structures
/// clause 1.2.1.2.4 lets it replace the stipulated type only when "validly derived from that type
/// definition"; no type is derived from `xs:untypedAtomic` ("No predefined schema types are derived
/// from xs:untypedAtomic", XPath 2.0 §2.5.1), so any other `xsi:type` is XTTE1540, while one naming
/// the type itself changes nothing. (The `xsi:type` values use `s`, bound by `xsl:namespace`: the
/// stylesheet excludes the XML Schema namespace from its result elements,
/// `exclude-result-prefixes="xs"`, which §11.1.3 applies to every prefix bound to that namespace.)
#[test]
fn xsi_type_under_an_xpath_type_in_1_0() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="u" as="element()"><z xsl:type="xs:untypedAtomic" xsi:type="s:untypedAtomic"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>a b</z></xsl:variable>"#,
            "string($u), $u instance of element(*, xs:anySimpleType), data($u) instance of xs:untypedAtomic"
        ),
        "a b true true"
    );
    assert_codes(
        v10(),
        &[
            (
                r#"<z xsl:type="xs:untypedAtomic" xsi:type="s:string"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>a</z>"#,
                codes::XTTE1540,
            ),
            (
                r#"<z xsl:type="xs:untypedAtomic" xsi:type="s:anySimpleType"><xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>a</z>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// M4g's workaround warned once per transformation and type where it annotated a stand-in; the
/// annotations are exact now (upstream `SchemaSet::xpath_type`), so there is nothing to say.
#[test]
fn the_xpath_types_give_no_warning() {
    let main = sheet(
        false,
        r#"<xsl:template match="/"><out>
             <xsl:for-each select="1 to 3"><e><xsl:attribute name="a" type="xs:anyAtomicType">x</xsl:attribute><z xsl:type="xs:anyAtomicType">x</z></e></xsl:for-each>
             <z xsl:type="xs:untypedAtomic">y</z>
           </out></xsl:template>"#,
    );
    let ran = run_with(v10(), &main, "<src/>", SourceValidation::Skip);
    ran.result.expect("valid");
    assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
}

// ══ Upstream C89: QName defaults that need a namespace declaration (held start tags) ═══════════

/// XML Schema 1.1 Part 1 §3.3.1: "if the element being ·validated· is empty, then for purposes of
/// calculating the [schema normalized value] and other contributions to the
/// ·post-schema-validation infoset· the element is treated as if {value constraint}.{lexical form}
/// was used as the content of the element"; the {value} of `default="p:dvalue"` is `{urn:p}dvalue`
/// (Datatypes §3.3.18: the bindings "are those in the [in-scope namespaces] property of the
/// relevant element" — the schema's). XSLT 2.0 §19.2.1.1: "the validation process will where
/// necessary create new nodes containing these default values". Where the constructed element
/// binds no prefix to `urn:p` (none at all, or `p` to another namespace), the text is written with
/// a prefix declared on the element — the namespace fixup Structures §3.4.5.1's Note leaves to the
/// processor —, so the tree's typed value is the {value}.
#[test]
fn a_qname_default_with_no_prefix_in_scope_is_declared_on_the_element() {
    for scope in ["", r#"xmlns:p="urn:other""#] {
        assert_eq!(
            answers(
                v10(),
                &format!(
                    r#"<xsl:variable name="t"><g:qd xsl:validation="strict" {scope}/></xsl:variable>"#
                ),
                "string($t/g:qd), namespace-uri-from-QName(data($t/g:qd)), \
                 local-name-from-QName(data($t/g:qd)), \
                 namespace-uri-for-prefix(substring-before(string($t/g:qd), ':'), $t/g:qd)"
            ),
            "ns_d0:dvalue urn:p dvalue urn:p",
            "{scope}"
        );
    }
    assert_eq!(
        result(v10(), r#"<out><g:qd xsl:validation="strict"/></out>"#),
        r#"<out><g:qd xmlns:g="urn:m4g" xmlns:ns_d0="urn:p">ns_d0:dvalue</g:qd></out>"#
    );
}

/// The same for `fixed`, a list, and simple content of a complex type (the {content type}'s simple
/// type definition decides whether the value holds QNames).
#[test]
fn fixed_list_and_simple_content_qname_values_are_declared_too() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="t">
                 <g:qf xsl:validation="strict"/>
                 <g:ql xsl:validation="strict"/>
                 <g:cq xsl:validation="strict" n="1"/>
               </xsl:variable>"#,
            "string($t/g:qf), namespace-uri-from-QName(data($t/g:qf)), \
             string($t/g:ql), for $q in data($t/g:ql) return namespace-uri-from-QName($q), \
             string($t/g:cq), namespace-uri-from-QName(data($t/g:cq))"
        ),
        "ns_d0:fvalue urn:p ns_d0:a ns_d0:b urn:p urn:p ns_d0:cvalue urn:p"
    );
}

/// §19.2.1.1: "Conceptually, the validation requested for a child element or attribute node is
/// applied before the validation requested for its parent element", and §19.2.1.3's
/// serialize–parse–validate: an outer validation sees the default the inner one created, as text
/// of the element, with the prefix declared for it in scope — so it reads the same {value} and the
/// element is valid for it (here `wrap` assesses its `qd` children against the same declaration).
/// Also where only the outer validation supplies the default (no boundary on `qd`), and under a
/// `type` boundary whose content model has a local declaration with a default.
#[test]
fn every_outer_validation_reads_the_declared_prefix() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="t">
                 <g:wrap xsl:validation="strict"><g:qd xsl:validation="strict"/><g:qd/></g:wrap>
                 <g:loose xsl:validation="strict" xmlns:p="urn:other"><g:lq/></g:loose>
                 <any xsl:type="g:looseType"><g:lq/></any>
               </xsl:variable>"#,
            "for $q in ($t/g:wrap/g:qd, $t/g:loose/g:lq, $t/any/g:lq) \
             return concat(string($q), '=', namespace-uri-from-QName(data($q)))"
        ),
        "ns_d0:dvalue=urn:p ns_d0:dvalue=urn:p ns_d0:lvalue=urn:p ns_d0:lvalue=urn:p"
    );
    assert_eq!(
        result(
            v10(),
            r#"<out><g:wrap xsl:validation="strict"><g:qd/></g:wrap></out>"#
        ),
        r#"<out><g:wrap xmlns:g="urn:m4g"><g:qd xmlns:ns_d0="urn:p">ns_d0:dvalue</g:qd></g:wrap></out>"#
    );
}

/// The element's own content decides whether the default applies (Structures §3.3.4.3 clause 5.1:
/// "E has neither element nor character [children]"): text keeps its value and needs no
/// declaration; comments and processing instructions do not prevent the default and keep their
/// order (the text goes first, where it would have been); an element child makes it invalid
/// (XTTE1510) — the content is held until the element ends, so every case is decided there.
#[test]
fn content_decides_and_keeps_its_order() {
    assert_eq!(
        result(
            v10(),
            r#"<out><g:qd xsl:validation="strict" xmlns:z="urn:p">z:own</g:qd><g:qd xsl:validation="strict"><xsl:comment>c</xsl:comment><xsl:processing-instruction name="pi">d</xsl:processing-instruction></g:qd><g:sd xsl:validation="strict"/></out>"#
        ),
        r#"<out><g:qd xmlns:g="urn:m4g" xmlns:z="urn:p">z:own</g:qd><g:qd xmlns:g="urn:m4g" xmlns:ns_d0="urn:p">ns_d0:dvalue<!--c--><?pi d?></g:qd><g:sd xmlns:g="urn:m4g">text</g:sd></out>"#
    );
    assert_codes(
        v10(),
        &[(
            r#"<out><g:qd xsl:validation="strict"><g:x/></g:qd></out>"#,
            codes::XTTE1510,
        )],
    );
    // A document node in the content (§5.7.1: replaced by its children) with only a comment: the
    // element is still empty of element and character children.
    assert_eq!(
        result(
            v10(),
            r#"<out><g:qd xsl:validation="strict"><xsl:document><xsl:comment>c</xsl:comment></xsl:document></g:qd></out>"#
        ),
        r#"<out><g:qd xmlns:g="urn:m4g" xmlns:ns_d0="urn:p">ns_d0:dvalue<!--c--></g:qd></out>"#
    );
}

/// Under `strip` (§19.2.1.1: the node is untyped, "The typed value of the node is changed to be the
/// same as its string value") the element keeps the default's text and its declaration; under
/// `preserve` the value is the validation's.
#[test]
fn the_declaration_survives_strip_and_preserve() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="t">
                 <w xsl:validation="strip"><g:wrap xsl:validation="strict"><g:qd/></g:wrap></w>
                 <w xsl:validation="preserve"><g:qd xsl:validation="strict"/></w>
               </xsl:variable>"#,
            "string($t/w[1]//g:qd), $t/w[1]//g:qd instance of element(*, xs:untyped), \
             namespace-uri-for-prefix('ns_d0', $t/w[1]//g:qd), \
             namespace-uri-from-QName(data($t/w[2]/g:qd))"
        ),
        "ns_d0:dvalue true urn:p urn:p"
    );
}

// ══ Part 2: residue fixed ═════════════════════════════════════════════════════════════════════

/// XPath 2.0 §2.5.4.2: "document-node(E) matches any document node that contains exactly one
/// element node, optionally accompanied by one or more comment and processing instruction nodes,
/// if E is an ElementTest or SchemaElementTest that matches the element node" — in a pattern too
/// (XSLT 2.0 §5.5.3: a pattern's node test is matched as XPath matches it). Two element children,
/// or a text child, and the document does not match; comments and processing instructions do not
/// count (`import-schema-055` has a document with two `address` children).
#[test]
fn a_document_node_pattern_needs_exactly_one_element_child() {
    let main = sheet(
        false,
        r#"<xsl:template match="/">
             <xsl:variable name="two"><a/><a/></xsl:variable>
             <xsl:variable name="text">x<a/></xsl:variable>
             <xsl:variable name="other"><b/></xsl:variable>
             <xsl:variable name="one"><xsl:comment>c</xsl:comment><a/><xsl:processing-instruction name="p"/></xsl:variable>
             <out><xsl:apply-templates select="$two, $text, $other, $one" mode="m"/></out>
           </xsl:template>
           <xsl:template match="document-node(element(a))" mode="m">[yes]</xsl:template>
           <xsl:template match="document-node()" mode="m">[no]</xsl:template>"#,
    );
    assert_eq!(
        run_with(v10(), &main, "<src/>", SourceValidation::Skip)
            .result
            .expect("runs"),
        "<out>[no][no][no][yes]</out>"
    );
}

/// XTDE0905: "the string value of the new namespace node is not valid in the lexical space of the
/// datatype xs:anyURI". In an XML Schema 1.1 context `xs:anyURI` is XML Schema 1.1's: "The
/// ·lexical space· of anyURI is the set of finite-length sequences of zero or more characters (as
/// defined in [XML]) that ·match· the Char production from [XML]" (Datatypes 1.1 §3.3.17.2), so
/// `####` is one (`error-0905b`); in an XML Schema 1.0 context this processor keeps XML Schema
/// 1.0's at-most-one-`#` rule (`error-0905a`).
#[test]
fn the_lexical_space_of_any_uri_follows_the_context() {
    let content = r#"<out><xsl:namespace name="ns">####</xsl:namespace></out>"#;
    assert_eq!(result(v11(), content), "<out xmlns:ns=\"####\"/>");
    assert_codes(v10(), &[(content, codes::XTDE0905)]);
}

/// §3.14: "The schema-location attribute is a URI Reference that gives a hint indicating where a
/// schema document or other resource containing the required definitions may be found", and the
/// synthetic schema document's `xs:import` follows XML Schema's rules (Structures 1.1 §4.2.6.2,
/// local): "It is not an error for the application schema component reference strategy to fail",
/// and "Given that the schemaLocation [attribute] is only a hint, it is open to applications to
/// ignore all but the first <import> for a given namespace, regardless of the ·actual value· of
/// schemaLocation". So where the context already holds the namespace's components (a host schema,
/// §3.13's mechanism), a location whose document has another target namespace is a hint that did
/// not lead to the namespace's schema: passed over (`found()` false), not XTSE0220
/// (`import-schema-186`, `-187`). Where nothing else supplies the namespace, the document found is
/// the import's referent and the mismatch is XTSE0220, as before (clause 3.1).
#[test]
fn a_mismatched_location_is_passed_over_when_a_host_schema_has_the_namespace() {
    const N: &str = "urn:m4g:held";
    let held = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="{N}">
             <xs:simpleType name="small"><xs:restriction base="xs:integer"><xs:maxInclusive value="9"/></xs:restriction></xs:simpleType>
           </xs:schema>"#
    );
    let other = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m4g:other"/>"#;
    let main = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:n="{N}" exclude-result-prefixes="n">
             <xsl:import-schema namespace="{N}" schema-location="other.xsd"/>
             <xsl:template match="/"><out><xsl:value-of select="10 castable as n:small"/></out></xsl:template>
           </xsl:stylesheet>"#
    );
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, &main);
    mem.insert("file:///m4g/other.xsd", other);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let functions = XsltFunctions::new();
    let build = |host: bool| {
        let builder = SchemaContext::builder().with_resolver(Arc::clone(&resolver));
        let builder = if host {
            builder.schema_str(&held, "file:///m4g/held.xsd")
        } else {
            builder
        };
        builder
            .stylesheet_uri(MAIN, &functions)
            .and_then(|builder| builder.build())
    };
    let context = build(true).expect("the host schema supplies the namespace");
    assert_eq!(context.imports().len(), 1);
    assert!(!context.imports()[0].found(), "{:?}", context.imports());
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .expect("compiles");
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source(SOURCE, "<x/>")
        .expect("runs");
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>false</out>"
    );
    let e = build(false).expect_err("nothing else supplies the namespace");
    assert_eq!(e.code, codes::XTSE0220, "{e}");
}

// ══ Fix round 1 (`XSLT2_M4G_REVIEW.md`) ═══════════════════════════════════════════════════════

/// The XPath types and the root's `xsi:type` under `xs:anyAtomicType` (review R4). XDM 1.0 §2.6.2:
/// "xs:anyAtomicType is an atomic type that includes all atomic values (and no values that are not
/// atomic). Its base type is xs:anySimpleType" — so `xsi:type="xs:anySimpleType"` is not derived
/// from it (XTTE1540, both versions), while `xs:untypedAtomic`, `xs:dayTimeDuration` and
/// `xs:yearMonthDuration` are (XPath 2.0 §2.5.1: "All primitive atomic types … have
/// xs:anyAtomicType as their base type"; the durations restrict `xs:duration`) and replace it
/// (Structures clause 1.2.1.2.4), validated by their own rules — in both versions since the Wave B
/// follow-up (`tests/wave_b_followup.rs`, `an_xsi_type_under_any_atomic_type_in_both_versions`;
/// M4g fenced the durations in a 1.0 context and gave `xs:untypedAtomic` its stand-in there).
/// (`s` is bound by `xsl:namespace`, as in `xsi_type_under_an_xpath_type_in_1_0`.)
#[test]
fn an_xsi_type_under_any_atomic_type_follows_the_xpath_hierarchy() {
    let ns = r#"<xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>"#;
    let top = format!(
        r#"<xsl:variable name="d" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:dayTimeDuration">{ns}PT1H</z></xsl:variable>
           <xsl:variable name="y" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:yearMonthDuration">{ns}P1Y</z></xsl:variable>"#
    );
    assert_eq!(
        answers(
            v11(),
            &top,
            "$d instance of element(*, xs:dayTimeDuration), string($d), $y instance of element(*, xs:yearMonthDuration)"
        ),
        "true PT1H true"
    );
    assert_codes(
        v11(),
        &[
            (
                &format!(
                    r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:dayTimeDuration">{ns}P1Y</z>"#
                ),
                codes::XTTE1540,
            ),
            (
                &format!(
                    r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:yearMonthDuration">{ns}PT1H</z>"#
                ),
                codes::XTTE1540,
            ),
        ],
    );
    for version in [v10(), v11()] {
        assert_codes(
            version,
            &[(
                &format!(r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:anySimpleType">{ns}x</z>"#),
                codes::XTTE1540,
            )],
        );
    }
    // `xs:untypedAtomic` under `xs:anyAtomicType` in a 1.0 set: valid for every string, annotated
    // `xs:untypedAtomic` (derived from `xs:anySimpleType`), as `[xsl:]type="xs:untypedAtomic"` is.
    let top = format!(
        r#"<xsl:variable name="u" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:untypedAtomic">{ns} a b </z></xsl:variable>"#
    );
    assert_eq!(
        answers(
            v10(),
            &top,
            "$u instance of element(*, xs:anySimpleType), data($u) instance of xs:untypedAtomic, concat('[', $u, ']')"
        ),
        "true true [ a b ]"
    );
}

/// An `xsi:type` naming `xs:untypedAtomic` in a 1.1 context (review R5). M4g fenced it
/// (`XSLT2-NOTIMPL` naming upstream proposal C92: a 1.1 set had no validator for the type). Upstream
/// has one now, and the validator of constructed trees resolves the XPath 2.0 types in an
/// `xsi:type` (`ValidationFlags::XPATH_TYPES`): `xs:untypedAtomic` is derived from
/// `xs:anyAtomicType` (XDM 1.0 §2.6.2) and governs the element.
#[test]
fn xsi_type_naming_untyped_atomic_in_a_1_1_context_is_valid() {
    let ns = r#"<xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>"#;
    assert_eq!(
        answers(
            v11(),
            &format!(
                r#"<xsl:variable name="u" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:untypedAtomic">{ns}x</z></xsl:variable>"#
            ),
            "$u instance of element(*, xs:untypedAtomic), data($u) instance of xs:untypedAtomic"
        ),
        "true true"
    );
}

/// The 1.1 built-in types in scope (review R6, R7). XPath 2.0 §3.10.4: "For every atomic type in the
/// in-scope schema types (except xs:NOTATION and xs:anyAtomicType, which are not instantiable), a
/// constructor function is implicitly defined", and XSLT 2.0 §18.1.1: function-available "returns
/// true if and only if there is an available function whose name matches" — so where
/// `type-available('xs:dateTimeStamp')` is true, `function-available('xs:dateTimeStamp', 1)` is too.
/// `xs:error` is no Part 2 type: Structures 1.1 §3.16.7.3 defines it ("A Simple Type Definition
/// for ·xs:error· is present in every schema by definition"), and §3.13 adds "All other built-in
/// types defined in [XML Schema Part 2]" — so it is not in scope (XTSE1520 for `type="xs:error"`).
#[test]
fn xsd11_built_ins_have_constructors_and_xs_error_is_no_part_2_type() {
    assert_eq!(
        answers(
            v11(),
            "",
            "type-available('xs:dateTimeStamp'), function-available('xs:dateTimeStamp', 1), \
             type-available('xs:error')"
        ),
        "true true false"
    );
    assert_eq!(
        answers(
            v10(),
            "",
            "type-available('xs:dateTimeStamp'), function-available('xs:dateTimeStamp', 1), \
             type-available('xs:error')"
        ),
        "false false false"
    );
    let main = sheet(
        true,
        r#"<xsl:template match="/"><out><xsl:attribute name="a" type="xs:error">x</xsl:attribute></out></xsl:template>"#,
    );
    let e = run_with(v11(), &main, "<src/>", SourceValidation::Skip)
        .result
        .expect_err("not in scope");
    assert_eq!(e.code, codes::XTSE1520, "{e}");
}

/// No warning where the annotation is exact (review R9 — and since the Wave B follow-up there is no
/// C72 warning at all): an attribute copied with `type="xs:untypedAtomic"` is exactly an unvalidated
/// one, as with `xsl:attribute`; nor for a `copy-of` that copies only atomic values.
#[test]
fn an_exact_xpath_type_annotation_gives_no_warning() {
    let main = sheet(
        false,
        r#"<xsl:template match="/">
             <xsl:variable name="s"><z t="x"/></xsl:variable>
             <out><xsl:copy-of select="$s/z/@t" type="xs:untypedAtomic"/><xsl:copy-of select="1, 'a'" type="xs:dayTimeDuration"/></out>
           </xsl:template>"#,
    );
    let ran = run_with(v10(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(ran.result.expect("valid"), r#"<out t="x">1 a</out>"#);
    assert!(ran.warnings.is_empty(), "{:?}", ran.warnings);
}

/// C89 in the other containers (review R11; the reviewer's probes as regression tests): a
/// validated document (`xsl:document`, §19.2.2: "The single element node child is validated"), a
/// secondary result (`xsl:result-document`), `strict` and `lax` copies (`xsl:copy-of`, `xsl:copy`,
/// §11.9.1 / §11.9.2: the copy is validated), each giving `qd` its default with a declared prefix.
#[test]
fn c89_in_documents_result_documents_and_copies() {
    let probe = "string($q), namespace-uri-from-QName(data($q))";
    let cases = [
        r#"<xsl:document validation="strict"><g:qd/></xsl:document>"#,
        r#"<xsl:copy-of select="$s/g:qd" validation="strict"/>"#,
        r#"<xsl:copy-of select="$s/g:qd" validation="lax"/>"#,
        r#"<xsl:for-each select="$s/g:qd"><xsl:copy validation="strict"/></xsl:for-each>"#,
        r#"<xsl:for-each select="$s/g:qd"><xsl:copy validation="lax"/></xsl:for-each>"#,
    ];
    for content in cases {
        assert_eq!(
            answers(
                v10(),
                &format!(
                    r#"<xsl:variable name="s"><g:qd/></xsl:variable>
                       <xsl:variable name="t">{content}</xsl:variable>"#
                ),
                &format!("for $q in $t//g:qd return ({probe})")
            ),
            "ns_d0:dvalue urn:p",
            "{content}"
        );
    }
    // A secondary result.
    let main = sheet(
        false,
        r#"<xsl:template match="/"><xsl:result-document href="r.xml" validation="strict"><g:qd/></xsl:result-document><out/></xsl:template>"#,
    );
    let resolver = resolver(&main);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .expect("context");
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .expect("compiles");
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source(SOURCE, "<src/>")
        .expect("runs");
    let secondary = outcome
        .secondary
        .iter()
        .find(|r| r.uri.ends_with("r.xml"))
        .expect("the result document");
    assert_eq!(
        common::body(compiled.serialize_secondary(secondary).expect("serializes")),
        r#"<g:qd xmlns:g="urn:m4g" xmlns:ns_d0="urn:p">ns_d0:dvalue</g:qd>"#
    );
}

/// C89 and is-id under `strip` (review R11): a held element that is an ID by an attribute keeps its
/// is-id after the `strip` around it ("The values of the is-id and is-idrefs properties are
/// unchanged", §19.2.1.1), and under `strict` too; its default carries the declared prefix.
#[test]
fn c89_keeps_is_id_under_strip_and_strict() {
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="t">
                 <w xsl:validation="strip"><g:qi xsl:validation="strict" id="x1"/></w>
                 <v xsl:validation="preserve"><g:qi xsl:validation="strict" id="x2"/></v>
               </xsl:variable>"#,
            "count(id('x1', $t)), string(id('x1', $t)), count(id('x2', $t)), \
             namespace-uri-from-QName(data(id('x2', $t)))"
        ),
        "1 ns_d0:ivalue 1 urn:p"
    );
}

/// The location hints D-g11 passes over, and the locations that cannot be read, are never silent
/// (review R8): the context says so (`SchemaContext::warnings`). Structures 1.1 §4.2.6.2: "it is open
/// to applications to ignore all but the first <import> for a given namespace … but such a strategy
/// risks missing useful information when new schemaLocations are offered" — hence the warning.
#[test]
fn passed_over_and_unread_locations_are_warned() {
    const N: &str = "urn:m4g:held";
    let held = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="{N}"/>"#
    );
    let other = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:m4g:other"/>"#;
    let main = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:import-schema namespace="{N}" schema-location="other.xsd"/>
             <xsl:import-schema namespace="urn:m4g:absent" schema-location="absent.xsd"/>
             <xsl:template match="/"><out/></xsl:template>
           </xsl:stylesheet>"#
    );
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, &main);
    mem.insert("file:///m4g/other.xsd", other);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .schema_str(&held, "file:///m4g/held.xsd")
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .expect("builds");
    let warnings = context.warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].contains("other.xsd") && warnings[0].contains("passed over"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].contains("absent.xsd") && warnings[1].contains("could not be read"),
        "{warnings:?}"
    );
    // A location that leads to its namespace's schema: nothing to say.
    let quiet = sheet(false, "");
    let quiet = SchemaContext::builder()
        .with_resolver(self::resolver(&quiet))
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .expect("builds");
    assert!(quiet.warnings().is_empty(), "{:?}", quiet.warnings());
}
