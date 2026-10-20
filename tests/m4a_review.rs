//! M4a review fixes (`XSLT2_M4A_REVIEW.md` R1 … R6, R9): `use-when` inside an inline schema, the URIs
//! the host resolver is asked for, an inline schema reached twice, a context reused by another
//! stylesheet, the XML Schema and XML namespaces, and the paths the first round left untested
//! (`xml:base` on an import, XTSE1650 from an imported module, a context compiled twice).
//!
//! Every test goes through the public API and cites the rule it checks (`xslt20.html` unless said
//! otherwise; `structures.html` for XML Schema).

mod common;

use std::sync::{Arc, Mutex};

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::error::codes;
use xslt2::{
    Compiler, ImportSource, MemoryResolver, Resolver, SchemaContext, TransformOptions, XsltError,
    XsltFunctions,
};

const XSL: &str = "http://www.w3.org/1999/XSL/Transform";

/// A stylesheet module with the namespace declarations every test here uses.
fn sheet(top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{XSL}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:a=\"urn:a\" xmlns:b=\"urn:b\" \
           xmlns:d=\"urn:d\" xmlns:i=\"urn:i\" exclude-result-prefixes=\"xs a b d i\">{top}</xsl:stylesheet>"
    )
}

/// A template matching `/` that writes `<out>` with the value of `select`.
fn out(select: &str) -> String {
    format!(
        "<xsl:template match=\"/\"><out><xsl:value-of select=\"{select}\"/></out></xsl:template>"
    )
}

/// A resolver that serves `files` and records every URI it is asked for.
#[derive(Debug, Default)]
struct Logged {
    files: MemoryResolver,
    requests: Mutex<Vec<String>>,
}

impl Logged {
    fn new(files: &[(&str, &str)]) -> Arc<Self> {
        let mut memory = MemoryResolver::new();
        for (uri, text) in files {
            memory.insert(*uri, *text);
        }
        Arc::new(Self {
            files: memory,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Resolver for Logged {
    fn resolve_text(&self, uri: &str) -> Result<String, String> {
        self.requests.lock().unwrap().push(uri.to_string());
        self.files.resolve_text(uri)
    }
}

/// Which step a run stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Context,
    Compile,
    Run,
}

type Outcome = Result<String, (Stage, XsltError)>;

/// Builds the context from the stylesheet at `main`, compiles it against the context and runs it
/// over `<doc/>`; the serialized primary result without the default XML declaration.
fn run_sa(main: &str, resolver: Arc<dyn Resolver>) -> Outcome {
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(main, &functions)
        .and_then(|builder| builder.build())
        .map_err(|e| (Stage::Context, e))?;
    run_against(&context, main, resolver)
}

/// Compiles the stylesheet at `main` against `context` and runs it over `<doc/>`.
fn run_against(context: &SchemaContext, main: &str, resolver: Arc<dyn Resolver>) -> Outcome {
    let functions = XsltFunctions::new();
    let compiled = Compiler::schema_aware(context, &functions)
        .with_resolver(resolver)
        .compile_uri(main)
        .map_err(|e| (Stage::Compile, e))?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("urn:in", "<doc/>")
        .map_err(|e| (Stage::Run, e))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| (Stage::Run, e))?;
    Ok(common::body(text))
}

fn context(main: &str, resolver: Arc<dyn Resolver>) -> Result<SchemaContext, XsltError> {
    let functions = XsltFunctions::new();
    SchemaContext::builder()
        .with_resolver(resolver)
        .stylesheet_uri(main, &functions)
        .and_then(|builder| builder.build())
}

fn memory(files: &[(&str, &str)]) -> Arc<dyn Resolver> {
    let mut memory = MemoryResolver::new();
    for (uri, text) in files {
        memory.insert(*uri, *text);
    }
    Arc::new(memory)
}

/// The code (and the step) a run stopped with.
fn failure(outcome: Outcome) -> (Stage, String) {
    match outcome {
        Ok(text) => panic!("expected an error, got the result {text:?}"),
        Err((stage, e)) => (stage, e.code.clone()),
    }
}

/// The 1-based line and byte column of the first occurrence of `needle` at or after byte `from`,
/// counted the way this crate reports a position in a stylesheet module.
fn position_of(text: &str, needle: &str, from: usize) -> (u32, u32) {
    let offset = from
        + text[from..]
            .find(needle)
            .expect("the needle is in the text");
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line = text[..offset].matches('\n').count() + 1;
    (line as u32, (offset - line_start + 1) as u32)
}

// ── R1: §3.12 inside an inline schema ───────────────────────────────────

/// §3.12: "A literal result element, or any other element within a stylesheet module that is not in
/// the XSLT namespace, may similarly carry an `xsl:use-when` attribute", and "When a node is
/// effectively excluded from a stylesheet module the stylesheet module has the same effect as if the
/// node were not there" — so of two alternative definitions inside an inline `xs:schema` only the one
/// `use-when` keeps is loaded (no XTSE0220 for a duplicate).
#[test]
fn use_when_inside_an_inline_schema_keeps_one_of_two_alternative_definitions() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:d">
             <xs:schema targetNamespace="urn:d">
               <xs:simpleType name="size" xsl:use-when="system-property('xsl:version') = '2.0'">
                 <xs:restriction base="xs:integer"><xs:maxInclusive value="10"/></xs:restriction>
               </xs:simpleType>
               <xs:simpleType name="size" xsl:use-when="system-property('xsl:version') != '2.0'">
                 <xs:restriction base="xs:string"/>
               </xs:simpleType>
             </xs:schema>
           </xsl:import-schema>
           {}"#,
        out("11 castable as d:size, 5 castable as d:size, 'abc' castable as d:size")
    ));
    let resolver = memory(&[("urn:main", &main)]);
    assert_eq!(
        run_sa("urn:main", resolver).unwrap(),
        "<out>false true false</out>"
    );
}

/// The same rule for a top-level component and for one nested in a complex type: an excluded type
/// is not in the in-scope schema types (§18.1.4 `type-available`), and an excluded attribute
/// declaration that would duplicate another is not there to conflict.
#[test]
fn an_element_excluded_inside_an_inline_schema_is_not_loaded() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:d">
             <xs:schema targetNamespace="urn:d">
               <xs:simpleType name="kept"><xs:restriction base="xs:string"/></xs:simpleType>
               <xs:simpleType name="gone" xsl:use-when="false()"><xs:restriction base="xs:string"/></xs:simpleType>
               <xs:complexType name="ct">
                 <xs:attribute name="a" type="xs:string"/>
                 <xs:attribute name="a" type="xs:integer" xsl:use-when="false()"/>
               </xs:complexType>
             </xs:schema>
           </xsl:import-schema>
           {}"#,
        out("type-available('d:kept'), type-available('d:gone'), type-available('d:ct')")
    ));
    let resolver = memory(&[("urn:main", &main)]);
    assert_eq!(
        run_sa("urn:main", resolver).unwrap(),
        "<out>true false true</out>"
    );
}

/// D-9: an error in an inline schema is reported at its position in the stylesheet module. Cutting
/// an excluded element out of the slice must not move what follows it: the duplicate declaration
/// after an excluded annotation (with a two-byte character in it) is reported at its own line and
/// column, on the same line as the excluded element and on the first line of the slice alike.
#[test]
fn an_error_after_an_excluded_element_keeps_its_position_in_the_module() {
    let multi_line = sheet(
        "\n<xsl:import-schema namespace=\"urn:d\">\n  <xs:schema targetNamespace=\"urn:d\">\n    \
         <xs:element name=\"temp\"/>\n    <xs:annotation xsl:use-when=\"false()\"><xs:documentation>d\u{e9}j\u{e0}\n      \
         vu \u{e0}</xs:documentation></xs:annotation> <xs:element name=\"temp\"/>\n  </xs:schema>\n</xsl:import-schema>\n\
         <xsl:template match=\"/\"><out/></xsl:template>",
    );
    let one_line = sheet(
        "<xsl:import-schema namespace=\"urn:d\"><xs:schema targetNamespace=\"urn:d\"><xs:element name=\"temp\"/>\
         <xs:annotation xsl:use-when=\"false()\"><xs:documentation>\u{e9}</xs:documentation></xs:annotation>\
         <xs:element name=\"temp\"/></xs:schema></xsl:import-schema><xsl:template match=\"/\"><out/></xsl:template>",
    );
    for main in [multi_line, one_line] {
        let after = main.find("</xs:annotation>").expect("the annotation");
        let expected = position_of(&main, "<xs:element name=\"temp\"", after);
        let resolver = memory(&[("urn:main", &main)]);
        match run_sa("urn:main", resolver) {
            Err((Stage::Context, e)) => {
                assert_eq!(e.code, codes::XTSE0220, "{e}");
                let location = e.location.clone().expect("a location");
                assert_eq!(location.module_uri.as_deref(), Some("urn:main"), "{e}");
                assert_eq!((location.line, location.column), expected, "{e}");
            }
            other => panic!("expected XTSE0220 for the duplicate declaration, got {other:?}"),
        }
    }
}

// ── R3: the URIs the host resolver is asked for ─────────────────────────

/// The schemas of the URI tests: `s/main.xsl` imports `a.xsd` by location and holds an inline schema;
/// `a.xsd` includes `parts/p.xsd` and `../shared/sh.xsd`; the inline schema includes
/// `../shared/d.xsd`.
fn uri_files(base: &str) -> Vec<(String, String)> {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:a" schema-location="a.xsd"/>
           <xsl:import-schema namespace="urn:i">
             <xs:schema targetNamespace="urn:i"><xs:include schemaLocation="../shared/d.xsd"/></xs:schema>
           </xsl:import-schema>
           {}"#,
        out("for $t in ('a:froma', 'a:fromparts', 'a:fromshared', 'i:dd') return type-available($t)")
    ));
    let simple = |ns: &str, name: &str, include: &str| {
        format!(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"{ns}\">{include}\
             <xs:simpleType name=\"{name}\"><xs:restriction base=\"xs:string\"/></xs:simpleType></xs:schema>"
        )
    };
    vec![
        (format!("{base}s/main.xsl"), main),
        (
            format!("{base}s/a.xsd"),
            simple(
                "urn:a",
                "froma",
                r#"<xs:include schemaLocation="parts/p.xsd"/><xs:include schemaLocation="../shared/sh.xsd"/>"#,
            ),
        ),
        (
            format!("{base}s/parts/p.xsd"),
            simple("urn:a", "fromparts", ""),
        ),
        (
            format!("{base}shared/sh.xsd"),
            simple("urn:a", "fromshared", ""),
        ),
        (format!("{base}shared/d.xsd"), simple("urn:i", "dd", "")),
    ]
}

fn logged(files: &[(String, String)]) -> Arc<Logged> {
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(uri, text)| (uri.as_str(), text.as_str()))
        .collect();
    Logged::new(&files)
}

/// The `Resolver` contract (`src/resolver.rs`): "`uri` is always absolute: `resolve_uri` has already
/// been applied" — RFC 3986 §5.2, whose §5.2.4 removes dot segments. Under an `http:` base the
/// directives of a location import and of an inline schema reach the resolver as
/// `http://example.com/x/shared/…`, not `…/s/../shared/…`, and every component they define is loaded.
#[test]
fn schema_directives_under_an_http_base_reach_the_resolver_without_dot_segments() {
    let base = "http://example.com/x/";
    let resolver = logged(&uri_files(base));
    let result = run_sa(&format!("{base}s/main.xsl"), resolver.clone());
    let requests = resolver.requests();
    assert_eq!(
        result.unwrap(),
        "<out>true true true true</out>",
        "requests: {requests:?}"
    );
    for uri in &requests {
        assert!(uri.starts_with(base), "{uri} is not under {base}");
        assert!(
            !uri.contains("/../") && !uri.contains("/./"),
            "{uri} keeps a dot segment"
        );
    }
    assert!(
        requests.contains(&format!("{base}shared/sh.xsd")),
        "{requests:?}"
    );
    assert!(
        requests.contains(&format!("{base}shared/d.xsd")),
        "{requests:?}"
    );
}

/// A stylesheet named by a relative URI (no scheme) resolves its references relatively
/// (`resolve_uri`: "A base that is itself relative (no scheme) is outside RFC 3986 … it is merged the
/// same way") — the schema directives too: the resolver is asked for `s/parts/p.xsd` and
/// `shared/sh.xsd`, never for a `file:` URI made absolute against the process's working directory.
#[test]
fn schema_directives_under_a_relative_base_are_not_made_absolute_against_the_working_directory() {
    let resolver = logged(&uri_files(""));
    let result = run_sa("s/main.xsl", resolver.clone());
    let requests = resolver.requests();
    assert_eq!(
        result.unwrap(),
        "<out>true true true true</out>",
        "requests: {requests:?}"
    );
    for uri in &requests {
        assert!(!uri.contains(':'), "{uri} has a scheme");
        assert!(!uri.contains(".."), "{uri} keeps a dot segment");
    }
    assert!(
        requests.contains(&"s/parts/p.xsd".to_string()),
        "{requests:?}"
    );
    assert!(
        requests.contains(&"shared/sh.xsd".to_string()),
        "{requests:?}"
    );
}

/// When the resolver cannot read a directive's document, XML Schema 1.1 §4.2.3: "It is not an error
/// for the ·actual value· of the schemaLocation \[attribute\] to fail to resolve at all, in which
/// case the corresponding inclusion must not be performed", and XSLT 2.0 §3.14: "This will cause an
/// error only if it results in the stylesheet containing references to names that have not been
/// imported" — the context builds, and the reference is the error (XPath 2.0 XPST0051 for `cast as`).
#[test]
fn a_directive_the_resolver_cannot_read_is_an_error_only_at_a_reference() {
    let base = "http://example.com/x/";
    let mut files = uri_files(base);
    files.retain(|(uri, _)| !uri.ends_with("shared/sh.xsd"));
    let resolver = logged(&files);
    assert_eq!(
        run_sa(&format!("{base}s/main.xsl"), resolver).unwrap(),
        "<out>true true false true</out>"
    );
    let mut files = uri_files(base);
    files.retain(|(uri, _)| !uri.ends_with("shared/sh.xsd"));
    files[0].1 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:a" schema-location="a.xsd"/>{}"#,
        out("'x' cast as a:fromshared")
    ));
    let resolver = logged(&files);
    assert_eq!(
        failure(run_sa(&format!("{base}s/main.xsl"), resolver)),
        (Stage::Compile, "XPST0051".to_string())
    );
}

/// The one directive XML Schema does not let fail quietly — 1.1 §4.2.1: "For non-empty
/// `<redefine>` elements, it is an error for the attempt to fail; otherwise, the attempt must be
/// made but it is not an error for it to fail" — so a non-empty `xs:redefine` of a document the
/// resolver cannot read is XTSE0220 (§3.14: the synthetic schema document must "satisfy the
/// constraints described in \[XML Schema Part 1\]"), and an empty one is not.
#[test]
fn a_non_empty_redefine_of_a_document_the_resolver_cannot_read_is_xtse0220() {
    for (content, expected) in [
        (
            r#"<xs:simpleType name="t"><xs:restriction base="d:t"><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#,
            Some(codes::XTSE0220),
        ),
        ("", None),
    ] {
        let main = sheet(&format!(
            r#"<xsl:import-schema>
                 <xs:schema targetNamespace="urn:d"><xs:redefine schemaLocation="missing.xsd">{content}</xs:redefine></xs:schema>
               </xsl:import-schema>{}"#,
            out("type-available('d:t')")
        ));
        let resolver = memory(&[("file:///m/main.xsl", &main)]);
        match (context("file:///m/main.xsl", resolver), expected) {
            (Err(e), Some(code)) => assert_eq!(e.code, code, "{e}"),
            (Ok(_), None) => {}
            (Err(e), None) => panic!("an empty xs:redefine: unexpected {e}"),
            (Ok(_), Some(code)) => panic!("a non-empty xs:redefine: expected {code}"),
        }
    }
}

// ── R4: one inline schema reached twice ─────────────────────────────────

/// A module included twice (§3.10.2 forbids only a module including itself) holds one inline schema,
/// not two: it is loaded once — no XTSE0220 for "multiple definitions of the same name" — under the
/// location `<module>#schema-1` (M4 plan D-9: N counted from 1 per module), with `xsl:include` and
/// with `xsl:import` alike.
#[test]
fn an_inline_schema_of_a_module_reached_twice_is_loaded_once() {
    let common = sheet(
        r#"<xsl:import-schema>
             <xs:schema targetNamespace="urn:d"><xs:simpleType name="t"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>
           </xsl:import-schema>"#,
    );
    let middle = sheet(r#"<xsl:include href="common.xsl"/>"#);
    for directive in ["include", "import"] {
        let main = sheet(&format!(
            r#"<xsl:{directive} href="x.xsl"/><xsl:{directive} href="y.xsl"/>{}"#,
            out("type-available('d:t')")
        ));
        let resolver = memory(&[
            ("urn:m/main.xsl", &main),
            ("urn:m/x.xsl", &middle),
            ("urn:m/y.xsl", &middle),
            ("urn:m/common.xsl", &common),
        ]);
        assert_eq!(
            run_sa("urn:m/main.xsl", resolver.clone()).unwrap(),
            "<out>true</out>",
            "xsl:{directive}"
        );
        let context = context("urn:m/main.xsl", resolver).unwrap();
        let sources: Vec<&ImportSource> = context.imports().iter().map(|i| i.source()).collect();
        assert_eq!(
            sources,
            vec![&ImportSource::Inline {
                system_id: "urn:m/common.xsl#schema-1".to_string()
            }],
            "xsl:{directive}"
        );
    }
}

/// Two inline schemas of one module are `#schema-1` and `#schema-2` in document order, and a second
/// module's first one is its own `#schema-1`.
#[test]
fn inline_schemas_are_numbered_from_one_in_each_module() {
    let other =
        sheet(r#"<xsl:import-schema><xs:schema targetNamespace="urn:b"/></xsl:import-schema>"#);
    let main = sheet(&format!(
        r#"<xsl:include href="other.xsl"/>
           <xsl:import-schema><xs:schema targetNamespace="urn:a"/></xsl:import-schema>
           <xsl:import-schema><xs:schema targetNamespace="urn:d"/></xsl:import-schema>
           {}"#,
        out("1")
    ));
    let resolver = memory(&[("urn:m/main.xsl", &main), ("urn:m/other.xsl", &other)]);
    let context = context("urn:m/main.xsl", resolver).unwrap();
    let mut ids: Vec<String> = context
        .imports()
        .iter()
        .map(|import| match import.source() {
            ImportSource::Inline { system_id } => system_id.clone(),
            other => panic!("not inline: {other:?}"),
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "urn:m/main.xsl#schema-1",
            "urn:m/main.xsl#schema-2",
            "urn:m/other.xsl#schema-1"
        ]
    );
}

// ── R5, R9: one context, several compilations ───────────────────────────

const A_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:a"><xs:simpleType name="t"><xs:restriction base="xs:integer"><xs:maxInclusive value="5"/></xs:restriction></xs:simpleType></xs:schema>"#;
const B_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:b"><xs:simpleType name="u"><xs:restriction base="xs:integer"/></xs:simpleType></xs:schema>"#;

/// D-1: the caller owns the context, so one context serves several compilations — the stylesheet it
/// was built from, twice, and another that imports the same namespace by the same location. §3.14:
/// "The namespace attribute indicates that a schema for the given namespace is required by the
/// stylesheet. This information may be enough on its own to enable an implementation to locate the
/// required schema components" — a context that holds the namespace's components satisfies a
/// namespace-only import of it (D-F). An import by a location the context did not load, and one of
/// a namespace it holds nothing for, are still refused (`XSLT2-INTERNAL`).
#[test]
fn a_context_serves_every_stylesheet_whose_imports_it_holds() {
    let s1 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:a" schema-location="a.xsd"/>{}"#,
        out("6 castable as a:t")
    ));
    let s2 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:a" schema-location="a.xsd"/>{}"#,
        out("3 castable as a:t")
    ));
    let s3 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:b" schema-location="b.xsd"/>{}"#,
        out("3 castable as b:u")
    ));
    let s5 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:a"/>{}"#,
        out("3 castable as a:t")
    ));
    let s6 = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:b"/>{}"#,
        out("1")
    ));
    let resolver = memory(&[
        ("file:///r/s1.xsl", &s1),
        ("file:///r/s2.xsl", &s2),
        ("file:///r/s3.xsl", &s3),
        ("file:///r/s5.xsl", &s5),
        ("file:///r/s6.xsl", &s6),
        ("file:///r/a.xsd", A_XSD),
        ("file:///r/b.xsd", B_XSD),
    ]);
    let context = context("file:///r/s1.xsl", resolver.clone()).unwrap();
    for _ in 0..2 {
        assert_eq!(
            run_against(&context, "file:///r/s1.xsl", resolver.clone()).unwrap(),
            "<out>false</out>"
        );
    }
    assert_eq!(
        run_against(&context, "file:///r/s2.xsl", resolver.clone()).unwrap(),
        "<out>true</out>"
    );
    assert_eq!(
        run_against(&context, "file:///r/s5.xsl", resolver.clone()).unwrap(),
        "<out>true</out>"
    );
    for refused in ["file:///r/s3.xsl", "file:///r/s6.xsl"] {
        match run_against(&context, refused, resolver.clone()) {
            Err((Stage::Compile, e)) => assert!(e.is_internal(), "{refused}: {e}"),
            other => panic!("{refused}: expected XSLT2-INTERNAL, got {other:?}"),
        }
    }
}

// ── R6: the XML Schema and XML namespaces ───────────────────────────────

/// §3.13's Note: "A stylesheet that is designed to process XML Schema documents as its input or
/// output may import the Schema for Schemas" — this processor has no copy of it, so a namespace-only
/// import of the XML Schema namespace finds no schema document (`found()` is false) and
/// `schema-element(xs:schema)` names nothing (XPath 2.0 XPST0008); the built-in types are there
/// without any import (§3.13).
#[test]
fn a_namespace_only_import_of_the_xml_schema_namespace_finds_no_schema_document() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="http://www.w3.org/2001/XMLSchema"/>{}"#,
        out("type-available('xs:integer')")
    ));
    let resolver = memory(&[("urn:main", &main)]);
    let context = context("urn:main", resolver.clone()).unwrap();
    assert_eq!(context.imports().len(), 1);
    assert!(!context.imports()[0].found());
    assert_eq!(
        run_against(&context, "urn:main", resolver).unwrap(),
        "<out>true</out>"
    );
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="http://www.w3.org/2001/XMLSchema"/>{}"#,
        out("exists(//*[. instance of schema-element(xs:schema)])")
    ));
    let resolver = memory(&[("urn:main", &main)]);
    assert_eq!(
        failure(run_sa("urn:main", resolver)),
        (Stage::Compile, "XPST0008".to_string())
    );
}

/// The XML namespace's schema is known to this processor (the copy of `xml.xsd` `xsd-schema`
/// embeds, which it also uses for an `xs:import` of the namespace whose location cannot be read): an
/// `xsl:import-schema` naming the namespace and its published location
/// `http://www.w3.org/2001/xml.xsd`, which the resolver cannot read, uses that copy — as the
/// namespace-only form does — so `schema-attribute(xml:lang)` names an in-scope declaration.
#[test]
fn a_location_import_of_the_xml_namespace_falls_back_to_the_known_schema() {
    let probe = out("exists(/*/@xml:lang[. instance of schema-attribute(xml:lang)])");
    for import in [
        r#"<xsl:import-schema namespace="http://www.w3.org/XML/1998/namespace" schema-location="http://www.w3.org/2001/xml.xsd"/>"#,
        r#"<xsl:import-schema namespace="http://www.w3.org/XML/1998/namespace"/>"#,
    ] {
        let main = sheet(&format!("{import}{probe}"));
        let resolver = memory(&[("urn:main", &main)]);
        let context = context("urn:main", resolver.clone()).unwrap();
        assert!(context.imports()[0].found(), "{import}");
        assert_eq!(
            run_against(&context, "urn:main", resolver).unwrap(),
            "<out>false</out>",
            "{import}"
        );
    }
}

// ── R9: paths the first round left untested ─────────────────────────────

/// §3.14: "The base URI of the `xs:import` element is the same as the base URI of the
/// `xsl:import-schema` declaration" — `xml:base` on an inline schema's `xsl:import-schema` (or on the
/// `xs:schema` itself) moves where its `xs:include` resolves.
#[test]
fn xml_base_moves_where_an_inline_schema_resolves_its_directives() {
    let included = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:d"><xs:simpleType name="inc"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>"#;
    for (on_import, on_schema) in [(r#" xml:base="sub/""#, ""), ("", r#" xml:base="sub/""#)] {
        let main = sheet(&format!(
            r#"<xsl:import-schema namespace="urn:d"{on_import}>
                 <xs:schema targetNamespace="urn:d"{on_schema}><xs:include schemaLocation="inc.xsd"/></xs:schema>
               </xsl:import-schema>{}"#,
            out("type-available('d:inc')")
        ));
        let resolver = memory(&[
            ("file:///m/main.xsl", &main),
            ("file:///m/sub/inc.xsd", included),
        ]);
        assert_eq!(
            run_sa("file:///m/main.xsl", resolver).unwrap(),
            "<out>true</out>",
            "{on_import}{on_schema}"
        );
    }
}

/// > \[ERR XTSE1650\] A basic XSLT processor must signal a static error if the stylesheet includes an
/// > `xsl:import-schema` declaration.
///
/// The stylesheet includes the declarations of every module it imports — and none that §3.12
/// excludes: by `use-when` on the `xsl:import` that would load the module, or on the imported
/// module's `xsl:stylesheet` ("the effect is to exclude all the children of the xsl:stylesheet").
#[test]
fn a_basic_processor_rejects_an_import_schema_of_an_imported_module() {
    let import_schema = r#"<xsl:import-schema namespace="urn:a" schema-location="a.xsd"/>"#;
    let imported = sheet(import_schema);
    let excluded_module = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{XSL}\" \
           use-when=\"system-property('xsl:is-schema-aware') = 'yes'\">{import_schema}</xsl:stylesheet>"
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    for (directive, expected) in [
        (
            r#"<xsl:import href="imported.xsl"/>"#,
            Some(codes::XTSE1650),
        ),
        (
            r#"<xsl:import href="imported.xsl" use-when="system-property('xsl:is-schema-aware') = 'yes'"/>"#,
            None,
        ),
        (r#"<xsl:import href="excluded.xsl"/>"#, None),
    ] {
        let main = sheet(&format!("{directive}{}", out("1")));
        let resolver = memory(&[
            ("file:///b/main.xsl", &main),
            ("file:///b/imported.xsl", &imported),
            ("file:///b/excluded.xsl", &excluded_module),
            ("file:///b/a.xsd", A_XSD),
        ]);
        let result = Compiler::new(&names, &functions)
            .with_resolver(resolver)
            .compile_uri("file:///b/main.xsl");
        match (result, expected) {
            (Err(e), Some(code)) => {
                assert_eq!(e.code, code, "{e}");
                let location = e.location.expect("a location");
                assert_eq!(
                    location.module_uri.as_deref(),
                    Some("file:///b/imported.xsl")
                );
            }
            (Ok(_), None) => {}
            (Err(e), None) => panic!("{directive}: unexpected {e}"),
            (Ok(_), Some(code)) => panic!("{directive}: expected {code}"),
        }
    }
}
