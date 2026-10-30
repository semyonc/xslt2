//! M2k — what a build **without** the `icu-collation` cargo feature does with
//! the UCA collations and with `xsl:sort lang` / `case-order` (DESIGN §9.5):
//! exactly what it did before M2k. The feature-on behaviour is in
//! `tests/m2k_collations.rs`.
//!
//! ROADMAP §5.1 rule 1: "Anything outside it raises `XSLT2-NOTIMPL` naming the
//! milestone that owns it." A UCA URI names a collation this build cannot
//! provide, so every consumer that reports an unrecognized collation with an
//! XSLT error code reports it as `XSLT2-NOTIMPL` owned by M2k instead —
//! including the URIs that a build with the feature would reject (they are
//! never parsed here).

#![cfg(not(feature = "icu-collation"))]

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const UCA: &str = "http://www.w3.org/2013/collation/UCA";

fn run(sheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///m2k/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///m2k/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///m2k/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

fn sheet(body: &str) -> String {
    format!(r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">{body}</xsl:stylesheet>"#)
}

fn eval(content: &str) -> Result<String, XsltError> {
    run(
        &sheet(&format!(
            r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
        )),
        "<doc/>",
    )
}

fn assert_m2k(result: Result<String, XsltError>, what: &str) {
    let error = result.expect_err(what);
    assert!(error.is_not_implemented(), "{what}: {error}");
    assert!(
        error
            .message
            .ends_with(" is not implemented (icu-collation)"),
        "{what}: {:?}",
        error.message
    );
}

/// The registry does not recognize the family at all.
#[test]
fn the_registry_does_not_recognize_uca_uris() {
    let functions = XsltFunctions::new();
    for uri in [
        UCA.to_string(),
        format!("{UCA}?lang=en"),
        format!("{UCA}?strength=secondary"),
    ] {
        assert!(!functions.collations().recognizes(&uri), "{uri}");
    }
}

/// Every XSLT consumer of a collation URI: `[xsl:]default-collation`
/// (otherwise XTSE0125), `xsl:sort collation` (XTDE1035), `xsl:key collation`
/// (XTSE1210), `xsl:for-each-group collation` (XTDE1110) — whatever the
/// parameters, `fallback=no` and ill-formed queries included.
#[test]
fn every_consumer_reports_uca_uris_as_m2k() {
    for query in [
        "",
        "?lang=en",
        "?strength=secondary",
        "?fallback=no;reorder=Grek",
        "?x",
    ] {
        let uri = format!("{UCA}{query}");
        assert_m2k(
            run(
                &sheet(&format!(
                    r#"<xsl:template match="/" default-collation="{uri}"><out/></xsl:template>"#
                )),
                "<doc/>",
            ),
            &format!("default-collation {uri}"),
        );
        assert_m2k(
            eval(&format!(
                r#"<xsl:for-each select="('b', 'a')"><xsl:sort select="." collation="{uri}"/></xsl:for-each>"#
            )),
            &format!("xsl:sort {uri}"),
        );
        assert_m2k(
            run(
                &sheet(&format!(
                    r#"<xsl:key name="k" match="*" use="." collation="{uri}"/><xsl:template match="/"><out/></xsl:template>"#
                )),
                "<doc/>",
            ),
            &format!("xsl:key {uri}"),
        );
        assert_m2k(
            eval(&format!(
                r#"<xsl:for-each-group select="('b', 'a')" group-by="." collation="{uri}"><x/></xsl:for-each-group>"#
            )),
            &format!("xsl:for-each-group {uri}"),
        );
    }
    // An unknown URI is still the context's own error.
    let result = eval(
        r#"<xsl:for-each select="('b', 'a')"><xsl:sort select="." collation="urn:none"/></xsl:for-each>"#,
    );
    assert_eq!(result.unwrap_err().code, "XTDE1035");
}

/// The XPath functions reach the registry through the engine's resolver, which
/// reports an unsupported collation as FOCH0002 (F&O §7.3.1) — unchanged.
#[test]
fn xpath_functions_report_foch0002() {
    let result = eval(&format!(
        r#"<xsl:value-of select="compare('a', 'b', '{UCA}?lang=en')"/>"#
    ));
    assert_eq!(result.unwrap_err().code, "FOCH0002");
}

/// §13.1.3: in the absence of a collation attribute, `lang` and `case-order`
/// "provide input to an implementation-defined algorithm to locate a suitable
/// collation" — without the feature `CaseOrderCollation` for every language:
/// case-blind first, then case, and no accent tailoring (`ä` after `z` in
/// German too, by codepoint).
#[test]
fn sort_lang_uses_the_case_order_collation() {
    let sorted = |attrs: &str| {
        eval(&format!(
            r#"<xsl:value-of separator=","><xsl:perform-sort select="('zebra', 'ärger', 'Apple', 'apple')"><xsl:sort select="." {attrs}/></xsl:perform-sort></xsl:value-of>"#
        ))
        .unwrap()
    };
    assert_eq!(sorted(r#"lang="de""#), "<out>apple,Apple,zebra,ärger</out>");
    assert_eq!(sorted(r#"lang="sv""#), "<out>apple,Apple,zebra,ärger</out>");
    assert_eq!(
        sorted(r#"case-order="upper-first""#),
        "<out>Apple,apple,zebra,ärger</out>"
    );
}
