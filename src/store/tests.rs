//! The document store: one URI is one document, a re-entrant load is an
//! error, and one name table serves every document of every run.
//!
//! Moved from the integration tests when the store's loading API became
//! crate-internal.

use std::cell::RefCell;

use bumpalo::Bump;
use pretty_assertions::assert_eq;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType};

use crate::error::codes;
use crate::receiver::Nav;
use crate::store::DocumentStore;
use crate::{Compiler, XsltFunctions};

const SOURCE: &str = concat!(
    r#"<catalog><item id="a" enabled="no"><t>A</t></item>"#,
    r#"<item id="b" enabled="yes"><t>B</t></item>"#,
    r#"<item id="c" enabled="yes"><t>C</t></item></catalog>"#,
);

const SECOND: &str = r#"<extra><note>second</note></extra>"#;

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

/// The same URI is the same document: one parse, one identity (§16.1).
#[test]
fn store_same_uri_same_identity() {
    let arena = Bump::new();
    let names = NameTable::new();
    let docs = store(&arena, &names);
    let first = docs
        .borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect("load");
    let second = docs
        .borrow_mut()
        .load_str("catalog.xml", SOURCE)
        .expect("load");
    assert_eq!(first, second, "the same URI resolves to the same DocId");
    assert_eq!(docs.borrow().len(), 1, "and is parsed once");

    let a = item(&docs.borrow().navigator(first), 0);
    let b = item(&docs.borrow().navigator(second), 0);
    assert!(a.is_same_position(&b), "so the nodes are the same nodes");
    assert_eq!(docs.borrow().generate_id(&a), docs.borrow().generate_id(&b));

    // Non-vacuous: a different URI with the same text is a different document.
    let other = docs
        .borrow_mut()
        .load_str("copy.xml", SOURCE)
        .expect("load");
    assert_ne!(first, other);
    let c = item(&docs.borrow().navigator(other), 0);
    assert!(!a.is_same_position(&c));
    assert_ne!(docs.borrow().generate_id(&a), docs.borrow().generate_id(&c));
}

/// A re-entrant load is an error with a code, not a panic and not a double
/// borrow.
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

/// One `NameTable`, shared by every document of every run, reused on one
/// thread. The table is a caller-owned value borrowed by the store, by every document
/// it builds and by the compiled stylesheet, so a `NameId` means the same thing
/// everywhere in a transformation *and* across two transformations driven by
/// the same compiled stylesheet.
#[test]
fn q35_one_name_table_serves_every_document_and_every_run() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            concat!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">"#,
                r#"<xsl:template match="/"><out/></xsl:template></xsl:stylesheet>"#
            ),
            "urn:invariants:sheet.xsl",
        )
        .expect("compile");
    assert!(
        std::ptr::eq(compiled.names(), &names),
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
