//! `[xsl:]inherit-namespaces="no"` (harness review F9).
//!
//! XSLT 2.0 §5.7.1, the last rule of *Constructing Complex Content*:
//!
//! > If the newly constructed node is an element node, and if namespaces are
//! > inherited, then each namespace node of the newly constructed element
//! > (including any produced as a result of the namespace fixup process) is
//! > copied to each descendant element of the newly constructed element …
//!
//! and §11.1.3 (literal result elements), §11.2 (`xsl:element`) and §11.9.1
//! (`xsl:copy`), each in the same words:
//!
//! > If the value is set to `no`, then these namespace nodes are not
//! > automatically copied to the children. This may result in namespace
//! > undeclarations (such as `xmlns=""` or, in the case of XML 1.1,
//! > `xmlns:p=""`) appearing on the child elements when a final result tree is
//! > serialized.
//!
//! The processor used to withhold only the default namespace. A child of a
//! non-inheriting element still had every **prefixed** namespace node of its
//! parent — invisible in XML 1.0 output, which cannot undeclare a prefix
//! (§5.7.3's note: "namespace declarations written for the parent element will
//! be inherited by its child elements as if the corresponding namespace nodes
//! were present on the child element"), but plain on the `namespace` axis and
//! in `in-scope-prefixes()` of the tree. Every test below reads the tree, and
//! every one failed before the fix (the `REPORT` says how that was checked).
//!
//! The tests from `withholding_covers_bindings_the_parent_inherited` on come
//! from the independent review of that fix (`XSLT2_NS_INHERIT_REVIEW.md`,
//! findings N1, N2, N4 and O1): deep copies must not hand withheld bindings
//! back, `copy-namespaces="no"` must not hand a copy's fixup-produced nodes
//! down, and a deep non-inheriting chain must stay linear to serialize. The
//! last one, `a_deep_non_inheriting_chain_is_linear_to_normalize`, comes from
//! the follow-up review (`XSLT2_NS_INHERIT_REVIEW2.md`, finding R1): it must
//! stay linear to normalize, too.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};

use xslt2::receiver::{serialize_document, ComplexContentNormalizer, EventSerializer, Name};
use xslt2::{Compiler, MemoryResolver, Receiver, ResultBuilder, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// The source document of every run: two prefixed bindings on the root.
const SOURCE: &str = r#"<p:a xmlns:p="urn:p" xmlns:q="urn:q"><x/><p:y/></p:a>"#;

/// `f:ns($e)`: the element's name and its in-scope namespaces as
/// `prefix=uri`, sorted, `xml` left out (`#d` for the default namespace).
const NS_FUNCTION: &str = r#"
  <xsl:function name="f:ns" as="xs:string">
    <xsl:param name="e" as="element()"/>
    <xsl:variable name="s" as="xs:string*">
      <xsl:perform-sort select="for $p in in-scope-prefixes($e)[. ne 'xml'] return
          concat(if ($p eq '') then '#d' else $p, '=', namespace-uri-for-prefix($p, $e))">
        <xsl:sort select="."/>
      </xsl:perform-sort>
    </xsl:variable>
    <xsl:sequence select="concat(name($e), '{', string-join($s, ' '), '}')"/>
  </xsl:function>"#;

/// What a run produced: the serialized primary result and, for every element
/// of the primary result tree in document order, `name{prefix=uri …}` read off
/// the namespace axis.
struct Run {
    serialized: String,
    tree: String,
}

fn run(body: &str) -> Run {
    run_on(body, SOURCE)
}

/// [`run`] over another source document.
fn run_on(body: &str, source: &str) -> Run {
    let sheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{NS_FUNCTION}{body}</xsl:stylesheet>"#
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///ns-inherit/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet, "file:///ns-inherit/main.xsl")
        .expect("compile");
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///ns-inherit/in.xml", source)
        .expect("run");
    let serialized = compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .expect("serialize");
    let mut tree = Vec::new();
    if let Some(doc) = outcome.primary {
        walk(&doc.create_navigator(), &mut tree);
    }
    Run {
        serialized,
        tree: tree.join(" "),
    }
}

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

// ═══ §11.1.3 — literal result elements ═══════════════════════════════════

/// The basic shape: a child built by `xsl:element` (which copies no namespace
/// node from the stylesheet, §11.2) under `<a xmlns:p xsl:inherit-namespaces=…>`.
/// With `yes` the child has `p`, with `no` it has none — on the tree; the
/// XML 1.0 serialization is the same both ways, because a prefix cannot be
/// undeclared there.
#[test]
fn literal_result_element_no_withholds_prefixed_namespaces() {
    let yes = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p"><xsl:element name="c"/></a></xsl:template>"#,
    );
    assert_eq!(yes.tree, "a{p=urn:p} c{p=urn:p}");
    let no = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><xsl:element name="c"/></a></xsl:template>"#,
    );
    assert_eq!(
        no.tree, "a{p=urn:p} c{}",
        "§11.1.3: not copied to the children"
    );
    // §5.7.3 note: in XML 1.0 output the child looks as if it had inherited.
    assert_eq!(no.serialized, r#"<a xmlns:p="urn:p"><c/></a>"#);
    assert_eq!(no.serialized, yes.serialized);
}

/// The same, seen from XPath on a temporary tree: `in-scope-prefixes()` and
/// the `namespace` axis of the child.
#[test]
fn the_effect_is_visible_to_xpath_on_a_temporary_tree() {
    let out = run(r#"<xsl:template match="/">
          <xsl:variable name="y"><a xmlns:p="urn:p"><xsl:element name="c"/></a></xsl:variable>
          <xsl:variable name="n"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><xsl:element name="c"/></a></xsl:variable>
          <r><xsl:value-of select="f:ns($y/a/c), f:ns($n/a/c), exists($n/a/c/namespace::p), count($n/a/c/namespace::*)"/></r>
        </xsl:template>"#);
    // `count(namespace::*)` is 1: the `xml` namespace node every element has.
    assert_eq!(out.serialized, "<r>c{p=urn:p} c{} false 1</r>");
}

/// A literal result element child keeps the namespace nodes it brings from
/// the stylesheet (§11.1.3: "The created element node will have a copy of the
/// namespace nodes that were present on the element node in the stylesheet
/// tree with the exception of any namespace node whose string value is
/// designated as an excluded namespace") — and with `p` excluded, it has none.
#[test]
fn a_literal_result_element_child_has_only_its_own_namespace_nodes() {
    let own = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xmlns:q="urn:q" xsl:inherit-namespaces="no"><b xsl:exclude-result-prefixes="q"/></a></xsl:template>"#,
    );
    assert_eq!(own.tree, "a{p=urn:p q=urn:q} b{p=urn:p}");
    let excluded = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><b xsl:exclude-result-prefixes="p"/></a></xsl:template>"#,
    );
    assert_eq!(excluded.tree, "a{p=urn:p} b{}");
}

/// The withheld bindings do not come back further down: a grandchild
/// inherits from the child (`inherit-namespaces` defaults to `yes` there),
/// and the child has nothing of the parent's to pass on.
#[test]
fn descendants_do_not_get_the_withheld_namespaces_either() {
    let out = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><xsl:element name="c"><xsl:element name="d"/></xsl:element></a></xsl:template>"#,
    );
    assert_eq!(out.tree, "a{p=urn:p} c{} d{}");
    // Two non-inheriting levels: `c` withholds what *it* has, not what `a` had.
    let chain = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><xsl:element name="c" inherit-namespaces="no"><xsl:namespace name="q">urn:q</xsl:namespace><xsl:element name="d"/></xsl:element></a></xsl:template>"#,
    );
    assert_eq!(chain.tree, "a{p=urn:p} c{q=urn:q} d{}");
    assert_eq!(
        chain.serialized,
        r#"<a xmlns:p="urn:p"><c xmlns:q="urn:q"><d/></c></a>"#
    );
}

// ═══ §5.7.3 — namespace fixup still applies to the child ══════════════════

/// "namespace nodes will still be added to the result element as required by
/// the namespace fixup process": a child whose own name, or an attribute's
/// name, is in the parent's namespace gets that one namespace node back — and
/// only that one (`q` stays withheld).
#[test]
fn fixup_gives_the_child_what_its_names_need_and_nothing_more() {
    let named = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xmlns:q="urn:q" xsl:inherit-namespaces="no"><xsl:element name="p:c"><xsl:element name="d"/></xsl:element></a></xsl:template>"#,
    );
    // `d` inherits from `p:c`, whose `p` is its own (produced by fixup).
    assert_eq!(
        named.tree, "a{p=urn:p q=urn:q} c{p=urn:p} d{p=urn:p}",
        "§5.7.1: fixup-produced namespace nodes are inherited"
    );
    assert_eq!(
        named.serialized,
        r#"<a xmlns:p="urn:p" xmlns:q="urn:q"><p:c><d/></p:c></a>"#
    );
    let attribute = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xmlns:q="urn:q" xsl:inherit-namespaces="no"><xsl:element name="c"><xsl:attribute name="p:at">1</xsl:attribute></xsl:element></a></xsl:template>"#,
    );
    assert_eq!(attribute.tree, "a{p=urn:p q=urn:q} c{p=urn:p}");
    assert_eq!(
        attribute.serialized,
        r#"<a xmlns:p="urn:p" xmlns:q="urn:q"><c p:at="1"/></a>"#
    );
    // The same prefix bound elsewhere on the child: its own binding, written.
    let rebound = run(
        r#"<xsl:template match="/"><a xmlns:p="urn:p" xmlns:q="urn:q" xsl:inherit-namespaces="no"><xsl:element name="p:c" namespace="urn:x"/></a></xsl:template>"#,
    );
    assert_eq!(rebound.tree, "a{p=urn:p q=urn:q} c{p=urn:x}");
    assert_eq!(
        rebound.serialized,
        r#"<a xmlns:p="urn:p" xmlns:q="urn:q"><p:c xmlns:p="urn:x"/></a>"#
    );
}

// ═══ §11.2 — xsl:element ══════════════════════════════════════════════════

/// `xsl:element … inherit-namespaces="no"`, with the parent's namespace node
/// made by `xsl:namespace` and, separately, by fixup for the parent's own
/// name — §11.2: "(whether these were copied from those of the source node,
/// or generated as a result of namespace fixup)". The first one is also built
/// as an `as="element()"` variable, i.e. through the sequence collector.
#[test]
fn xsl_element_no_withholds_explicit_and_fixup_produced_namespaces() {
    let explicit = run(
        r#"<xsl:template match="/"><xsl:element name="a" inherit-namespaces="no"><xsl:namespace name="p">urn:p</xsl:namespace><xsl:element name="c"/></xsl:element></xsl:template>"#,
    );
    assert_eq!(explicit.tree, "a{p=urn:p} c{}");
    let fixup = run(
        r#"<xsl:template match="/"><xsl:element name="p:a" namespace="urn:p" inherit-namespaces="no"><xsl:element name="c"/></xsl:element></xsl:template>"#,
    );
    assert_eq!(fixup.tree, "a{p=urn:p} c{}");
    assert_eq!(fixup.serialized, r#"<p:a xmlns:p="urn:p"><c/></p:a>"#);
    let collected = run(r#"<xsl:template match="/">
          <xsl:variable name="n" as="element()"><xsl:element name="a" inherit-namespaces="no"><xsl:namespace name="p">urn:p</xsl:namespace><xsl:element name="c"/></xsl:element></xsl:variable>
          <xsl:variable name="y" as="element()"><xsl:element name="a"><xsl:namespace name="p">urn:p</xsl:namespace><xsl:element name="c"/></xsl:element></xsl:variable>
          <r><xsl:value-of select="f:ns($y/c), f:ns($n/c)"/></r>
        </xsl:template>"#);
    assert_eq!(collected.serialized, "<r>c{p=urn:p} c{}</r>");
}

// ═══ §11.9.1 — xsl:copy ═══════════════════════════════════════════════════

/// `xsl:copy inherit-namespaces="no"` of `<p:a xmlns:p xmlns:q>`: the copy
/// has both namespace nodes (`copy-namespaces` defaults to `yes`), its new
/// child has neither. A copied child (`xsl:copy copy-namespaces="no"`) gets
/// only what fixup gives it for its own name.
#[test]
fn xsl_copy_no_withholds_the_copied_namespaces() {
    let out = run(
        r#"<xsl:template match="/*"><xsl:copy inherit-namespaces="no"><xsl:element name="c"/></xsl:copy></xsl:template>"#,
    );
    assert_eq!(out.tree, "a{p=urn:p q=urn:q} c{}");
    let identity = run(
        r#"<xsl:template match="/*"><xsl:copy inherit-namespaces="no"><xsl:apply-templates/></xsl:copy></xsl:template>
           <xsl:template match="*"><xsl:copy copy-namespaces="no"/></xsl:template>"#,
    );
    assert_eq!(identity.tree, "a{p=urn:p q=urn:q} x{} y{p=urn:p}");
    assert_eq!(
        identity.serialized,
        r#"<p:a xmlns:p="urn:p" xmlns:q="urn:q"><x/><p:y/></p:a>"#
    );
    // The default `yes`, for contrast.
    let yes = run(
        r#"<xsl:template match="/*"><xsl:copy><xsl:element name="c"/></xsl:copy></xsl:template>"#,
    );
    assert_eq!(yes.tree, "a{p=urn:p q=urn:q} c{p=urn:p q=urn:q}");
}

/// A deep copy attached to a non-inheriting element: the copy keeps the
/// namespace nodes it was copied with (§11.9.2), and none of the parent's.
#[test]
fn a_deep_copy_under_a_non_inheriting_element_gets_only_its_own_namespaces() {
    let out = run(
        r#"<xsl:template match="/"><a xmlns:z="urn:z" xsl:inherit-namespaces="no"><xsl:copy-of select="/*"/></a></xsl:template>"#,
    );
    assert_eq!(
        out.tree,
        "a{z=urn:z} a{p=urn:p q=urn:q} x{p=urn:p q=urn:q} y{p=urn:p q=urn:q}"
    );
    let bare = run(
        r#"<xsl:template match="/"><a xmlns:z="urn:z" xsl:inherit-namespaces="no"><xsl:copy-of select="/*/x" copy-namespaces="no"/></a></xsl:template>"#,
    );
    assert_eq!(bare.tree, "a{z=urn:z} x{}");
}

// ═══ The pipeline itself: both destinations of the normalizer ═════════════

/// Architecture invariant 2 (`tests/m0b_invariants.rs`) — direct
/// serialization equals serialization of the materialized tree — for a stream
/// with `no_inherit_namespaces`. The tree says "no namespace node `p`" with a
/// prefixed undeclaration, which XML 1.0 cannot write and upstream
/// `serialize` never does; the event serializer has to agree, and must not
/// re-declare a binding the written output still has in force.
#[test]
fn both_destinations_agree_on_a_non_inheriting_stream() {
    fn play<'t>(out: &mut dyn Receiver<'t>) -> Result<(), xslt2::XsltError> {
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

// ═══ Review N4 — what the parent itself inherited is withheld too ═════════

/// The shape every nested literal result element has: `m`'s stylesheet
/// namespace `q` is already in scope — `n` declared it — so `m` does not
/// declare it again, yet `q` is one of `m`'s namespace nodes, and §11.1.3
/// says "these namespace nodes are not automatically copied to the
/// children". Withholding only what the parent declares itself passed every
/// test above and leaked `q` here (review N4). The second case pins the
/// reading of review N5: `m` excludes `q` from its own nodes but still has it
/// by inheritance from `n`, and `d` gets neither `q` nor `m`'s own `p`.
#[test]
fn withholding_covers_bindings_the_parent_inherited() {
    let nested = run(
        r#"<xsl:template match="/"><n xmlns:q="urn:q"><m xsl:inherit-namespaces="no"><xsl:element name="d"/></m></n></xsl:template>"#,
    );
    assert_eq!(nested.tree, "n{q=urn:q} m{q=urn:q} d{}");
    let inherited = run(
        r#"<xsl:template match="/"><n xmlns:q="urn:q"><m xmlns:p="urn:p" xsl:inherit-namespaces="no" xsl:exclude-result-prefixes="q"><xsl:element name="d"/></m></n></xsl:template>"#,
    );
    assert_eq!(inherited.tree, "n{q=urn:q} m{p=urn:p q=urn:q} d{}");
    // The same through `xsl:copy inherit-namespaces="no"` of a source element
    // whose namespace nodes are all inherited from the copy's new parent.
    let copied = run(
        r#"<xsl:template match="/*"><n xmlns:q="urn:q"><xsl:for-each select="x"><xsl:copy inherit-namespaces="no" copy-namespaces="no"><xsl:element name="d"/></xsl:copy></xsl:for-each></n></xsl:template>"#,
    );
    assert_eq!(copied.tree, "n{q=urn:q} x{q=urn:q} d{}");
}

// ═══ Review N1 — a copy does not hand withheld bindings back ══════════════

/// §11.9.2: with `copy-namespaces="yes"` "all the namespace nodes of the
/// original element are copied to the new element" — all of them, and so no
/// others: a deep copy of a tree built with `inherit-namespaces="no"` must not
/// give the child of the non-inheriting element the bindings it was denied,
/// whether the copy lands in a new parent (`xsl:copy-of`, `xsl:sequence` in
/// element content), at the top level, in a temporary tree or as a parentless
/// `as=` node. The new parent's own namespace nodes still reach the copy
/// (§5.7.1's last rule is about the element the copy is attached to). The
/// shape is the commonest way the fix is used: build in a variable, copy into
/// the result. XML 1.0 output cannot tell.
#[test]
fn a_copy_does_not_give_a_withheld_child_its_bindings_back() {
    const T: &str = r#"<xsl:variable name="t"><a xmlns:p="urn:p" xsl:inherit-namespaces="no"><xsl:element name="c"><xsl:element name="d"/></xsl:element></a></xsl:variable>"#;
    let into = run(&format!(
        r#"<xsl:template match="/">{T}<r><xsl:copy-of select="$t"/></r></xsl:template>"#
    ));
    assert_eq!(into.tree, "r{} a{p=urn:p} c{} d{}");
    let top = run(&format!(
        r#"<xsl:template match="/">{T}<xsl:copy-of select="$t"/></xsl:template>"#
    ));
    assert_eq!(top.tree, "a{p=urn:p} c{} d{}");
    let sequence = run(&format!(
        r#"<xsl:template match="/">{T}<r xmlns:z="urn:z"><xsl:sequence select="$t/a"/></r></xsl:template>"#
    ));
    assert_eq!(
        sequence.tree,
        "r{z=urn:z} a{p=urn:p z=urn:z} c{z=urn:z} d{z=urn:z}"
    );
    assert_eq!(
        sequence.serialized,
        r#"<r xmlns:z="urn:z"><a xmlns:p="urn:p"><c><d/></c></a></r>"#
    );
    let xpath = run(&format!(
        r#"<xsl:template match="/">{T}
          <xsl:variable name="u"><xsl:copy-of select="$t"/></xsl:variable>
          <xsl:variable name="v" as="element()*"><xsl:copy-of select="$t/a"/></xsl:variable>
          <r><xsl:value-of select="f:ns($t/a/c), f:ns($u/a/c), f:ns($v/c), f:ns($v/c/d)"/></r>
        </xsl:template>"#
    ));
    assert_eq!(xpath.serialized, "<r>c{} c{} c{} d{}</r>");
}

// ═══ Review O1 — copy-namespaces="no" ═════════════════════════════════════

/// §11.9.2, `copy-namespaces="no"`: "none of the namespace nodes are copied:
/// however, namespace nodes will still be created in the result tree as
/// required by the namespace fixup process … This attribute affects all
/// elements copied by this instruction: both elements selected directly by the
/// select expression, and elements that are descendants of nodes selected by
/// the select expression." Fixup gives each copied element what *its* names
/// need, and §5.7.3 adds that it "does not add a namespace node to an element
/// merely because its parent node in the result tree has such a namespace
/// node": `x` (no namespace) has none of `p:a`'s `p`. What does reach the copy
/// is the namespace nodes of the element it is attached to, if that one
/// inherits (§5.7.1).
#[test]
fn copy_namespaces_no_hands_down_no_fixup_produced_node() {
    let top = run(
        r#"<xsl:template match="/"><xsl:copy-of select="/*" copy-namespaces="no"/></xsl:template>"#,
    );
    assert_eq!(top.tree, "a{p=urn:p} x{} y{p=urn:p}");
    assert_eq!(top.serialized, r#"<p:a xmlns:p="urn:p"><x/><p:y/></p:a>"#);
    let inheriting = run(
        r#"<xsl:template match="/"><a xmlns:z="urn:z"><xsl:copy-of select="/*" copy-namespaces="no"/></a></xsl:template>"#,
    );
    assert_eq!(
        inheriting.tree,
        "a{z=urn:z} a{p=urn:p z=urn:z} x{z=urn:z} y{p=urn:p z=urn:z}"
    );
    let withholding = run(
        r#"<xsl:template match="/"><a xmlns:z="urn:z" xsl:inherit-namespaces="no"><xsl:copy-of select="/*" copy-namespaces="no"/></a></xsl:template>"#,
    );
    assert_eq!(withholding.tree, "a{z=urn:z} a{p=urn:p} x{} y{p=urn:p}");
}

/// The same for the default namespace — "a namespace node with the same name
/// (or absence of a name)", §5.7.1 treats both alike — and here XML 1.0
/// output does show it: the copied `p:b` has no default namespace of its own
/// with `copy-namespaces="no"`, so it is undeclared on it. With the default
/// `yes` the copy keeps the one its original had.
#[test]
fn copy_namespaces_no_hands_down_no_default_namespace_either() {
    let out = run(r#"<xsl:template match="/">
             <xsl:variable name="s"><a xmlns="urn:d"><p:b xmlns:p="urn:p"/></a></xsl:variable>
             <xsl:copy-of select="$s/*" copy-namespaces="no"/>
             <xsl:copy-of select="$s/*"/>
           </xsl:template>"#);
    assert_eq!(
        out.tree,
        "a{#d=urn:d} b{p=urn:p} a{#d=urn:d} b{#d=urn:d p=urn:p}"
    );
    assert_eq!(
        out.serialized,
        r#"<a xmlns="urn:d"><p:b xmlns="" xmlns:p="urn:p"/></a><a xmlns="urn:d"><p:b xmlns:p="urn:p"/></a>"#
    );
}

// ═══ Review N2 — a deep non-inheriting chain stays linear ═════════════════

/// Runs `sheet` over `source` on a large stack (the chains below are
/// thousands of levels deep) and returns the number of namespace declarations each element
/// of the result carries itself (`NamespaceAxisScope::Local`), in document
/// order, and how long transformation plus serialization took. A chain that
/// re-declares on every level is reported without serializing it, which is
/// what would take minutes.
fn timed(sheet: String, source: String) -> (Vec<usize>, Option<Duration>) {
    xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || {
        let names = NameTable::new();
        let functions = XsltFunctions::new();
        let compiled = Compiler::new(&names, &functions)
            .compile_str(&sheet, "file:///ns-inherit/deep.xsl")
            .expect("compile");
        let arena = Bump::new();
        let started = Instant::now();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///ns-inherit/deep.xml", &source)
            .expect("run");
        let transformed = started.elapsed();
        let mut counts = Vec::new();
        let mut element = outcome
            .primary
            .as_ref()
            .expect("a primary result")
            .create_navigator();
        while element.move_to_first_child() {
            let mut count = 0;
            let mut ns = element.clone();
            if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
                count += 1;
                while ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                    count += 1;
                }
            }
            counts.push(count);
        }
        if counts.iter().skip(1).any(|&count| count > 0) {
            return (counts, None);
        }
        let started = Instant::now();
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .expect("serialize");
        (counts, Some(transformed + started.elapsed()))
    })
}

/// A non-inheriting element that carries namespace nodes of its own — a
/// recursive literal result element with `xsl:inherit-namespaces="no"`, an
/// identity `xsl:copy inherit-namespaces="no"` — used to re-declare them on
/// every level, because they were withheld and then re-declared with the very
/// URI the parent binds. Upstream's namespace axis walks the declaration chain
/// from every ancestor that has declarations, so serializing such a chain was
/// cubic: 47 s at 2,000 levels in a release build instead of 4 ms (review N2).
/// Now only the top of the chain declares anything. The depth is 1,500 levels
/// in a debug build, whose frames are large enough for the identity chain to
/// reach the stack guard at 2,000, and 3,000 in a release build; the fixed
/// code takes milliseconds either way, and the regression took about 230 s
/// per chain at 1,500 levels in a debug build (20 s in a release one) and
/// would take minutes at 3,000 in a release build — two orders of magnitude
/// above the 2 s bound, which in turn is far above what the test needs.
#[test]
fn a_deep_non_inheriting_chain_does_not_redeclare_its_namespaces() {
    const DEPTH: usize = if cfg!(debug_assertions) { 1_500 } else { 3_000 };
    const BOUND: Duration = Duration::from_secs(2);
    let declarations: String = (0..5)
        .map(|i| format!(r#" xmlns:n{i}="urn:n{i}""#))
        .collect();

    let literal = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:template match="/"><xsl:call-template name="r"><xsl:with-param name="d" select="{DEPTH}"/></xsl:call-template></xsl:template>
             <xsl:template name="r"><xsl:param name="d"/>
               <r{declarations} xsl:inherit-namespaces="no"><xsl:if test="$d gt 1"><xsl:call-template name="r"><xsl:with-param name="d" select="$d - 1"/></xsl:call-template></xsl:if></r>
             </xsl:template>
           </xsl:stylesheet>"#
    );
    let (counts, elapsed) = timed(literal, "<x/>".to_string());
    assert_eq!(counts.len(), DEPTH);
    assert_eq!(counts[0], 5, "the top of the chain declares its five");
    assert!(
        counts[1..].iter().all(|&count| count == 0),
        "no level below re-declares them: {:?}",
        &counts[1..6]
    );
    let elapsed = elapsed.expect("serialized");
    assert!(
        elapsed < BOUND,
        "deep literal result chain took {elapsed:?}"
    );

    let mut source = format!("<e{declarations}>");
    source.push_str(&"<e>".repeat(DEPTH - 1));
    source.push_str(&"</e>".repeat(DEPTH));
    let identity = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:template match="*"><xsl:copy inherit-namespaces="no"><xsl:apply-templates/></xsl:copy></xsl:template>
           </xsl:stylesheet>"#
    );
    let (counts, elapsed) = timed(identity, source);
    assert_eq!(counts.len(), DEPTH);
    assert_eq!(counts[0], 5);
    assert!(
        counts[1..].iter().all(|&count| count == 0),
        "no level below re-declares them: {:?}",
        &counts[1..6]
    );
    let elapsed = elapsed.expect("serialized");
    assert!(elapsed < BOUND, "deep identity chain took {elapsed:?}");
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
/// were withheld (review R1). Now each element carries the bindings in scope
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
