//! Regression tests for the two defects an independent review found in M2a.
//!
//! * **Finding 1** — §5.5.4's recovery action was applied to *every* error
//!   raised while matching a pattern, so a missing feature (`XSLT2-NOTIMPL`)
//!   or a broken invariant (`XSLT2-INTERNAL`) silently selected a different
//!   template rule. The licence covers "any dynamic error or type error" and
//!   nothing else; the tests below pin both halves of that — the two internal
//!   codes now propagate, and a genuine dynamic error is still recovered.
//! * **Finding 2** — `element(E, xs:untyped)` and `attribute(A, xs:untypedAtomic)`
//!   looked only at the node's kind and name, so a schema-typed node matched
//!   them. Both the type test (§4.3, XPath 2.0 §2.5.4.3) and the §21.1
//!   admission rule XTDE1665 are pinned here.
//!
//! The helper mirrors `m2a_groundwork.rs`: an in-memory resolver serving
//! `file:///m2a/in.xml`, a stylesheet compiled at `file:///m2a/main.xsl`.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;
use xsd_schema::pipeline::load_and_process_schema;
use xsd_schema::SchemaSet;

use xslt2::error::codes;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn sheet(body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
           exclude-result-prefixes=\"xs\">{body}</xsl:stylesheet>"
    )
}

/// Runs one stylesheet over one source document, with or without
/// `strict_recoverable`, and returns the serialized primary result.
fn run(body: &str, source: &str, strict: bool) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///m2a/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet(body), "file:///m2a/main.xsl")?;
    let arena = Bump::new();
    let options = TransformOptions {
        strict_recoverable: strict,
        ..TransformOptions::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .with_resolver(resolver)
        .transform_source("file:///m2a/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// A stylesheet with a high-priority rule carrying `predicate` and a plain
/// `item` rule below it. If the predicate's error is recovered from, the
/// fallback rule wins and the result says so — which is exactly the silent
/// wrong answer finding 1 is about.
fn two_rules(predicate: &str) -> String {
    format!(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="r/item"/></out></xsl:template>
           <xsl:template match="item[{predicate}]">PREDICATE</xsl:template>
           <xsl:template match="item">FALLBACK</xsl:template>"#
    )
}

const ONE_ITEM: &str = r#"<r><item n="zz"/></r>"#;

// ═══ Finding 1 — recovery may not swallow a processor failure ═════════════

/// The reviewer's reproduction, with today's stand-in. `format-number()` was in
/// the catalog as an `XSLT2-NOTIMPL` stub (M2a, phase 3), which is what made
/// this reachable: a stylesheet calling it used to fail to compile. M3d
/// implemented it (`tests/m3d_formatting.rs`), and the next stand-in, an
/// extension instruction without `xsl:fallback`, is XTDE1450 since M3e; the
/// missing feature is now a UCA collation computed at run time in a build
/// without the `icu-collation` feature (M2k), in the body of a stylesheet
/// function the predicate calls. With the feature no missing feature is left
/// for an evaluation to reach, and the guard is pinned by the unit tests of
/// `runtime::patterns` (`guard_recoverable`).
///
/// §5.5.4 licenses recovery for "any dynamic error or type error"; a missing
/// implementation is neither, so the run must fail — in **both** modes — and
/// must never quietly fall through to the `FALLBACK` rule, which is what the
/// defect made it do.
#[cfg(not(feature = "icu-collation"))]
#[test]
fn a_not_implemented_function_in_a_pattern_is_never_recovered_from() {
    for strict in [false, true] {
        let error = match run(&rules_calling_a_missing_feature(), ONE_ITEM, strict) {
            Err(e) => e,
            Ok(out) => panic!(
                "strict={strict}: a missing feature in a pattern must fail the run, \
                 but it produced {out} — the error was swallowed and a rule chosen"
            ),
        };
        assert!(
            error.is_not_implemented(),
            "strict={strict}: expected XSLT2-NOTIMPL, got {error}"
        );
        assert!(
            error.to_string().contains("collation/UCA"),
            "strict={strict}: the message should name the feature: {error}"
        );
    }
}

/// The hole is older and wider than the `format-*` stubs: **any** missing
/// feature reached inside a pattern predicate was swallowed the same way.
/// (`key()` was an `XSLT2-NOTIMPL` stub from M1 until M2f implemented it, and
/// was the third case here; `current-group() = 1` was the first until M2g,
/// which made it the static XTSE1060 — `tests/m2g_grouping.rs`; `regex-group(1)`
/// was the second until M2h implemented it — `tests/m2h_regex.rs`; the
/// `format-*` stubs until M3d — `tests/m3d_formatting.rs`. No function an
/// expression can call is missing any more, so the feature is reached through a
/// stylesheet function, called on its own and inside a larger expression.)
#[cfg(not(feature = "icu-collation"))]
#[test]
fn any_missing_feature_in_a_pattern_predicate_propagates() {
    for predicate in ["m:missing(.)", "not(m:missing(.)) or true()"] {
        let body = rules_calling_a_missing_feature().replace("m:missing(.)", predicate);
        for strict in [false, true] {
            let error = match run(&body, ONE_ITEM, strict) {
                Err(e) => e,
                Ok(out) => panic!("{predicate} (strict={strict}) produced {out}"),
            };
            assert!(
                error.is_not_implemented(),
                "{predicate} (strict={strict}): expected XSLT2-NOTIMPL, got {error}"
            );
        }
    }
}

/// [`two_rules`] whose predicate calls `m:missing(.)`, a stylesheet function
/// that sorts under a UCA collation computed at run time (`XSLT2-NOTIMPL`,
/// M2k, in a build without the `icu-collation` feature).
#[cfg(not(feature = "icu-collation"))]
fn rules_calling_a_missing_feature() -> String {
    r#"<xsl:template match="/"><out><xsl:apply-templates select="r/item"/></out></xsl:template>
       <xsl:template match="item[m:missing(.)]" xmlns:m="urn:m">PREDICATE</xsl:template>
       <xsl:template match="item">FALLBACK</xsl:template>
       <xsl:function name="m:missing" xmlns:m="urn:m">
         <xsl:param name="n"/>
         <xsl:perform-sort select="(2, 1)">
           <xsl:sort select="." collation="{concat('http://www.w3.org/2013/collation/UCA', '?strength=secondary')}"/>
         </xsl:perform-sort>
         <xsl:sequence select="true()"/>
       </xsl:function>"#
        .to_string()
}

/// The other half of §5.5.4, which must **not** be lost: a genuine dynamic
/// error of the stylesheet is still recovered by treating the pattern as not
/// matching, so the lower-priority rule is selected.
///
/// > \[§5.5.4\] Any dynamic error or type error that occurs during the
/// > evaluation of a pattern against a particular node is treated as a
/// > recoverable error … The optional recovery action is to treat the pattern
/// > as not matching.
#[test]
fn a_genuine_dynamic_error_in_a_predicate_is_still_recovered() {
    // A failed cast (FORG0001) and a division by zero (FOAR0001).
    for (predicate, code) in [
        ("xs:integer(@n) = 1", "FORG0001"),
        ("@n cast as xs:integer = 1", "FORG0001"),
        ("xs:date(@n) = xs:date('2000-01-01')", "FORG0001"),
        ("1 idiv 0 = 1", "FOAR0001"),
    ] {
        assert_eq!(
            run(&two_rules(predicate), ONE_ITEM, false).unwrap(),
            "<out>FALLBACK</out>",
            "{predicate}: the recovery action is to treat the pattern as not matching"
        );
        let error = run(&two_rules(predicate), ONE_ITEM, true)
            .expect_err("strict_recoverable must signal it instead");
        assert_eq!(error.code, code, "{predicate}: {error}");
        assert!(
            !error.is_processor_failure(),
            "{predicate}: this is an error of the stylesheet, not of the processor"
        );
    }
}

/// An operator applied to operands it is not defined for is a **type** error —
/// XPath 2.0 §B.2: "If the operator is not defined for the types of its
/// operands, a type error is raised \[err:XPTY0004\]" — and the engine reports
/// it without a code of its own. It used to reach this crate as
/// `XSLT2-INTERNAL`, which after finding 1 would have made it non-recoverable;
/// it is classified as XPTY0004 instead, so §5.5.4 still applies to it.
#[test]
fn an_undefined_operator_is_a_type_error_not_an_internal_failure() {
    let predicate = "xs:gYear('2000') gt xs:gYear('2001')";
    assert_eq!(
        run(&two_rules(predicate), ONE_ITEM, false).unwrap(),
        "<out>FALLBACK</out>",
        "a type error is recoverable in a pattern"
    );
    let error = run(&two_rules(predicate), ONE_ITEM, true).expect_err("strict signals it");
    assert_eq!(error.code, "XPTY0004", "{error}");
    assert!(!error.is_internal(), "{error}");
}

/// The classification rule itself, which is what every site shares.
#[test]
fn the_recoverability_rule_is_one_predicate() {
    let notimpl = XsltError::not_implemented("format-number()", "M3d");
    let internal = XsltError::internal("a broken invariant");
    for e in [&notimpl, &internal] {
        assert!(e.is_processor_failure());
        assert!(!e.is_recoverable_in_pattern());
    }
    for code in [codes::XTDE1340, codes::XTTE0570, "XPTY0004", "FORG0001"] {
        let e = XsltError::new(code, "x");
        assert!(!e.is_processor_failure(), "{code}");
        assert!(e.is_recoverable_in_pattern(), "{code}");
    }
}

// ═══ Finding 2 — the untyped type tests read the type annotation ══════════

const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="item" type="xs:integer"/>
      </xs:sequence>
      <xs:attribute name="n" type="xs:integer"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

const TYPED_XML: &str = r#"<r n="7"><item>42</item></r>"#;

fn typed_schema() -> SchemaSet {
    let mut schema_set = SchemaSet::xsd11();
    load_and_process_schema(
        SCHEMA.as_bytes(),
        "file:///m2a/s.xsd",
        &mut schema_set,
        None,
    )
    .expect("the test schema compiles");
    schema_set
}

/// A schema-validated document, inserted into the store as a prebuilt tree and
/// run with `transform_document()` — the one route by which a typed node can
/// reach this processor.
///
/// > \[ERR XTDE1665\] A basic XSLT processor must raise a non-recoverable
/// > dynamic error if the input to the processor includes a node with a type
/// > annotation other than `xs:untyped` or `xs:untypedAtomic` …
#[test]
fn a_prebuilt_typed_document_is_rejected_with_xtde1665() {
    let schema_set = typed_schema();
    let arena = Bump::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&schema_set.name_table, &functions)
        .compile_str(
            &sheet(r#"<xsl:template match="/"><out/></xsl:template>"#),
            "file:///m2a/main.xsl",
        )
        .expect("the stylesheet compiles");

    let doc = build_typed_document(
        TYPED_XML.as_bytes(),
        &arena,
        &schema_set,
        BufferDocumentOptions::default(),
    )
    .expect("a schema-validated document");

    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run
        .store()
        .borrow_mut()
        .insert(doc, Some("file:///m2a/typed.xml"));
    let error = match run.transform_document(id) {
        Err(e) => e,
        Ok(_) => panic!("a basic XSLT processor must reject a typed input tree"),
    };
    assert_eq!(error.code, codes::XTDE1665, "{error}");
    assert!(
        error.to_string().contains("item") || error.to_string().contains('r'),
        "the message should name the offending node: {error}"
    );

    // Nothing about a rejected tree may be remembered as "checked": running the
    // same `Transformation` over the same typed tree a second time must fail
    // the same way. (An earlier version cached the verdict per document and
    // let the second run through.)
    let again = match run.transform_document(id) {
        Err(e) => e,
        Ok(_) => panic!("the second run over the same typed tree must be rejected too"),
    };
    assert_eq!(again.code, codes::XTDE1665, "{again}");
}

/// The same tree passed as a **stylesheet parameter** rather than as the
/// source: §21.1's note makes "input to the processor" deliberately wide, and
/// "trees passed as stylesheet parameters" are named in it.
#[test]
fn a_typed_node_parameter_is_rejected_with_xtde1665() {
    let schema_set = typed_schema();
    let arena = Bump::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&schema_set.name_table, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:param name="p"/>
                   <xsl:template match="/"><out/></xsl:template>"#,
            ),
            "file:///m2a/main.xsl",
        )
        .expect("the stylesheet compiles");

    let doc = build_typed_document(
        TYPED_XML.as_bytes(),
        &arena,
        &schema_set,
        BufferDocumentOptions::default(),
    )
    .expect("a schema-validated document");

    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run.store().borrow_mut().insert(doc, None);
    let nav = run.store().borrow().navigator(id);
    let run = run.with_param(
        xslt2::QName::new("", "p"),
        xsd_schema::xpath::functions::XPathValue::from_item(xsd_schema::xpath::XmlItem::Node(nav)),
    );
    // The source document is untyped; only the parameter is not.
    let error = match run.transform_source("file:///m2a/in.xml", "<r/>") {
        Err(e) => e,
        Ok(_) => panic!("a typed parameter is input to the processor too"),
    };
    assert_eq!(error.code, codes::XTDE1665, "{error}");
}

/// The check asks the document, in constant time, whether it carries any type
/// annotation (`BufferDocument::has_type_annotations`) and walks it only when
/// it does — so an ordinary run is unaffected and an untyped prebuilt document
/// is admitted without being visited.
#[test]
fn an_untyped_prebuilt_document_is_still_admitted() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:template match="/"><out><xsl:apply-templates select="r/item"/></out></xsl:template>
                   <xsl:template match="element(item, xs:untyped)">UNTYPED</xsl:template>"#,
            ),
            "file:///m2a/main.xsl",
        )
        .expect("the stylesheet compiles");
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let doc = run
        .store()
        .borrow()
        .parse("<r><item>42</item></r>", Some("file:///m2a/plain.xml"))
        .expect("a plain parse");
    let id = run
        .store()
        .borrow_mut()
        .insert_ref(doc, Some("file:///m2a/plain.xml"));
    let outcome = run.transform_document(id).expect("an untyped tree is fine");
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out>UNTYPED</out>"
    );
}

/// Every untyped-document behaviour M2a pinned is unchanged: the type tests
/// still match, and still outrank the one-argument forms (§6.4).
///
/// The three conformance cases `next-match-008`, `-010` and `-021` exercise the
/// same ground through the W3C suite.
#[test]
fn the_untyped_type_tests_still_decide_an_untyped_document_exactly_as_before() {
    for (pattern, source, select) in [
        ("element(*, xs:untyped)", "<r><a/></r>", "r/a"),
        ("element(a, xs:untyped)", "<r><a/></r>", "r/a"),
        ("element(*, xs:anyType)", "<r><a/></r>", "r/a"),
        ("element(a, xs:anyType?)", "<r><a/></r>", "r/a"),
        (
            "attribute::attribute(*, xs:untypedAtomic)",
            r#"<r n="1"/>"#,
            "r/@n",
        ),
        (
            "attribute::attribute(n, xs:anySimpleType)",
            r#"<r n="1"/>"#,
            "r/@n",
        ),
        (
            "attribute::attribute(*, xs:anyAtomicType)",
            r#"<r n="1"/>"#,
            "r/@n",
        ),
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="{select}"/></out></xsl:template>
               <xsl:template match="{pattern}">HIT</xsl:template>"#
        );
        assert_eq!(
            run(&body, source, false).unwrap(),
            "<out>HIT</out>",
            "pattern {pattern}"
        );
    }
}
