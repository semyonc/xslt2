//! Copies and document-level validation: `[xsl:]validation` `strict` / `preserve` / `strip` on
//! `xsl:copy` and `xsl:copy-of` (XSLT 2.0 §11.9.1, §11.9.2, §19.2.1.1), XTTE0950 for
//! namespace-sensitive copies, and the validation of document nodes (§19.2.2) on `xsl:document`,
//! `xsl:result-document` and copies of document nodes — XTTE1550, ID/IDREF (XTTE1555) — but not on
//! the documents of `xsl:message` or of variables.
//!
//! Every test goes through the public API and cites the rule it checks (XSLT 2.0).

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::SchemaSetBuilder;

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Outcome, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///copies/main.xsl";
const SOURCE: &str = "file:///copies/doc.xml";
const XSD_URI: &str = "file:///copies/copies.xsd";

/// `urn:copies`: `doc` (the source's root: `row`s, then `qe`, `qa`, `nums`); `row` (an `xs:ID`
/// attribute `id`, an `xs:IDREF` attribute `ref`, the global attribute `size`, an optional `n`);
/// `n` (a nillable `xs:integer`); `qe` (an `xs:QName` element); `qa` (an `xs:QName` attribute `q`,
/// an integer-or-QName union attribute `iq`, an attribute `code` of a restriction of `xs:ID`); `nums` (a list of integers); `table` (`row`s
/// with an `xs:unique` on `@e:size`); the global attributes `size` (an integer range) and `qn`
/// (an `xs:QName`); `w` (a sequence of `n`) and `ww` (a sequence of `w`).
const SCHEMA_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:copies"
    xmlns:e="urn:copies" targetNamespace="urn:copies" elementFormDefault="qualified">
  <xs:element name="doc">
    <xs:complexType>
      <xs:sequence>
        <xs:element ref="row" maxOccurs="unbounded"/>
        <xs:element ref="qe" minOccurs="0"/>
        <xs:element ref="qa" minOccurs="0"/>
        <xs:element ref="nums" minOccurs="0"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:element name="row">
    <xs:complexType>
      <xs:sequence><xs:element ref="n" minOccurs="0"/></xs:sequence>
      <xs:attribute name="id" type="xs:ID"/>
      <xs:attribute name="ref" type="xs:IDREF"/>
      <xs:attribute ref="size"/>
    </xs:complexType>
  </xs:element>
  <xs:element name="n" type="xs:integer" nillable="true"/>
  <xs:element name="qe" type="xs:QName"/>
  <xs:element name="qa">
    <xs:complexType>
      <xs:attribute name="q" type="xs:QName"/>
      <xs:attribute name="iq" type="intOrQName"/>
      <xs:attribute name="code" type="code"/>
    </xs:complexType>
  </xs:element>
  <xs:element name="nums" type="ints"/>
  <xs:element name="w">
    <xs:complexType><xs:sequence><xs:element ref="n" minOccurs="0" maxOccurs="unbounded"/></xs:sequence></xs:complexType>
  </xs:element>
  <xs:element name="ww">
    <xs:complexType><xs:sequence><xs:element ref="w" minOccurs="0" maxOccurs="unbounded"/></xs:sequence></xs:complexType>
  </xs:element>
  <xs:element name="table">
    <xs:complexType>
      <xs:sequence><xs:element ref="row" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
    </xs:complexType>
    <xs:unique name="unique-size"><xs:selector xpath="e:row"/><xs:field xpath="@e:size"/></xs:unique>
  </xs:element>
  <xs:attribute name="size" type="size"/>
  <xs:attribute name="qn" type="xs:QName"/>
  <xs:simpleType name="size">
    <xs:restriction base="xs:integer"><xs:minInclusive value="1"/><xs:maxInclusive value="20"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="ints"><xs:list itemType="xs:integer"/></xs:simpleType>
  <xs:simpleType name="code"><xs:restriction base="xs:ID"><xs:pattern value="c[0-9]"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="intOrQName"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
</xs:schema>"#;

/// Valid against `SCHEMA_XSD`: two rows (the second with a nilled `n` and an IDREF to the first), a
/// QName element, a QName attribute, a list.
const DOC: &str = r#"<e:doc xmlns:e="urn:copies" xmlns:p="urn:p" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><e:row id="r1" e:size="3"><e:n>5</e:n></e:row><e:row id="r2" ref="r1"><e:n xsi:nil="true"/></e:row><e:qe>p:x</e:qe><e:qa q="p:y" iq="5" code="c1"/><e:nums>1 2  3</e:nums></e:doc>"#;

/// An untyped temporary tree `$u` (literal result elements under the default `strip`): a valid
/// `row`, an invalid `n`, an element with no top-level declaration, an attribute of the QName
/// type `qn`, an invalid `size`, a list.
const UNTYPED: &str = r#"<xsl:variable name="u"><e:row id="r9" e:size="4"><e:n> 07 </e:n></e:row><e:n>x</e:n><e:other/><e:x e:qn="p:z" xmlns:p="urn:p"/><e:y e:size="99"/><e:nums> 1  2 3 </e:nums></xsl:variable>"#;

fn sheet(top: &str) -> String {
    sheet_with("", top)
}

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:e=\"urn:copies\" \
           exclude-result-prefixes=\"xs e\" {attrs}>\
           <xsl:import-schema namespace=\"urn:copies\" schema-location=\"copies.xsd\"/>\
           {top}</xsl:stylesheet>"
    )
}

/// `urn:copies-pre`: `pre`, whose `xml:space` is fixed at `preserve` (the shape of XHTML 1.0's `pre`,
/// `style` and `script`).
const PRE_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:copies-pre"
    elementFormDefault="qualified">
  <xs:import namespace="http://www.w3.org/XML/1998/namespace"/>
  <xs:element name="pre">
    <xs:complexType mixed="true"><xs:attribute ref="xml:space" fixed="preserve"/></xs:complexType>
  </xs:element>
</xs:schema>"#;

fn resolver(main: &str) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, SCHEMA_XSD);
    mem.insert("file:///copies/pre.xsd", PRE_XSD);
    Arc::new(mem)
}

/// Schema-aware: compiles `main` against the context its `xsl:import-schema`s make, runs it over
/// `DOC` validated strictly, and hands the outcome and the serialized primary result to
/// `inspect`.
fn run_with<T>(
    main: &str,
    inspect: impl FnOnce(&Outcome<'_>, String) -> T,
) -> Result<T, XsltError> {
    run_over(main, SourceValidation::Strict, inspect)
}

fn run_over<T>(
    main: &str,
    validation: SourceValidation,
    inspect: impl FnOnce(&Outcome<'_>, String) -> T,
) -> Result<T, XsltError> {
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
    let outcome = transformation.transform_source_with(SOURCE, DOC, validation)?;
    let text = common::body(compiled.serialize_primary(&outcome)?);
    Ok(inspect(&outcome, text))
}

fn run(main: &str) -> Result<String, XsltError> {
    run_with(main, |_, text| text)
}

/// The text of `<out>…</out>` the template matching `/` writes: the declarations `top` (variables)
/// in the template, then `<xsl:value-of select="{body}"/>`.
fn answers(top: &str, body: &str) -> String {
    answers_with("", top, body)
}

fn answers_with(attrs: &str, top: &str, body: &str) -> String {
    let main = sheet_with(
        attrs,
        &format!(
            r#"<xsl:template match="/">{top}<out><xsl:value-of select="{body}"/></out></xsl:template>"#
        ),
    );
    match run(&main) {
        Ok(text) => inner(&text),
        Err(e) => panic!("the run failed: {e}\n{top}\n{body}"),
    }
}

/// The error the template matching `/` raises with `content` (declarations, then the body of
/// `<out>`).
fn error(content: &str) -> XsltError {
    error_with("", content)
}

fn error_with(attrs: &str, content: &str) -> XsltError {
    let main = sheet_with(
        attrs,
        &format!(r#"<xsl:template match="/">{content}</xsl:template>"#),
    );
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

fn assert_code(e: &XsltError, code: &str, what: &str) {
    assert_eq!(e.code, code, "{what}: {e}");
}

// ── §11.9.2 / §19.2.1.1: xsl:copy-of with preserve ───────────────────────────────────────────

/// §19.2.1.1, `preserve`: "In the case of xsl:copy-of, all the nodes that are copied will retain
/// their type annotations unchanged" — and §11.9.2: "The new element will have the same values of
/// the is-id, is-idrefs, and nilled properties as the original element". In a temporary tree (a
/// `preserve` document, §19.2.2), in a sequence (parentless copies), for an attribute selected on
/// its own, for a document node, and with `preserve` from `default-validation` (§3.6).
#[test]
fn copy_of_preserve_keeps_annotations_nilled_and_ids() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><xsl:copy-of select="/e:doc/e:row" validation="preserve"/></xsl:variable>"#,
            "$t/e:row[1] instance of schema-element(e:row), \
             data($t/e:row[1]/@e:size) instance of xs:integer, data($t/e:row[1]/e:n) + 1, \
             nilled($t/e:row[2]/e:n), exists(id('r1', $t)), \
             $t/e:row[2]/@ref instance of attribute(*, xs:IDREF)"
        ),
        "true true 6 true true true"
    );
    // Parentless copies in a sequence (§11.9.2: "a new element is constructed and appended to the
    // result sequence").
    assert_eq!(
        answers(
            r#"<xsl:variable name="s" as="element()*"><xsl:copy-of select="/e:doc/e:row" validation="preserve"/></xsl:variable>"#,
            "count($s), $s[1] instance of schema-element(e:row), $s[1] is /e:doc/e:row[1]"
        ),
        "2 true false"
    );
    // An attribute copied on its own: §19.2.1.1 keeps its annotation.
    assert_eq!(
        answers(
            r#"<xsl:variable name="a" as="attribute()"><xsl:copy-of select="/e:doc/e:row[1]/@e:size" validation="preserve"/></xsl:variable>
               <xsl:variable name="t"><x xsl:validation="preserve"><xsl:copy-of select="/e:doc/e:row[1]/@id" validation="preserve"/></x></xsl:variable>"#,
            "$a instance of attribute(*, e:size), $t/x/@id instance of attribute(*, xs:ID), \
             exists(id('r1', $t))"
        ),
        "true true true"
    );
    // A document node: "the children of this document node will be one-to-one copies of the
    // children of the original document node".
    assert_eq!(
        answers(
            r#"<xsl:variable name="d" as="document-node()"><xsl:copy-of select="/" validation="preserve"/></xsl:variable>"#,
            "$d instance of document-node(schema-element(e:doc)), $d is /"
        ),
        "true false"
    );
    // §3.6: the module's `default-validation` is the copy's when it says nothing.
    assert_eq!(
        answers_with(
            r#"default-validation="preserve""#,
            r#"<xsl:variable name="t"><xsl:copy-of select="/e:doc/e:row[1]"/></xsl:variable>"#,
            "$t/e:row instance of schema-element(e:row)"
        ),
        "true"
    );
}

/// §19.2.1.1: the outermost enclosing validation that is not `preserve` decides — a `preserve`
/// copy inside a literal result element under `strip` (the default) is untyped, while is-id is
/// unchanged ("The values of the is-id and is-idrefs properties are unchanged").
#[test]
fn a_preserve_copy_under_a_strip_parent_is_stripped() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><w><xsl:copy-of select="/e:doc/e:row" validation="preserve"/></w></xsl:variable>"#,
            "$t/w/e:row[1] instance of element(*, xs:untyped), \
             $t/w/e:row[1]/@id instance of attribute(*, xs:untypedAtomic), exists(id('r1', $t))"
        ),
        "true true true"
    );
}

// ── §19.2.1.1: xsl:copy-of with strip ────────────────────────────────────────────────────────

/// §19.2.1.1, `strip`: "the new node and each of the contained nodes will have the type
/// annotation xs:untyped if it is an element, or xs:untypedAtomic if it is an attribute … In the
/// case of elements the nilled property is set to false. The values of the is-id and is-idrefs
/// properties are unchanged."
#[test]
fn copy_of_strip_untypes_but_keeps_is_id() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><xsl:copy-of select="/e:doc/e:row" validation="strip"/></xsl:variable>"#,
            "$t/e:row[1] instance of element(*, xs:untyped), \
             $t/e:row[1]/@e:size instance of attribute(*, xs:untypedAtomic), \
             nilled($t/e:row[2]/e:n), exists(id('r1', $t))"
        ),
        "true true false true"
    );
    // An attribute copied on its own into a `preserve` element keeps its is-id too.
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><x xsl:validation="preserve"><xsl:copy-of select="/e:doc/e:row[1]/@id" validation="strip"/></x></xsl:variable>"#,
            "$t/x/@id instance of attribute(*, xs:untypedAtomic), exists(id('r1', $t))"
        ),
        "true true"
    );
    // A document node and a parentless element.
    assert_eq!(
        answers(
            r#"<xsl:variable name="d" as="document-node()"><xsl:copy-of select="/" validation="strip"/></xsl:variable>
               <xsl:variable name="s" as="element()"><xsl:copy-of select="/e:doc/e:row[1]" validation="strip"/></xsl:variable>"#,
            "$d/e:doc instance of element(*, xs:untyped), exists(id('r2', $d)), \
             exists(id('c1', $d)), $s instance of element(*, xs:untyped)"
        ),
        "true true true true"
    );
}

// ── §11.9.2 / §19.2.1.1: xsl:copy-of with strict ─────────────────────────────────────────────

/// §11.9.2: "These attributes are applied individually to each element, attribute, and document
/// node that is selected by the expression in the select attribute … they do not apply to nodes
/// that are implicitly copied because they have selected nodes as an ancestor. However, these
/// attributes do indirectly affect the type annotation carried by such implicitly copied nodes, as
/// a consequence of the validation process." — §19.2.1.1 `strict`: the copy is assessed against
/// the top-level declaration of its name.
#[test]
fn copy_of_strict_validates_each_selected_node() {
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}<xsl:variable name="t"><xsl:copy-of select="$u/e:row, $u/e:nums" validation="strict"/></xsl:variable>"#
            ),
            "$t/e:row instance of schema-element(e:row), \
             $t/e:row/e:n instance of element(*, xs:integer), data($t/e:row/e:n), \
             data($t/e:row/@e:size) instance of xs:integer, count(data($t/e:nums)), \
             $u/e:row instance of element(*, xs:untyped)"
        ),
        "true true 7 true 3 true"
    );
    // Parentless: an element and an attribute in sequences.
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}<xsl:variable name="s" as="element()"><xsl:copy-of select="$u/e:row" validation="strict"/></xsl:variable>
                   <xsl:variable name="a" as="attribute()"><xsl:copy-of select="$u/e:row/@e:size" validation="strict"/></xsl:variable>"#
            ),
            "$s instance of schema-element(e:row), $a instance of attribute(*, e:size), \
             $a instance of schema-attribute(e:size)"
        ),
        "true true true"
    );
}

/// §19.2.1.1: "If there is no matching element declaration, or if the element is not considered
/// valid, the transformation fails" (XTTE1510, XTTE1512 — both name `xsl:copy-of`); §19.2.1.3:
/// XTTE1545 for a parentless attribute validated against a type built from `xs:QName` ("This
/// affects the instructions xsl:attribute, xsl:copy, and xsl:copy-of").
#[test]
fn strict_copy_of_errors() {
    for (select, code) in [
        ("$u/e:n", codes::XTTE1510),
        ("$u/e:other", codes::XTTE1512),
        ("$u/e:row/@id", codes::XTTE1512),
        ("$u/e:y/@e:size", codes::XTTE1510),
        ("$u/e:x/@e:qn", codes::XTTE1545),
    ] {
        let e = error(&format!(
            r#"{UNTYPED}<out><xsl:copy-of select="{select}" validation="strict"/></out>"#
        ));
        assert_code(&e, code, select);
    }
}

// ── §11.9.1 / §19.2.1.1: xsl:copy ────────────────────────────────────────────────────────────

/// §19.2.1.1, `preserve`, "In the case of xsl:copy … Where the node being copied is an attribute,
/// the copied attribute will retain its type annotation. Where the node being copied is an
/// element, the copied element will have a type annotation of xs:anyType … but any contained nodes
/// will have their type annotations retained in the same way as with xsl:element" — and `strict`
/// validates the new node.
#[test]
fn xsl_copy_with_preserve_and_strict() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><xsl:for-each select="/e:doc/e:row[1]"><xsl:copy validation="preserve"><xsl:copy-of select="@*, *" validation="preserve"/></xsl:copy></xsl:for-each></xsl:variable>
               <xsl:variable name="a" as="attribute()"><xsl:for-each select="/e:doc/e:row[1]/@e:size"><xsl:copy validation="preserve"/></xsl:for-each></xsl:variable>"#,
            "$t/e:row instance of element(*, xs:anyType), \
             $t/e:row instance of element(*, xs:untyped), $t/e:row instance of schema-element(e:row), \
             $t/e:row/e:n instance of element(*, xs:integer), \
             $t/e:row/@e:size instance of attribute(*, e:size), $a instance of attribute(*, e:size)"
        ),
        "true false false true true true"
    );
    assert_eq!(
        answers(
            &format!(
                r#"{UNTYPED}<xsl:variable name="t"><xsl:for-each select="$u/e:row"><xsl:copy validation="strict"><xsl:copy-of select="@*, node()"/></xsl:copy></xsl:for-each></xsl:variable>
                   <xsl:variable name="a" as="attribute()"><xsl:for-each select="$u/e:row/@e:size"><xsl:copy validation="strict"/></xsl:for-each></xsl:variable>"#
            ),
            "$t/e:row instance of schema-element(e:row), data($t/e:row/e:n), \
             $a instance of attribute(*, e:size)"
        ),
        "true 7 true"
    );
    for (content, code) in [
        (
            r#"<xsl:for-each select="$u/e:n"><xsl:copy validation="strict">x</xsl:copy></xsl:for-each>"#,
            codes::XTTE1510,
        ),
        (
            r#"<xsl:for-each select="$u/e:other"><xsl:copy validation="strict"/></xsl:for-each>"#,
            codes::XTTE1512,
        ),
        (
            r#"<xsl:for-each select="$u/e:y/@e:size"><xsl:copy validation="strict"/></xsl:for-each>"#,
            codes::XTTE1510,
        ),
    ] {
        let e = error(&format!("{UNTYPED}<out>{content}</out>"));
        assert_code(&e, code, content);
    }
}

// ── XTTE0950 ─────────────────────────────────────────────────────────────────────────────────

/// §11.9.1, XTTE0950: "It is a type error to use the xsl:copy or xsl:copy-of instruction to copy a
/// node that has namespace-sensitive content if the copy-namespaces attribute has the value no
/// and its explicit or implicit validation attribute has the value preserve. It is also a type
/// error if either of these instructions (with validation="preserve") is used to copy an
/// attribute having namespace-sensitive content, unless the parent element is also copied. A node
/// has namespace-sensitive content if its typed value contains an item of type xs:QName or
/// xs:NOTATION or a type derived therefrom."
#[test]
fn namespace_sensitive_copies_with_preserve_are_xtte0950() {
    for content in [
        // An element whose typed value is a QName, and one whose attribute's is, with no
        // namespaces copied.
        r#"<xsl:copy-of select="/e:doc/e:qe" copy-namespaces="no" validation="preserve"/>"#,
        r#"<xsl:copy-of select="/e:doc/e:qa" copy-namespaces="no" validation="preserve"/>"#,
        r#"<xsl:copy-of select="/e:doc" copy-namespaces="no" validation="preserve"/>"#,
        // An attribute copied without its parent, whatever copy-namespaces says.
        r#"<xsl:copy-of select="/e:doc/e:qa/@q" validation="preserve"/>"#,
        // xsl:copy.
        r#"<xsl:for-each select="/e:doc/e:qe"><xsl:copy copy-namespaces="no" validation="preserve"/></xsl:for-each>"#,
        r#"<xsl:for-each select="/e:doc/e:qa/@q"><xsl:copy validation="preserve"/></xsl:for-each>"#,
    ] {
        let e = error(&format!("<out>{content}</out>"));
        assert_code(&e, codes::XTTE0950, content);
    }
    // "explicit or implicit": `preserve` from `default-validation` (§3.6).
    let e = error_with(
        r#"default-validation="preserve""#,
        r#"<out><xsl:copy-of select="/e:doc/e:qa/@q"/></out>"#,
    );
    assert_code(&e, codes::XTTE0950, "default-validation");
    // Not errors: the namespaces copied with the element; `strip` (the typed value is then the
    // string value); a union-typed attribute whose value is an integer — its typed value contains
    // no QName; a node without namespace-sensitive content.
    let main = sheet(
        r#"<xsl:template match="/"><out>
             <xsl:copy-of select="/e:doc/e:qa/@q" validation="strip"/>
             <xsl:copy-of select="/e:doc/e:qa/@iq" validation="preserve"/>
             <xsl:copy-of select="/e:doc/e:qe, /e:doc/e:qa" validation="preserve"/>
             <xsl:copy-of select="/e:doc/e:row[1]" copy-namespaces="no" validation="preserve"/>
           </out></xsl:template>"#,
    );
    let text = run(&main).unwrap();
    assert!(
        text.contains("<e:qe") && text.contains(r#"iq="5""#),
        "{text}"
    );
}

// ── §19.2.2: document-level validation ───────────────────────────────────────────────────────

/// §19.2.2: on `xsl:document` with `validation="strict"`, "the children of the document node
/// comprise exactly one element node, no text nodes, and zero or more comment and processing
/// instruction nodes, in any order. The single element node child is validated … All other
/// children of the document node (comments and processing instructions) are copied unchanged."
#[test]
fn xsl_document_strict_validates_its_element() {
    assert_eq!(
        answers(
            r#"<xsl:variable name="d" as="document-node()"><xsl:document validation="strict"><xsl:comment>c</xsl:comment><e:row id="a"><e:n> 1 </e:n></e:row><xsl:processing-instruction name="p">x</xsl:processing-instruction></xsl:document></xsl:variable>
               <xsl:variable name="t"><xsl:document validation="strict"><e:row id="b"/></xsl:document></xsl:variable>"#,
            "$d instance of document-node(schema-element(e:row)), data($d/e:row/e:n), \
             count($d/comment()) + count($d/processing-instruction()), \
             $t/e:row instance of schema-element(e:row), exists(id('b', $t))"
        ),
        "true 1 2 true true"
    );
}

/// XTTE1550: "A type error occurs unless the children of the document node comprise exactly one
/// element node, no text nodes, and zero or more comment and processing instruction nodes" —
/// checked before the element's own validation, so it wins over an invalid first element; a
/// document node in element content is still validated (§19.2.2 validates the document node the
/// instruction constructs; §5.7.1 then replaces it by its children).
#[test]
fn a_validated_document_needs_exactly_one_element_and_no_text() {
    for content in [
        r#"<xsl:document validation="strict"><e:row/><e:row/></xsl:document>"#,
        r#"<xsl:document validation="strict"/>"#,
        r#"<xsl:document validation="strict"><xsl:comment>c</xsl:comment></xsl:document>"#,
        r#"<xsl:document validation="strict">text<e:row/></xsl:document>"#,
        r#"<xsl:document validation="strict"><e:row/><xsl:text> </xsl:text></xsl:document>"#,
        r#"<xsl:document validation="strict"><e:n>x</e:n><e:n>1</e:n></xsl:document>"#,
        r#"<out><xsl:document validation="strict"><e:row/><e:row/></xsl:document></out>"#,
        // An empty one in element content, which contributes nothing to the element.
        r#"<out><xsl:document validation="strict"/></out>"#,
        r#"<out><xsl:copy-of select="$e" validation="strict"/></out>"#,
    ] {
        let e = error(&format!(
            r#"<xsl:variable name="e"><xsl:sequence select="()"/></xsl:variable><xsl:variable name="d" as="item()">{content}</xsl:variable><out><xsl:value-of select="count($d/node())"/></out>"#
        ));
        assert_code(&e, codes::XTTE1550, content);
    }
}

/// §19.2.2: "The validation rule "Validation Root Valid (ID/IDREF)" is applied to the single
/// element node child of the document node. This means that validation will fail if there are
/// non-unique ID values or dangling IDREF values in the document tree." (XTTE1555) — where
/// element-level validation does not apply it (§19.2.1.3), nor `preserve`. An identity
/// constraint is part of the element's own validation (§19.2.1.3: "Identity-constraint
/// Satisfied" "should be applied"), so its failure is XTTE1510.
#[test]
fn a_validated_document_applies_id_idref_rules() {
    for (content, code) in [
        (
            r#"<e:table><e:row id="a"/><e:row id="a"/></e:table>"#,
            codes::XTTE1555,
        ),
        (r#"<e:table><e:row ref="zz"/></e:table>"#, codes::XTTE1555),
        (
            r#"<e:table><e:row e:size="3"/><e:row e:size="3"/></e:table>"#,
            codes::XTTE1510,
        ),
        (r#"<e:other/>"#, codes::XTTE1512),
        // The innermost failure first: the element's own validation before the document's.
        (
            r#"<e:table><e:row id="a"/><e:row id="a"/><e:row xsl:validation="strict" e:size="99"/></e:table>"#,
            codes::XTTE1510,
        ),
    ] {
        let e = error(&format!(
            r#"<xsl:variable name="d" as="document-node()"><xsl:document validation="strict">{content}</xsl:document></xsl:variable><out><xsl:value-of select="count($d/node())"/></out>"#
        ));
        assert_code(&e, code, content);
    }
    // Element-level validation, and a `preserve` document, do not apply the ID/IDREF rule.
    assert_eq!(
        answers(
            r#"<xsl:variable name="t"><e:table xsl:validation="strict"><e:row id="a"/><e:row id="a" ref="zz"/></e:table></xsl:variable>
               <xsl:variable name="d" as="document-node()"><xsl:document validation="preserve"><e:table xsl:validation="strict"><e:row id="a"/><e:row id="a"/></e:table></xsl:document></xsl:variable>"#,
            "$t/e:table instance of schema-element(e:table), \
             $d/e:table instance of schema-element(e:table)"
        ),
        "true true"
    );
}

/// §19.1 / §19.2.2: `xsl:result-document` with `validation="strict"` validates the new document
/// node; `preserve` keeps the annotations of what it contains.
#[test]
fn xsl_result_document_validation() {
    let main = sheet(
        r#"<xsl:template match="/"><xsl:result-document validation="strict"><e:row id="a"><e:n>2</e:n></e:row></xsl:result-document></xsl:template>"#,
    );
    let typed = run_with(&main, |outcome, text| {
        let doc = outcome.primary.expect("a primary result");
        (doc.has_type_annotations(), text)
    })
    .unwrap();
    assert_eq!(
        typed,
        (
            true,
            r#"<e:row xmlns:e="urn:copies" id="a"><e:n>2</e:n></e:row>"#.to_string()
        )
    );
    let main = sheet(
        r#"<xsl:template match="/"><xsl:result-document validation="preserve"><xsl:copy-of select="/e:doc/e:row[1]" validation="preserve"/></xsl:result-document></xsl:template>"#,
    );
    assert!(run_with(&main, |outcome, _| outcome
        .primary
        .unwrap()
        .has_type_annotations())
    .unwrap());
    for (content, code) in [
        (r#"<e:row/><e:row/>"#, codes::XTTE1550),
        (
            r#"<e:table><e:row id="a"/><e:row id="a"/></e:table>"#,
            codes::XTTE1555,
        ),
        (r#"<e:row e:size="99"/>"#, codes::XTTE1510),
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="/"><xsl:result-document validation="strict">{content}</xsl:result-document></xsl:template>"#
        ));
        let e = run(&main).unwrap_err();
        assert_code(&e, code, content);
    }
}

/// §19.2.2: validation is applied to a document node "constructed by one of the instructions
/// xsl:document, xsl:result-document, xsl:copy, or xsl:copy-of" — a copy of a document node with
/// `validation="strict"` is validated as a document (§11.9.2: "These attributes are applied
/// individually to each element, attribute, and document node that is selected").
#[test]
fn a_strict_copy_of_a_document_node_is_document_validation() {
    let n = r#"<xsl:variable name="n"><e:row id="a"><e:n>3</e:n></e:row></xsl:variable>
               <xsl:variable name="two"><e:row/><e:row/></xsl:variable>
               <xsl:variable name="dup"><e:table><e:row id="a"/><e:row id="a"/></e:table></xsl:variable>"#;
    assert_eq!(
        answers(
            &format!(
                r#"{n}<xsl:variable name="c" as="document-node()"><xsl:copy-of select="$n" validation="strict"/></xsl:variable>
                   <xsl:variable name="v"><xsl:copy-of select="$n" validation="strict"/></xsl:variable>
                   <xsl:variable name="s" as="document-node()"><xsl:for-each select="$n"><xsl:copy validation="strict"><xsl:copy-of select="node()"/></xsl:copy></xsl:for-each></xsl:variable>"#
            ),
            "$c instance of document-node(schema-element(e:row)), \
             $v/e:row instance of schema-element(e:row), data($v/e:row/e:n), \
             $s instance of document-node(schema-element(e:row))"
        ),
        "true true 3 true"
    );
    for (content, code) in [
        (
            r#"<xsl:copy-of select="$two" validation="strict"/>"#,
            codes::XTTE1550,
        ),
        (
            r#"<xsl:copy-of select="$dup" validation="strict"/>"#,
            codes::XTTE1555,
        ),
        (
            r#"<xsl:for-each select="$two"><xsl:copy validation="strict"><xsl:copy-of select="node()"/></xsl:copy></xsl:for-each>"#,
            codes::XTTE1550,
        ),
    ] {
        let e = error(&format!("{n}<out>{content}</out>"));
        assert_code(&e, code, content);
    }
}

/// §19.2.2: "Similarly, validation is not applied to document nodes created using xsl:message" —
/// while an element constructed in a message with `strict` is validated like any other (§17
/// builds the message's document with §5.7.1; §19.2.1.1 validates every constructed element).
#[test]
fn message_documents_are_not_document_validated() {
    let main = sheet(
        r#"<xsl:template match="/"><xsl:message><e:table xsl:validation="strict"><e:row id="a"/><e:row id="a"/></e:table><e:row/>text</xsl:message><out/></xsl:template>"#,
    );
    let messages = run_with(&main, |outcome, _| outcome.messages.len()).unwrap();
    assert_eq!(messages, 1);
    let e = error(r#"<xsl:message><e:n xsl:validation="strict">x</e:n></xsl:message><out/>"#);
    assert_code(&e, codes::XTTE1510, "a strict element in a message");
}

/// §3.6: `default-validation? = "preserve" | "strip"` — the implicit final result tree is never
/// validated as a document (§19.2.2: "The values validation="preserve" and validation="strip" do
/// not request validation"), so it may have two element children; `strict` is not a value of the
/// attribute (XTSE0020).
#[test]
fn the_implicit_final_result_tree_is_never_document_validated() {
    let main = sheet_with(
        r#"default-validation="preserve""#,
        r#"<xsl:template match="/"><e:row/><e:row/></xsl:template>"#,
    );
    assert_eq!(
        run(&main).unwrap(),
        r#"<e:row xmlns:e="urn:copies"/><e:row xmlns:e="urn:copies"/>"#
    );
    let main = sheet_with(
        r#"default-validation="strict""#,
        r#"<xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_code(
        &run(&main).unwrap_err(),
        codes::XTSE0020,
        "default-validation=strict",
    );
}

/// A defaulted attribute in the XML namespace (§19.2.1.3: "If default values for elements or
/// attributes are defined in the schema, the validation process will where necessary create new
/// nodes containing these default values") takes the prefix `xml`, which is bound to that
/// namespace by definition and to no other (Namespaces in XML 1.0 §3: "The prefix xml is by
/// definition bound to the namespace name http://www.w3.org/XML/1998/namespace … Other prefixes
/// MUST NOT be bound to this namespace name") — the shape of XHTML's `pre` in
/// `validation-0202`, whose result document is validated.
#[test]
fn a_defaulted_attribute_in_the_xml_namespace_has_the_prefix_xml() {
    let main = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xmlns:x=\"urn:copies-pre\">\
           <xsl:import-schema namespace=\"http://www.w3.org/XML/1998/namespace\"/>\
           <xsl:import-schema namespace=\"urn:copies-pre\" schema-location=\"pre.xsd\"/>\
           <xsl:template match=\"/\"><xsl:result-document validation=\"strict\"><x:pre>a</x:pre></xsl:result-document></xsl:template>\
         </xsl:stylesheet>"
    );
    assert_eq!(
        run_over(&main, SourceValidation::Skip, |_, text| text).unwrap(),
        r#"<x:pre xmlns:x="urn:copies-pre" xml:space="preserve">a</x:pre>"#
    );
}

// ── Simple content ───────────────────────────────────────────────────────────────────────────

/// §5.7.2: "The sequence is atomized" — a copy in simple content contributes its typed value: the
/// source's with `preserve`, the validated one with `strict`, the string value with `strip`.
#[test]
fn a_copy_in_simple_content_contributes_its_typed_value() {
    let main = sheet(&format!(
        r#"<xsl:template match="/">{UNTYPED}<out>
             <a><xsl:value-of separator=","><xsl:copy-of select="/e:doc/e:nums" validation="preserve"/></xsl:value-of></a>
             <b><xsl:value-of separator=","><xsl:copy-of select="$u/e:nums" validation="strict"/></xsl:value-of></b>
             <c><xsl:value-of separator=","><xsl:copy-of select="/e:doc/e:nums" validation="strip"/></xsl:value-of></c>
             <d><xsl:value-of separator=","><xsl:copy-of select="/e:doc/e:row[1]/@e:size, $u/e:row/@e:size" validation="strict"/></xsl:value-of></d>
           </out></xsl:template>"#
    ));
    assert_eq!(
        inner(&run(&main).unwrap()),
        "<a>1,2,3</a><b>1,2,3</b><c>1 2 3</c><d>3,4</d>"
    );
}

// ── A node typed against another schema set ─────────────────────────────────────────────────

/// One schema set per transformation; a tree typed against another set can only come in through
/// `DocumentStore::insert`, and is stripped with a warning when it is admitted
/// (`tests/typed_sources.rs`) — so
/// a `preserve` copy of it is untyped, never typed with keys of a foreign set.
#[test]
fn a_tree_typed_against_another_set_is_copied_untyped() {
    let other = SchemaSetBuilder::new()
        .add_source(SCHEMA_XSD, "file:///elsewhere/copies.xsd")
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let main = sheet(
        r#"<xsl:template match="/"><xsl:variable name="t"><xsl:copy-of select="/e:doc/e:row[1]" validation="preserve"/></xsl:variable><out><xsl:value-of select="$t/e:row instance of element(*, xs:untyped)"/></out></xsl:template>"#,
    );
    let resolver = resolver(&main);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .unwrap()
        .build()
        .unwrap();
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let doc = build_typed_document(
        DOC.as_bytes(),
        &arena,
        &other,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    assert!(doc.has_type_annotations());
    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run.store().borrow_mut().insert(doc, Some(SOURCE));
    let outcome = run.transform_document(id).unwrap();
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>true</out>"
    );
    assert_eq!(run.take_warnings().len(), 1);
}

// ── `lax` and `type` on copies and documents ─────────────────────────────────────────────────

/// `lax` and `type` on copies and document nodes (`tests/type_driven_and_lax_validation.rs` checks
/// what they do): valid copies and documents are built, an invalid one is XTTE1515 (`lax`: the
/// copied `e:doc` element has no `row`). A copy is validated against an XPath 2.0 duration type
/// XML Schema 1.0 does not define, and an element with element children is not valid against a
/// simple type: XTTE1540 (`tests/xpath_types_timezone_and_locations.rs`).
#[test]
fn lax_and_type_on_copies_and_documents() {
    for body in [
        r#"<out><xsl:copy-of select="/e:doc/e:row[1]" validation="lax"/></out>"#,
        r#"<out><xsl:copy-of select="/e:doc/e:row[1]" type="xs:anyType"/></out>"#,
        r#"<out><xsl:document validation="lax"><e:row/></xsl:document></out>"#,
        r#"<out><xsl:document type="xs:anyType"><e:row/></xsl:document></out>"#,
        r#"<xsl:result-document validation="lax"><e:row/></xsl:result-document>"#,
    ] {
        let main = sheet(&format!(r#"<xsl:template match="/">{body}</xsl:template>"#));
        if let Err(e) = run(&main) {
            panic!("{body}: {e}");
        }
    }
    assert_code(
        &error(
            r#"<out><xsl:for-each select="/e:doc"><xsl:copy validation="lax"/></xsl:for-each></out>"#,
        ),
        codes::XTTE1515,
        "an empty lax copy of e:doc",
    );
    assert_code(
        &error(r#"<out><xsl:copy-of select="/e:doc/e:row" type="xs:yearMonthDuration"/></out>"#),
        codes::XTTE1540,
        "a copy of e:row against xs:yearMonthDuration",
    );
}

// ══ Fix round ════════════════════════════════════════════════════════════════════════════════

/// §19.2.2 validates the document node an instruction constructs; §5.7.1 then puts its children in
/// the element's content: "Any document node within the result sequence is replaced by a sequence
/// containing each of its children, in document order … Adjacent text nodes within the result
/// sequence are merged into a single text node." Text, comments and processing instructions next
/// to the document node — before or after it, in element content or at the top of a temporary
/// tree, next to `xsl:document` or to a copy of a document node — are the element's (or the tree's)
/// children, not the document node's: no XTTE1550 for them. Text inside the document node still is.
#[test]
fn nodes_around_a_validated_document_are_not_its_children() {
    let n = r#"<xsl:variable name="n"><e:row id="a"/></xsl:variable>"#;
    let mut wrong = Vec::new();
    for (content, expected) in [
        (
            r#"<out>before<xsl:document validation="strict"><e:row/></xsl:document></out>"#,
            r#"<out>before<e:row xmlns:e="urn:copies"/></out>"#,
        ),
        (
            r#"<out><xsl:document validation="strict"><e:row/></xsl:document>after</out>"#,
            r#"<out><e:row xmlns:e="urn:copies"/>after</out>"#,
        ),
        (
            r#"<out>before<xsl:document validation="strict"><xsl:comment>c</xsl:comment><e:row/></xsl:document>after</out>"#,
            r#"<out>before<!--c--><e:row xmlns:e="urn:copies"/>after</out>"#,
        ),
        (
            r#"<out><xsl:comment>c</xsl:comment><xsl:processing-instruction name="p"/><xsl:document validation="strict"><e:row/></xsl:document></out>"#,
            r#"<out><!--c--><?p?><e:row xmlns:e="urn:copies"/></out>"#,
        ),
        (
            r#"<out>before<xsl:copy-of select="$n" validation="strict"/>after</out>"#,
            r#"<out>before<e:row xmlns:e="urn:copies" id="a"/>after</out>"#,
        ),
        (
            r#"<out>before<xsl:for-each select="$n"><xsl:copy validation="strict"><xsl:copy-of select="node()"/></xsl:copy></xsl:for-each>after</out>"#,
            r#"<out>before<e:row xmlns:e="urn:copies" id="a"/>after</out>"#,
        ),
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="/">{n}{content}</xsl:template>"#
        ));
        match run(&main) {
            Ok(text) if text == expected => {}
            Ok(text) => wrong.push(format!("{content}: {text}")),
            Err(e) => wrong.push(format!("{content}: {e}")),
        }
    }
    // At the top of a temporary tree (document content): the text is the tree's child.
    let main = sheet(
        r#"<xsl:template match="/"><xsl:variable name="d"><xsl:text>x</xsl:text><xsl:document validation="strict"><e:row id="b"/></xsl:document><xsl:text>y</xsl:text></xsl:variable><out><xsl:value-of select="string($d), count($d/node()), $d/e:row instance of schema-element(e:row)"/></out></xsl:template>"#,
    );
    match run(&main) {
        Ok(text) if text == "<out>xy 3 true</out>" => {}
        Ok(text) => wrong.push(format!("document content: {text}")),
        Err(e) => wrong.push(format!("document content: {e}")),
    }
    // The same in the other places a document node can open after pending text: the document of an
    // `xsl:message` (§17: its content is constructed as §5.7.1 says), simple content (§5.7.2: the
    // document node is one item, atomized), a sequence (§6.3: the text is an item of its own).
    for (content, body, expected) in [
        (
            r#"<xsl:message>before<xsl:document validation="strict"><e:row/></xsl:document></xsl:message>"#,
            "'m'",
            "<out>m</out>",
        ),
        (
            r#"<xsl:variable name="v"><xsl:value-of>before<xsl:document validation="strict"><e:n> 1 </e:n></xsl:document>after</xsl:value-of></xsl:variable>"#,
            "string($v)",
            "<out>before1after</out>",
        ),
        (
            r#"<xsl:variable name="s" as="item()*"><xsl:text>x</xsl:text><xsl:document validation="strict"><e:row/></xsl:document></xsl:variable>"#,
            "count($s), $s[2] instance of document-node(schema-element(e:row))",
            "<out>2 true</out>",
        ),
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="/">{content}<out><xsl:value-of select="{body}"/></out></xsl:template>"#
        ));
        match run(&main) {
            Ok(text) if text == expected => {}
            Ok(text) => wrong.push(format!("{content}: {text}")),
            Err(e) => wrong.push(format!("{content}: {e}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // Text that is the document node's own child is still XTTE1550.
    for content in [
        r#"<out>before<xsl:document validation="strict">x<e:row/></xsl:document></out>"#,
        r#"<out><xsl:document validation="strict"><e:row/>x</xsl:document>after</out>"#,
    ] {
        assert_code(&error(content), codes::XTTE1550, content);
    }
}

/// §19.2.1.1: "the validation requested for a child element or attribute node is applied before
/// the validation requested for its parent element" — so the outermost enclosing validation that
/// is not `preserve` decides the annotation, whether the boundaries are documents (§19.2.2), elements
/// or copies (§11.9.1, §11.9.2), in any nesting: outside a document node (`xsl:document`, `xsl:copy`
/// of a document node), an element (a literal result element, `xsl:copy` of an element), inside a
/// document node, an element, a copy of an element or of a document node. An outer `strict` gives the leaf `e:n` its `xs:integer`, an
/// outer `strip` untypes it, an outer `preserve` leaves it to the inner one (and a `preserve` leaf
/// constructed under `preserve` is `xs:anyType`, a `preserve` copy of a typed node keeps its type).
#[test]
fn the_outermost_boundary_decides_across_documents_elements_and_copies() {
    let leaf = r#"<e:n xsl:validation="preserve">5</e:n>"#;
    let mut vars = String::new();
    let mut probes = Vec::new();
    let mut expected = Vec::new();
    let mut i = 0;
    for outer_kind in ["doc", "elem", "copy", "copydoc"] {
        for outer in ["strict", "strip", "preserve"] {
            for inner_kind in ["doc", "elem", "copy", "copydoc"] {
                for inner in ["strict", "strip", "preserve"] {
                    i += 1;
                    let (content, wrapper) = match inner_kind {
                        "doc" => (
                            format!(r#"<xsl:document validation="{inner}">{leaf}</xsl:document>"#),
                            "w",
                        ),
                        "elem" => (
                            format!(r#"<e:w xsl:validation="{inner}">{leaf}</e:w>"#),
                            "ww",
                        ),
                        "copy" => (
                            format!(r#"<xsl:copy-of select="$S" validation="{inner}"/>"#),
                            "w",
                        ),
                        _ => (
                            format!(r#"<xsl:copy-of select="$D" validation="{inner}"/>"#),
                            "w",
                        ),
                    };
                    vars.push_str(&match outer_kind {
                        "doc" => format!(
                            r#"<xsl:variable name="t{i}" as="document-node()"><xsl:document validation="{outer}">{content}</xsl:document></xsl:variable>"#
                        ),
                        "elem" => format!(
                            r#"<xsl:variable name="t{i}"><e:{wrapper} xsl:validation="{outer}">{content}</e:{wrapper}></xsl:variable>"#
                        ),
                        // `xsl:copy` of an (untyped) element, and of a document node, with content.
                        "copy" => format!(
                            r#"<xsl:variable name="t{i}"><xsl:for-each select="$E/e:{wrapper}"><xsl:copy validation="{outer}">{content}</xsl:copy></xsl:for-each></xsl:variable>"#
                        ),
                        _ => format!(
                            r#"<xsl:variable name="t{i}" as="document-node()"><xsl:for-each select="$E0"><xsl:copy validation="{outer}">{content}</xsl:copy></xsl:for-each></xsl:variable>"#
                        ),
                    });
                    probes.push(format!("c(($t{i}//e:n)[1])"));
                    let class = match (outer, inner, inner_kind) {
                        ("strict", _, _) => "int",
                        ("strip", _, _) => "untyped",
                        (_, "strict", _) => "int",
                        (_, "strip", _) => "untyped",
                        (_, _, "doc" | "elem") => "any",
                        _ => "int",
                    };
                    expected.push(format!("{outer_kind}-{outer}/{inner_kind}-{inner}={class}"));
                }
            }
        }
    }
    let labels: Vec<String> = expected
        .iter()
        .map(|e| e.split('=').next().unwrap().to_string())
        .collect();
    let main = sheet(&format!(
        r#"<xsl:function name="e:c" as="xs:string"><xsl:param name="n" as="element()"/><xsl:sequence select="if ($n instance of element(*, xs:integer)) then 'int' else if ($n instance of element(*, xs:untyped)) then 'untyped' else if ($n instance of element(*, xs:anyType)) then 'any' else 'other'"/></xsl:function>
           <xsl:template match="/"><xsl:variable name="S" select="/e:doc/e:row[1]/e:n"/><xsl:variable name="D"><xsl:copy-of select="$S" validation="preserve"/></xsl:variable><xsl:variable name="E"><e:w/><e:ww/></xsl:variable><xsl:variable name="E0"><xsl:sequence select="()"/></xsl:variable>{vars}<out><xsl:value-of select="{}" separator=" "/></out></xsl:template>"#,
        probes.join(", ").replace("c((", "e:c((")
    ));
    let text = inner(&run(&main).unwrap_or_else(|e| {
        let column = e.location.as_ref().map_or(0, |l| l.column as usize);
        let line2 = main.lines().nth(1).unwrap_or("");
        let at = line2.get(column.saturating_sub(300)..(column + 100).min(line2.len()));
        panic!("{e}\nnear: {at:?}")
    }));
    let got: Vec<String> = labels
        .iter()
        .zip(text.split(' '))
        .map(|(label, class)| format!("{label}={class}"))
        .collect();
    let wrong: Vec<String> = got
        .iter()
        .zip(expected.iter())
        .filter(|(got, expected)| got != expected)
        .map(|(got, expected)| format!("{got} (expected {expected})"))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} wrong:\n{}",
        wrong.len(),
        expected.len(),
        wrong.join("\n")
    );
}
