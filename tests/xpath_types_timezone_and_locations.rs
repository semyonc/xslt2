//! The XPath 2.0 types in schema sets, QName element defaults, the implicit timezone, schema
//! locations and base URIs: what this crate does with the APIs of `xsd-schema` for them.
//!
//! - The XPath 2.0 types (`xs:untypedAtomic`, `xs:anyAtomicType`, `xs:dayTimeDuration`,
//!   `xs:yearMonthDuration`) are components of every schema set (`SchemaSet::xpath_type`), and an
//!   `xsi:type` may name them in a constructed tree (`ValidationFlags::XPATH_TYPES`): `[xsl:]type`
//!   naming them validates and annotates exactly in an XML Schema 1.0 context too, with no warning.
//! - `SchemaSet::element_value_constraint_for_type` gives the value an empty element takes before
//!   its content: only an element whose value needs a new prefix has its start tag held, and the
//!   text is the {lexical form}.
//! - `TransformOptions::implicit_timezone` fixes the implicit timezone of a transformation, for its
//!   expressions and for the host's own comparisons (sort keys, key values, grouping keys — the
//!   `xs:g*` types included, which `eq` compares by starting instant).
//! - Schema locations resolve by RFC 3986 (dot segments, `file:` URIs, one document under several
//!   spellings), under every base.
//! - A node's base URI is its `xml:base` chain resolved (`xsd-schema`'s `fn:base-uri`;
//!   `DomNavigator::base_uri` is unresolved): `document()` on a node, parentless copies.
//!
//! Every test goes through the public API and cites the rule it checks.

mod common;

use std::sync::Arc;

use bumpalo::Bump;

use xsd_schema::namespace::NameTable;
use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TimezoneOffset,
    TransformOptions, XsdVersion, XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///fu/main.xsl";
const SOURCE: &str = "file:///fu/doc.xml";
const XSD_URI: &str = "file:///fu/fu.xsd";

/// `urn:fu`, valid in both XML Schema versions; the schema document binds `p` to `urn:p`.
/// Global elements: `dur` (`xs:duration`), `any` (`xs:anySimpleType`), `qd` (`xs:QName`, default
/// `p:dvalue`), `an` (`xs:anySimpleType`, default `p:svalue`), `u` (a union of `xs:token` and
/// `xs:string`, default `"  a   b  "`), `wrap` (any number of `qd`, by reference).
const FU_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:fu"
    xmlns:p="urn:p" targetNamespace="urn:fu" elementFormDefault="qualified">
  <xs:element name="dur" type="xs:duration"/>
  <xs:element name="any" type="xs:anySimpleType"/>
  <xs:element name="qd" type="xs:QName" default="p:dvalue"/>
  <xs:element name="an" type="xs:anySimpleType" default="p:svalue"/>
  <xs:simpleType name="tokenOrString"><xs:union memberTypes="xs:token xs:string"/></xs:simpleType>
  <xs:element name="u" type="tokenOrString" default="  a   b  "/>
  <xs:element name="wrap">
    <xs:complexType>
      <xs:sequence><xs:element ref="qd" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

/// A stylesheet importing `urn:fu`, with `top` as its top-level content. `f` is `urn:fu`, `xs`
/// the XML Schema namespace, `xsi` the instance namespace; `p` is not declared.
fn sheet(top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:f=\"urn:fu\" \
           xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
           exclude-result-prefixes=\"xs f xsi\">\
           <xsl:import-schema namespace=\"urn:fu\" schema-location=\"fu.xsd\"/>\
           {top}</xsl:stylesheet>"
    )
}

/// `urn:fu11`, XML Schema 1.1 only (a stylesheet imports it with `schema-location="fu11.xsd"`):
/// `alt` has a QName default, `p:avalue`, and type alternatives — `kind="q"` gives it `xs:QName`
/// content, anything else `xs:string` content.
const FU11_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:fu11"
    xmlns:p="urn:p" targetNamespace="urn:fu11" elementFormDefault="qualified">
  <xs:complexType name="qnameAlt">
    <xs:simpleContent>
      <xs:extension base="xs:QName"><xs:attribute name="kind" type="xs:string"/></xs:extension>
    </xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="stringAlt">
    <xs:simpleContent>
      <xs:extension base="xs:string"><xs:attribute name="kind" type="xs:string"/></xs:extension>
    </xs:simpleContent>
  </xs:complexType>
  <xs:element name="alt" default="p:avalue">
    <xs:alternative test="@kind = 'q'" type="qnameAlt"/>
    <xs:alternative type="stringAlt"/>
  </xs:element>
</xs:schema>"#;

fn resolver(main: &str) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, FU_XSD);
    mem.insert("file:///fu/fu11.xsd", FU11_XSD);
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
    let failed = |e| Ran {
        result: Err(e),
        warnings: Vec::new(),
    };
    let context = match SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .xsd_version(version)
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
    {
        Ok(context) => context,
        Err(e) => return failed(e),
    };
    let compiled = match Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
    {
        Ok(compiled) => compiled,
        Err(e) => return failed(e),
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

/// `<xsl:namespace name="s">` binding `s` to the XML Schema namespace on the element it is in
/// (the stylesheet excludes `xs` from its result elements, so an `xsi:type` value uses `s`).
const S: &str = r#"<xsl:namespace name="s">http://www.w3.org/2001/XMLSchema</xsl:namespace>"#;

/// The text of `<out>…</out>` the template matching `/` writes: the declarations `top` (variables)
/// in the template, then `<xsl:value-of select="{body}"/>`; and the run's warnings.
fn answers_and_warnings(version: XsdVersion, top: &str, body: &str) -> (String, Vec<String>) {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{top}<out><xsl:value-of select="{body}"/></out></xsl:template>"#
    ));
    let ran = run_with(version, &main, "<src/>", SourceValidation::Skip);
    match ran.result {
        Ok(text) => (inner(&text), ran.warnings),
        Err(e) => panic!("the run failed: {e}\n{top}\n{body}"),
    }
}

fn answers(version: XsdVersion, top: &str, body: &str) -> String {
    answers_and_warnings(version, top, body).0
}

/// The serialized result of the template matching `/` with `content`.
fn result(version: XsdVersion, content: &str) -> String {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{content}</xsl:template>"#
    ));
    match run_with(version, &main, "<src/>", SourceValidation::Skip).result {
        Ok(text) => text,
        Err(e) => panic!("the run failed: {e}\n{content}"),
    }
}

/// Every `(content, code)` raises `code`; the ones that do not are listed together.
fn assert_codes(version: XsdVersion, cases: &[(&str, &str)]) {
    let mut wrong = Vec::new();
    for (content, code) in cases {
        let main = sheet(&format!(
            r#"<xsl:template match="/">{content}</xsl:template>"#
        ));
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

// ══ the XPath 2.0 types are the set's own in both versions ═════════════════════════════════════

/// The XPath 2.0 types in an XML Schema 1.0 context answer as they do in a 1.1 context. XDM 1.0 §2.6.2: "the data model defines five additional types:
/// xs:anyAtomicType, xs:untyped, xs:untypedAtomic, xs:dayTimeDuration, and xs:yearMonthDuration",
/// which XSLT 2.0 §3.13 puts in the in-scope schema components of every processor, so
/// `[xsl:]type` may name them (§19.2.1.2: "the element or attribute is validated against that
/// type"). The typed value of a node annotated `xs:dayTimeDuration` is a `xs:dayTimeDuration`
/// (ordered, with arithmetic: XPath 2.0 Appendix B.2), so `lt`, `+`, `*`, `sum` / `avg` / `min` /
/// `max` and `xsl:sort` answer, and `element(*, xs:dayTimeDuration)` matches. No warning: the
/// annotation is the type itself.
#[test]
fn durations_answer_as_xpath_defines_them_in_1_0() {
    let d = r#"<xsl:variable name="d" as="element()*"><z xsl:type="xs:dayTimeDuration">PT1H</z><z xsl:type="xs:dayTimeDuration">PT3H</z><z xsl:type="xs:dayTimeDuration">PT2H</z></xsl:variable>"#;
    let y = r#"<xsl:variable name="y" as="element()*"><z xsl:type="xs:yearMonthDuration">P1Y</z><z xsl:type="xs:yearMonthDuration">P2M</z></xsl:variable>"#;
    let rest = r#"<xsl:variable name="e" as="element()"><xsl:element name="z" type="xs:yearMonthDuration"> P1Y2M </xsl:element></xsl:variable>
                  <xsl:variable name="a" as="attribute()"><xsl:attribute name="t" type="xs:dayTimeDuration">P2D</xsl:attribute></xsl:variable>
                  <xsl:variable name="src"><z>PT3M</z><w t="PT5M"/></xsl:variable>
                  <xsl:variable name="c" as="element()"><xsl:copy-of select="$src/z" type="xs:dayTimeDuration"/></xsl:variable>
                  <xsl:variable name="ca" as="attribute()"><xsl:copy-of select="$src/w/@t" type="xs:dayTimeDuration"/></xsl:variable>
                  <xsl:variable name="cp" as="element()"><xsl:for-each select="$src/z"><xsl:copy type="xs:dayTimeDuration">PT4M</xsl:copy></xsl:for-each></xsl:variable>
                  <xsl:variable name="doc" as="document-node()"><xsl:document type="xs:yearMonthDuration"><z>P3M</z></xsl:document></xsl:variable>"#;
    let (text, warnings) = answers_and_warnings(
        v10(),
        &format!("{d}{y}{rest}"),
        "data($d[1]) instance of xs:dayTimeDuration, data($d[1]) eq xs:dayTimeDuration('PT1H'), \
         data($d[1]) lt xs:dayTimeDuration('PT2H'), data($d[1]) + xs:dayTimeDuration('PT1H'), \
         data($d[1]) * 2, data($y[1]) + data($y[2]), xs:date('2020-01-01') + data($y[1]), \
         sum($d), avg($d), min($d), max($d), \
         $d[1] instance of element(*, xs:dayTimeDuration), $y[1] instance of element(*, xs:yearMonthDuration), \
         string($e), $e instance of element(*, xs:yearMonthDuration), \
         data($a) instance of xs:dayTimeDuration, data($a) gt xs:dayTimeDuration('PT1H'), \
         $c instance of element(*, xs:dayTimeDuration), $ca instance of attribute(*, xs:dayTimeDuration), \
         $cp instance of element(*, xs:dayTimeDuration), \
         $doc/z instance of element(*, xs:yearMonthDuration), \
         hours-from-duration(data($d[2])), distinct-values(($d, $d))",
    );
    assert_eq!(
        text,
        "true true true PT2H PT2H P1Y2M 2021-01-01 PT6H PT2H PT1H PT3H true true P1Y2M true \
         true true true true true true 3 PT1H PT3H PT2H"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    // `xsl:sort` (§13.1.2: the sort key values compared with `lt`).
    assert_eq!(
        inner(&result(
            v10(),
            &format!(
                r#"{d}<out><xsl:for-each select="$d"><xsl:sort select="."/><xsl:value-of select="."/><xsl:text> </xsl:text></xsl:for-each></out>"#
            )
        )),
        "PT1H PT2H PT3H"
    );
    // Typed patterns naming the types (§5.5.2; XPath 2.0 §2.5.4: derives-from the annotation).
    let patterns = sheet(&format!(
        r#"<xsl:template match="/">{d}{y}<out><xsl:apply-templates select="$d[1], $y[1]"/></out></xsl:template>
           <xsl:template match="element(*, xs:dayTimeDuration)" priority="2">[dt]</xsl:template>
           <xsl:template match="element(*, xs:yearMonthDuration)" priority="2">[ym]</xsl:template>
           <xsl:template match="element(*, xs:duration)">[duration]</xsl:template>"#
    ));
    let ran = run_with(v10(), &patterns, "<src/>", SourceValidation::Skip);
    assert_eq!(inner(&ran.result.expect("runs")), "[dt][ym]");
    // A value outside the type is XTTE1540 (§19.2.1.2) — XDM 1.0 §2.6.2's patterns
    // `[^YM]*[DT].*` and `[^DT]*` on `xs:duration`.
    assert_codes(
        v10(),
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
                r#"<out><xsl:attribute name="t" type="xs:yearMonthDuration">PT1H</xsl:attribute></out>"#,
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
            (
                r#"<z xsl:type="xs:dayTimeDuration">PT1H<y/></z>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// `xs:untypedAtomic` and `xs:anyAtomicType` in an XML Schema 1.0 context: annotated with the type
/// itself, so the type tests are true — XPath 2.0 §2.5.4:
/// `element(*, T)` matches "if the type annotation of the element … is T or is derived from T"; XDM
/// 1.0 §2.6.2: `xs:untypedAtomic`'s base type is `xs:anyAtomicType`. The typed value stays the
/// string value as `xs:untypedAtomic` (XDM 1.0 §3.3.1.2; §2.6.4: "When xs:untypedAtomic is
/// specified as the type, no validation takes place"), white space kept. No warning.
#[test]
fn untyped_atomic_and_any_atomic_type_are_exact_in_1_0() {
    let (text, warnings) = answers_and_warnings(
        v10(),
        r#"<xsl:variable name="v" as="node()*">
             <xsl:attribute name="a" select="'abcd'" type="xs:untypedAtomic"/>
             <z xsl:type="xs:untypedAtomic"> ab  cd </z>
           </xsl:variable>
           <xsl:variable name="e" as="element()"><z xsl:type="xs:anyAtomicType">12</z></xsl:variable>
           <xsl:variable name="t" as="attribute()"><xsl:attribute name="t" type="xs:anyAtomicType">x</xsl:attribute></xsl:variable>"#,
        "$v[1] instance of attribute(*, xs:untypedAtomic), \
         $v[2] instance of element(*, xs:untypedAtomic), $v[2] instance of element(*, xs:anyAtomicType), \
         $v[2] instance of element(*, xs:untyped), \
         data($v[2]) instance of xs:untypedAtomic, concat('[', data($v[2]), ']'), \
         $e instance of element(*, xs:anyAtomicType), $e instance of element(*, xs:untypedAtomic), \
         data($e) instance of xs:untypedAtomic, data($e) + 1, \
         $t instance of attribute(*, xs:anyAtomicType), data($t) instance of xs:untypedAtomic",
    );
    assert_eq!(
        text,
        "true true true false true [ ab  cd ] true false true 13 true true"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    // Simple content: an element child or an attribute other than `xsi:*` is XTTE1540.
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
                r#"<z xsl:type="xs:anyAtomicType"><y/></z>"#,
                codes::XTTE1540,
            ),
        ],
    );
}

/// `validation-0108` (Bugzilla 17480) in an XML Schema 1.0 context — the catalog's stylesheet: an
/// attribute and an element constructed with `type="xs:untypedAtomic"` are `attribute(*,
/// xs:untypedAtomic)` and `element(*, xs:untypedAtomic)`.
#[test]
fn validation_0108_in_1_0() {
    let main = sheet(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="node()*">
               <xsl:attribute name="a" select="'abcd'" type="xs:untypedAtomic"/>
               <z xsl:type="xs:untypedAtomic">abcd</z>
             </xsl:variable>
             <out a="{$v[1] instance of attribute(*, xs:untypedAtomic)}"
                  b="{$v[2] instance of element(*, xs:untypedAtomic)}"/>
           </xsl:template>"#,
    );
    let ran = run_with(v10(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(ran.result.expect("runs"), r#"<out a="true" b="true"/>"#);
}

/// An `xsi:type` on the root of a run for `xs:anyAtomicType`, in both versions. XDM 1.0
/// §2.6.2: "xs:anyAtomicType … Its base type is xs:anySimpleType", so `xsi:type="xs:anySimpleType"`
/// is not derived from it — XTTE1540 (Structures 1.1 §3.3.4.2: an `xsi:type` overrides only when
/// ·validly substitutable·; `cvc-elt.4.2`). `xs:untypedAtomic` and the two durations are
/// derived from it (§2.6.2; the durations restrict `xs:duration`, a primitive whose base type is
/// `xs:anyAtomicType`) and take its place, annotated exactly — also in a 1.0 context. In a 1.1
/// context `xsi:type="xs:untypedAtomic"` is valid.
#[test]
fn an_xsi_type_under_any_atomic_type_in_both_versions() {
    let top = format!(
        r#"<xsl:variable name="d" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:dayTimeDuration">{S}PT1H</z></xsl:variable>
           <xsl:variable name="y" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:yearMonthDuration">{S}P1Y</z></xsl:variable>
           <xsl:variable name="u" as="element()"><z xsl:type="xs:anyAtomicType" xsi:type="s:untypedAtomic">{S} a b </z></xsl:variable>"#
    );
    for version in [v10(), v11()] {
        let (text, warnings) = answers_and_warnings(
            version,
            &top,
            "$d instance of element(*, xs:dayTimeDuration), data($d) lt xs:dayTimeDuration('PT2H'), \
             $y instance of element(*, xs:yearMonthDuration), \
             $u instance of element(*, xs:untypedAtomic), data($u) instance of xs:untypedAtomic, \
             concat('[', $u, ']')",
        );
        assert_eq!(text, "true true true true true [ a b ]", "{version:?}");
        assert!(warnings.is_empty(), "{version:?}: {warnings:?}");
        assert_codes(
            version,
            &[
                (
                    &format!(
                        r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:anySimpleType">{S}x</z>"#
                    ),
                    codes::XTTE1540,
                ),
                (
                    &format!(
                        r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:dayTimeDuration">{S}P1Y</z>"#
                    ),
                    codes::XTTE1540,
                ),
                (
                    &format!(r#"<z xsl:type="xs:anyAtomicType" xsi:type="s:NMTOKENS">{S}a b</z>"#),
                    codes::XTTE1540,
                ),
                // Nothing is derived from `xs:untypedAtomic` (XPath 2.0 §2.5.1: "No predefined
                // schema types are derived from xs:untypedAtomic").
                (
                    &format!(r#"<z xsl:type="xs:untypedAtomic" xsi:type="s:string">{S}a</z>"#),
                    codes::XTTE1540,
                ),
            ],
        );
    }
}

/// An `xsi:type` naming an XPath 2.0 type on an element a `strict` run validates against its
/// declaration (§19.2.1.1): the type is an in-scope schema component (§3.13), so it resolves —
/// `ValidationFlags::XPATH_TYPES` on the validation of constructed trees — and, validly derived
/// from the declared `xs:duration`, governs (Structures 1.1 §3.3.4.2). In both versions.
#[test]
fn an_xsi_type_naming_an_xpath_type_on_a_strict_element() {
    for version in [v10(), v11()] {
        assert_eq!(
            answers(
                version,
                &format!(
                    r#"<xsl:variable name="t"><f:dur xsl:validation="strict" xsi:type="s:dayTimeDuration">{S}PT90M</f:dur></xsl:variable>"#
                ),
                "$t/f:dur instance of element(*, xs:dayTimeDuration), data($t/f:dur) gt xs:dayTimeDuration('PT1H')"
            ),
            "true true",
            "{version:?}"
        );
        assert_codes(
            version,
            &[(
                &format!(
                    r#"<out><f:dur xsl:validation="strict" xsi:type="s:dayTimeDuration">{S}P1Y</f:dur></out>"#
                ),
                codes::XTTE1510,
            )],
        );
    }
}

/// A **source** document is validated as XML Schema validates it: an `xsi:type` naming
/// `xs:untypedAtomic` (which no XML Schema version defines — Structures 1.1 §1.3.1.1) does not
/// resolve there, so a strictly validated source using it is invalid (XTDE1170). Only the
/// validation of constructed trees, where §3.13's in-scope types apply, resolves the XPath 2.0
/// types in an `xsi:type`.
#[test]
fn a_source_xsi_type_naming_untyped_atomic_is_still_invalid() {
    let main =
        sheet(r#"<xsl:template match="/"><out><xsl:value-of select="."/></out></xsl:template>"#);
    for version in [v10(), v11()] {
        let source = r#"<f:any xmlns:f="urn:fu" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:s="http://www.w3.org/2001/XMLSchema" xsi:type="s:untypedAtomic">x</f:any>"#;
        let e = run_with(version, &main, source, SourceValidation::Strict)
            .result
            .expect_err("invalid");
        assert_eq!(e.code, codes::XTDE1170, "{version:?}: {e}");
    }
}

// ══ a QName element default that needs a namespace declaration ════════════════════════════════

/// XML Schema 1.1 Part 1 §3.3.5.4: the [schema normalized value] of an element that takes its
/// value constraint is "the {lexical form} of the {value constraint}", and its {value} is read with
/// the schema's bindings (Datatypes §3.3.18) — `default="p:dvalue"` is `{urn:p}dvalue`. XSLT 2.0
/// §19.2.1.3: "the validation process will where necessary create new nodes containing these
/// default values". Where the element binds no prefix to `urn:p` the value is written with a
/// prefix declared on the element (`ns_d0`), so its typed value is the {value}; where it binds one
/// (`p`, or another prefix), that one is used and nothing is declared.
#[test]
fn a_qname_default_is_written_with_a_prefix_bound_to_its_namespace() {
    for (scope, text) in [
        ("", "ns_d0:dvalue"),
        (r#"xmlns:p="urn:other""#, "ns_d0:dvalue"),
        (r#"xmlns:p="urn:p""#, "p:dvalue"),
        (r#"xmlns:q="urn:p""#, "q:dvalue"),
    ] {
        assert_eq!(
            answers(
                v10(),
                &format!(
                    r#"<xsl:variable name="t"><f:qd xsl:validation="strict" {scope}/></xsl:variable>"#
                ),
                "string($t/f:qd), namespace-uri-from-QName(data($t/f:qd)), \
                 count(in-scope-prefixes($t/f:qd)[starts-with(., 'ns_d')])"
            ),
            format!("{text} urn:p {}", u8::from(text.starts_with("ns_d"))),
            "{scope}"
        );
    }
    // Under an outer validation too (§19.2.1.1: "the validation requested for a child element … is
    // applied before the validation requested for its parent"): `wrap` reads what `qd` wrote.
    assert_eq!(
        result(
            v10(),
            r#"<out><f:wrap xsl:validation="strict"><f:qd xsl:validation="strict"/><f:qd/></f:wrap></out>"#
        ),
        r#"<out><f:wrap xmlns:f="urn:fu"><f:qd xmlns:ns_d0="urn:p">ns_d0:dvalue</f:qd><f:qd xmlns:ns_d0="urn:p">ns_d0:dvalue</f:qd></f:wrap></out>"#
    );
}

/// No namespace node beyond what the value needs: an element that gets content takes no value
/// constraint (Structures §3.3.4.3 clause 5.1: only when "E has neither element nor character
/// [children]"), so its QName default needs no prefix and none is declared — XSLT 2.0 §5.7.3: "a
/// namespace node must not be added to the tree unless the namespace node is necessary either to
/// satisfy these constraints, or to enable the tree to be serialized using the original namespace
/// prefixes from the source document or stylesheet" (the start tag of an element whose default
/// would need a new prefix is still held until its content shows whether it takes the default).
#[test]
fn an_element_with_content_gets_no_spare_declaration() {
    assert_eq!(
        result(
            v10(),
            r#"<out><f:qd xsl:validation="strict" xmlns:z="urn:z">z:own</f:qd><f:qd xsl:validation="strict">own</f:qd><f:wrap xsl:validation="strict"><f:qd>own</f:qd></f:wrap></out>"#
        ),
        r#"<out><f:qd xmlns:f="urn:fu" xmlns:z="urn:z">z:own</f:qd><f:qd xmlns:f="urn:fu">own</f:qd><f:wrap xmlns:f="urn:fu"><f:qd>own</f:qd></f:wrap></out>"#
    );
    assert_eq!(
        answers(
            v10(),
            r#"<xsl:variable name="t"><f:qd xsl:validation="strict" xmlns:z="urn:z">z:own</f:qd></xsl:variable>"#,
            "string-join(in-scope-prefixes($t/f:qd), ' '), namespace-uri-from-QName(data($t/f:qd))"
        )
        .split(' ')
        .filter(|p| p.starts_with("ns_d"))
        .count(),
        0
    );
}

/// The governing type decides whether the value is a QName (Structures §3.3.4.3 clause 5.1.2: the
/// {lexical form} "is locally ·valid· with respect to the ·governing type definition·"): the
/// `xs:anySimpleType` declaration `an` with `default="p:svalue"` takes it as an untyped string
/// (nothing to declare); with `xsi:type="xs:QName"` it is the QName `{urn:p}svalue`, written with a
/// declared prefix.
#[test]
fn the_governing_type_decides_whether_the_default_needs_a_prefix() {
    assert_eq!(
        answers(
            v10(),
            &format!(
                r#"<xsl:variable name="t"><f:an xsl:validation="strict"/><f:an xsl:validation="strict" xsi:type="s:QName">{S}</f:an></xsl:variable>"#
            ),
            "string($t/f:an[1]), data($t/f:an[1]) instance of xs:untypedAtomic, \
             count(in-scope-prefixes($t/f:an[1])[starts-with(., 'ns_d')]), \
             string($t/f:an[2]), namespace-uri-from-QName(data($t/f:an[2]))"
        ),
        "p:svalue true 0 ns_d0:svalue urn:p"
    );
}

/// The text an empty element takes is the value constraint's {lexical form} (§3.3.5.4 clause 1.1),
/// the `default` attribute normalized for the declaration's type (§3.3.2.1: "the ·normalized
/// value· (with respect to the ·effective simple type definition·)") — for the union `xs:token |
/// xs:string`, by `xs:token`, which accepts it: `"  a   b  "` is `"a b"`, also under an `xsi:type`
/// that would keep the white space (`xs:string`).
#[test]
fn the_text_of_a_defaulted_element_is_the_lexical_form() {
    assert_eq!(
        answers(
            v10(),
            &format!(
                r#"<xsl:variable name="t"><f:u xsl:validation="strict"/><f:u xsl:validation="strict" xsi:type="s:string">{S}</f:u></xsl:variable>"#
            ),
            "concat('[', $t/f:u[1], ']'), concat('[', $t/f:u[2], ']')"
        ),
        "[a b] [a b]"
    );
}

/// An XML Schema 1.1 type alternative governs the value too (Structures 1.1 §3.3.2.1, conditional
/// type assignment, and §3.3.4.3 clause 5.1.2): `alt` with `kind="q"` takes its default as the
/// QName `{urn:p}avalue`, written with a declared prefix; with another `kind` it is a string and
/// needs none.
#[test]
fn a_type_alternative_decides_whether_the_default_needs_a_prefix() {
    let main = sheet(
        r#"<xsl:import-schema namespace="urn:fu11" schema-location="fu11.xsd"/>
           <xsl:template match="/" xmlns:h="urn:fu11">
             <xsl:variable name="t"><h:alt xsl:validation="strict" kind="q"/><h:alt xsl:validation="strict" kind="s"/></xsl:variable>
             <out xsl:exclude-result-prefixes="h"><xsl:value-of select="string($t/h:alt[1]), namespace-uri-from-QName(data($t/h:alt[1])),
               string($t/h:alt[2]), count(in-scope-prefixes($t/h:alt[2])[starts-with(., 'ns_d')])"/></out>
           </xsl:template>"#,
    );
    let ran = run_with(v11(), &main, "<src/>", SourceValidation::Skip);
    assert_eq!(
        inner(&ran.result.expect("runs")),
        "ns_d0:avalue urn:p p:avalue 0"
    );
}

// ══ `TransformOptions::implicit_timezone` ═════════════════════════════════════════════════════

/// A basic stylesheet `main` (no schema) run over `<in/>` with `implicit_timezone`: the primary
/// result and each secondary one, serialized.
fn run_basic(main: &str, implicit_timezone: Option<TimezoneOffset>) -> (String, Vec<String>) {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(main, "file:///fu/tz.xsl")
        .unwrap_or_else(|e| panic!("compiles: {e}"));
    let arena = Bump::new();
    let options = TransformOptions {
        implicit_timezone,
        base_output_uri: Some("file:///fu/out/".into()),
        ..Default::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .transform_source("file:///fu/in.xml", "<in/>")
        .unwrap_or_else(|e| panic!("runs: {e}"));
    let primary = compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .unwrap_or_default();
    let secondary = outcome
        .secondary
        .iter()
        .map(|tree| common::body(compiled.serialize_secondary(tree).expect("serializes")))
        .collect();
    (primary, secondary)
}

fn text_sheet(templates: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xs="http://www.w3.org/2001/XMLSchema"
             xmlns:t="urn:t" exclude-result-prefixes="xs t">
             <xsl:output method="text"/>{templates}</xsl:stylesheet>"#
    )
}

/// The option fixes the implicit timezone (XSLT 2.0 §5.4.3.2: "The implicit timezone is
/// implementation-defined"; XPath 2.0 §C.2 makes it a component of the dynamic context): it is what
/// `implicit-timezone()` returns, the timezone of `current-dateTime()`, and the one a date / time
/// without a timezone is compared and subtracted with (F&O 1.0 §10.4: "an implicit timezone,
/// provided by the dynamic context … is assumed to be present as part of the value") — whatever
/// the host's offset is.
#[test]
fn the_option_is_the_implicit_timezone() {
    let main = text_sheet(
        r#"<xsl:template match="/"><xsl:value-of select="implicit-timezone(),
             timezone-from-dateTime(current-dateTime()),
             xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00+05:00'),
             xs:dateTime('2000-01-01T05:00:00') - xs:dateTime('2000-01-01T00:00:00Z'),
             xs:time('12:00:00') lt xs:time('12:00:00-03:00')"/></xsl:template>"#,
    );
    for (offset, expected) in [
        (TimezoneOffset::from_hm(5, 0), "PT5H PT5H true PT0S true"),
        (
            TimezoneOffset::from_hm(-3, 0),
            "-PT3H -PT3H false PT8H false",
        ),
        (TimezoneOffset::UTC, "PT0S PT0S false PT5H true"),
    ] {
        assert_eq!(run_basic(&main, Some(offset)).0, expected, "{offset:?}");
    }
}

/// §5.4.3.2: "The current date and time represents an implementation-dependent point in time
/// during processing of the transformation; it does not change during the course of the
/// transformation"; F&O §16.3 calls `current-dateTime()` ·stable·. One value everywhere the
/// stylesheet reads it — a template, a global variable, a stylesheet function, a sort key, the
/// content of `xsl:result-document` — with the option and without it.
#[test]
fn the_clock_is_stable_for_the_whole_transformation() {
    let main = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xs="http://www.w3.org/2001/XMLSchema"
             xmlns:t="urn:t" exclude-result-prefixes="xs t">
             <xsl:output method="text"/>
             <xsl:variable name="g" select="string(current-dateTime()), string(implicit-timezone())"/>
             <xsl:function name="t:now"><xsl:sequence select="string(current-dateTime()), string(implicit-timezone())"/></xsl:function>
             <xsl:template match="/">
               <xsl:variable name="sorted"><xsl:for-each select="1 to 3"><xsl:sort select="current-dateTime()"/><xsl:value-of select="current-dateTime()"/></xsl:for-each></xsl:variable>
               <xsl:result-document href="r.txt" method="text"><xsl:value-of select="current-dateTime(), implicit-timezone()" separator="|"/></xsl:result-document>
               <xsl:value-of select="current-dateTime(), implicit-timezone()" separator="|"/>
               <xsl:text>#</xsl:text><xsl:value-of select="$g" separator="|"/>
               <xsl:text>#</xsl:text><xsl:value-of select="t:now()" separator="|"/>
               <xsl:text>#</xsl:text><xsl:value-of select="substring(string($sorted), 1, string-length(string(current-dateTime())))"/><xsl:text>|</xsl:text><xsl:value-of select="implicit-timezone()"/>
             </xsl:template></xsl:stylesheet>"#
    );
    for offset in [Some(TimezoneOffset::from_hm(9, 30)), None] {
        let (primary, secondary) = run_basic(&main, offset);
        let parts: Vec<&str> = primary.split('#').collect();
        assert_eq!(parts.len(), 4, "{primary}");
        assert!(
            parts.iter().all(|part| *part == parts[0]),
            "{offset:?}: {primary}"
        );
        assert_eq!(secondary, vec![parts[0].to_string()], "{offset:?}");
        if offset.is_some() {
            assert!(parts[0].ends_with("+09:30|PT9H30M"), "{primary}");
        }
    }
}

/// The host-side comparisons follow the same implicit timezone as the expressions: `xsl:sort`
/// (§13.1.3: values "compared using the `lt` operator"), `key()` (§16.3.2: equal "under the rules
/// appropriate to the XPath `eq` operator") and `xsl:for-each-group group-by` (§14.3: "the `eq`
/// operator") over a dateTime without a timezone and one with. `2000-01-01T00:00:00` read at +05:00
/// is `1999-12-31T19:00:00Z`, earlier than `2000-01-01T03:00:00+05:00` (`…T22:00:00Z`); read at
/// -03:00 it is `…T03:00:00Z`, later. And `2000-01-01T00:00:00` equals `2000-01-01T00:00:00+05:00`
/// (and `1999-12-31T16:00:00-03:00`, the same instant) exactly when the implicit timezone is +05:00.
#[test]
fn sorting_keys_and_grouping_use_the_implicit_timezone() {
    let main = text_sheet(
        r#"<xsl:key name="k" match="d" use="xs:dateTime(.)"/>
           <xsl:variable name="data"><d>2000-01-01T00:00:00</d><d>2000-01-01T03:00:00+05:00</d></xsl:variable>
           <xsl:template match="/">
             <xsl:for-each select="$data/d"><xsl:sort select="xs:dateTime(.)"/><xsl:value-of select="."/><xsl:text> </xsl:text></xsl:for-each>
             <xsl:text>| </xsl:text>
             <xsl:value-of select="count(key('k', xs:dateTime('2000-01-01T00:00:00+05:00'), $data))"/>
             <xsl:text> </xsl:text>
             <xsl:value-of select="count(key('k', xs:dateTime('1999-12-31T19:00:00Z'), $data))"/>
             <xsl:text> | </xsl:text>
             <xsl:for-each-group select="xs:dateTime('2000-01-01T00:00:00'), xs:dateTime('2000-01-01T00:00:00+05:00'), xs:dateTime('1999-12-31T16:00:00-03:00')" group-by=".">
               <xsl:value-of select="count(current-group())"/><xsl:text> </xsl:text>
             </xsl:for-each-group>
           </xsl:template>"#,
    );
    assert_eq!(
        run_basic(&main, Some(TimezoneOffset::from_hm(5, 0))).0,
        "2000-01-01T00:00:00 2000-01-01T03:00:00+05:00 | 1 1 | 3 "
    );
    assert_eq!(
        run_basic(&main, Some(TimezoneOffset::from_hm(-3, 0))).0,
        "2000-01-01T03:00:00+05:00 2000-01-01T00:00:00 | 0 0 | 1 2 "
    );
}

/// An implicit timezone outside -14:00 … +14:00 is no timezone (XML Schema 1.1 Part 2 §3.3.7:
/// "timezone offsets … range from -14:00 to +14:00"): the run refuses it, FODT0003 ("Invalid
/// timezone value", the code F&O gives such a value).
#[test]
fn an_implicit_timezone_out_of_range_is_refused() {
    let main = text_sheet(r#"<xsl:template match="/"><xsl:value-of select="1"/></xsl:template>"#);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&main, "file:///fu/tz.xsl")
        .expect("compiles");
    for minutes in [14 * 60 + 1, -(14 * 60) - 1] {
        let arena = Bump::new();
        let options = TransformOptions {
            implicit_timezone: Some(TimezoneOffset(minutes)),
            ..Default::default()
        };
        match compiled
            .transformation(&arena, options)
            .transform_source("file:///fu/in.xml", "<in/>")
        {
            Err(e) => assert_eq!(e.code, "FODT0003", "{minutes}: {e}"),
            Ok(_) => panic!("{minutes}: the run accepted an implicit timezone out of range"),
        };
    }
    for minutes in [14 * 60, -(14 * 60)] {
        let arena = Bump::new();
        let options = TransformOptions {
            implicit_timezone: Some(TimezoneOffset(minutes)),
            ..Default::default()
        };
        compiled
            .transformation(&arena, options)
            .transform_source("file:///fu/in.xml", "<in/>")
            .unwrap_or_else(|e| panic!("{minutes}: {e}"));
    }
}

/// Without the option the implicit timezone is the host's local offset, read once per
/// transformation; with it, the option, whatever the host's. Checked in a child process of this
/// test binary under each `TZ` (fixed-offset zones, no daylight saving: `Etc/GMT-5` is +05:00,
/// `Etc/GMT+3` is -03:00).
#[cfg(unix)]
#[test]
fn the_host_offset_is_the_default_and_the_option_overrides_it() {
    const CHILD: &str = "XSLT2_TEST_TZ_CHILD";
    let main = text_sheet(
        r#"<xsl:template match="/"><xsl:value-of select="implicit-timezone(),
             timezone-from-dateTime(current-dateTime()),
             xs:dateTime('2000-01-01T00:00:00') eq xs:dateTime('2000-01-01T00:00:00Z')"/></xsl:template>"#,
    );
    if let Ok(expected) = std::env::var(CHILD) {
        let (host, fixed) = expected.split_once('/').expect("two expectations");
        assert_eq!(
            run_basic(&main, None).0,
            host,
            "TZ={:?}",
            std::env::var("TZ")
        );
        assert_eq!(
            run_basic(&main, Some(TimezoneOffset::from_hm(2, 0))).0,
            fixed,
            "TZ={:?}",
            std::env::var("TZ")
        );
        return;
    }
    let fixed = "PT2H PT2H false";
    for (tz, host) in [
        ("Etc/GMT-5", "PT5H PT5H false"),
        ("Etc/GMT+3", "-PT3H -PT3H false"),
        ("UTC", "PT0S PT0S true"),
    ] {
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "the_host_offset_is_the_default_and_the_option_overrides_it",
                "--test-threads=1",
            ])
            .env("TZ", tz)
            .env(CHILD, format!("{host}/{fixed}"))
            .output()
            .expect("the child runs");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "TZ={tz}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The `xs:g*` types too: F&O 1.0 §10.4.15 – §10.4.19 compare "the starting instants of $arg1
/// and $arg2", with the implicit timezone for a value without one, so
/// `xs:gYear('2000') eq xs:gYear('2000Z')` is true under the implicit timezone UTC and false under
/// +05:00, and `xs:gDay('---25-14:00') eq xs:gDay('---26+10:00')` is true (one instant). `key()`
/// and `group-by` agree with `eq` (§16.3.2, §14.3).
#[test]
fn gregorian_keys_and_groups_follow_eq() {
    let main = text_sheet(
        r#"<xsl:key name="y" match="y" use="xs:gYear(.)"/>
           <xsl:key name="d" match="d" use="xs:gDay(.)"/>
           <xsl:variable name="data"><y>2000</y><d>---25-14:00</d></xsl:variable>
           <xsl:template match="/">
             <xsl:value-of select="xs:gYear('2000') eq xs:gYear('2000Z'),
               count(key('y', xs:gYear('2000Z'), $data)),
               xs:gDay('---25-14:00') eq xs:gDay('---26+10:00'),
               count(key('d', xs:gDay('---26+10:00'), $data))"/>
             <xsl:text> |</xsl:text>
             <xsl:for-each-group select="xs:gYear('2000'), xs:gYear('2000Z'), xs:gDay('---25-14:00'), xs:gDay('---26+10:00')" group-by=".">
               <xsl:text> </xsl:text><xsl:value-of select="count(current-group())"/>
             </xsl:for-each-group>
           </xsl:template>"#,
    );
    assert_eq!(
        run_basic(&main, Some(TimezoneOffset::UTC)).0,
        "true 1 true 1 | 2 2"
    );
    assert_eq!(
        run_basic(&main, Some(TimezoneOffset::from_hm(5, 0))).0,
        "false 0 true 1 | 1 1 2"
    );
}

// ══ schema locations with dot segments, `file:` URIs and relative bases ═════════════════════════

/// A schema document `fu-a.xsd` in `urn:fu-a` that includes `inc.xsd` twice, spelled `sub/../inc.xsd`
/// and `./inc.xsd`, and imports `urn:fu-b` from `b/../fu-b.xsd`.
const FU_A_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    targetNamespace="urn:fu-a" xmlns:a="urn:fu-a" xmlns:b="urn:fu-b" elementFormDefault="qualified">
  <xs:include schemaLocation="sub/../inc.xsd"/>
  <xs:include schemaLocation="./inc.xsd"/>
  <xs:import namespace="urn:fu-b" schemaLocation="b/../fu-b.xsd"/>
  <xs:element name="a"><xs:complexType><xs:sequence>
    <xs:element ref="a:i"/><xs:element ref="b:b"/>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#;
const FU_INC_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    targetNamespace="urn:fu-a" elementFormDefault="qualified">
  <xs:element name="i" type="xs:integer"/>
</xs:schema>"#;
const FU_B_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    targetNamespace="urn:fu-b" elementFormDefault="qualified">
  <xs:element name="b" type="xs:date"/>
</xs:schema>"#;

/// The stylesheet at `main` imports `urn:fu-a` from `location`; the three schema documents are
/// served at `dir` + `fu-a.xsd`, `inc.xsd`, `fu-b.xsd` and nowhere else. Its result: the strict
/// validation of `<a:a><a:i> 7 </a:i><b:b>2026-10-27</b:b></a:a>`, typed.
fn imports_under(main: &str, location: &str, dir: &str) -> Result<String, XsltError> {
    let sheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xs="http://www.w3.org/2001/XMLSchema"
             xmlns:a="urn:fu-a" xmlns:b="urn:fu-b" exclude-result-prefixes="xs a b">
             <xsl:import-schema namespace="urn:fu-a" schema-location="{location}"/>
             <xsl:template match="/">
               <xsl:variable name="t"><a:a xsl:validation="strict"><a:i> 7 </a:i><b:b>2026-10-27</b:b></a:a></xsl:variable>
               <out><xsl:value-of select="data($t/a:a/a:i) + 1, $t/a:a/b:b instance of element(*, xs:date)"/></out>
             </xsl:template></xsl:stylesheet>"#
    );
    let mut mem = MemoryResolver::new();
    mem.insert(main, &sheet);
    mem.insert(format!("{dir}fu-a.xsd"), FU_A_XSD);
    mem.insert(format!("{dir}inc.xsd"), FU_INC_XSD);
    mem.insert(format!("{dir}fu-b.xsd"), FU_B_XSD);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(main, &functions)
        .and_then(|builder| builder.build())?;
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(main)?;
    let arena = Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = transformation.transform_source("urn:fu:in", "<in/>")?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// RFC 3986 §5.2 resolves every location (`xsl:import-schema`'s against the stylesheet, §3.14;
/// `xs:include` / `xs:import`'s against their schema document — Structures §4.2.3: "resolve the
/// ·actual value· of the schemaLocation … relative to the base URI"), §5.2.4 removing the dot
/// segments, and one document is one document however its location is spelled (Structures §4.2.3's
/// include "does not change" a schema that already includes it — `inc.xsd` twice is no duplicate
/// declaration). Under an `http:` base, a `file:` base and a relative one (a stylesheet named by a
/// relative URI: the locations stay relative, never resolved against the working directory).
#[test]
fn schema_locations_resolve_by_rfc_3986_under_every_base() {
    for (main, location, dir) in [
        (
            "http://h.example/s/x/main.xsl",
            "../y/../fu-a.xsd",
            "http://h.example/s/",
        ),
        (
            "file:///fu/c15a/x/main.xsl",
            "./../fu-a.xsd",
            "file:///fu/c15a/",
        ),
        ("rel/x/main.xsl", "../fu-a.xsd", "rel/"),
    ] {
        assert_eq!(
            imports_under(main, location, dir).unwrap_or_else(|e| panic!("{main}: {e}")),
            "<out>8 true</out>",
            "{main}"
        );
    }
}

// ══ `xml:base` and the base URI of a node ═════════════════════════════════════════════════════

/// Runs the module `main` (with `modules`, each `(uri, text)`) over the source `source_uri`, every
/// document served by a memory resolver (`docs`, `(uri, text)`); the primary result.
fn run_at(
    main: (&str, &str),
    modules: &[(&str, &str)],
    source: (&str, &str),
    docs: &[(&str, &str)],
) -> String {
    let mut mem = MemoryResolver::new();
    mem.insert(main.0, main.1);
    for (uri, text) in modules.iter().chain(docs) {
        mem.insert(*uri, *text);
    }
    mem.insert(source.0, source.1);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(main.0)
        .unwrap_or_else(|e| panic!("compiles: {e}"));
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_uri(source.0)
        .unwrap_or_else(|e| panic!("runs: {e}"));
    compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .expect("serializes")
}

/// §16.1 `document()`: a relative URI that is a node's value "is resolved against the base URI of
/// $base-node if supplied, or against the base URI of the node that contained it otherwise" — the
/// node's base URI being XDM's `base-uri` accessor, what `fn:base-uri` computes: every `xml:base`
/// from the outermost ancestor-or-self inward, resolved against the document's base URI (XML Base;
/// RFC 3986 §5.2). `DomNavigator::base_uri` is the **unresolved** `xml:base` in scope (upstream
/// C7's written contract), so the processor resolves the chain itself.
#[test]
fn document_resolves_against_the_node_s_resolved_base_uri() {
    let main = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
        <xsl:output method="text"/>
        <xsl:template match="/">
          <xsl:value-of select="document(//ref)/x, '|', document('x.xml', //ref[1])/x, '|',
            base-uri(//ref[1]), '|', resolve-uri('x.xml', base-uri(//ref[1]))"/>
        </xsl:template></xsl:stylesheet>"#;
    let source = r#"<in xml:base="a/"><p xml:base="b/"><ref>x.xml</ref></p></in>"#;
    assert_eq!(
        run_at(
            ("file:///fu/c7/main.xsl", main),
            &[],
            ("file:///fu/c7/src/in.xml", source),
            &[
                ("file:///fu/c7/src/a/b/x.xml", "<x>ab</x>"),
                ("file:///fu/c7/x.xml", "<x>stylesheet</x>"),
                ("file:///fu/c7/src/x.xml", "<x>source</x>"),
            ],
        ),
        "ab | ab | file:///fu/c7/src/a/b/ | file:///fu/c7/src/a/b/x.xml"
    );
}

/// The static base URI of an expression is "the base URI of the containing element in the
/// stylesheet" (§5.4.1), `xml:base` included (§5.8: "the base URI of a node in the stylesheet is
/// affected by xml:base attributes"), and a constructed element's is its literal result element's,
/// or its own `xml:base` resolved against it (§11.1.1, for a parentless one) — `static-base-uri()`,
/// `base-uri()` of a parentless element, of one in a temporary tree, and a `xsl:copy-of` of a source
/// element with a relative `xml:base` (§11.9.2: "the base URI of the new node is taken as the value
/// of the xml:base attribute, resolved if it is relative against the base URI of the xsl:copy-of
/// instruction").
#[test]
fn static_and_constructed_base_uris() {
    let main = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
          xml:base="m/">
        <xsl:output method="text"/>
        <xsl:template match="/">
          <xsl:variable name="free" as="element()"><e xml:base="e/"/></xsl:variable>
          <xsl:variable name="tree"><e><f xml:base="f/"/></e></xsl:variable>
          <xsl:variable name="copy" as="element()"><xsl:copy-of select="//s"/></xsl:variable>
          <xsl:value-of select="static-base-uri(), base-uri($free), base-uri($tree/e/f),
            base-uri($copy), base-uri(//s)" separator=" | "/>
        </xsl:template></xsl:stylesheet>"#;
    assert_eq!(
        run_at(
            ("file:///fu/c7/main.xsl", main),
            &[],
            ("file:///fu/c7/src/in.xml", r#"<in><s xml:base="s/"/></in>"#),
            &[],
        ),
        "file:///fu/c7/m/ | file:///fu/c7/m/e/ | file:///fu/c7/m/f/ | file:///fu/c7/m/s/ | \
         file:///fu/c7/src/s/"
    );
}

/// §11.9.2 `xsl:copy-of`: "The base URI of a node is copied" (an element with an `xml:base` of its
/// own aside) — a parentless copy of a source element or document node has the original's base URI,
/// the source's `xml:base` chain resolved, not the stylesheet's; and `document()` on a relative URI
/// inside the copy resolves against it.
#[test]
fn a_parentless_copy_keeps_the_base_uri_of_its_original() {
    let main = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
        <xsl:output method="text"/>
        <xsl:template match="/">
          <xsl:variable name="t" as="element()"><xsl:copy-of select="//t"/></xsl:variable>
          <xsl:variable name="d" as="document-node()"><xsl:copy-of select="/"/></xsl:variable>
          <xsl:value-of select="base-uri($t), base-uri($d), base-uri($d//t), document($t/@href)/d"
            separator=" | "/>
        </xsl:template></xsl:stylesheet>"#;
    assert_eq!(
        run_at(
            ("file:///fu/c7/main.xsl", main),
            &[],
            (
                "file:///fu/c7/src/in.xml",
                r#"<in><g xml:base="g/"><t href="d.xml"/></g></in>"#
            ),
            &[("file:///fu/c7/src/g/d.xml", "<d>gd</d>")],
        ),
        "file:///fu/c7/src/g/ | file:///fu/c7/src/in.xml | file:///fu/c7/src/g/ | gd"
    );
}

/// §11.9.1 `xsl:copy` of an element with `xml:base="s/"`: "the base URI of the new node is taken as
/// the value of the xml:base attribute, resolved if it is relative against the base URI of the
/// xsl:copy instruction" — once. A parentless copy whose content copies that `xml:base` attribute
/// too has the same base URI, and `document()` resolves a relative URI in it against that, not
/// against the `xml:base` applied a second time (`s/s/`).
#[test]
fn a_copied_xml_base_is_applied_once() {
    let main = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
        <xsl:output method="text"/>
        <xsl:template match="/">
          <xsl:variable name="a" as="element()"><xsl:for-each select="//s"><xsl:copy/></xsl:for-each></xsl:variable>
          <xsl:variable name="b" as="element()"><xsl:for-each select="//s"><xsl:copy><xsl:copy-of select="@*"/></xsl:copy></xsl:for-each></xsl:variable>
          <xsl:value-of select="base-uri($a), base-uri($b), document($b/@href)/d" separator=" | "/>
        </xsl:template></xsl:stylesheet>"#;
    assert_eq!(
        run_at(
            ("file:///fu/c7/main.xsl", main),
            &[],
            (
                "file:///fu/c7/src/in.xml",
                r#"<in><s xml:base="s/" href="d.xml"/></in>"#
            ),
            &[
                ("file:///fu/c7/s/d.xml", "<d>once</d>"),
                ("file:///fu/c7/s/s/d.xml", "<d>twice</d>"),
            ],
        ),
        "file:///fu/c7/s/ | file:///fu/c7/s/ | once"
    );
}

/// §11.1 (literal result elements) and §11.2 (`xsl:element`): "The base URI of the new element is
/// copied from the base URI of the literal result element in the stylesheet" — of the instruction,
/// so a parentless element keeps it wherever its base URI is asked: here by a stylesheet function in
/// a module of another directory (`lib/lib.xsl`). And a parentless copy keeps its original's
/// (§11.9.2).
#[test]
fn a_parentless_element_keeps_the_base_uri_of_its_instruction() {
    let main = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
          xmlns:f="urn:f" exclude-result-prefixes="f">
        <xsl:include href="lib/lib.xsl"/>
        <xsl:output method="text"/>
        <xsl:template match="/">
          <xsl:variable name="lre" as="element()"><e/></xsl:variable>
          <xsl:variable name="computed" as="element()"><xsl:element name="e"/></xsl:variable>
          <xsl:variable name="copy" as="element()"><xsl:copy-of select="//t"/></xsl:variable>
          <xsl:value-of select="f:base($lre), f:base($computed), f:base($copy)" separator=" | "/>
        </xsl:template></xsl:stylesheet>"#;
    let lib = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
          xmlns:f="urn:f">
        <xsl:function name="f:base"><xsl:param name="n"/><xsl:sequence select="base-uri($n)"/></xsl:function>
        </xsl:stylesheet>"#;
    assert_eq!(
        run_at(
            ("file:///fu/c7/main.xsl", main),
            &[("file:///fu/c7/lib/lib.xsl", lib)],
            (
                "file:///fu/c7/src/in.xml",
                r#"<in><g xml:base="g/"><t/></g></in>"#
            ),
            &[],
        ),
        "file:///fu/c7/main.xsl | file:///fu/c7/main.xsl | file:///fu/c7/src/g/"
    );
}
