//! The serializer driven directly, without a transformation: the cost of an
//! element does not depend on its depth.
//!
//! Moved from the integration tests when the serializer became
//! crate-internal.

use std::time::Duration;

use xsd_schema::document::{BufferDocumentBuilder, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;

use super::{serialize, TreeContext};
use crate::output::{OutputDefinition, OutputMethod};
use crate::scaling::{assert_linear, timed};

// ═══ Per-element cost independent of depth ══════════════════════════════

/// Serializes a tree of `depth` nested `e` elements `times` times, and
/// returns the text and how long that took (building the tree left out); with
/// `flip`, every level switches the default namespace (`urn:d`, then none).
fn deep_tree(depth: usize, flip: bool, method: OutputMethod, times: usize) -> (String, Duration) {
    let arena = bumpalo::Bump::new();
    let names = NameTable::new();
    let mut b =
        BufferDocumentBuilder::new(&arena, &names, None, BufferDocumentOptions::default()).unwrap();
    for level in 0..depth {
        if flip {
            let uri = if level % 2 == 0 { "urn:d" } else { "" };
            b.start_element("e", uri, "", &[("", uri)]).unwrap();
        } else {
            b.start_element("e", "", "", &[]).unwrap();
        }
        b.end_of_attributes();
    }
    for _ in 0..depth {
        b.end_element().unwrap();
    }
    let doc = b.finalize().unwrap();
    let mut definition = OutputDefinition::default();
    definition.method.set(method, 1);
    definition.omit_xml_declaration.set(true, 1);
    definition.indent.set(false, 1);
    let mut text = String::new();
    let elapsed = timed(|| {
        for _ in 0..times {
            text = serialize(
                &doc.create_navigator(),
                &definition,
                TreeContext::default(),
                None,
            )
            .unwrap();
        }
    });
    (text, elapsed)
}

/// The writer reads each element's own namespace declarations and keeps its
/// own scope, instead of the in-scope namespaces, which the tree gathers from
/// every ancestor: the declarations come out as they should, and the time
/// grows linearly with the depth (`crate::scaling`), not with its square (or,
/// with a declaration on every level, its cube). Reading the in-scope
/// namespaces, 3,000 levels that each switch the default namespace took
/// about 30 s in a release build; now they take about a millisecond.
#[test]
fn deep_trees_serialize_in_linear_time() {
    let (small, _) = deep_tree(4, true, OutputMethod::Xml, 1);
    assert_eq!(
        small,
        r#"<e xmlns="urn:d"><e xmlns=""><e xmlns="urn:d"><e xmlns=""/></e></e></e>"#
    );
    assert_linear("levels switching the default namespace", 400, 8, |depth| {
        let (flipped, elapsed) = deep_tree(depth, true, OutputMethod::Xml, 5);
        // Every odd level undeclares the default namespace; the innermost
        // (odd) one is written as an empty element.
        assert_eq!(flipped.matches("<e xmlns=\"\">").count(), depth / 2 - 1);
        elapsed
    });
    assert_linear("nested elements", 12_500, 8, |depth| {
        let (plain, elapsed) = deep_tree(depth, false, OutputMethod::Html, 1);
        assert_eq!(plain.len(), depth * "<e></e>".len());
        elapsed
    });
}
