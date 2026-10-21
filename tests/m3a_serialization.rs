//! M3a — serialization: the `xml`, `xhtml`, `html` and `text` output methods
//! and their parameters (XSLT 2.0 §20; *XSLT 2.0 and XQuery 1.0 Serialization
//! (Second Edition)*, REC-xslt-xquery-serialization-20101214, read at w3.org —
//! cited "Ser. §N"), character maps (§20.1, Ser. §9), `disable-output-escaping`
//! (§20.2), the four encodings (lead decision Q28) and the `xsl:output` merge.
//!
//! Every test goes through the public API — compile, transform,
//! `serialize_primary` — and compares the serialized text exactly unless the
//! rule leaves the detail open.

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const DECL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;

fn sheet(body: &str) -> String {
    format!("<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>")
}

/// Compiles `main` (with `modules` beside it, for `xsl:import`), runs it over
/// `<doc/>` and serializes the primary result as `serialize_primary` does.
fn run_modules(main: &str, modules: &[(&str, &str)]) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    for (uri, text) in modules {
        mem.insert(*uri, *text);
    }
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::new(mem))
        .compile_str(main, "file:///m3a/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///m3a/in.xml", "<doc/>")?;
    compiled.serialize_primary(&outcome)
}

/// `body` (declarations and templates) in an `xsl:stylesheet` 2.0.
fn run(body: &str) -> Result<String, XsltError> {
    run_modules(&sheet(body), &[])
}

/// One template matching `/` with `content`, after `output`.
fn out(output: &str, content: &str) -> Result<String, XsltError> {
    run(&format!(
        r#"{output}<xsl:template match="/">{content}</xsl:template>"#
    ))
}

fn code(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => panic!("expected an error, got {text:?}"),
        Err(e) => e.code,
    }
}

// ═══ §20 the default method and the XML method ═══════════════════════════

/// §20 `omit-xml-declaration`: "The default value is no." Ser. §5.1.5: "The XML
/// declaration MUST include both version information and an encoding
/// declaration." Compact output, `<a/>` for an empty element.
#[test]
fn the_xml_method_writes_a_declaration_by_default() {
    assert_eq!(
        out("", "<out><a/>t</out>").unwrap(),
        format!("{DECL}<out><a/>t</out>")
    );
}

/// §20: "If the expanded-QName of this first element child has local part html
/// (in any combination of upper and lower case) and a null namespace URI, then
/// the default output method is html" — no declaration, `<br>` without an end
/// tag; "local part html (in lower case), and namespace URI
/// http://www.w3.org/1999/xhtml, then the default output method is normally
/// xhtml" — `<br />`; text that is not whitespace before the element, or any
/// other element, keeps `xml`.
#[test]
fn the_default_method_follows_the_first_element() {
    assert_eq!(
        out(r#"<xsl:output indent="no"/>"#, "<HTML><br/></HTML>").unwrap(),
        "<HTML><br></HTML>"
    );
    assert_eq!(
        out(
            r#"<xsl:output indent="no"/>"#,
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><br/></html>"#
        )
        .unwrap(),
        format!(r#"{DECL}<html xmlns="http://www.w3.org/1999/xhtml"><br /></html>"#)
    );
    assert_eq!(
        out("", "<xsl:text>x</xsl:text><html/>").unwrap(),
        format!("{DECL}x<html/>")
    );
    assert_eq!(
        out("", "<xsl:text> </xsl:text><html/>").unwrap(),
        " <html></html>",
        "whitespace before the element does not change the default"
    );
}

/// §20: "if the version attribute of the xsl:stylesheet element of the principal
/// stylesheet module has the value 1.0, and if the result tree is generated
/// implicitly …, then the default output method in this situation is xml."
#[test]
fn an_xhtml_result_of_a_1_0_stylesheet_defaults_to_xml() {
    let one = format!(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="{NS}">
             <xsl:template match="/"><html xmlns="http://www.w3.org/1999/xhtml"><br/></html></xsl:template>
           </xsl:stylesheet>"#
    );
    assert_eq!(
        run_modules(&one, &[]).unwrap(),
        format!(r#"{DECL}<html xmlns="http://www.w3.org/1999/xhtml"><br/></html>"#)
    );
}

/// Ser. §5: "the non-whitespace control characters #x1 through #x1F and #x7F
/// through #x9F in text nodes and attribute nodes MUST be output as character
/// references"; "CR, NEL and LINE SEPARATOR characters in text nodes MUST be
/// output respectively as "&#xD;", "&#x85;", and "&#x2028;", or their
/// equivalents; while CR, NL, TAB, NEL and LINE SEPARATOR characters in
/// attribute nodes MUST be output respectively as … character references".
#[test]
fn control_characters_and_line_ends_are_character_references() {
    let result = out(
        "",
        r#"<out a="{codepoints-to-string((9, 10, 13, 133, 8232))}"><xsl:value-of select="codepoints-to-string((13, 127, 133, 150, 159, 8232, 10, 9))"/></out>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        format!(
            "{DECL}<out a=\"&#x9;&#xA;&#xD;&#133;&#8232;\">&#xD;&#127;&#133;&#150;&#159;&#8232;\n\t</out>"
        )
    );
}

/// The escaping of markup characters (Ser. §4 rule e): `&`, `<`, `>` in text;
/// `"` too in an attribute value.
#[test]
fn markup_characters_are_escaped() {
    assert_eq!(
        out("", r#"<out a="&lt;&amp;&gt;&quot;'">&lt;&amp;&gt;"'</out>"#).unwrap(),
        format!(r#"{DECL}<out a="&lt;&amp;&gt;&#34;'">&lt;&amp;&gt;"'</out>"#)
    );
}

// ═══ Ser. §6 the XHTML method ═══════════════════════════════════════════

/// Ser. §6: "If an element that has no children is an XHTML element with an
/// EMPTY content model, the serializer MUST use the minimized tag syntax, for
/// example <br />" with "a space before the trailing />"; "Given an empty
/// instance of an XHTML element whose content model is not EMPTY … it MUST
/// output <p></p> and not <p />." Script content is escaped as in XML.
#[test]
fn the_xhtml_method_minimizes_only_empty_content_models() {
    let result = out(
        r#"<xsl:output method="xhtml" indent="no"/>"#,
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><br/><hr/><p/><script>a &lt; b</script></body></html>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        format!(
            r#"{DECL}<html xmlns="http://www.w3.org/1999/xhtml"><body><br /><hr /><p></p><script>a &lt; b</script></body></html>"#
        )
    );
}

/// Ser. §6.1.13: "If the instance of the data model includes a head element in
/// the XHTML namespace, and the include-content-type parameter has the value
/// yes, the XHTML output method MUST add a meta element as the first child
/// element of the head element … any existing meta element child of the head
/// element having an http-equiv attribute with the value "Content-Type" …
/// MUST be discarded." The content is the media type (§20 default `text/html`).
#[test]
fn the_xhtml_method_adds_the_content_type_meta() {
    let result = out(
        r#"<xsl:output method="xhtml" indent="no"/>"#,
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><meta http-equiv=" content-TYPE " content="x"/><title>t</title></head></html>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        format!(
            r#"{DECL}<html xmlns="http://www.w3.org/1999/xhtml"><head><meta http-equiv="Content-Type" content="text/html; charset=UTF-8" /><title>t</title></head></html>"#
        )
    );
    let none = out(
        r#"<xsl:output method="xhtml" indent="no" include-content-type="no"/>"#,
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/></html>"#,
    )
    .unwrap();
    assert!(!none.contains("meta"), "{none}");
}

// ═══ Ser. §7 the HTML method ════════════════════════════════════════════

/// Ser. §7.1: "The HTML output method MUST NOT output an end-tag for an empty
/// element if the element type has an empty content model"; "MUST recognize
/// the names of HTML elements regardless of case"; "MUST NOT perform escaping
/// for the content of the script and style elements". §7.2: "MUST NOT escape
/// "<" characters occurring in attribute values", boolean attributes "in
/// minimized form", and "MUST NOT escape a & character occurring in an
/// attribute value immediately followed by a { character". No XML declaration
/// (§7.4.5: not applicable).
#[test]
fn the_html_method_rules() {
    let result = out(
        r#"<xsl:output method="html" indent="no"/>"#,
        r#"<html><BR/><p/><option selected="selected" title="a&lt;b" x="&amp;{{y}}"/><script>if (a &lt; b &amp;&amp; c) x()</script><style>p &gt; a {}</style></html>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        r#"<html><BR><p></p><option selected title="a<b" x="&{y}"></option><script>if (a < b && c) x()</script><style>p > a {}</style></html>"#
    );
}

/// Ser. §7.4.13: "If there is a head element, and the include-content-type
/// parameter has the value yes, the HTML output method MUST add a meta element
/// as the first child element of the head element specifying the character
/// encoding actually used … The content type MUST be set to the value given
/// for the media-type parameter."
#[test]
fn the_html_method_adds_the_content_type_meta_with_the_media_type() {
    let result = out(
        r#"<xsl:output method="html" indent="no" media-type="application/x-test" encoding="ISO-8859-1"/>"#,
        "<html><HEAD><title>t</title></HEAD></html>",
    )
    .unwrap();
    assert_eq!(
        result,
        r#"<html><HEAD><meta http-equiv="Content-Type" content="application/x-test; charset=ISO-8859-1"><title>t</title></HEAD></html>"#
    );
}

/// Ser. §7.3: "The HTML output method MUST terminate processing instructions
/// with > rather than ?>. It is a serialization error [err:SERE0015] to use the
/// HTML output method when > appears within a processing instruction", and
/// "[err:SERE0014]" for "the control characters #x7F-#x9F".
#[test]
fn html_processing_instructions_and_control_characters() {
    assert_eq!(
        out(
            r#"<xsl:output method="html"/>"#,
            r#"<html><xsl:processing-instruction name="pi">data</xsl:processing-instruction></html>"#
        )
        .unwrap(),
        "<html><?pi data></html>"
    );
    assert_eq!(
        code(out(
            r#"<xsl:output method="html"/>"#,
            r#"<html><xsl:processing-instruction name="pi">a>b</xsl:processing-instruction></html>"#
        )),
        "SERE0015"
    );
    assert_eq!(
        code(out(
            r#"<xsl:output method="html"/>"#,
            r#"<html><xsl:value-of select="codepoints-to-string(150)"/></html>"#
        )),
        "SERE0014"
    );
}

/// Ser. §7.4.12 / §6.1.12: "If the escape-uri-attributes parameter has the
/// value yes, the HTML output method MUST apply URI escaping to URI attribute
/// values" — §4: "normalize to NFC", then "percent-encode … using the method
/// defined in … fn:escape-html-uri" (every character outside printable ASCII).
/// A non-URI attribute (Appendix C lists the URI ones) is left alone.
#[test]
fn uri_attributes_are_escaped_unless_turned_off() {
    let content = r#"<html><a href="http://x/&#xE5;r a&#x30A;" title="&#xE5;"/></html>"#;
    assert_eq!(
        out(r#"<xsl:output method="html" indent="no"/>"#, content).unwrap(),
        "<html><a href=\"http://x/%C3%A5r %C3%A5\" title=\"\u{E5}\"></a></html>"
    );
    assert_eq!(
        out(
            r#"<xsl:output method="html" indent="no" escape-uri-attributes="no"/>"#,
            content
        )
        .unwrap(),
        "<html><a href=\"http://x/\u{E5}r a\u{30A}\" title=\"\u{E5}\"></a></html>"
    );
}

/// Ser. §7.4.6: "The name following <!DOCTYPE MUST be HTML or html. If the
/// doctype-public parameter is specified, then the output method MUST output
/// PUBLIC followed by the specified public identifier; if the doctype-system
/// parameter is also specified, it MUST also output the specified system
/// identifier".
#[test]
fn html_doctype_with_only_a_public_identifier() {
    assert_eq!(
        out(
            r#"<xsl:output method="html" indent="no" doctype-public="-//W3C//DTD HTML 4.01//EN"/>"#,
            "<html/>"
        )
        .unwrap(),
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\">\n<html></html>"
    );
}

/// Ser. §7.4.3: "Whitespace MUST NOT be added or removed adjacent to an inline
/// element"; "MUST NOT be added or removed inside a formatted element … pre,
/// script, style, and textarea". §20: indent defaults to yes for html. An
/// element HTML does not know is written "in the same way as a non-empty,
/// inline element such as span" (§7.1).
#[test]
fn html_indentation_keeps_away_from_inline_and_formatted_elements() {
    let result = out(
        r#"<xsl:output method="html"/>"#,
        "<html><body><div><b>x</b><i>y</i></div><pre><p/></pre><unknown><p/></unknown></body></html>",
    )
    .unwrap();
    assert_eq!(
        result,
        "<html>\n   <body>\n      <div><b>x</b><i>y</i></div>\n      <pre><p></p></pre><unknown><p></p></unknown></body>\n</html>"
    );
}

// ═══ Ser. §8 the text method ════════════════════════════════════════════

/// Ser. §8: "outputting the string value of the document node … without any
/// escaping"; §8.1.2: "If the instance of the data model contains a character
/// that cannot be represented in the encoding that the serializer is using for
/// output, the serializer MUST signal a serialization error [err:SERE0008]."
#[test]
fn the_text_method_writes_the_string_value_unescaped() {
    assert_eq!(
        out(
            r#"<xsl:output method="text"/>"#,
            "<out>a &lt; b<x>&amp;</x></out>"
        )
        .unwrap(),
        "a < b&"
    );
    assert_eq!(
        code(out(
            r#"<xsl:output method="text" encoding="US-ASCII"/>"#,
            "<out>caf&#xE9;</out>"
        )),
        "SERE0008"
    );
}

// ═══ Ser. §3 parameters ═════════════════════════════════════════════════

/// Ser. §3 `byte-order-mark`: "This parameter indicates whether the serialized
/// sequence of octets is to be preceded by a Byte Order Mark … If the encoding
/// defines no Byte Order Mark … then this parameter is ignored." §20: "If the
/// encoding is UTF-16, the default is yes". As characters, the mark is U+FEFF.
#[test]
fn the_byte_order_mark() {
    assert!(out(r#"<xsl:output byte-order-mark="yes"/>"#, "<out/>")
        .unwrap()
        .starts_with("\u{FEFF}<?xml"));
    assert!(out(r#"<xsl:output encoding="UTF-16"/>"#, "<out/>")
        .unwrap()
        .starts_with("\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
    assert!(out(
        r#"<xsl:output encoding="UTF-16" byte-order-mark="no"/>"#,
        "<out/>"
    )
    .unwrap()
    .starts_with("<?xml"));
    assert!(
        out(
            r#"<xsl:output encoding="ISO-8859-1" byte-order-mark="yes"/>"#,
            "<out/>"
        )
        .unwrap()
        .starts_with("<?xml"),
        "ISO-8859-1 has no byte order mark"
    );
}

/// Ser. §5.1.4: "If the expanded QName of the parent of a text node is a member
/// of the list, then the text node MUST be output as a CDATA section"; "If the
/// text node contains the sequence of characters ]]>, then the currently open
/// CDATA section MUST be closed following the ]] and a new CDATA section opened
/// before the >"; an unrepresentable character closes the section, is written
/// as a reference, and a new one opens. §20: an unprefixed name in the list
/// uses the default namespace.
#[test]
fn cdata_section_elements() {
    let result = out(
        r#"<xsl:output cdata-section-elements="c p:d" xmlns:p="urn:p" encoding="US-ASCII"/>"#,
        // `]]&gt;`: XML 1.0 [14] CharData excludes a literal `]]>`.
        r#"<out><c>a]]&gt;b&#xE9;c</c><d xmlns="urn:p">&lt;x</d><e>&lt;</e></out>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        r#"<?xml version="1.0" encoding="US-ASCII"?><out><c><![CDATA[a]]]]><![CDATA[>b]]>&#233;<![CDATA[c]]></c><d xmlns="urn:p"><![CDATA[<x]]></d><e>&lt;</e></out>"#
    );
    // The default namespace of the xsl:output applies to an unprefixed name.
    let ns = run(r#"<xsl:output xmlns="urn:q" cdata-section-elements="c"/>
                    <xsl:template match="/"><out><c>x</c><q:c xmlns:q="urn:q">y</q:c></out></xsl:template>"#)
    .unwrap();
    assert!(
        ns.contains("<c>x</c>") && ns.contains("<![CDATA[y]]>"),
        "{ns}"
    );
}

/// Ser. §5.1.6: "If the doctype-system parameter is specified, the XML output
/// method MUST output a document type declaration immediately before the first
/// element. The name following <!DOCTYPE MUST be the name of the first element
/// … PUBLIC followed by the public identifier and then the system identifier"
/// and "The doctype-public parameter MUST be ignored unless the doctype-system
/// parameter is specified." A system literal holding `"` is quoted with `'`.
#[test]
fn xml_doctype_declarations() {
    assert_eq!(
        out(
            r#"<xsl:output doctype-public="-//A//B" doctype-system="a.dtd"/>"#,
            "<xsl:comment>c</xsl:comment><a/>"
        )
        .unwrap(),
        format!("{DECL}<!--c--><!DOCTYPE a PUBLIC \"-//A//B\" \"a.dtd\">\n<a/>")
    );
    assert_eq!(
        out(r#"<xsl:output doctype-system='x"y'/>"#, "<a/>").unwrap(),
        format!("{DECL}<!DOCTYPE a SYSTEM 'x\"y'>\n<a/>")
    );
    assert_eq!(
        out(r#"<xsl:output doctype-public="-//A//B"/>"#, "<a/>").unwrap(),
        format!("{DECL}<a/>")
    );
}

/// §20: doctype-system "If the attribute is absent or has a zero-length string
/// as its value, then the serialization parameter is not set (is "absent")" —
/// and the empty value still wins over a lower import precedence
/// (`output-0312`).
#[test]
fn an_empty_doctype_overrides_an_imported_one() {
    let imported = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:output doctype-system="x.dtd"/></xsl:stylesheet>"#
    );
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/><xsl:output doctype-system=""/>
           <xsl:template match="/"><a/></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///m3a/imported.xsl", &imported)]).unwrap(),
        format!("{DECL}<a/>")
    );
}

/// Ser. §5.1.5: SEPM0009 "if the omit-xml-declaration parameter has the value
/// yes, and the standalone parameter has a value other than omit"; §5: SEPM0004
/// for doctype-system or standalone "if the instance of the data model contains
/// text nodes or multiple element nodes as children of the root node"; §5.1.7:
/// SEPM0010 for undeclare-prefixes="yes" with version 1.0; §5.1.1: SESU0013 for
/// an unsupported version (this serializer writes XML 1.0), §7.4.1 the same for
/// HTML.
#[test]
fn parameter_combinations_that_are_serialization_errors() {
    assert_eq!(
        code(out(
            r#"<xsl:output omit-xml-declaration="yes" standalone="yes"/>"#,
            "<a/>"
        )),
        "SEPM0009"
    );
    assert_eq!(
        code(out(r#"<xsl:output standalone="no"/>"#, "<a/><b/>")),
        "SEPM0004"
    );
    assert_eq!(
        code(out(r#"<xsl:output doctype-system="a.dtd"/>"#, "<a/>text")),
        "SEPM0004"
    );
    assert_eq!(
        code(out(r#"<xsl:output undeclare-prefixes="yes"/>"#, "<a/>")),
        "SEPM0010"
    );
    assert_eq!(
        code(out(r#"<xsl:output version="1.1"/>"#, "<a/>")),
        "SESU0013"
    );
    assert_eq!(
        code(out(r#"<xsl:output method="html" version="0.0"/>"#, "<a/>")),
        "SESU0013"
    );
    assert_eq!(
        out(r#"<xsl:output standalone="yes"/>"#, "<a/>").unwrap(),
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a/>"#
    );
}

/// Ser. §3: "It is a serialization error [err:SEPM0016] if a parameter value is
/// invalid for the given parameter. It is the responsibility of the host
/// language to specify how invalid values should be handled" — in a stylesheet
/// it is XTSE0020 (§3.4: "a value that is not one of the permitted values for
/// that attribute"), whether or not the method uses the parameter (§20: "The
/// processor may validate the value of such an attribute").
#[test]
fn invalid_parameter_values_are_xtse0020() {
    for output in [
        r#"<xsl:output byte-order-mark="true"/>"#,
        r#"<xsl:output escape-uri-attributes="true"/>"#,
        r#"<xsl:output include-content-type="1"/>"#,
        r#"<xsl:output undeclare-prefixes="true"/>"#,
        r#"<xsl:output doctype-public="&#xA3;[~~~]"/>"#,
        r#"<xsl:output doctype-system="'&quot;"/>"#,
        r#"<xsl:output encoding="utf 8"/>"#,
        r#"<xsl:output normalization-form="N F C"/>"#,
        r#"<xsl:output standalone="maybe"/>"#,
    ] {
        assert_eq!(code(out(output, "<a/>")), "XTSE0020", "{output}");
    }
}

/// An attribute `xsl:output` does not have is XTSE0090 — unless forwards-compatible
/// behaviour is on (§3.9: "if an element has an attribute that XSLT 2.0 does not
/// allow the element to have, then the attribute must be ignored"), as for the
/// XSLT 3.0 serialization parameters.
#[test]
fn unknown_output_attributes() {
    let body =
        r#"<xsl:output suppress-indentation="a"/><xsl:template match="/"><a/></xsl:template>"#;
    assert_eq!(code(run(body)), "XTSE0090");
    let three =
        format!(r#"<xsl:stylesheet version="3.0" xmlns:xsl="{NS}">{body}</xsl:stylesheet>"#);
    assert_eq!(run_modules(&three, &[]).unwrap(), format!("{DECL}<a/>"));
}

/// §20 \[ERR XTSE1570\]: the method "must (if present) be a valid QName. If the
/// QName does not have a prefix, then it … must be one of xml, html, xhtml, or
/// text". A prefixed QName names an implementation-defined method, of which
/// this processor has none: serializing with one is SEPM0016.
#[test]
fn output_methods_outside_the_four() {
    assert_eq!(
        code(out(r#"<xsl:output method="adaptive"/>"#, "<a/>")),
        "XTSE1570"
    );
    assert_eq!(
        code(out(r#"<xsl:output method="a::b"/>"#, "<a/>")),
        "XTSE1570"
    );
    assert_eq!(
        code(out(r#"<xsl:output method="p:m" xmlns:p="urn:p"/>"#, "<a/>")),
        "SEPM0016"
    );
}

/// Ser. §5.1.8: "The values NFC and none MUST be supported … A serialization
/// error [err:SESU0011] results if the value of the normalization-form
/// parameter specifies a normalization form that is not supported".
/// `fully-normalized` is not supported here.
#[test]
fn normalization_forms() {
    let content = r#"<out a="A&#x301;">A&#x301;</out>"#;
    assert_eq!(
        out(r#"<xsl:output normalization-form="NFC"/>"#, content).unwrap(),
        format!("{DECL}<out a=\"\u{C1}\">\u{C1}</out>")
    );
    assert_eq!(
        out(r#"<xsl:output normalization-form="none"/>"#, content).unwrap(),
        format!("{DECL}<out a=\"A\u{301}\">A\u{301}</out>")
    );
    assert_eq!(
        out(
            r#"<xsl:output method="text" normalization-form="NFD"/>"#,
            "<out>&#xC1;</out>"
        )
        .unwrap(),
        "A\u{301}"
    );
    for form in ["ABC", "fully-normalized"] {
        assert_eq!(
            code(out(
                &format!(r#"<xsl:output normalization-form="{form}"/>"#),
                "<a/>"
            )),
            "SESU0011"
        );
    }
}

/// Ser. §5.1.3: with indent="yes", whitespace "MUST NOT be added adjacent to a
/// text node that contains non-whitespace characters", only "immediately before
/// a start tag or immediately after an end tag", and not where `xml:space` is
/// `preserve`.
#[test]
fn xml_indentation() {
    assert_eq!(
        out(
            r#"<xsl:output indent="yes"/>"#,
            r#"<out><a><b/></a><m>t<b/></m><p xml:space="preserve"><b/></p></out>"#
        )
        .unwrap(),
        format!(
            "{DECL}\n<out>\n   <a>\n      <b/>\n   </a>\n   <m>t<b/></m>\n   <p xml:space=\"preserve\"><b/></p>\n</out>"
        )
    );
}

// ═══ Encodings (Q28) ════════════════════════════════════════════════════

/// Ser. §5.1.2: "if the character occurs in a context where XML recognizes
/// character references (that is, in the value of an attribute node or text
/// node), then the character MUST be output as a character reference. A
/// serialization error [err:SERE0008] occurs if such a character appears in a
/// context where character references are not allowed". The XML declaration
/// names the encoding.
#[test]
fn us_ascii_and_iso_8859_1() {
    assert_eq!(
        out(
            r#"<xsl:output encoding="US-ASCII"/>"#,
            r#"<out a="&#xE9;">&#xE9;&#x20AC;</out>"#
        )
        .unwrap(),
        r#"<?xml version="1.0" encoding="US-ASCII"?><out a="&#233;">&#233;&#8364;</out>"#
    );
    assert_eq!(
        out(
            r#"<xsl:output encoding="iso-8859-1"/>"#,
            r#"<out a="&#xE9;">&#xE9;&#x20AC;</out>"#
        )
        .unwrap(),
        "<?xml version=\"1.0\" encoding=\"iso-8859-1\"?><out a=\"\u{E9}\">\u{E9}&#8364;</out>"
    );
    for (what, content) in [
        ("comment", "<out><xsl:comment>&#xE9;</xsl:comment></out>"),
        (
            "processing instruction",
            r#"<out><xsl:processing-instruction name="p">&#xE9;</xsl:processing-instruction></out>"#,
        ),
        ("element name", r#"<xsl:element name="e&#xE9;"/>"#),
        (
            "attribute name",
            r#"<out><xsl:attribute name="a&#xE9;">v</xsl:attribute></out>"#,
        ),
    ] {
        assert_eq!(
            code(out(r#"<xsl:output encoding="US-ASCII"/>"#, content)),
            "SERE0008",
            "{what}"
        );
    }
}

/// UTF-16 represents everything; the declaration says UTF-16 and the byte
/// order mark leads (above). The encoding phase is available as octets.
#[test]
fn utf_16_and_the_encoding_phase() {
    assert_eq!(
        out(
            r#"<xsl:output encoding="UTF-16" byte-order-mark="no"/>"#,
            "<out>&#x10000;</out>"
        )
        .unwrap(),
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><out>\u{10000}</out>"
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:output encoding="UTF-16" omit-xml-declaration="yes"/>
                   <xsl:template match="/"><a/></xsl:template>"#,
            ),
            "urn:m3a",
        )
        .unwrap();
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("urn:in", "<doc/>")
        .unwrap();
    assert_eq!(
        compiled.serialize_primary_to_bytes(&outcome).unwrap(),
        [0xFE, 0xFF, 0, b'<', 0, b'a', 0, b'/', 0, b'>']
    );
}

/// Ser. §5.1.2: "A serialization error [err:SESU0007] occurs if an output
/// encoding other than UTF-8 or UTF-16 is requested and the serializer does not
/// support that encoding." Signalled for every method (§7.4.2 and §8.1.2 allow
/// no recovery). `windows-1252` is not supported: it is not ISO-8859-1.
#[test]
fn an_unsupported_encoding_is_sesu0007() {
    for method in ["xml", "xhtml", "html", "text"] {
        assert_eq!(
            code(out(
                &format!(r#"<xsl:output method="{method}" encoding="XXX-xx"/>"#),
                "<a/>"
            )),
            "SESU0007",
            "{method}"
        );
    }
    assert_eq!(
        code(out(r#"<xsl:output encoding="windows-1252"/>"#, "<a/>")),
        "SESU0007"
    );
}

/// Ser. §7.4.2: "if the character occurs in a context where HTML recognizes
/// character references, then the character MUST be output as a character
/// entity reference or decimal numeric character reference; otherwise (for
/// example, in a script or style element or in a comment), the serializer MUST
/// signal a serialization error [err:SERE0008]." The XHTML method escapes
/// script content as XML, so references are allowed there.
#[test]
fn html_contexts_without_character_references() {
    let html = r#"<xsl:output method="html" encoding="US-ASCII"/>"#;
    assert_eq!(
        out(html, "<html>&#xE9;</html>").unwrap(),
        "<html>&#233;</html>"
    );
    for content in [
        "<html><script>&#xE9;</script></html>",
        "<html><STYLE>&#xE9;</STYLE></html>",
        "<html><xsl:comment>&#xE9;</xsl:comment></html>",
    ] {
        assert_eq!(code(out(html, content)), "SERE0008", "{content}");
    }
    assert_eq!(
        out(
            r#"<xsl:output method="xhtml" encoding="US-ASCII" indent="no" omit-xml-declaration="yes"/>"#,
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><script>&#xE9;</script></html>"#
        )
        .unwrap(),
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><script>&#233;</script></html>"#
    );
}

// ═══ §20.1 character maps ═══════════════════════════════════════════════

/// Ser. §9: "If a character is mapped, then it is not subjected to XML or HTML
/// escaping" — in text and attribute values; names, comments and processing
/// instructions are not mapped ("a specific character appearing in a text or
/// attribute node", §20.1).
#[test]
fn mapped_characters_are_written_as_they_are() {
    let result = run(
        r#"<xsl:character-map name="m">
             <xsl:output-character character="«" string="&lt;%"/>
             <xsl:output-character character="»" string="%&gt;"/>
             <xsl:output-character character="c" string="[C]"/>
           </xsl:character-map>
           <xsl:output use-character-maps="m"/>
           <xsl:template match="/"><c a="«c»"><xsl:comment>c</xsl:comment>«c»&lt;</c></xsl:template>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        format!(r#"{DECL}<c a="<%[C]%>"><!--c--><%[C]%>&lt;</c>"#)
    );
}

/// §20.1: "Within a single xsl:character-map element, the characters defined in
/// character maps referenced in the use-character-maps attribute are considered
/// before the characters defined in the child xsl:output-character elements.
/// The character maps referenced in a single use-character-maps attribute are
/// considered in the order in which they are listed … the last character
/// mapping takes precedence"; §20 concatenates the `use-character-maps` of the
/// `xsl:output` declarations "in order of increasing import precedence".
#[test]
fn the_last_mapping_takes_precedence() {
    let result = run(
        r#"<xsl:character-map name="a"><xsl:output-character character="x" string="A"/></xsl:character-map>
           <xsl:character-map name="b"><xsl:output-character character="x" string="B"/>
                                       <xsl:output-character character="y" string="B"/></xsl:character-map>
           <xsl:character-map name="ab" use-character-maps="a b">
             <xsl:output-character character="y" string="OWN"/>
           </xsl:character-map>
           <xsl:character-map name="ba" use-character-maps="b a"/>
           <xsl:output use-character-maps="ab"/>
           <xsl:template match="/"><out>xy</out></xsl:template>"#,
    )
    .unwrap();
    assert_eq!(result, format!("{DECL}<out>BOWN</out>"));
    let reversed = run(
        r#"<xsl:character-map name="a"><xsl:output-character character="x" string="A"/></xsl:character-map>
           <xsl:character-map name="b"><xsl:output-character character="x" string="B"/></xsl:character-map>
           <xsl:output use-character-maps="b"/>
           <xsl:output use-character-maps="a"/>
           <xsl:template match="/"><out>x</out></xsl:template>"#,
    )
    .unwrap();
    assert_eq!(reversed, format!("{DECL}<out>A</out>"));
}

/// §20.1: "the character map with the highest import precedence is used", and
/// \[ERR XTSE1580\] for two of the same name and precedence; \[ERR XTSE1590\] for
/// a name nothing declares (in `xsl:output` or in `xsl:character-map`);
/// \[ERR XTSE1600\] for a map that references itself, directly or indirectly —
/// "It is not an error if the same character map is referenced more than once".
#[test]
fn character_map_static_errors_and_precedence() {
    let imported = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:character-map name="m"><xsl:output-character character="x" string="LOW"/></xsl:character-map>
           </xsl:stylesheet>"#
    );
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:character-map name="m"><xsl:output-character character="x" string="HIGH"/></xsl:character-map>
           <xsl:output use-character-maps="m"/>
           <xsl:template match="/"><out>x</out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///m3a/imported.xsl", &imported)]).unwrap(),
        format!("{DECL}<out>HIGH</out>")
    );
    let map = |name: &str, uses: &str| {
        format!(r#"<xsl:character-map name="{name}" use-character-maps="{uses}"/>"#)
    };
    let template = r#"<xsl:template match="/"><out/></xsl:template>"#;
    assert_eq!(
        code(run(&format!("{}{}{template}", map("m", ""), map("m", "")))),
        "XTSE1580"
    );
    assert_eq!(
        code(run(&format!(
            r#"<xsl:output use-character-maps="nope"/>{template}"#
        ))),
        "XTSE1590"
    );
    assert_eq!(
        code(run(&format!("{}{template}", map("m", "nope")))),
        "XTSE1590"
    );
    assert_eq!(
        code(run(&format!(
            "{}{}{}{template}",
            map("a", "b"),
            map("b", "c"),
            map("c", "a")
        ))),
        "XTSE1600"
    );
    assert!(run(&format!(
        "{}{}{}{template}",
        map("a", "b c"),
        map("b", "c"),
        map("c", "")
    ))
    .is_ok());
    assert_eq!(
        code(run(&format!(
            r#"<xsl:character-map name="m"><xsl:output-character character="ab" string="x"/></xsl:character-map>{template}"#
        ))),
        "XTSE0020"
    );
}

/// Ser. §9: "Character mapping is not applied to characters in text nodes whose
/// parent elements are listed in the cdata-section-elements parameter";
/// "A serialization error [err:SERE0008] occurs if character mapping causes the
/// output of a string containing a character that cannot be represented"; the
/// substituted string "is not subjected to Unicode Normalization" while the
/// unmapped characters are.
#[test]
fn character_maps_with_cdata_encodings_and_normalization() {
    let maps = r##"<xsl:character-map name="m">
                    <xsl:output-character character="a" string="#&#xE7;#"/>
                  </xsl:character-map>"##;
    assert_eq!(
        run(&format!(
            r#"{maps}<xsl:output use-character-maps="m" cdata-section-elements="c" normalization-form="NFD"/>
               <xsl:template match="/"><out><c>a</c>a&#xE7;</out></xsl:template>"#
        ))
        .unwrap(),
        format!("{DECL}<out><c><![CDATA[a]]></c>#\u{E7}#c\u{327}</out>")
    );
    assert_eq!(
        code(run(&format!(
            r#"{maps}<xsl:output use-character-maps="m" encoding="US-ASCII"/>
               <xsl:template match="/"><out>a</out></xsl:template>"#
        ))),
        "SERE0008"
    );
    // The text method applies character maps too (Ser. §8.1.10).
    assert_eq!(
        run(&format!(
            r#"{maps}<xsl:output method="text" use-character-maps="m"/>
               <xsl:template match="/"><out>ba</out></xsl:template>"#
        ))
        .unwrap(),
        "b#\u{E7}#"
    );
}

// ═══ §20.2 disable-output-escaping ══════════════════════════════════════

/// §20.2: "every character in the text node generated by evaluating the
/// xsl:value-of or xsl:text element should have the disable-escaping property
/// set … the normal action of the serializer to escape special characters such
/// as & and < is suppressed." The text merges with its neighbours into one text
/// node of the tree; only the flagged characters lose their escaping.
#[test]
fn disable_output_escaping_on_text_and_value_of() {
    assert_eq!(
        out(
            "",
            r#"<out>&lt;<xsl:text disable-output-escaping="yes">&lt;b/&gt;</xsl:text>&amp;<xsl:value-of select="'&amp;amp;'" disable-output-escaping="yes"/></out>"#
        )
        .unwrap(),
        format!("{DECL}<out>&lt;<b/>&amp;&amp;</out>")
    );
}

/// §20.2: "If output escaping is disabled for an xsl:value-of or xsl:text
/// instruction evaluated when temporary output state is in effect, the request
/// to disable output escaping is ignored" — a variable's content (§19.1:
/// "xsl:variable … always evaluate the instructions in their contained sequence
/// constructor in temporary output state"), and an attribute's (`bug-1202`).
#[test]
fn disable_output_escaping_is_ignored_in_temporary_output_state() {
    let result = run(
        r#"<xsl:template match="/">
             <xsl:variable name="v"><q><xsl:text disable-output-escaping="yes">&lt;x/&gt;</xsl:text></q></xsl:variable>
             <out><xsl:attribute name="a"><xsl:text disable-output-escaping="yes">&lt;</xsl:text></xsl:attribute>
               <xsl:copy-of select="$v"/><xsl:value-of select="$v" disable-output-escaping="yes"/></out>
           </xsl:template>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        format!(r#"{DECL}<out a="&lt;"><q>&lt;x/&gt;</q><x/></out>"#)
    );
}

/// §20.2: "If output escaping is disabled for text within an element that would
/// normally be output using a CDATA section … the relevant text will not be
/// included in a CDATA section"; "The text output method ignores the
/// disable-output-escaping attribute"; "If output escaping is disabled for a
/// character that is not representable in the encoding …, the request to
/// disable output escaping is ignored in respect of that character".
#[test]
fn disable_output_escaping_with_cdata_text_and_encodings() {
    assert_eq!(
        out(
            r#"<xsl:output cdata-section-elements="t"/>"#,
            r#"<t>a<xsl:text disable-output-escaping="yes">&lt;hr/&gt;</xsl:text>b</t>"#
        )
        .unwrap(),
        format!("{DECL}<t><![CDATA[a]]><hr/><![CDATA[b]]></t>")
    );
    assert_eq!(
        out(
            r#"<xsl:output method="text"/>"#,
            r#"<t><xsl:text disable-output-escaping="yes">&lt;</xsl:text></t>"#
        )
        .unwrap(),
        "<"
    );
    assert_eq!(
        out(
            r#"<xsl:output encoding="US-ASCII"/>"#,
            r#"<t><xsl:text disable-output-escaping="yes">&lt;&#xE9;</xsl:text></t>"#
        )
        .unwrap(),
        r#"<?xml version="1.0" encoding="US-ASCII"?><t><&#233;</t>"#
    );
}

/// §20.1: "Character mapping is not applied to characters for which output
/// escaping has been disabled".
#[test]
fn disabled_characters_are_not_mapped() {
    assert_eq!(
        run(
            r#"<xsl:character-map name="m"><xsl:output-character character="x" string="X"/></xsl:character-map>
               <xsl:output use-character-maps="m"/>
               <xsl:template match="/"><out>x<xsl:text disable-output-escaping="yes">x</xsl:text></out></xsl:template>"#
        )
        .unwrap(),
        format!("{DECL}<out>Xx</out>")
    );
}

// ═══ §20 the xsl:output merge ═══════════════════════════════════════════

/// §20: "For other attributes, the output definition uses the value of that
/// attribute from the xsl:output declaration with the highest import
/// precedence"; "For the cdata-section-elements attribute, the output
/// definition uses the union of the values"; \[ERR XTSE1560\] "if two xsl:output
/// declarations within an output definition specify explicit values for the
/// same attribute … with the values of the attributes being not equal, unless
/// there is another xsl:output declaration … that has higher import precedence".
#[test]
fn the_output_merge() {
    let imported = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
             <xsl:output encoding="US-ASCII" cdata-section-elements="a"/>
             <xsl:output encoding="ISO-8859-1"/>
           </xsl:stylesheet>"#
    );
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:output encoding="UTF-8" cdata-section-elements="b"/>
           <xsl:template match="/"><out><a>1</a><b>2</b></out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///m3a/imported.xsl", &imported)]).unwrap(),
        format!("{DECL}<out><a><![CDATA[1]]></a><b><![CDATA[2]]></b></out>"),
        "the conflict in the imported module is overridden"
    );
    for attribute in [
        r#"indent="yes"|indent="no""#,
        r#"byte-order-mark="yes"|byte-order-mark="no""#,
        r#"normalization-form="NFC"|normalization-form="NFD""#,
        r#"doctype-system="a"|doctype-system="""#,
    ] {
        let (a, b) = attribute.split_once('|').unwrap();
        assert_eq!(
            code(out(&format!("<xsl:output {a}/><xsl:output {b}/>"), "<a/>")),
            "XTSE1560",
            "{attribute}"
        );
    }
    // The same value twice is no conflict.
    assert!(out(
        r#"<xsl:output indent="no"/><xsl:output indent="no"/>"#,
        "<a/>"
    )
    .is_ok());
}

/// §20: "A named output definition is used when its name matches the format
/// attribute used in an xsl:result-document element" (M3b); the named
/// definitions are merged and checked like the unnamed one, and the unnamed one
/// is what the primary result uses.
#[test]
fn named_output_definitions_are_merged_and_kept() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:output name="t" method="text"/><xsl:output name="t" indent="yes"/>
                   <xsl:template match="/"><a/></xsl:template>"#,
            ),
            "urn:m3a",
        )
        .unwrap();
    let t = compiled
        .named_output(&xslt2::QName::local("t"))
        .expect("the named definition");
    assert_eq!(t.method.get(), Some(&xslt2::OutputMethod::Text));
    assert_eq!(t.indent.get(), Some(&true));
    assert_eq!(compiled.output().method.get(), None);
    assert_eq!(
        code(run(
            r#"<xsl:output name="t" method="text"/><xsl:output name="t" method="xml"/>
               <xsl:template match="/"><a/></xsl:template>"#
        )),
        "XTSE1560"
    );
}
