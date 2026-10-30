//! The serializer driven directly, without a transformation: the cost of an
//! element does not depend on its depth.
//!
//! Moved from the integration tests when the serializer became
//! crate-internal.

use std::time::{Duration, Instant};

use xsd_schema::document::{BufferDocumentBuilder, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;

use super::{serialize, TreeContext};
use crate::output::{OutputDefinition, OutputMethod};

// ═══ Per-element cost independent of depth ══════════════════════════════

/// A tree of `depth` nested `e` elements; with `flip`, every level switches the
/// default namespace (`urn:d`, then none).
fn deep_tree(depth: usize, flip: bool, method: OutputMethod) -> (String, Duration) {
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
    let start = Instant::now();
    let text = serialize(
        &doc.create_navigator(),
        &definition,
        TreeContext::default(),
        None,
    )
    .unwrap();
    (text, start.elapsed())
}

/// The writer reads each element's own namespace declarations and keeps its
/// own scope, instead of the in-scope namespaces, which the tree gathers from
/// every ancestor: the declarations come out as before, and the time no longer
/// grows with the square (or, with a declaration on every level, the cube) of
/// the depth. Before the fix, 3,000 levels that each switch the default
/// namespace took about 30 s in a release build (the review measured 1,000
/// levels at 1.16 s); now they take about a millisecond. The bound leaves a
/// wide margin for a debug build on a busy machine.
#[test]
fn r4_deep_trees_serialize_in_linear_time() {
    let (small, _) = deep_tree(4, true, OutputMethod::Xml);
    assert_eq!(
        small,
        r#"<e xmlns="urn:d"><e xmlns=""><e xmlns="urn:d"><e xmlns=""/></e></e></e>"#
    );
    let (flipped, elapsed) = deep_tree(3_000, true, OutputMethod::Xml);
    assert_eq!(flipped.matches("<e xmlns=\"\">").count(), 1_499);
    assert!(
        elapsed < Duration::from_secs(5),
        "3,000 levels switching the default namespace took {elapsed:?}"
    );
    let (plain, elapsed) = deep_tree(100_000, false, OutputMethod::Html);
    assert_eq!(plain.len(), 100_000 * "<e></e>".len());
    assert!(
        elapsed < Duration::from_secs(5),
        "100,000 nested elements took {elapsed:?}"
    );
}
