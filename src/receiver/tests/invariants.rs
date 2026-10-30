//! The architecture invariants of the push pipeline: direct serialization
//! equals serialization of the materialized tree, a typed copy keeps or strips
//! the PSVI state, the §5.7.1 rules of constructing complex content, and how
//! constructed nodes and sequences come out.
//!
//! Moved from the integration tests when the pipeline became crate-internal.
//! Every test asserts the negative as well as the positive.

use std::cell::RefCell;

use bumpalo::Bump;
use pretty_assertions::assert_eq;
use xsd_schema::document::{
    serialize, BufferDocument, BufferDocumentOptions, DocumentKind, NodeSchemaBinding,
};
use xsd_schema::ids::TypeKey;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType, TypedValue};
use xsd_schema::pipeline::load_and_process_schema;
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSet;

use crate::error::codes;
use crate::receiver::{
    serialize_document, Annotations, ComplexContentNormalizer, EventSerializer, Name, Nav,
    Receiver, ResultBuilder, SequenceCollector,
};
use crate::store::DocumentStore;

const SOURCE: &str = concat!(
    r#"<catalog><item id="a" enabled="no"><t>A</t></item>"#,
    r#"<item id="b" enabled="yes"><t>B</t></item>"#,
    r#"<item id="c" enabled="yes"><t>C</t></item></catalog>"#,
);

/// A store over a caller-owned arena and name table.
fn store<'t>(arena: &'t Bump, names: &'t NameTable) -> RefCell<DocumentStore<'t>> {
    RefCell::new(DocumentStore::new(arena, names))
}

/// The `n`-th `item` element of the principal source.
fn item<'t>(nav: &Nav<'t>, index: usize) -> Nav<'t> {
    let mut cursor = nav.clone();
    assert!(cursor.move_to_first_child(), "document element");
    assert!(cursor.move_to_first_child(), "first item");
    let mut seen = 0;
    loop {
        if cursor.node_type() == DomNodeType::Element && cursor.local_name() == "item" {
            if seen == index {
                return cursor;
            }
            seen += 1;
        }
        assert!(cursor.move_to_next_sibling(), "no item at index {index}");
    }
}

// ── Invariant 2 ──────────────────────────────────────────────────────────

/// One scripted event stream, replayed into two destinations.
fn play(pipe: &mut dyn Ops) -> Result<(), crate::XsltError> {
    pipe.doc_start()?;
    pipe.el_start(&Name::qualified("p", "doc", "urn:x"))?;
    pipe.ns("p", "urn:x")?;
    pipe.attr(&Name::local("k"), "v<&\"")?;
    // Last of two attributes with the same name wins.
    pipe.attr(&Name::local("dup"), "first")?;
    pipe.attr(&Name::local("dup"), "last")?;
    pipe.chars("a")?;
    pipe.chars("")?;
    pipe.chars("b")?;
    pipe.el_start(&Name::local("empty"))?;
    pipe.el_end()?;
    pipe.comment("c")?;
    pipe.pi("work", "now")?;
    pipe.atomic("1")?;
    pipe.atomic("2")?;
    pipe.chars("x > y")?;
    pipe.el_end()?;
    pipe.doc_end()
}

/// Direct serialization of a transformation equals serialization of its
/// materialized result tree, byte for byte, and both raise the same error codes.
#[test]
fn inv2_direct_serialization_equals_materialized_serialization() {
    let arena = Bump::new();
    let names = NameTable::new();

    let mut direct = ComplexContentNormalizer::new(EventSerializer::new());
    play(&mut direct).expect("direct");
    let direct = direct.into_inner().into_string();

    let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
    let mut tree = ComplexContentNormalizer::new(builder);
    play(&mut tree).expect("tree");
    let doc = tree.into_inner().finish().expect("finish");
    let materialized = serialize_document(&doc).expect("serialize");

    assert_eq!(
        direct, materialized,
        "the event serializer and document::serialize must agree byte for byte"
    );
    // Non-vacuous: the output really is the document that was scripted.
    assert_eq!(
        direct,
        r#"<p:doc xmlns:p="urn:x" k="v&lt;&amp;&quot;" dup="last">ab<empty/><!--c--><?work now?>1 2x &gt; y</p:doc>"#,
    );

    // The riskiest path for invariant 2 is the deep copy of an *existing*
    // node, where the normalizer's own walk has to agree with upstream's
    // namespace bookkeeping. Copy a subtree whose prefix is declared on an
    // ancestor, into both destinations.
    let names2 = NameTable::new();
    let docs = store(&arena, &names2);
    let id = docs
        .borrow_mut()
        .load_str(
            "ns.xml",
            r#"<r xmlns:p="urn:x" xmlns="urn:d"><p:kid k="1">t<!--c--><in/></p:kid></r>"#,
        )
        .expect("load");
    let mut kid = docs.borrow().navigator(id);
    assert!(kid.move_to_first_child());
    assert!(kid.move_to_first_child());
    assert_eq!(kid.local_name(), "kid");

    let mut direct = ComplexContentNormalizer::new(EventSerializer::new());
    copy_into(&mut direct, &kid);
    let direct = direct.into_inner().into_string();
    let builder = ResultBuilder::new(&arena, &names2, None).expect("builder");
    let mut tree = ComplexContentNormalizer::new(builder);
    copy_into(&mut tree, &kid);
    let doc = tree.into_inner().finish().expect("finish");
    assert_eq!(
        direct,
        serialize_document(&doc).expect("serialize"),
        "a deep copy must serialize the same way through both destinations"
    );
    assert!(
        direct.contains(r#"xmlns:p="urn:x""#),
        "the copy carries the declaration it needs: {direct}"
    );

    // The same three errors, from both destinations.
    for (name, script) in error_scripts() {
        let mut direct = ComplexContentNormalizer::new(EventSerializer::new());
        let direct_err = script(&mut direct).expect_err(name);
        let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
        let mut tree = ComplexContentNormalizer::new(builder);
        let tree_err = script(&mut tree).expect_err(name);
        assert_eq!(direct_err.code, tree_err.code, "{name}: same code");
    }
}

/// Copies one existing node through a normalizer, as `xsl:copy-of` does.
fn copy_into<'t, R: Receiver<'t>>(pipe: &mut ComplexContentNormalizer<'t, R>, node: &Nav<'t>) {
    pipe.start_document().unwrap();
    pipe.append_item(&XmlItem::Node(node.clone())).unwrap();
    pipe.end_document().unwrap();
}

type Script = fn(&mut dyn Ops) -> Result<(), crate::XsltError>;

/// The part of [`Receiver`] the scripted tests drive, as an object-safe view.
///
/// The names differ from `Receiver`'s deliberately: a blanket impl for every
/// normalizer would otherwise make `pipe.start_element(..)` ambiguous.
trait Ops {
    fn doc_start(&mut self) -> Result<(), crate::XsltError>;
    fn doc_end(&mut self) -> Result<(), crate::XsltError>;
    fn el_start(&mut self, name: &Name) -> Result<(), crate::XsltError>;
    fn el_end(&mut self) -> Result<(), crate::XsltError>;
    fn attr(&mut self, name: &Name, value: &str) -> Result<(), crate::XsltError>;
    fn ns(&mut self, prefix: &str, uri: &str) -> Result<(), crate::XsltError>;
    fn chars(&mut self, s: &str) -> Result<(), crate::XsltError>;
    fn atomic(&mut self, s: &str) -> Result<(), crate::XsltError>;
    fn comment(&mut self, s: &str) -> Result<(), crate::XsltError>;
    fn pi(&mut self, target: &str, data: &str) -> Result<(), crate::XsltError>;
}

impl<'t, R: Receiver<'t>> Ops for ComplexContentNormalizer<'t, R> {
    fn doc_start(&mut self) -> Result<(), crate::XsltError> {
        Receiver::start_document(self)
    }
    fn doc_end(&mut self) -> Result<(), crate::XsltError> {
        Receiver::end_document(self)
    }
    fn el_start(&mut self, name: &Name) -> Result<(), crate::XsltError> {
        Receiver::start_element(self, name, None, false)
    }
    fn el_end(&mut self) -> Result<(), crate::XsltError> {
        Receiver::end_element(self)
    }
    fn attr(&mut self, name: &Name, value: &str) -> Result<(), crate::XsltError> {
        Receiver::attribute(self, name, value, None)
    }
    fn ns(&mut self, prefix: &str, uri: &str) -> Result<(), crate::XsltError> {
        Receiver::namespace(self, prefix, uri)
    }
    fn chars(&mut self, s: &str) -> Result<(), crate::XsltError> {
        Receiver::text(self, s, false)
    }
    fn atomic(&mut self, s: &str) -> Result<(), crate::XsltError> {
        Receiver::append_item(
            self,
            &XmlItem::Atomic(xsd_schema::types::XmlValue::string(s.to_string())),
        )
    }
    fn comment(&mut self, s: &str) -> Result<(), crate::XsltError> {
        Receiver::comment(self, s)
    }
    fn pi(&mut self, target: &str, data: &str) -> Result<(), crate::XsltError> {
        Receiver::pi(self, target, data)
    }
}

fn error_scripts() -> Vec<(&'static str, Script)> {
    vec![
        ("XTDE0410 attribute after a child", |t| {
            t.doc_start()?;
            t.el_start(&Name::local("a"))?;
            t.chars("x")?;
            t.attr(&Name::local("k"), "v")
        }),
        ("XTDE0420 attribute under a document node", |t| {
            t.doc_start()?;
            t.attr(&Name::local("k"), "v")
        }),
        ("XTDE0430 conflicting namespace nodes", |t| {
            t.doc_start()?;
            t.el_start(&Name::local("a"))?;
            t.ns("p", "urn:x")?;
            t.ns("p", "urn:y")
        }),
    ]
}

// ── Invariant 5 ──────────────────────────────────────────────────────────

const NILLABLE_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="root">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="value" type="xs:integer" nillable="true"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

const NILLED_XML: &str =
    r#"<root><value xsi:nil="true" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"/></root>"#;

fn nillable_schema() -> SchemaSet {
    let mut schema_set = SchemaSet::xsd11();
    load_and_process_schema(NILLABLE_XSD.as_bytes(), "test.xsd", &mut schema_set, None)
        .expect("schema");
    schema_set
}

/// Typed copy preserves the type annotation, the declaration binding and the
/// nilled state; `strip` replaces them.
#[test]
fn inv5_typed_copy_preserves_or_strips_the_psvi_state() {
    let schema_set = nillable_schema();
    let arena = Bump::new();
    let source = xsd_schema::document::build_typed_document(
        NILLED_XML.as_bytes(),
        &arena,
        &schema_set,
        BufferDocumentOptions::default(),
    )
    .expect("typed document");

    let mut value = source.create_navigator();
    assert!(value.move_to_first_child());
    assert!(value.move_to_first_child());
    assert_eq!(value.local_name(), "value");
    let source_binding: NodeSchemaBinding =
        *value.schema_binding().expect("the source is annotated");
    assert!(
        matches!(
            source_binding.type_key,
            TypeKey::Simple(_) | TypeKey::Complex(_)
        ),
        "the source carries a real TypeKey"
    );
    assert!(
        matches!(value.typed_value(), TypedValue::Nilled),
        "the source element is nilled"
    );

    let preserved = copy_through(&arena, &schema_set, &value, Annotations::Preserve);
    let stripped = copy_through(&arena, &schema_set, &value, Annotations::Strip);

    let mut kept = preserved.create_navigator();
    assert!(kept.move_to_first_child());
    assert_eq!(
        kept.type_annotation(),
        Some(source_binding.type_key),
        "preserve keeps the type annotation"
    );
    assert_eq!(
        kept.schema_binding().map(|b| b.element_decl),
        Some(source_binding.element_decl),
        "preserve keeps the declaration binding"
    );
    assert!(
        matches!(kept.typed_value(), TypedValue::Nilled),
        "preserve keeps the nilled state"
    );

    let mut lost = stripped.create_navigator();
    assert!(lost.move_to_first_child());
    assert_eq!(
        lost.type_annotation(),
        None,
        "strip replaces the type annotation"
    );
    assert!(
        lost.schema_binding().is_none(),
        "strip drops the declaration binding"
    );
    assert!(
        !matches!(lost.typed_value(), TypedValue::Nilled),
        "strip drops the nilled state"
    );
}

fn copy_through<'a>(
    arena: &'a Bump,
    schema_set: &'a SchemaSet,
    node: &Nav<'a>,
    annotations: Annotations,
) -> BufferDocument<'a> {
    let builder = ResultBuilder::with_options(
        arena,
        &schema_set.name_table,
        Some(schema_set),
        BufferDocumentOptions::default(),
    )
    .expect("builder");
    let mut pipe = ComplexContentNormalizer::with_annotations(builder, annotations);
    pipe.start_document().unwrap();
    pipe.append_item(&XmlItem::Node(node.clone())).unwrap();
    pipe.end_document().unwrap();
    pipe.into_inner().finish().expect("finish")
}

/// A `DocumentKind::Full` document accepts several element children.
#[test]
fn q7_full_document_accepts_several_element_children() {
    let arena = Bump::new();
    let names = NameTable::new();
    let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
    let mut pipe = ComplexContentNormalizer::new(builder);
    pipe.start_document().unwrap();
    for name in ["one", "two", "three"] {
        pipe.start_element(&Name::local(name), None, false).unwrap();
        pipe.end_element().unwrap();
    }
    pipe.end_document().unwrap();
    let doc = pipe.into_inner().finish().expect("finish");

    assert_eq!(doc.kind(), DocumentKind::Full);
    let mut children = 0;
    let mut nav = doc.create_navigator();
    if nav.move_to_first_child() {
        loop {
            if nav.node_type() == DomNodeType::Element {
                children += 1;
            }
            if !nav.move_to_next_sibling() {
                break;
            }
        }
    }
    assert_eq!(
        children, 3,
        "three document elements under one Full document"
    );
    assert_eq!(
        serialize::to_string(&doc.create_navigator(), &Default::default()).expect("serialize"),
        "<one/><two/><three/>",
    );
}

/// A constructed node that lands in a sequence is parentless.
///
/// `DocumentKind::Fragment` hides the synthetic root from `/` and `//`, and
/// the upstream `BufferDocNavigator::new_orphan` hides it from
/// `parent::node()` and `fn:root()` too; this test pins that a constructed
/// node is parentless (XDM: a node built by a sequence constructor evaluated
/// with `as` has no parent).
#[test]
fn q6_a_constructed_node_in_a_sequence_is_parentless() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let mut collector = SequenceCollector::new(&docs);
    collector
        .start_element(&Name::local("made"), None, false)
        .unwrap();
    collector.text("x", false).unwrap();
    collector.end_element().unwrap();
    let items = collector.into_items();
    assert_eq!(items.len(), 1);
    let node = items[0].as_node().expect("an element came out").clone();
    assert_eq!(node.local_name(), "made");
    assert_eq!(node.document().kind(), DocumentKind::Fragment);

    // `/` and `//` see the element, not the hidden document node.
    let mut visible = node.clone();
    visible.move_to_visible_root();
    assert!(
        visible.is_same_position(&node),
        "move_to_visible_root lands on the constructed element"
    );

    // `parent::node()` is empty and `fn:root()` is the element itself.
    let mut parent = node.clone();
    assert!(!parent.move_to_parent(), "a constructed node has no parent");
    let mut root = node.clone();
    root.move_to_root();
    assert!(
        root.is_same_position(&node),
        "fn:root() returns the element, not the document that holds it"
    );
    assert_ne!(root.node_type(), DomNodeType::Root);

    // Its own content is unaffected.
    let mut child = node.clone();
    assert!(child.move_to_first_child());
    assert_eq!(child.value(), "x");
    assert!(child.move_to_parent());
    assert!(child.is_same_position(&node));
}

// ── §5.7.1, rule by rule ─────────────────────────────────────────────────

/// The §5.7.1 table, driven from one list of cases.
#[test]
fn normalizer_rules_5_7_1() {
    type Case = (&'static str, Script, Result<&'static str, &'static str>);

    let cases: Vec<Case> = vec![
        (
            "two atomic values are separated by a single space",
            |t| {
                t.atomic("1")?;
                t.atomic("2")
            },
            Ok("<a>1 2</a>"),
        ),
        (
            "an atomic value next to a text node gets no separator",
            |t| {
                t.chars("x")?;
                t.atomic("1")?;
                t.chars("y")
            },
            Ok("<a>x1y</a>"),
        ),
        (
            "zero-length text is dropped and adjacent text merged",
            |t| {
                t.chars("p")?;
                t.chars("")?;
                t.chars("q")
            },
            Ok("<a>pq</a>"),
        ),
        (
            "an attribute after a child is XTDE0410",
            |t| {
                t.chars("x")?;
                t.attr(&Name::local("k"), "v")
            },
            Err(codes::XTDE0410),
        ),
        (
            "a namespace after a child is XTDE0410",
            |t| {
                t.chars("x")?;
                t.ns("p", "urn:x")
            },
            Err(codes::XTDE0410),
        ),
        (
            "two attributes with one name: the last wins",
            |t| {
                t.attr(&Name::local("k"), "first")?;
                t.attr(&Name::local("k"), "last")
            },
            Ok(r#"<a k="last"/>"#),
        ),
        (
            "conflicting namespace nodes are XTDE0430",
            |t| {
                t.ns("p", "urn:x")?;
                t.ns("p", "urn:y")
            },
            Err(codes::XTDE0430),
        ),
        (
            "identical namespace nodes collapse",
            |t| {
                t.ns("p", "urn:x")?;
                t.ns("p", "urn:x")
            },
            Ok(r#"<a xmlns:p="urn:x"/>"#),
        ),
        (
            "an element in an unbound namespace gets a declaration (fixup)",
            |t| {
                t.el_start(&Name::qualified("q", "n", "urn:z"))?;
                t.el_end()
            },
            Ok(r#"<a><q:n xmlns:q="urn:z"/></a>"#),
        ),
        (
            "a default namespace on an element in no namespace is XTDE0440",
            |t| t.ns("", "urn:z"),
            Err(codes::XTDE0440),
        ),
        (
            "a default namespace on an element in that namespace is fine",
            |t| {
                t.el_start(&Name::qualified("", "n", "urn:z"))?;
                t.ns("", "urn:z")?;
                t.el_end()
            },
            Ok(r#"<a><n xmlns="urn:z"/></a>"#),
        ),
        (
            "an element that inherits its parent's default namespace keeps it (§5.7.3 fixup)",
            |t| {
                t.el_start(&Name::qualified("", "n", "urn:z"))?;
                t.el_start(&Name::qualified("", "m", "urn:z"))?;
                t.el_end()?;
                t.el_end()
            },
            Ok(r#"<a><n xmlns="urn:z"><m/></n></a>"#),
        ),
    ];

    for (name, script, expected) in cases {
        let mut pipe = ComplexContentNormalizer::new(EventSerializer::new());
        let outcome = (|| -> Result<(), crate::XsltError> {
            pipe.doc_start()?;
            pipe.el_start(&Name::local("a"))?;
            script(&mut pipe)?;
            pipe.el_end()?;
            pipe.doc_end()
        })();
        match (outcome, expected) {
            (Ok(()), Ok(want)) => {
                assert_eq!(pipe.into_inner().into_string(), want, "{name}")
            }
            (Err(err), Err(code)) => assert_eq!(err.code, code, "{name}"),
            (Ok(()), Err(code)) => panic!(
                "{name}: expected {code}, produced {}",
                pipe.into_inner().into_string()
            ),
            (Err(err), Ok(want)) => panic!("{name}: expected {want}, got {err}"),
        }
    }
}

/// §5.7.1: "Any document node within the result sequence is replaced by a
/// sequence containing each of its children, in document order."
#[test]
fn document_node_in_content_contributes_its_children() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let id = docs
        .borrow_mut()
        .load_str("two.xml", "<!--lead--><a>1</a>")
        .expect("load");
    let doc_node = docs.borrow().navigator(id);
    assert_eq!(doc_node.node_type(), DomNodeType::Root);

    let mut pipe = ComplexContentNormalizer::new(EventSerializer::new());
    pipe.start_document().unwrap();
    pipe.start_element(&Name::local("wrap"), None, false)
        .unwrap();
    pipe.append_item(&XmlItem::Node(doc_node.clone())).unwrap();
    pipe.end_element().unwrap();
    pipe.end_document().unwrap();

    // The document node itself leaves no trace; its children do.
    assert_eq!(
        pipe.into_inner().into_string(),
        "<wrap><!--lead--><a>1</a></wrap>"
    );
}

/// `SequenceCollector` keeps an existing node by reference.
#[test]
fn sequence_collector_keeps_identity_and_builds_parentless_nodes() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let id = docs
        .borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect("load");
    let root = docs.borrow().navigator(id);
    let existing = item(&root, 0);

    let mut collector = SequenceCollector::new(&docs);
    collector
        .append_item(&XmlItem::Node(existing.clone()))
        .unwrap();
    collector
        .start_element(&Name::local("made"), None, false)
        .unwrap();
    collector.end_element().unwrap();
    let items = collector.into_items();

    assert_eq!(items.len(), 2);
    let kept = items[0].as_node().expect("node");
    assert!(
        kept.is_same_position(&existing),
        "an appended node keeps its identity"
    );
    let mut parent = kept.clone();
    assert!(parent.move_to_parent());
    assert_eq!(parent.local_name(), "catalog", "and keeps its parent");

    let made = items[1].as_node().expect("node");
    assert_eq!(made.local_name(), "made");
    assert!(
        !made.is_same_position(&existing),
        "the constructed node is a different node"
    );
}

/// What a deep copy of a namespaced subtree actually looks like — the exact
/// text, so a change in upstream's declaration policy shows up here.
///
/// `copy-namespaces="yes"` is the default the normalizer uses, as §11.9.2
/// requires, so the copy carries every in-scope namespace of the source
/// element, not only the declarations written on it.
#[test]
fn deep_copy_of_a_namespaced_subtree_is_self_contained() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let id = docs
        .borrow_mut()
        .load_str(
            "ns.xml",
            r#"<r xmlns:p="urn:x" xmlns="urn:d"><p:kid k="1">t<!--c--><in/></p:kid></r>"#,
        )
        .expect("load");
    let mut kid = docs.borrow().navigator(id);
    assert!(kid.move_to_first_child());
    assert!(kid.move_to_first_child());

    let mut pipe = ComplexContentNormalizer::new(EventSerializer::new());
    copy_into(&mut pipe, &kid);
    assert_eq!(
        pipe.into_inner().into_string(),
        r#"<p:kid xmlns="urn:d" xmlns:p="urn:x" k="1">t<!--c--><in/></p:kid>"#,
        "copy-namespaces=\"yes\": every in-scope namespace travels with the copy"
    );
}
