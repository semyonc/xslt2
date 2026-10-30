//! `use-when` exclusion, the functions every processor must bind, the untyped type
//! tests a basic processor decides in a pattern, and the owner a missing feature
//! names. Every test pins a *classification* or an invariant:
//!
//! * §3.12 `use-when` exclusion happens in one filtered child iterator, so every
//!   traversal of the stylesheet excludes. Each case has a negative control that
//!   shows the check is load bearing: remove `use-when` and the result changes.
//! * Every function §16 defines binds at every arity the specification gives it,
//!   so a call is never an unknown function; and the untyped type tests a basic
//!   processor can decide in a pattern are decided.
//! * A construct this processor does not implement is `XSLT2-NOTIMPL`, naming what
//!   would provide it.
//!
//! The helper mirrors `basic_semantics.rs`: an in-memory resolver serving
//! `file:///use-when/in.xml`, a stylesheet compiled at `file:///use-when/main.xsl`,
//! and the serialized primary result.

mod common;

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// `xs` and `my` are bound (and excluded from the result) because the type-test
/// cases name schema types and one user type in patterns and constructors.
fn sheet(body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:my=\"urn:my\" \
           exclude-result-prefixes=\"xs my\">{body}</xsl:stylesheet>"
    )
}

/// Runs one stylesheet over one source document and returns the serialized
/// primary result.
fn transform(body: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///use-when/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet(body), "file:///use-when/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///use-when/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

// ═══ §3.12 exclusion by construction ═══════════════════════════════════════

/// `Builder::sort_keys` is the traversal with the least coverage: it looks for
/// one element kind among the children. It goes through `Builder::children`,
/// and an excluded `xsl:sort` must be invisible.
///
/// §3.12: "the stylesheet module has the same effect as if the node were not
/// there."
#[test]
fn use_when_hides_a_sort_from_the_traversal_that_rejects_it() {
    let out = transform(
        r#"<xsl:template match="/">
             <out><xsl:for-each select="r/i">
               <xsl:sort use-when="false()" select="."/>
               <xsl:value-of select="."/>
             </xsl:for-each></out>
           </xsl:template>"#,
        "<r><i>b</i><i>a</i></r>",
    )
    .unwrap();
    // Document order, because there is no sort left to apply.
    assert_eq!(out, "<out>ba</out>");
}

/// The negative control: without `use-when` the very same stylesheet sorts,
/// which is what makes the test above meaningful.
#[test]
fn a_sort_that_is_not_excluded_still_sorts() {
    let out = transform(
        r#"<xsl:template match="/">
             <out><xsl:for-each select="r/i">
               <xsl:sort select="."/>
               <xsl:value-of select="."/>
             </xsl:for-each></out>
           </xsl:template>"#,
        "<r><i>b</i><i>a</i></r>",
    )
    .unwrap();
    assert_eq!(out, "<out>ab</out>");
}

/// `Builder::has_content` decides the `select`-xor-content rule of §9.2
/// (XTSE0620). An excluded child is not content, so `select` plus one excluded
/// element child is legal.
#[test]
fn use_when_makes_an_excluded_child_not_count_as_content() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" select="'ok'">
               <wrong xsl:use-when="false()"/>
             </xsl:variable>
             <out><xsl:value-of select="$v"/></out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap();
    assert_eq!(out, "<out>ok</out>");
}

/// The negative control for XTSE0620.
#[test]
fn a_child_that_is_not_excluded_still_conflicts_with_select() {
    let error = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" select="'ok'">
               <wrong/>
             </xsl:variable>
             <out><xsl:value-of select="$v"/></out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap_err();
    assert_eq!(error.code, "XTSE0620", "{error}");
}

/// `Builder::fallback_bodies` (§18.2.2) reads *every* `xsl:fallback` child of
/// an unknown extension instruction. An excluded one must not be one of them,
/// so the remaining fallback alone decides the result.
#[test]
fn use_when_hides_a_fallback_from_an_extension_instruction() {
    let out = transform(
        r#"<xsl:template match="/" xmlns:ext="urn:ext" extension-element-prefixes="ext">
             <out><ext:nonesuch>
               <xsl:fallback use-when="false()">WRONG</xsl:fallback>
               <xsl:fallback>RIGHT</xsl:fallback>
             </ext:nonesuch></out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

// ═══ The §16 functions ═══════════════════════════════════════════════════════

/// §16.4 `format-number` and §16.5 `format-dateTime`, `format-date` and
/// `format-time` run in each of their forms (more in
/// `tests/number_and_date_formatting.rs`).
#[test]
fn the_16_4_and_16_5_functions_that_were_stubs_now_run() {
    for (call, expected) in [
        (r#"format-number(1.5, '#.#')"#, "1.5"),
        (
            r#"format-dateTime(xs:dateTime('2002-12-31T12:00:00'), '[Y]')"#,
            "2002",
        ),
        (
            r#"format-dateTime(xs:dateTime('2002-12-31T12:00:00'), '[Y]', 'en', (), ())"#,
            "2002",
        ),
        (r#"format-date(xs:date('2002-12-31'), '[Y]')"#, "2002"),
        (
            r#"format-date(xs:date('2002-12-31'), '[Y]', 'en', (), ())"#,
            "2002",
        ),
        (r#"format-time(xs:time('12:00:00'), '[H]')"#, "12"),
        (
            r#"format-time(xs:time('12:00:00'), '[H]', 'en', (), ())"#,
            "12",
        ),
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:value-of select="{call}"/></out></xsl:template>"#
        );
        let out = transform(&body, "<r/>").unwrap_or_else(|e| panic!("{call}: {e}"));
        assert_eq!(out, format!("<out>{expected}</out>"), "{call}");
    }
}

/// §18.1.2: `function-available` "returns true if and only if there is a
/// function available with the given name and arity": the four are
/// available.
#[test]
fn function_available_for_the_former_stubs() {
    let out = transform(
        r#"<xsl:template match="/">
             <out>
               <xsl:value-of select="function-available('format-number')"/>
               <xsl:text>,</xsl:text>
               <xsl:value-of select="function-available('format-date')"/>
               <xsl:text>,</xsl:text>
               <xsl:value-of select="function-available('format-time')"/>
               <xsl:text>,</xsl:text>
               <xsl:value-of select="function-available('format-dateTime')"/>
               <xsl:text>|</xsl:text>
               <!-- A control: one that was always available. -->
               <xsl:value-of select="function-available('generate-id')"/>
             </out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap();
    assert_eq!(out, "<out>true,true,true,true|true</out>");
}

/// §3.12 `use-when` is evaluated at compile time by a static evaluator that
/// holds no transformation, and §3.12 keeps only the F&O functions and four
/// XSLT ones in scope in a `use-when` expression: the guard below is false
/// although `format-date` is implemented, so the element stays excluded.
#[test]
fn use_when_does_not_start_succeeding_because_of_a_stub() {
    let out = transform(
        r#"<xsl:template match="/">
             <out>
               <wrong xsl:use-when="function-available('format-date')"/>
               <xsl:text>RIGHT</xsl:text>
             </out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// The §16 catalog audit, as a test: every function §16 (plus §14, §15 and
/// §18.1) defines binds at every arity the spec gives it. A name missing from
/// the catalog fails to compile with XPST0017 and its cases are then counted as
/// wrong answers rather than as missing features.
#[test]
fn every_16_function_binds_at_every_spec_arity() {
    for call in [
        // §16.1 Multiple Source Documents
        "document('a.xml')",
        "document('a.xml', /)",
        // §16.2 Reading Text Files
        "unparsed-text('a.txt')",
        "unparsed-text('a.txt', 'utf-8')",
        "unparsed-text-available('a.txt')",
        "unparsed-text-available('a.txt', 'utf-8')",
        // §16.3.2 key
        "key('k', 'v')",
        "key('k', 'v', /)",
        // §16.4 Number Formatting
        "format-number(1, '#')",
        "format-number(1, '#', 'df')",
        // §16.5 Formatting Dates and Times
        "format-dateTime(xs:dateTime('2002-01-01T00:00:00'), '[Y]')",
        "format-dateTime(xs:dateTime('2002-01-01T00:00:00'), '[Y]', (), (), ())",
        "format-date(xs:date('2002-01-01'), '[Y]')",
        "format-date(xs:date('2002-01-01'), '[Y]', (), (), ())",
        "format-time(xs:time('00:00:00'), '[H]')",
        "format-time(xs:time('00:00:00'), '[H]', (), (), ())",
        // §16.6 Miscellaneous
        "current()",
        "unparsed-entity-uri('e')",
        "unparsed-entity-public-id('e')",
        "generate-id()",
        "generate-id(.)",
        "system-property('xsl:version')",
        // §14 Grouping, §15 Regular Expressions, §18.1 Extensibility
        "current-group()",
        "current-grouping-key()",
        "regex-group(1)",
        "function-available('concat')",
        "element-available('xsl:if')",
        "type-available('xs:string')",
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:value-of select="{call}"/></out></xsl:template>"#
        );
        // Binding is what is under test, so any *evaluation* outcome is fine;
        // only a static "unknown function" is a failure.
        if let Err(error) = transform(&body, "<r/>") {
            assert_ne!(error.code, "XPST0017", "{call} does not bind: {error}");
            assert!(
                !error.to_string().contains("not found"),
                "{call} does not bind: {error}"
            );
        }
    }
}

// ═══ Untyped type tests in a pattern ═══════════════════════════════════════

/// §4.3: with no schema every element is annotated `xs:untyped`, so
/// `element(*, xs:untyped)` matches every element — and §6.4's table gives it
/// priority **0**, above the −0.5 of `element()`. The rule with the type
/// argument therefore wins.
#[test]
fn an_untyped_element_test_matches_every_element_and_outranks_element() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="r/*"/></out></xsl:template>
           <xsl:template match="element()">LOW</xsl:template>
           <xsl:template match="element(*, xs:untyped)">TYPED</xsl:template>"#,
        "<r><a/><b/></r>",
    )
    .unwrap();
    assert_eq!(out, "<out>TYPEDTYPED</out>");
}

/// `element(E, T)` has priority 0.25 in §6.4's table — "matches by name and
/// type" — so it outranks the bare name test `one`, whose priority is 0.
#[test]
fn a_named_untyped_element_test_outranks_the_plain_name_test() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="r/*"/></out></xsl:template>
           <xsl:template match="one">NAME</xsl:template>
           <xsl:template match="element(one, xs:untyped)">NAME-AND-TYPE</xsl:template>"#,
        "<r><one/></r>",
    )
    .unwrap();
    assert_eq!(out, "<out>NAME-AND-TYPE</out>");
}

/// The attribute counterpart: `xs:untypedAtomic` is the annotation every
/// attribute carries, so `attribute(*, xs:untypedAtomic)` reduces to `@*`, with
/// priority 0 against the −0.5 of `attribute()`.
///
/// The axis is written out to keep this test about the type test alone. A
/// bare `attribute(…)` step means the same thing: XPath 2.0 §3.2.4, "the
/// default axis is child unless the axis step contains an AttributeTest or
/// SchemaAttributeTest; in that case, the default axis is attribute" —
/// `tests/conformance_regressions.rs` covers that rule.
#[test]
fn an_untyped_attribute_test_matches_every_attribute() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="r/@*"/></out></xsl:template>
           <xsl:template match="attribute::attribute()">LOW</xsl:template>
           <xsl:template match="attribute::attribute(*, xs:untypedAtomic)">TYPED</xsl:template>"#,
        r#"<r n="1"/>"#,
    )
    .unwrap();
    assert_eq!(out, "<out>TYPED</out>");
}

/// The supertypes decide the same way: an element's `xs:untyped` annotation is
/// derived from `xs:anyType`, and an attribute's `xs:untypedAtomic` from
/// `xs:anySimpleType` and `xs:anyAtomicType`, and an `ElementTest`/
/// `AttributeTest` matches a type "or derived from" it.
///
/// §6.4: "The presence or absence of the symbol `?` following a type name does
/// not affect the priority", so the nillable spelling is accepted too.
#[test]
fn the_supertypes_and_the_nillable_spelling_are_decided_too() {
    for pattern in [
        "element(*, xs:anyType)",
        "element(a, xs:untyped)",
        "element(*, xs:untyped?)",
        "element(a, xs:anyType?)",
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="r/a"/></out></xsl:template>
               <xsl:template match="{pattern}">HIT</xsl:template>"#
        );
        assert_eq!(
            transform(&body, "<r><a/></r>").unwrap(),
            "<out>HIT</out>",
            "pattern {pattern}"
        );
    }
    for pattern in [
        "attribute::attribute(*, xs:anySimpleType)",
        "attribute::attribute(*, xs:anyAtomicType)",
        "attribute::attribute(n, xs:untypedAtomic)",
        // No nillable spelling here: XPath 2.0 [66] `AttributeTest` has no
        // `"?"`, unlike [64] `ElementTest`.
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="r/@n"/></out></xsl:template>
               <xsl:template match="{pattern}">HIT</xsl:template>"#
        );
        assert_eq!(
            transform(&body, r#"<r n="1"/>"#).unwrap(),
            "<out>HIT</out>",
            "pattern {pattern}"
        );
    }
}

/// Any other type name needs a schema: in a basic processor it is a static
/// error.
#[test]
fn a_type_test_that_needs_a_schema_is_still_not_implemented() {
    // A user-defined type name with no schema behind it is not "schema-aware,
    // later": "TypeName must be present in the in-scope schema types
    // [err:XPST0008]" (XPath 2.0 §2.5.4.3), for any processor. The XPath
    // engine reports it when the pattern is compiled, and the pattern
    // compiler keeps that code: XTSE0340 is for a pattern that does
    // not match the Pattern syntax, while §5.3 gives an expression's other
    // static errors the XPath codes.
    let body = r#"<xsl:template match="/"><out/></xsl:template>
               <xsl:template match="element(a, my:t)">HIT</xsl:template>"#;
    let error = transform(body, "<r><a/></r>").expect_err("element(a, my:t) unexpectedly compiled");
    assert!(!error.is_not_implemented(), "{error}");
    assert_eq!(error.code, "XPST0008", "{error}");

    // A built-in type in a typed node test is fine statically (§3.13: a basic
    // processor's in-scope schema types include the built-in types), and it
    // is matched: an untyped node's
    // annotation is `xs:untyped` / `xs:untypedAtomic` (XPath 2.0 §2.5.1), which
    // derives from neither `xs:integer` nor `xs:string`, so neither rule fires.
    for pattern in [
        "element(*, xs:integer)",
        "attribute::attribute(*, xs:string)",
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:apply-templates select="//(*|@*)"/></out></xsl:template>
               <xsl:template match="{pattern}">HIT</xsl:template>"#
        );
        let result = transform(&body, r#"<r b="1"><a/></r>"#)
            .unwrap_or_else(|e| panic!("pattern {pattern}: {e}"));
        assert!(!result.contains("HIT"), "pattern {pattern}: {result}");
    }
}

// ═══ A NOTIMPL names what would provide the feature ═══════════════════════════

/// Every `XSLT2-NOTIMPL` names what would provide the feature, and only that —
/// `… is not implemented (icu-collation)`. One row per producer.
#[test]
fn every_notimpl_names_what_would_provide_it() {
    // The only rows are the UCA collations, which a build with the
    // `icu-collation` feature implements: that build checks no row.
    #[cfg(not(feature = "icu-collation"))]
    let template =
        |content: &str| format!(r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#);
    let cases: Vec<(String, &str, &str)> = vec![
        // The instruction and declaration tables, the output layer, the §16
        // evaluator and the pattern parser have no row: every construct they
        // recognize is implemented. An extension instruction without
        // `xsl:fallback` is XTDE1450 (the next test).
        // stylesheet.rs: a UCA collation as `[xsl:]default-collation`. A
        // build with the `icu-collation` feature implements them
        // (`tests/uca_collations.rs`).
        #[cfg(not(feature = "icu-collation"))]
        (
            r#"<xsl:template match="/" default-collation="http://www.w3.org/2013/collation/UCA?strength=secondary"><out/></xsl:template>"#
                .to_string(),
            "the collation",
            "icu-collation",
        ),
        // runtime/sort.rs: the same collation named by xsl:sort.
        #[cfg(not(feature = "icu-collation"))]
        (
            template(
                r#"<xsl:for-each select="(1, 2)"><xsl:sort collation="http://www.w3.org/2013/collation/UCA?lang=en"/></xsl:for-each>"#,
            ),
            "the collation",
            "icu-collation",
        ),
    ];
    for (body, feature, owner) in cases {
        let error = transform(&body, "<r/>").expect_err(&format!("{feature}: unexpectedly ran"));
        assert!(
            error.is_not_implemented(),
            "{feature}: expected XSLT2-NOTIMPL, got {error}"
        );
        assert!(
            error.message.contains(feature),
            "{feature}: the message does not name the feature: {error}"
        );
        assert!(
            error
                .message
                .ends_with(&format!(" is not implemented ({owner})")),
            "{feature}: expected the owner {owner} and nothing else, got {:?}",
            error.message
        );
    }
}

/// §18.2.3's extension instruction with no `xsl:fallback` is no missing
/// feature: "it is a non-recoverable dynamic error if it has no xsl:fallback
/// children" (XTDE1450; more in `tests/compatibility.rs`).
#[test]
fn an_extension_instruction_without_fallback_is_xtde1450() {
    let body = r#"<xsl:template match="/"><out><my:inst xsl:extension-element-prefixes="my"/></out></xsl:template>"#;
    let error = transform(body, "<r/>").expect_err("the extension instruction ran");
    assert!(!error.is_not_implemented(), "{error}");
    assert_eq!(error.code, "XTDE1450", "{error}");
    assert!(error.message.contains("my:inst"), "{:?}", error.message);
}
