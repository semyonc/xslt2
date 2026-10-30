//! A zero-length text node from a typed template makes the implicit result tree (§2.4), and rootless
//! paths keep the slash RFC 3986 §5.2.4 leaves. Public API only; "§N" is XSLT 2.0, "RFC 3986 §N"
//! the URI syntax.

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, Outcome, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
/// The stylesheet's base URI: the default base output URI.
const SHEET_URI: &str = "file:///results/style/main.xsl";
/// An `xsl:result-document` without `href`: a tree at the base output URI.
const RD: &str = r#"<xsl:result-document><r/></xsl:result-document>"#;

/// A stylesheet of the declarations `decls`, with `omit-xml-declaration`.
fn sheet(decls: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f" exclude-result-prefixes="xs f"><xsl:output omit-xml-declaration="yes"/>{decls}</xsl:stylesheet>"#
    )
}

/// Compiles `text`, runs it **twice** over `<doc/>` through one
/// `Transformation` (the §2.4 state is per run), checks that both runs agree
/// and hands the second outcome — or the error's code — to `check`.
fn with_outcome<R>(
    text: &str,
    check: impl FnOnce(&xslt2::CompiledStylesheet<'_>, Result<&Outcome<'_>, String>) -> R,
) -> R {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, SHEET_URI)
        .unwrap_or_else(|e| panic!("static error {e:?}"));
    let arena = bumpalo::Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let summary = |outcome: &Result<Outcome<'_>, xslt2::XsltError>| match outcome {
        Ok(o) => format!(
            "primary {:?}, secondary {:?}",
            o.primary.map(|_| compiled.serialize_primary(o).unwrap()),
            o.secondary.iter().map(|t| &t.uri).collect::<Vec<_>>()
        ),
        Err(e) => e.code.clone(),
    };
    let first = run.transform_source("file:///results/in/doc.xml", "<doc/>");
    let second = run.transform_source("file:///results/in/doc.xml", "<doc/>");
    assert_eq!(summary(&first), summary(&second), "two runs disagree");
    match second {
        Ok(outcome) => check(&compiled, Ok(&outcome)),
        Err(e) => check(&compiled, Err(e.code)),
    }
}

/// The code of the dynamic error `text` raises.
fn error(text: &str) -> String {
    with_outcome(text, |sheet, outcome| match outcome {
        Ok(o) => panic!(
            "expected an error; primary {:?}, secondary {:?}",
            o.primary.map(|_| sheet.serialize_primary(o).unwrap()),
            o.secondary.iter().map(|t| &t.uri).collect::<Vec<_>>()
        ),
        Err(code) => code,
    })
}

/// The serialized primary result of `text`, which must succeed.
fn primary(text: &str) -> String {
    with_outcome(text, |sheet, outcome| {
        let outcome = outcome.unwrap_or_else(|code| panic!("dynamic error {code}"));
        sheet.serialize_primary(outcome).unwrap()
    })
}

// ═══ a zero-length text node from a typed template counts (§2.4) ══════════

/// The zero-length text nodes `<xsl:text/>` (§11.4.2: "An xsl:text element
/// may be empty, in which case the result of evaluating the instruction is a
/// text node whose string value is the zero-length string") and
/// `<xsl:value-of select="''"/>` (§11.4.3: "The xsl:value-of instruction is
/// evaluated to construct a new text node").
const ZERO_LENGTH_TEXTS: [&str; 2] = ["<xsl:text/>", r#"<xsl:value-of select="''"/>"#];

/// §2.4: "If the initial template has an as attribute, then the result
/// sequence of the initial template is checked against the required type in
/// the same way as for any other template", and "If the result sequence is
/// non-empty, then it is used to construct an implicit final result tree" —
/// the `as` attribute does not change whether the sequence is empty. A
/// zero-length text node makes it non-empty with `as` exactly as without, so
/// next to an `xsl:result-document` without `href`, before or after it, it is
/// XTDE1490 either way (the probes `empty-text-typed-*` and
/// `empty-value-of-typed-*` probes).
#[test]
fn a_typed_initial_template_s_zero_length_text_makes_the_implicit_tree() {
    for as_attribute in [
        "",
        r#" as="item()*""#,
        r#" as="node()*""#,
        r#" as="text()*""#,
    ] {
        for item in ZERO_LENGTH_TEXTS {
            for content in [format!("{item}{RD}"), format!("{RD}{item}")] {
                let text = sheet(&format!(
                    r#"<xsl:template match="/"{as_attribute}>{content}</xsl:template>"#
                ));
                assert_eq!(error(&text), "XTDE1490", "{as_attribute} {content}");
            }
        }
    }
}

/// The same through a typed template the initial template calls or applies,
/// directly or through another typed template, and through
/// `xsl:perform-sort`'s sequence constructor, which also collects a sequence
/// and passes it on (the `empty-typed-called` probe and its
/// variants).
#[test]
fn a_typed_template_the_initial_template_invokes_counts_too() {
    let typed = r#"<xsl:template name="t" as="item()*"><xsl:text/></xsl:template>
                   <xsl:template name="v" as="text()*"><xsl:value-of select="''"/></xsl:template>
                   <xsl:template name="outer" as="item()*"><xsl:call-template name="t"/></xsl:template>
                   <xsl:template match="doc" mode="m" as="item()*"><xsl:text/></xsl:template>"#;
    for call in [
        r#"<xsl:call-template name="t"/>"#,
        r#"<xsl:call-template name="v"/>"#,
        r#"<xsl:call-template name="outer"/>"#,
        r#"<xsl:apply-templates select="doc" mode="m"/>"#,
        r#"<xsl:for-each select="doc"><xsl:call-template name="t"/></xsl:for-each>"#,
        r#"<xsl:perform-sort><xsl:sort select="."/><xsl:text/></xsl:perform-sort>"#,
    ] {
        for content in [format!("{call}{RD}"), format!("{RD}{call}")] {
            let text = sheet(&format!(
                r#"{typed}<xsl:template match="/">{content}</xsl:template>"#
            ));
            assert_eq!(error(&text), "XTDE1490", "{content}");
        }
    }
}

/// Where the zero-length text node of a typed template lands in content it
/// still vanishes — §11.4: "zero-length text nodes are ignored when they appear
/// in a sequence that is used to form the content of a node, as described in
/// 5.7.1 Constructing Complex Content and 5.7.2 Constructing Simple Content" —
/// and inside an `xsl:result-document` it is that tree's content, not the
/// initial template's result. The empty sequence from a typed template is
/// still empty.
#[test]
fn a_typed_template_s_zero_length_text_still_vanishes_from_content() {
    let typed = r#"<xsl:template name="t" as="item()*"><xsl:text/></xsl:template>
                   <xsl:template name="ab" as="item()*"><xsl:text/><xsl:sequence select="'a'"/><xsl:text/><xsl:sequence select="'b'"/></xsl:template>
                   <xsl:template name="none" as="item()*"><xsl:sequence select="()"/></xsl:template>
                   <xsl:function name="f:f" as="item()*"><xsl:text/></xsl:function>"#;
    let run = |content: &str| {
        primary(&sheet(&format!(
            r#"{typed}<xsl:template match="/">{content}</xsl:template>"#
        )))
    };
    // Complex content (§5.7.1) and simple content (§5.7.2).
    assert_eq!(run(r#"<out><xsl:call-template name="t"/></out>"#), "<out/>");
    assert_eq!(
        run(r#"<out><xsl:attribute name="a"><xsl:call-template name="t"/></xsl:attribute></out>"#),
        r#"<out a=""/>"#
    );
    // §5.7.2 discards zero-length text nodes before it joins the strings, so
    // the separator stands between `a` and `b` only — with or without `as`.
    assert_eq!(
        run(
            r#"<out><xsl:value-of separator="|"><xsl:call-template name="ab"/></xsl:value-of></out>"#
        ),
        "<out>a|b</out>"
    );
    assert_eq!(
        run(
            r#"<out><xsl:value-of separator="|"><xsl:text/><xsl:sequence select="'a'"/><xsl:text/><xsl:sequence select="'b'"/></xsl:value-of></out>"#
        ),
        "<out>a|b</out>"
    );
    // A typed variable and a function result used as content.
    assert_eq!(
        run(
            r#"<xsl:variable name="v" as="item()*"><xsl:call-template name="t"/></xsl:variable><out><xsl:copy-of select="$v"/><xsl:sequence select="$v"/><xsl:sequence select="f:f()"/></out>"#
        ),
        "<out/>"
    );
    // Sorted, then content.
    assert_eq!(
        run(
            r#"<out><xsl:perform-sort><xsl:sort select="."/><xsl:text/><xsl:sequence select="'x'"/></xsl:perform-sort></out>"#
        ),
        "<out>x</out>"
    );
    // Inside the result document it is that tree's content: the result
    // document alone is the primary result, with nothing in it.
    assert_eq!(
        run(r#"<xsl:result-document><xsl:call-template name="t"/></xsl:result-document>"#),
        ""
    );
    // With no `xsl:result-document`, the implicit tree is the empty document.
    assert_eq!(run(r#"<xsl:call-template name="t"/>"#), "");
    // The empty sequence makes no implicit tree, typed or not.
    assert_eq!(
        run(&format!(r#"<xsl:call-template name="none"/>{RD}"#)),
        "<r/>"
    );
    assert_eq!(
        primary(&sheet(&format!(
            r#"<xsl:template match="/" as="item()*"><xsl:sequence select="()"/>{RD}</xsl:template>"#
        ))),
        "<r/>"
    );
}

// ═══ rootless paths keep the slash RFC 3986 §5.2.4 leaves ═════════════════

/// RFC 3986 §5.2.4 step C: "replace that prefix with "/" in the input buffer
/// and remove the last segment and its preceding "/" (if any) from the output
/// buffer" — so in the rootless path of `example:a/../c` the slash before `c`
/// survives the removal of `a`, and the URI is `example:/c`, not `example:c`.
/// §6.2.2.2 decodes `%2E%2E` to `..` before §6.2.2.3 removes it. XTDE1490 is
/// about the same URI: `example:a/%2E%2E/c` and `example:/c` are one
/// (a collision), `example:a/%2E%2E/c` and `example:c` are
/// two (its false one).
#[test]
fn a_rootless_path_keeps_the_slash_rfc_3986_leaves() {
    let two = |first: &str, second: &str| {
        sheet(&format!(
            r#"<xsl:template match="/"><xsl:result-document href="{first}"><a/></xsl:result-document><xsl:result-document href="{second}"><b/></xsl:result-document></xsl:template>"#
        ))
    };
    for (first, second) in [
        ("example:a/%2E%2E/c", "example:/c"),
        ("example:a/../c", "example:/c"),
        ("example:a/b/%2E%2E/%2E%2E/c", "example:/c"),
        ("example:a/%2E%2E", "example:/"),
    ] {
        assert_eq!(error(&two(first, second)), "XTDE1490", "{first} / {second}");
    }
    with_outcome(&two("example:a/%2E%2E/c", "example:c"), |_, outcome| {
        let outcome = outcome.unwrap_or_else(|code| panic!("dynamic error {code}"));
        let uris: Vec<_> = outcome.secondary.iter().map(|t| t.uri.as_str()).collect();
        // Reported as resolution spells them; compared in normal form.
        assert_eq!(uris, ["example:a/%2E%2E/c", "example:c"]);
        let found = |uri: &str| outcome.result_document(uri).map(|t| t.uri.as_str());
        assert_eq!(found("example:/c"), Some("example:a/%2E%2E/c"));
        assert_eq!(found("example:c"), Some("example:c"));
    });
    // Raw dots are removed when the `href` is resolved, by the same rule.
    with_outcome(&two("example:a/../c", "example:c"), |_, outcome| {
        let outcome = outcome.unwrap_or_else(|code| panic!("dynamic error {code}"));
        let uris: Vec<_> = outcome.secondary.iter().map(|t| t.uri.as_str()).collect();
        assert_eq!(uris, ["example:/c", "example:c"]);
        assert_eq!(
            outcome.secondary[0].document.document_base_uri(),
            Some("example:/c")
        );
    });
}
