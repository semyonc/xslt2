//! `[xsl:]inherit-namespaces="no"` in the push pipeline itself, without a
//! stylesheet: both destinations of the normalizer agree on a non-inheriting
//! stream, and a deep non-inheriting chain is linear to normalize.
//!
//! XSLT 2.0 §11.1.3 (and §11.2, §11.9.1 in the same words): "If the value is
//! set to `no`, then these namespace nodes are not automatically copied to the
//! children." Moved from the integration tests when the pipeline became
//! crate-internal.

use std::time::{Duration, Instant};

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};

use crate::receiver::{
    serialize_document, ComplexContentNormalizer, EventSerializer, Name, Receiver, ResultBuilder,
};

/// Appends `name{prefix=uri …}` for every element below `nav`.
fn walk<N: DomNavigator>(nav: &N, out: &mut Vec<String>) {
    let mut child = nav.clone();
    if !child.move_to_first_child() {
        return;
    }
    loop {
        if child.node_type() == DomNodeType::Element {
            out.push(format!("{}{{{}}}", child.local_name(), namespaces(&child)));
            walk(&child, out);
        }
        if !child.move_to_next_sibling() {
            break;
        }
    }
}

/// The element's namespace nodes (the `namespace` axis, i.e.
/// `NamespaceAxisScope::All`), `xml` left out, sorted.
fn namespaces<N: DomNavigator>(element: &N) -> String {
    let mut pairs = Vec::new();
    let mut ns = element.clone();
    if ns.move_to_first_namespace(NamespaceAxisScope::All) {
        loop {
            let prefix = ns.local_name().to_string();
            if prefix != "xml" {
                let prefix = if prefix.is_empty() {
                    "#d".into()
                } else {
                    prefix
                };
                pairs.push(format!("{prefix}={}", ns.value()));
            }
            if !ns.move_to_next_namespace(NamespaceAxisScope::All) {
                break;
            }
        }
    }
    pairs.sort();
    pairs.join(" ")
}

// ═══ The pipeline itself: both destinations of the normalizer ═════════════

/// The pipeline's invariant that direct serialization equals serialization
/// of the materialized tree (`invariants.rs`), for a stream
/// with `no_inherit_namespaces`. The tree says "no namespace node `p`" with a
/// prefixed undeclaration, which XML 1.0 cannot write and upstream
/// `serialize` never does; the event serializer has to agree, and must not
/// re-declare a binding the written output still has in force.
#[test]
fn both_destinations_agree_on_a_non_inheriting_stream() {
    fn play<'t>(out: &mut dyn Receiver<'t>) -> Result<(), crate::XsltError> {
        out.start_document()?;
        out.start_element(&Name::local("a"), None, false)?;
        out.no_inherit_namespaces()?;
        out.namespace("p", "urn:p")?;
        out.namespace("q", "urn:q")?;
        out.end_start_tag()?;
        out.start_element(&Name::local("c"), None, false)?;
        out.end_start_tag()?;
        out.start_element(&Name::qualified("p", "d", "urn:p"), None, false)?;
        out.end_start_tag()?;
        out.end_element()?;
        out.end_element()?;
        out.end_element()?;
        out.end_document()
    }
    let arena = Bump::new();
    let names = NameTable::new();

    let mut direct = ComplexContentNormalizer::new(EventSerializer::new());
    play(&mut direct).expect("direct");
    let direct = direct.into_inner().into_string();

    let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
    let mut tree = ComplexContentNormalizer::new(builder);
    play(&mut tree).expect("tree");
    let doc = tree.into_inner().finish().expect("finish");
    let mut elements = Vec::new();
    walk(&doc.create_navigator(), &mut elements);
    assert_eq!(
        elements.join(" "),
        "a{p=urn:p q=urn:q} c{} d{p=urn:p}",
        "c withholds both; fixup gives d back the p its name needs"
    );
    let materialized = serialize_document(&doc).expect("serialize");
    assert_eq!(
        materialized,
        r#"<a xmlns:p="urn:p" xmlns:q="urn:q"><c><p:d/></c></a>"#
    );
    assert_eq!(direct, materialized, "invariant 2");
}

// ═══ Review R1 — finding what a parent withholds does not cost its depth ══

/// Plays a chain of `depth` elements `e` in no namespace, every one of them
/// built with `inherit-namespaces="no"`, into `out`, the outermost declaring
/// `n0` … `n{k-1}`. Fails as soon as `bound` has passed, checking every 1,024
/// levels, so that a regression is reported in seconds rather than minutes.
/// Returns how long it took.
fn non_inheriting_chain<'t>(
    out: &mut dyn Receiver<'t>,
    depth: usize,
    k: usize,
    bound: Duration,
) -> Duration {
    let started = Instant::now();
    let name = Name::local("e");
    out.start_document().expect("start document");
    for level in 0..depth {
        out.start_element(&name, None, false).expect("start");
        if level == 0 {
            for i in 0..k {
                out.namespace(&format!("n{i}"), &format!("urn:n{i}"))
                    .expect("namespace");
            }
        }
        out.no_inherit_namespaces().expect("no");
        if level % 1_024 == 0 {
            let elapsed = started.elapsed();
            assert!(
                elapsed < bound,
                "{level} of {depth} levels took {elapsed:?} already"
            );
        }
    }
    for _ in 0..depth {
        out.end_element().expect("end");
    }
    out.end_document().expect("end document");
    started.elapsed()
}

/// Every child of a non-inheriting element withholds the prefixed bindings
/// in scope at its parent. Finding them used to re-read the declarations of
/// every open ancestor — once per parent, which on a chain is once per
/// element — even where nothing is declared at all: O(depth²), 2.3 s at
/// 80,000 levels in a release build against 16 ms before prefixed bindings
/// were withheld. Now each element carries the bindings in scope
/// at it, shared with its parent when it declares nothing, so a level costs
/// the same at any depth.
///
/// The stream goes through the public `Receiver` API — no stylesheet, no
/// tree navigation, no serializer of a tree — so it times the normalizer
/// alone (with the event serializer, which writes as it goes), and then the
/// same chain with five bindings on its top into the tree builder. At
/// 130,000 levels the fixed code takes about 0.2 s per chain in a debug build
/// and 0.03 s in a release build; the regression took 100 s per chain in a
/// debug build (50 times the 2 s bound; the check stops it at the bound) and
/// 6 s in a release build. The depth is what keeps the test's memory near
/// 100 MB.
#[test]
fn a_deep_non_inheriting_chain_is_linear_to_normalize() {
    const DEPTH: usize = 130_000;
    const BOUND: Duration = Duration::from_secs(2);

    let mut direct = ComplexContentNormalizer::new(EventSerializer::new());
    let elapsed = non_inheriting_chain(&mut direct, DEPTH, 0, BOUND);
    let xml = direct.into_inner().into_string();
    // `<e>` and `</e>` per level, the innermost one written `<e/>`.
    let expected = format!(
        "{}<e/>{}",
        "<e>".repeat(DEPTH - 1),
        "</e>".repeat(DEPTH - 1)
    );
    assert!(
        xml == expected,
        "{} bytes instead of {}",
        xml.len(),
        expected.len()
    );
    assert!(elapsed < BOUND, "{DEPTH} levels took {elapsed:?}");

    let arena = Bump::new();
    let names = NameTable::new();
    let builder = ResultBuilder::new(&arena, &names, None).expect("builder");
    let mut tree = ComplexContentNormalizer::new(builder);
    let elapsed = non_inheriting_chain(&mut tree, DEPTH, 5, BOUND);
    assert!(
        elapsed < BOUND,
        "{DEPTH} levels into a tree took {elapsed:?}"
    );
    let doc = tree.into_inner().finish().expect("finish");
    // What each level declares itself (`Local`: the `namespace` axis walks
    // the ancestors, which would make this check the quadratic part).
    let mut element = doc.create_navigator();
    let mut declarations = Vec::new();
    while element.move_to_first_child() {
        let mut local = Vec::new();
        let mut ns = element.clone();
        if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
            loop {
                local.push(format!("{}={}", ns.local_name(), ns.value()));
                if !ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                    break;
                }
            }
        }
        local.sort();
        declarations.push(local.join(" "));
    }
    assert_eq!(declarations.len(), DEPTH);
    assert_eq!(
        declarations[0],
        "n0=urn:n0 n1=urn:n1 n2=urn:n2 n3=urn:n3 n4=urn:n4"
    );
    assert_eq!(
        declarations[1], "n0= n1= n2= n3= n4=",
        "the second level undeclares all five"
    );
    assert!(
        declarations[2..].iter().all(String::is_empty),
        "nothing is declared below the second level"
    );
}
