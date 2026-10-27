//! M4f — type-driven and lax validation roots (`xslt_doc/M4_PLAN.md` §4 M4f; decisions D-5, D-10,
//! D-12; upstream C36, `xsd_schema::validation::ValidationRoot`): `[xsl:]type` on literal result
//! elements, `xsl:element`, `xsl:copy`, `xsl:copy-of`, `xsl:document` and `xsl:result-document`
//! (XSLT 2.0 §19.2.1.2, §19.2.2), `validation="lax"` on all of them (§19.2.1.1), XTTE1515 and
//! XTTE1540 for elements, and `SourceValidation::Lax` for a source whose root has no declaration.
//!
//! Every test goes through the public API and cites the rule it checks (`xslt20.html`; XML Schema
//! Part 1 for the assessment rules §19.2.1.2 refers to).

mod common;

use std::sync::Arc;

use bumpalo::Bump;

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4f/main.xsl";
const SOURCE: &str = "file:///m4f/doc.xml";
const XSD_URI: &str = "file:///m4f/m4f.xsd";

/// `urn:m4f`: simple types `small` (an integer up to 10), `ints` (a list of integers), `qn` (a
/// restriction of `xs:QName`); complex types `person` (`name`, an optional `age`, an `xs:ID`
/// attribute `id`), `employee` (an extension of `person`), `sealed` (`block="extension"`) and its
/// extension `sealedPlus`, the abstract `shape` and its extension `square`, `price` (simple content,
/// an attribute `cur`), `refs` (a sequence of `r`), `bag` (a lax wildcard); global elements `r`
/// (`xs:ID` `id`, `xs:IDREF` `to`), `refs`, `n` (`small`), `card` (`xs:string`), `person`, `bag`;
/// global attributes `ga` (`xs:integer`) and `gq` (`xs:QName`).
const M4F_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:m4f"
    targetNamespace="urn:m4f" elementFormDefault="qualified">
  <xs:simpleType name="small">
    <xs:restriction base="xs:integer"><xs:maxInclusive value="10"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="ints"><xs:list itemType="xs:integer"/></xs:simpleType>
  <xs:simpleType name="qn"><xs:restriction base="xs:QName"/></xs:simpleType>
  <xs:complexType name="person">
    <xs:sequence>
      <xs:element name="name" type="xs:string"/>
      <xs:element name="age" type="xs:integer" minOccurs="0"/>
    </xs:sequence>
    <xs:attribute name="id" type="xs:ID"/>
  </xs:complexType>
  <xs:complexType name="employee">
    <xs:complexContent>
      <xs:extension base="person">
        <xs:sequence><xs:element name="salary" type="xs:decimal"/></xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
  <xs:complexType name="sealed" block="extension">
    <xs:sequence><xs:element name="name" type="xs:string"/></xs:sequence>
  </xs:complexType>
  <xs:complexType name="sealedPlus">
    <xs:complexContent>
      <xs:extension base="sealed">
        <xs:sequence><xs:element name="more" type="xs:string"/></xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
  <xs:complexType name="shape" abstract="true">
    <xs:attribute name="w" type="xs:integer"/>
  </xs:complexType>
  <xs:complexType name="square">
    <xs:complexContent><xs:extension base="shape"/></xs:complexContent>
  </xs:complexType>
  <xs:complexType name="price">
    <xs:simpleContent>
      <xs:extension base="xs:decimal"><xs:attribute name="cur" type="xs:token"/></xs:extension>
    </xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="refs">
    <xs:sequence><xs:element ref="r" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
  </xs:complexType>
  <xs:complexType name="bag">
    <xs:sequence><xs:any processContents="lax" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
  </xs:complexType>
  <xs:element name="r">
    <xs:complexType>
      <xs:attribute name="id" type="xs:ID"/>
      <xs:attribute name="to" type="xs:IDREF"/>
    </xs:complexType>
  </xs:element>
  <xs:element name="refs" type="refs"/>
  <xs:element name="n" type="small"/>
  <xs:element name="card" type="xs:string"/>
  <xs:element name="person" type="person"/>
  <xs:element name="bag" type="bag"/>
  <xs:attribute name="ga" type="xs:integer"/>
  <xs:attribute name="gq" type="xs:QName"/>
</xs:schema>"#;

fn sheet(top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:f=\"urn:m4f\" \
           xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
           exclude-result-prefixes=\"xs f xsi\">\
           <xsl:import-schema namespace=\"urn:m4f\" schema-location=\"m4f.xsd\"/>\
           {top}</xsl:stylesheet>"
    )
}

fn resolver(main: &str) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, M4F_XSD);
    Arc::new(mem)
}

/// Schema-aware: compiles `main` against the context its `xsl:import-schema` makes and runs it over
/// `source`, validated as `validation` says; the serialized primary result.
fn run_over(main: &str, source: &str, validation: SourceValidation) -> Result<String, XsltError> {
    let resolver = resolver(main);
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
    let outcome = transformation.transform_source_with(SOURCE, source, validation)?;
    Ok(common::body(compiled.serialize_primary(&outcome)?))
}

fn run(main: &str) -> Result<String, XsltError> {
    run_over(main, "<src/>", SourceValidation::Skip)
}

/// The text of `<out>…</out>` the template matching `/` writes: the declarations `top` (variables)
/// in the template, then `<xsl:value-of select="{body}"/>`.
fn answers(top: &str, body: &str) -> String {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{top}<out><xsl:value-of select="{body}"/></out></xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => inner(&text),
        Err(e) => panic!("the run failed: {e}\n{top}\n{body}"),
    }
}

/// The serialized result of the template matching `/` with `content`.
fn result(content: &str) -> String {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{content}</xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => text,
        Err(e) => panic!("the run failed: {e}\n{content}"),
    }
}

/// The error the template matching `/` raises with `content`.
fn error(content: &str) -> XsltError {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{content}</xsl:template>"#
    ));
    match run(&main) {
        Ok(text) => panic!("expected an error, got {text}\n{content}"),
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

/// Every `(content, code)` raises `code`; the ones that do not are listed together.
fn assert_codes(cases: &[(&str, &str)]) {
    let mut wrong = Vec::new();
    for (content, code) in cases {
        let main = sheet(&format!(
            r#"<xsl:template match="/">{content}</xsl:template>"#
        ));
        match run(&main) {
            Ok(text) => wrong.push(format!(
                "{content}\n    expected {code}, got the result {text}"
            )),
            Err(e) if e.code == *code => {}
            Err(e) => wrong.push(format!("{content}\n    expected {code}, got {e}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// ── §19.2.1.2: [xsl:]type on element constructors ────────────────────────────────────────────

/// §19.2.1.2: "If the [xsl:]type attribute is present, then the newly constructed element or
/// attribute is validated against the type definition identified by this attribute. In the case
/// of an element, schema-validity assessment is carried out according to the rules defined in
/// [XML Schema Part 1] (section 3.3.4 "Element Declaration Validation Rules", validation rule
/// "Schema-Validity Assessment (Element)", clauses 1.2 and 2), using this type definition as the
/// "processor-stipulated type definition"." — on a literal result element and on `xsl:element`; a
/// simple type, a complex type with element content, one with simple content; the element needs no
/// declaration, and a declaration of its name does not govern (`card` is declared `xs:string`, the
/// stipulated `small` decides). The value is the [schema normalized value] (§19.2.1.3's
/// serialize–parse–validate equivalence).
#[test]
fn type_on_an_element_validates_it_against_the_named_type() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="a" as="element()"><f:x xsl:type="f:small"> 07 </f:x></xsl:variable>
               <xsl:variable name="b" as="element()"><xsl:element name="f:x" type="f:person"><f:name>Ann</f:name><f:age> 41 </f:age></xsl:element></xsl:variable>
               <xsl:variable name="c" as="element()"><f:card xsl:type="f:small">3</f:card></xsl:variable>
               <xsl:variable name="p" as="element()"><f:x xsl:type="f:price" cur="  EUR  "> 1.50 </f:x></xsl:variable>"#,
            "$a instance of element(*, f:small), data($a) + 1, string($a), \
             $b instance of element(*, f:person), $b/f:age instance of element(*, xs:integer), \
             string($b/f:age), \
             $c instance of element(*, f:small), $c instance of schema-element(f:card), \
             data($p) * 2, string($p/@cur), $p/@cur instance of attribute(*, xs:token)"
        ),
        "true 8 07 true true 41 true false 3 EUR true"
    );
}

/// §19.2.1.2: "If the element or attribute is not considered valid, as defined above, the
/// transformation fails [see ERR XTTE1540]" — "[ERR XTTE1540] It is a type error if an [xsl:]type
/// attribute is defined for a constructed element or attribute, and the outcome of schema validity
/// assessment against that type is that the validity property of that element or attribute
/// information item is other than valid." A facet, a missing child, an attribute on a simple type,
/// an unexpected child, and a value the declaration of the name would accept but the stipulated
/// type does not; in the final result tree too, whose `strip` (the default) still validates.
#[test]
fn an_element_invalid_for_its_type_is_xtte1540() {
    assert_codes(&[
        (
            r#"<out><f:x xsl:type="f:small">11</f:x></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><xsl:element name="f:x" type="f:person"><f:age>1</f:age></xsl:element></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:x xsl:type="f:small" a="1">1</f:x></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:x xsl:type="f:person"><f:name>a</f:name><f:other/></f:x></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:card xsl:type="f:small">abc</f:card></out>"#,
            codes::XTTE1540,
        ),
    ]);
}

/// `xsi:type` under a stipulated type. §19.2.1.1's note: "If an element that is being validated
/// has an xsi:type attribute, then the value of the xsi:type attribute will be taken into account
/// when performing the validation." XML Schema 1.0 §3.3.4 clause 1.2.1.2.4 (the version XSLT 2.0
/// refers to; quoted in `WAVE_B_BATCH_R_REPORT.md`): "If there is also a processor-stipulated type
/// definition, the ·local type definition· must be validly derived from that type definition given
/// its {prohibited substitutions}" — a derived type overrides; a type not derived from it, or one
/// the stipulated type's `block` prohibits, makes the element invalid: XTTE1540.
#[test]
fn xsi_type_under_a_stipulated_type() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="e" as="element()"><f:x xsl:type="f:person" xsi:type="f:employee"><f:name>a</f:name><f:salary>1</f:salary></f:x></xsl:variable>
               <xsl:variable name="s" as="element()"><f:x xsl:type="f:sealed"><f:name>a</f:name></f:x></xsl:variable>"#,
            "$e instance of element(*, f:employee), $s instance of element(*, f:sealed)"
        ),
        "true true"
    );
    assert_codes(&[
        (
            r#"<out><f:x xsl:type="f:person" xsi:type="f:shape"><f:name>a</f:name></f:x></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:x xsl:type="f:sealed" xsi:type="f:sealedPlus"><f:name>a</f:name><f:more>b</f:more></f:x></out>"#,
            codes::XTTE1540,
        ),
    ]);
}

/// An abstract stipulated type. XML Schema 1.0 §3.3.4 Element Locally Valid (Type) (cvc-type): "2
/// If the type definition is a complex type definition, then its {abstract} must be false." —
/// XTTE1540, unless an `xsi:type` names a concrete type derived from it, which then governs.
#[test]
fn an_abstract_stipulated_type_needs_a_concrete_xsi_type() {
    assert_codes(&[(
        r#"<out><f:x xsl:type="f:shape" w="1"/></out>"#,
        codes::XTTE1540,
    )]);
    assert_eq!(
        answers(
            r#"<xsl:variable name="q" as="element()"><f:x xsl:type="f:shape" xsi:type="f:square" w=" 1 "/></xsl:variable>"#,
            "$q instance of element(*, f:square), data($q/@w) instance of xs:integer"
        ),
        "true true"
    );
}

/// A stipulated type has no {nillable}: XML Schema 1.1 §3.3.1 makes an element ·nilled· only "with
/// respect to some element declaration D … 2 D.{nillable} = true", and type-driven validation has
/// no declaration (Structures §3.3.4.6, governing element declaration clause 4.2). So `xsi:nil`
/// nils nothing: the content is checked against the type (an empty `xs:integer` is invalid,
/// XTTE1540; an empty `xs:string` valid), and the element is not nilled.
#[test]
fn an_element_validated_against_a_type_is_never_nilled() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="v" as="element()"><f:x xsl:type="xs:string" xsi:nil="true"/></xsl:variable>"#,
            "nilled($v), $v instance of element(*, xs:string)"
        ),
        "false true"
    );
    assert_codes(&[(
        r#"<out><f:x xsl:type="xs:integer" xsi:nil="true"/></out>"#,
        codes::XTTE1540,
    )]);
}

/// `type="xs:untyped"` (the shape of the catalog's `validation-0107`): XDM 1.0 §2.6.2 (read at
/// w3.org): "The datatype xs:untyped denotes the dynamic type of an element node that has not been
/// validated, or has been validated in skip mode"; §6.2.1: "If the dm:type-name of an Element Node
/// is xs:untyped, then the dm:type-name of all its descendant elements must also be xs:untyped and
/// the dm:type-name of all its Attribute Nodes must be xs:untypedAtomic". Validating against it is
/// the effect of `strip` on that element (decision D-f3): the element, its attributes and its
/// contained elements are untyped. A contained element's own `strict` validation still runs, for its
/// errors (§19.2.1.1).
#[test]
fn type_xs_untyped_untypes_the_element_and_its_content() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="v" as="element()"><f:x xsl:type="xs:untyped" a="1"><f:n xsl:validation="strict">3</f:n></f:x></xsl:variable>"#,
            "$v instance of element(*, xs:untyped), $v/@a instance of attribute(*, xs:untypedAtomic), \
             $v/f:n instance of element(*, xs:untyped)"
        ),
        "true true true"
    );
    assert_codes(&[(
        r#"<out><f:x xsl:type="xs:untyped"><f:n xsl:validation="strict">99</f:n></f:x></out>"#,
        codes::XTTE1510,
    )]);
}

// ── §19.2.1.1: validation="lax" on element constructors ──────────────────────────────────────

/// §19.2.1.1, `lax`: "The value lax has the same effect as the value strict, except that whereas
/// strict validation fails if there is no matching top-level element declaration or if the outcome
/// of validity assessment is a validity property of invalid or notKnown, lax validation fails only
/// if the outcome of validity assessment is a validity property of invalid … If no such declaration
/// is available, then the element or attribute is not validated, but its attributes and children
/// are validated, again with lax validation. Any nodes whose validation outcome is a validity
/// property of notKnown are annotated as xs:anyType in the case of an element, and
/// xs:untypedAtomic in the case of an attribute." A declared element is validated (`n`); an
/// undeclared one (`u`) is `xs:anyType` — not `xs:untyped` — with its declared attribute (`ga`) and
/// child (`n`) validated, its undeclared attribute `xs:untypedAtomic` and its undeclared child
/// `xs:anyType`.
#[test]
fn lax_validates_a_declared_element_and_laxly_an_undeclared_one() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="a" as="element()"><f:n xsl:validation="lax"> 3 </f:n></xsl:variable>
               <xsl:variable name="b" as="element()"><xsl:element name="f:n" validation="lax">4</xsl:element></xsl:variable>
               <xsl:variable name="u" as="element()"><f:u xsl:validation="lax" f:ga=" 5 " b="x"><f:n>4</f:n><f:v>t</f:v></f:u></xsl:variable>"#,
            "$a instance of schema-element(f:n), string($a), $b instance of schema-element(f:n), \
             $u instance of element(*, xs:anyType), $u instance of element(*, xs:untyped), \
             data($u/@f:ga) + 1, $u/@b instance of attribute(*, xs:untypedAtomic), \
             $u/f:n instance of schema-element(f:n), $u/f:v instance of element(*, xs:untyped)"
        ),
        "true 3 true true false 6 true true false"
    );
}

/// "[ERR XTTE1515] If the validation attribute of an xsl:element, xsl:attribute, xsl:copy,
/// xsl:copy-of, or xsl:result-document instruction, or the xsl:validation attribute of a literal
/// result element, has the effective value lax, and schema validity assessment concludes that the
/// element or attribute is invalid, a type error occurs." A declared element, a declared child or
/// attribute of an undeclared one; an undeclared element with nothing declared in it is no error.
#[test]
fn lax_validation_that_finds_an_invalid_node_is_xtte1515() {
    assert_codes(&[
        (
            r#"<out><f:n xsl:validation="lax">99</f:n></out>"#,
            codes::XTTE1515,
        ),
        (
            r#"<out><xsl:element name="f:n" validation="lax">x</xsl:element></out>"#,
            codes::XTTE1515,
        ),
        (
            r#"<out><f:u xsl:validation="lax"><f:n>99</f:n></f:u></out>"#,
            codes::XTTE1515,
        ),
        (
            r#"<out><f:u xsl:validation="lax" f:ga="x"/></out>"#,
            codes::XTTE1515,
        ),
    ]);
    assert_eq!(
        result(r#"<out><f:u xsl:validation="lax" a="1"><f:v>t</f:v></f:u></out>"#),
        r#"<out><f:u xmlns:f="urn:m4f" a="1"><f:v>t</f:v></f:u></out>"#
    );
}

/// §19.2.1.1's note: "the presence of an xsi:type attribute will not of itself cause an element to
/// be validated: if validation against a named type is required, as distinct from validation
/// against a top-level element declaration, then it must be requested using the XSLT [xsl:]type
/// attribute". An undeclared element under `lax` is not validated against its `xsi:type` (decision
/// D-f2; XML Schema's lax wildcard validation would): no error for a value the type refuses, and
/// the element is `xs:anyType`, not the `xsi:type`.
#[test]
fn xsi_type_does_not_make_an_undeclared_lax_element_validated() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="w" as="element()"><f:u xsl:validation="lax" xsi:type="f:small">abc</f:u></xsl:variable>"#,
            "$w instance of element(*, f:small), $w instance of element(*, xs:untyped), string($w)"
        ),
        "false false abc"
    );
}

// ── §11.9, §19.2.1: type and lax on copies ───────────────────────────────────────────────────

/// An untyped temporary tree `$t` (literal result elements under the default `strip`).
const UNTYPED: &str = r#"<xsl:variable name="t"><f:raw> 5 </f:raw><f:big>11</f:big><f:p><f:name>Ann</f:name></f:p><f:att a=" 07 " b="x" q="p:y" xmlns:p="urn:p"/><f:n>4</f:n><f:n>99</f:n><f:u f:ga="2"><f:n>4</f:n></f:u></xsl:variable>"#;

/// §11.9.2: "These attributes are applied individually to each element, attribute, and document
/// node that is selected by the expression in the select attribute. These attributes are ignored
/// when copying an item that is not an element, attribute or document node." `type` on
/// `xsl:copy-of`: an element validated against the type (§19.2.1.2's element rule), an attribute's
/// normalized value against it with String Valid (its attribute rule: "the attribute is considered
/// valid if … the attribute's normalized value is locally valid with respect to that type
/// definition according to the rules for "String Valid""); an atomic value and a text node are
/// copied as they are.
#[test]
fn type_on_copy_of_validates_each_selected_element_and_attribute() {
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}
                   <xsl:variable name="e" as="element()"><xsl:copy-of select="$t/f:raw" type="f:small"/></xsl:variable>
                   <xsl:variable name="p" as="element()"><xsl:copy-of select="$t/f:p" type="f:person"/></xsl:variable>
                   <xsl:variable name="a" as="attribute()"><xsl:copy-of select="$t/f:att/@a" type="f:small"/></xsl:variable>
                   <xsl:variable name="s" as="item()*"><xsl:copy-of select="1, 'a'" type="f:small"/></xsl:variable>
                   <xsl:variable name="x"><xsl:copy-of select="$t/f:big/text()" type="f:small"/></xsl:variable>"#
            ),
            "$e instance of element(*, f:small), string($e), $p instance of element(*, f:person), \
             $p/f:name instance of element(*, xs:string), $a instance of attribute(*, f:small), \
             data($a) + 1, count($s), string($x)"
        ),
        "true 5 true true true 8 2 11"
    );
}

/// XTTE1540 for a copy that is not valid against its `type` (§19.2.1.2) — an element, an attribute
/// value, and an attribute against a complex type, which is no simple type definition for String
/// Valid to apply (decision D-f4; XSLT 3.0's XTTE1535 does not exist in 2.0); XTTE1545 (§19.2.1.3)
/// for an attribute copied with a type built from `xs:QName` — "This affects the instructions
/// xsl:attribute, xsl:copy, and xsl:copy-of".
#[test]
fn type_on_copies_errors() {
    assert_codes(&[
        (
            &format!(r#"{UNTYPED}<out><xsl:copy-of select="$t/f:big" type="f:small"/></out>"#),
            codes::XTTE1540,
        ),
        (
            &format!(r#"{UNTYPED}<out><xsl:copy-of select="$t/f:att/@b" type="f:small"/></out>"#),
            codes::XTTE1540,
        ),
        (
            &format!(r#"{UNTYPED}<out><xsl:copy-of select="$t/f:att/@a" type="f:person"/></out>"#),
            codes::XTTE1540,
        ),
        (
            &format!(r#"{UNTYPED}<out><xsl:copy-of select="$t/f:att/@q" type="f:qn"/></out>"#),
            codes::XTTE1545,
        ),
        (
            &format!(
                r#"{UNTYPED}<out><xsl:for-each select="$t/f:big"><xsl:copy type="f:small"><xsl:value-of select="."/></xsl:copy></xsl:for-each></out>"#
            ),
            codes::XTTE1540,
        ),
    ]);
}

/// `lax` on `xsl:copy-of` (§19.2.1.1, applied to each selected node, §11.9.2): a declared element
/// is validated (XTTE1515 when invalid), an undeclared one is `xs:anyType` with its declared
/// attribute and child validated; a declared attribute is validated, an undeclared one
/// `xs:untypedAtomic`.
#[test]
fn lax_on_copy_of() {
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}
                   <xsl:variable name="n" as="element()"><xsl:copy-of select="$t/f:n[1]" validation="lax"/></xsl:variable>
                   <xsl:variable name="u" as="element()"><xsl:copy-of select="$t/f:u" validation="lax"/></xsl:variable>
                   <xsl:variable name="ga" as="attribute()"><xsl:copy-of select="$t/f:u/@f:ga" validation="lax"/></xsl:variable>
                   <xsl:variable name="b" as="attribute()"><xsl:copy-of select="$t/f:att/@b" validation="lax"/></xsl:variable>"#
            ),
            "$n instance of schema-element(f:n), $u instance of element(*, xs:untyped), \
             data($u/@f:ga) instance of xs:integer, $u/f:n instance of schema-element(f:n), \
             $ga instance of attribute(*, xs:integer), $b instance of attribute(*, xs:untypedAtomic)"
        ),
        "true false true true true true"
    );
    assert_codes(&[(
        &format!(r#"{UNTYPED}<out><xsl:copy-of select="$t/f:n[2]" validation="lax"/></out>"#),
        codes::XTTE1515,
    )]);
}

/// `type` and `lax` on `xsl:copy` (§11.9.1, §19.2.1.1, §19.2.1.2): of an element (its content is
/// the sequence constructor's), of an attribute, and of a text node, which they do not apply to
/// ("These attributes are ignored when copying an item that is not an element, attribute or
/// document node", §11.9.2; the M4e fence D-e8 checked them whatever the item was).
#[test]
fn type_and_lax_on_xsl_copy() {
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}
                   <xsl:variable name="k" as="element()"><xsl:for-each select="$t/f:raw"><xsl:copy type="f:small"><xsl:value-of select="."/></xsl:copy></xsl:for-each></xsl:variable>
                   <xsl:variable name="ka" as="attribute()"><xsl:for-each select="$t/f:att/@a"><xsl:copy type="f:small"/></xsl:for-each></xsl:variable>
                   <xsl:variable name="kl" as="element()"><xsl:for-each select="$t/f:n[1]"><xsl:copy validation="lax"><xsl:value-of select="."/></xsl:copy></xsl:for-each></xsl:variable>
                   <xsl:variable name="kt"><xsl:for-each select="$t/f:big/text()"><xsl:copy type="f:small"/></xsl:for-each></xsl:variable>"#
            ),
            "$k instance of element(*, f:small), $ka instance of attribute(*, f:small), \
             $kl instance of schema-element(f:n), string($kt)"
        ),
        "true true true 11"
    );
    assert_codes(&[(
        &format!(
            r#"{UNTYPED}<out><xsl:for-each select="$t/f:n[2]"><xsl:copy validation="lax"><xsl:value-of select="."/></xsl:copy></xsl:for-each></out>"#
        ),
        codes::XTTE1515,
    )]);
}

// ── §19.2.2: documents with type and lax ─────────────────────────────────────────────────────

/// §19.2.2: "When validation is requested for a document node (that is, when validation is set to
/// strict or lax, or when a type attribute is present) … The single element node child is
/// validated, using the supplied values of the validation and type attributes, as described in
/// 19.2.1" — and its note: "The type attribute on xsl:document and xsl:result-document, and on
/// xsl:copy and xsl:copy-of when copying a document node, thus refers to the required type of the
/// element node that is the only element child of the document node." On `xsl:document`, on copies
/// of a document node (`xsl:copy-of`, `xsl:copy`), with `type` and with `lax`.
#[test]
fn type_and_lax_on_documents_validate_the_element_child() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="src"><f:p><f:name>Ann</f:name></f:p></xsl:variable>
               <xsl:variable name="d" as="document-node()"><xsl:document type="f:person"><xsl:comment>c</xsl:comment><f:p><f:name>Ann</f:name></f:p></xsl:document></xsl:variable>
               <xsl:variable name="l" as="document-node()"><xsl:document validation="lax"><f:u><f:n>4</f:n></f:u></xsl:document></xsl:variable>
               <xsl:variable name="c" as="document-node()"><xsl:copy-of select="$src" type="f:person"/></xsl:variable>
               <xsl:variable name="cl" as="document-node()"><xsl:copy-of select="$src" validation="lax"/></xsl:variable>
               <xsl:variable name="k" as="document-node()"><xsl:for-each select="$src"><xsl:copy type="f:person"><xsl:copy-of select="node()"/></xsl:copy></xsl:for-each></xsl:variable>"#,
            "$d instance of document-node(element(*, f:person)), \
             $l/f:u instance of element(*, xs:untyped), $l/f:u/f:n instance of schema-element(f:n), \
             $c instance of document-node(element(*, f:person)), \
             $cl/f:p instance of element(*, xs:untyped), \
             $k instance of document-node(element(*, f:person))"
        ),
        "true false true true false true"
    );
}

/// The document-level errors with `type` and `lax`: XTTE1550 ("unless the children of the
/// document node comprise exactly one element node, no text nodes, and zero or more comment and
/// processing instruction nodes"), the element child's own invalidity (XTTE1540 for `type`, XTTE1515
/// for `lax`), and "The validation rule "Validation Root Valid (ID/IDREF)" is applied to the single
/// element node child of the document node" — XTTE1555 ("It is a type error if, when validating a
/// document node, document-level constraints are not satisfied"); an empty one in element content
/// too.
#[test]
fn type_and_lax_documents_errors() {
    assert_codes(&[
        (
            r#"<out><xsl:document type="f:person"><f:p><f:name>a</f:name></f:p><f:p><f:name>b</f:name></f:p></xsl:document></out>"#,
            codes::XTTE1550,
        ),
        (
            r#"<out><xsl:document type="f:person">t<f:p><f:name>a</f:name></f:p></xsl:document></out>"#,
            codes::XTTE1550,
        ),
        (
            r#"<out><xsl:document type="f:person"/></out>"#,
            codes::XTTE1550,
        ),
        (
            r#"<out><xsl:document validation="lax">t<f:u/></xsl:document></out>"#,
            codes::XTTE1550,
        ),
        (
            r#"<out><xsl:document type="f:person"><f:p/></xsl:document></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><xsl:document validation="lax"><f:n>99</f:n></xsl:document></out>"#,
            codes::XTTE1515,
        ),
        (
            r#"<out><xsl:document type="f:refs"><f:x><f:r id="a" to="b"/></f:x></xsl:document></out>"#,
            codes::XTTE1555,
        ),
        (
            r#"<out><xsl:document validation="lax"><f:refs><f:r id="a"/><f:r id="a"/></f:refs></xsl:document></out>"#,
            codes::XTTE1555,
        ),
        // Element-level validation does not apply the ID/IDREF rule (§19.2.1.3): no error.
        (
            r#"<out><f:x xsl:type="f:refs"><f:r id="a" to="b"/></f:x><xsl:document type="f:small">x</xsl:document></out>"#,
            codes::XTTE1550,
        ),
    ]);
}

/// `xsl:result-document` with `type` and `lax` (§19.2.2; XTTE1515 lists `xsl:result-document`): the
/// primary result validated as a document.
#[test]
fn type_and_lax_on_xsl_result_document() {
    assert_eq!(
        result(
            r#"<xsl:result-document type="f:person"><f:p><f:name>a</f:name></f:p></xsl:result-document>"#
        ),
        r#"<f:p xmlns:f="urn:m4f"><f:name>a</f:name></f:p>"#
    );
    assert_eq!(
        result(
            r#"<xsl:result-document validation="lax"><f:u><f:n>3</f:n></f:u></xsl:result-document>"#
        ),
        r#"<f:u xmlns:f="urn:m4f"><f:n>3</f:n></f:u>"#
    );
    assert_codes(&[
        (
            r#"<xsl:result-document type="f:person"><f:p/></xsl:result-document>"#,
            codes::XTTE1540,
        ),
        (
            r#"<xsl:result-document validation="lax"><f:u><f:n>99</f:n></f:u></xsl:result-document>"#,
            codes::XTTE1515,
        ),
    ]);
}

// ── Nesting, values, simple content ──────────────────────────────────────────────────────────

/// §19.2.1.1: "Conceptually, the validation requested for a child element or attribute node is
/// applied before the validation requested for its parent element" — the innermost failure is the
/// one reported (D-10) across `type`, `lax` and `strict` boundaries; the stipulated type's lax
/// wildcard (`bag`) validates a declared child and leaves an undeclared one `xs:anyType`; an outer
/// `strip` (the final result tree's default) keeps the values the validation gave (§19.2.1.1:
/// strip changes "The typed value of the node … to be the same as its string value").
#[test]
fn nested_type_and_lax_boundaries() {
    assert_codes(&[
        (
            r#"<out><f:bag xsl:validation="strict"><f:x xsl:type="f:small">11</f:x></f:bag></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:x xsl:type="f:bag"><f:n xsl:validation="strict">99</f:n></f:x></out>"#,
            codes::XTTE1510,
        ),
        (
            r#"<out><f:x xsl:type="f:bag"><f:n xsl:validation="lax">99</f:n></f:x></out>"#,
            codes::XTTE1515,
        ),
        (
            r#"<out><f:u xsl:validation="lax"><f:x xsl:type="f:small">11</f:x></f:u></out>"#,
            codes::XTTE1540,
        ),
    ]);
    assert_eq!(
        answers(
            r#"<xsl:variable name="b" as="element()"><f:x xsl:type="f:bag"><f:n>4</f:n><f:zz/></f:x></xsl:variable>"#,
            "$b instance of element(*, f:bag), $b/f:n instance of schema-element(f:n), \
             $b/f:zz instance of element(*, xs:untyped)"
        ),
        "true true false"
    );
    assert_eq!(
        result(
            r#"<out><f:x xsl:type="f:small"> 07 </f:x><f:y xsl:type="f:price" cur=" EUR "> 1.50 </f:y></out>"#
        ),
        r#"<out><f:x xmlns:f="urn:m4f">07</f:x><f:y xmlns:f="urn:m4f" cur="EUR">1.50</f:y></out>"#
    );
}

/// §5.7.2 step 3 ("The sequence is atomized"): an element `type` validates in the content of
/// `xsl:value-of` contributes its typed value — a list type's items joined by the separator; an
/// invalid one is XTTE1540.
#[test]
fn a_type_validated_element_in_simple_content_contributes_its_typed_value() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="v"><xsl:value-of separator=","><f:x xsl:type="f:ints"> 1  2 3 </f:x></xsl:value-of></xsl:variable>"#,
            "string($v)"
        ),
        "1,2,3"
    );
    assert_codes(&[(
        r#"<out><xsl:value-of><f:x xsl:type="f:small">11</f:x></xsl:value-of></out>"#,
        codes::XTTE1540,
    )]);
}

/// The same for a copy with `type` or `lax` in simple content (§11.9.2, §5.7.2): it is validated
/// (XTTE1515 for an invalid `lax` copy) and contributes its typed value — not the original's
/// string value, which an untyped (`strip`) copy contributes.
#[test]
fn a_type_or_lax_copy_in_simple_content_is_validated() {
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}<xsl:variable name="v"><xsl:value-of separator=","><xsl:copy-of select="$t/f:raw" type="f:small"/><xsl:copy-of select="$t/f:n[1]" validation="lax"/><xsl:copy-of select="$t/f:raw"/></xsl:value-of></xsl:variable>"#
            ),
            "concat('[', string($v), ']')"
        ),
        "[5,4, 5 ]"
    );
    assert_codes(&[(
        &format!(
            r#"{UNTYPED}<out><xsl:value-of><xsl:copy-of select="$t/f:n[2]" validation="lax"/></xsl:value-of></out>"#
        ),
        codes::XTTE1515,
    )]);
}

// ── M4b's lax source with an undeclared root ─────────────────────────────────────────────────

/// `SourceValidation::Lax` for a source whose root has no declaration (M4b decision D-O fenced it
/// as M4f's; upstream C36's `ValidationRoot::Lax` is XML Schema 1.1 §5.2's "lax wildcard
/// validation": "If they do not identify any declaration or definition, then ·lax assessment· is
/// performed"): the declared descendants are validated and typed, an invalid one makes the
/// document an error (XTDE1170 for the principal source), and the undeclared root is no error.
/// Its descendant `f:n` was validated, so it is `xs:anyType` in the typed tree, not `xs:untyped` —
/// XDM 1.0 §3.3.1.1's answer for "[validation attempted] … "partial"" — the same as every element
/// a lax wildcard matches in a typed source (decision D-f6; upstream C85, `wave-b` `7608d3a`).
#[test]
fn lax_source_validation_of_an_undeclared_root() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="data(/f:u/f:n) instance of xs:integer, data(/f:u/f:n) + 1, /f:u instance of element(*, xs:untyped)"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_over(
            &main,
            r#"<f:u xmlns:f="urn:m4f"><f:n> 4 </f:n><f:v/></f:u>"#,
            SourceValidation::Lax
        )
        .map(|text| inner(&text))
        .unwrap(),
        "true 5 false"
    );
    let e = run_over(
        &main,
        r#"<f:u xmlns:f="urn:m4f"><f:n>99</f:n></f:u>"#,
        SourceValidation::Lax,
    )
    .unwrap_err();
    assert_eq!(e.code, codes::XTDE1170, "{e}");
}

// ── What stays fenced ─────────────────────────────────────────────────────────────────────────

/// The XPath 2.0 types that §3.13 puts in scope and an XML Schema 1.0 set has no definition of
/// (`xs:untypedAtomic`, `xs:anyAtomicType`, `xs:dayTimeDuration`, `xs:yearMonthDuration`) on an
/// element constructor or a copy were `XSLT2-NOTIMPL` naming M4g. M4g worked the first two around
/// and fenced the durations (upstream proposal C72); since the Wave B follow-up all four are the
/// set's own (upstream `SchemaSet::xpath_type`, `tests/wave_b_followup.rs`). A copy of an element
/// with an element child is no `xs:anyAtomicType`, a simple type.
#[test]
fn xpath_only_types_on_elements_run_since_m4g() {
    assert_eq!(
        result(r#"<out><f:x xsl:type="xs:untypedAtomic">a</f:x></out>"#),
        r#"<out><f:x xmlns:f="urn:m4f">a</f:x></out>"#
    );
    assert_eq!(
        result(
            r#"<out><xsl:element name="f:x" type="xs:dayTimeDuration">PT1H</xsl:element></out>"#
        ),
        r#"<out><f:x xmlns:f="urn:m4f">PT1H</f:x></out>"#
    );
    let e = error(
        r#"<xsl:variable name="v"><a><b/></a></xsl:variable><out><xsl:copy-of select="$v/a" type="xs:anyAtomicType"/></out>"#,
    );
    assert_eq!(e.code, codes::XTTE1540, "{e}");
}

// ══ Fix round 1: the lax region ══════════════════════════════════════════════════════════════

/// §19.2.1.1, `lax`: "If no such declaration is available, then the element or attribute is not
/// validated, but its attributes and children are validated, again with lax validation. Any nodes
/// whose validation outcome is a validity property of notKnown are annotated as xs:anyType in the
/// case of an element" — and its note: "the presence of an xsi:type attribute will not of itself
/// cause an element to be validated". Children of an undeclared element are validated "again with
/// lax validation", so the note holds for them too: an undeclared `v` under an undeclared lax `u` is
/// not validated against its `xsi:type` — no XTTE1515 for a value the type refuses, and no `f:small`
/// annotation for one it accepts (the reviewer's two reproducers).
#[test]
fn xsi_type_does_not_make_an_undeclared_descendant_of_a_lax_element_validated() {
    assert_eq!(
        result(r#"<out><f:u xsl:validation="lax"><f:v xsi:type="f:small">99</f:v></f:u></out>"#),
        r#"<out><f:u xmlns:f="urn:m4f"><f:v xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:type="f:small">99</f:v></f:u></out>"#
    );
}

/// The reviewer's second reproducer: a value the `xsi:type` accepts does not give the undeclared
/// descendant that type either — it is `xs:anyType` (its typed value `xs:untypedAtomic`).
#[test]
fn an_undeclared_descendant_of_a_lax_element_does_not_take_its_xsi_type() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="u" as="element()"><f:u xsl:validation="lax"><f:v xsi:type="f:small">7</f:v></f:u></xsl:variable>"#,
            "$u/f:v instance of element(*, f:small), $u/f:v instance of element(*, xs:untyped), \
             data($u/f:v) instance of xs:untypedAtomic"
        ),
        "false false true"
    );
}

/// The same at every level of the recursion ("again with lax validation"): `u`, `v`, `w`, all
/// undeclared, each with an `xsi:type` its content does not match — no error, and every one
/// `xs:anyType`; with types their content does match, none takes its `xsi:type`.
#[test]
fn the_lax_region_reaches_every_undeclared_level() {
    assert!(result(
        r#"<out><f:u xsl:validation="lax" xsi:type="f:small"><f:v xsi:type="f:small"><f:w xsi:type="f:small">99</f:w></f:v></f:u></out>"#
    )
    .contains(">99</f:w>"));
    assert_eq!(
        answers(
            r#"<xsl:variable name="t" as="element()"><f:u xsl:validation="lax" xsi:type="f:person"><f:v xsi:type="f:person"><f:w xsi:type="f:small">5</f:w></f:v></f:u></xsl:variable>"#,
            "$t instance of element(*, f:person), $t/f:v instance of element(*, f:person), \
             $t/f:v/f:w instance of element(*, f:small), $t/f:v/f:w instance of element(*, xs:untyped), \
             data($t/f:v/f:w) instance of xs:untypedAtomic"
        ),
        "false false false false true"
    );
}

/// The region ends at a declared element: §19.2.1.1 validates a child of an undeclared element
/// "again with lax validation", which "has the same effect as the value strict" for a node with a
/// top-level declaration — "the element … being validated must conform to its declaration if a
/// top-level declaration is available" — and strict validation "applies recursively to contained
/// elements and attributes to the extent required by the schema definition". So below the
/// declared `bag` (a lax wildcard) XML Schema decides: its undeclared child `v` is validated against
/// its `xsi:type` (`f:small`), the way XML Schema's lax assessment does — valid 7 is `f:small`,
/// invalid 99 is XTTE1515.
#[test]
fn below_a_declared_element_xml_schema_decides() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t" as="element()"><f:u xsl:validation="lax"><f:bag><f:v xsi:type="f:small">7</f:v></f:bag></f:u></xsl:variable>"#,
            "$t/f:bag instance of schema-element(f:bag), $t/f:bag/f:v instance of element(*, f:small)"
        ),
        "true true"
    );
    assert_codes(&[(
        r#"<out><f:u xsl:validation="lax"><f:bag><f:v xsi:type="f:small">99</f:v></f:bag></f:u></out>"#,
        codes::XTTE1515,
    )]);
}

/// Attributes of an element in the region are validated against a top-level attribute
/// declaration when there is one (`f:ga`; XML Schema's built-in `xsi:type` declaration, whose type is
/// `xs:QName`), otherwise `xs:untypedAtomic` ("Any nodes whose validation outcome is a validity
/// property of notKnown are annotated as … xs:untypedAtomic in the case of an attribute"). An
/// `xsi:type` naming no type is no error there (the element is not validated against it), and
/// `xsi:nil="true"` nils nothing: the element is not validated, so not nilled (XML Schema 1.1 §3.3.1
/// nils only "with respect to some element declaration D").
#[test]
fn attributes_xsi_type_and_xsi_nil_in_the_lax_region() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t" as="element()"><f:u xsl:validation="lax" xsi:nil="true">x<f:v f:ga=" 3 " b="y" xsi:type="f:nosuch">z</f:v><f:w xsi:nil="true">w</f:w></f:u></xsl:variable>"#,
            "nilled($t), string($t), data($t/f:v/@f:ga) instance of xs:integer, \
             $t/f:v/@b instance of attribute(*, xs:untypedAtomic), \
             data($t/f:v/@xsi:type) instance of xs:QName, nilled($t/f:w), string($t/f:w)"
        ),
        "false xzw true true true false w"
    );
}

/// Each run has its own region, and the innermost failure is reported first (D-10): a `strict`
/// boundary inside the region validates its element; a `type` boundary inside it validates against
/// the type (below its lax wildcard XML Schema decides: XTTE1540 for 99); a `strip` boundary does not
/// change the outer region; a `lax` region inside a `strict` run is that inner run's only — the
/// outer `strict` validation of `bag` assesses the whole subtree as XML Schema does, where `v`'s
/// `xsi:type` counts (XTTE1510).
#[test]
fn each_run_has_its_own_lax_region() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t" as="element()"><f:u xsl:validation="lax"><f:n xsl:validation="strict">4</f:n><f:v xsl:validation="strip" xsi:type="f:small">99</f:v>a<f:w xsi:type="f:small">99</f:w>b</f:u></xsl:variable>"#,
            "$t/f:n instance of schema-element(f:n), $t/f:v instance of element(*, xs:untyped), \
             $t/f:w instance of element(*, f:small), string($t)"
        ),
        "true false false 499a99b"
    );
    assert_codes(&[
        (
            r#"<out><f:u xsl:validation="lax"><f:x xsl:type="f:bag"><f:v xsi:type="f:small">99</f:v></f:x></f:u></out>"#,
            codes::XTTE1540,
        ),
        (
            r#"<out><f:bag xsl:validation="strict"><f:u xsl:validation="lax"><f:v xsi:type="f:small">99</f:v></f:u></f:bag></out>"#,
            codes::XTTE1510,
        ),
    ]);
}

/// `xsl:copy-of validation="lax"` of a source tree (§11.9.2: applied to each selected node; the
/// copy is validated as a constructed element, §19.2.1.1) whose undeclared descendants carry
/// `xsi:type`s: no error for `v` (99, not an `f:small`), `v` `xs:anyType`; the declared `bag`'s
/// undeclared child `w` is validated against its `xsi:type` (XML Schema decides below `bag`).
#[test]
fn a_lax_copy_of_a_source_tree_with_xsi_type_descendants() {
    let main = sheet(
        r#"<xsl:template match="/"><xsl:variable name="c" as="element()"><xsl:copy-of select="/f:u" validation="lax"/></xsl:variable><out><xsl:value-of select="$c/f:v instance of element(*, f:small), $c/f:v instance of element(*, xs:untyped), $c/f:bag/f:w instance of element(*, f:small), string($c/f:v)"/></out></xsl:template>"#,
    );
    let source = r#"<f:u xmlns:f="urn:m4f" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:type="f:small"><f:v xsi:type="f:small">99</f:v><f:bag><f:w xsi:type="f:small">7</f:w></f:bag></f:u>"#;
    assert_eq!(
        run_over(&main, source, SourceValidation::Skip)
            .map(|text| inner(&text))
            .unwrap(),
        "false false true 99"
    );
}

/// A document node with `validation="lax"` (§19.2.2: "The single element node child is validated,
/// using the supplied values of the validation and type attributes, as described in 19.2.1"): its
/// undeclared element child and that child's undeclared descendants are the region — in
/// `xsl:document` and in `xsl:result-document`.
#[test]
fn the_lax_region_of_a_validated_document() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="d" as="document-node()"><xsl:document validation="lax"><f:u><f:v xsi:type="f:small">99</f:v><f:n>4</f:n></f:u></xsl:document></xsl:variable>"#,
            "$d/f:u/f:v instance of element(*, xs:untyped), $d/f:u/f:v instance of element(*, f:small), \
             $d/f:u/f:n instance of schema-element(f:n)"
        ),
        "false false true"
    );
    assert_eq!(
        result(
            r#"<xsl:result-document validation="lax"><f:u><f:v xsi:type="f:small">99</f:v></f:u></xsl:result-document>"#
        ),
        r#"<f:u xmlns:f="urn:m4f"><f:v xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:type="f:small">99</f:v></f:u>"#
    );
}

/// Typed sources are not constructed nodes: §19.2.1 is "Validating Constructed Elements and
/// Attributes", and §4.1 says "Construction of the XDM tree is outside the scope of this
/// specification". A `SourceValidation::Lax` source is assessed by XML Schema's lax wildcard
/// validation (§5.2, `ValidationRoot::Lax`), where an `xsi:type` does make an undeclared element
/// validated — at the root and below it: an undeclared `v` with `xsi:type="f:small"` is `f:small`
/// when 7 and makes the document invalid (XTDE1170) when 99. Unchanged by fix round 1, pinned.
#[test]
fn a_lax_source_follows_xml_schema_for_xsi_type() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="/f:u/f:v instance of element(*, f:small), /f:u instance of element(*, f:small)"/></out></xsl:template>"#,
    );
    let doc = |root_type: &str, value: &str| {
        format!(
            r#"<f:u xmlns:f="urn:m4f" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"{root_type}><f:v xsi:type="f:small">{value}</f:v></f:u>"#
        )
    };
    assert_eq!(
        run_over(&main, &doc("", "7"), SourceValidation::Lax)
            .map(|text| inner(&text))
            .unwrap(),
        "true false"
    );
    let e = run_over(&main, &doc("", "99"), SourceValidation::Lax).unwrap_err();
    assert_eq!(e.code, codes::XTDE1170, "{e}");
    // The root's own `xsi:type` makes it validated too (§5.2: "or a type definition (by xsi:type),
    // then ·strict validation· is performed"): `f:person` has no `v` child.
    let e = run_over(
        &main,
        &doc(r#" xsi:type="f:person""#, "7"),
        SourceValidation::Lax,
    )
    .unwrap_err();
    assert_eq!(e.code, codes::XTDE1170, "{e}");
}
