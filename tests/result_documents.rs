//! final result trees: `xsl:result-document` (XSLT 2.0 §19.1), the base
//! output URI (§2.3), the implicit result tree (§2.4), output states (XTDE1480)
//! and URI clashes (XTDE1490).
//!
//! Every test goes through the public API — compile, transform,
//! `Outcome::secondary`, `serialize_primary` / `serialize_secondary` — and
//! cites the rule it checks.

mod common;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, QName, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const DECL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;
/// The stylesheet's base URI: the default base output URI.
const SHEET_URI: &str = "file:///results/style/main.xsl";

fn sheet(body: &str) -> String {
    format!("<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>")
}

/// `content` as the template matching `/`, after the declarations `decls`.
fn root(decls: &str, content: &str) -> String {
    sheet(&format!(
        r#"{decls}<xsl:template match="/">{content}</xsl:template>"#
    ))
}

/// What a run produced, serialized: the primary result (if there is one) and
/// every secondary result under its URI, in the order of `Outcome::secondary`.
#[derive(Debug)]
struct Results {
    primary: Option<String>,
    base_output_uri: String,
    secondary: Vec<(String, String)>,
}

impl Results {
    fn secondary(&self, uri: &str) -> &str {
        self.secondary
            .iter()
            .find(|(u, _)| u == uri)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| panic!("no secondary result {uri}; have {:?}", self.secondary))
    }
}

/// Where a run failed.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Static(String),
    Dynamic(String),
}

/// Compiles `text`, runs it over `<doc/>` (or from the initial template
/// `initial` with no source) with `options`, and serializes every result.
fn run_full(
    text: &str,
    options: TransformOptions,
    initial: Option<&str>,
) -> Result<Results, Failure> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, SHEET_URI)
        .map_err(|e| Failure::Static(e.code))?;
    let arena = bumpalo::Bump::new();
    let dynamic = |e: XsltError| Failure::Dynamic(e.code);
    let mut run = compiled.transformation(&arena, options);
    let outcome = match initial {
        Some(name) => {
            run = run.with_initial_template(QName::local(name));
            run.transform_without_source()
        }
        None => run.transform_source("file:///results/in/doc.xml", "<doc/>"),
    }
    .map_err(dynamic)?;
    let primary = match outcome.primary {
        Some(_) => Some(compiled.serialize_primary(&outcome).map_err(dynamic)?),
        None => None,
    };
    let secondary = outcome
        .secondary
        .iter()
        .map(|tree| Ok((tree.uri.clone(), compiled.serialize_secondary(tree)?)))
        .collect::<Result<Vec<_>, XsltError>>()
        .map_err(dynamic)?;
    Ok(Results {
        primary,
        base_output_uri: outcome.base_output_uri.clone(),
        secondary,
    })
}

fn run(text: &str) -> Result<Results, Failure> {
    run_full(text, TransformOptions::default(), None)
}

fn ok(text: &str) -> Results {
    run(text).unwrap_or_else(|f| panic!("{f:?}"))
}

fn static_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Static(code)) => code,
        other => panic!("expected a static error, got {other:?}"),
    }
}

fn dynamic_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Dynamic(code)) => code,
        other => panic!("expected a dynamic error, got {other:?}"),
    }
}

// ═══ §19.1 the instruction ═══════════════════════════════════════════════

/// §18.1.1: `element-available` "returns true if and only if … the
/// expanded-QName identifies an instruction that the implementation supports".
#[test]
fn element_available_is_true_for_result_document() {
    let r = ok(&root(
        "",
        r#"<out><xsl:value-of select="element-available('xsl:result-document')"/></out>"#,
    ));
    assert_eq!(r.primary.clone().unwrap(), format!("{DECL}<out>true</out>"));
}

/// §19.1: "The xsl:result-document instruction is used to create a final
/// result tree. … A document node is created, and the sequence obtained by
/// evaluating the sequence constructor is used to construct the content of the
/// document"; "Technically, the result of evaluating the xsl:result-document
/// instruction is an empty sequence. This means it does not contribute any
/// nodes to the result of the sequence constructor it is part of."
#[test]
fn a_result_document_is_a_tree_of_its_own_and_contributes_nothing() {
    let r = ok(&root(
        "",
        r#"<out><a/><xsl:result-document href="r.xml"><r><xsl:value-of select="name(/*)"/></r></xsl:result-document><b/></out>"#,
    ));
    assert_eq!(
        r.primary.clone().unwrap(),
        format!("{DECL}<out><a/><b/></out>")
    );
    assert_eq!(
        r.secondary,
        [(
            "file:///results/style/r.xml".to_string(),
            format!("{DECL}<r>doc</r>")
        )]
    );
}

/// §19.1: "If the effective value is a relative URI, then it is resolved
/// relative to the base output URI" — `TransformOptions::base_output_uri`;
/// "The base URI of the document node at the root of the final result tree is
/// based on the effective value of the href attribute"; "If the implementation
/// provides an API to access final result trees, then it must allow a final
/// result tree to be identified by means of this base URI"
/// (`Outcome::result_document`). An absolute href is taken as it is, and the
/// href is an attribute value template.
#[test]
fn a_relative_href_resolves_against_the_base_output_uri() {
    let text = root(
        "",
        r#"<xsl:result-document href="sub/{name(/*)}.xml"><a/></xsl:result-document>
           <xsl:result-document href="../up.xml"><b/></xsl:result-document>
           <xsl:result-document href="urn:x:abs"><c/></xsl:result-document>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, SHEET_URI)
        .unwrap();
    let arena = bumpalo::Bump::new();
    let options = TransformOptions {
        base_output_uri: Some("file:///out/dir/main.out".into()),
        ..Default::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .transform_source("file:///results/in/doc.xml", "<doc/>")
        .unwrap();
    let uris: Vec<&str> = outcome.secondary.iter().map(|t| t.uri.as_str()).collect();
    assert_eq!(
        uris,
        [
            "file:///out/dir/sub/doc.xml",
            "file:///out/up.xml",
            "urn:x:abs"
        ]
    );
    assert_eq!(outcome.base_output_uri, "file:///out/dir/main.out");
    let tree = outcome
        .result_document("file:///out/dir/sub/doc.xml")
        .expect("found by its URI");
    assert_eq!(
        tree.document.document_base_uri(),
        Some("file:///out/dir/sub/doc.xml")
    );
    assert!(outcome.result_document("sub/doc.xml").is_none());
}

/// `TransformOptions::base_output_uri` absent: the base URI of the principal
/// stylesheet module (§2.3: "The way in which a base output URI is established
/// is implementation-defined") — not the source document's.
#[test]
fn the_default_base_output_uri_is_the_principal_stylesheet_module() {
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="r.xml"><r/></xsl:result-document>"#,
    ));
    assert_eq!(r.base_output_uri, SHEET_URI);
    assert_eq!(r.secondary[0].0, "file:///results/style/r.xml");
}

/// The base URI of the result's nodes follows its document node (§19.1), and
/// an `xml:base` inside it resolves against it — what `result-document-0102`
/// asserts with `base-uri()`. Checked here through the tree's navigator.
#[test]
fn the_result_tree_carries_its_uri_as_base_uri() {
    let text = root(
        "",
        r#"<xsl:result-document href="out/second.xml"><two><l xml:base="in/third.xml"/></two></xsl:result-document>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, SHEET_URI)
        .unwrap();
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///results/in/doc.xml", "<doc/>")
        .unwrap();
    let tree = &outcome.secondary[0];
    assert_eq!(tree.uri, "file:///results/style/out/second.xml");
    assert_eq!(
        tree.document.document_base_uri(),
        Some("file:///results/style/out/second.xml")
    );
}

/// Secondary results come in the order their instructions were evaluated — an
/// enclosing instruction before the one nested in it, though the nested tree is
/// finished first.
#[test]
fn secondary_results_are_in_evaluation_order() {
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="outer.xml"><o><xsl:result-document href="inner.xml"><i/></xsl:result-document></o></xsl:result-document>
           <xsl:for-each select="1 to 2"><xsl:result-document href="n{.}.xml"><n/></xsl:result-document></xsl:for-each>"#,
    ));
    let uris: Vec<&str> = r.secondary.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(
        uris,
        [
            "file:///results/style/outer.xml",
            "file:///results/style/inner.xml",
            "file:///results/style/n1.xml",
            "file:///results/style/n2.xml"
        ]
    );
    assert_eq!(
        r.secondary("file:///results/style/outer.xml"),
        format!("{DECL}<o/>")
    );
}

// ═══ §19.1 format and the serialization attributes ═══════════════════════

/// §19.1: "The expanded-QName must match the expanded QName of a named output
/// definition in the stylesheet. This identifies the xsl:output declaration
/// that will control the serialization"; "If the format attribute is omitted,
/// the unnamed output definition is used"; the QName "is expanded using the
/// namespace declarations in scope for the xsl:result-document element".
#[test]
fn format_selects_a_named_output_definition() {
    let r = ok(&root(
        r#"<xsl:output method="text"/>
           <xsl:output name="p:fmt" method="xml" omit-xml-declaration="yes" xmlns:p="urn:p"/>"#,
        r#"<xsl:result-document href="a.xml" format="q:fmt" xmlns:q="urn:p" exclude-result-prefixes="q"><a>1</a></xsl:result-document>
           <xsl:result-document href="b.txt"><b>2</b></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary("file:///results/style/a.xml"), "<a>1</a>");
    assert_eq!(r.secondary("file:///results/style/b.txt"), "2");
}

/// §19.1: "In all other cases, the effective value of an attribute actually
/// present on this instruction takes precedence over the value defined in the
/// selected output definition" — and the attributes are attribute value
/// templates.
#[test]
fn instruction_attributes_override_the_selected_definition() {
    let r = ok(&root(
        r#"<xsl:output name="f" method="text" indent="yes"/>"#,
        r#"<xsl:result-document href="a.xml" format="f" method="{'xml'}" omit-xml-declaration="yes" indent="no"><a><b/></a></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary("file:///results/style/a.xml"), "<a><b/></a>");
}

/// §19.1: "In the case of cdata-section-elements, the value of the
/// serialization parameter is the union of the expanded names of the elements
/// named in this instruction and the elements named in the selected output
/// definition"; "In the case of cdata-section-elements, an unprefixed element
/// name is expanded using the default namespace."
#[test]
fn cdata_section_elements_is_the_union() {
    let r = ok(&root(
        r#"<xsl:output name="f" omit-xml-declaration="yes" cdata-section-elements="a"/>"#,
        r#"<xsl:result-document href="r.xml" format="f" cdata-section-elements="b {'c'}"><r><a>&lt;</a><b>&lt;</b><c>&lt;</c><d>&lt;</d></r></xsl:result-document>
           <xsl:result-document href="n.xml" format="f" cdata-section-elements="e" xmlns="urn:d"><r xmlns=""><e>1</e><e xmlns="urn:d">2</e></r></xsl:result-document>"#,
    ));
    assert_eq!(
        r.secondary("file:///results/style/r.xml"),
        "<r><a><![CDATA[<]]></a><b><![CDATA[<]]></b><c><![CDATA[<]]></c><d>&lt;</d></r>"
    );
    assert_eq!(
        r.secondary("file:///results/style/n.xml"),
        r#"<r><e>1</e><e xmlns="urn:d"><![CDATA[2]]></e></r>"#
    );
}

/// §19.1: "In the case of use-character-maps, the character maps referenced
/// in this instruction supplement and take precedence over those defined in
/// the selected output definition" (§20.1: "the last character mapping takes
/// precedence").
#[test]
fn character_maps_supplement_and_take_precedence() {
    let r = ok(&root(
        r#"<xsl:character-map name="map1"><xsl:output-character character="a" string="1"/><xsl:output-character character="b" string="2"/></xsl:character-map>
           <xsl:character-map name="map2"><xsl:output-character character="a" string="X"/><xsl:output-character character="c" string="Y"/></xsl:character-map>
           <xsl:output name="f" omit-xml-declaration="yes" use-character-maps="map1"/>"#,
        r#"<xsl:result-document href="r.xml" format="f" use-character-maps="map2"><r>abcd</r></xsl:result-document>
           <xsl:result-document href="s.xml" format="f"><r>abcd</r></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary("file:///results/style/r.xml"), "<r>X2Yd</r>");
    assert_eq!(r.secondary("file:///results/style/s.xml"), "<r>12cd</r>");
}

/// §19.1: "In the case of the attributes doctype-system and doctype-public,
/// setting the effective value of the attribute to a zero-length string has the
/// effect of overriding any value for these attributes obtained from the output
/// definition. The corresponding serialization parameter is not set (is
/// "absent")."
#[test]
fn an_empty_doctype_overrides_to_absent() {
    let r = ok(&root(
        r#"<xsl:output name="f" omit-xml-declaration="yes" doctype-system="x.dtd" doctype-public="-//X//EN"/>"#,
        r#"<xsl:result-document href="a.xml" format="f" doctype-system="" doctype-public="{''}"><a/></xsl:result-document>
           <xsl:result-document href="b.xml" format="f"><b/></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary("file:///results/style/a.xml"), "<a/>");
    assert_eq!(
        r.secondary("file:///results/style/b.xml"),
        "<!DOCTYPE b PUBLIC \"-//X//EN\" \"x.dtd\">\n<b/>"
    );
}

/// §19.1: "The output-version attribute on the xsl:result-document instruction
/// overrides the version attribute on xsl:output (it has been renamed because
/// version is available with a different meaning as a standard attribute"). XML
/// 1.1 is not supported (SESU0013), so the override is what makes the second
/// tree serializable.
#[test]
fn output_version_overrides_version() {
    let text = root(
        r#"<xsl:output name="f" version="1.1"/>"#,
        r#"<xsl:result-document href="a.xml" format="f" output-version="1.0" version="2.0"><a/></xsl:result-document>"#,
    );
    assert_eq!(
        ok(&text).secondary("file:///results/style/a.xml"),
        format!("{DECL}<a/>")
    );
    let without = root(
        r#"<xsl:output name="f" version="1.1"/>"#,
        r#"<xsl:result-document href="a.xml" format="f"><a/></xsl:result-document>"#,
    );
    assert_eq!(dynamic_error(&without), "SESU0013");
}

/// §2.2: "\[ERR XTSE0020\] It is a static error if an attribute (other than an
/// attribute written using curly brackets in a position where an attribute
/// value template is permitted) contains a value that is not one of the
/// permitted values for that attribute"; "\[ERR XTDE0030\] It is a
/// non-recoverable dynamic error if the effective value of an attribute
/// written using curly brackets … is a value that is not one of the permitted
/// values for that attribute." `result-document-0276`: `include-content-type="Yes"`.
#[test]
fn a_bad_serialization_value_is_xtse0020_written_and_xtde0030_computed() {
    for attribute in [
        r#"include-content-type="Yes""#,
        r#"method="adaptive""#,
        r#"standalone="maybe""#,
        r#"output-version="1 0""#,
    ] {
        let text = root(
            "",
            &format!(r#"<xsl:result-document href="a.xml" {attribute}><a/></xsl:result-document>"#),
        );
        assert_eq!(static_error(&text), "XTSE0020", "{attribute}");
    }
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml" indent="{'maybe'}"><a/></xsl:result-document>"#,
    );
    assert_eq!(dynamic_error(&text), "XTDE0030");
    // An unbound prefix in a computed QName is the more specific XTDE0290 (§5.1).
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml" method="{'nope:m'}"><a/></xsl:result-document>"#,
    );
    assert_eq!(dynamic_error(&text), "XTDE0290");
}

/// §19.1: "If a value is supplied for an attribute that is inapplicable to the
/// output method, its value is not passed to the serializer"
/// (`result-document-0222`, `-0237`).
#[test]
fn inapplicable_attributes_are_dropped() {
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="a.txt" method="text" doctype-system="x.dtd" output-version="5.0" indent="yes"><a>hello</a></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary("file:///results/style/a.txt"), "hello");
}

/// §20: "if the version attribute of the xsl:stylesheet element of the
/// principal stylesheet module has the value 1.0, and if the result tree is
/// generated implicitly (rather than by an explicit xsl:result-document
/// instruction), then the default output method in this situation is xml" —
/// so an explicit result document of such a stylesheet is `xhtml`.
#[test]
fn an_explicit_xhtml_result_of_a_1_0_stylesheet_keeps_the_xhtml_default() {
    let text = format!(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="{NS}">
             <xsl:output omit-xml-declaration="yes"/>
             <xsl:template match="/">
               <html xmlns="http://www.w3.org/1999/xhtml"><br/></html>
               <xsl:result-document href="x.html"><html xmlns="http://www.w3.org/1999/xhtml"><br/></html></xsl:result-document>
             </xsl:template>
           </xsl:stylesheet>"#
    );
    let r = ok(&text);
    assert_eq!(
        r.primary.clone().unwrap(),
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><br/></html>"#
    );
    assert_eq!(
        r.secondary("file:///results/style/x.html"),
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><br /></html>"#
    );
}

/// `serialize_secondary_to_bytes` performs the encoding phase with the
/// effective `encoding` (Serialization §4).
#[test]
fn a_secondary_result_encodes_in_its_own_encoding() {
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml" encoding="ISO-8859-1" omit-xml-declaration="yes"><a>é€</a></xsl:result-document>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, SHEET_URI)
        .unwrap();
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///results/in/doc.xml", "<doc/>")
        .unwrap();
    let tree = &outcome.secondary[0];
    assert_eq!(
        compiled.serialize_secondary_to_bytes(tree).unwrap(),
        b"<a>\xE9&#8364;</a>"
    );
    let params = compiled.secondary_serialization_params(tree).unwrap();
    assert_eq!(params.method, xslt2::output::ResolvedMethod::Xml);
}

// ═══ XTDE1460 ═════════════════════════════════════════════════════════════

/// "\[ERR XTDE1460\] It is a non-recoverable dynamic error if the effective
/// value of the format attribute is not a valid lexical QName, or if it does
/// not match the expanded-QName of an output definition in the stylesheet. If
/// the processor is able to detect the error statically (for example, when the
/// format attribute contains no curly brackets), then the processor may
/// optionally signal this as a static error." Signalled statically without
/// curly brackets — even in a template that never runs — and dynamically with
/// them.
#[test]
fn an_unknown_format_is_xtde1460_static_or_dynamic() {
    let unused = |format: &str| {
        sheet(&format!(
            r#"<xsl:output name="known"/>
               <xsl:template match="/"><out/></xsl:template>
               <xsl:template name="never"><xsl:result-document format="{format}"><a/></xsl:result-document></xsl:template>"#
        ))
    };
    assert_eq!(static_error(&unused("unknown")), "XTDE1460");
    assert_eq!(static_error(&unused("not:a:qname")), "XTDE1460");
    assert_eq!(static_error(&unused("nobody:known")), "XTDE1460");
    assert!(run(&unused("known")).is_ok());

    let computed = |format: &str| {
        root(
            r#"<xsl:output name="known"/>"#,
            &format!(
                r#"<xsl:result-document href="a.xml" format="{format}"><a/></xsl:result-document>"#
            ),
        )
    };
    assert_eq!(dynamic_error(&computed("{'unknown'}")), "XTDE1460");
    assert_eq!(dynamic_error(&computed("{'a:b:c'}")), "XTDE1460");
    assert_eq!(dynamic_error(&computed("{'your:known'}")), "XTDE1460");
    assert!(run(&computed("{'known'}")).is_ok());
}

/// §20.1's XTSE1590 for a `use-character-maps` name no `xsl:character-map`
/// declares, on the instruction as on `xsl:output`.
#[test]
fn an_unknown_character_map_is_xtse1590() {
    let text = sheet(
        r#"<xsl:template match="/"><out/></xsl:template>
           <xsl:template name="never"><xsl:result-document use-character-maps="none"><a/></xsl:result-document></xsl:template>"#,
    );
    assert_eq!(static_error(&text), "XTSE1590");
}

/// §19.1: "The optional attributes type and validation may be used on the
/// xsl:result-document instruction to validate the contents of the new
/// document" — schema-aware processing. For a basic processor, §21.1:
/// "\[ERR XTSE1660\] A basic XSLT processor must signal a static error if the
/// stylesheet includes an \[xsl:\]type attribute, or an \[xsl:\]validation or
/// default-validation attribute with a value other than strip", as for
/// `xsl:document`.
#[test]
fn validation_and_type_follow_the_basic_processor_rule() {
    for attribute in [
        r#"validation="strict""#,
        r#"validation="preserve""#,
        r#"type="xs:string""#,
    ] {
        let text = root(
            "",
            &format!(
                r#"<xsl:result-document href="a.xml" {attribute} xmlns:xs="http://www.w3.org/2001/XMLSchema"><a/></xsl:result-document>"#
            ),
        );
        assert_eq!(static_error(&text), "XTSE1660", "{attribute}");
    }
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="a.xml" validation="strip"><a/></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary.len(), 1);
}

// ═══ XTDE1480: output states ═════════════════════════════════════════════

/// A named template `rd` that evaluates `xsl:result-document`, the template
/// `main` with `content`, and `decls`.
fn temporary(decls: &str, content: &str) -> String {
    sheet(&format!(
        r#"{decls}
           <xsl:template name="rd"><xsl:result-document href="t.xml"><t/></xsl:result-document><xsl:text>x</xsl:text></xsl:template>
           <xsl:template match="/"><out>{content}</out></xsl:template>"#
    ))
}

/// §19.1: "The instructions in the initial template are evaluated in final
/// output state. An instruction is evaluated in the same output state as its
/// calling instruction, except that xsl:variable, xsl:param, xsl:with-param,
/// xsl:attribute, xsl:comment, xsl:processing-instruction, xsl:namespace,
/// xsl:value-of, xsl:function, xsl:key, xsl:sort, and xsl:message always
/// evaluate the instructions in their contained sequence constructor in
/// temporary output state." "\[ERR XTDE1480\] It is a non-recoverable dynamic
/// error to evaluate the xsl:result-document instruction in temporary output
/// state." The state follows the call: `rd` itself is innocent.
#[test]
fn result_document_in_temporary_output_state_is_xtde1480() {
    let cases: [(&str, &str, &str); 12] = [
        (
            "xsl:variable",
            "",
            r#"<xsl:variable name="v"><xsl:call-template name="rd"/></xsl:variable><xsl:sequence select="$v"/>"#,
        ),
        (
            "xsl:with-param",
            r#"<xsl:template name="p"><xsl:param name="n"/><xsl:value-of select="$n"/></xsl:template>"#,
            r#"<xsl:call-template name="p"><xsl:with-param name="n"><xsl:call-template name="rd"/></xsl:with-param></xsl:call-template>"#,
        ),
        (
            "xsl:param",
            r#"<xsl:template name="p"><xsl:param name="n"><xsl:call-template name="rd"/></xsl:param><xsl:value-of select="$n"/></xsl:template>"#,
            r#"<xsl:call-template name="p"/>"#,
        ),
        (
            "xsl:attribute",
            "",
            r#"<xsl:attribute name="a"><xsl:call-template name="rd"/></xsl:attribute>"#,
        ),
        (
            "xsl:comment",
            "",
            r#"<xsl:comment><xsl:call-template name="rd"/></xsl:comment>"#,
        ),
        (
            "xsl:processing-instruction",
            "",
            r#"<xsl:processing-instruction name="pi"><xsl:call-template name="rd"/></xsl:processing-instruction>"#,
        ),
        (
            "xsl:namespace",
            "",
            r#"<xsl:namespace name="n"><xsl:call-template name="rd"/></xsl:namespace>"#,
        ),
        (
            "xsl:value-of",
            "",
            r#"<xsl:value-of><xsl:call-template name="rd"/></xsl:value-of>"#,
        ),
        (
            "xsl:message",
            "",
            r#"<xsl:message><xsl:call-template name="rd"/></xsl:message>"#,
        ),
        (
            "xsl:sort",
            "",
            r#"<xsl:perform-sort select="1 to 3"><xsl:sort><xsl:call-template name="rd"/></xsl:sort></xsl:perform-sort>"#,
        ),
        (
            "xsl:function",
            r#"<xsl:function name="f:f" xmlns:f="urn:f"><xsl:call-template name="rd"/></xsl:function>"#,
            r#"<xsl:value-of select="f:f()" xmlns:f="urn:f"/>"#,
        ),
        (
            "xsl:key",
            r#"<xsl:key name="k" match="*"><xsl:call-template name="rd"/></xsl:key>"#,
            r#"<xsl:value-of select="count(key('k', 'x'))"/>"#,
        ),
    ];
    for (construct, decls, content) in cases {
        assert_eq!(
            dynamic_error(&temporary(decls, content)),
            "XTDE1480",
            "{construct}"
        );
    }
}

/// §11.5: "The temporary tree rooted at this document node forms the result
/// tree" of `xsl:document`, and temporary output state "applies when
/// instructions are writing to a temporary tree or any other non-final
/// destination". A global variable is the same (§9.5's value is a temporary
/// tree or a sequence, never a final result tree), and so is a template called
/// from inside a variable: the state is that of the calling instruction.
#[test]
fn xsl_document_global_variables_and_calls_from_variables_are_temporary() {
    assert_eq!(
        dynamic_error(&temporary(
            "",
            r#"<xsl:document><xsl:call-template name="rd"/></xsl:document>"#
        )),
        "XTDE1480"
    );
    assert_eq!(
        dynamic_error(&temporary(
            r#"<xsl:variable name="g"><xsl:call-template name="rd"/></xsl:variable>"#,
            r#"<xsl:value-of select="$g"/>"#
        )),
        "XTDE1480"
    );
    assert_eq!(
        dynamic_error(&temporary(
            r#"<xsl:template match="doc"><xsl:call-template name="rd"/></xsl:template>"#,
            r#"<xsl:variable name="v"><xsl:apply-templates select="doc"/></xsl:variable><xsl:value-of select="$v"/>"#
        )),
        "XTDE1480"
    );
}

/// The instructions §19.1 does not list keep the caller's state: element
/// construction, iteration, conditionals, template calls, `xsl:copy`,
/// `xsl:perform-sort`'s sequence constructor — and `xsl:result-document`
/// itself, so one may be nested in another (`result-document-0205`).
#[test]
fn final_output_state_follows_calls_and_constructors() {
    let r = ok(&temporary(
        r#"<xsl:template match="doc"><xsl:copy><xsl:call-template name="rd"/></xsl:copy></xsl:template>"#,
        r#"<xsl:for-each select="/"><xsl:if test="true()"><e><xsl:apply-templates select="doc"/></e></xsl:if></xsl:for-each>"#,
    ));
    assert_eq!(
        r.primary.clone().unwrap(),
        format!("{DECL}<out><e><doc>x</doc></e></out>")
    );
    assert_eq!(
        r.secondary("file:///results/style/t.xml"),
        format!("{DECL}<t/>")
    );

    let r = ok(&root(
        "",
        r#"<xsl:perform-sort><xsl:sort select="."/><xsl:result-document href="p.xml"><p/></xsl:result-document></xsl:perform-sort>
           <xsl:result-document href="outer.xml"><o><xsl:result-document href="inner.xml"><i/></xsl:result-document></o></xsl:result-document>"#,
    ));
    assert_eq!(r.secondary.len(), 3);
    assert_eq!(
        r.secondary("file:///results/style/outer.xml"),
        format!("{DECL}<o/>")
    );
}

// ═══ XTDE1490 and §2.4 ════════════════════════════════════════════════════

/// "\[ERR XTDE1490\] It is a non-recoverable dynamic error for a
/// transformation to generate two or more final result trees with the same
/// URI." The URIs are compared once resolved: `./a.xml` is `a.xml`.
#[test]
fn two_trees_at_one_uri_are_xtde1490() {
    for (first, second) in [
        ("a.xml", "a.xml"),
        ("a.xml", "./a.xml"),
        ("a.xml", "file:///results/style/a.xml"),
        ("sub/../a.xml", "a.xml"),
    ] {
        let text = root(
            "",
            &format!(
                r#"<xsl:result-document href="{first}"><a/></xsl:result-document><xsl:result-document href="{{'{second}'}}"><b/></xsl:result-document>"#
            ),
        );
        assert_eq!(dynamic_error(&text), "XTDE1490", "{first} / {second}");
    }
}

/// The Note to XTDE1490: "this means that it is an error to evaluate more than
/// one xsl:result-document instruction that omits the href attribute, or to
/// evaluate any xsl:result-document instruction that omits the href attribute
/// if an initial final result tree is created implicitly." §2.4: "If the result
/// of the initial template is non-empty, and an explicit xsl:result-document
/// instruction has been evaluated with the empty attribute href="", then an
/// error will occur" — whichever comes first, and whether the result-document
/// is nested in the implicit tree's content or not.
#[test]
fn omitted_hrefs_clash_with_each_other_and_with_the_implicit_tree() {
    for content in [
        // Two omitted hrefs (`result-document-1004`).
        r#"<xsl:result-document><a/></xsl:result-document><xsl:result-document href=""><b/></xsl:result-document>"#,
        // Implicit content first (`-1001`, `-1003`).
        r#"<out/><xsl:result-document><a/></xsl:result-document>"#,
        // Implicit content after (`-1002`).
        r#"<xsl:result-document><a/></xsl:result-document><out/>"#,
        // Nested in the implicit tree's content (`error-1490c`, `-1005`).
        r#"<out><xsl:result-document><a/></xsl:result-document></out>"#,
        // Text alone is a non-empty result.
        r#"<xsl:result-document><a/></xsl:result-document><xsl:text>t</xsl:text>"#,
    ] {
        assert_eq!(dynamic_error(&root("", content)), "XTDE1490", "{content}");
    }
}

/// §2.4: "An implicit result tree is also created when the result sequence is
/// empty, provided that no xsl:result-document instruction has been evaluated
/// during the course of the transformation. In this situation the implicit
/// result tree will consist of a document node with no children." "It also
/// means that if the content of the initial template is a single
/// xsl:result-document instruction, as in the example above, then only one
/// result tree is produced, not two" — it is the primary result, serialized
/// with its own output definition (`result-document-0216`); with an href that
/// is not the base output URI there is no primary result at all.
#[test]
fn the_implicit_result_tree_follows_section_2_4() {
    // Empty result, no xsl:result-document: an empty primary result.
    let r = ok(&root(r#"<xsl:output omit-xml-declaration="yes"/>"#, ""));
    assert_eq!(r.primary.as_deref(), Some(""));
    assert!(r.secondary.is_empty());

    // Only xsl:result-document without href: it is the primary result.
    let r = ok(&root(
        r#"<xsl:output name="t" method="text"/>"#,
        r#"<xsl:result-document format="t"><a>hello</a></xsl:result-document>"#,
    ));
    assert_eq!(r.primary.as_deref(), Some("hello"));
    assert!(r.secondary.is_empty());

    // Its href may name the base output URI explicitly.
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="main.xsl" method="text"><a>same</a></xsl:result-document>"#,
    ));
    assert_eq!(r.primary.as_deref(), Some("same"));

    // Only xsl:result-document elsewhere: no primary result.
    let r = ok(&root(
        "",
        r#"<xsl:result-document href="a.xml"><a/></xsl:result-document>"#,
    ));
    assert_eq!(r.primary, None);
    assert_eq!(r.secondary.len(), 1);

    // A nested xsl:result-document without href writes the primary result
    // after a secondary one (`result-document-0205`).
    let r = ok(&root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<xsl:result-document href="s.xml"><s><xsl:result-document><p/></xsl:result-document></s></xsl:result-document>"#,
    ));
    assert_eq!(r.primary.as_deref(), Some("<p/>"));
    assert_eq!(r.secondary("file:///results/style/s.xml"), "<s/>");
}

/// The same rules with an initial named template and no source document, and
/// a base output URI from the options.
#[test]
fn the_rules_hold_from_an_initial_template() {
    let text = sheet(
        r#"<xsl:template name="main"><xsl:result-document href="x/y.xml"><y/></xsl:result-document></xsl:template>"#,
    );
    let options = TransformOptions {
        base_output_uri: Some("file:///base/".into()),
        ..Default::default()
    };
    let r = run_full(&text, options, Some("main")).unwrap();
    assert_eq!(r.primary, None);
    assert_eq!(r.base_output_uri, "file:///base/");
    assert_eq!(r.secondary("file:///base/x/y.xml"), format!("{DECL}<y/>"));
}

// ═══ §20.2 disable-output-escaping ═══════════════════════════════════════

/// §20.2: d-o-e is honoured in a final result tree — the content of
/// `xsl:result-document` is in final output state — with marks kept per tree:
/// the secondary's do not leak into the primary result, nor the primary's into
/// the secondary.
#[test]
fn disable_output_escaping_works_in_a_secondary_result() {
    let r = ok(&root(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<out><xsl:text disable-output-escaping="yes">&lt;p/&gt;</xsl:text><xsl:text>&lt;q/&gt;</xsl:text></out>
           <xsl:result-document href="s.xml" omit-xml-declaration="yes"><s><xsl:text>&lt;x/&gt;</xsl:text><xsl:value-of select="'&lt;y/&gt;'" disable-output-escaping="yes"/></s></xsl:result-document>"#,
    ));
    assert_eq!(r.primary.clone().unwrap(), "<out><p/>&lt;q/&gt;</out>");
    assert_eq!(
        r.secondary("file:///results/style/s.xml"),
        "<s>&lt;x/&gt;<y/></s>"
    );
}

/// A primary result made by `xsl:result-document` carries that instruction's
/// d-o-e marks into `serialize_primary`.
#[test]
fn disable_output_escaping_reaches_a_primary_result_document() {
    let r = ok(&root(
        "",
        r#"<xsl:result-document omit-xml-declaration="yes"><p><xsl:text disable-output-escaping="yes">&amp;nbsp;</xsl:text></p></xsl:result-document>"#,
    ));
    assert_eq!(r.primary.clone().unwrap(), "<p>&nbsp;</p>");
}

/// The run's state is per run: a second run of the same `Transformation` does
/// not see the first run's URIs (no spurious XTDE1490).
#[test]
fn a_transformation_can_run_twice() {
    let text = root(
        "",
        r#"<xsl:result-document href="a.xml"><a/></xsl:result-document>"#,
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&text, SHEET_URI)
        .unwrap();
    let arena = bumpalo::Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    for _ in 0..2 {
        let outcome = run
            .transform_source("file:///results/in/doc.xml", "<doc/>")
            .unwrap();
        assert_eq!(outcome.secondary.len(), 1);
    }
}

#[test]
fn common_helpers_are_linked() {
    // `mod common` is shared by every integration test; this file uses only
    // its declaration constant's spelling, which `DECL` repeats.
    assert_eq!(common::DECLARATION, DECL);
}
