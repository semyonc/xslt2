//! M4d, part 1 — the annotations of constructed attributes and elements (`xslt_doc/M4_PLAN.md`
//! §4 M4d; decisions D-5, D-10, D-12, D-13): `[xsl:]validation` `strict` / `preserve` / `strip` and
//! `type` on `xsl:attribute` (XSLT 2.0 §19.2.1.1, §19.2.1.2, §19.2.1.3), the rule that the outermost
//! enclosing boundary whose validation is not `preserve` decides a node's annotation, `preserve`,
//! `default-validation` per module (§3.6) and the typed temporary trees of §9.4 / §19.2.2.
//! Element validation (`strict` on a literal result element or `xsl:element`) is part 2's.
//!
//! Every test goes through the public API and cites the rule it checks (`xslt20.html`; XML Schema
//! 1.1 Part 1 `structures.html` for the attribute rules §19.2.1.1 refers to).

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::ids::TypeKey;
use xsd_schema::navigator::DomNavigator;
use xsd_schema::SchemaSet;

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Outcome, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4d/main.xsl";
const SOURCE: &str = "file:///m4d/doc.xml";

/// `urn:m4d`: user simple types (an integer range, a restriction of `xs:ID`, a list and a list of
/// fixed length, a restriction of `xs:QName`, a list of `xs:QName`, a union with an `xs:QName`
/// member, an enumeration over `xs:token`), top-level attribute declarations (one with a `fixed`
/// value, one with no type), and the element `doc` the source is validated against. For part 2:
/// `x` (a defaulted attribute, children `y` with `xs:ID` / `xs:IDREF` attributes and an
/// `xs:unique` on `@u`), `n` (`xs:integer` with a default value), `tok` (`xs:token`), `wrap` (a
/// sequence of `tok` / `n`, an attribute of type `size`). For the fix round: `fx` (local
/// declarations of `n` fixed at `8`, of `tok` as an `xs:string` fixed at `a b`, of `ga` with an
/// `xs:string` attribute fixed at `a b`, of `gid` with an `xs:string` attribute `id`), the global
/// `ga` (an `xs:token` attribute), the global `gid` (an `xs:ID` attribute `id`), `gdef` (the
/// global attribute `gsp`, an `xs:token` whose default has extra spaces) and `nums` (a list of
/// integers). For fix round 2: `nl` (a nillable `xs:integer`).
const M4D_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:m4d"
    xmlns:d="urn:m4d" targetNamespace="urn:m4d" elementFormDefault="qualified">
  <xs:element name="x">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="y" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="to" type="xs:IDREF"/>
            <xs:attribute name="u" type="xs:string"/>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
      <xs:attribute name="dflt" type="xs:string" default="abc"/>
      <xs:attribute ref="gdflt"/>
    </xs:complexType>
    <xs:unique name="unique-u"><xs:selector xpath="d:y"/><xs:field xpath="@u"/></xs:unique>
  </xs:element>
  <xs:attribute name="gdflt" type="xs:token" default="g"/>
  <xs:element name="n" type="xs:integer" default="7"/>
  <xs:element name="tok" type="xs:token"/>
  <xs:element name="wrap">
    <xs:complexType>
      <xs:choice minOccurs="0" maxOccurs="unbounded"><xs:element ref="tok"/><xs:element ref="n"/></xs:choice>
      <xs:attribute name="size" type="size"/>
    </xs:complexType>
  </xs:element>
  <xs:simpleType name="size">
    <xs:restriction base="xs:integer"><xs:minInclusive value="1"/><xs:maxInclusive value="20"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="code">
    <xs:restriction base="xs:ID"><xs:pattern value="[a-z]{4}"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="ints"><xs:list itemType="xs:integer"/></xs:simpleType>
  <xs:simpleType name="three">
    <xs:restriction base="ints"><xs:length value="3"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="qn"><xs:restriction base="xs:QName"/></xs:simpleType>
  <xs:simpleType name="qns"><xs:list itemType="xs:QName"/></xs:simpleType>
  <xs:simpleType name="intOrQName"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
  <xs:simpleType name="color">
    <xs:restriction base="xs:token"><xs:enumeration value="red"/><xs:enumeration value="green"/></xs:restriction>
  </xs:simpleType>
  <xs:attribute name="color" type="color"/>
  <xs:attribute name="one" type="xs:integer" fixed="1"/>
  <xs:attribute name="when" type="xs:date"/>
  <xs:attribute name="ref" type="xs:QName"/>
  <xs:attribute name="any"/>
  <xs:element name="fx">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="n" type="xs:integer" fixed="8" minOccurs="0"/>
        <xs:element name="tok" type="xs:string" fixed="a b" minOccurs="0"/>
        <xs:element name="ga" minOccurs="0">
          <xs:complexType><xs:attribute name="a" type="xs:string" fixed="a b"/></xs:complexType>
        </xs:element>
        <xs:element name="gid" minOccurs="0">
          <xs:complexType><xs:attribute name="id" type="xs:string"/></xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:element name="ga">
    <xs:complexType><xs:attribute name="a" type="xs:token"/></xs:complexType>
  </xs:element>
  <xs:element name="gid">
    <xs:complexType><xs:attribute name="id" type="xs:ID"/></xs:complexType>
  </xs:element>
  <xs:element name="gdef">
    <xs:complexType><xs:attribute ref="gsp"/></xs:complexType>
  </xs:element>
  <xs:attribute name="gsp" type="xs:token" default="  g   h "/>
  <xs:element name="nums" type="ints"/>
  <xs:element name="nl" type="xs:integer" nillable="true"/>
  <xs:element name="doc">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="item" maxOccurs="unbounded" nillable="true">
          <xs:complexType>
            <xs:simpleContent>
              <xs:extension base="xs:integer">
                <xs:attribute name="key" type="xs:ID"/>
                <xs:attribute ref="color"/>
              </xs:extension>
            </xs:simpleContent>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

/// Valid against `M4D_XSD`: two `d:item`s, the second nilled.
const DOC: &str = r#"<d:doc xmlns:d="urn:m4d" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><d:item key="k1" d:color="red">5</d:item><d:item key="k2" xsi:nil="true"/></d:doc>"#;

fn sheet(top: &str) -> String {
    sheet_with("", top)
}

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:d=\"urn:m4d\" \
           exclude-result-prefixes=\"xs d\" {attrs}>\
           <xsl:import-schema namespace=\"urn:m4d\" schema-location=\"m4d.xsd\"/>\
           {top}</xsl:stylesheet>"
    )
}

fn resolver(main: &str, more: &[(&str, &str)]) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert("file:///m4d/m4d.xsd", M4D_XSD);
    for (uri, text) in more {
        mem.insert(*uri, *text);
    }
    Arc::new(mem)
}

/// Schema-aware: compiles `main` against the context its `xsl:import-schema`s make, runs it over
/// `DOC` validated strictly, and hands the outcome and the schema set to `inspect`.
fn run_with<T>(
    main: &str,
    more: &[(&str, &str)],
    inspect: impl FnOnce(&Outcome<'_>, &SchemaSet, String) -> T,
) -> Result<T, XsltError> {
    let resolver = resolver(main, more);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)?
        .build()?;
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)?;
    let arena = Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = transformation.transform_source_with(SOURCE, DOC, SourceValidation::Strict)?;
    let text = common::body(compiled.serialize_primary(&outcome)?);
    Ok(inspect(&outcome, context.schema_set(), text))
}

fn run(main: &str) -> Result<String, XsltError> {
    run_with(main, &[], |_, _, text| text)
}

/// The text of `<out>…</out>` the template matching `/` writes with `body`, trimmed.
fn out(top: &str, body: &str) -> String {
    let main = sheet(&format!(
        r#"{top}<xsl:template match="/"><out><xsl:value-of select="{body}"/></out></xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => inner(&text),
        Err(e) => panic!("the run failed: {e}\n{body}"),
    }
}

/// The error the template matching `/` raises with `body` as its content.
fn error(top: &str, body: &str) -> XsltError {
    let main = sheet(&format!(
        r#"{top}<xsl:template match="/"><out>{body}</out></xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => panic!("expected an error, got {text}\n{body}"),
        Err(e) => e,
    }
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

/// A parentless attribute in a variable `$a` (§6.3: a node constructed into a sequence has no
/// parent), built by `attribute`.
fn parentless(attribute: &str) -> String {
    format!(r#"<xsl:variable name="a" as="attribute()">{attribute}</xsl:variable>"#)
}

// ── §19.2.1.2: type on xsl:attribute ─────────────────────────────────────────────────────────

/// §19.2.1.2: "If the [xsl:]type attribute is present, then the newly constructed element or
/// attribute is validated against the type definition identified by this attribute. … In the case
/// of an attribute, the attribute is considered valid if … the attribute's normalized value is
/// locally valid with respect to that type definition according to the rules for "String Valid""
/// — and §19.2.1.3: "the validity assessment process causes type annotations to be associated with
/// element and attribute nodes". A parentless attribute keeps the annotation (no enclosing
/// constructor strips it); its typed value is a value of the type, its string value the
/// [schema normalized value] (XDM: a validated attribute's string value).
#[test]
fn type_annotates_a_parentless_attribute() {
    let a = |attr: &str, test: &str| {
        let main = sheet(&format!(
            r#"<xsl:template match="/"><xsl:variable name="a" as="attribute()">{attr}</xsl:variable><out><xsl:value-of select="{test}"/></out></xsl:template>"#
        ));
        match run(&main) {
            Ok(text) => inner(&text),
            Err(e) => panic!("the run failed: {e}\n{attr}"),
        }
    };
    let size = r#"<xsl:attribute name="n" type="d:size"> 07 </xsl:attribute>"#;
    assert_eq!(
        a(
            size,
            "$a instance of attribute(n, d:size), $a instance of attribute(*, xs:integer), \
             data($a) instance of xs:integer, data($a) + 1, string($a) = '07'"
        ),
        "true true true 8 true"
    );
    assert_eq!(
        a(
            r#"<xsl:attribute name="n" type="xs:date" select="'2006-02-28'"/>"#,
            "data($a) instance of xs:date, $a instance of attribute(*, xs:untypedAtomic)"
        ),
        "true false"
    );
    // A list type: the value of a select is its items joined by spaces (§11.3), the typed value
    // their sequence.
    assert_eq!(
        a(
            r#"<xsl:attribute name="n" type="d:three" select="1 to 3"/>"#,
            "count(data($a)), sum(data($a)), data($a)[2] instance of xs:integer"
        ),
        "3 6 true"
    );
    // `xs:untypedAtomic`: every string is valid, and the attribute is what an unvalidated one is
    // (XPath 2.0 §2.5.1: "An attribute that has been validated in skip mode is represented in the
    // data model by an attribute node with the type annotation xs:untypedAtomic").
    assert_eq!(
        a(
            r#"<xsl:attribute name="n" type="xs:untypedAtomic" select="'abcd'"/>"#,
            "$a instance of attribute(*, xs:untypedAtomic), data($a) instance of xs:untypedAtomic"
        ),
        "true true"
    );
    // A token type: whitespace collapsed in the string value.
    assert_eq!(
        a(
            r#"<xsl:attribute name="n" type="xs:token" select="'  a    b '"/>"#,
            "concat('[', string($a), ']')"
        ),
        "[a b]"
    );
}

/// §19.2.1.2: "If the element or attribute is not considered valid, as defined above, the
/// transformation fails [see ERR XTTE1540]." — a facet (`maxInclusive`, `length`, `enumeration`)
/// and a built-in lexical / value-space rule.
#[test]
fn an_invalid_value_for_type_is_xtte1540() {
    for attribute in [
        r#"<xsl:attribute name="n" type="d:size">21</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="d:three" select="1 to 4"/>"#,
        r#"<xsl:attribute name="n" type="d:color">blue</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="xs:date">2006-02-31</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="xs:integer">fred</xsl:attribute>"#,
    ] {
        assert_eq!(error("", attribute).code, codes::XTTE1540, "{attribute}");
        // Also when the attribute lands in a sequence (a global variable is evaluated when it is
        // referenced).
        assert_eq!(
            error(&parentless(attribute), r#"<xsl:value-of select="$a"/>"#).code,
            codes::XTTE1540,
            "{attribute} (parentless)"
        );
    }
}

// ── §19.2.1.1: validation="strict" on xsl:attribute ─────────────────────────────────────────

/// §19.2.1.1: "In the case of an attribute, a top-level attribute declaration is identified whose
/// local name and namespace (if any) match the name of the attribute, and schema-validity
/// assessment is carried out … If the attribute is not considered valid, the transformation fails
/// [see ERR XTTE1510]." The annotation is the declaration's type (`schema-attribute(N)` matches).
#[test]
fn strict_validates_against_the_top_level_attribute_declaration() {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{}<out><xsl:value-of select="$a instance of schema-attribute(d:color), $a instance of attribute(*, d:color), concat('[', $a, ']')"/></out></xsl:template>"#,
        parentless(r#"<xsl:attribute name="d:color" validation="strict"> red </xsl:attribute>"#)
    ));
    assert_eq!(inner(&run(&main).unwrap()), "true true [red]");
    // A declaration without a type: xs:anySimpleType accepts anything.
    assert_eq!(
        out(
            &parentless(r#"<xsl:attribute name="d:any" validation="strict">x y</xsl:attribute>"#),
            "$a instance of schema-attribute(d:any)"
        ),
        "true"
    );
    for attribute in [
        r#"<xsl:attribute name="d:color" validation="strict">blue</xsl:attribute>"#,
        r#"<xsl:attribute name="d:when" validation="strict">2006-02-31</xsl:attribute>"#,
    ] {
        assert_eq!(error("", attribute).code, codes::XTTE1510, "{attribute}");
    }
}

/// XTTE1512: "If the validation attribute of an xsl:element, xsl:attribute, … has the effective
/// value strict, and there is no matching top-level declaration in the schema, then a type error
/// occurs." `key` is declared only locally, inside `d:item`'s type.
#[test]
fn strict_with_no_top_level_declaration_is_xtte1512() {
    for attribute in [
        r#"<xsl:attribute name="d:nosuch" validation="strict">1</xsl:attribute>"#,
        r#"<xsl:attribute name="key" validation="strict">k</xsl:attribute>"#,
    ] {
        assert_eq!(error("", attribute).code, codes::XTTE1512, "{attribute}");
    }
}

/// §19.2.1.1 assesses an attribute by "Schema-Validity Assessment (Attribute)", which (XML Schema
/// 1.1 Part 1 §3.2.4.3) evaluates "Attribute Locally Valid (§3.2.4.1)", whose clause 4 reads: "If
/// D.{value constraint} is present and D.{value constraint}.{variety} = fixed, then A's ·actual
/// value· is equal or identical to D.{value constraint}.{value}." — equal in the value space, so
/// `01` matches a fixed `1` of type `xs:integer`.
#[test]
fn strict_applies_the_fixed_value_of_the_declaration() {
    assert_eq!(
        out(
            &parentless(r#"<xsl:attribute name="d:one" validation="strict">01</xsl:attribute>"#),
            "data($a) = 1"
        ),
        "true"
    );
    assert_eq!(
        error(
            "",
            r#"<xsl:attribute name="d:one" validation="strict">2</xsl:attribute>"#
        )
        .code,
        codes::XTTE1510
    );
}

// ── §19.2.1.3: XTTE1545 ──────────────────────────────────────────────────────────────────────

/// XTTE1545: "A type error occurs if a type or validation attribute is defined (explicitly or
/// implicitly) for an instruction that constructs a new attribute node, if the effect of this is
/// to cause the attribute value to be validated against a type that is derived from, or
/// constructed by list or union from, the primitive types xs:QName or xs:NOTATION." — whether or
/// not the value would be valid.
#[test]
fn qname_and_notation_types_are_xtte1545() {
    for attribute in [
        r#"<xsl:attribute name="n" type="xs:QName" xmlns:a="urn:a">a:b</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="d:qn">d:b</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="d:qns">d:b d:c</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="d:intOrQName">12</xsl:attribute>"#,
        r#"<xsl:attribute name="n" type="xs:NOTATION">d:b</xsl:attribute>"#,
        r#"<xsl:attribute name="d:ref" validation="strict">d:b</xsl:attribute>"#,
    ] {
        assert_eq!(error("", attribute).code, codes::XTTE1545, "{attribute}");
    }
}

// ── §19.2.1.1: strip, preserve, and the annotation rule ─────────────────────────────────────

/// §19.2.1.1: "The value strip indicates that the new node and each of the contained nodes will
/// have the type annotation xs:untyped if it is an element, or xs:untypedAtomic if it is an
/// attribute. … The typed value of the node is changed to be the same as its string value, as an
/// instance of xs:untypedAtomic. … The values of the is-id and is-idrefs properties are
/// unchanged." A literal result element without `xsl:validation` in a module without
/// `default-validation` is `strip` (§19.2: "if this is not specified, the effect is the same as
/// specifying validation="strip""), so a typed attribute it contains loses its annotation but an
/// `xs:ID` one still identifies the element.
#[test]
fn a_strip_parent_strips_the_annotation_but_keeps_is_id() {
    let tree = r#"<xsl:variable name="t">
        <e><xsl:attribute name="i" type="xs:ID">e1</xsl:attribute><xsl:attribute name="n" type="xs:integer">5</xsl:attribute></e>
        <f><xsl:attribute name="c" type="d:code">abcd</xsl:attribute></f>
      </xsl:variable>"#;
    assert_eq!(
        out(
            tree,
            "$t/e/@n instance of attribute(*, xs:untypedAtomic), data($t/e/@n) instance of xs:untypedAtomic, \
             $t/e/@i instance of attribute(*, xs:ID), $t/e instance of element(*, xs:untyped), \
             exists(id('e1', $t)), name(id('abcd', $t))"
        ),
        "true true false true true f"
    );
}

/// §19.2.1.1: "The value preserve indicates that nodes that are copied will retain their type
/// annotations, but nodes whose content is newly constructed will be annotated as xs:anyType in the
/// case of elements … In the case of xsl:element and literal result elements, the new element has
/// a type annotation of xs:anyType, and the type annotations of contained nodes are retained
/// unchanged." An element of type `xs:anyType` has mixed content: its typed value is its string
/// value as `xs:untypedAtomic` (XDM).
#[test]
fn preserve_makes_a_new_element_xs_any_type_and_keeps_contained_annotations() {
    for (open, close) in [
        (r#"<e xsl:validation="preserve">"#, "</e>"),
        (
            r#"<xsl:element name="e" validation="preserve">"#,
            "</xsl:element>",
        ),
    ] {
        let tree = format!(
            r#"<xsl:variable name="t">{open}<xsl:attribute name="n" type="xs:integer">5</xsl:attribute><g/>text{close}</xsl:variable>"#
        );
        assert_eq!(
            out(
                &tree,
                "$t/e instance of element(e, xs:anyType), $t/e instance of element(*, xs:untyped), \
                 $t/e/@n instance of attribute(n, xs:integer), data($t/e) instance of xs:untypedAtomic, \
                 $t/e/g instance of element(*, xs:untyped)"
            ),
            "true false true true true",
            "{open}"
        );
    }
}

/// §19.2.1.1, `preserve`: "In the case of xsl:attribute, the effect is exactly the same as
/// specifying validation="strip": that is, the new attribute will have the type annotation
/// xs:untypedAtomic." And §11.1.2: a literal result element's own attributes are
/// `xs:untypedAtomic` — "If the xsl:validation attribute is set to preserve or strip, the type
/// annotation will be xs:untypedAtomic".
#[test]
fn preserve_on_xsl_attribute_and_attributes_of_a_literal_result_element_are_untyped() {
    let tree = r#"<xsl:variable name="t"><e xsl:validation="preserve" lit="1"><xsl:attribute name="p" validation="preserve">5</xsl:attribute></e></xsl:variable>"#;
    assert_eq!(
        out(
            tree,
            "$t/e/@p instance of attribute(*, xs:untypedAtomic), $t/e/@lit instance of attribute(*, xs:untypedAtomic)"
        ),
        "true true"
    );
}

/// §19.2.1.1: "Conceptually, the validation requested for a child element or attribute node is
/// applied before the validation requested for its parent element. For example, … if the
/// instruction that constructs its parent element specifies validation="strip", then the final
/// effect will be that the child node is annotated as xs:untyped." So the outermost enclosing
/// constructor whose validation is not `preserve` decides (M4 plan D-5). §19.2.2 for a document
/// node: with `strip` "elements within the tree have their type annotation set to xs:untyped".
/// (`xs:untyped` derives from `xs:anyType`, so the probe asks for `xs:untyped`.)
#[test]
fn the_outermost_boundary_that_is_not_preserve_decides() {
    let attribute = r#"<xsl:attribute name="n" type="xs:integer">5</xsl:attribute>"#;
    let cases = [
        // strip around preserve: stripped.
        (
            format!(r#"<o><e xsl:validation="preserve">{attribute}</e></o>"#),
            "false true",
        ),
        // preserve around preserve: kept.
        (
            format!(
                r#"<o xsl:validation="preserve"><e xsl:validation="preserve">{attribute}</e></o>"#
            ),
            "true false",
        ),
        // preserve around strip: stripped.
        (
            format!(r#"<o xsl:validation="preserve"><e>{attribute}</e></o>"#),
            "false true",
        ),
        // a strip document node inside a preserve element: stripped.
        (
            format!(
                r#"<o xsl:validation="preserve"><xsl:document><e xsl:validation="preserve">{attribute}</e></xsl:document></o>"#
            ),
            "false true",
        ),
        // ... and an empty one, before an attribute of the element, changes nothing.
        (
            format!(
                r#"<o xsl:validation="preserve"><xsl:document/><e xsl:validation="preserve">{attribute}</e></o>"#
            ),
            "true false",
        ),
    ];
    for (content, expected) in cases {
        let tree = format!(r#"<xsl:variable name="t">{content}</xsl:variable>"#);
        assert_eq!(
            out(
                &tree,
                "$t//e/@n instance of attribute(*, xs:integer), $t//e instance of element(*, xs:untyped)"
            ),
            expected,
            "{content}"
        );
    }
}

// ── §3.6: default-validation ─────────────────────────────────────────────────────────────────

/// §3.6: "The default-validation attribute defines the default value of the validation attribute
/// of all xsl:document, xsl:element, xsl:attribute, xsl:copy, xsl:copy-of, and xsl:result-document
/// instructions, and of the xsl:validation attribute of all literal result elements. … This
/// default applies within the stylesheet module: it does not extend to included or imported
/// stylesheet modules."
#[test]
fn default_validation_is_the_default_of_its_own_module_only() {
    let included = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:template name="inc"><i/></xsl:template></xsl:stylesheet>"#
    );
    let main = sheet_with(
        r#"default-validation="preserve""#,
        r#"<xsl:include href="inc.xsl"/>
           <xsl:template match="/">
             <xsl:variable name="t"><m><xsl:attribute name="n" type="xs:integer">5</xsl:attribute></m><xsl:call-template name="inc"/></xsl:variable>
             <out><xsl:value-of select="$t/m instance of element(*, xs:anyType), $t/m/@n instance of attribute(*, xs:integer), $t/i instance of element(*, xs:untyped)"/></out>
           </xsl:template>"#,
    );
    let text = run_with(&main, &[("file:///m4d/inc.xsl", &included)], |_, _, t| t)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(inner(&text), "true true true");
}

/// §3.6: `default-validation` "also determines the validation applied to the implicit final result
/// tree created in the absence of an xsl:result-document instruction" — the principal module's.
/// Neither value validates (§19.2.2: "The values validation="preserve" and validation="strip" do
/// not request validation"): with `preserve` the tree keeps what its nodes were given (a new
/// element is `xs:anyType`), with `strip` nothing in it is typed.
#[test]
fn the_implicit_final_result_tree_follows_the_principal_modules_default_validation() {
    let body = r#"<xsl:template match="/"><out><xsl:attribute name="n" type="xs:integer">5</xsl:attribute></out></xsl:template>"#;
    let (annotated, root_is_any_type, attribute_is_integer) = run_with(
        &sheet_with(r#"default-validation="preserve""#, body),
        &[],
        |outcome, set, _| {
            let doc = outcome.primary.expect("a primary result");
            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child());
            let root = nav
                .type_annotation()
                .is_some_and(|key| set.is_any_type(key));
            assert!(nav.move_to_first_attribute());
            let integer = set
                .get_built_in_simple_type_by_code(xsd_schema::types::XmlTypeCode::Integer)
                .map(TypeKey::Simple);
            (
                doc.has_type_annotations(),
                root,
                nav.type_annotation() == integer,
            )
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(annotated && root_is_any_type && attribute_is_integer);

    let annotated = run_with(&sheet(body), &[], |outcome, _, _| {
        outcome
            .primary
            .expect("a primary result")
            .has_type_annotations()
    })
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(!annotated, "strip: the final result tree is untyped");
}

// ── §9.4 / §19.2.2: temporary trees ──────────────────────────────────────────────────────────

/// §9.4: a temporary tree "can be regarded as a shorthand for … `<xsl:document
/// validation="preserve">`", and "No document-level validation takes place … However, type
/// annotations on nodes within the new tree are copied unchanged." (§19.2.2: "Document-level
/// validation is not applied to the document node that is created implicitly when a
/// variable-binding element has no select attribute and no as attribute … This is equivalent to
/// using validation="preserve" on xsl:document: nodes within such trees retain their type
/// annotation.") Nodes of the validated source sequenced into a temporary tree keep their
/// annotations, nilled and is-id; under a `strip` element they lose them (§19.2.1.1: "In the case
/// of elements the nilled property is set to false"), and `xsl:copy-of`, whose own validation is
/// `strip`, copies them untyped.
#[test]
fn a_temporary_tree_keeps_the_annotations_of_the_nodes_sequenced_into_it() {
    let probe = "data($t//d:item[1]) instance of xs:integer, $t//d:item[1]/@key instance of attribute(*, xs:ID), \
                 nilled(($t//d:item)[2]), exists(id('k1', $t))";
    let tree = |content: &str| format!(r#"<xsl:variable name="t">{content}</xsl:variable>"#);
    assert_eq!(
        out(&tree(r#"<xsl:sequence select="/d:doc/d:item"/>"#), probe),
        "true true true true"
    );
    assert_eq!(
        out(
            &tree(r#"<w><xsl:sequence select="/d:doc/d:item"/></w>"#),
            probe
        ),
        "false false false true"
    );
    assert_eq!(
        out(
            &tree(r#"<w xsl:validation="preserve"><xsl:sequence select="/d:doc/d:item"/></w>"#),
            probe
        ),
        "true true true true"
    );
    let copied = out(&tree(r#"<xsl:copy-of select="/d:doc/d:item"/>"#), probe);
    assert!(copied.starts_with("false false false"), "{copied}");
}

/// The attributes of a named attribute set are constructed by its `xsl:attribute` instructions
/// (§10.2), validated as those say.
#[test]
fn an_attribute_set_member_is_validated_by_its_own_type() {
    let top = r#"<xsl:attribute-set name="s"><xsl:attribute name="n" type="xs:integer">5</xsl:attribute></xsl:attribute-set>
                 <xsl:variable name="t"><e xsl:validation="preserve" xsl:use-attribute-sets="s"/></xsl:variable>"#;
    assert_eq!(
        out(top, "$t/e/@n instance of attribute(*, xs:integer)"),
        "true"
    );
    let bad = r#"<xsl:attribute-set name="s"><xsl:attribute name="n" type="xs:integer">x</xsl:attribute></xsl:attribute-set>"#;
    assert_eq!(
        error(bad, r#"<e xsl:use-attribute-sets="s"/>"#).code,
        codes::XTTE1540
    );
}

/// The XML namespace's attributes validated strictly against the declarations of `xml.xsd`
/// (a namespace-only `xsl:import-schema`, which finds `xsd-schema`'s embedded copy; §19.2.1.1).
///
/// That copy is the 2001 edition (`http://www.w3.org/2001/03/xml.xsd`): it declares no `xml:id`
/// and types `xml:lang` as `xs:language`, which has no empty value. The current edition
/// (`http://www.w3.org/2009/01/xml.xsd`, the catalog's `admin/xml.xsd`) declares `xml:id` of
/// type `xs:ID` and lets `xml:lang` be empty. The two lines marked C71 pin today's answers and
/// change when the embedded copy is updated (proposed upstream item C71).
#[test]
fn xml_namespace_attributes_validate_strictly() {
    let import = r#"<xsl:import-schema namespace="http://www.w3.org/XML/1998/namespace"/>"#;
    assert_eq!(
        out(
            &format!(
                r#"{import}{}"#,
                parentless(
                    r#"<xsl:attribute name="xml:space" validation="strict">preserve</xsl:attribute>"#
                )
            ),
            "$a instance of schema-attribute(xml:space)"
        ),
        "true"
    );
    assert_eq!(
        out(
            &format!(
                r#"{import}{}"#,
                parentless(
                    r#"<xsl:attribute name="xml:lang" validation="strict">en-US</xsl:attribute>"#
                )
            ),
            "$a instance of schema-attribute(xml:lang), data($a) instance of xs:language"
        ),
        "true true"
    );
    assert_eq!(
        error(
            import,
            r#"<xsl:attribute name="xml:space" validation="strict">foo</xsl:attribute>"#
        )
        .code,
        codes::XTTE1510
    );
    // The embedded schema for the XML namespace is the current W3C edition (upstream C71,
    // `wave-b` `cfe061e`): it declares xml:id (an xs:ID, so an invalid value is XTTE1510) and
    // allows an empty xml:lang.
    assert_eq!(
        out(
            &format!(
                r#"{import}{}"#,
                parentless(
                    r#"<xsl:attribute name="xml:id" validation="strict">x1</xsl:attribute>"#
                )
            ),
            "$a instance of schema-attribute(xml:id), data($a) instance of xs:ID"
        ),
        "true true"
    );
    assert_eq!(
        error(
            import,
            r#"<xsl:attribute name="xml:id" validation="strict">1x</xsl:attribute>"#
        )
        .code,
        codes::XTTE1510
    );
    assert_eq!(
        out(
            &format!(
                r#"{import}{}"#,
                parentless(r#"<xsl:attribute name="xml:lang" validation="strict" select="''"/>"#)
            ),
            "$a instance of schema-attribute(xml:lang)"
        ),
        "true"
    );
}

// ── What part 1 leaves fenced ───────────────────────────────────────────────────────────────

/// What part 1 fenced runs now. (Element validation, part 2, and `lax` on `xsl:attribute`,
/// decision D-d1, were fenced until part 2; copies and document nodes, M4e's, until M4e —
/// `tests/m4e_copies_documents.rs`; `lax` and `type` on elements, M4f's, until M4f —
/// `tests/m4f_type_lax.rs`.) An XPath 2.0 duration type an XML Schema 1.0 set had no definition of
/// was fenced, naming M4g and then upstream proposal C72, until the Wave B follow-up: upstream's
/// `SchemaSet::xpath_type` has it in every set (`tests/wave_b_followup.rs`), so the attribute is
/// validated against it and annotated with it.
#[test]
fn what_part_1_did_not_implement_runs() {
    assert_eq!(
        out(
            r#"<xsl:variable name="a" as="attribute()"><xsl:attribute name="n" type="xs:dayTimeDuration">PT1H</xsl:attribute></xsl:variable>"#,
            "$a instance of attribute(*, xs:dayTimeDuration), data($a) lt xs:dayTimeDuration('PT2H')"
        ),
        "true true"
    );
    // `default-validation="preserve"` gives `xsl:copy-of` the value `preserve` (§3.6), which runs
    // since M4e: the copy keeps its annotation, here in the implicit final result tree, which the
    // principal module's `preserve` does not strip (decision D-d3).
    let main = sheet_with(
        r#"default-validation="preserve""#,
        r#"<xsl:template match="/"><out><xsl:copy-of select="/d:doc/d:item[1]"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main).unwrap(),
        r#"<out><d:item xmlns:d="urn:m4d" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" key="k1" d:color="red">5</d:item></out>"#
    );
}

// ══ Part 2 (M4d-2): validation of constructed elements ════════════════════════════════════════

/// The answers `body` gives about the temporary tree `$t` built by `content` — a variable's
/// document node, which is `preserve` (§9.4), so the annotations the validation gives stay.
fn tree(content: &str, body: &str) -> String {
    out(
        &format!(r#"<xsl:variable name="t">{content}</xsl:variable>"#),
        body,
    )
}

/// §19.2.1.1: "In the case of an element, a top-level element declaration is identified whose
/// local name and namespace (if any) match the name of the element, and schema-validity
/// assessment is carried out according to the rules defined in [XML Schema Part 1] … The process
/// of validation applies recursively to contained elements and attributes to the extent required
/// by the schema definition." — for a literal result element (`xsl:validation`) and for
/// `xsl:element` (`validation`).
#[test]
fn strict_validates_an_element_against_its_top_level_declaration() {
    let probe = "$t/d:doc instance of schema-element(d:doc), data(($t//d:item)[1]) instance of xs:integer, \
                 ($t//d:item)[1]/@key instance of attribute(key, xs:ID), exists(id('k1', $t)), \
                 ($t//d:item)[1]/@d:color instance of schema-attribute(d:color), \
                 $t/d:doc instance of element(*, xs:untyped)";
    for (open, close) in [
        (r#"<d:doc xsl:validation="strict">"#, "</d:doc>"),
        (
            r#"<xsl:element name="d:doc" validation="strict">"#,
            "</xsl:element>",
        ),
    ] {
        let content = format!(
            r#"{open}<d:item key="k1" d:color=" red ">5</d:item><d:item>6</d:item>{close}"#
        );
        assert_eq!(
            tree(&content, probe),
            "true true true true true false",
            "{open}"
        );
    }
    // A parentless element in a sequence is validated the same way.
    assert_eq!(
        out(
            r#"<xsl:variable name="e" as="element()"><d:tok xsl:validation="strict">a</d:tok></xsl:variable>"#,
            "$e instance of schema-element(d:tok), data($e) instance of xs:token"
        ),
        "true true"
    );
}

/// XTTE1510: "If the validation attribute of an xsl:element … or the xsl:validation attribute of a
/// literal result element, has the effective value strict, and schema validity assessment
/// concludes that the validity of the element or attribute is invalid or unknown, a type error
/// occurs." — the content model, a simple value, an attribute of the element.
#[test]
fn an_invalid_element_is_xtte1510() {
    for content in [
        r#"<d:doc xsl:validation="strict"/>"#,
        r#"<d:doc xsl:validation="strict"><d:item>five</d:item></d:doc>"#,
        r#"<d:doc xsl:validation="strict"><d:item d:color="blue">5</d:item></d:doc>"#,
        r#"<d:doc xsl:validation="strict"><d:other/></d:doc>"#,
        r#"<d:wrap xsl:validation="strict" size="99"/>"#,
        r#"<xsl:element name="d:n" validation="strict">x</xsl:element>"#,
    ] {
        assert_eq!(error("", content).code, codes::XTTE1510, "{content}");
    }
}

/// XTTE1512: "… has the effective value strict, and there is no matching top-level declaration in
/// the schema, then a type error occurs." `d:item` is declared only locally, inside `d:doc`'s
/// type.
#[test]
fn strict_with_no_top_level_element_declaration_is_xtte1512() {
    for content in [
        r#"<d:nosuch xsl:validation="strict"/>"#,
        r#"<d:item xsl:validation="strict">5</d:item>"#,
        r#"<xsl:element name="plain" validation="strict"/>"#,
    ] {
        assert_eq!(error("", content).code, codes::XTTE1512, "{content}");
    }
}

/// §19.2.1.1's example: "if the instruction that constructs a child element specifies
/// validation="strict", this will cause the child element to be checked against an element
/// declaration, but if the instruction that constructs its parent element specifies
/// validation="strip", then the final effect will be that the child node is annotated as
/// xs:untyped." The child is still checked (an invalid one is XTTE1510), and the outermost
/// boundary that is not `preserve` decides the annotation (M4 plan D-5): a `strict` parent
/// re-annotates what a `strip` child stripped.
#[test]
fn the_outermost_validating_or_stripping_boundary_decides() {
    assert_eq!(
        tree(
            r#"<w><d:tok xsl:validation="strict">a</d:tok></w>"#,
            "$t/w/d:tok instance of element(*, xs:untyped)"
        ),
        "true"
    );
    assert_eq!(
        error("", r#"<w><d:n xsl:validation="strict">x</d:n></w>"#).code,
        codes::XTTE1510
    );
    assert_eq!(
        tree(
            r#"<d:wrap xsl:validation="strict"><d:n xsl:validation="strip">3</d:n></d:wrap>"#,
            "data($t/d:wrap/d:n) instance of xs:integer"
        ),
        "true"
    );
    assert_eq!(
        tree(
            r#"<w xsl:validation="preserve"><d:n xsl:validation="strict">3</d:n></w>"#,
            "data($t/w/d:n) instance of xs:integer, $t/w instance of element(w, xs:anyType)"
        ),
        "true true"
    );
}

/// M4 plan D-10, from §19.2.1.1: "Conceptually, the validation requested for a child element or
/// attribute node is applied before the validation requested for its parent element" — so with
/// both invalid, the child's failure is the one reported.
#[test]
fn the_innermost_failure_is_reported_first() {
    let e = error(
        "",
        r#"<d:wrap xsl:validation="strict" size="99"><d:n xsl:validation="strict">notanumber</d:n></d:wrap>"#,
    );
    assert_eq!(e.code, codes::XTTE1510);
    assert!(e.message.contains("notanumber"), "{}", e.message);
}

/// §19.2.1.3: "If default values for elements or attributes are defined in the schema, the
/// validation process will where necessary create new nodes containing these default values."
///
/// The defaulted attributes come from upstream's `get_default_attributes`, which reports every
/// defaulted attribute use with its type since `wave-b` `cfe061e` (upstream C74): the default of a
/// **local** declaration (`dflt`) is created too.
#[test]
fn validation_creates_defaulted_attributes_and_element_values() {
    assert_eq!(
        tree(
            r#"<d:x xsl:validation="strict"/>"#,
            "string($t/d:x/@d:gdflt), $t/d:x/@d:gdflt instance of schema-attribute(d:gdflt)"
        ),
        "g true"
    );
    // The local `dflt` (upstream C74).
    assert_eq!(
        tree(
            r#"<d:x xsl:validation="strict"/>"#,
            "count($t/d:x/@dflt), string($t/d:x/@dflt)"
        ),
        "1 abc"
    );
    assert_eq!(
        tree(
            r#"<d:x xsl:validation="strict" dflt="given" d:gdflt="mine"/>"#,
            "string($t/d:x/@dflt), string($t/d:x/@d:gdflt)"
        ),
        "given mine"
    );
    assert_eq!(
        tree(
            r#"<d:n xsl:validation="strict"/>"#,
            "string($t/d:n), data($t/d:n) instance of xs:integer"
        ),
        "7 true"
    );
}

/// §19.2.1.3: "The validation rule "Validation Root Valid (ID/IDREF)" is not applied. This means
/// that validation will not fail if there are non-unique ID values or dangling IDREF values in
/// the subtree being validated. The validation rule "Validation Rule: Identity-constraint
/// Satisfied" should be applied."
#[test]
fn id_idref_root_validity_is_not_applied_but_identity_constraints_are() {
    assert_eq!(
        tree(
            r#"<d:x xsl:validation="strict"><d:y id="a" u="1"/><d:y id="a" to="nowhere" u="2"/></d:x>"#,
            "count($t/d:x/d:y)"
        ),
        "2"
    );
    assert_eq!(
        error(
            "",
            r#"<d:x xsl:validation="strict"><d:y u="1"/><d:y u="1"/></d:x>"#
        )
        .code,
        codes::XTTE1510
    );
}

/// The nilled property comes from validation (XDM: an element validated with `xsi:nil="true"`
/// against a nillable declaration is nilled), and §19.2.1.3 maps the PSVI back; the string value of
/// a valid simple-content element is its [schema normalized value].
#[test]
fn nilled_and_the_normalized_value_come_from_the_validation() {
    assert_eq!(
        tree(
            r#"<d:doc xsl:validation="strict" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><d:item xsi:nil="true"/></d:doc>"#,
            "nilled($t//d:item)"
        ),
        "true"
    );
    assert_eq!(
        tree(
            r#"<d:tok xsl:validation="strict">  a   b  </d:tok>"#,
            "concat('[', string($t/d:tok), ']')"
        ),
        "[a b]"
    );
}

/// §19.2.1.1, `lax`: "In practice this means that the element or attribute being validated must
/// conform to its declaration if a top-level declaration is available. If no such declaration is
/// available, then the element or attribute is not validated" — and XTTE1515: "… has the effective
/// value lax, and schema validity assessment concludes that the element or attribute is invalid, a
/// type error occurs." (Decision D-d1: `lax` on `xsl:attribute` is M4d's; on elements M4f's.)
#[test]
fn lax_on_xsl_attribute() {
    assert_eq!(
        out(
            &parentless(r#"<xsl:attribute name="d:color" validation="lax">red</xsl:attribute>"#),
            "$a instance of schema-attribute(d:color)"
        ),
        "true"
    );
    assert_eq!(
        out(
            &parentless(r#"<xsl:attribute name="plain" validation="lax">anything</xsl:attribute>"#),
            "$a instance of attribute(*, xs:untypedAtomic)"
        ),
        "true"
    );
    assert_eq!(
        error(
            "",
            r#"<xsl:attribute name="d:color" validation="lax">blue</xsl:attribute>"#
        )
        .code,
        codes::XTTE1515
    );
}

/// `lax` and `type` on an element constructor were M4f's fence (M4_PLAN §4 M4f, upstream C36) and
/// run since M4f (`tests/m4f_type_lax.rs` checks what they do): a declared element under `lax` is
/// validated against its declaration (`d:n` is an `xs:integer`), an element with `type` against the
/// type (`d:size` is 1–20).
#[test]
fn lax_and_type_on_elements_run_since_m4f() {
    for (body, code) in [
        (r#"<d:n xsl:validation="lax">x</d:n>"#, codes::XTTE1515),
        (
            r#"<xsl:element name="d:n" validation="lax">x</xsl:element>"#,
            codes::XTTE1515,
        ),
        (r#"<d:n xsl:type="d:size">21</d:n>"#, codes::XTTE1540),
    ] {
        assert_eq!(error("", body).code, code, "{body}");
    }
}

/// §3.6 for the implicit final result tree: with the principal module's `preserve` the validated
/// element keeps its annotation; with `strip` (the default) — §19.2.2: "In the second case,
/// elements within the tree have their type annotation set to xs:untyped" — it does not.
#[test]
fn a_validated_element_in_the_final_result_tree() {
    let body = r#"<xsl:template match="/"><d:tok xsl:validation="strict">a</d:tok></xsl:template>"#;
    let typed = run_with(
        &sheet_with(r#"default-validation="preserve""#, body),
        &[],
        |outcome, set, _| {
            let doc = outcome.primary.expect("a primary result");
            let mut nav = doc.create_navigator();
            assert!(nav.move_to_first_child());
            let token = set
                .get_built_in_simple_type_by_code(xsd_schema::types::XmlTypeCode::Token)
                .map(TypeKey::Simple);
            nav.type_annotation() == token
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(typed);
    let annotated = run_with(&sheet(body), &[], |outcome, _, _| {
        outcome
            .primary
            .expect("a primary result")
            .has_type_annotations()
    })
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(!annotated);
}

// ══ Fix round: values, nesting, simple content, whitespace ═══════════════════════════════════

/// The serialized final result tree the template matching `/` writes with `body`, under the
/// default `default-validation="strip"` (§3.6).
fn final_tree(body: &str) -> String {
    let main = sheet(&format!(r#"<xsl:template match="/">{body}</xsl:template>"#));
    match run(&main) {
        Ok(text) => text.trim().to_string(),
        Err(e) => panic!("the run failed: {e}\n{body}"),
    }
}

/// §19.2.1.1: "Conceptually, the validation requested for a child element or attribute node is
/// applied before the validation requested for its parent element." — and §19.2.1.3 makes
/// validating an element "equivalent to" serializing it, parsing it, validating it and mapping
/// the PSVI back, so the parent's validation sees the child as the child's validation left it:
/// with the default value it created and with its [schema normalized value]s. `d:fx` declares a
/// local `n` fixed at `8` (the global `n` defaults to `7`), a local `tok` and a local `ga`
/// attribute as `xs:string`s fixed at `a b` (the global ones are `xs:token`s).
#[test]
fn a_parent_validates_what_the_child_validation_produced() {
    // The child takes its own default, 7, which is not the parent's fixed 8.
    assert_eq!(
        error(
            "",
            r#"<d:fx xsl:validation="strict"><d:n xsl:validation="strict"/></d:fx>"#
        )
        .code,
        codes::XTTE1510
    );
    // The child's normalized value meets the parent's fixed value; its raw text would not.
    assert_eq!(
        tree(
            r#"<d:fx xsl:validation="strict"><d:tok xsl:validation="strict">  a   b  </d:tok></d:fx>"#,
            "concat('[', string($t/d:fx/d:tok), ']')"
        ),
        "[a b]"
    );
    assert_eq!(
        tree(
            r#"<d:fx xsl:validation="strict"><d:ga xsl:validation="strict" a="  a   b "/></d:fx>"#,
            "concat('[', string($t/d:fx/d:ga/@a), ']')"
        ),
        "[a b]"
    );
    // Without a validation of its own the child is the parent's alone: the fixed value applies.
    assert_eq!(
        tree(
            r#"<d:fx xsl:validation="strict"><d:n/></d:fx>"#,
            "string($t/d:fx/d:n)"
        ),
        "8"
    );
}

/// §19.2.1.1: under `strip` "The typed value of the node is changed to be the same as its string
/// value, as an instance of xs:untypedAtomic" — the annotations go, the values stay — and
/// §19.2.1.3: "If default values for elements or attributes are defined in the schema, the
/// validation process will where necessary create new nodes containing these default values."
/// So a `strict` child of a `strip` parent keeps the default value its validation created and the
/// [schema normalized value]s it gave, untyped; the same in the implicit final result tree, whose
/// document node is `strip` by default (§3.6).
#[test]
fn values_the_validation_produced_survive_a_strip_boundary() {
    assert_eq!(
        tree(
            r#"<w xsl:validation="strip"><d:n xsl:validation="strict"/></w>"#,
            "string($t/w/d:n), $t/w/d:n instance of element(*, xs:untyped)"
        ),
        "7 true"
    );
    assert_eq!(
        tree(
            r#"<w xsl:validation="strip"><d:wrap xsl:validation="strict" size="  3  "/></w>"#,
            "concat('[', string($t/w/d:wrap/@size), ']'), $t/w/d:wrap/@size instance of attribute(*, xs:untypedAtomic)"
        ),
        "[3] true"
    );
    assert_eq!(
        tree(
            r#"<w xsl:validation="strip"><d:tok xsl:validation="strict">  a   b  </d:tok></w>"#,
            "concat('[', string($t/w/d:tok), ']')"
        ),
        "[a b]"
    );
    // A defaulted attribute's [schema normalized value] is "The ·effective value constraint·'s
    // {lexical form}" (XML Schema 1.1 §3.4.5.1), "the ·normalized value· (with respect to the
    // {type definition}) of the [attribute]" (§3.2.2.1): `  g   h ` as an `xs:token` is `g h`.
    for content in [
        r#"<w xsl:validation="strip"><d:gdef xsl:validation="strict"/></w>"#,
        r#"<w xsl:validation="preserve"><d:gdef xsl:validation="strict"/></w>"#,
    ] {
        assert_eq!(
            tree(content, "concat('[', string($t/w/d:gdef/@d:gsp), ']')"),
            "[g h]",
            "{content}"
        );
    }
    assert_eq!(
        final_tree(r#"<d:n xsl:validation="strict"/>"#),
        r#"<d:n xmlns:d="urn:m4d">7</d:n>"#
    );
    assert_eq!(
        final_tree(r#"<d:wrap xsl:validation="strict" size=" 3 "/>"#),
        r#"<d:wrap xmlns:d="urn:m4d" size="3"/>"#
    );
}

/// §5.7.1: "Any document node within the result sequence is replaced by a sequence containing
/// each of its children, in document order." — the text an `xsl:document` (or an `xsl:copy` of a
/// document node) contributes is the enclosing element's content: it is what the validation of
/// that element sees, it makes the element non-empty (so no default value is added), and it is
/// part of the text the element's [schema normalized value] replaces.
#[test]
fn a_document_node_in_element_content_is_the_elements_content() {
    assert_eq!(
        tree(
            r#"<d:n xsl:validation="strict"><xsl:document>3</xsl:document></d:n>"#,
            "string($t/d:n)"
        ),
        "3"
    );
    assert_eq!(
        tree(
            r#"<d:n xsl:validation="strict"><xsl:text> </xsl:text><xsl:document>4</xsl:document><xsl:text> </xsl:text></d:n>"#,
            "concat('[', string($t/d:n), ']'), count($t/d:n/text())"
        ),
        "[4] 1"
    );
    assert_eq!(
        tree(
            r#"<d:n xsl:validation="strict"><xsl:for-each select="/"><xsl:copy>5</xsl:copy></xsl:for-each></d:n>"#,
            "string($t/d:n)"
        ),
        "5"
    );
    // An empty document node contributes nothing: the element is empty and takes its default.
    assert_eq!(
        tree(
            r#"<d:n xsl:validation="strict"><xsl:document/></d:n>"#,
            "string($t/d:n)"
        ),
        "7"
    );
}

/// §5.7.2: "Zero-length text nodes in the sequence are discarded. Adjacent text nodes in the
/// sequence are merged into a single text node. The sequence is atomized. Every value in the
/// atomized sequence is cast to a string. The strings within the resulting sequence are
/// concatenated, with a (possibly zero-length) separator inserted between successive strings." —
/// an element constructed in the content of `xsl:value-of`, `xsl:attribute`, `xsl:comment`,
/// `xsl:processing-instruction` or `xsl:namespace` is a node like any other: its validation runs
/// (decision D-d11 reversed), and it contributes its typed value. An `xsl:message` builds a
/// document node (§17), whose elements are validated too.
#[test]
fn elements_constructed_for_simple_content_are_validated() {
    let strict = r#"<d:n xsl:validation="strict">01</d:n>"#;
    let defaulted = r#"<d:n xsl:validation="strict"/>"#;
    let body = |body: &str| final_tree(&format!("<out>{body}</out>"));
    assert_eq!(
        body(&format!("<xsl:value-of>{strict}</xsl:value-of>")),
        "<out>1</out>"
    );
    assert_eq!(
        body(&format!("<xsl:value-of>{defaulted}</xsl:value-of>")),
        "<out>7</out>"
    );
    assert_eq!(
        body(&format!(
            r#"<xsl:attribute name="v">{strict}</xsl:attribute>"#
        )),
        r#"<out v="1"/>"#
    );
    assert_eq!(
        body(&format!("<xsl:comment>{strict}</xsl:comment>")),
        "<out><!--1--></out>"
    );
    assert_eq!(
        body(&format!(
            r#"<xsl:processing-instruction name="p">{defaulted}</xsl:processing-instruction>"#
        )),
        "<out><?p 7?></out>"
    );
    assert_eq!(
        body(
            r#"<xsl:namespace name="p"><d:tok xsl:validation="strict">  urn:x  </d:tok></xsl:namespace>"#
        ),
        r#"<out xmlns:p="urn:x"/>"#
    );
    // A list type atomizes to its items, each cast to a string, joined by the separator.
    assert_eq!(
        body(
            r#"<xsl:value-of separator=","><d:nums xsl:validation="strict">1 2  3</d:nums></xsl:value-of>"#
        ),
        "<out>1,2,3</out>"
    );
    // A document node atomizes to its string value, with the default its element took.
    assert_eq!(
        body(&format!(
            "<xsl:value-of><xsl:document>{defaulted}</xsl:document></xsl:value-of>"
        )),
        "<out>7</out>"
    );
    // XPath 2.0 §2.5.2: "If the type annotation denotes a complex type with element-only content,
    // then the typed value of the node is undefined. The fn:data function raises a type error
    // [err:FOTY0012] when applied to such a node."
    assert_eq!(
        error(
            "",
            r#"<xsl:value-of><d:wrap xsl:validation="strict"><d:tok>a</d:tok></d:wrap></xsl:value-of>"#
        )
        .code,
        "FOTY0012"
    );
    for invalid in [
        r#"<xsl:value-of><d:n xsl:validation="strict">x</d:n></xsl:value-of>"#,
        r#"<xsl:attribute name="v"><d:n xsl:validation="strict">x</d:n></xsl:attribute>"#,
        r#"<xsl:message terminate="yes"><d:n xsl:validation="strict">x</d:n></xsl:message>"#,
    ] {
        assert_eq!(error("", invalid).code, codes::XTTE1510, "{invalid}");
    }
}

/// XDM 1.0 §6.7.4 (Construction from a PSVI, the mapping §19.2.1.3 applies): "if the resulting
/// Text Node consists entirely of whitespace and the character information items used to
/// construct this node have a parent and that parent is an element and its {content type} is not
/// "mixed", then the content of the Text Node is the zero-length string" — and "an empty Text
/// Node will be discarded when its parent is constructed". A value, not an annotation: it holds
/// under a `strip` boundary too. Unvalidated, the whitespace stays.
#[test]
fn whitespace_in_validated_element_only_content_is_no_text_node() {
    let wrap = |validation: &str| {
        format!(
            r#"<d:wrap xsl:validation="{validation}"><xsl:text> </xsl:text><d:tok>a</d:tok><xsl:text>  </xsl:text><xsl:comment>c</xsl:comment><xsl:text> </xsl:text></d:wrap>"#
        )
    };
    assert_eq!(
        tree(
            &wrap("strict"),
            "count($t/d:wrap/text()), count($t/d:wrap/comment()), string($t/d:wrap)"
        ),
        "0 1 a"
    );
    assert_eq!(
        tree(
            &format!(r#"<w xsl:validation="strip">{}</w>"#, wrap("strict")),
            "count($t/w/d:wrap/text())"
        ),
        "0"
    );
    assert_eq!(tree(&wrap("strip"), "count($t/d:wrap/text())"), "3");
    assert_eq!(
        final_tree(&wrap("strict")),
        r#"<d:wrap xmlns:d="urn:m4d"><d:tok>a</d:tok><!--c--></d:wrap>"#
    );
}

/// §19.2.1.1 under `strip`: "The values of the is-id and is-idrefs properties are unchanged" —
/// unchanged from what the node was just before the strip, which is what the outermost validation
/// inside the `strip` boundary made of it (each validation re-assesses what the inner ones left,
/// §19.2.1.3). The global `gid` declares `id` an `xs:ID`; `fx`'s local `gid` declares it an
/// `xs:string`.
#[test]
fn is_id_under_a_strip_boundary_is_the_outermost_validations() {
    assert_eq!(
        tree(
            r#"<w xsl:validation="strip"><d:gid xsl:validation="strict" id="k"/></w>"#,
            "count(id('k', $t))"
        ),
        "1"
    );
    assert_eq!(
        tree(
            r#"<w xsl:validation="strip"><d:fx xsl:validation="strict"><d:gid xsl:validation="strict" id="k"/></d:fx></w>"#,
            "count(id('k', $t))"
        ),
        "0"
    );
}

/// §5.7.2 step 3 ("The sequence is atomized") for nodes that already exist: a typed node
/// sequenced into the content of `xsl:value-of` contributes its typed value, exactly as it does
/// through `select` (the same atomization), while a copy (`xsl:copy-of`, `validation="strip"`)
/// is untyped and contributes its string value. `@size` is an `xs:integer` whose string value is
/// `07`; `d:nums` a list of integers.
#[test]
fn an_existing_typed_node_in_simple_content_contributes_its_typed_value() {
    let content = r#"<d:wrap xsl:validation="strict" size="07"/><d:nums xsl:validation="strict">1 2 3</d:nums>"#;
    let main = |body: &str| {
        sheet(&format!(
            r#"<xsl:variable name="t">{content}</xsl:variable><xsl:template match="/"><out>{body}</out></xsl:template>"#
        ))
    };
    let text = |body: &str| match run(&main(body)) {
        Ok(text) => inner(&text),
        Err(e) => panic!("the run failed: {e}\n{body}"),
    };
    assert_eq!(text(r#"<xsl:value-of select="$t/d:wrap/@size"/>"#), "7");
    assert_eq!(
        text(r#"<xsl:value-of><xsl:sequence select="$t/d:wrap/@size"/></xsl:value-of>"#),
        "7"
    );
    assert_eq!(
        text(r#"<xsl:value-of separator=","><xsl:sequence select="$t/d:nums"/></xsl:value-of>"#),
        "1,2,3"
    );
    assert_eq!(
        text(r#"<xsl:value-of separator=","><xsl:copy-of select="$t/d:nums"/></xsl:value-of>"#),
        "1 2 3"
    );
}

// ══ Fix round 2: atomization in simple content ═══════════════════════════════════════════════

/// The text `<out>` gets from `body`, with `$t` a `preserve` temporary tree built from `content`
/// (so its nodes keep their annotations).
fn simple(content: &str, body: &str) -> String {
    let main = sheet(&format!(
        r#"<xsl:variable name="t">{content}</xsl:variable><xsl:template match="/"><out>{body}</out></xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => text.trim().to_string(),
        Err(e) => panic!("the run failed: {e}\n{body}"),
    }
}

/// §5.7.2 step 3, "The sequence is atomized", for attribute nodes: a parentless attribute that
/// `xsl:attribute` constructs with `type`, `validation="strict"` or `validation="lax"` in simple
/// content contributes its typed value — the integer `01` is `1`, a list one string per item,
/// joined by the separator — like an existing typed attribute node and like an attribute of a
/// constructed element that keeps its annotation (`preserve`). Every simple-content instruction
/// with a sequence constructor goes through the same collector: `xsl:value-of`, `xsl:attribute`,
/// `xsl:comment`, `xsl:processing-instruction` (and `xsl:namespace`).
#[test]
fn a_typed_attribute_in_simple_content_contributes_its_typed_value() {
    let int = r#"<xsl:attribute name="a" type="xs:integer">01</xsl:attribute>"#;
    let list = r#"<xsl:attribute name="a" type="d:ints">1 2  3</xsl:attribute>"#;
    let cases = [
        (format!("<xsl:value-of>{int}</xsl:value-of>"), "<out>1</out>"),
        (
            format!(r#"<xsl:value-of separator=",">{list}</xsl:value-of>"#),
            "<out>1,2,3</out>",
        ),
        (
            r#"<xsl:value-of><xsl:attribute name="d:one" validation="strict">01</xsl:attribute></xsl:value-of>"#
                .to_string(),
            "<out>1</out>",
        ),
        (
            r#"<xsl:value-of><xsl:attribute name="d:one" validation="lax">01</xsl:attribute></xsl:value-of>"#
                .to_string(),
            "<out>1</out>",
        ),
        (
            format!(r#"<xsl:attribute name="v" separator=",">{list}</xsl:attribute>"#),
            r#"<out v="1,2,3"/>"#,
        ),
        (format!("<xsl:comment>{int}</xsl:comment>"), "<out><!--1--></out>"),
        (
            format!(r#"<xsl:processing-instruction name="p">{int}</xsl:processing-instruction>"#),
            "<out><?p 1?></out>",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(simple("", &body), expected, "{body}");
    }
    // An existing typed attribute node, and the typed attribute of a constructed element that
    // keeps its annotation.
    let element = r#"<e xsl:validation="preserve"><xsl:attribute name="a" type="d:ints">4 5</xsl:attribute></e>"#;
    assert_eq!(
        simple(
            element,
            r#"<xsl:value-of separator=","><xsl:sequence select="$t/e/@a"/></xsl:value-of>"#
        ),
        "<out>4,5</out>"
    );
}

/// §5.7.2: "Zero-length text nodes in the sequence are discarded. Adjacent text nodes in the
/// sequence are merged into a single text node." Two text nodes with another item between them
/// are not adjacent, whatever that item atomizes to — so an item that atomizes to the empty
/// sequence (a nilled element, an empty list, a complex type with empty content: XPath 2.0
/// §2.5.2, "the typed value of the node is the empty sequence") still separates them: `a|b`.
#[test]
fn an_item_that_atomizes_to_nothing_still_separates_text() {
    let between = |item: &str| {
        format!(
            r#"<xsl:value-of separator="|"><xsl:text>a</xsl:text>{item}<xsl:text>b</xsl:text></xsl:value-of>"#
        )
    };
    let nilled = r#"<d:nl xsl:validation="strict" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>"#;
    for item in [
        // Constructed: nilled, an empty list, empty content.
        nilled,
        r#"<d:nums xsl:validation="strict"/>"#,
        r#"<d:ga xsl:validation="strict"/>"#,
        // Existing: the source's nilled `d:item`, and a constructed nilled element kept typed.
        r#"<xsl:sequence select="(//d:item)[2]"/>"#,
        r#"<xsl:sequence select="$t/d:nl"/>"#,
    ] {
        assert_eq!(simple(nilled, &between(item)), "<out>a|b</out>", "{item}");
    }
    assert_eq!(
        simple(
            "",
            &format!(
                r#"<xsl:attribute name="v" separator="|"><xsl:text>a</xsl:text>{nilled}<xsl:text>b</xsl:text></xsl:attribute>"#
            )
        ),
        r#"<out v="a|b"/>"#
    );
}
