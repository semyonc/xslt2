//! The host's follow-up to upstream Wave B batch (t) (`wave-b` `d5f2a8e`; `../../WAVE_B_BATCH_T_REPORT.md`):
//! the typed build now gives defaulted attributes and element defaults as nodes (C39), drops the
//! whitespace text of element-only content (C67), files an element whose typed value is an
//! `xs:ID` in the id index (C62), and has the is-idrefs property and `fn:idref` (C54).
//!
//! What the host does with it, each test citing its rule (`xslt20.html`; XDM 1.0 and F&O 1.0 as the
//! batch (t) report quotes them):
//!
//! - §4.3 (`input-type-annotations="strip"`) and §19.2.1.1 (`validation="strip"`): "The values of
//!   the is-id and is-idrefs properties are not changed" / "are unchanged" — for an element that is
//!   an ID by its own typed value, and for the is-idrefs property of elements and attributes, on
//!   the strip admission copy, on strip copies (`xsl:copy-of`, `xsl:copy`, `type="xs:untyped"`, the
//!   public pipeline) and under a `strip` boundary; §11.9.1: the copy of an attribute "will also
//!   have the same values for the is-id and is-idrefs properties".
//! - XDM 1.0 §6.7.4: whitespace text in element-only content is no text node — now the typed
//!   build's rule; the host's own copy of it went, with no change of behaviour.
//! - §4.3's Note and §4.4: defaults stay through `strip`, and §4.4 strips a defaulted element's
//!   whitespace text only once its annotation is gone.
//!
//! Every test goes through the public API.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::navigator::DomNavigator;
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSetBuilder;

use xslt2::receiver::{CopyNamespaces, CopyValidation};
use xslt2::{
    Compiler, ComplexContentNormalizer, MemoryResolver, Name, Receiver, Resolver, ResultBuilder,
    SchemaContext, SourceValidation, TransformOptions, XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///tf/main.xsl";
const SOURCE: &str = "file:///tf/in.xml";
const XSD_URI: &str = "file:///tf/t.xsd";

/// A no-namespace schema. `doc`: `row`s (an `xs:ID` attribute `id`, an `xs:IDREF` `ref`, an
/// `xs:IDREFS` `refs`), then elements that are IDs by their own typed value — `eid` (`xs:ID`), `cid`
/// (simple content extending `xs:ID`, with an `xs:string` attribute), `rid` (a restriction of
/// `xs:ID`) —, the IDREF elements `eref` (`xs:IDREF`) and `erefs` (`xs:IDREFS`), and `s`, an
/// `xs:string` (no ID). `r`: elements with defaults (`d` `dflt`, `ws` a single space) and an
/// attribute default. `wrap`: no declaration (for lax validation).
const XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="doc">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="row" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="ref" type="xs:IDREF"/>
            <xs:attribute name="refs" type="xs:IDREFS"/>
          </xs:complexType>
        </xs:element>
        <xs:element ref="eid" minOccurs="0" maxOccurs="unbounded"/>
        <xs:element name="cid" minOccurs="0">
          <xs:complexType><xs:simpleContent>
            <xs:extension base="xs:ID"><xs:attribute name="note" type="xs:string"/></xs:extension>
          </xs:simpleContent></xs:complexType>
        </xs:element>
        <xs:element name="rid" type="rid" minOccurs="0"/>
        <xs:element name="eref" type="xs:IDREF" minOccurs="0"/>
        <xs:element name="erefs" type="xs:IDREFS" minOccurs="0"/>
        <xs:element name="s" type="xs:string" minOccurs="0"/>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:element name="eid" type="xs:ID"/>
  <xs:simpleType name="rid">
    <xs:restriction base="xs:ID"><xs:pattern value="k.*"/></xs:restriction>
  </xs:simpleType>
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="d" type="xs:string" default="dflt" minOccurs="0"/>
        <xs:element name="ws" type="xs:string" default=" " minOccurs="0"/>
      </xs:sequence>
      <xs:attribute name="dflt" type="xs:string" default="adflt"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;

/// Valid against `XSD`: the IDREFs point at attribute ids (`r1`, `r2`) and element ids (`e1`,
/// `e2`, `k1`).
const DOC: &str = r#"<doc><row id="r1" refs="e1 k1"/><row id="r2" ref="r1"/><eid>e1</eid><eid>e2</eid><cid note="n">c1</cid><rid>k1</rid><eref>r2</eref><erefs>e2 r1</erefs><s>s1</s></doc>"#;

/// What `fn:id` and `fn:idref` find in the tree `$t` (or the context document, for `""`): the
/// names of the elements the ids `r1 e1 e2 c1 k1 s1` select (document order), then, for each of
/// `r1 e1 e2 r2 k1`, the names of the nodes `fn:idref` returns (`+`-joined).
fn probe(tree: &str) -> String {
    let arg = if tree.is_empty() {
        String::new()
    } else {
        format!(", {tree}")
    };
    format!(
        "concat(string-join(id(('r1', 'e1', 'e2', 'c1', 'k1', 's1'){arg})/name(), ' '), ' | ', \
         string-join(for $i in ('r1', 'e1', 'e2', 'r2', 'k1') return \
         string-join(idref($i{arg})/name(), '+'), ' '))"
    )
}

/// The answer of [`probe`] over a tree with every id and is-idrefs property of `DOC`.
const ALL: &str = "row eid eid cid rid | ref+erefs refs erefs eref refs";

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" exclude-result-prefixes=\"xs\" {attrs}>\
           {top}</xsl:stylesheet>"
    )
}

/// A schema-aware run of `main` (host schema `XSD`) over `source`, validated as `validation` says:
/// the text of its `<out>` element.
fn run(main: &str, source: &str, validation: SourceValidation) -> Result<String, XsltError> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, XSD);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)?
        .schema_uri(XSD_URI)
        .build()?;
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)?;
    let arena = Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = transformation.transform_source_with(SOURCE, source, validation)?;
    let text = common::body(compiled.serialize_primary(&outcome)?);
    let text = text.trim();
    Ok(if text == "<out/>" {
        String::new()
    } else {
        text.strip_prefix("<out>")
            .and_then(|t| t.strip_suffix("</out>"))
            .unwrap_or_else(|| panic!("not an <out> element: {text}"))
            .to_string()
    })
}

/// The `<out>` text of a template for `/` that declares `top` and writes `select` (the stylesheet
/// attributes `attrs`, the declarations `decls` beside the template), over `DOC` validated strictly.
fn answer(attrs: &str, decls: &str, top: &str, select: &str) -> String {
    let main = sheet_with(
        attrs,
        &format!(
            r#"{decls}<xsl:template match="/">{top}<out><xsl:value-of select="{select}"/></out></xsl:template>"#
        ),
    );
    run(&main, DOC, SourceValidation::Strict)
        .unwrap_or_else(|e| panic!("the run failed: {e}\n{top}\n{select}"))
}

// ── §4.3: the strip admission copy ───────────────────────────────────────────────────────────

/// §4.3: when type annotations are stripped, "The values of the is-id and is-idrefs properties are
/// not changed" — and the Note: "elements and attributes validated as IDs will still be
/// accessible using the id function". The typed tree has every id (C62: `eid`, `cid`, `rid` by
/// their own typed value) and every is-idrefs node (C54); the strip admission copy is untyped and
/// keeps them all.
#[test]
fn the_strip_admission_copy_keeps_element_ids_and_is_idrefs() {
    let untyped = "/doc/eid[1] instance of element(*, xs:untyped), \
                   /doc/erefs instance of element(*, xs:untyped), \
                   /doc/row[1]/@refs instance of attribute(*, xs:untypedAtomic)";
    assert_eq!(answer("", "", "", &probe("")), ALL, "typed");
    assert_eq!(answer("", "", "", untyped), "false false false", "typed");
    let strip = r#"input-type-annotations="strip""#;
    assert_eq!(answer(strip, "", "", &probe("")), ALL, "strip");
    assert_eq!(answer(strip, "", "", untyped), "true true true", "strip");
    // §4.4 after §4.3: the same with whitespace stripping in the one admission copy.
    let space = r#"<xsl:strip-space elements="*"/>"#;
    assert_eq!(
        answer(strip, space, "", &probe("")),
        ALL,
        "strip, strip-space"
    );
}

// ── §19.2.1.1 / §11.9: strip copies ──────────────────────────────────────────────────────────

/// §19.2.1.1, `strip`: "the new node and each of the contained nodes will have the type annotation
/// xs:untyped … The values of the is-id and is-idrefs properties are unchanged"; §11.9.2: "The new
/// element will have the same values of the is-id, is-idrefs, and nilled properties as the original
/// element". A copy of the document element into a temporary tree, of the document node, inside a
/// literal result element (the shape of `copy-5034`), with `type="xs:untyped"` (the effect of
/// `strip`, decision D-f3) — and, as the control, `preserve`.
#[test]
fn strip_copies_keep_element_ids_and_is_idrefs() {
    for (variable, untyped) in [
        (
            r#"<xsl:variable name="t"><xsl:copy-of select="/doc" validation="strip"/></xsl:variable>"#,
            "$t/doc/eid[1] instance of element(*, xs:untyped)",
        ),
        (
            r#"<xsl:variable name="t" as="document-node()"><xsl:copy-of select="/" validation="strip"/></xsl:variable>"#,
            "$t/doc/eid[1] instance of element(*, xs:untyped)",
        ),
        (
            r#"<xsl:variable name="t"><Z><xsl:copy-of select="/doc" validation="strip"/></Z></xsl:variable>"#,
            "$t/Z/doc/eid[1] instance of element(*, xs:untyped)",
        ),
        (
            r#"<xsl:variable name="t"><xsl:copy-of select="/doc" type="xs:untyped"/></xsl:variable>"#,
            "$t/doc/eid[1] instance of element(*, xs:untyped)",
        ),
        (
            r#"<xsl:variable name="t"><xsl:copy-of select="/doc" validation="preserve"/></xsl:variable>"#,
            "not($t/doc/eid[1] instance of element(*, xs:untyped))",
        ),
    ] {
        assert_eq!(
            answer("", "", variable, &format!("{untyped}, {}", probe("$t"))),
            format!("true {ALL}"),
            "{variable}"
        );
    }
}

/// §11.9.1 (`xsl:copy`): "In the case of an attribute node, it will also have the same values for
/// the is-id and is-idrefs properties"; §11.9.2: an attribute selected on its own by
/// `xsl:copy-of validation="strip"` keeps them too — copied onto a new element.
#[test]
fn a_copied_attribute_keeps_is_idrefs() {
    let top = r#"<xsl:variable name="t"><x><xsl:for-each select="/doc/row[2]/@ref"><xsl:copy validation="strip"/></xsl:for-each><xsl:copy-of select="/doc/row[1]/@refs" validation="strip"/></x></xsl:variable>"#;
    assert_eq!(
        answer(
            "",
            "",
            top,
            "$t/x/@ref instance of attribute(*, xs:untypedAtomic), \
             string-join(idref('r1', $t)/name(), '+'), string-join(idref('k1', $t)/name(), '+')"
        ),
        "true ref refs"
    );
}

/// The same rule whichever receivers build the copy (M4e's fix round): the public pipeline, a
/// `ComplexContentNormalizer` over a bare `ResultBuilder`, keeps the element ids and the is-idrefs
/// property of a `strip` copy — and a `preserve` copy has them through its annotations.
#[test]
fn a_strip_copy_keeps_them_through_the_public_pipeline() {
    let set = SchemaSetBuilder::new()
        .add_source(XSD, XSD_URI)
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let arena = Bump::new();
    let source = build_typed_document(
        DOC.as_bytes(),
        &arena,
        &set,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    let mut root = source.create_navigator();
    root.move_to_root();
    for (validation, typed) in [
        (CopyValidation::Strip, false),
        (CopyValidation::Preserve, true),
    ] {
        let mut out = ComplexContentNormalizer::new(
            ResultBuilder::new(&arena, &set.name_table, Some(&set)).unwrap(),
        );
        out.start_document().unwrap();
        out.start_element(&Name::local("x"), None, false).unwrap();
        out.end_start_tag().unwrap();
        out.append_copy(
            &XmlItem::Node(root.clone()),
            CopyNamespaces::All,
            validation,
        )
        .unwrap();
        out.end_element().unwrap();
        out.end_document().unwrap();
        let doc = out.into_inner().finish().unwrap();
        assert_eq!(doc.has_type_annotations(), typed, "{validation:?}");
        for id in ["r1", "e1", "e2", "c1", "k1"] {
            assert!(
                doc.get_element_by_id(id).is_some(),
                "{validation:?}: id {id}"
            );
        }
        assert!(doc.get_element_by_id("s1").is_none(), "{validation:?}: s1");
        let nav = doc.create_navigator();
        let names = |id: &str| -> Vec<String> {
            nav.find_nodes_by_idref(id)
                .unwrap()
                .iter()
                .map(|n| n.local_name().to_string())
                .collect()
        };
        assert_eq!(names("r1"), ["ref", "erefs"], "{validation:?}: r1");
        assert_eq!(names("e1"), ["refs"], "{validation:?}: e1");
        assert_eq!(names("r2"), ["eref"], "{validation:?}: r2");
    }
}

// ── §19.2.1.1: nodes under a strip boundary ──────────────────────────────────────────────────

/// §19.2.1.1, `strip` on a constructed element: "Any previous type annotation present on a
/// contained element or attribute node (for example, a type annotation that is present on an
/// element copied from a source document) is also replaced by xs:untyped … The values of the is-id
/// and is-idrefs properties are unchanged" — for typed nodes sequenced into the element (whose
/// annotations travel with the copy until the boundary strips them), and for an element a `strict`
/// validation inside the `strip` boundary typed (unchanged from what that validation made of it,
/// as for attributes since M4d).
#[test]
fn nodes_a_strip_boundary_untypes_keep_element_ids_and_is_idrefs() {
    assert_eq!(
        answer(
            "",
            "",
            r#"<xsl:variable name="t"><w xsl:validation="strip"><xsl:sequence select="/doc/*"/></w></xsl:variable>"#,
            &format!(
                "$t/w/eid[1] instance of element(*, xs:untyped), {}",
                probe("$t")
            )
        ),
        format!("true {ALL}")
    );
    assert_eq!(
        answer(
            "",
            "",
            r#"<xsl:variable name="t"><w xsl:validation="strip"><doc xsl:validation="strict"><row id="r9" ref="e9"/><eid>e9</eid><eref>r9</eref><erefs>e9 r9</erefs></doc></w></xsl:variable>"#,
            "$t/w/doc/eid instance of element(*, xs:untyped), \
             string-join(id(('r9', 'e9'), $t)/name(), ' '), \
             string-join(idref('e9', $t)/name(), '+'), string-join(idref('r9', $t)/name(), '+')"
        ),
        "true row eid ref+erefs eref+erefs"
    );
}

// ── §4.3 Note, §4.4: defaults (C39) ──────────────────────────────────────────────────────────

/// §4.3's Note: "any defaulted elements and attributes that were added to the tree by the
/// validation process will still be present" (the typed build creates them since C39); §4.4: "If an
/// element in a source document has a type annotation that is a simple type or a complex type with
/// simple content, then any whitespace text nodes among its children are preserved" — so `ws`,
/// whose default is one space, keeps its text node under `xsl:strip-space` while typed, and loses it
/// once §4.3 stripped its annotation ("Stripping of type annotations happens before stripping of
/// whitespace text nodes").
#[test]
fn defaults_are_in_the_tree_and_section_4_4_sees_them() {
    let main = |attrs: &str, decls: &str| {
        sheet_with(
            attrs,
            &format!(
                r#"{decls}<xsl:template match="/"><out><xsl:value-of select="string(/r/d), count(/r/ws/text()), string-length(/r/ws), string(/r/@dflt), count(/r/@*)"/></out></xsl:template>"#
            ),
        )
    };
    let strip = r#"input-type-annotations="strip""#;
    let space = r#"<xsl:strip-space elements="*"/>"#;
    for (attrs, decls, expected) in [
        ("", "", "dflt 1 1 adflt 1"),
        ("", space, "dflt 1 1 adflt 1"),
        (strip, "", "dflt 1 1 adflt 1"),
        (strip, space, "dflt 0 0 adflt 1"),
    ] {
        assert_eq!(
            run(
                &main(attrs, decls),
                "<r><d/><ws/></r>",
                SourceValidation::Strict
            )
            .unwrap(),
            expected,
            "{attrs} {decls}"
        );
    }
}

/// M4d's construction validation makes its own defaulted attributes (`get_default_attributes`):
/// one per attribute use, whether the element is constructed, or copied from a typed source that
/// now has the defaulted attribute as a node (C39) — no duplicate.
#[test]
fn constructed_and_copied_elements_get_each_default_once() {
    assert_eq!(
        answer(
            "",
            "",
            r#"<xsl:variable name="t"><r xsl:validation="strict"><d/></r></xsl:variable>"#,
            "count($t/r/@*), string($t/r/@dflt), string($t/r/d)"
        ),
        "1 adflt dflt"
    );
    let main = sheet_with(
        "",
        r#"<xsl:template match="/"><xsl:variable name="t"><xsl:copy-of select="/r" validation="strict"/></xsl:variable><out><xsl:value-of select="count(/r/@*), count($t/r/@*), string($t/r/@dflt), string($t/r/d)"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<r><d/></r>", SourceValidation::Strict).unwrap(),
        "1 1 adflt dflt"
    );
}

// ── XDM 1.0 §6.7.4: whitespace in element-only content (C67) ─────────────────────────────────

/// XDM 1.0 §6.7.4: "if the resulting Text Node consists entirely of whitespace and the character
/// information items used to construct this node have a parent and that parent is an element and
/// its {content type} is not "mixed", then the content of the Text Node is the zero-length string …
/// an empty Text Node will be discarded" — the typed build's rule since C67, which replaced the
/// host's own: CDATA whitespace and character references included (one text node of the parse);
/// `s` (simple content) and an element no declaration governs (`wrap`, validated laxly: "If they do
/// not identify any declaration or definition, then ·lax assessment· is performed") keep theirs;
/// untyped, all stay.
#[test]
fn whitespace_in_element_only_content_is_not_in_the_typed_tree() {
    let main = sheet_with(
        "",
        r#"<xsl:template match="/"><out><xsl:value-of select="count(//doc/text()), string-length(//s), count(/wrap/text())"/></out></xsl:template>"#,
    );
    let doc = "<doc>\n  <![CDATA[ ]]>&#x20;<row id=\"r1\"/>\t<s>  </s>\n</doc>";
    assert_eq!(run(&main, doc, SourceValidation::Strict).unwrap(), "0 2 0");
    assert_eq!(run(&main, doc, SourceValidation::Skip).unwrap(), "3 2 0");
    let wrapped = format!("<wrap>\n {doc}\n</wrap>");
    assert_eq!(
        run(&main, &wrapped, SourceValidation::Lax).unwrap(),
        "0 2 2"
    );
    assert_eq!(
        run(&main, &wrapped, SourceValidation::Skip).unwrap(),
        "3 2 2"
    );
}
