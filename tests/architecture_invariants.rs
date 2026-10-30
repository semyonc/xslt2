//! The architecture invariants the processor is built on — node identity, isolated runs, successive
//! pattern predicates, documents shared across a run — asserted through the public API.
//!
//! Every test asserts the negative as well as the positive: a proof that a copy
//! keeps a node's identity is worthless unless the test also shows that a real
//! copy does *not*.
//!
//! The invariants that need the crate's internal push pipeline or document
//! store (direct serialization against the materialized tree, typed copies, the
//! §5.7.1 rules, parentless constructed nodes, one name table for a run, and the
//! store's identity and re-entrancy) are unit tests in
//! `src/receiver/tests/invariants.rs` and `src/store/tests.rs`; this file keeps
//! the ones the public API expresses.

mod common;

use bumpalo::Bump;
use pretty_assertions::assert_eq;
use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::XPathExpr;

use xslt2::{Compiler, TransformOptions, XsltFunctions};

const SOURCE: &str = concat!(
    r#"<catalog><item id="a" enabled="no"><t>A</t></item>"#,
    r#"<item id="b" enabled="yes"><t>B</t></item>"#,
    r#"<item id="c" enabled="yes"><t>C</t></item></catalog>"#,
);

const SECOND: &str = r#"<extra><note>second</note></extra>"#;

/// Compiles `body` as a stylesheet and runs it over `SOURCE`, with `extra.xml`
/// registered so that `document()` can reach it.
///
/// The invariants below are asserted through the real compiler and runtime
/// wherever a stylesheet can express the claim.
fn run_sheet(body: &str) -> String {
    let mut memory = xslt2::MemoryResolver::new();
    // `document('extra.xml')` resolves against the stylesheet's base URI
    // `urn:invariants:sheet.xsl` by RFC 3986 §5.2: the base has a scheme and a
    // path with no `/`, so the merge (§5.2.3) replaces the whole path and the
    // target is `urn:extra.xml`.
    memory.insert("urn:extra.xml", SECOND);
    let resolver = std::sync::Arc::new(memory);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(resolver.clone())
        .compile_str(
            &format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">{body}</xsl:stylesheet>"#
            ),
            "urn:invariants:sheet.xsl",
        )
        .expect("compile");
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = run
        .transform_source("urn:invariants:catalog.xml", SOURCE)
        .expect("run");
    compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .expect("serialize")
}

// ── Invariant 1 ──────────────────────────────────────────────────────────

/// An existing node passed through a typed variable, a template parameter and
/// a recursive named template keeps its identity (`is`), its parent and its
/// `generate-id()`.
#[test]
fn node_identity_survives_variables_parameters_and_functions() {
    let out = run_sheet(
        r#"<xsl:template match="/">
             <!-- (1) a typed variable holds the node itself -->
             <xsl:variable name="n" as="element()" select="/catalog/item[2]"/>
             <!-- (2, 3) a template parameter, through two levels of recursion -->
             <xsl:call-template name="down">
               <xsl:with-param name="node" select="$n"/>
               <xsl:with-param name="depth" select="2"/>
             </xsl:call-template>
           </xsl:template>

           <xsl:template name="down">
             <xsl:param name="node" as="element()"/>
             <xsl:param name="depth" as="xs:integer"
                        xmlns:xs="http://www.w3.org/2001/XMLSchema"/>
             <xsl:choose>
               <xsl:when test="$depth > 0">
                 <xsl:call-template name="down">
                   <xsl:with-param name="node" select="$node"/>
                   <xsl:with-param name="depth" select="$depth - 1"/>
                 </xsl:call-template>
               </xsl:when>
               <xsl:otherwise>
                 <!-- A copy of the same node, for the counterexample. -->
                 <xsl:variable name="copy"><xsl:copy-of select="$node"/></xsl:variable>
                 <out same="{$node is /catalog/item[2]}"
                      parent="{name($node/..)}"
                      id-kept="{generate-id($node) = generate-id(/catalog/item[2])}"
                      copy-is-same="{$copy/item is $node}"
                      copy-id-differs="{generate-id($copy/item) != generate-id($node)}"
                      copy-parent="{name($copy/item/..)}"/>
               </xsl:otherwise>
             </xsl:choose>
           </xsl:template>"#,
    );
    assert_eq!(
        out,
        concat!(
            r#"<out same="true" parent="catalog" id-kept="true" "#,
            r#"copy-is-same="false" copy-id-differs="true" copy-parent=""/>"#
        ),
        "a node keeps its identity through variables and parameters, and a copy does not"
    );
}

// ── Invariant 3 ──────────────────────────────────────────────────────────

/// Running one `CompiledStylesheet` twice leaks nothing, and the second run's
/// `generate-id()` values repeat the first's.
#[test]
fn two_runs_of_one_compiled_stylesheet_are_isolated() {
    // Compiled once, shared by both runs; nothing below mutates it.
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            concat!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">"#,
                r#"<xsl:template match="/"><out n="{count(//item)}" "#,
                r#"id="{generate-id(/catalog/item[3])}"/></xsl:template>"#,
                r#"</xsl:stylesheet>"#
            ),
            "urn:invariants:sheet.xsl",
        )
        .expect("compile");

    let mut outputs = Vec::new();
    let mut serials = Vec::new();
    for _ in 0..2 {
        // A fresh arena per run: a run's documents cannot outlive it.
        let arena = Bump::new();
        let run = compiled.transformation(&arena, TransformOptions::default());
        assert!(
            run.store().borrow().is_empty(),
            "a run starts with no documents"
        );
        let outcome = run
            .transform_source("urn:invariants:catalog.xml", SOURCE)
            .expect("run");
        assert!(outcome.messages.is_empty(), "a run starts with no messages");
        serials.push(
            run.store()
                .borrow()
                .navigator(
                    run.store()
                        .borrow()
                        .id_of_uri("urn:invariants:catalog.xml")
                        .expect("the source is in the store"),
                )
                .document()
                .serial(),
        );
        outputs.push(
            compiled
                .serialize_primary(&outcome)
                .map(common::body)
                .expect("serialize"),
        );
    }

    assert_eq!(
        outputs[0], outputs[1],
        "both runs agree, generate-id() included"
    );
    assert!(outputs[0].contains(r#"n="3""#), "{}", outputs[0]);
    // Non-vacuous: the two runs really are two different documents. Their
    // process-wide serials differ, which is exactly why generate-id() is built
    // from the store ordinal instead.
    assert_ne!(
        serials[0], serials[1],
        "the two runs parsed two distinct documents"
    );
}

// ── Invariant 4 ──────────────────────────────────────────────────────────

/// `item[@enabled='yes'][1]` applies the second predicate to the sequence the
/// first left behind — position 1 among *enabled* items (§5.5.3).
#[test]
fn chained_pattern_predicates_apply_successively() {
    let matched = run_sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="catalog/item"/></out></xsl:template>
           <xsl:template match="item[@enabled='yes'][1]"><hit id="{@id}"/></xsl:template>
           <xsl:template match="item"/>"#,
    );
    // item b is the first *enabled* item; item a is the first item overall and
    // is not enabled, so the naive reading would match nothing at all.
    assert_eq!(matched, r#"<out><hit id="b"/></out>"#);

    // Non-vacuous counterexample: reversing the predicates asks for "the first
    // item, if it is enabled", which selects nothing here.
    let reversed = run_sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="catalog/item"/></out></xsl:template>
           <xsl:template match="item[1][@enabled='yes']"><hit id="{@id}"/></xsl:template>
           <xsl:template match="item"/>"#,
    );
    assert_eq!(
        reversed, "<out/>",
        "item[1][@enabled='yes'] must select nothing: item a is first but not enabled"
    );
}

// ── Invariant 6 ──────────────────────────────────────────────────────────

/// A global variable that loads a second document with `document()`, whose
/// nodes travel through a recursive named template together with nodes of the
/// principal source, without dangling borrows (§6.1).
#[test]
fn global_variable_spans_two_documents_through_a_recursive_function() {
    let out = run_sheet(
        r#"<xsl:variable name="extra" select="document('extra.xml')/extra/note"/>
           <xsl:template match="/">
             <xsl:call-template name="down">
               <xsl:with-param name="both" select="(/catalog/item[1], $extra)"/>
               <xsl:with-param name="depth" select="2"/>
             </xsl:call-template>
           </xsl:template>
           <xsl:template name="down">
             <xsl:param name="both" as="node()*"/>
             <xsl:param name="depth" as="xs:integer"
                        xmlns:xs="http://www.w3.org/2001/XMLSchema"/>
             <xsl:choose>
               <xsl:when test="$depth > 0">
                 <xsl:call-template name="down">
                   <xsl:with-param name="both" select="$both"/>
                   <xsl:with-param name="depth" select="$depth - 1"/>
                 </xsl:call-template>
               </xsl:when>
               <xsl:otherwise>
                 <out count="{count($both)}"
                      first-parent="{name($both[1]/..)}"
                      second="{$both[2]}"
                      second-doc="{name($both[2]/..)}"
                      distinct="{count($both[1] | $both[2])}"/>
               </xsl:otherwise>
             </xsl:choose>
           </xsl:template>"#,
    );
    // Two nodes from two documents come back; the first still knows its parent
    // after the second document was loaded, and the two are distinct nodes.
    assert_eq!(
        out,
        concat!(
            r#"<out count="2" first-parent="catalog" second="second" "#,
            r#"second-doc="extra" distinct="2"/>"#
        )
    );
}

/// A guard on the expression cache: `XPathExpr` has no lifetimes, so a
/// compiled stylesheet really is reusable across runs.
#[test]
fn compiled_expressions_are_lifetime_free() {
    fn takes_owned(_: XPathExpr) {}
    fn assert_send_sync<T: Send + Sync>() {}
    let names = NameTable::new();
    let ctx = xsd_schema::xpath::XPathContext::new(&names);
    takes_owned(XPathExpr::compile("1 + 1", &ctx).expect("compile"));
    assert_send_sync::<XPathExpr>();
}
