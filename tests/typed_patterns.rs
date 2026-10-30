//! Typed patterns.
//!
//! A pattern step's NodeTest may be any XPath 2.0 kind test (§5.5.2: "The constructs NodeTest,
//! PredicateList, … are part of the XPath expression language"), so a schema-aware processor
//! matches `element(*, T)`, `element(N, T)`, `element(N, T?)`, `attribute(*, T)`,
//! `attribute(N, T)`, `schema-element(N)`, `schema-attribute(N)` and `document-node(…)` with one
//! of them, by XPath 2.0 §2.5.4.2 – §2.5.4.6, in every pattern: `match` of
//! `xsl:template` and `xsl:key`, `count` / `from` of `xsl:number`, `group-starting-with` /
//! `group-ending-with`. The default priorities are §6.4's table.
//!
//! Every test goes through the public API and cites the rule it checks. The typed source is
//! validated against the stylesheet's own `xsl:import-schema`s (`SourceValidation::Strict`);
//! the same source unvalidated (`Skip`) is the untyped control.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///patterns/main.xsl";
const SOURCE: &str = "file:///patterns/shop.xml";

/// `urn:shop`: a substitution group headed by `product` — `book` and `gadget` are members,
/// `ebook` a member of `book`'s group and so, transitively, of `product`'s; the types derive the
/// same way (`ebookType` from `bookType` from `productType`). `price` is a global, nillable
/// element of the user type `money` (a restriction of `xs:decimal`); `currency` a global
/// attribute; `sku` a local attribute of the user type `skuType`.
const SHOP_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:shop"
    targetNamespace="urn:shop" elementFormDefault="qualified">
  <xs:element name="shop">
    <xs:complexType>
      <xs:sequence>
        <xs:element ref="product" maxOccurs="unbounded"/>
        <xs:element name="note" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:element name="product" type="productType"/>
  <xs:element name="book" type="bookType" substitutionGroup="product"/>
  <xs:element name="ebook" type="ebookType" substitutionGroup="book"/>
  <xs:element name="gadget" type="productType" substitutionGroup="product"/>
  <xs:element name="price" type="money" nillable="true"/>
  <xs:complexType name="productType">
    <xs:sequence><xs:element ref="price"/></xs:sequence>
    <xs:attribute name="sku" type="skuType"/>
    <xs:attribute ref="currency"/>
  </xs:complexType>
  <xs:complexType name="bookType">
    <xs:complexContent>
      <xs:extension base="productType">
        <xs:sequence><xs:element name="isbn" type="xs:string"/></xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
  <xs:complexType name="ebookType">
    <xs:complexContent>
      <xs:extension base="bookType">
        <xs:sequence><xs:element name="format" type="xs:token"/></xs:sequence>
      </xs:extension>
    </xs:complexContent>
  </xs:complexType>
  <xs:simpleType name="money"><xs:restriction base="xs:decimal"/></xs:simpleType>
  <xs:simpleType name="skuType">
    <xs:restriction base="xs:string"><xs:pattern value="[A-Z]{2}[0-9]+"/></xs:restriction>
  </xs:simpleType>
  <xs:attribute name="currency" type="currencyType"/>
  <xs:simpleType name="currencyType">
    <xs:restriction base="xs:string">
      <xs:enumeration value="EUR"/><xs:enumeration value="USD"/>
    </xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

/// `urn:more`: a member of `s:product`'s substitution group in **another namespace** — the
/// dispatch index must file `schema-element(s:product)` under `m:special` too.
const MORE_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:s="urn:shop"
    targetNamespace="urn:more" elementFormDefault="qualified">
  <xs:import namespace="urn:shop" schemaLocation="shop.xsd"/>
  <xs:element name="special" type="s:productType" substitutionGroup="s:product"/>
</xs:schema>"#;

/// Valid against `SHOP_XSD` + `MORE_XSD`. Elements in document order: `s:shop`, `s:product`,
/// `s:price`, `s:book`, `s:price`, `s:isbn`, `s:ebook`, `s:price` (nilled), `s:isbn`, `s:format`,
/// `s:gadget`, `s:price`, `m:special`, `s:price`, `s:note`.
const SHOP: &str = r#"<s:shop xmlns:s="urn:shop" xmlns:m="urn:more"
        xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <s:product sku="AB1" s:currency="EUR"><s:price>10</s:price></s:product>
  <s:book sku="BK2"><s:price>20.5</s:price><s:isbn>123</s:isbn></s:book>
  <s:ebook sku="EB3"><s:price xsi:nil="true"/><s:isbn>456</s:isbn><s:format>pdf</s:format></s:ebook>
  <s:gadget sku="GD4" s:currency="USD"><s:price>5</s:price></s:gadget>
  <m:special sku="SP5"><s:price>7</s:price></m:special>
  <s:note>n</s:note>
</s:shop>"#;

/// Every element of `SHOP`, as `matched` writes them.
const ALL_ELEMENTS: &str = "s:shop s:product s:price s:book s:price s:isbn s:ebook s:price s:isbn \
                            s:format s:gadget s:price m:special s:price s:note";

/// Every element of `SHOP` but the nilled `s:price`.
const NON_NILLED_ELEMENTS: &str = "s:shop s:product s:price s:book s:price s:isbn s:ebook s:isbn \
                                   s:format s:gadget s:price m:special s:price s:note";

/// The attributes of `SHOP` other than `xsi:nil`, in document order.
const ATTRIBUTES: &str = "//@*[namespace-uri() ne 'http://www.w3.org/2001/XMLSchema-instance']";

const ALL_ATTRIBUTES: &str = "sku s:currency sku sku sku s:currency sku";

fn sheet(top: &str) -> String {
    sheet_with("", top)
}

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:s=\"urn:shop\" xmlns:m=\"urn:more\" \
           exclude-result-prefixes=\"xs s m\" {attrs}>\
           <xsl:import-schema namespace=\"urn:shop\" schema-location=\"shop.xsd\"/>\
           <xsl:import-schema namespace=\"urn:more\" schema-location=\"more.xsd\"/>\
           {top}</xsl:stylesheet>"
    )
}

fn resolver(main: &str) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert("file:///patterns/shop.xsd", SHOP_XSD);
    mem.insert("file:///patterns/more.xsd", MORE_XSD);
    Arc::new(mem)
}

/// Schema-aware: compiles `main` against the context its `xsl:import-schema`s make, and runs it
/// over `SHOP` validated as `validation` says.
fn run(main: &str, validation: SourceValidation) -> Result<String, XsltError> {
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
    let outcome = transformation.transform_source_with(SOURCE, SHOP, validation)?;
    Ok(common::body(compiled.serialize_primary(&outcome)?))
}

/// A basic processor (§21.1): no `xsl:import-schema`, an untyped source.
fn run_basic(top: &str, source: &str) -> Result<String, XsltError> {
    let main = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:s=\"urn:shop\" \
           exclude-result-prefixes=\"xs s\">{top}</xsl:stylesheet>"
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions).compile_str(&main, MAIN)?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source(SOURCE, source)?;
    Ok(common::body(compiled.serialize_primary(&outcome)?))
}

fn out(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => text,
        Err(e) => panic!("the run failed: {e}"),
    }
}

fn err(result: Result<String, XsltError>) -> XsltError {
    match result {
        Ok(text) => panic!("expected an error, got {text}"),
        Err(e) => e,
    }
}

/// The text content of `<out>…</out>`, trimmed.
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

/// The template rule of `pattern` decides which of the nodes `select` picks it matches: through
/// `xsl:apply-templates`, so through the dispatch index of the mode, with a
/// catch-all rule of priority −9 for every other node. The matching nodes' names, in document
/// order (`#doc` for a document node).
fn matched_top(top: &str, pattern: &str, select: &str, validation: SourceValidation) -> String {
    let main = sheet(&format!(
        r#"{top}
           <xsl:template match="/"><out><xsl:apply-templates select="{select}" mode="t"/></out></xsl:template>
           <xsl:template match="{pattern}" mode="t">
             <xsl:value-of select="if (. instance of document-node()) then '#doc' else name()"/>
             <xsl:text> </xsl:text>
           </xsl:template>
           <xsl:template match="node() | @* | document-node()" mode="t" priority="-9"/>"#
    ));
    inner(&out(run(&main, validation)))
}

fn matched(pattern: &str, select: &str, validation: SourceValidation) -> String {
    matched_top("", pattern, select, validation)
}

fn typed(pattern: &str, select: &str) -> String {
    matched(pattern, select, SourceValidation::Strict)
}

fn untyped(pattern: &str, select: &str) -> String {
    matched(pattern, select, SourceValidation::Skip)
}

// ── XPath 2.0 §2.5.4.3: element(*, T), element(N, T), element(N, T?) ────────────────────────

/// §2.5.4.3: "`element(*, TypeName)` matches an element node regardless of its name, if
/// derives-from(AT, TypeName) is true, where AT is the type annotation of the element node, and
/// the nilled property of the node is false" — `bookType` and `ebookType` derive from
/// `productType` by extension; `m:special` has `productType` in another namespace.
#[test]
fn element_star_t_matches_by_type_and_its_derivations() {
    assert_eq!(
        typed("element(*, s:productType)", "//*"),
        "s:product s:book s:ebook s:gadget m:special"
    );
    assert_eq!(typed("element(*, s:bookType)", "//*"), "s:book s:ebook");
    assert_eq!(typed("element(*, s:ebookType)", "//*"), "s:ebook");
    // Built-in types: `money` restricts `xs:decimal`; `xs:token` derives from `xs:string`.
    assert_eq!(
        typed("element(*, xs:decimal)", "//*"),
        "s:price s:price s:price s:price"
    );
    assert_eq!(
        typed("element(*, xs:string)", "//*"),
        "s:isbn s:isbn s:format s:note"
    );
    assert_eq!(typed("element(*, xs:token)", "//*"), "s:format");
}

/// §2.5.4.3: "`element(ElementName, TypeName)` matches an element node whose name is
/// ElementName if derives-from(AT, TypeName ) is true" — and "Note that substitution groups do
/// not affect the semantics of ElementTest". "ElementName need not be present in the in-scope
/// element declarations": an undeclared name compiles and matches nothing here.
#[test]
fn element_n_t_matches_by_name_and_type() {
    assert_eq!(typed("element(s:book, s:productType)", "//*"), "s:book");
    assert_eq!(
        typed("element(s:product, s:productType)", "//*"),
        "s:product"
    );
    assert_eq!(typed("element(s:gadget, s:bookType)", "//*"), "");
    assert_eq!(typed("element(s:isbn, xs:string)", "//*"), "s:isbn s:isbn");
    assert_eq!(typed("element(s:unknown, s:productType)", "//*"), "");
}

/// §2.5.4.3: without `?`, "the nilled property of the node is false"; with `TypeName ?` "The
/// nilled property of the node may be either true or false".
#[test]
fn a_nilled_element_matches_only_with_the_question_mark() {
    assert_eq!(
        typed("element(s:price, s:money)", "//*"),
        "s:price s:price s:price s:price"
    );
    assert_eq!(
        typed("element(s:price, s:money?)", "//*"),
        "s:price s:price s:price s:price s:price"
    );
    assert_eq!(
        typed("element(*, s:money ?)", "//*"),
        "s:price s:price s:price s:price s:price"
    );
    // `xs:anyType` is a supertype of every type, but the nilled rule still applies.
    assert_eq!(typed("element(*, xs:anyType)", "//*"), NON_NILLED_ELEMENTS);
    assert_eq!(typed("element(*, xs:anyType?)", "//*"), ALL_ELEMENTS);
}

// ── §2.5.4.5: attribute(*, T), attribute(N, T) ───────────────────────────────────────────

/// §2.5.4.5: "`attribute(AttributeName, TypeName)` matches an attribute node whose name is
/// AttributeName if derives-from(AT, TypeName ) is true"; "`attribute(*, TypeName)` matches an
/// attribute node regardless of its name". An AttributeTest selects along the attribute axis
/// by default (XPath 2.0 §3.2.4), and `@` says so explicitly.
#[test]
fn attribute_tests_match_by_type() {
    assert_eq!(
        typed("attribute(*, s:skuType)", ATTRIBUTES),
        "sku sku sku sku sku"
    );
    assert_eq!(
        typed("attribute(sku, xs:string)", ATTRIBUTES),
        "sku sku sku sku sku"
    );
    assert_eq!(
        typed("@attribute(sku, s:skuType)", ATTRIBUTES),
        "sku sku sku sku sku"
    );
    assert_eq!(
        typed("attribute(s:currency, s:currencyType)", ATTRIBUTES),
        "s:currency s:currency"
    );
    assert_eq!(typed("attribute(*, xs:string)", ATTRIBUTES), ALL_ATTRIBUTES);
    assert_eq!(
        typed("attribute(*, xs:anySimpleType)", ATTRIBUTES),
        ALL_ATTRIBUTES
    );
    assert_eq!(typed("attribute(*, xs:decimal)", ATTRIBUTES), "");
    // An attribute is never on the child axis (§5.5.3: `child-or-top`).
    assert_eq!(typed("child::attribute(*, xs:string)", ATTRIBUTES), "");
}

// ── §2.5.4.4 / §2.5.4.6: schema-element(N), schema-attribute(N) ──────────────────────────

/// §2.5.4.4: "The name of the candidate node matches the specified ElementName or matches the
/// name of an element in a substitution group headed by an element named ElementName" — `ebook`
/// through `book`'s group, `m:special` in another namespace. The match goes through the
/// dispatch index, which keys rules by name: it must not miss them.
#[test]
fn schema_element_matches_the_substitution_group() {
    assert_eq!(
        typed("schema-element(s:product)", "//*"),
        "s:product s:book s:ebook s:gadget m:special"
    );
    assert_eq!(typed("schema-element(s:book)", "//*"), "s:book s:ebook");
    assert_eq!(typed("schema-element(s:ebook)", "//*"), "s:ebook");
    assert_eq!(typed("schema-element(m:special)", "//*"), "m:special");
    assert_eq!(typed("schema-element(s:shop)", "//*"), "s:shop");
    // Clause 3: "If the element declaration for ElementName … is not nillable, then the nilled
    // property of the candidate node is false" — `price` is nillable, so all five match.
    assert_eq!(
        typed("schema-element(s:price)", "//*"),
        "s:price s:price s:price s:price s:price"
    );
}

/// The same rules in the `#all` mode index and in a rule filed under several modes.
#[test]
fn schema_element_rules_are_found_in_every_mode_index() {
    let main = sheet(
        r##"<xsl:template match="/">
             <out><xsl:apply-templates select="/s:shop/*" mode="a"/>|<xsl:apply-templates select="/s:shop/*" mode="b"/></out>
           </xsl:template>
           <xsl:template match="schema-element(s:book)" mode="#all" priority="1">B</xsl:template>
           <xsl:template match="schema-element(s:product)" mode="a b">P</xsl:template>
           <xsl:template match="*" mode="#all">-</xsl:template>"##,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>PBBPP-|PBBPP-</out>"
    );
}

/// §2.5.4.6: "The name of the candidate node matches the specified AttributeName" and
/// derives-from(AT, ET) for the declared type.
#[test]
fn schema_attribute_matches_the_declared_attribute() {
    assert_eq!(
        typed("schema-attribute(s:currency)", ATTRIBUTES),
        "s:currency s:currency"
    );
    assert_eq!(
        typed("@schema-attribute(s:currency)", ATTRIBUTES),
        "s:currency s:currency"
    );
    assert_eq!(typed("child::schema-attribute(s:currency)", ATTRIBUTES), "");
}

// ── §2.5.4.2: document-node(E) ───────────────────────────────────────────────────────────

/// §2.5.4.2: "document-node(E) matches any document node that contains exactly one element
/// node, optionally accompanied by one or more comment and processing instruction nodes, if E is
/// an ElementTest or SchemaElementTest that matches the element node"; §5.5.3: with no explicit
/// axis the step's axis is `self`.
#[test]
fn document_node_tests_take_typed_element_tests() {
    assert_eq!(typed("document-node(schema-element(s:shop))", "/"), "#doc");
    assert_eq!(
        typed("document-node(element(s:shop, xs:anyType))", "/"),
        "#doc"
    );
    assert_eq!(typed("document-node(element(*, xs:untyped))", "/"), "");
    assert_eq!(typed("document-node(schema-element(s:product))", "/"), "");
    assert_eq!(
        untyped("document-node(element(s:shop, xs:untyped))", "/"),
        "#doc"
    );
    assert_eq!(untyped("document-node(schema-element(s:shop))", "/"), "");
}

// ── typed patterns on untyped trees ──────────────────────────────────────────────────────

/// XPath 2.0 §2.5.1: "xs:untyped is used as the type annotation of an element node that has not
/// been validated … No predefined schema types are derived from xs:untyped", and an unvalidated
/// attribute is `xs:untypedAtomic`. So on the unvalidated source a user type and a declaration
/// match nothing, `xs:untyped` / `xs:untypedAtomic` and their supertypes match everything — and
/// on the validated one `xs:untyped` / `xs:untypedAtomic` match nothing (one path, decided
/// by the node's annotation).
#[test]
fn typed_patterns_on_untyped_and_typed_trees() {
    assert_eq!(untyped("element(*, s:productType)", "//*"), "");
    assert_eq!(untyped("schema-element(s:product)", "//*"), "");
    assert_eq!(untyped("element(*, xs:string)", "//*"), "");
    assert_eq!(untyped("schema-attribute(s:currency)", ATTRIBUTES), "");
    assert_eq!(untyped("attribute(*, s:skuType)", ATTRIBUTES), "");
    assert_eq!(untyped("element(*, xs:untyped)", "//*"), ALL_ELEMENTS);
    assert_eq!(untyped("element(*, xs:anyType)", "//*"), ALL_ELEMENTS);
    assert_eq!(
        untyped("attribute(*, xs:untypedAtomic)", ATTRIBUTES),
        ALL_ATTRIBUTES
    );
    assert_eq!(
        untyped("attribute(*, xs:anyAtomicType)", ATTRIBUTES),
        ALL_ATTRIBUTES
    );

    assert_eq!(typed("element(*, xs:untyped)", "//*"), "");
    assert_eq!(typed("attribute(*, xs:untypedAtomic)", ATTRIBUTES), "");
}

/// §4.3: `input-type-annotations="strip"` makes a validated source untyped before any pattern
/// sees it.
#[test]
fn stripped_annotations_make_typed_patterns_see_untyped_nodes() {
    let main = sheet_with(
        r#"input-type-annotations="strip""#,
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//s:book"/></out></xsl:template>
           <xsl:template match="schema-element(s:product)">S</xsl:template>
           <xsl:template match="element(*, xs:untyped)">U</xsl:template>"#,
    );
    assert_eq!(out(run(&main, SourceValidation::Strict)), "<out>U</out>");
}

// ── Patterns with steps and predicates (§5.5.3) ──────────────────────────────────────────

/// §5.5.3: a pattern means `root(.)//(EE)`; typed tests in any step, predicates over typed values,
/// and positional predicates counting "the siblings selected by the step's node test".
#[test]
fn typed_tests_in_steps_and_with_predicates() {
    assert_eq!(
        typed(
            "schema-element(s:shop)/schema-element(s:product)[s:price gt 9]",
            "//*"
        ),
        "s:product s:book"
    );
    assert_eq!(
        typed(
            "element(s:shop, xs:anyType)//schema-element(s:price)",
            "//*"
        ),
        "s:price s:price s:price s:price s:price"
    );
    assert_eq!(typed("schema-element(s:book)[2]", "//*"), "s:ebook");
    assert_eq!(
        typed("element(*, s:productType)[last()]", "//*"),
        "m:special"
    );
    assert_eq!(
        typed("element(*, s:bookType)/s:isbn", "//*"),
        "s:isbn s:isbn"
    );
    assert_eq!(
        typed(
            "schema-element(s:product)/@attribute(*, s:skuType)",
            ATTRIBUTES
        ),
        "sku sku sku sku sku"
    );
    assert_eq!(
        typed("/schema-element(s:shop) | /element(*, xs:untyped)", "//*"),
        "s:shop"
    );
}

// ── §6.4: default priorities ─────────────────────────────────────────────────────────────

/// §6.4's table: `schema-element(E)` 0.25 ("matches by substitution group and type"),
/// `element(E,T)` 0.25, `element(*,T)` 0, a QName 0, `*` −0.5; a later rule of a lower priority
/// does not win. (Were `schema-element` 0, the later `s:book` would win for `s:book`.)
#[test]
fn default_priorities_of_typed_element_tests() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="/s:shop/*"/></out></xsl:template>
           <xsl:template match="schema-element(s:product)">S</xsl:template>
           <xsl:template match="s:book | s:gadget">N</xsl:template>
           <xsl:template match="*">A</xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>SSSSSA</out>"
    );

    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="/s:shop/*"/></out></xsl:template>
           <xsl:template match="element(*, s:bookType)">T</xsl:template>
           <xsl:template match="*">A</xsl:template>
           <xsl:template match="element(s:gadget, s:productType)">G</xsl:template>
           <xsl:template match="s:gadget">N</xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>ATTGAA</out>"
    );

    // "The presence or absence of the symbol ? following a type name does not affect the
    // priority": still 0.25, above the later `s:price` (0).
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//s:price"/></out></xsl:template>
           <xsl:template match="element(s:price, s:money?)">Q</xsl:template>
           <xsl:template match="s:price">N</xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>QQQQQ</out>"
    );
}

/// §6.4: `schema-attribute(A)` and `attribute(A,T)` 0.25, `attribute(*,T)` 0, `@A` 0, `@*` −0.5.
#[test]
fn default_priorities_of_typed_attribute_tests() {
    let main = sheet(&format!(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="{ATTRIBUTES}"/></out></xsl:template>
           <xsl:template match="schema-attribute(s:currency)">C</xsl:template>
           <xsl:template match="@s:currency">N</xsl:template>
           <xsl:template match="attribute(*, s:skuType)">K</xsl:template>
           <xsl:template match="@*">A</xsl:template>
           <xsl:template match="attribute(sku, s:skuType)">L</xsl:template>
           <xsl:template match="@sku">M</xsl:template>"#
    ));
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>LCLLLCL</out>"
    );
}

/// §6.4: "If the pattern has the form of a DocumentTest, then if it includes no ElementTest or
/// SchemaElementTest the priority is −0.5. If it does …, then the priority is the same as the
/// priority of that ElementTest or SchemaElementTest" — 0.25 and 0 beat `/` (−0.5) declared later.
#[test]
fn default_priorities_of_document_tests() {
    for (pattern, expected) in [
        ("document-node(schema-element(s:shop))", "D"),
        ("document-node(element(*, xs:anyType))", "D"),
        ("document-node(element(s:shop, xs:anyType))", "D"),
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="{pattern}"><out>D</out></xsl:template>
               <xsl:template match="/"><out>R</out></xsl:template>"#
        ));
        assert_eq!(
            out(run(&main, SourceValidation::Strict)),
            format!("<out>{expected}</out>"),
            "{pattern}"
        );
    }
}

// ── Every kind of pattern ────────────────────────────────────────────────────────────────

/// §16.3.1: `xsl:key match` is a pattern.
#[test]
fn typed_patterns_in_key_declarations() {
    let main = sheet(
        r#"<xsl:key name="p" match="schema-element(s:product)" use="@sku"/>
           <xsl:key name="t" match="attribute(*, s:skuType)" use="."/>
           <xsl:key name="d" match="element(*, s:money)" use="."/>
           <xsl:template match="/">
             <out><xsl:value-of select="key('p', ('AB1', 'EB3', 'SP5'))/name(), '|',
                                        key('t', 'GD4')/../name(), '|',
                                        count(key('d', 20.5)), count(key('d', 7))"/></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>s:product s:ebook m:special | s:gadget | 1 1</out>"
    );
}

/// §12.2: the `count` and `from` patterns of `xsl:number`. Each typed number is checked against
/// the same instruction with an equivalent name pattern.
#[test]
fn typed_patterns_in_xsl_number() {
    let main = sheet(
        r#"<xsl:template match="/">
             <out>
               <xsl:for-each select="/s:shop/*">
                 <xsl:number count="schema-element(s:product)"/>=<xsl:number
                   count="s:product | s:book | s:ebook | s:gadget | m:special"/>;</xsl:for-each>
               <xsl:text>|</xsl:text>
               <xsl:for-each select="//s:price">
                 <xsl:number level="any" count="element(*, s:money?)" from="schema-element(s:book)"/>=<xsl:number
                   level="any" count="s:price" from="s:book | s:ebook"/>;</xsl:for-each>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        // The first `s:price` precedes every `from` match; it is numbered as
        // `xsl:number` numbers such a node, from the root, whichever pattern is used.
        "<out>1=1;2=2;3=3;4=4;5=5;=;|1=1;1=1;1=1;2=2;3=3;</out>"
    );
}

/// §14.3: `group-starting-with` / `group-ending-with` are patterns.
#[test]
fn typed_patterns_in_grouping() {
    let main = sheet(
        r#"<xsl:template match="/">
             <out>
               <xsl:for-each-group select="/s:shop/*" group-starting-with="schema-element(s:book)">
                 <xsl:value-of select="count(current-group())"/>;</xsl:for-each-group>
               <xsl:text>|</xsl:text>
               <xsl:for-each-group select="/s:shop/*" group-ending-with="element(*, s:bookType)">
                 <xsl:value-of select="count(current-group())"/>;</xsl:for-each-group>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(run(&main, SourceValidation::Strict)),
        "<out>1;1;4;|2;1;3;</out>"
    );
}

// ── XPST0008 ─────────────────────────────────────────────────────────────────────────────

/// XPath 2.0 §2.5.4.3 / §2.5.4.5: "TypeName must be present in the in-scope schema types
/// [err:XPST0008]"; §2.5.4.4 / §2.5.4.6: an ElementName / AttributeName of a schema test "not
/// found in the in-scope element declarations" (attribute declarations) is XPST0008. XSLT §5.3:
/// the XPath error code is kept. In every kind of pattern.
#[test]
fn undeclared_names_in_typed_patterns_are_xpst0008() {
    for pattern in [
        "schema-element(s:nothing)",
        "schema-attribute(s:nothing)",
        "schema-attribute(sku)",
        "element(*, s:nothing)",
        "element(s:book, s:nothing?)",
        "attribute(s:currency, s:nothing)",
        "document-node(schema-element(s:nothing))",
        "s:shop/schema-element(s:nothing)[1]",
        "element(*, xs:noSuchType)",
    ] {
        let templates = sheet(&format!(
            r#"<xsl:template match="{pattern}"/><xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_eq!(
            err(run(&templates, SourceValidation::Strict)).code,
            "XPST0008",
            "template {pattern}"
        );
    }
    for top in [
        r#"<xsl:key name="k" match="schema-element(s:nothing)" use="."/>"#,
        r#"<xsl:template match="/"><out><xsl:number count="element(*, s:nothing)"/></out></xsl:template>"#,
        r#"<xsl:template match="/"><out><xsl:number from="schema-element(s:nothing)"/></out></xsl:template>"#,
        r#"<xsl:template match="/"><out><xsl:for-each-group select="*" group-starting-with="schema-attribute(s:nothing)"/></out></xsl:template>"#,
        r#"<xsl:template match="/"><out><xsl:for-each-group select="*" group-ending-with="element(*, xs:nothing)"/></out></xsl:template>"#,
    ] {
        let main = sheet(&format!(
            r#"{top}<xsl:template match="/" mode="unused"><out/></xsl:template>"#
        ));
        assert_eq!(
            err(run(&main, SourceValidation::Strict)).code,
            "XPST0008",
            "{top}"
        );
    }
}

// ── A basic processor (§21.1, §3.13) ─────────────────────────────────────────────────────

/// §3.13: a basic processor's in-scope schema types are the built-in ones and it has no element
/// or attribute declarations, so a user type or a schema test is XPST0008; a built-in type
/// compiles and decides on the untyped tree.
#[test]
fn a_basic_processor_compiles_built_in_typed_patterns() {
    let top = |pattern: &str| {
        format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="//* | //@*"/></out></xsl:template>
               <xsl:template match="{pattern}">+</xsl:template>
               <xsl:template match="node() | @*" priority="-9">-</xsl:template>"#
        )
    };
    let source = r#"<r a="1"><e>5</e></r>"#;
    for (pattern, expected) in [
        ("element(*, xs:integer)", "---"),
        ("attribute(*, xs:integer)", "---"),
        ("element(*, xs:untyped)", "+-+"),
        ("element(e, xs:anyType)", "--+"),
        ("attribute(*, xs:untypedAtomic)", "-+-"),
        ("attribute(a, xs:anySimpleType)", "-+-"),
    ] {
        assert_eq!(
            out(run_basic(&top(pattern), source)),
            format!("<out>{expected}</out>"),
            "{pattern}"
        );
    }
    for pattern in [
        "schema-element(s:product)",
        "schema-attribute(s:currency)",
        "element(*, s:productType)",
        "document-node(schema-element(s:shop))",
    ] {
        assert_eq!(
            err(run_basic(&top(pattern), source)).code,
            "XPST0008",
            "{pattern}"
        );
    }
}
