//! The stack guard everywhere a compilation or a run recurses: deep nesting in
//! a stylesheet, a long chain of `xsl:include`, and a deep source document end
//! in an error the caller gets back, or succeed, on a thread with Rust's
//! default 2 MiB stack — never in a stack overflow, which aborts the process.
//!
//! XSLT 2.0 §21: "A conforming processor may impose limits on the processing
//! resources consumed by the processing of a stylesheet." The limit is the
//! stack guard's (`TransformOptions::stack_limit`, and on a thread the run
//! cannot measure `OTHER_THREAD_STACK_LIMIT`), and the error is the guard's
//! `XSLT2-INTERNAL`.
//!
//! An overflow would take the whole test binary down, so each deep case runs
//! in a child process: the test binary started again for this one test, with
//! the case named in an environment variable. The parent only reads the
//! child's exit status.
mod common;

use std::process::Command;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xslt2::{
    Compiler, MemoryResolver, SchemaContext, SourceValidation, TransformOptions, XsltError,
    XsltFunctions,
};

/// The environment variable that names the case a child process runs.
const CASE: &str = "XSLT2_STACK_GUARD_CASE";

/// Rust's default stack for a spawned thread.
const TWO_MIB: usize = 2 * 1024 * 1024;

const SHEET_URI: &str = "http://example.com/s.xsl";
const SCHEMA_URI: &str = "http://example.com/s.xsd";

/// `n` levels of `open` … `close` around `inner`.
fn nest(n: usize, open: &str, inner: &str, close: &str) -> String {
    format!("{}{inner}{}", open.repeat(n), close.repeat(n))
}

fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">{body}</xsl:stylesheet>"#
    )
}

/// An element `e` nested `n` deep in `d`, with `between` around each level.
fn deep_source(n: usize, between: &str) -> String {
    format!(
        "<d>{}</d>",
        nest(
            n,
            &format!("<e>{between}"),
            "<e>t</e>",
            &format!("{between}</e>")
        )
    )
}

/// A schema for [`deep_source`]: `d` and `e` of one recursive, mixed type.
const RECURSIVE_SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="d" type="T"/>
  <xs:element name="e" type="T"/>
  <xs:complexType name="T" mixed="true">
    <xs:sequence><xs:element ref="e" minOccurs="0" maxOccurs="unbounded"/></xs:sequence>
  </xs:complexType>
</xs:schema>"#;

/// Compiles `text` (served with `extra` by a `MemoryResolver`), in schema-aware
/// mode with [`RECURSIVE_SCHEMA`] when `schema_aware`, runs it over `source`
/// and serializes the primary result.
fn transform(
    text: &str,
    extra: &[(String, String)],
    source: &str,
    schema_aware: bool,
    options: TransformOptions,
) -> Result<String, XsltError> {
    let mut resolver = MemoryResolver::new()
        .with(SHEET_URI, text)
        .with(SCHEMA_URI, RECURSIVE_SCHEMA);
    for (uri, text) in extra {
        resolver = resolver.with(uri, text);
    }
    let resolver = Arc::new(resolver);
    let functions = XsltFunctions::new();
    let names = NameTable::new();
    let context;
    let compiler = if schema_aware {
        context = SchemaContext::builder()
            .with_resolver(resolver.clone())
            .stylesheet_uri(SHEET_URI, &functions)?
            .build()?;
        Compiler::schema_aware(&context, &functions)
    } else {
        Compiler::new(&names, &functions).with_resolver(resolver)
    };
    let compiled = compiler.compile_uri(SHEET_URI)?;
    let arena = Bump::new();
    let run = compiled.transformation(&arena, options);
    let outcome = run.transform_source("urn:in", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// Checks that `result` is the stack guard's error.
fn expect_guard_error(result: Result<String, XsltError>) -> Result<(), String> {
    match result {
        Ok(out) => Err(format!(
            "expected the stack guard's error, got a result of {} bytes",
            out.len()
        )),
        Err(e) if e.code == "XSLT2-INTERNAL" && e.message.contains("bytes of stack") => Ok(()),
        Err(e) => Err(format!("expected the stack guard's error, got {e}")),
    }
}

/// Checks that `result` is `Ok` and that `check` accepts it.
fn expect_result(
    result: Result<String, XsltError>,
    check: impl Fn(&str) -> bool,
) -> Result<(), String> {
    match result {
        Ok(out) if check(&out) => Ok(()),
        Ok(out) => Err(format!(
            "unexpected result: {}…",
            out.chars().take(200).collect::<String>()
        )),
        Err(e) => Err(format!("expected a result, got {e}")),
    }
}

/// The deep cases, by name: each is deep enough to overflow a 2 MiB stack
/// without the guard (in a debug and in a release build) and either ends in
/// the guard's error or, for a walk over a tree that needs no recursion,
/// succeeds.
const DEEP_CASES: &[&str] = &[
    "compiling nested instructions",
    "running nested literal result elements",
    "a long chain of xsl:include",
    "xsl:copy-of of a deep document",
    "a deep document copied into a variable",
    "a deep result serialized as html and as text",
    "xsl:strip-space over a deep document",
    "xsl:copy-of with validation=\"strict\" of a deep document",
    "xsl:strip-space over a deep typed document",
];

/// The depth of the deep source documents.
const DOCUMENT_DEPTH: usize = 20_000;

fn deep_case(case: &str) -> Result<(), String> {
    let plain = TransformOptions::default;
    let strict = || TransformOptions {
        source_validation: SourceValidation::Strict,
        ..TransformOptions::default()
    };
    let n = DOCUMENT_DEPTH;
    // The serialized deep document: `<d>`, n `<e>`, the innermost `<e>t</e>`.
    let copied = |out: &str| {
        out.starts_with("<d><e><e>")
            && out.ends_with("</e></e></d>")
            && out.matches("<e>").count() == n + 1
    };
    match case {
        // Never called: only the compilation nests.
        "compiling nested instructions" => expect_guard_error(transform(
            &sheet(&format!(
                r#"<xsl:template match="/"><out/></xsl:template>
                   <xsl:template name="unused">{}</xsl:template>"#,
                nest(1_000, r#"<xsl:if test="true()">"#, "x", "</xsl:if>")
            )),
            &[],
            "<d/>",
            false,
            plain(),
        )),
        "running nested literal result elements" => expect_guard_error(transform(
            &sheet(&format!(
                r#"<xsl:template match="/">{}</xsl:template>"#,
                nest(300, "<e>", "x", "</e>")
            )),
            &[],
            "<d/>",
            false,
            plain(),
        )),
        "a long chain of xsl:include" => {
            let module = |include: &str| sheet(include);
            let modules: Vec<(String, String)> = (1..=2_000)
                .map(|i| {
                    (
                        format!("http://example.com/m{i}.xsl"),
                        module(&format!(r#"<xsl:include href="m{}.xsl"/>"#, i + 1)),
                    )
                })
                .chain(std::iter::once((
                    "http://example.com/m2001.xsl".to_string(),
                    module(""),
                )))
                .collect();
            expect_guard_error(transform(
                &sheet(
                    r#"<xsl:include href="m1.xsl"/><xsl:template match="/"><out/></xsl:template>"#,
                ),
                &modules,
                "<d/>",
                false,
                plain(),
            ))
        }
        "xsl:copy-of of a deep document" => expect_result(
            transform(
                &sheet(r#"<xsl:template match="/"><xsl:copy-of select="."/></xsl:template>"#),
                &[],
                &deep_source(n, ""),
                false,
                plain(),
            ),
            copied,
        ),
        "a deep document copied into a variable" => expect_result(
            transform(
                &sheet(
                    r#"<xsl:template match="/">
                         <xsl:variable name="copy"><xsl:sequence select="/d"/></xsl:variable>
                         <out><xsl:value-of select="count($copy//e)"/></out></xsl:template>"#,
                ),
                &[],
                &deep_source(n, ""),
                false,
                plain(),
            ),
            |out| out == format!("<out>{}</out>", n + 1),
        ),
        "a deep result serialized as html and as text" => {
            for (method, check) in [
                ("html", &copied as &dyn Fn(&str) -> bool),
                ("text", &|out: &str| out == "t"),
            ] {
                expect_result(
                    transform(
                        &sheet(&format!(
                            r#"<xsl:output method="{method}" indent="no"/>
                               <xsl:template match="/"><xsl:copy-of select="."/></xsl:template>"#
                        )),
                        &[],
                        &deep_source(n, ""),
                        false,
                        plain(),
                    ),
                    check,
                )
                .map_err(|e| format!("method {method}: {e}"))?;
            }
            Ok(())
        }
        "xsl:strip-space over a deep document" => expect_result(
            transform(
                &sheet(
                    r#"<xsl:strip-space elements="*"/>
                       <xsl:template match="/"><out><xsl:value-of select="count(//text())"/></out></xsl:template>"#,
                ),
                &[],
                &deep_source(n, " "),
                false,
                plain(),
            ),
            |out| out == "<out>1</out>",
        ),
        "xsl:copy-of with validation=\"strict\" of a deep document" => expect_result(
            transform(
                &sheet(&format!(
                    r#"<xsl:import-schema schema-location="{SCHEMA_URI}"/>
                       <xsl:template match="/">
                         <xsl:variable name="copy"><xsl:copy-of select="d" validation="strict"/></xsl:variable>
                         <out><xsl:value-of select="count($copy//element(e, T))"/></out></xsl:template>"#
                )),
                &[],
                &deep_source(n, ""),
                true,
                plain(),
            ),
            |out| out == format!("<out>{}</out>", n + 1),
        ),
        "xsl:strip-space over a deep typed document" => expect_result(
            transform(
                &sheet(&format!(
                    r#"<xsl:import-schema schema-location="{SCHEMA_URI}"/>
                       <xsl:strip-space elements="*"/>
                       <xsl:template match="/"><out><xsl:value-of select="count(//text())"/></out></xsl:template>"#
                )),
                &[],
                &deep_source(n, " "),
                true,
                strict(),
            ),
            |out| out == "<out>1</out>",
        ),
        other => Err(format!("no case named {other:?}")),
    }
}

/// Every deep case, each in a child process on a 2 MiB thread: the child
/// exits with success only when the case ended as expected, and a child that
/// overflowed its stack is reported with the signal that ended it.
#[test]
fn deep_input_ends_in_an_error_or_a_result_never_in_an_overflow() {
    if let Ok(case) = std::env::var(CASE) {
        let outcome = std::thread::Builder::new()
            .stack_size(TWO_MIB)
            .spawn(move || deep_case(&case))
            .expect("a 2 MiB thread")
            .join()
            .expect("the case does not panic");
        if let Err(message) = outcome {
            panic!("{message}");
        }
        return;
    }
    let me = std::env::current_exe().expect("the test binary");
    let mut failures = Vec::new();
    for case in DEEP_CASES {
        let child = Command::new(&me)
            .args([
                "--exact",
                "deep_input_ends_in_an_error_or_a_result_never_in_an_overflow",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CASE, case)
            .output()
            .expect("the test binary starts");
        if !child.status.success() {
            let stderr = String::from_utf8_lossy(&child.stderr);
            let tail: Vec<&str> = stderr.lines().rev().take(4).collect();
            failures.push(format!(
                "{case}: the child ended with {} — {}",
                child.status,
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The guard is checked at every instruction, not only where a template or a
/// function is entered: one template body of nested literal result elements
/// and instructions stops at the limit, with the location of the instruction
/// it stopped at. (Without that check the body ran to its end, whatever the
/// limit, and on a small enough stack overflowed it.)
#[test]
fn every_instruction_is_checked_against_the_stack_limit() {
    let open =
        r#"<e><xsl:if test="true()"><xsl:for-each select="."><xsl:choose><xsl:when test="true()">"#;
    let close = "</xsl:when></xsl:choose></xsl:for-each></xsl:if></e>";
    let text = sheet(&format!(
        r#"<xsl:template match="/">
{}</xsl:template>"#,
        nest(40, &format!("{open}\n"), "x", close)
    ));
    let options = TransformOptions {
        stack_limit: Some(512 * 1024),
        ..TransformOptions::default()
    };
    let result = xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || {
        transform(&text, &[], "<d/>", false, options)
    });
    let error = match result {
        Ok(out) => panic!(
            "expected the stack guard's error, got a result of {} bytes",
            out.len()
        ),
        Err(e) => e,
    };
    assert_eq!(error.code, "XSLT2-INTERNAL", "{error}");
    assert!(error.message.contains("more than the 524288"), "{error}");
    // The body starts on line 2, one level per line: the error names a line
    // inside it, past the first level.
    let line = error.location.as_ref().map_or(0, |l| l.line);
    assert!((3..=41).contains(&line), "{error} at line {line}");
}

/// The guard does not stop what fits: moderate nesting on this test's own
/// thread, and deep nesting, deep documents and long include chains on a
/// `run_on_large_stack` thread, compile and run as before.
#[test]
fn moderate_depth_on_any_thread_and_deep_input_on_a_large_stack_still_run() {
    let ifs = |n: usize| {
        sheet(&format!(
            r#"<xsl:template match="/"><out>{}</out></xsl:template>"#,
            nest(n, r#"<xsl:if test="true()">"#, "x", "</xsl:if>")
        ))
    };
    let elements = |n: usize| {
        sheet(&format!(
            r#"<xsl:template match="/">{}</xsl:template>"#,
            nest(n, "<e>", "x", "</e>")
        ))
    };
    let copy = sheet(r#"<xsl:template match="/"><xsl:copy-of select="."/></xsl:template>"#);

    // This thread: a test thread, whose size the run cannot know.
    assert_eq!(
        transform(&ifs(8), &[], "<d/>", false, TransformOptions::default())
            .unwrap_or_else(|e| panic!("8 nested xsl:if: {e}")),
        "<out>x</out>"
    );
    assert_eq!(
        transform(
            &elements(8),
            &[],
            "<d/>",
            false,
            TransformOptions::default()
        )
        .unwrap_or_else(|e| panic!("8 nested elements: {e}")),
        nest(8, "<e>", "x", "</e>")
    );
    let out = transform(
        &copy,
        &[],
        &deep_source(1_000, ""),
        false,
        TransformOptions::default(),
    )
    .unwrap_or_else(|e| panic!("a copy 1,000 deep: {e}"));
    assert_eq!(out.matches("<e>").count(), 1_001);

    // A large stack.
    let (ifs_out, elements_out, copy_out) =
        xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || {
            let options = TransformOptions::default;
            (
                transform(&ifs(1_000), &[], "<d/>", false, options()),
                transform(&elements(1_000), &[], "<d/>", false, options()),
                transform(&copy, &[], &deep_source(100_000, ""), false, options()),
            )
        });
    assert_eq!(
        ifs_out.unwrap_or_else(|e| panic!("1,000 nested xsl:if: {e}")),
        "<out>x</out>"
    );
    assert_eq!(
        elements_out.unwrap_or_else(|e| panic!("1,000 nested elements: {e}")),
        nest(1_000, "<e>", "x", "</e>")
    );
    assert_eq!(
        copy_out
            .unwrap_or_else(|e| panic!("a copy 100,000 deep: {e}"))
            .matches("<e>")
            .count(),
        100_001
    );
}

/// Compiling nested instructions takes time linear in their depth: each
/// element's static context (§5.4.1), which accumulates the standard
/// attributes of all its ancestors, is built from its parent's rather than by
/// walking every ancestor again — which made 10,000 nested `xsl:if` take 21.5 s
/// to compile in a release build, and gave the stack guard a quadratic cost to
/// hide behind on a large stack.
#[test]
fn compiling_nested_instructions_is_linear_in_the_depth() {
    xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || {
        common::scaling::assert_linear("nested xsl:if", 125, 8, |n| {
            let text = sheet(&format!(
                r#"<xsl:template match="/"><out>{}</out></xsl:template>"#,
                nest(n, r#"<xsl:if test="true()">"#, "x", "</xsl:if>")
            ));
            let names = NameTable::new();
            let functions = XsltFunctions::new();
            let compiler = Compiler::new(&names, &functions);
            let mut compiled = None;
            let elapsed = common::scaling::timed(|| {
                compiled = Some(compiler.compile_str(&text, "urn:nested").map(|_| ()))
            });
            if let Some(Err(e)) = compiled {
                panic!("{n} nested xsl:if: {e}");
            }
            elapsed
        });
    });
}
