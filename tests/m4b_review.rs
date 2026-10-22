//! M4b fix round — the findings of the independent review (`XSLT2_M4B_REVIEW.md`, F1 … F9).
//!
//! Each test cites the rule it checks: `xslt20.html` §4.3 (`input-type-annotations`), §4.4 (the source
//! trees: "the document containing the initial context node, and any document returned by the functions
//! document, doc, or collection"), §16.1, §21.1 (XTDE1665); M4 plan D-2 ("A document typed against
//! another set … is stripped with a warning"); XDM 1.0 §6.2.4 (nilled, is-id, element defaults) and
//! F&O 1.0 §15.5.2 (`fn:id`), read at w3.org. Two tests pin behaviour that waits for upstream items
//! (C39, C62): they say what changes when those land.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;
use xsd_schema::{SchemaSet, SchemaSetBuilder};

use xslt2::error::codes;
use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4b/main.xsl";
const SOURCE: &str = "file:///m4b/in.xml";
const XSD_URI: &str = "file:///m4b/r.xsd";

/// `qty` an `xs:integer`, `code` a user type, `when` a nillable `xs:date`; `e` an `xs:ID` with an
/// `xs:ID` attribute `a` (simple content); `d` an `xs:string` with a default, `@dflt` an attribute with a
/// default; `tok` and `@ta` `xs:token` (final round, C66).
const XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="qty" type="xs:integer" minOccurs="0"/>
        <xs:element name="when" type="xs:date" nillable="true" minOccurs="0"/>
        <xs:element name="e" minOccurs="0">
          <xs:complexType><xs:simpleContent>
            <xs:extension base="xs:ID"><xs:attribute name="a" type="xs:ID"/></xs:extension>
          </xs:simpleContent></xs:complexType>
        </xs:element>
        <xs:element name="d" type="xs:string" default="dflt" minOccurs="0"/>
        <xs:element name="tok" type="xs:token" minOccurs="0"/>
      </xs:sequence>
      <xs:attribute name="code" type="code"/>
      <xs:attribute name="ta" type="xs:token"/>
      <xs:attribute name="dflt" type="xs:string" default="adflt"/>
    </xs:complexType>
  </xs:element>
  <xs:simpleType name="code">
    <xs:restriction base="xs:string"><xs:pattern value="[A-Z]{2}"/></xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

const DOC: &str = r#"<r code="AB"><qty>3</qty></r>"#;

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" exclude-result-prefixes=\"xs\" {attrs}>\
           {top}</xsl:stylesheet>"
    )
}

fn root_template_with(attrs: &str, body: &str) -> String {
    sheet_with(
        attrs,
        &format!("<xsl:template match=\"/\"><out>{body}</out></xsl:template>"),
    )
}

fn resolver(main: &str, files: &[(&str, &str)]) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(XSD_URI, XSD);
    for (uri, text) in files {
        mem.insert(*uri, *text);
    }
    Arc::new(mem)
}

fn other_set() -> SchemaSet {
    SchemaSetBuilder::new()
        .add_source(XSD, "file:///elsewhere/r.xsd")
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set()
}

/// A typed tree of `DOC` the host puts into the store under a URI before the run, typed against the
/// context's own set or against another one.
enum Insert<'o> {
    Nothing,
    OwnSet(&'static str),
    OtherSet(&'static str, &'o SchemaSet),
}

/// A run of `main` (schema-aware, context with the host schema `r.xsd`): `insert`, then `go`.
fn run_sa(
    main: &str,
    files: &[(&str, &str)],
    options: TransformOptions,
    insert: Insert<'_>,
    go: impl for<'a, 't> FnOnce(&xslt2::Transformation<'a, 't>) -> Result<xslt2::Outcome<'t>, XsltError>,
) -> (Result<String, XsltError>, Vec<String>) {
    let resolver = resolver(main, files);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .unwrap()
        .schema_uri(XSD_URI)
        .build()
        .unwrap();
    let compiled = match Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
    {
        Ok(compiled) => compiled,
        Err(e) => return (Err(e), Vec::new()),
    };
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, options)
        .with_resolver(resolver);
    let (uri, set) = match insert {
        Insert::Nothing => (None, None),
        Insert::OwnSet(uri) => (Some(uri), Some(context.schema_set())),
        Insert::OtherSet(uri, set) => (Some(uri), Some(set)),
    };
    if let (Some(uri), Some(set)) = (uri, set) {
        let doc = build_typed_document(
            DOC.as_bytes(),
            &arena,
            set,
            BufferDocumentOptions::default(),
        )
        .unwrap();
        run.store().borrow_mut().insert(doc, Some(uri));
    }
    let result = go(&run)
        .and_then(|outcome| compiled.serialize_primary(&outcome))
        .map(common::body);
    (result, run.take_warnings())
}

// ── F1: a host-inserted typed tree reached by doc() / document() is admitted ─────────

/// §4.4's source trees include "any document returned by the functions document, doc"; D-2: a tree typed
/// against another schema set is stripped with a warning — reached through `doc()` and `document()` as
/// well as as the principal source.
#[test]
fn doc_of_a_host_inserted_tree_of_another_set_is_stripped_with_a_warning() {
    let other = other_set();
    for call in ["doc('urn:side')", "document('urn:side')"] {
        let main = root_template_with(
            "",
            &format!(
                "<xsl:value-of select=\"data(({call}//qty)[1]) instance of xs:untypedAtomic, \
                 data(({call}/r/@code)) instance of xs:untypedAtomic\"/>"
            ),
        );
        let (result, warnings) = run_sa(
            &main,
            &[],
            TransformOptions::default(),
            Insert::OtherSet("urn:side", &other),
            |run| run.transform_source(SOURCE, "<doc/>"),
        );
        assert_eq!(result.unwrap(), "<out>true true</out>", "{call}");
        assert_eq!(warnings.len(), 1, "{call}: {warnings:?}");
        assert!(warnings[0].contains("urn:side"), "{}", warnings[0]);
        // F8: the warning does not claim the stylesheet said `strip`.
        assert!(
            !warnings[0].contains("input-type-annotations="),
            "{}",
            warnings[0]
        );
    }
}

/// §4.3: under `input-type-annotations="strip"` a tree typed against the context's own set, reached
/// through `doc()`, is stripped too — without a warning (nothing is wrong with it).
#[test]
fn doc_of_a_host_inserted_tree_is_stripped_under_input_type_annotations_strip() {
    let main = root_template_with(
        r#"input-type-annotations="strip""#,
        "<xsl:value-of select=\"data((doc('urn:side')//qty)[1]) instance of xs:untypedAtomic\"/>",
    );
    let (result, warnings) = run_sa(
        &main,
        &[],
        TransformOptions::default(),
        Insert::OwnSet("urn:side"),
        |run| run.transform_source(SOURCE, "<doc/>"),
    );
    assert_eq!(result.unwrap(), "<out>true</out>");
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// §21.1, XTDE1665: "the input to the processor … includes … trees accessed using the document, doc
/// and collection functions" — a basic processor refuses a typed tree it reaches through `doc()`, and
/// strips it under `input-type-annotations="strip"` ("This error will not arise if the
/// input-type-annotations attribute is set to strip").
#[test]
fn a_basic_processor_admits_doc_of_a_host_inserted_typed_tree_only_under_strip() {
    let other = other_set();
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    for (attrs, expected) in [
        (r#"input-type-annotations="strip""#, Ok("<out>true</out>")),
        ("", Err(codes::XTDE1665)),
    ] {
        let main = root_template_with(
            attrs,
            "<xsl:value-of select=\"data((doc('urn:side')//qty)[1]) instance of xs:untypedAtomic\"/>",
        );
        let compiled = Compiler::new(&names, &functions)
            .compile_str(&main, MAIN)
            .unwrap();
        let arena = Bump::new();
        let doc = build_typed_document(
            DOC.as_bytes(),
            &arena,
            &other,
            BufferDocumentOptions::default(),
        )
        .unwrap();
        let run = compiled.transformation(&arena, TransformOptions::default());
        run.store().borrow_mut().insert(doc, Some("urn:side"));
        let result = run
            .transform_source(SOURCE, "<doc/>")
            .and_then(|outcome| compiled.serialize_primary(&outcome))
            .map(common::body);
        match expected {
            Ok(text) => assert_eq!(result.unwrap(), text, "{attrs}"),
            Err(code) => assert_eq!(result.unwrap_err().code, code, "{attrs}"),
        }
    }
}

// ── F2: nilled follows the PSVI in the admission copy ────────────────────────────────

/// XDM 1.0 §6.2.4: nilled is true when the element was valid with `[nil]` true; XML Schema allows a
/// nilled element comments and processing instructions (cvc-elt.3.2.1 forbids character and element
/// children only). The admission copy made for `xsl:strip-space` keeps it.
#[test]
fn nilled_survives_the_admission_copy_of_an_element_with_a_comment_child() {
    let source = r#"<r xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><when xsi:nil="true"><!--c--><?p d?></when></r>"#;
    for top in ["", r#"<xsl:strip-space elements="*"/>"#] {
        let main = sheet_with(
            "",
            &format!(
                "{top}<xsl:template match=\"/\"><out><xsl:value-of \
                 select=\"nilled(/r/when), count(data(/r/when)), count(/r/when/node())\"/></out>\
                 </xsl:template>"
            ),
        );
        let (result, _) = run_sa(
            &main,
            &[],
            TransformOptions::default(),
            Insert::Nothing,
            |run| run.transform_source_with(SOURCE, source, SourceValidation::Strict),
        );
        assert_eq!(result.unwrap(), "<out>true 0 2</out>", "{top}");
    }
}

// ── F3 (pinned): element and attribute defaults wait for upstream C39 ───────────────

/// XDM 1.0 §6.2.4: "where a fixed or default value for an element is defined in the schema, and the
/// element takes this default value, a text node will be created to contain the value"; §4.3's Note:
/// "any defaulted elements and attributes that were added to the tree by the validation process will
/// still be present". **Pinned, not met**: upstream's typed build creates neither the text node of an
/// element default nor a defaulted attribute (WAVE_B C39, extended by the M4b review F3) — the typed
/// value of `d` is the default, its string value empty, and under `strip` the default is gone. When
/// C39 lands both rows become `[dflt][dflt][adflt]`. No catalog schema declares an element default or
/// fixed value (VERIFIED, fix round).
#[test]
fn element_and_attribute_defaults_are_not_materialized_yet() {
    for (attrs, expected) in [
        ("", "<out>[][dflt][]</out>"),
        (r#"input-type-annotations="strip""#, "<out>[][][]</out>"),
    ] {
        let main = root_template_with(
            attrs,
            "<xsl:value-of select=\"concat('[', string(/r/d), '][', string-join(data(/r/d), ''), \
             '][', string(/r/@dflt), ']')\"/>",
        );
        let (result, _) = run_sa(
            &main,
            &[],
            TransformOptions::default(),
            Insert::Nothing,
            |run| run.transform_source_with(SOURCE, "<r><d/></r>", SourceValidation::Strict),
        );
        assert_eq!(result.unwrap(), expected, "{attrs}");
    }
}

// ── F4 (pinned): element ids wait for upstream C62 ───────────────────────────────────

/// F&O 1.0 §15.5.2 `fn:id` selects an element when "The is-id property … of the element node is true,
/// and the typed value of the element node is equal to V", or when it "has an attribute node whose
/// is-id property … is true"; §4.3: under `strip` "The values of the is-id and is-idrefs properties
/// are not changed". **Pinned**: the attribute id `a1` is found, typed and stripped; the element id
/// `id1` is found in neither, because upstream's id index files attributes only (C62). When C62 lands
/// the typed row becomes `true true`, and the strip copy must then also register the ids the element
/// itself answers to (today it re-registers what its attributes answer to), or the stripped row stays
/// `true false` — the follow-up recorded in the report.
#[test]
fn element_ids_wait_for_c62_and_strip_keeps_attribute_ids() {
    for attrs in ["", r#"input-type-annotations="strip""#] {
        let main = root_template_with(
            attrs,
            "<xsl:value-of select=\"exists(id('a1')), exists(id('id1'))\"/>",
        );
        let (result, _) = run_sa(
            &main,
            &[],
            TransformOptions::default(),
            Insert::Nothing,
            |run| {
                run.transform_source_with(
                    SOURCE,
                    r#"<r><e a="a1">id1</e></r>"#,
                    SourceValidation::Strict,
                )
            },
        );
        assert_eq!(result.unwrap(), "<out>true false</out>", "{attrs}");
    }
}

// ── F7: the stylesheet's own modules are not sources the default validates ──────────

/// `document('')` "refers to the document node of the containing stylesheet module" (§16.1). Under a
/// transformation-wide `Strict` default it is read unvalidated — a stylesheet module is not a source
/// the host asked to validate (decision D-V) — unless the host names the URI itself
/// (`set_source_validation`), which then validates it like any other document ("The XML resource
/// containing the stylesheet module is processed exactly as if it were any other XML document").
#[test]
fn the_stylesheet_module_read_by_document_is_not_validated_by_the_default() {
    let main = root_template_with("", "<xsl:value-of select=\"document('')/*/@version\"/>");
    let strict = || TransformOptions {
        source_validation: SourceValidation::Strict,
        ..Default::default()
    };
    let (result, _) = run_sa(&main, &[], strict(), Insert::Nothing, |run| {
        run.transform_source_with(SOURCE, "<r/>", SourceValidation::Skip)
    });
    assert_eq!(result.unwrap(), "<out>2.0</out>");
    let (result, _) = run_sa(&main, &[], strict(), Insert::Nothing, |run| {
        run.set_source_validation(MAIN, SourceValidation::Strict);
        run.transform_source_with(SOURCE, "<r/>", SourceValidation::Skip)
    });
    let e = result.unwrap_err();
    assert_eq!(e.code, "FODC0002", "{e}");
}

/// `set_source_validation` sets the mode of one URI the resolver serves, without supplying its text:
/// here it exempts an invalid document from the transformation-wide `Strict`.
#[test]
fn set_source_validation_overrides_the_default_for_one_uri() {
    let main = root_template_with(
        "",
        "<xsl:value-of select=\"count(doc('bad.xml')//qty), data((doc('good.xml')//qty)[1]) instance of xs:integer\"/>",
    );
    let files = [
        ("file:///m4b/bad.xml", "<r><qty>three</qty></r>"),
        ("file:///m4b/good.xml", DOC),
    ];
    let options = TransformOptions {
        source_validation: SourceValidation::Strict,
        ..Default::default()
    };
    let (result, _) = run_sa(&main, &files, options, Insert::Nothing, |run| {
        run.set_source_validation("file:///m4b/bad.xml", SourceValidation::Skip);
        run.transform_source_with(SOURCE, "<r/>", SourceValidation::Skip)
    });
    assert_eq!(result.unwrap(), "<out>1 true</out>");
}

// ── F8: small fixes ──────────────────────────────────────────────────────────────────

/// `register_source` after `register_source_with` for the same URI takes the run's default again
/// (the later registration wins, its text and its mode).
#[test]
fn a_later_register_source_resets_the_mode_of_the_uri() {
    let main = root_template_with(
        "",
        "<xsl:value-of select=\"data((doc('a.xml')//qty)[1]) instance of xs:untypedAtomic\"/>",
    );
    let (result, _) = run_sa(
        &main,
        &[],
        TransformOptions::default(),
        Insert::Nothing,
        |run| {
            run.register_source_with("file:///m4b/a.xml", DOC, SourceValidation::Strict);
            run.register_source("file:///m4b/a.xml", DOC);
            run.transform_source(SOURCE, "<doc/>")
        },
    );
    assert_eq!(result.unwrap(), "<out>true</out>");
}

/// A host schema given twice is loaded once and listed once.
#[test]
fn a_host_schema_given_twice_is_listed_once() {
    let resolver = resolver("", &[]);
    let context = SchemaContext::builder()
        .with_resolver(resolver)
        .schema_uri(XSD_URI)
        .schema_uri(XSD_URI)
        .build()
        .unwrap();
    assert_eq!(context.host_schemas(), [XSD_URI.to_string()]);
}

/// The `XSLT2-INTERNAL` message for a parameter typed against another set reads as one sentence (no run
/// of spaces from a broken line continuation).
#[test]
fn the_message_for_a_parameter_of_another_set_has_no_run_of_spaces() {
    let other = other_set();
    let main = sheet_with(
        "",
        r#"<xsl:param name="p"/><xsl:template match="/"><out><xsl:value-of select="count($p//qty)"/></out></xsl:template>"#,
    );
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .unwrap()
        .build()
        .unwrap();
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let doc = build_typed_document(
        DOC.as_bytes(),
        &arena,
        &other,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    let doc: &xsd_schema::document::BufferDocument<'_> = arena.alloc(doc);
    let value = xsd_schema::xpath::functions::XPathValue::from_node(doc.create_navigator());
    let e = compiled
        .transformation(&arena, TransformOptions::default())
        .with_param(xslt2::QName::local("p"), value)
        .transform_source(SOURCE, "<doc/>")
        .err()
        .expect("refused");
    assert_eq!(e.code, xslt2::error::codes_internal::INTERNAL, "{e}");
    assert!(!e.message.contains("  "), "{:?}", e.message);
}

// ── Final round: the [schema normalized value] (upstream C66) ─────────────────────────

/// XDM 1.0 §6.2.4 (an element of a simple type or with simple content: "its string-value is the
/// [schema normalized value] of the node") and §6.3.4 (an attribute's string value: "The [schema
/// normalized value] PSVI property if that exists"): an `xs:token` element and attribute of a validated
/// source have their whitespace collapsed in the tree — `string()`, a general comparison and a key on
/// the string value see `a b` — and §4.3 keeps it under `input-type-annotations="strip"` ("The typed
/// value of every element and attribute node is set to be the same as its string value"), where the
/// values are untyped. Upstream's typed build stores the normalized value since `wave-b` `66e3e3f`
/// (C66); before, the tree kept the text as read.
#[test]
fn a_token_element_and_attribute_have_their_normalized_string_value() {
    let source = "<r ta=\"  a   b  \"><tok>  a\n   b  </tok></r>";
    for attrs in ["", r#"input-type-annotations="strip""#] {
        let main = sheet_with(
            attrs,
            r#"<xsl:key name="k" match="tok" use="string(.)"/>
               <xsl:key name="ka" match="r" use="string(@ta)"/>
               <xsl:template match="/"><out><xsl:value-of select="
                 concat('[', string(/r/tok), '][', string(/r/@ta), ']'),
                 /r/tok = 'a b', /r/@ta = 'a b',
                 count(key('k', 'a b')), count(key('ka', 'a b'))"/></out></xsl:template>"#,
        );
        let (result, _) = run_sa(
            &main,
            &[],
            TransformOptions::default(),
            Insert::Nothing,
            |run| run.transform_source_with(SOURCE, source, SourceValidation::Strict),
        );
        assert_eq!(
            result.unwrap(),
            "<out>[a b][a b] true true 1 1</out>",
            "{attrs}"
        );
    }
    // Not validated, the same text keeps its whitespace (an untyped tree is the text as read).
    let main = root_template_with(
        "",
        "<xsl:value-of select=\"string-length(/r/tok), string-length(/r/@ta)\"/>",
    );
    let (result, _) = run_sa(
        &main,
        &[],
        TransformOptions::default(),
        Insert::Nothing,
        |run| run.transform_source_with(SOURCE, source, SourceValidation::Skip),
    );
    // `@ta` is attribute-value normalized by the XML parser (XML 1.0 §3.3.3: no newline in it, spaces
    // kept), the element text is not.
    assert_eq!(result.unwrap(), "<out>10 9</out>");
}
