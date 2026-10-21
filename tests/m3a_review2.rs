//! Regression tests for the second independent review of M3a (serialization),
//! findings F1–F4 (`XSLT2_M3A_REVIEW2.md`, fixes in `XSLT2_M3A_REVIEW2_FIXES.md`).
//!
//! "Ser. §N" is *XSLT 2.0 and XQuery 1.0 Serialization (Second Edition)*,
//! REC-xslt-xquery-serialization-20101214, read at w3.org; "§N" is XSLT 2.0.
//! Where an expected string says "Saxon 9.1", it is that processor's output
//! for the same stylesheet, byte for byte.

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const XHTML: &str = "http://www.w3.org/1999/xhtml";

/// `declarations` and a template matching `/` with `content`, in an
/// `xsl:stylesheet` 2.0 that also binds `x` to `urn:x`, run over `<doc/>`;
/// the primary result serialized with `serialize_primary`.
fn out(declarations: &str, content: &str) -> Result<String, XsltError> {
    let stylesheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:x="urn:x" exclude-result-prefixes="x">
             {declarations}<xsl:template match="/">{content}</xsl:template>
           </xsl:stylesheet>"#
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled =
        Compiler::new(&names, &functions).compile_str(&stylesheet, "file:///r/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///r/in.xml", "<doc/>")?;
    compiled.serialize_primary(&outcome)
}

/// The text content of the document element of `xml`, as a parser reads it.
fn parsed_text(xml: &str) -> String {
    let doc = roxmltree::Document::parse(xml).unwrap();
    doc.root_element()
        .children()
        .filter_map(|n| n.text())
        .collect()
}

// ═══ F1: indentation and character-mapped whitespace ═════════════════════════

const SPACE_MAP: &str = r#"<xsl:character-map name="m"><xsl:output-character character=" " string="X"/></xsl:character-map>"#;

/// Ser. §4 runs character expansion — rule c, "Apply character mapping as
/// determined by the use-character-maps parameter" — before "Indentation, as
/// controlled by the indent parameter, MAY add or remove whitespace", so a
/// whitespace-only text node whose spaces the map replaces is not whitespace
/// to the indenter: it is kept, and nothing is added next to it (§5.1.3:
/// "Whitespace characters MUST NOT be added adjacent to a text node that
/// contains non-whitespace characters"). The review's case is Saxon 9.1's
/// output byte for byte. Only mapped characters count: an unmapped LF is still
/// replaced by the layout, and the whitespace the indenter writes is never
/// mapped.
#[test]
fn f1_character_mapped_whitespace_is_kept_and_not_indented() {
    let output = format!(
        r#"<xsl:output indent="yes" use-character-maps="m" omit-xml-declaration="yes"/>{SPACE_MAP}"#
    );
    let review = out(
        &output,
        r#"<r><xsl:text> </xsl:text><a/><xsl:text> </xsl:text><b/><xsl:text> </xsl:text></r>"#,
    )
    .unwrap();
    assert_eq!(review, "<r>X<a/>X<b/>X</r>");
    let nested = out(
        &output,
        r#"<doc><r><xsl:text> </xsl:text><a/><xsl:text> </xsl:text><b/><xsl:text> </xsl:text></r><s><xsl:text>&#10;</xsl:text><c/></s></doc>"#,
    )
    .unwrap();
    assert_eq!(
        nested,
        "<doc>\n   <r>X<a/>X<b/>X</r>\n   <s>\n      <c/>\n   </s>\n</doc>"
    );
}

/// Characters that are never mapped stay whitespace to the layout. §20.1:
/// "Character mapping is not applied to characters for which output escaping
/// has been disabled"; Ser. §9 adds "characters in text nodes whose parent
/// elements are listed in the cdata-section-elements parameter". In one text
/// node, a space written with escaping disabled is not mapped and a space
/// written normally is; the `disable-output-escaping` marks are found by
/// text-node ordinal, through the text of an element child before the space.
#[test]
fn f1_unmapped_whitespace_keeps_the_layout() {
    let output = format!(
        r#"<xsl:output indent="yes" use-character-maps="m" omit-xml-declaration="yes"/>{SPACE_MAP}"#
    );
    // Escaping disabled for every space: the layout replaces them.
    let disabled = out(
        &output,
        r#"<r><xsl:text disable-output-escaping="yes"> </xsl:text><a/><xsl:text disable-output-escaping="yes"> </xsl:text><b/></r>"#,
    )
    .unwrap();
    assert_eq!(disabled, "<r>\n   <a/>\n   <b/>\n</r>");
    // One text node, its first space disabled, its second mapped.
    let split = out(
        &output,
        r#"<r><xsl:text disable-output-escaping="yes"> </xsl:text><xsl:text> </xsl:text><a/></r>"#,
    )
    .unwrap();
    assert_eq!(split, "<r> X<a/></r>");
    // The text of `a` comes first in document order: the mapped space after
    // it is the second text node, and its mark (none) is that node's.
    let after_marked = out(
        &output,
        r#"<r><a><xsl:text disable-output-escaping="yes">&lt;t/&gt;</xsl:text></a><xsl:text> </xsl:text><b/></r>"#,
    )
    .unwrap();
    assert_eq!(after_marked, "<r><a><t/></a>X<b/></r>");
    let marked_after = out(
        &output,
        r#"<r><a>t</a><xsl:text disable-output-escaping="yes"> </xsl:text><b/></r>"#,
    )
    .unwrap();
    assert_eq!(marked_after, "<r>\n   <a>t</a>\n   <b/>\n</r>");
    // A cdata-section-elements element: not mapped, written as CDATA, and
    // kept as it is (as without a character map).
    let cdata = out(
        &format!(
            r#"<xsl:output indent="yes" use-character-maps="m" cdata-section-elements="r" omit-xml-declaration="yes"/>{SPACE_MAP}"#
        ),
        r#"<doc><r><xsl:text> </xsl:text><a/></r></doc>"#,
    )
    .unwrap();
    assert_eq!(cdata, "<doc>\n   <r><![CDATA[ ]]><a/>\n   </r>\n</doc>");
}

// ═══ F2: references inside CDATA sections ════════════════════════════════════

/// Ser. §5: "certain characters MUST be output as character references, to
/// ensure that they survive the round trip through serialization and parsing.
/// Specifically, CR, NEL and LINE SEPARATOR characters in text nodes MUST be
/// output respectively as "&#xD;", "&#x85;", and "&#x2028;", or their
/// equivalents … In addition, the non-whitespace control characters #x1
/// through #x1F and #x7F through #x9F in text nodes and attribute nodes MUST be
/// output as character references." In a `cdata-section-elements` element the
/// section is closed around them as §5.1.4 does for an unrepresentable
/// character: "the currently open CDATA section MUST be closed before such
/// characters, the characters MUST be output using character references …,
/// and a new CDATA section MUST be opened for any further characters". A
/// literal CR in a CDATA section comes back from a parser as LF.
#[test]
fn f2_cdata_sections_close_around_round_trip_references() {
    let output = r#"<xsl:output cdata-section-elements="r" omit-xml-declaration="yes"/>"#;
    let review = out(
        output,
        "<r><xsl:text>A&#13;B&#x85;C&#x2028;D&#x7f;E</xsl:text></r>",
    )
    .unwrap();
    assert_eq!(
        review,
        "<r><![CDATA[A]]>&#xD;<![CDATA[B]]>&#133;<![CDATA[C]]>&#8232;<![CDATA[D]]>&#127;<![CDATA[E]]></r>"
    );
    assert_eq!(parsed_text(&review), "A\rB\u{85}C\u{2028}D\u{7f}E");
    // Tab and LF stay in the section; `]]>` is still split; a reference
    // first or last opens no empty section.
    let mixed = out(
        output,
        // `]]>` is written `]]&gt;` in the stylesheet: XML 1.0 [14] CharData
        // excludes the literal sequence.
        "<r><xsl:text>&#13;a&#9;b&#10;c]]&gt;&#13;</xsl:text></r>",
    )
    .unwrap();
    assert_eq!(mixed, "<r>&#xD;<![CDATA[a\tb\nc]]]]><![CDATA[>]]>&#xD;</r>");
    assert_eq!(parsed_text(&mixed), "\ra\tb\nc]]>\r");
    // An unrepresentable character still closes the section, as before.
    let ascii = out(
        r#"<xsl:output cdata-section-elements="r" encoding="US-ASCII" omit-xml-declaration="yes"/>"#,
        "<r><xsl:text>a&#xE9;&#13;b</xsl:text></r>",
    )
    .unwrap();
    assert_eq!(ascii, "<r><![CDATA[a]]>&#233;&#xD;<![CDATA[b]]></r>");
    // An XML island of the HTML method writes what its escaped text writes.
    let island = out(
        r#"<xsl:output method="html" indent="no" cdata-section-elements="x:i"/>"#,
        "<html><x:i><xsl:text>A&#13;B&#x2028;C</xsl:text></x:i></html>",
    )
    .unwrap();
    assert_eq!(
        island,
        r#"<html><x:i xmlns:x="urn:x"><![CDATA[A]]>&#xD;<![CDATA[B]]>&#8232;<![CDATA[C]]></x:i></html>"#
    );
}

// ═══ F3: disable-output-escaping and Unicode normalization ═══════════════════

/// §20.2: "When this property is set, the normal action of the serializer to
/// escape special characters such as & and < is suppressed" — escaping only;
/// Ser. §4 rule d, "Apply Unicode Normalization if requested by the
/// normalization-form parameter", still applies. Each disabled run is
/// normalized; a character the encoding cannot hold is still written as a
/// reference ("the request to disable output escaping is ignored in respect of
/// that character"). The review's case is Saxon 9.1's output.
#[test]
fn f3_text_with_escaping_disabled_is_normalized() {
    let nfc = r#"<xsl:output normalization-form="NFC" omit-xml-declaration="yes"/>"#;
    let review = out(
        nfc,
        r#"<r><xsl:text disable-output-escaping="yes">a&#x301;</xsl:text><xsl:text>b&#x301;</xsl:text></r>"#,
    )
    .unwrap();
    assert_eq!(review, "<r>\u{E1}b\u{301}</r>");
    let markup = out(
        nfc,
        r#"<r><xsl:text disable-output-escaping="yes">&lt;e a="o&#x308;"&gt;a&#x301;&lt;/e&gt;</xsl:text></r>"#,
    )
    .unwrap();
    assert_eq!(markup, "<r><e a=\"\u{F6}\">\u{E1}</e></r>");
    let ascii = out(
        r#"<xsl:output normalization-form="NFC" encoding="US-ASCII" omit-xml-declaration="yes"/>"#,
        r#"<r><xsl:text disable-output-escaping="yes">&lt;a&#x301;&gt;</xsl:text></r>"#,
    )
    .unwrap();
    assert_eq!(ascii, "<r><&#225;></r>");
    // Without normalization-form the run is written as it is.
    let none = out(
        r#"<xsl:output omit-xml-declaration="yes"/>"#,
        r#"<r><xsl:text disable-output-escaping="yes">a&#x301;</xsl:text></r>"#,
    )
    .unwrap();
    assert_eq!(none, "<r>a\u{301}</r>");
}

// ═══ F4: XHTML names are case-sensitive ══════════════════════════════════════

/// Ser. §6: "The serialization of the instance of the data model follows the
/// same rules as for the XML output method", and XML names are case-sensitive:
/// in the XHTML namespace only `meta` is the meta element and only
/// `http-equiv` its attribute. §6.1.13 discards "any existing meta element
/// child of the head element having an http-equiv attribute with the value
/// "Content-Type", making the comparison without consideration of casing and
/// leading/trailing spaces" — the casing of the value. Both original elements
/// of the review's case are kept, as Saxon 9.1 keeps them (`META` is not an
/// element with an EMPTY content model, so it gets an end tag). Likewise the
/// URI attributes of Appendix C are `href` of `a`, not `HREF` or `A` (§6.1.12;
/// Saxon 9.1 escapes `A/@HREF`, the one difference from its output).
#[test]
fn f4_xhtml_names_are_case_sensitive() {
    let output = r#"<xsl:output method="xhtml" include-content-type="yes" indent="no" omit-xml-declaration="yes"/>"#;
    let review = out(
        output,
        &format!(
            r#"<html xmlns="{XHTML}"><head><META HTTP-EQUIV="Content-Type" content="old"/><meta HTTP-EQUIV="Content-Type" content="old2"/></head><body><A HREF="&#xE9;"/></body></html>"#
        ),
    )
    .unwrap();
    assert_eq!(
        review,
        concat!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head>"#,
            r#"<meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />"#,
            r#"<META HTTP-EQUIV="Content-Type" content="old"></META>"#,
            r#"<meta HTTP-EQUIV="Content-Type" content="old2" /></head>"#,
            "<body><A HREF=\"\u{E9}\"></A></body></html>"
        )
    );
    // The lower-case names: the value is compared without case and spaces,
    // and `a/@href` is URI-escaped; mixed-case names are not.
    let lower = out(
        output,
        &format!(
            r#"<html xmlns="{XHTML}"><head><meta http-equiv=" content-TYPE " content="old"/><title>t</title></head><body><a href="&#xE9;"/><a HREF="&#xE9;"/><A href="&#xE9;"/></body></html>"#
        ),
    )
    .unwrap();
    assert_eq!(
        lower,
        concat!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head>"#,
            r#"<meta http-equiv="Content-Type" content="text/html; charset=UTF-8" /><title>t</title></head>"#,
            "<body><a href=\"%C3%A9\"></a><a HREF=\"\u{E9}\"></a><A href=\"\u{E9}\"></A></body></html>"
        )
    );
}

/// The HTML method keeps recognizing its names "regardless of case" (Ser.
/// §7.1): the same names in no namespace are the meta element, its
/// `http-equiv`, the URI attribute `href` of `a` and the boolean attribute
/// `checked`.
#[test]
fn f4_the_html_method_still_ignores_case() {
    let result = out(
        r#"<xsl:output method="html" include-content-type="yes" indent="no"/>"#,
        r#"<HTML><HEAD><META HTTP-EQUIV="Content-Type" content="old"/><Meta Http-Equiv=" content-type " content="old2"/></HEAD><BODY><A HREF="&#xE9;">x</A><INPUT CHECKED="checked"/></BODY></HTML>"#,
    )
    .unwrap();
    assert_eq!(
        result,
        concat!(
            r#"<HTML><HEAD><meta http-equiv="Content-Type" content="text/html; charset=UTF-8"></HEAD>"#,
            r#"<BODY><A HREF="%C3%A9">x</A><INPUT CHECKED></BODY></HTML>"#
        )
    );
}
