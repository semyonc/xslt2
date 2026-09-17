//! The six architecture invariants of the design sketch §2.6, plus the open
//! questions M0b was asked to close (Q1, Q6, Q7, Q35).
//!
//! Every test asserts the negative as well as the positive: a proof that a copy
//! keeps a node's identity is worthless unless the test also shows that a real
//! copy does *not*.

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
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::{XPathExpr, XmlItem};
use xsd_schema::SchemaSet;

use xslt2::error::codes;
use xslt2::proto::{
    pattern_step_matches, recursive_collect, CompiledStylesheet, Frame, Transformation,
};
use xslt2::receiver::{
    serialize_document, Annotations, ComplexContentNormalizer, EventSerializer, Name, Nav, Receiver,
    ResultBuilder, SequenceCollector,
};
use xslt2::store::{DocumentStore, ProtoFunctions};

const SOURCE: &str = concat!(
    r#"<catalog><item id="a" enabled="no"><t>A</t></item>"#,
    r#"<item id="b" enabled="yes"><t>B</t></item>"#,
    r#"<item id="c" enabled="yes"><t>C</t></item></catalog>"#,
);

const SECOND: &str = r#"<extra><note>second</note></extra>"#;

/// A store over a caller-owned arena and name table (§6.1, §6.6).
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

// ── Invariant 1 ──────────────────────────────────────────────────────────

/// An existing node passed through a typed variable, a stylesheet-function
/// argument and result, and a template parameter keeps its identity (`is`), its
/// parent, and its `generate-id()`.
#[test]
fn inv1_node_identity_survives_variables_parameters_and_functions() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);

    let id = docs
        .borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect("load");
    let root = docs.borrow().navigator(id);
    let original = item(&root, 1);
    let original_id = docs.borrow().generate_id(&original);

    // (1) through a typed variable: a frame slot holds the item itself.
    let mut frame = Frame::with_slots(1);
    frame.set(0, XPathValue::from_node(original.clone()));
    let from_variable = match frame.get(0) {
        Some(XPathValue::Item(XmlItem::Node(n))) => n.clone(),
        _ => panic!("expected a node in the slot"),
    };

    // (2) through a stylesheet-function argument and result, and (3) through a
    // template parameter — both are a frame handed to a call in this prototype.
    let mut run = Transformation::new(&docs);
    let collected = recursive_collect(
        &mut run,
        Frame {
            slots: vec![XPathValue::from_node(from_variable.clone())],
        },
        1,
        "extra.xml",
    );
    // No source registered for "extra.xml": the error is the load, and the
    // identity claims below are unaffected.
    assert!(collected.is_err(), "the second document is not registered yet");

    docs.borrow_mut().register_source("extra.xml", SECOND);
    let mut run = Transformation::new(&docs);
    let items = recursive_collect(
        &mut run,
        Frame {
            slots: vec![XPathValue::from_node(from_variable.clone())],
        },
        1,
        "extra.xml",
    )
    .expect("recursion");
    let returned = items
        .iter()
        .filter_map(|i| i.as_node())
        .find(|n| n.local_name() == "item")
        .expect("the item came back out of the function");

    assert!(
        returned.is_same_position(&original),
        "the node that came back is the node that went in"
    );
    let mut parent = returned.clone();
    assert!(parent.move_to_parent());
    assert_eq!(parent.local_name(), "catalog", "the parent is unchanged");
    assert_eq!(
        docs.borrow().generate_id(returned),
        original_id,
        "generate-id() is unchanged"
    );

    // The negative: a *copy* is a different node with a different id.
    let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
    let mut pipe = ComplexContentNormalizer::new(builder);
    pipe.start_document().unwrap();
    pipe.append_item(&XmlItem::Node(original.clone())).unwrap();
    pipe.end_document().unwrap();
    let copy_doc = pipe.into_inner().finish().expect("finish");
    let copy_id = docs.borrow_mut().insert(copy_doc, None);
    let copy_root = docs.borrow().navigator(copy_id);
    let mut copy = copy_root.clone();
    assert!(copy.move_to_first_child());
    assert_eq!(copy.local_name(), "item", "the copy is the item subtree");
    assert!(
        !copy.is_same_position(&original),
        "a copied node is NOT `is`-equal to its original"
    );
    assert_ne!(
        docs.borrow().generate_id(&copy),
        original_id,
        "a copied node gets a different generate-id()"
    );
    // ... and it lost the parent the original has.
    let mut copy_parent = copy.clone();
    assert!(copy_parent.move_to_parent());
    assert_eq!(
        copy_parent.node_type(),
        DomNodeType::Root,
        "the copy's parent is its own new document node, not <catalog>"
    );
}

// ── Invariant 2 ──────────────────────────────────────────────────────────

/// One scripted event stream, replayed into two destinations.
fn play(pipe: &mut dyn Ops) -> Result<(), xslt2::XsltError> {
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

type Script = fn(&mut dyn Ops) -> Result<(), xslt2::XsltError>;

/// The part of [`Receiver`] the scripted tests drive, as an object-safe view.
///
/// The names differ from `Receiver`'s deliberately: a blanket impl for every
/// normalizer would otherwise make `pipe.start_element(..)` ambiguous.
trait Ops {
    fn doc_start(&mut self) -> Result<(), xslt2::XsltError>;
    fn doc_end(&mut self) -> Result<(), xslt2::XsltError>;
    fn el_start(&mut self, name: &Name) -> Result<(), xslt2::XsltError>;
    fn el_end(&mut self) -> Result<(), xslt2::XsltError>;
    fn attr(&mut self, name: &Name, value: &str) -> Result<(), xslt2::XsltError>;
    fn ns(&mut self, prefix: &str, uri: &str) -> Result<(), xslt2::XsltError>;
    fn chars(&mut self, s: &str) -> Result<(), xslt2::XsltError>;
    fn atomic(&mut self, s: &str) -> Result<(), xslt2::XsltError>;
    fn comment(&mut self, s: &str) -> Result<(), xslt2::XsltError>;
    fn pi(&mut self, target: &str, data: &str) -> Result<(), xslt2::XsltError>;
}

impl<'t, R: Receiver<'t>> Ops for ComplexContentNormalizer<'t, R> {
    fn doc_start(&mut self) -> Result<(), xslt2::XsltError> {
        Receiver::start_document(self)
    }
    fn doc_end(&mut self) -> Result<(), xslt2::XsltError> {
        Receiver::end_document(self)
    }
    fn el_start(&mut self, name: &Name) -> Result<(), xslt2::XsltError> {
        Receiver::start_element(self, name, None, false)
    }
    fn el_end(&mut self) -> Result<(), xslt2::XsltError> {
        Receiver::end_element(self)
    }
    fn attr(&mut self, name: &Name, value: &str) -> Result<(), xslt2::XsltError> {
        Receiver::attribute(self, name, value, None)
    }
    fn ns(&mut self, prefix: &str, uri: &str) -> Result<(), xslt2::XsltError> {
        Receiver::namespace(self, prefix, uri)
    }
    fn chars(&mut self, s: &str) -> Result<(), xslt2::XsltError> {
        Receiver::text(self, s, false)
    }
    fn atomic(&mut self, s: &str) -> Result<(), xslt2::XsltError> {
        Receiver::append_item(
            self,
            &XmlItem::Atomic(xsd_schema::types::XmlValue::string(s.to_string())),
        )
    }
    fn comment(&mut self, s: &str) -> Result<(), xslt2::XsltError> {
        Receiver::comment(self, s)
    }
    fn pi(&mut self, target: &str, data: &str) -> Result<(), xslt2::XsltError> {
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

// ── Invariant 3 ──────────────────────────────────────────────────────────

/// Running a `CompiledStylesheet` twice leaks nothing, and the second run's
/// `generate-id()` values are reproducible.
#[test]
fn inv3_two_runs_of_one_compiled_stylesheet_are_isolated() {
    // Compiled once, shared by both runs; nothing below mutates it.
    let names = NameTable::new();
    let functions = ProtoFunctions::new();
    let sheet = CompiledStylesheet::compile(&names, &functions, &["count(//item)"]).expect("compile");

    let mut ids = Vec::new();
    let mut serials = Vec::new();
    let mut counts = Vec::new();
    for _ in 0..2 {
        // A fresh arena per run: a run's documents cannot outlive it.
        let arena = Bump::new();
        let docs = store(&arena, &names);
        let run = Transformation::new(&docs);
        assert!(run.store().borrow().is_empty(), "a run starts with no documents");
        assert!(run.messages().is_empty(), "a run starts with no messages");

        let id = docs.borrow_mut().load_str("catalog.xml", SOURCE).expect("load");
        let root = docs.borrow().navigator(id);
        let third = item(&root, 2);
        ids.push(docs.borrow().generate_id(&third));
        serials.push(root.document().serial());
        counts.push(
            sheet
                .expr(0)
                .evaluator(sheet.context())
                .run_with_node::<Nav<'_>>(root.clone())
                .expect("eval")
                .as_f64()
                .expect("count(//item) is a number"),
        );
        assert_eq!(docs.borrow().len(), 1, "only this run's document is visible");
    }

    assert_eq!(ids[0], ids[1], "generate-id() repeats across runs");
    assert_eq!(counts, vec![3.0, 3.0], "the compiled expression is reusable and correct in both runs");
    // Non-vacuous: the two runs really are two different documents. Their
    // process-wide serials differ, which is exactly why generate-id() is built
    // from the store ordinal instead.
    assert_ne!(
        serials[0], serials[1],
        "the two runs parsed two distinct documents"
    );
}

// ── Invariant 4 ──────────────────────────────────────────────────────────

/// `item[@enabled][1]` applies the second predicate to the sequence the first
/// left behind — position 1 among *enabled* items.
#[test]
fn inv4_chained_pattern_predicates_apply_successively() {
    let arena = Bump::new();
    let names = NameTable::new();
    let functions = ProtoFunctions::new();
    let sheet = CompiledStylesheet::compile(&names, &functions, &["@enabled = 'yes'", "1"])
        .expect("compile");
    let docs = store(&arena, &names);
    let id = docs.borrow_mut().load_str("catalog.xml", SOURCE).expect("load");
    let root = docs.borrow().navigator(id);

    let predicates = [sheet.expr(0).clone(), sheet.expr(1).clone()];
    let matches: Vec<String> = (0..3)
        .map(|i| item(&root, i))
        .filter(|node| {
            pattern_step_matches(node, "item", &predicates, sheet.context()).expect("match")
        })
        .map(|node| attribute_value(&node, "id"))
        .collect();

    // item b is the first *enabled* item; item a is the first item overall and
    // is not enabled, so the naive reading would match nothing at all.
    assert_eq!(matches, vec!["b".to_string()]);

    // Non-vacuous counterexample: reversing the predicates asks for "the first
    // item, if it is enabled", which selects nothing here.
    let reversed = [sheet.expr(1).clone(), sheet.expr(0).clone()];
    let reversed_matches: Vec<String> = (0..3)
        .map(|i| item(&root, i))
        .filter(|node| {
            pattern_step_matches(node, "item", &reversed, sheet.context()).expect("match")
        })
        .map(|node| attribute_value(&node, "id"))
        .collect();
    assert!(
        reversed_matches.is_empty(),
        "item[1][@enabled='yes'] must select nothing: item a is first but not enabled"
    );
}

fn attribute_value(node: &Nav<'_>, name: &str) -> String {
    let mut attr = node.clone();
    assert!(attr.move_to_first_attribute());
    loop {
        if attr.local_name() == name {
            return attr.value();
        }
        assert!(attr.move_to_next_attribute(), "no attribute {name}");
    }
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

const NILLED_XML: &str = r#"<root><value xsi:nil="true" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"/></root>"#;

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
        matches!(source_binding.type_key, TypeKey::Simple(_) | TypeKey::Complex(_)),
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

// ── Invariant 6 ──────────────────────────────────────────────────────────

/// A global variable that loads a document via `document()`, whose evaluation
/// recurses through a stylesheet function that touches a second document,
/// returns nodes from both without dangling borrows (§6.1).
#[test]
fn inv6_global_variable_spans_two_documents_through_a_recursive_function() {
    let arena = Bump::new();
    let names = NameTable::new();
    let functions = ProtoFunctions::new();
    let sheet = CompiledStylesheet::compile(&names, &functions, &["proto:doc('catalog.xml')/catalog/item"])
        .expect("compile");
    let docs = store(&arena, &names);
    docs.borrow_mut().register_source("catalog.xml", SOURCE);
    docs.borrow_mut().register_source("extra.xml", SECOND);

    // The global variable: evaluated through the XPath engine, loading its
    // document from the store as it goes.
    let evaluator = functions.evaluator(&docs);
    let global = sheet
        .expr(0)
        .evaluator(sheet.context())
        .run_with::<Nav<'_>, _>(|eval| {
            let ctx = eval.context();
            ctx.set_function_evaluator(Some(&evaluator));
        })
        .expect("the global variable evaluates");
    let items = global.into_vec();
    assert_eq!(items.len(), 3, "proto:doc() loaded the first document");

    // The navigator obtained before the second load is still live after it —
    // this is the borrow-check shape §6.1 asks M0b to prove.
    let first = items[0].as_node().expect("node").clone();
    let mut run = Transformation::new(&docs);
    let collected = recursive_collect(
        &mut run,
        Frame {
            slots: vec![XPathValue::from_node(first.clone())],
        },
        2,
        "extra.xml",
    )
    .expect("recursion");

    let nodes: Vec<&Nav<'_>> = collected.iter().filter_map(|i| i.as_node()).collect();
    let from_first = nodes
        .iter()
        .filter(|n| std::ptr::eq(n.document(), first.document()))
        .count();
    let from_second = nodes
        .iter()
        .filter(|n| !std::ptr::eq(n.document(), first.document()))
        .count();
    assert!(from_first > 0, "nodes from the first document came back");
    assert_eq!(from_second, 1, "and the node from the second document too");

    // Both are still usable after every load: the first document's node still
    // knows its parent, the second one's still has its name.
    let mut parent = first.clone();
    assert!(parent.move_to_parent());
    assert_eq!(parent.local_name(), "catalog");
    let second = nodes
        .iter()
        .find(|n| !std::ptr::eq(n.document(), first.document()))
        .expect("second");
    assert_eq!(second.local_name(), "extra");
    // Non-vacuous: two documents, two identities.
    assert!(!second.is_same_position(&first));
}

// ── Q6 / Q7 and the store's own contracts ────────────────────────────────

/// Q7: a `DocumentKind::Full` document accepts several element children.
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
    assert_eq!(children, 3, "three document elements under one Full document");
    assert_eq!(
        serialize::to_string(&doc.create_navigator(), &Default::default()).expect("serialize"),
        "<one/><two/><three/>",
    );
}

/// Q6: what `DocumentKind::Fragment` plus `new_assertion` actually does to a
/// constructed, supposedly parentless element.
#[test]
fn q6_fragment_scope_hides_the_root_from_paths_but_not_from_parent() {
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

    // What fragment scope *does* give: `/` and `//` see the element, not the
    // synthetic root, because the root's child axis is hidden.
    let mut visible = node.clone();
    visible.move_to_visible_root();
    assert!(
        visible.is_same_position(&node),
        "move_to_visible_root lands on the constructed element"
    );
    let mut root = node.clone();
    root.move_to_root();
    assert!(
        !root.move_to_first_child(),
        "the synthetic root exposes no children in fragment scope"
    );

    // What it does NOT give (the deviation M1 must handle): `parent::node()`
    // still reaches the synthetic document node, and `fn:root()` —
    // `move_to_root` — still lands on it.
    let mut parent = node.clone();
    assert!(
        parent.move_to_parent(),
        "DocumentKind::Fragment does NOT make parent:: empty; \
         move_to_parent ignores the assertion flags (xsd-schema navigator.rs:589)"
    );
    assert_eq!(parent.node_type(), DomNodeType::Root);
    assert!(
        !root.is_same_position(&node),
        "fn:root() returns the synthetic root, not the element"
    );
}

/// The same URI is the same document: one parse, one identity (§16.1).
#[test]
fn store_same_uri_same_identity() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let first = docs.borrow_mut().load_str("catalog.xml", SOURCE).expect("load");
    let second = docs.borrow_mut().load_str("catalog.xml", SOURCE).expect("load");
    assert_eq!(first, second, "the same URI resolves to the same DocId");
    assert_eq!(docs.borrow().len(), 1, "and is parsed once");

    let a = item(&docs.borrow().navigator(first), 0);
    let b = item(&docs.borrow().navigator(second), 0);
    assert!(a.is_same_position(&b), "so the nodes are the same nodes");
    assert_eq!(docs.borrow().generate_id(&a), docs.borrow().generate_id(&b));

    // Non-vacuous: a different URI with the same text is a different document.
    let other = docs.borrow_mut().load_str("copy.xml", SOURCE).expect("load");
    assert_ne!(first, other);
    let c = item(&docs.borrow().navigator(other), 0);
    assert!(!a.is_same_position(&c));
    assert_ne!(docs.borrow().generate_id(&a), docs.borrow().generate_id(&c));
}

/// A re-entrant load is an error with a code, not a panic and not a double
/// borrow (§9.1).
#[test]
fn store_reentrant_load_is_an_error_not_a_panic() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    docs.borrow_mut().enter_load("catalog.xml").expect("guard");

    let err = docs
        .borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect_err("a re-entrant load is refused");
    assert_eq!(err.code, codes::XTDE1170);
    assert!(docs.borrow().is_empty(), "nothing was registered");

    // And it is only the guard: once released, the same load succeeds.
    docs.borrow_mut().leave_load("catalog.xml");
    docs.borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect("the load succeeds once the guard is released");
    assert_eq!(docs.borrow().len(), 1);
}

/// Q35: one `NameTable`, shared by every document of every run.
///
/// The prototype takes option (a) of §6.6 — documented single-threaded reuse.
/// The table is a caller-owned value borrowed by the store, by every document
/// it builds and by the compiled stylesheet, so a `NameId` means the same thing
/// everywhere in a transformation *and* across two transformations driven by
/// the same compiled stylesheet.
#[test]
fn q35_one_name_table_serves_every_document_and_every_run() {
    let names = NameTable::new();
    let functions = ProtoFunctions::new();
    let sheet = CompiledStylesheet::compile(&names, &functions, &["1"]).expect("compile");
    assert!(
        std::ptr::eq(sheet.names(), &names),
        "the compiled stylesheet borrows the caller's table"
    );

    for _ in 0..2 {
        let arena = Bump::new();
        let docs = store(&arena, &names);
        let a = docs.borrow_mut().load_str("a.xml", SOURCE).expect("load");
        let b = docs.borrow_mut().load_str("b.xml", SECOND).expect("load");
        let (doc_a, doc_b) = {
            let borrowed = docs.borrow();
            (
                borrowed.get(a).expect("a").doc,
                borrowed.get(b).expect("b").doc,
            )
        };
        assert!(
            std::ptr::eq(doc_a.names(), &names) && std::ptr::eq(doc_b.names(), &names),
            "every document of the run was built with that same table"
        );
        // Non-vacuous: a table of its own would be a different address.
        let other = NameTable::new();
        assert!(!std::ptr::eq(doc_a.names(), &other));
    }
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
        let outcome = (|| -> Result<(), xslt2::XsltError> {
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
    pipe.start_element(&Name::local("wrap"), None, false).unwrap();
    pipe.append_item(&XmlItem::Node(doc_node.clone())).unwrap();
    pipe.end_element().unwrap();
    pipe.end_document().unwrap();

    // The document node itself leaves no trace; its children do.
    assert_eq!(
        pipe.into_inner().into_string(),
        "<wrap><!--lead--><a>1</a></wrap>"
    );
}

/// `SequenceCollector` keeps an existing node by reference (§6.3 row 2).
#[test]
fn sequence_collector_keeps_identity_and_builds_parentless_nodes() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let id = docs.borrow_mut().load_str("catalog.xml", SOURCE).expect("load");
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

/// A guard on the expression cache: `XPathExpr` has no lifetimes, so a compiled
/// stylesheet really is reusable across runs (Q14).
#[test]
fn compiled_expressions_are_lifetime_free() {
    fn takes_owned(_: XPathExpr) {}
    let names = NameTable::new();
    let functions = ProtoFunctions::new();
    let sheet = CompiledStylesheet::compile(&names, &functions, &["1 + 1"]).expect("compile");
    takes_owned(sheet.expr(0).clone());
}

/// What a deep copy of a namespaced subtree actually looks like — the exact
/// text, so a change in upstream's declaration policy shows up here.
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
        r#"<p:kid xmlns:p="urn:x" k="1">t<!--c--><in xmlns="urn:d"/></p:kid>"#,
        "the copy re-declares every prefix its own names need, and nothing else"
    );
}
