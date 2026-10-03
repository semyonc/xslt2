//! Regression tests for M2a — Groundwork.
//!
//! M2a adds no features, so every test here pins a *classification* or an
//! invariant that the groundwork is supposed to make impossible to break:
//!
//! * Phase 2 — §3.12 `use-when` exclusion now happens in one filtered child
//!   iterator, so the traversals that used to carry their own check still
//!   exclude. Each case has a negative control that shows the check is load
//!   bearing: remove `use-when` and the compilation fails.
//! * Phase 3 — the §16 functions this processor does not implement are in the
//!   function catalog as `XSLT2-NOTIMPL` stubs, so a call to one is reported as
//!   a missing feature rather than as an unknown function; and the untyped type
//!   tests a basic processor can decide in a pattern are decided.
//!
//! The helper mirrors `m1_review.rs`: an in-memory resolver serving
//! `file:///m2a/in.xml`, a stylesheet compiled at `file:///m2a/main.xsl`, and
//! the serialized primary result.

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// `xs` and `my` are bound (and excluded from the result) because the Phase 3
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
    mem.insert("file:///m2a/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet(body), "file:///m2a/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///m2a/in.xml", source)?;
    compiled.serialize_primary(&outcome)
}

// ═══ Phase 2 — §3.12 exclusion by construction ═══════════════════════════

/// `Builder::reject_sort` is the traversal with the least coverage: it looks
/// for one element kind and, finding it, raises `XSLT2-NOTIMPL` for M2d. It
/// used to carry its own `!self.excluded(..)`; it now goes through
/// `Builder::children`, and an excluded `xsl:sort` must still be invisible.
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

/// The negative control: without `use-when` the very same stylesheet stops
/// compiling, which is what makes the test above meaningful.
#[test]
fn a_sort_that_is_not_excluded_is_still_rejected() {
    let error = transform(
        r#"<xsl:template match="/">
             <out><xsl:for-each select="r/i">
               <xsl:sort select="."/>
               <xsl:value-of select="."/>
             </xsl:for-each></out>
           </xsl:template>"#,
        "<r><i>b</i><i>a</i></r>",
    )
    .unwrap_err();
    assert!(error.is_not_implemented(), "{error}");
    assert!(error.to_string().contains("xsl:sort"), "{error}");
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

// ═══ Phase 3a — the §16 functions that are registered but not implemented ══

/// Every §16 function this processor does not implement is in the catalog with
/// its spec signature, so the call *compiles* and the failure is reported as a
/// missing feature (`XSLT2-NOTIMPL`) naming the milestone that owns it — not as
/// XPST0017, which is indistinguishable from a wrong answer.
#[test]
fn the_unimplemented_16_4_and_16_5_functions_report_their_milestone() {
    // §16.4 format-number/2 and /3; §16.5 format-dateTime, format-date and
    // format-time, each in the 2-argument and the 5-argument form.
    for call in [
        r#"format-number(1.5, '#.#')"#,
        r#"format-number(1.5, '#.#', 'df')"#,
        r#"format-dateTime(xs:dateTime('2002-12-31T12:00:00'), '[Y]')"#,
        r#"format-dateTime(xs:dateTime('2002-12-31T12:00:00'), '[Y]', 'en', (), ())"#,
        r#"format-date(xs:date('2002-12-31'), '[Y]')"#,
        r#"format-date(xs:date('2002-12-31'), '[Y]', 'en', (), ())"#,
        r#"format-time(xs:time('12:00:00'), '[H]')"#,
        r#"format-time(xs:time('12:00:00'), '[H]', 'en', (), ())"#,
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out><xsl:value-of select="{call}"/></out></xsl:template>"#
        );
        let error =
            transform(&body, "<r/>").expect_err(&format!("{call} unexpectedly produced a result"));
        assert!(
            error.is_not_implemented(),
            "{call} should be NOTIMPL, got {error}"
        );
        assert!(
            error.to_string().contains("M3d"),
            "{call} should name M3d, got {error}"
        );
    }
}

/// §18.1.2: `function-available` "returns true if and only if there is a
/// function available with the given name and arity". A stub is registered but
/// is not available, so the answer stays **false** and a stylesheet that guards
/// its use of one still takes the fallback branch.
#[test]
fn function_available_is_false_for_a_stub() {
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
               <!-- A control: one that really is available. -->
               <xsl:value-of select="function-available('generate-id')"/>
             </out>
           </xsl:template>"#,
        "<r/>",
    )
    .unwrap();
    assert_eq!(out, "<out>false,false,false,false|true</out>");
}

/// §3.12 `use-when` is evaluated at compile time by a static evaluator that
/// holds no transformation. A stub must not make one *succeed*: the guard below
/// is false, so the element stays excluded, exactly as it was before the stubs
/// were registered.
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
/// wrong answers rather than as missing features — which is what M2a fixed.
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

// ═══ Phase 3b — untyped type tests in a pattern ═══════════════════════════

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
/// The axis is written out because §5.5.3 adjusts the axis of a first pattern
/// step from an explicit `PatternAxis` only; a bare `attribute(…)` node test
/// keeps the (implicit) `child-or-top` axis, whose principal node kind is
/// element. That is a separate question from the type test.
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

/// Any other type name needs a schema and stays not implemented (M4).
#[test]
fn a_type_test_that_needs_a_schema_is_still_not_implemented() {
    // A user-defined type name with no schema behind it is not "schema-aware,
    // later": "TypeName must be present in the in-scope schema types
    // [err:XPST0008]" (XPath 2.0 §2.5.4.3), for any processor. The XPath
    // engine reports it when the pattern is compiled. The pattern compiler
    // still wraps every XPath compile error as XTSE0340, which is meant for a
    // pattern that does not match the Pattern syntax; passing a name error
    // through with its own code is an M2i item — until then the XPST0008 is
    // carried in the message.
    let body = r#"<xsl:template match="/"><out/></xsl:template>
               <xsl:template match="element(a, my:t)">HIT</xsl:template>"#;
    let error = transform(body, "<r><a/></r>").expect_err("element(a, my:t) unexpectedly compiled");
    assert!(!error.is_not_implemented(), "{error}");
    assert!(error.to_string().contains("XPST0008"), "{error}");

    // A built-in type in a typed node test is fine statically; matching it
    // needs typed nodes, which arrive with the schema-aware milestone.
    for pattern in [
        "element(*, xs:integer)",
        "attribute::attribute(*, xs:string)",
    ] {
        let body = format!(
            r#"<xsl:template match="/"><out/></xsl:template>
               <xsl:template match="{pattern}">HIT</xsl:template>"#
        );
        let error =
            transform(&body, "<r><a/></r>").expect_err(&format!("{pattern} unexpectedly compiled"));
        assert!(
            error.is_not_implemented() && error.to_string().contains("M4"),
            "pattern {pattern}: {error}"
        );
    }
}

// ═══ X1 item 1 — a NOTIMPL names the sub-milestone that owns the feature ═══

/// Every `XSLT2-NOTIMPL` names the one ROADMAP §4 sub-milestone that owns the
/// feature, and only that — `… is not implemented (M2d)`, never a coarse `M2`
/// or `M3`, and never "in M1", which stopped being true when M2a landed. One
/// row per producer: the instruction and declaration tables, the output layer,
/// the function evaluator, the pattern parser and `[xsl:]default-collation`.
#[test]
fn every_notimpl_names_its_sub_milestone() {
    let template =
        |content: &str| format!(r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#);
    let cases: Vec<(String, &str, &str)> = vec![
        // compile/instructions.rs: the instruction table.
        (
            template(r#"<xsl:for-each select="*"><xsl:sort/></xsl:for-each>"#),
            "xsl:sort",
            "M2d",
        ),
        (
            template(r#"<xsl:perform-sort select="1"><xsl:sort/></xsl:perform-sort>"#),
            "xsl:perform-sort",
            "M2d",
        ),
        (
            template(r#"<xsl:for-each-group select="*" group-by="."/>"#),
            "xsl:for-each-group",
            "M2g",
        ),
        (
            template(r#"<xsl:analyze-string select="'a'" regex="a"/>"#),
            "xsl:analyze-string",
            "M2h",
        ),
        (template(r#"<xsl:number/>"#), "xsl:number", "M3c"),
        (
            template(r#"<xsl:result-document href="x.xml"/>"#),
            "xsl:result-document",
            "M3b",
        ),
        // compile/declarations.rs: the declaration table.
        (
            format!(r#"<xsl:key name="k" match="*" use="."/>{}"#, template("")),
            "xsl:key",
            "M2f",
        ),
        (
            format!(
                r#"<xsl:function name="my:f"><xsl:sequence select="1"/></xsl:function>{}"#,
                template("")
            ),
            "xsl:function",
            "M2e",
        ),
        (
            format!(r#"<xsl:decimal-format name="d"/>{}"#, template("")),
            "xsl:decimal-format",
            "M3d",
        ),
        (
            format!(r#"<xsl:character-map name="c"/>{}"#, template("")),
            "xsl:character-map",
            "M3a",
        ),
        // output.rs: methods, doctype, encodings.
        (
            format!(r#"<xsl:output method="html"/>{}"#, template("")),
            "the html output method",
            "M3a",
        ),
        (
            format!(r#"<xsl:output doctype-system="x.dtd"/>{}"#, template("")),
            "xsl:output/@doctype-*",
            "M3a",
        ),
        (
            format!(r#"<xsl:output encoding="EBCDIC-X"/>{}"#, template("")),
            "the output encoding",
            "M3a",
        ),
        // runtime/functions.rs: the §16 evaluator.
        (
            template(r#"<xsl:value-of select="key('k', 'v')"/>"#),
            "key()",
            "M2f",
        ),
        (
            template(r#"<xsl:value-of select="current-group()"/>"#),
            "current-group()",
            "M2g",
        ),
        (
            template(r#"<xsl:value-of select="current-grouping-key()"/>"#),
            "current-grouping-key()",
            "M2g",
        ),
        (
            template(r#"<xsl:value-of select="regex-group(1)"/>"#),
            "regex-group()",
            "M2h",
        ),
        (
            template(r#"<xsl:copy-of select="document('in.xml#frag')"/>"#),
            "a fragment identifier in document()",
            "M2i",
        ),
        // pattern.rs: the key()/id() pattern.
        (
            format!(
                r#"<xsl:template match="key('k', 'v')">HIT</xsl:template>{}"#,
                template("")
            ),
            "the id()/key() pattern",
            "M2f",
        ),
        // stylesheet.rs: a standard collation other than codepoint (§9.5).
        (
            r#"<xsl:template match="/" default-collation="http://www.w3.org/2005/xpath-functions/collation/html-ascii-case-insensitive"><out/></xsl:template>"#
                .to_string(),
            "the collation",
            "M2d",
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

/// §18.2.3's extension instruction with no `xsl:fallback` is M3e's.
#[test]
fn an_extension_instruction_without_fallback_names_m3e() {
    let body = r#"<xsl:template match="/"><out><my:inst xsl:extension-element-prefixes="my"/></out></xsl:template>"#;
    let error = transform(body, "<r/>").expect_err("the extension instruction ran");
    assert!(error.is_not_implemented(), "{error}");
    assert!(
        error.message.ends_with(" is not implemented (M3e)"),
        "{:?}",
        error.message
    );
}
