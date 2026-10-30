//! Result URIs and the implicit result tree: the base output URI is normalized, the implicit
//! primary result's base URI, a zero-length initial result, result URIs compared after RFC 3986
//! §6.2.2 normalization, relative and empty base output URIs, the public API for secondary results,
//! and every way the initial result becomes non-empty. Public API only; "§N" is XSLT 2.0, "RFC
//! 3986 §N" the URI syntax.

use xsd_schema::namespace::NameTable;
use xslt2::output::OutputMethod;
use xslt2::{Compiler, Outcome, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
/// The stylesheet's base URI: the default base output URI.
const SHEET_URI: &str = "file:///results/style/main.xsl";

/// `content` as the template matching `/`, after the declarations `decls`.
fn root(decls: &str, content: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">{decls}<xsl:template match="/">{content}</xsl:template></xsl:stylesheet>"#
    )
}

/// Compiles `text` as the module `sheet_uri`, runs it over `<doc/>` with the
/// base output URI `base` (`None`: the default), and hands the outcome — or
/// the error's code — to `check`.
fn with_outcome<R>(
    text: &str,
    sheet_uri: &str,
    base: Option<&str>,
    check: impl FnOnce(&xslt2::CompiledStylesheet<'_>, Result<&Outcome<'_>, String>) -> R,
) -> R {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, sheet_uri)
        .unwrap_or_else(|e| panic!("static error {e:?}"));
    let arena = bumpalo::Bump::new();
    let options = TransformOptions {
        base_output_uri: base.map(str::to_string),
        ..Default::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .transform_source("file:///results/in/doc.xml", "<doc/>");
    match outcome {
        Ok(outcome) => check(&compiled, Ok(&outcome)),
        Err(e) => check(&compiled, Err(e.code)),
    }
}

/// The code of the dynamic error `text` raises with the base output URI
/// `base`.
fn error_with(text: &str, base: Option<&str>) -> String {
    with_outcome(text, SHEET_URI, base, |_, outcome| match outcome {
        Ok(o) => panic!(
            "expected an error; primary {:?}, secondary {:?}",
            o.primary.is_some(),
            o.secondary.iter().map(|t| &t.uri).collect::<Vec<_>>()
        ),
        Err(code) => code,
    })
}

fn error(text: &str) -> String {
    error_with(text, None)
}

/// The URIs of the secondary results.
fn uris(outcome: &Outcome<'_>) -> Vec<String> {
    outcome.secondary.iter().map(|t| t.uri.clone()).collect()
}

fn ok<'o, 't>(outcome: Result<&'o Outcome<'t>, String>) -> &'o Outcome<'t> {
    outcome.unwrap_or_else(|code| panic!("dynamic error {code}"))
}

// ═══ the base output URI is normalized ═════════════════════════════════════

/// §2.4 makes the tree created implicitly an `xsl:result-document href=""`,
/// and §19.1 resolves every `href` against the base output URI — RFC 3986
/// §5.2.4 removes the dot segments of every non-empty one. So
/// `file:///o/x/../main.xml` and `href="main.xml"` are one URI: "[ERR
/// XTDE1490] It is a non-recoverable dynamic error for a transformation to
/// generate two or more final result trees with the same URI" — here the
/// tree created implicitly and the result document.
#[test]
fn a_base_output_uri_with_dot_segments_clashes_with_its_normal_form() {
    let clash = root(
        "",
        r#"<out/><xsl:result-document href="main.xml"><r/></xsl:result-document>"#,
    );
    assert_eq!(
        error_with(&clash, Some("file:///o/x/../main.xml")),
        "XTDE1490"
    );
}

/// The second half: alone, the result document at the
/// normalized base output URI is the primary result (§2.4), and
/// `Outcome::base_output_uri` is spelled as `href` resolution spells it — from
/// the option and from the stylesheet's own URI.
#[test]
fn a_result_document_at_the_normalized_base_output_uri_is_the_primary() {
    let base = "file:///o/x/../main.xml";
    let alone = root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<xsl:result-document href="main.xml"><r/></xsl:result-document>"#,
    );
    for (sheet_uri, option) in [(SHEET_URI, Some(base)), (base, None)] {
        with_outcome(&alone, sheet_uri, option, |sheet, outcome| {
            let outcome = ok(outcome);
            assert!(outcome.secondary.is_empty(), "{:?}", uris(outcome));
            assert_eq!(sheet.serialize_primary(outcome).unwrap(), "<r/>");
            assert_eq!(outcome.base_output_uri, "file:///o/main.xml");
        });
    }
}

// ═══ the implicit primary result's base URI ═════════════════════════════════

/// §19.1: "The base URI of the document node at the root of the final result
/// tree is based on the effective value of the href attribute. If the
/// effective value is a relative URI, then it is resolved relative to the base
/// output URI." §2.4 makes the tree created implicitly an
/// `xsl:result-document href=""`: its base URI is the base output URI, as for
/// the tree an explicit `href=""` creates — none when that is empty.
#[test]
fn the_implicit_primary_result_has_the_base_output_uri_as_base_uri() {
    let base = "file:///probe/out/main.xml";
    for content in [
        "<out/>",
        r#"<xsl:result-document href=""><out/></xsl:result-document>"#,
    ] {
        with_outcome(&root("", content), SHEET_URI, Some(base), |_, outcome| {
            let primary = ok(outcome).primary.expect("a primary result");
            assert_eq!(primary.document_base_uri(), Some(base), "{content}");
        });
    }
    // The default: the stylesheet's URI.
    with_outcome(&root("", "<out/>"), SHEET_URI, None, |_, outcome| {
        let primary = ok(outcome).primary.unwrap();
        assert_eq!(primary.document_base_uri(), Some(SHEET_URI));
    });
    // An empty base output URI gives no base URI.
    with_outcome(&root("", "<out/>"), "", None, |_, outcome| {
        let primary = ok(outcome).primary.unwrap();
        assert_eq!(primary.document_base_uri(), None);
    });
}

// ═══ a zero-length initial result is a non-empty sequence ═══════════════════

/// §2.4: "If the result sequence is non-empty, then it is used to construct an
/// implicit final result tree" — the sequence, read literally: a zero-length
/// text node, a zero-length string and an empty document node are items, even
/// though §11.4 ignores "zero-length text nodes … when they appear in a
/// sequence that is used to form the content of a node". Next to an
/// `xsl:result-document` without `href` (before or after it) that is two trees
/// at the base output URI: XTDE1490. The empty sequence is not.
#[test]
fn a_zero_length_initial_result_still_makes_the_implicit_tree() {
    let rd = r#"<xsl:result-document><r/></xsl:result-document>"#;
    for item in [
        "<xsl:text/>",
        r#"<xsl:value-of select="''"/>"#,
        r#"<xsl:sequence select="''"/>"#,
        "<xsl:document/>",
    ] {
        assert_eq!(
            error(&root("", &format!("{item}{rd}"))),
            "XTDE1490",
            "{item} first"
        );
        assert_eq!(
            error(&root("", &format!("{rd}{item}"))),
            "XTDE1490",
            "{item} after"
        );
    }
    let text = root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        &format!(r#"<xsl:sequence select="()"/>{rd}"#),
    );
    with_outcome(&text, SHEET_URI, None, |sheet, outcome| {
        assert_eq!(sheet.serialize_primary(ok(outcome)).unwrap(), "<r/>");
    });
}

// ═══ result URIs are compared after RFC 3986 §6.2.2 normalization ═══════════

/// XTDE1490 is about "the same URI", and RFC 3986 §6.2.2 spellings of one URI
/// are one URI: the scheme's and host's case, the case of percent-encoding hex
/// digits, percent-encoded unreserved characters, dot segments (also
/// percent-encoded ones). §5.8: an `href` is resolved "after first escaping all
/// characters that need to be escaped to make it a valid RFC3986 URI
/// reference", so `a b.xml` is `a%20b.xml` and `é` is `%C3%A9`.
#[test]
fn two_spellings_of_one_result_uri_are_xtde1490() {
    for (first, second) in [
        ("a.xml", "a%2Exml"),
        ("a.xml", "FILE:///results/style/a.xml"),
        ("http://example.com/a.xml", "HTTP://Example.COM/a.xml"),
        ("%c3%a9.xml", "%C3%A9.xml"),
        ("\u{e9}.xml", "%C3%A9.xml"),
        ("a b.xml", "a%20b.xml"),
        ("~a.xml", "%7Ea.xml"),
        ("a.xml", "sub/%2E%2E/a.xml"),
    ] {
        let text = root(
            "",
            &format!(
                r#"<xsl:result-document href="{first}"><a/></xsl:result-document><xsl:result-document href="{second}"><b/></xsl:result-document>"#
            ),
        );
        assert_eq!(error(&text), "XTDE1490", "{first} / {second}");
    }
}

/// RFC 3986 keeps empty path segments, so `sub//a.xml` and `sub/a.xml` are
/// different URIs (§6.2.2 does not equate them; at most XTRE1495, "URIs that
/// identify the same physical resource", which "may be impossible for the
/// processor to detect"). Nor is the path's case folded, nor §6.2.3's
/// scheme-based `file://localhost/` applied.
#[test]
fn different_uris_stay_apart() {
    for (first, second) in [
        ("sub/a.xml", "sub//a.xml"),
        ("a.xml", "A.xml"),
        ("a.xml", "file://localhost/results/style/a.xml"),
    ] {
        let text = root(
            "",
            &format!(
                r#"<xsl:result-document href="{first}"><a/></xsl:result-document><xsl:result-document href="{second}"><b/></xsl:result-document>"#
            ),
        );
        with_outcome(&text, SHEET_URI, None, |_, outcome| {
            assert_eq!(ok(outcome).secondary.len(), 2, "{first} / {second}");
        });
    }
}

/// The URI reported is the resolved one, as written (`a b.xml` keeps its
/// space); `Outcome::result_document` finds a tree by any spelling of its URI
/// (§19.1: "If the implementation provides an API to access final result
/// trees, then it must allow a final result tree to be identified by means of
/// this base URI"), and the primary result is chosen by its normalized URI
/// too (§2.4).
#[test]
fn lookup_and_the_primary_result_use_the_normalized_uri() {
    let text = root(
        "",
        r#"<xsl:result-document href="a b.xml"><a/></xsl:result-document><xsl:result-document href="sub/c.xml"><c/></xsl:result-document>"#,
    );
    with_outcome(&text, SHEET_URI, None, |_, outcome| {
        let outcome = ok(outcome);
        assert_eq!(
            uris(outcome),
            [
                "file:///results/style/a b.xml",
                "file:///results/style/sub/c.xml"
            ]
        );
        let found = |uri: &str| outcome.result_document(uri).map(|t| t.uri.as_str());
        assert_eq!(
            found("file:///results/style/a b.xml"),
            Some("file:///results/style/a b.xml")
        );
        assert_eq!(
            found("file:///results/style/a%20b.xml"),
            Some("file:///results/style/a b.xml")
        );
        assert_eq!(
            found("FILE:///results/style/./x/../sub/c%2Exml"),
            Some("file:///results/style/sub/c.xml")
        );
        assert_eq!(found("file:///results/style/sub//c.xml"), None);
        assert_eq!(found("sub/c.xml"), None);
    });

    // An href naming the base output URI in another spelling makes the
    // primary result.
    let text = root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<xsl:result-document href="FILE:///o/main%2Exml"><r/></xsl:result-document>"#,
    );
    with_outcome(
        &text,
        SHEET_URI,
        Some("file:///o/main.xml"),
        |sheet, outcome| {
            let outcome = ok(outcome);
            assert!(outcome.secondary.is_empty(), "{:?}", uris(outcome));
            assert_eq!(sheet.serialize_primary(outcome).unwrap(), "<r/>");
        },
    );
}

// ═══ relative and empty base output URIs ═══════════════════════════════════

/// `compile_str(text, "")` and no option: the base output URI is empty, the
/// result URIs are the `href`s themselves (relative), the primary result's
/// URI is `""` — and XTDE1490 still holds for the tree created implicitly and
/// an `xsl:result-document` without `href` (both `""`), or with `href="."`.
#[test]
fn an_empty_base_output_uri_gives_relative_uris_and_xtde1490_still_holds() {
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml"><a/></xsl:result-document><xsl:result-document href="../b.xml"><b/></xsl:result-document>"#,
    );
    with_outcome(&text, "", None, |_, outcome| {
        let outcome = ok(outcome);
        assert_eq!(outcome.base_output_uri, "");
        assert_eq!(uris(outcome), ["a.xml", "../b.xml"]);
        assert!(outcome.result_document("./a.xml").is_some());
        assert!(outcome.primary.is_none());
    });
    for content in [
        r#"<out/><xsl:result-document><r/></xsl:result-document>"#,
        r#"<xsl:result-document><r/></xsl:result-document><out/>"#,
        r#"<xsl:result-document><r/></xsl:result-document><xsl:result-document href="."><s/></xsl:result-document>"#,
    ] {
        with_outcome(&root("", content), "", None, |_, outcome| {
            assert_eq!(outcome.err().as_deref(), Some("XTDE1490"), "{content}");
        });
    }
    // Alone, the href-less tree is the primary result.
    let text = root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<xsl:result-document><r/></xsl:result-document>"#,
    );
    with_outcome(&text, "", None, |sheet, outcome| {
        let outcome = ok(outcome);
        assert!(outcome.secondary.is_empty());
        assert_eq!(sheet.serialize_primary(outcome).unwrap(), "<r/>");
    });
}

/// A relative base output URI gives relative result URIs; a directory needs
/// its trailing `/`, because RFC 3986 §5.2.3 merges a relative reference with
/// "all but the last segment of the base URI's path".
#[test]
fn relative_bases_and_the_trailing_slash() {
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml"><a/></xsl:result-document><xsl:result-document href="../b.xml"><b/></xsl:result-document>"#,
    );
    for (base, expected) in [
        ("out/", ["out/a.xml", "b.xml"]),
        ("file:///o/out", ["file:///o/a.xml", "file:///b.xml"]),
        ("file:///o/out/", ["file:///o/out/a.xml", "file:///o/b.xml"]),
    ] {
        with_outcome(&text, SHEET_URI, Some(base), |_, outcome| {
            assert_eq!(uris(ok(outcome)), expected, "{base}");
        });
    }
}

// ═══ the public API ═══════════════════════════════════════════════════════

/// §19.1: "the character maps referenced in this instruction supplement and
/// take precedence over those defined in the selected output definition" —
/// `SecondaryResult::output` says so in its names as well as in its map: the
/// definition's maps first, then the instruction's, the order in which a later
/// mapping wins (§20.1).
#[test]
fn secondary_output_lists_the_instructions_character_maps() {
    let text = root(
        r#"<xsl:character-map name="map1"><xsl:output-character character="A" string="[A1]"/><xsl:output-character character="B" string="[B1]"/></xsl:character-map>
           <xsl:character-map name="map2"><xsl:output-character character="A" string="[A2]"/></xsl:character-map>
           <xsl:output name="f" omit-xml-declaration="yes" use-character-maps="map1"/>"#,
        r#"<xsl:result-document href="r.xml" format="f" use-character-maps="map2"><r>AB</r></xsl:result-document>
           <xsl:result-document href="s.xml" format="f"><r>AB</r></xsl:result-document>"#,
    );
    with_outcome(&text, SHEET_URI, None, |sheet, outcome| {
        let outcome = ok(outcome);
        let names = |uri: &str| -> Vec<String> {
            outcome
                .result_document(uri)
                .unwrap()
                .output
                .character_map_names()
                .map(|q| q.local.clone())
                .collect()
        };
        assert_eq!(names("file:///results/style/r.xml"), ["map1", "map2"]);
        assert_eq!(names("file:///results/style/s.xml"), ["map1"]);
        let r = outcome
            .result_document("file:///results/style/r.xml")
            .unwrap();
        assert_eq!(sheet.serialize_secondary(r).unwrap(), "<r>[A2][B1]</r>");
    });
}

/// `Outcome::result_document` finds secondary results only: the primary
/// result — here one an `xsl:result-document` created — is `primary`, under
/// `base_output_uri`, and its effective output definition is
/// `CompiledStylesheet::primary_output_definition` (the unnamed one for the
/// tree created implicitly).
#[test]
fn the_primary_result_is_found_through_primary_not_result_document() {
    let text = root(
        r#"<xsl:output name="t" method="text"/>"#,
        r#"<xsl:result-document format="t"><a>hello</a></xsl:result-document>"#,
    );
    with_outcome(&text, SHEET_URI, None, |sheet, outcome| {
        let outcome = ok(outcome);
        assert!(outcome.primary.is_some());
        assert!(outcome.result_document(&outcome.base_output_uri).is_none());
        let definition = sheet.primary_output_definition(outcome).unwrap();
        assert_eq!(definition.method.get(), Some(&OutputMethod::Text));
        assert_eq!(sheet.serialize_primary(outcome).unwrap(), "hello");
    });
    with_outcome(&root("", "<out/>"), SHEET_URI, None, |sheet, outcome| {
        let definition = sheet.primary_output_definition(ok(outcome)).unwrap();
        assert!(!definition.method.is_set());
    });
    // No primary result, no definition.
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml"><a/></xsl:result-document>"#,
    );
    with_outcome(&text, SHEET_URI, None, |sheet, outcome| {
        assert!(sheet.primary_output_definition(ok(outcome)).is_none());
    });
}

// ═══ every way the initial result becomes non-empty ═════════════════════════

/// §2.4: "If the result sequence is non-empty, then it is used to construct an
/// implicit final result tree", and XTDE1490's Note: "it is an error … to
/// evaluate any xsl:result-document instruction that omits the href attribute
/// if an initial final result tree is created implicitly". One construct per
/// receiver event the initial template can write — an atomic value or a node
/// by reference (`xsl:sequence`), a copied node (`xsl:copy-of`), a document
/// node (an empty `xsl:document`, nothing but the document node), a comment, a
/// processing instruction — before the
/// result document (the eager check) and after it (the end-of-run check).
#[test]
fn every_kind_of_initial_content_makes_the_implicit_tree() {
    let rd = r#"<xsl:result-document><r/></xsl:result-document>"#;
    for item in [
        r#"<xsl:sequence select="1"/>"#,
        r#"<xsl:sequence select="/doc"/>"#,
        r#"<xsl:copy-of select="/doc"/>"#,
        "<xsl:document/>",
        "<xsl:comment>c</xsl:comment>",
        r#"<xsl:processing-instruction name="p">d</xsl:processing-instruction>"#,
    ] {
        assert_eq!(
            error(&root("", &format!("{item}{rd}"))),
            "XTDE1490",
            "{item} first"
        );
        assert_eq!(
            error(&root("", &format!("{rd}{item}"))),
            "XTDE1490",
            "{item} after"
        );
    }
}

/// The state of §2.4 is per run: one `Transformation` whose first run writes
/// to the primary result and whose second run does not must, in the second
/// run, let an `xsl:result-document` without `href` be the primary result —
/// not report XTDE1490 for the first run's content.
#[test]
fn a_second_run_starts_with_an_empty_initial_result() {
    let text = root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<xsl:if test="/doc/@w"><out/></xsl:if>
           <xsl:if test="not(/doc/@w)"><xsl:result-document><r/></xsl:result-document></xsl:if>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, SHEET_URI)
        .unwrap();
    let arena = bumpalo::Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let serialized = |outcome: Result<Outcome<'_>, XsltError>| {
        compiled
            .serialize_primary(&outcome.unwrap_or_else(|e| panic!("{}", e.code)))
            .unwrap()
    };
    assert_eq!(
        serialized(run.transform_source("file:///results/in/w.xml", r#"<doc w="1"/>"#)),
        "<out/>"
    );
    assert_eq!(
        serialized(run.transform_source("file:///results/in/n.xml", "<doc/>")),
        "<r/>"
    );
}
