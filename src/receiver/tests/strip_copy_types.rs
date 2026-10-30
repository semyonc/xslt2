//! `xsl:copy-of` with `validation="strip"` makes the copy untyped, and with
//! `preserve` keeps the annotations, whichever receivers build it — here the
//! plain pipeline, a `ComplexContentNormalizer` over a `ResultBuilder` with no
//! validating receiver.
//!
//! Moved from the integration tests when the pipeline became crate-internal.

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::navigator::DomNavigator;
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSetBuilder;

use crate::receiver::{
    ComplexContentNormalizer, CopyNamespaces, CopyValidation, Name, Receiver, ResultBuilder,
};

const XSD_URI: &str = "file:///m4e/m4e.xsd";

/// `urn:m4e`: `doc` (the source's root: `row`s, then `qe`, `qa`, `nums`); `row` (an `xs:ID`
/// attribute `id`, an `xs:IDREF` attribute `ref`, the global attribute `size`, an optional `n`);
/// `n` (a nillable `xs:integer`); `qe` (an `xs:QName` element); `qa` (an `xs:QName` attribute `q`,
/// an integer-or-QName union attribute `iq`, an attribute `code` of a restriction of `xs:ID`); `nums` (a list of integers); `table` (`row`s
/// with an `xs:unique` on `@e:size`); the global attributes `size` (an integer range) and `qn`
/// (an `xs:QName`); `w` (a sequence of `n`) and `ww` (a sequence of `w`).
const M4E_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns="urn:m4e"
    xmlns:e="urn:m4e" targetNamespace="urn:m4e" elementFormDefault="qualified">
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

/// Valid against `M4E_XSD`: two rows (the second with a nilled `n` and an IDREF to the first), a
/// QName element, a QName attribute, a list.
const DOC: &str = r#"<e:doc xmlns:e="urn:m4e" xmlns:p="urn:p" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><e:row id="r1" e:size="3"><e:n>5</e:n></e:row><e:row id="r2" ref="r1"><e:n xsi:nil="true"/></e:row><e:qe>p:x</e:qe><e:qa q="p:y" iq="5" code="c1"/><e:nums>1 2  3</e:nums></e:doc>"#;

/// §19.2.1.1, `strip`: "the new node and each of the contained nodes will have the type annotation
/// xs:untyped if it is an element, or xs:untypedAtomic if it is an attribute … The values of the is-id
/// and is-idrefs properties are unchanged" — whichever receiver chain builds the copy: the plain
/// pipeline (`ComplexContentNormalizer` over a `ResultBuilder`, no validating receiver) too. A
/// `preserve` copy keeps the annotations there as well.
#[test]
fn a_strip_copy_is_untyped_through_the_public_pipeline() {
    let set = SchemaSetBuilder::new()
        .add_source(M4E_XSD, XSD_URI)
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let arena = Bump::new();
    let source = build_typed_document(
        DOC.as_bytes(),
        &arena,
        &set,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    assert!(source.has_type_annotations());
    let mut row1 = source.create_navigator();
    assert!(row1.move_to_first_child() && row1.move_to_first_child());
    let mut row2 = row1.clone();
    assert!(row2.move_to_next_sibling());
    let mut id2 = row2.clone();
    assert!(id2.move_to_first_attribute() && id2.local_name() == "id");
    let mut root = source.create_navigator();
    root.move_to_root();
    for (validation, typed) in [
        (CopyValidation::Strip, false),
        (CopyValidation::Preserve, true),
    ] {
        // An attribute copied on its own, an element, a document node.
        let mut out = ComplexContentNormalizer::new(
            ResultBuilder::new(&arena, &set.name_table, Some(&set)).unwrap(),
        );
        out.start_document().unwrap();
        out.start_element(&Name::local("x"), None, false).unwrap();
        out.append_copy(&XmlItem::Node(id2.clone()), CopyNamespaces::All, validation)
            .unwrap();
        out.end_start_tag().unwrap();
        out.append_copy(
            &XmlItem::Node(row1.clone()),
            CopyNamespaces::All,
            validation,
        )
        .unwrap();
        out.end_element().unwrap();
        out.end_document().unwrap();
        let doc = out.into_inner().finish().unwrap();
        assert_eq!(doc.has_type_annotations(), typed, "{validation:?}");
        assert!(doc.get_element_by_id("r1").is_some(), "{validation:?}: r1");
        assert!(doc.get_element_by_id("r2").is_some(), "{validation:?}: r2");

        let mut out = ComplexContentNormalizer::new(
            ResultBuilder::new(&arena, &set.name_table, Some(&set)).unwrap(),
        );
        out.start_document().unwrap();
        out.append_copy(
            &XmlItem::Node(root.clone()),
            CopyNamespaces::All,
            validation,
        )
        .unwrap();
        out.end_document().unwrap();
        let doc = out.into_inner().finish().unwrap();
        assert_eq!(
            doc.has_type_annotations(),
            typed,
            "{validation:?}: document"
        );
        assert!(
            doc.get_element_by_id("r2").is_some(),
            "{validation:?}: document r2"
        );
        assert!(
            doc.get_element_by_id("c1").is_some(),
            "{validation:?}: document c1"
        );
    }
}
