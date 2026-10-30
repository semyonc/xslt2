//! `xsl:copy-of` with `validation="strip"` keeps the element ids and the
//! is-idrefs property (§19.2.1.1: "The values of the is-id and is-idrefs
//! properties are unchanged") whichever receivers build the copy — here the
//! plain pipeline, a `ComplexContentNormalizer` over a bare `ResultBuilder`.
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

const XSD_URI: &str = "file:///tf/t.xsd";

/// A no-namespace schema. `doc`: `row`s (an `xs:ID` attribute `id`, an `xs:IDREF` `ref`, an
/// `xs:IDREFS` `refs`), then elements that are IDs by their own typed value — `eid` (`xs:ID`), `cid`
/// (simple content extending `xs:ID`, with an `xs:string` attribute), `rid` (a restriction of
/// `xs:ID`) —, the IDREF elements `eref` (`xs:IDREF`) and `erefs` (`xs:IDREFS`), and `s`, an
/// `xs:string` (no ID). `r`: elements with defaults (`d` `dflt`, `ws` a single space) and an
/// attribute default. `wrap`: no declaration (for lax validation).
const XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="doc">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="row" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="ref" type="xs:IDREF"/>
            <xs:attribute name="refs" type="xs:IDREFS"/>
          </xs:complexType>
        </xs:element>
        <xs:element ref="eid" minOccurs="0" maxOccurs="unbounded"/>
        <xs:element name="cid" minOccurs="0">
          <xs:complexType><xs:simpleContent>
            <xs:extension base="xs:ID"><xs:attribute name="note" type="xs:string"/></xs:extension>
          </xs:simpleContent></xs:complexType>
        </xs:element>
        <xs:element name="rid" type="rid" minOccurs="0"/>
        <xs:element name="eref" type="xs:IDREF" minOccurs="0"/>
        <xs:element name="erefs" type="xs:IDREFS" minOccurs="0"/>
        <xs:element name="s" type="xs:string" minOccurs="0"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:element name="eid" type="xs:ID"/>
  <xs:simpleType name="rid">
    <xs:restriction base="xs:ID"><xs:pattern value="k.*"/></xs:restriction>
  </xs:simpleType>
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="d" type="xs:string" default="dflt" minOccurs="0"/>
        <xs:element name="ws" type="xs:string" default=" " minOccurs="0"/>
      </xs:sequence>
      <xs:attribute name="dflt" type="xs:string" default="adflt"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

/// Valid against `XSD`: the IDREFs point at attribute ids (`r1`, `r2`) and element ids (`e1`,
/// `e2`, `k1`).
const DOC: &str = r#"<doc><row id="r1" refs="e1 k1"/><row id="r2" ref="r1"/><eid>e1</eid><eid>e2</eid><cid note="n">c1</cid><rid>k1</rid><eref>r2</eref><erefs>e2 r1</erefs><s>s1</s></doc>"#;

/// The same rule whichever receivers build the copy (a later fix): the plain pipeline, a
/// `ComplexContentNormalizer` over a bare `ResultBuilder`, keeps the element ids and the is-idrefs
/// property of a `strip` copy — and a `preserve` copy has them through its annotations.
#[test]
fn a_strip_copy_keeps_them_through_the_public_pipeline() {
    let set = SchemaSetBuilder::new()
        .add_source(XSD, XSD_URI)
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
    let mut root = source.create_navigator();
    root.move_to_root();
    for (validation, typed) in [
        (CopyValidation::Strip, false),
        (CopyValidation::Preserve, true),
    ] {
        let mut out = ComplexContentNormalizer::new(
            ResultBuilder::new(&arena, &set.name_table, Some(&set)).unwrap(),
        );
        out.start_document().unwrap();
        out.start_element(&Name::local("x"), None, false).unwrap();
        out.end_start_tag().unwrap();
        out.append_copy(
            &XmlItem::Node(root.clone()),
            CopyNamespaces::All,
            validation,
        )
        .unwrap();
        out.end_element().unwrap();
        out.end_document().unwrap();
        let doc = out.into_inner().finish().unwrap();
        assert_eq!(doc.has_type_annotations(), typed, "{validation:?}");
        for id in ["r1", "e1", "e2", "c1", "k1"] {
            assert!(
                doc.get_element_by_id(id).is_some(),
                "{validation:?}: id {id}"
            );
        }
        assert!(doc.get_element_by_id("s1").is_none(), "{validation:?}: s1");
        let nav = doc.create_navigator();
        let names = |id: &str| -> Vec<String> {
            nav.find_nodes_by_idref(id)
                .unwrap()
                .iter()
                .map(|n| n.local_name().to_string())
                .collect()
        };
        assert_eq!(names("r1"), ["ref", "erefs"], "{validation:?}: r1");
        assert_eq!(names("e1"), ["refs"], "{validation:?}: e1");
        assert_eq!(names("r2"), ["eref"], "{validation:?}: r2");
    }
}
