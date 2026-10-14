//! `fn:error` (F&O §3), registered by this processor.
//!
//! The XPath engine ships the implementation but deliberately leaves the
//! function out of its built-in catalog, so `XsltFunctions` registers it for
//! arities 0 to 3 and `XsltEvaluator` calls the engine's function through.
//!
//! What these tests pin:
//!
//! * the function binds at every arity it accepts, and
//!   `function-available('error')` is true;
//! * the error QName's **local name** becomes the XSLT error code — the
//!   default QName gives `FOER0000`, a user QName gives its own local name —
//!   and the `$description` argument becomes the message;
//! * the QName's **namespace URI** and the error's [`ErrorOrigin`] travel with
//!   it, so an error the stylesheet raised is a dynamic error whatever its
//!   local name is spelled like, and §5.5.4's recovery applies to it;
//! * a first argument that is not an `xs:QName` is `XPTY0004`, not an error
//!   the stylesheet asked for, and the declared argument types are enforced.
//!
//! The helper mirrors `m2a_groundwork.rs`: an in-memory resolver serving
//! `file:///e/in.xml`, a stylesheet compiled at `file:///e/main.xsl`, and the
//! serialized primary result.

mod common;

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, ErrorOrigin, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const SOURCE: &str = "<r/>";
/// A source document with one `item`, for the pattern-recovery tests.
const ONE_ITEM: &str = r#"<r><item n="x"/></r>"#;

fn sheet(body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
           exclude-result-prefixes=\"xs\">{body}</xsl:stylesheet>"
    )
}

fn transform(body: &str) -> Result<String, XsltError> {
    run(body, SOURCE, false)
}

/// Runs one stylesheet over one source document, with or without
/// `strict_recoverable`, and returns the serialized primary result.
fn run(body: &str, source: &str, strict: bool) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///e/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet(body), "file:///e/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let options = TransformOptions {
        strict_recoverable: strict,
        ..TransformOptions::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .with_resolver(resolver)
        .transform_source("file:///e/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// A stylesheet with a high-priority rule carrying `predicate` and a plain
/// `item` rule below it. If the predicate's error is recovered from, the
/// fallback rule wins and the result says so.
fn two_rules(predicate: &str) -> String {
    format!(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="r/item"/></out></xsl:template>
           <xsl:template match="item[{predicate}]" xmlns:e="urn:e" xmlns:n="urn:n">PREDICATE</xsl:template>
           <xsl:template match="item">FALLBACK</xsl:template>"#
    )
}

/// Run a template body that calls `error` and return the resulting error.
fn raise(select: &str) -> XsltError {
    let body = format!(
        r#"<xsl:template match="/"><z><xsl:value-of select="{select}"/></z></xsl:template>"#
    );
    match transform(&body) {
        Err(e) => e,
        Ok(out) => panic!("{select} produced {out} instead of raising"),
    }
}

#[test]
fn error_with_no_arguments_reports_foer0000() {
    let err = raise("error()");
    assert_eq!(err.code, "FOER0000");
    // The description of the no-argument form is the error code's own URI.
    assert!(
        err.message.contains("FOER0000"),
        "message was {:?}",
        err.message
    );
}

#[test]
fn error_reports_the_description_as_its_message() {
    let err = raise("error((), 'application error - p=2')");
    assert_eq!(err.code, "FOER0000");
    assert_eq!(err.message, "application error - p=2");
}

#[test]
fn a_user_error_qname_becomes_the_error_code() {
    let err = raise("error(QName('urn:e', 'e:boom'), 'why')");
    assert_eq!(err.code, "boom");
    assert_eq!(err.message, "why");

    // An error QName in no namespace works the same way.
    let err = raise("error(QName('', 'bare'), 'plain')");
    assert_eq!(err.code, "bare");
    assert_eq!(err.message, "plain");
}

/// F&O §3 identifies the error by an `xs:QName` — namespace **and** local name
/// — so both halves have to survive the trip into `XsltError`, and both have to
/// be readable through the public API.
#[test]
fn the_whole_error_qname_is_observable() {
    let err = raise("error(QName('urn:e', 'e:boom'), 'why')");
    assert_eq!(err.code, "boom");
    assert_eq!(err.error_namespace(), Some("urn:e"));
    assert_eq!(err.error_qname(), Some(("urn:e", "boom")));
    assert_eq!(err.origin(), ErrorOrigin::Raised);
    assert!(err.is_raised());
    // Display shows the QName whole, in the EQName form the catalogues use
    // (a stylesheet location may follow).
    assert!(
        err.to_string().starts_with("Q{urn:e}boom: why"),
        "{}",
        err.to_string()
    );

    // Two errors with the same local name in different namespaces stay
    // distinguishable.
    let other = raise("error(QName('urn:n', 'n:boom'), 'why')");
    assert_eq!(other.code, err.code);
    assert_ne!(other.error_namespace(), err.error_namespace());

    // An error QName in no namespace: `Some("")`, not `None`, and printed bare.
    let bare = raise("error(QName('', 'bare'), 'plain')");
    assert_eq!(bare.error_namespace(), Some(""));
    assert_eq!(bare.error_qname(), Some(("", "bare")));
    assert!(bare.is_raised());
    assert!(
        bare.to_string().starts_with("bare: plain"),
        "{}",
        bare.to_string()
    );

    // `fn:error`'s own default QName is in the standard namespace, whose codes
    // this crate prints bare, so the display of an ordinary coded error is
    // unchanged.
    let default = raise("error((), 'oops')");
    assert_eq!(
        default.error_namespace(),
        Some("http://www.w3.org/2005/xqt-errors")
    );
    assert!(
        default.to_string().starts_with("FOER0000: oops"),
        "{}",
        default.to_string()
    );

    // A processor error carries no error QName at all.
    let processor = XsltError::new("XTDE0040", "no such template");
    assert_eq!(processor.error_namespace(), None);
    assert_eq!(processor.error_qname(), None);
    assert_eq!(processor.origin(), ErrorOrigin::Processor);
    assert_eq!(processor.to_string(), "XTDE0040: no such template");
}

/// §5.5.4: "Any dynamic error or type error that occurs during the evaluation
/// of a pattern against a particular node is treated as a recoverable error,
/// even if the error would normally be non-recoverable. The optional recovery
/// action is to treat the pattern as not matching."
///
/// An error raised by `fn:error` **is** a dynamic error, so the rule applies to
/// it however the stylesheet spelled its QName. Classifying it by the spelling
/// of the local name — as a code of no known family, hence
/// [`ErrorKind::Other`] — used to keep the recovery from happening at all.
#[test]
fn a_raised_error_in_a_predicate_is_recovered() {
    for predicate in [
        "error(QName('urn:e', 'e:boom'), 'why')",
        "error(QName('', 'bare'), 'why')",
        "error()",
        "error((), 'why')",
    ] {
        assert_eq!(
            run(&two_rules(predicate), ONE_ITEM, false).unwrap(),
            "<out>FALLBACK</out>",
            "{predicate}: the recovery action is to treat the pattern as not matching"
        );
    }
}

/// The other half: `strict_recoverable` signals the error instead of recovering,
/// and it arrives with its identity intact.
#[test]
fn strict_recoverable_signals_a_raised_error_with_its_namespace() {
    let err = run(
        &two_rules("error(QName('urn:e', 'e:boom'), 'why')"),
        ONE_ITEM,
        true,
    )
    .expect_err("strict_recoverable must signal it instead of recovering");
    assert_eq!(err.code, "boom");
    assert_eq!(err.error_namespace(), Some("urn:e"));
    assert_eq!(err.message, "why");
    assert!(err.is_raised());
    assert!(!err.is_processor_failure());
}

/// The defect this test exists for: a stylesheet may name its own error
/// anything, including the two codes this crate uses for its *own* failures.
/// Classifying by spelling turned such an error into "the processor does not
/// implement this" or "a broken invariant", which escapes §5.5.4's recovery and
/// misreports the case in the conformance harness.
#[test]
fn a_user_error_named_like_an_internal_code_is_still_a_dynamic_error() {
    for local in ["XSLT2-NOTIMPL", "XSLT2-INTERNAL", "XTMM9000"] {
        let select = format!("error(QName('urn:e', 'e:{local}'), 'why')");

        let err = raise(&select);
        assert_eq!(err.code, local);
        assert_eq!(err.error_namespace(), Some("urn:e"));
        assert!(err.is_raised(), "{local}");
        assert!(
            !err.is_not_implemented(),
            "{local}: the stylesheet raised this, not the processor"
        );
        assert!(!err.is_internal(), "{local}");
        assert!(!err.is_termination(), "{local}");
        assert!(!err.is_processor_failure(), "{local}");
        assert!(
            err.is_recoverable_in_pattern(),
            "{local}: a raised error is a dynamic error"
        );

        // …and so it is recovered in a pattern, like any other dynamic error.
        assert_eq!(
            run(&two_rules(&select), ONE_ITEM, false).unwrap(),
            "<out>FALLBACK</out>",
            "{local}"
        );
    }
}

/// The guard must not swing the other way: the **processor's** own missing
/// feature still propagates out of a pattern predicate, in both modes.
#[test]
fn the_processors_own_not_implemented_still_propagates() {
    for strict in [false, true] {
        // An extension instruction without `xsl:fallback` (M3e), reached through
        // a stylesheet function the predicate calls, stands for a feature this
        // processor still lacks: `format-number()` did until M3d implemented it
        // (`tests/m3d_formatting.rs`), and `key()` until M2f.
        let body = format!("{}{MISSING_FEATURE_FUNCTION}", two_rules("e:missing(.)"));
        let err = match run(&body, ONE_ITEM, strict) {
            Err(e) => e,
            Ok(out) => panic!("strict={strict} produced {out}"),
        };
        assert!(err.is_not_implemented(), "strict={strict}: {err}");
        assert!(err.is_processor_failure(), "strict={strict}");
        assert!(!err.is_raised(), "strict={strict}");
        assert!(!err.is_recoverable_in_pattern(), "strict={strict}");
    }

    // The two constructors keep their meaning.
    assert!(XsltError::not_implemented("format-number()", "M3d").is_not_implemented());
    assert!(XsltError::internal("broken invariant").is_internal());
    assert!(!XsltError::not_implemented("format-number()", "M3d").is_raised());
}

/// A stylesheet function `e:missing` (namespace `urn:e`, bound on the
/// predicate's template by [`two_rules`]) whose body evaluates an extension
/// instruction this processor does not implement and that has no
/// `xsl:fallback` — `XSLT2-NOTIMPL` (M3e).
const MISSING_FEATURE_FUNCTION: &str = r#"<xsl:function name="e:missing" xmlns:e="urn:e">
       <xsl:param name="n"/>
       <ext:instruction xmlns:ext="urn:ext" xsl:extension-element-prefixes="ext"/>
       <xsl:sequence select="true()"/>
     </xsl:function>"#;

/// `xsl:message terminate="yes"` does not go through `fn:error` — the
/// instruction builds `XTMM9000` directly — so it is a processor error and stays
/// a termination, while a stylesheet that raises its own `XTMM9000` through
/// `fn:error` does not reach the termination path.
#[test]
fn a_termination_is_not_reachable_through_fn_error() {
    let terminating = r#"<xsl:template match="/">
          <xsl:message terminate="yes">stop</xsl:message>
        </xsl:template>"#;
    let err = transform(terminating).expect_err("terminate=\"yes\" ends the transformation");
    assert_eq!(err.code, "XTMM9000");
    assert!(err.is_termination());
    assert!(!err.is_raised());

    let raised = raise("error(QName('urn:e', 'e:XTMM9000'), 'not a termination')");
    assert_eq!(raised.code, "XTMM9000");
    assert!(!raised.is_termination());
}

#[test]
fn the_error_object_argument_is_accepted() {
    let err = raise("error((), 'with an object', (1, 2, 3))");
    assert_eq!(err.code, "FOER0000");
    assert_eq!(err.message, "with an object");
}

#[test]
fn a_first_argument_that_is_not_a_qname_is_a_type_error() {
    // `fn:error($error as xs:QName, …)`: a string does not match, so the
    // failure is the stylesheet's type error, not the error it asked for.
    let err = raise("error('application error - p=2')");
    assert_eq!(err.code, "XPTY0004");
    assert!(!err.is_raised(), "a type error is the processor's own");
}

/// The declared argument types and cardinalities, enforced by the engine's
/// implementation: `fn:error($error as xs:QName)` in the one-argument form, and
/// `$description as xs:string` — no occurrence indicator — in the other two.
#[test]
fn the_declared_argument_types_are_enforced() {
    for select in [
        // Only the two- and three-argument forms accept an empty `$error`.
        "error(())",
        // `$description` is a required single `xs:string`.
        "error((), 42)",
        "error((), ())",
        "error(QName('urn:e', 'e:boom'), 42)",
        "error((), 42, 'obj')",
    ] {
        let err = raise(select);
        assert_eq!(err.code, "XPTY0004", "{select}: {err}");
        assert!(!err.is_raised(), "{select}");
    }

    // The empty `$error` of the two- and three-argument forms still gives the
    // default QName.
    assert_eq!(raise("error((), 'why')").code, "FOER0000");
    assert_eq!(raise("error((), 'why', 1)").code, "FOER0000");
}

#[test]
fn error_is_available_at_every_registered_arity() {
    // `function-available()` (§18.1.2) must say yes, and each arity must bind.
    let body = r#"<xsl:template match="/"><z>
          <a><xsl:value-of select="function-available('error')"/></a>
          <b><xsl:value-of select="if (false()) then error() else 'ok0'"/></b>
          <c><xsl:value-of select="if (false()) then error(QName('urn:e','boom')) else 'ok1'"/></c>
          <d><xsl:value-of select="if (false()) then error((), 'x') else 'ok2'"/></d>
          <e><xsl:value-of select="if (false()) then error((), 'x', 1) else 'ok3'"/></e>
        </z></xsl:template>"#;
    let out = transform(body).expect("every arity binds");
    assert!(out.contains("<a>true</a>"), "{out}");
    for expected in ["<b>ok0</b>", "<c>ok1</c>", "<d>ok2</d>", "<e>ok3</e>"] {
        assert!(out.contains(expected), "{expected} missing from {out}");
    }
}

#[test]
fn a_fourth_argument_is_an_unknown_function() {
    // Only arities 0 to 3 are registered.
    let err = raise("error((), 'x', 1, 2)");
    assert_eq!(err.code, "XPST0017");
}
