//! Regression tests for the independent review of M3a (serialization),
//! findings R1–R6 (`XSLT2_M3A_REVIEW.md`, fixes in `XSLT2_M3A_REVIEW_FIXES.md`).
//!
//! "Ser. §N" is *XSLT 2.0 and XQuery 1.0 Serialization (Second Edition)*,
//! REC-xslt-xquery-serialization-20101214, read at w3.org; "§N" is XSLT 2.0.
//! Where an expected string says "Saxon 9.1", it is that processor's output
//! for the same stylesheet, byte for byte.

use std::time::{Duration, Instant};

use xsd_schema::document::{BufferDocumentBuilder, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;
use xslt2::output::{OutputDefinition, OutputMethod};
use xslt2::serialize::{serialize, ResolvedMethod, TreeContext};
use xslt2::{Compiler, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Compiles `stylesheet`, runs it over `source` and serializes the primary
/// result with `serialize_primary`.
fn run_on(stylesheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled =
        Compiler::new(&names, &functions).compile_str(stylesheet, "file:///r/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///r/in.xml", source)?;
    compiled.serialize_primary(&outcome)
}

/// `declarations` and a template matching `/` with `content`, in an
/// `xsl:stylesheet` 2.0 that also binds `x` to `urn:x`, run over `<doc/>`.
fn out(declarations: &str, content: &str) -> Result<String, XsltError> {
    run_on(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:x="urn:x" exclude-result-prefixes="x">
                 {declarations}<xsl:template match="/">{content}</xsl:template>
               </xsl:stylesheet>"#
        ),
        "<doc/>",
    )
}

fn code(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => panic!("expected an error, got {text:?}"),
        Err(e) => e.code,
    }
}

// ═══ R1: the attribute delimiter and character maps ═════════════════════════

/// §20.1, the JSP example, verbatim: the character map, then "the required
/// output can be produced by writing the following in the stylesheet: <jsp:setProperty
/// name="user" property="id" value='«= §id§ + idValue »'/>. This works on the
/// assumption that when an apostrophe or quotation mark is generated as part
/// of an attribute value by the use of character maps, the serializer will
/// (where possible) use the other choice of delimiter around the attribute
/// value." The attribute is the spec's required output byte for byte; the
/// whole element is Saxon 9.1's (the `jsp` prefix has to be declared).
#[test]
fn r1_the_jsp_example_of_20_1() {
    let stylesheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:jsp="urn:jsp">
<xsl:output use-character-maps="jsp" omit-xml-declaration="yes"/>
<xsl:character-map name="jsp">
  <xsl:output-character character="«" string="&lt;%"/>
  <xsl:output-character character="»" string="%&gt;"/>
  <xsl:output-character character="§" string='"'/>
</xsl:character-map>
<xsl:template match="/"><jsp:setProperty name="user" property="id" value='«= §id§ + idValue »'/></xsl:template>
</xsl:stylesheet>"#
    );
    let result = run_on(&stylesheet, "<doc/>").unwrap();
    assert!(
        result.contains(r#"value='<%= "id" + idValue %>'"#),
        "{result}"
    );
    assert_eq!(
        result,
        r#"<jsp:setProperty xmlns:jsp="urn:jsp" name="user" property="id" value='<%= "id" + idValue %>'/>"#
    );
}

const QUOTE_MAPS: &str = r#"<xsl:character-map name="m">
  <xsl:output-character character="§" string='"'/>
  <xsl:output-character character="¤" string="'"/>
</xsl:character-map>"#;

/// With `'` as the delimiter, an unmapped `'` of the value is escaped (as
/// `&#39;`, never `&apos;`) and an unmapped `"` needs no escape; with `"`,
/// the reverse. A value whose mapped strings hold both quotes has no
/// delimiter that works ("where possible"): `"` stays and nothing is raised —
/// Ser. §9: "The string that is substituted is output "as is," and the
/// serializer performs no checks that the resulting document is well-formed".
/// Text nodes do not change.
#[test]
fn r1_the_delimiter_follows_the_mapped_quotes() {
    let result = out(
        &format!(r#"<xsl:output use-character-maps="m" omit-xml-declaration="yes"/>{QUOTE_MAPS}"#),
        r#"<r><a v="§x§ it's"/><b v='§x§ "q" &amp; it&apos;s &lt;'/><c v="§x§ ¤y¤"/><d v="¤y¤ &quot;q&quot;"/><e v="no map &quot;q&quot; it's"/><t>§x¤ "q" it's</t></r>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        concat!(
            r#"<r><a v='"x" it&#39;s'/>"#,
            r#"<b v='"x" "q" &amp; it&#39;s &lt;'/>"#,
            r#"<c v=""x" 'y'"/>"#,
            r#"<d v="'y' &#34;q&#34;"/>"#,
            r#"<e v="no map &#34;q&#34; it's"/>"#,
            r#"<t>"x' "q" it's</t></r>"#
        )
    );
}

/// The HTML and XHTML methods choose the delimiter the same way (HTML keeps
/// `<` in attribute values, §7.2); a URI attribute is not character-mapped
/// (Ser. §9: not "to characters in attribute values that are subject to URI
/// escaping"), so it keeps `"`; the added content-type `meta` is mapped like
/// any attribute (§4 phase 1), so it switches as well.
#[test]
fn r1_html_and_xhtml_attributes() {
    let maps = r#"<xsl:character-map name="m">
  <xsl:output-character character="§" string='"'/>
  <xsl:output-character character=";" string='";'/>
</xsl:character-map>"#;
    let html = out(
        &format!(r#"<xsl:output method="html" indent="no" use-character-maps="m"/>{maps}"#),
        r#"<html><head/><body><p title="§x§ it's &lt;">x</p><a href="§x§ it's">y</a></body></html>"#,
    )
    .unwrap();
    assert_eq!(
        html,
        concat!(
            r#"<html><head><meta http-equiv="Content-Type" content='text/html"; charset=UTF-8'></head>"#,
            r#"<body><p title='"x" it&#39;s <'>x</p><a href="%C2%A7x%C2%A7 it's">y</a></body></html>"#
        )
    );
    let xhtml = out(
        &format!(
            r#"<xsl:output method="xhtml" indent="no" omit-xml-declaration="yes" include-content-type="no" use-character-maps="m"/>{maps}"#
        ),
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p title="§x§ it's">x</p></body></html>"#,
    )
    .unwrap();
    assert_eq!(
        xhtml,
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p title='"x" it&#39;s'>x</p></body></html>"#
    );
}

// ═══ R2: SERE0014 wherever the tree holds #x7F–#x9F (HTML) ═══════════════════

/// Ser. §7.3: "Certain characters, specifically the control characters
/// #x7F-#x9F, are legal in XML but not in HTML. It is a serialization error
/// [err:SERE0014] to use the HTML output method when such characters appear in
/// the instance of the data model. The serializer MUST signal the error." —
/// in a comment, a processing instruction, text written with escaping
/// disabled, a URI attribute (URI escaping would hide it) and a CDATA section
/// of an XML island, as well as in ordinary text.
#[test]
fn r2_html_control_characters_in_every_context() {
    let contexts = [
        r#"<xsl:comment select="'a&#x85;b'"/>"#,
        r#"<xsl:processing-instruction name="x" select="'a&#x85;b'"/>"#,
        r#"<p><xsl:value-of select="'a&#x85;b'" disable-output-escaping="yes"/></p>"#,
        r#"<a href="x&#x96;y">t</a>"#,
        r#"<x:i>a&#x85;b</x:i>"#,
        r#"<p>a&#x7F;b</p>"#,
    ];
    let output = r#"<xsl:output method="html" cdata-section-elements="x:i"/>"#;
    for content in contexts {
        assert_eq!(
            code(out(output, &format!("<html>{content}</html>"))),
            "SERE0014",
            "{content}"
        );
    }
}

/// The XML and XHTML methods are not affected: XML 1.0 allows the characters,
/// so comments and processing instructions hold them as they are; Ser. §5
/// asks for references only "in text nodes and attribute nodes".
#[test]
fn r2_xml_and_xhtml_keep_control_characters_in_comments_and_pis() {
    for method in ["xml", "xhtml"] {
        let result = out(
            &format!(r#"<xsl:output method="{method}" indent="no" omit-xml-declaration="yes"/>"#),
            r#"<html><xsl:comment select="'a&#x85;b'"/><xsl:processing-instruction name="x" select="'a&#x85;b'"/><p>a&#x85;b</p></html>"#,
        )
        .unwrap();
        assert_eq!(
            result, "<html><!--a\u{85}b--><?x a\u{85}b?><p>a&#133;b</p></html>",
            "{method}"
        );
    }
}

// ═══ R3: indentation and whitespace-only text ════════════════════════════════

/// The review's case: a leading newline before `<html>` (default html method,
/// `indent` defaults to yes, §20) no longer switches indentation off for the
/// document. The whole output is Saxon 9.1's, byte for byte. Ser. §7.4.3: the
/// HTML output method "MAY add or remove whitespace as it serializes the result
/// tree" within its constraints; a whitespace-only text node is not mixed
/// content.
#[test]
fn r3_a_leading_newline_keeps_html_indented() {
    let result = out(
        "",
        "<xsl:text>&#10;</xsl:text><html><head><title>t</title></head><body><div><p>x</p><table><tr><td>1</td></tr></table></div></body></html>",
    )
    .unwrap();
    assert_eq!(
        result,
        "\n<html>\n   <head>\n      <meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\n      <title>t</title>\n   </head>\n   <body>\n      <div>\n         <p>x</p>\n         <table>\n            <tr>\n               <td>1</td>\n            </tr>\n         </table>\n      </div>\n   </body>\n</html>"
    );
}

/// Ser. §5.1.3: "In any location where the above rules allow the addition of
/// whitespace characters, existing whitespace characters MAY also be removed or
/// replaced." Whitespace-only text next to an element is replaced by the
/// layout; whitespace-only text that is the only content of an element is not
/// next to an element ("immediately before a start tag or immediately after an
/// end tag") and stays; so does whitespace between a comment and an end tag.
/// An indented source copied with `indent="yes"` comes out indented the
/// serializer's way.
#[test]
fn r3_whitespace_only_text_is_not_mixed_content() {
    let result = out(
        r#"<xsl:output indent="yes" omit-xml-declaration="yes"/>"#,
        r#"<r><a><xsl:text> </xsl:text><b/><xsl:text>  </xsl:text><c><d/></c><xsl:text>&#10;</xsl:text></a><e><xsl:text>   </xsl:text></e><f><xsl:comment>c</xsl:comment><xsl:text> </xsl:text></f></r>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        "<r>\n   <a>\n      <b/>\n      <c>\n         <d/>\n      </c>\n   </a>\n   <e>   </e>\n   <f><!--c--> </f>\n</r>"
    );
    let copied = run_on(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:output indent="yes" omit-xml-declaration="yes"/>
                 <xsl:template match="/"><xsl:copy-of select="."/></xsl:template>
               </xsl:stylesheet>"#
        ),
        "<root>\n  <a>\n    <b>x</b>\n    <c/>\n  </a>\n  <d> </d>\n</root>",
    )
    .unwrap();
    assert_eq!(
        copied,
        "<root>\n   <a>\n      <b>x</b>\n      <c/>\n   </a>\n   <d> </d>\n</root>"
    );
}

/// Where indentation must not apply, the content is written exactly as it is
/// in the tree, whitespace-only text included: genuinely mixed content and
/// everything below it (§5.1.3: "Whitespace characters MUST NOT be added
/// adjacent to a text node that contains non-whitespace characters"),
/// `xml:space="preserve"` ("MUST NOT be inserted in a part of the result
/// document that is controlled by an xml:space attribute with value
/// preserve"), and `indent="no"` ("it MUST NOT output any additional
/// whitespace").
#[test]
fn r3_content_is_unchanged_where_indentation_does_not_apply() {
    let content = r#"<r><m>t<b><xsl:text> </xsl:text><i/><xsl:text> </xsl:text></b>u</m><p xml:space="preserve"><xsl:text> </xsl:text><q><xsl:text> </xsl:text><s/></q><xsl:text>&#10;</xsl:text></p></r>"#;
    let indented = out(
        r#"<xsl:output indent="yes" omit-xml-declaration="yes"/>"#,
        content,
    )
    .unwrap();
    assert_eq!(
        indented,
        "<r>\n   <m>t<b> <i/> </b>u</m>\n   <p xml:space=\"preserve\"> <q> <s/></q>\n</p>\n</r>"
    );
    let flat = out(
        r#"<xsl:output indent="no" omit-xml-declaration="yes"/>"#,
        content,
    )
    .unwrap();
    assert_eq!(
        flat,
        "<r><m>t<b> <i/> </b>u</m><p xml:space=\"preserve\"> <q> <s/></q>\n</p></r>"
    );
}

/// Ser. §7.4.3 (and §6.1.3 for XHTML): "Whitespace MUST NOT be added or
/// removed adjacent to an inline element" and "MUST NOT be added or removed
/// inside a formatted element": whitespace next to `span`/`b` stays as it is
/// and nothing is added beside it; `pre` is untouched; the block-level `div`
/// and `td` around them are still laid out where they meet block elements.
#[test]
fn r3_html_whitespace_next_to_inline_and_formatted_elements() {
    let result = out(
        r#"<xsl:output method="html" include-content-type="no"/>"#,
        r#"<html><body><div><span>a</span><xsl:text> </xsl:text><span>b</span></div><table><tr><td><xsl:text> </xsl:text><b>x</b><xsl:text> </xsl:text></td></tr></table><pre><xsl:text> </xsl:text><b/><xsl:text>&#10;</xsl:text></pre></body></html>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        "<html>\n   <body>\n      <div><span>a</span> <span>b</span></div>\n      <table>\n         <tr>\n            <td> <b>x</b> </td>\n         </tr>\n      </table>\n      <pre> <b></b>\n</pre>\n   </body>\n</html>"
    );
}

// ═══ R4: per-element cost independent of depth ══════════════════════════════

/// A tree of `depth` nested `e` elements; with `flip`, every level switches the
/// default namespace (`urn:d`, then none).
fn deep_tree(depth: usize, flip: bool, method: OutputMethod) -> (String, Duration) {
    let arena = bumpalo::Bump::new();
    let names = NameTable::new();
    let mut b =
        BufferDocumentBuilder::new(&arena, &names, None, BufferDocumentOptions::default()).unwrap();
    for level in 0..depth {
        if flip {
            let uri = if level % 2 == 0 { "urn:d" } else { "" };
            b.start_element("e", uri, "", &[("", uri)]).unwrap();
        } else {
            b.start_element("e", "", "", &[]).unwrap();
        }
        b.end_of_attributes();
    }
    for _ in 0..depth {
        b.end_element().unwrap();
    }
    let doc = b.finalize().unwrap();
    let mut definition = OutputDefinition::default();
    definition.method.set(method, 1);
    definition.omit_xml_declaration.set(true, 1);
    definition.indent.set(false, 1);
    let start = Instant::now();
    let text = serialize(
        &doc.create_navigator(),
        &definition,
        TreeContext::default(),
        None,
    )
    .unwrap();
    (text, start.elapsed())
}

/// The writer reads each element's own namespace declarations and keeps its
/// own scope, instead of the in-scope namespaces, which the tree gathers from
/// every ancestor: the declarations come out as before, and the time no longer
/// grows with the square (or, with a declaration on every level, the cube) of
/// the depth. Before the fix, 3,000 levels that each switch the default
/// namespace took about 30 s in a release build (the review measured 1,000
/// levels at 1.16 s); now they take about a millisecond. The bound leaves a
/// wide margin for a debug build on a busy machine.
#[test]
fn r4_deep_trees_serialize_in_linear_time() {
    let (small, _) = deep_tree(4, true, OutputMethod::Xml);
    assert_eq!(
        small,
        r#"<e xmlns="urn:d"><e xmlns=""><e xmlns="urn:d"><e xmlns=""/></e></e></e>"#
    );
    let (flipped, elapsed) = deep_tree(3_000, true, OutputMethod::Xml);
    assert_eq!(flipped.matches("<e xmlns=\"\">").count(), 1_499);
    assert!(
        elapsed < Duration::from_secs(5),
        "3,000 levels switching the default namespace took {elapsed:?}"
    );
    let (plain, elapsed) = deep_tree(100_000, false, OutputMethod::Html);
    assert_eq!(plain.len(), 100_000 * "<e></e>".len());
    assert!(
        elapsed < Duration::from_secs(5),
        "100,000 nested elements took {elapsed:?}"
    );
}

// ═══ R5: disable-output-escaping inside xsl:document ═════════════════════════

/// §11.5: "The temporary tree rooted at this document node forms the result
/// tree"; §19.1: temporary output state "applies when instructions are writing
/// to a temporary tree or any other non-final destination"; §20.2: "If output
/// escaping is disabled for an xsl:value-of or xsl:text instruction evaluated
/// when temporary output state is in effect, the request to disable output
/// escaping is ignored." So text inside `xsl:document` — called templates
/// included — is escaped even though its children land in the final result
/// tree, while d-o-e right after it is honoured (the result is Saxon 9.1's).
/// `xsl:copy` of a document node is not a temporary tree of its own and keeps
/// d-o-e (Saxon 9.1 likewise).
#[test]
fn r5_disable_output_escaping_inside_xsl_document_is_ignored() {
    let sheet = |template: &str| {
        format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:output omit-xml-declaration="yes"/>
                 <xsl:template match="/">{template}</xsl:template>
                 <xsl:template name="t"><xsl:text disable-output-escaping="yes">&lt;called/&gt;</xsl:text></xsl:template>
               </xsl:stylesheet>"#
        )
    };
    let nested = sheet(
        r#"<r><xsl:document><d><xsl:value-of select="'&lt;doc/&gt;'" disable-output-escaping="yes"/><xsl:call-template name="t"/></d><xsl:text disable-output-escaping="yes">&lt;top/&gt;</xsl:text></xsl:document><after><xsl:text disable-output-escaping="yes">&lt;raw/&gt;</xsl:text></after></r>"#,
    );
    assert_eq!(
        run_on(&nested, "<doc/>").unwrap(),
        "<r><d>&lt;doc/&gt;&lt;called/&gt;</d>&lt;top/&gt;<after><raw/></after></r>"
    );
    let top = sheet(
        r#"<xsl:document><r><xsl:value-of select="'&lt;doc/&gt;'" disable-output-escaping="yes"/></r></xsl:document>"#,
    );
    assert_eq!(run_on(&top, "<doc/>").unwrap(), "<r>&lt;doc/&gt;</r>");
    let copied = sheet(
        r#"<xsl:copy><r><xsl:value-of select="'&lt;copy/&gt;'" disable-output-escaping="yes"/></r></xsl:copy>"#,
    );
    assert_eq!(run_on(&copied, "<doc/>").unwrap(), "<r><copy/></r>");
}

// ═══ R6: the method serialize_primary uses, for a host ═══════════════════════

/// §20: "if the version attribute of the xsl:stylesheet element of the
/// principal stylesheet module has the value 1.0, and if the result tree is
/// generated implicitly …, then the default output method in this situation
/// is xml" — `primary_serialization_params` reports what `serialize_primary`
/// does (the W3C harness records this method).
#[test]
fn r6_primary_serialization_params_apply_the_1_0_rule() {
    let method = |version: &str| {
        let stylesheet = format!(
            r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}">
                 <xsl:template match="/"><html xmlns="http://www.w3.org/1999/xhtml"><br/></html></xsl:template>
               </xsl:stylesheet>"#
        );
        let names = NameTable::new();
        let functions = XsltFunctions::new();
        let compiled = Compiler::new(&names, &functions)
            .compile_str(&stylesheet, "file:///r/main.xsl")
            .unwrap();
        let arena = bumpalo::Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///r/in.xml", "<doc/>")
            .unwrap();
        let params = compiled
            .primary_serialization_params(&outcome)
            .unwrap()
            .expect("a primary result");
        let text = compiled.serialize_primary(&outcome).unwrap();
        (params.method, text.contains("<br/>"))
    };
    assert_eq!(method("1.0"), (ResolvedMethod::Xml, true));
    assert_eq!(method("2.0"), (ResolvedMethod::Xhtml, false));
}
