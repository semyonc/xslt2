//! M4b — typed source documents (`xslt_doc/M4_PLAN.md` §4 M4b, decisions D-2, D-4, D-13).
//!
//! A schema-aware run validates a source document when it enters the run, if the host asks for
//! it: a transformation-wide default for every source-tree loader (`TransformOptions::
//! source_validation`, default `Skip` — D-4) and a per-call mode (`transform_source_with`,
//! `load_document_with`, `register_source_with`). The schemas come from the one schema set of the
//! stylesheet's `SchemaContext` (D-2), which the host can extend (§3.13: "An implementation may
//! define mechanisms that allow additional schema components to be added to the in-scope schema
//! components for the stylesheet") with `SchemaContextBuilder::schema_str` / `schema_uri` /
//! `schema_file`. A typed document atomizes to typed values; an untyped one exactly as before
//! (D-13, §2.8).
//!
//! Every test goes through the public API and cites the rule it checks: `xslt20.html` §2.8, §3.13,
//! §4.3 (`input-type-annotations`), §4.4 (whitespace stripping, the simple-content exemption),
//! §13.1.2 / §14.3 / §16.3 (typed sort, grouping and key values), §21.1 (XTDE1665); XDM 1.0 §6.7.4
//! (whitespace text nodes of a PSVI) and F&O 1.0 §15.5.4 (`fn:doc`, FODC0002), read at w3.org.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;
use xsd_schema::SchemaSetBuilder;

use xslt2::error::codes;
use xslt2::{
    Compiler, ImportSource, MemoryResolver, Resolver, SchemaContext, SourceValidation,
    TransformOptions, XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4b/main.xsl";
const SOURCE: &str = "file:///m4b/in.xml";
const ORDERS_XSD_URI: &str = "file:///m4b/orders.xsd";

/// A no-namespace schema: `orders` holds `order` elements (element-only content); an `order` has
/// typed children — `qty` an `xs:integer`, `note` an `xs:string`, `when` a nillable `xs:date`, `memo`
/// mixed content, `tag` a complex type with simple content — and typed attributes, `id` an `xs:ID`.
const ORDERS_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="orders">
    <xs:complexType><xs:sequence>
      <xs:element ref="order" maxOccurs="unbounded"/>
    </xs:sequence></xs:complexType>
  </xs:element>
  <xs:element name="order">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="qty" type="xs:integer"/>
        <xs:element name="note" type="xs:string" minOccurs="0"/>
        <xs:element name="when" type="xs:date" minOccurs="0" nillable="true"/>
        <xs:element name="memo" minOccurs="0">
          <xs:complexType mixed="true">
            <xs:sequence><xs:element name="b" type="xs:string" minOccurs="0"/></xs:sequence>
          </xs:complexType>
        </xs:element>
        <xs:element name="tag" minOccurs="0">
          <xs:complexType><xs:simpleContent>
            <xs:extension base="xs:string"><xs:attribute name="kind" type="xs:string"/></xs:extension>
          </xs:simpleContent></xs:complexType>
        </xs:element>
      </xs:sequence>
      <xs:attribute name="id" type="xs:ID"/>
      <xs:attribute name="price" type="xs:decimal"/>
      <xs:attribute name="code" type="code"/>
    </xs:complexType>
  </xs:element>
  <xs:element name="free" type="xs:anyType"/>
  <xs:simpleType name="code">
    <xs:restriction base="xs:string"><xs:pattern value="[A-Z]{2}"/></xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

/// Four orders: `qty` `03`, `10`, `9`, `3` — equal as integers in pairs (`03` and `3`), and in
/// another order as strings than as numbers. Indented, so `orders` and `order` have whitespace text
/// children in the text.
const ORDERS: &str = r#"<orders xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <order id="o1" price="10.50" code="AB">
    <qty>03</qty>
    <note>  </note>
    <when xsi:nil="true"/>
    <tag kind="k">  </tag>
  </order>
  <order id="o2" price="9.5">
    <qty>10</qty>
    <memo> some <b>bold</b> </memo>
  </order>
  <order id="o3" price="100">
    <qty>9</qty>
  </order>
  <order id="o4">
    <qty>3</qty>
  </order>
</orders>"#;

/// Not valid: `qty` is not an integer.
const INVALID_ORDERS: &str = r#"<orders><order id="o1"><qty>three</qty></order></orders>"#;

fn sheet(top: &str) -> String {
    sheet_with("", top)
}

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" exclude-result-prefixes=\"xs\" {attrs}>\
           {top}</xsl:stylesheet>"
    )
}

/// A stylesheet with one template for `/` whose body is `body`.
fn root_template(body: &str) -> String {
    sheet(&format!(
        "<xsl:template match=\"/\"><out>{body}</out></xsl:template>"
    ))
}

fn resolver(main: &str, files: &[(&str, &str)]) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(ORDERS_XSD_URI, ORDERS_XSD);
    for (uri, text) in files {
        mem.insert(*uri, *text);
    }
    Arc::new(mem)
}

/// The context of `main` plus the host schema `orders.xsd`, served by `resolver`.
fn context_for(resolver: &Arc<dyn Resolver>, functions: &XsltFunctions) -> SchemaContext {
    SchemaContext::builder()
        .with_resolver(Arc::clone(resolver))
        .stylesheet_uri(MAIN, functions)
        .expect("the pre-scan succeeds")
        .schema_uri(ORDERS_XSD_URI)
        .build()
        .expect("the context builds")
}

/// Schema-aware: compiles `main` against a context with the host schema, then runs `go` on the
/// transformation (options `options`) and serializes the primary result.
fn run_with(
    main: &str,
    files: &[(&str, &str)],
    options: TransformOptions,
    go: impl for<'a, 't> FnOnce(&xslt2::Transformation<'a, 't>) -> Result<xslt2::Outcome<'t>, XsltError>,
) -> Result<String, XsltError> {
    let resolver = resolver(main, files);
    let functions = XsltFunctions::new();
    let context = context_for(&resolver, &functions);
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)?;
    let arena = Bump::new();
    let run = compiled
        .transformation(&arena, options)
        .with_resolver(resolver);
    let outcome = go(&run)?;
    Ok(common::body(compiled.serialize_primary(&outcome)?))
}

/// The principal source `ORDERS` (or `source`) validated with `validation`.
fn run_over(main: &str, source: &str, validation: SourceValidation) -> Result<String, XsltError> {
    run_with(main, &[], TransformOptions::default(), |run| {
        run.transform_source_with(SOURCE, source, validation)
    })
}

fn out(result: Result<String, XsltError>) -> String {
    match result {
        Ok(text) => text,
        Err(e) => panic!("the run failed: {e}"),
    }
}

fn err(result: Result<String, XsltError>) -> XsltError {
    match result {
        Ok(text) => panic!("expected an error, got {text}"),
        Err(e) => e,
    }
}

// ── §2.8: a validated source is typed; an untyped one behaves as before (D-13) ──────────

/// §2.8: "where the source document has undergone schema validity assessment, the XSLT processor
/// has access to the type information associated with individual nodes, not merely to the untyped
/// text" — the typed values of `qty`, `@price`, `@code` (a user type of the host schema, §3.13).
#[test]
fn a_validated_source_atomizes_to_typed_values() {
    let main = root_template(
        r#"<xsl:for-each select="/orders/order[1]">
             <xsl:value-of select="data(qty) instance of xs:integer, qty = 3,
                                   data(@price) instance of xs:decimal,
                                   . instance of element(order),
                                   @code instance of attribute(code, xs:string),
                                   data(@code) instance of xs:untypedAtomic"/>
           </xsl:for-each>"#,
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out>true true true true true false</out>"
    );
}

/// D-4: `Skip` is the default, and an untyped source atomizes to `xs:untypedAtomic` exactly as on
/// a basic processor (§2.8: "A stylesheet that works with a basic XSLT processor will produce the
/// same results with a schema-aware XSLT processor provided that the source documents are
/// untyped").
#[test]
fn skip_is_the_default_and_leaves_the_source_untyped() {
    let main = root_template(
        r#"<xsl:value-of select="data(/orders/order[1]/qty) instance of xs:untypedAtomic,
                                 /orders/order[1]/qty = '03'"/>"#,
    );
    let via_default = run_with(&main, &[], TransformOptions::default(), |run| {
        run.transform_source(SOURCE, ORDERS)
    });
    assert_eq!(out(via_default), "<out>true true</out>");
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Skip)),
        "<out>true true</out>"
    );
}

/// D-4: the transformation-wide mode applies to every source-tree loader — `doc()` and
/// `document()` of a registered source and of one the resolver serves — and a per-call mode on
/// `register_source_with` overrides it.
#[test]
fn the_transformation_wide_mode_applies_to_doc_and_document() {
    let main = root_template(
        r#"<xsl:value-of select="data(doc('a.xml')/orders/order[1]/qty) instance of xs:integer,
                                 data(document('b.xml')/orders/order[1]/qty) instance of xs:integer,
                                 data(doc('c.xml')/orders/order[1]/qty) instance of xs:integer,
                                 data(doc('skip.xml')/orders/order[1]/qty) instance of xs:integer"/>"#,
    );
    let options = TransformOptions {
        source_validation: SourceValidation::Strict,
        ..Default::default()
    };
    let files = [("file:///m4b/c.xml", ORDERS)];
    let result = run_with(&main, &files, options, |run| {
        run.register_source("file:///m4b/a.xml", ORDERS);
        run.register_source("file:///m4b/b.xml", ORDERS);
        run.register_source_with("file:///m4b/skip.xml", ORDERS, SourceValidation::Skip);
        run.transform_source_with(SOURCE, "<doc/>", SourceValidation::Skip)
    });
    assert_eq!(out(result), "<out>true true true false</out>");
}

/// A per-call `Strict` on `register_source_with` holds under the default `Skip`.
#[test]
fn a_per_call_mode_overrides_the_default() {
    let main = root_template(
        r#"<xsl:value-of select="data((doc('a.xml')//qty)[1]) instance of xs:integer,
                                 data((doc('b.xml')//qty)[1]) instance of xs:integer"/>"#,
    );
    let result = run_with(&main, &[], TransformOptions::default(), |run| {
        run.register_source_with("file:///m4b/a.xml", ORDERS, SourceValidation::Strict);
        run.register_source("file:///m4b/b.xml", ORDERS);
        run.transform_source(SOURCE, "<doc/>")
    });
    assert_eq!(out(result), "<out>true false</out>");
}

/// F&O §15.5.4: `fn:doc` is stable — "Two calls on this function return the same document node if
/// the same URI Reference … is supplied to both calls" — a validated document too.
#[test]
fn a_validated_document_keeps_its_identity() {
    let main = root_template(r#"<xsl:value-of select="doc('a.xml') is doc('a.xml')"/>"#);
    let result = run_with(&main, &[], TransformOptions::default(), |run| {
        run.register_source_with("file:///m4b/a.xml", ORDERS, SourceValidation::Strict);
        run.transform_source(SOURCE, "<doc/>")
    });
    assert_eq!(out(result), "<out>true</out>");
}

// ── Invalid sources ─────────────────────────────────────────────────────────────────────

/// An invalid principal source is an error (D-4), reported with the code the loader reports a
/// source it cannot turn into a tree with (the initial source: XTDE1170, as for a parse error), and
/// naming the offending node (B8's instance path).
#[test]
fn an_invalid_principal_source_is_an_error_naming_the_node() {
    let main = root_template("<xsl:value-of select=\"count(//qty)\"/>");
    let e = err(run_over(&main, INVALID_ORDERS, SourceValidation::Strict));
    assert_eq!(e.code, codes::XTDE1170, "{e}");
    assert!(
        e.message.contains("/Q{}orders[1]/Q{}order[1]/Q{}qty[1]"),
        "{}",
        e.message
    );
    assert!(e.message.contains("not valid"), "{}", e.message);
    // The same text, not validated, runs.
    assert_eq!(
        out(run_over(&main, INVALID_ORDERS, SourceValidation::Skip)),
        "<out>1</out>"
    );
}

/// F&O 1.0 §15.5.4 (`fn:doc`), read at w3.org: "Implementations may provide user-defined error
/// handling options that allow processing to continue following an error in retrieving a resource,
/// or in parsing and validating its content" — an invalid document is an error of `fn:doc`, raised
/// with its code for a resource that cannot be parsed, FODC0002; `document()` passes the URI to
/// `fn:doc` (§16.1).
#[test]
fn an_invalid_document_read_by_doc_or_document_is_fodc0002() {
    for call in ["doc('bad.xml')", "document('bad.xml')"] {
        let main = root_template(&format!("<xsl:value-of select=\"count({call}//qty)\"/>"));
        let result = run_with(&main, &[], TransformOptions::default(), |run| {
            run.register_source_with(
                "file:///m4b/bad.xml",
                INVALID_ORDERS,
                SourceValidation::Strict,
            );
            run.transform_source(SOURCE, "<doc/>")
        });
        let e = err(result);
        assert_eq!(e.code, "FODC0002", "{call}: {e}");
    }
}

/// `Strict`: an element with no declaration (and no `xsi:type`) is not valid (XML Schema 1.1 Part 1
/// §3.3.4.3, cvc-elt.1), so the document is an error.
#[test]
fn strict_validation_of_an_undeclared_root_is_an_error() {
    let main = root_template("<xsl:value-of select=\"name(/*)\"/>");
    let e = err(run_over(&main, "<unknown/>", SourceValidation::Strict));
    assert_eq!(e.code, codes::XTDE1170, "{e}");
    assert!(e.message.contains("cvc-elt.1"), "{}", e.message);
}

/// `Lax` with a declared root validates as `Strict` does (the root is assessed against its
/// declaration). With an undeclared root it is XML Schema 1.1 §5.2's lax wildcard validation
/// (upstream C36, `ValidationRoot::Lax`; it was M4f's fence until M4f): "If they do not identify any
/// declaration or definition, then ·lax assessment· is performed" — no error for the root, its
/// declared descendants validated and typed (`order`), an invalid one an error.
#[test]
fn lax_validation_types_a_declared_root_and_laxly_assesses_an_undeclared_one() {
    let main =
        root_template("<xsl:value-of select=\"data(//order[1]/qty) instance of xs:integer\"/>");
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Lax)),
        "<out>true</out>"
    );
    assert_eq!(
        out(run_over(
            &main,
            "<unknown><order><qty> 7 </qty></order></unknown>",
            SourceValidation::Lax,
        )),
        "<out>true</out>"
    );
    // An invalid document is an error under `Lax` too, with a declared root or below an undeclared
    // one.
    let e = err(run_over(&main, INVALID_ORDERS, SourceValidation::Lax));
    assert_eq!(e.code, codes::XTDE1170, "{e}");
    let e = err(run_over(
        &main,
        "<unknown><order><qty>seven</qty></order></unknown>",
        SourceValidation::Lax,
    ));
    assert_eq!(e.code, codes::XTDE1170, "{e}");
}

/// §21.1: a basic processor has no schema to validate against, and "must raise a non-recoverable
/// dynamic error if the input to the processor includes a node with a type annotation other than
/// xs:untyped or xs:untypedAtomic" — asking it to validate a source is XTDE1665; `Skip` runs.
#[test]
fn a_basic_processor_refuses_to_validate_a_source() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &root_template("<xsl:value-of select=\"count(//qty)\"/>"),
            MAIN,
        )
        .unwrap();
    for validation in [SourceValidation::Strict, SourceValidation::Lax] {
        let arena = Bump::new();
        let e = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source_with(SOURCE, ORDERS, validation)
            .expect_err("refused");
        assert_eq!(e.code, codes::XTDE1665, "{e}");
    }
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source_with(SOURCE, ORDERS, SourceValidation::Skip)
        .unwrap();
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>4</out>"
    );
}

// ── XDM 1.0 §6.7.4 and §4.4: whitespace in a typed tree ─────────────────────────────────

/// XDM 1.0 §6.7.4 (Construction from a PSVI, read at w3.org): "if the resulting Text Node consists
/// entirely of whitespace and the character information items used to construct this node have a
/// parent and that parent is an element and its {content type} is not 'mixed', then the content of
/// the Text Node is the zero-length string … an empty Text Node will be discarded" — the
/// indentation inside `orders` and `order` (element-only content) is not in the typed tree, with
/// no `xsl:strip-space`; mixed content (`memo`) and simple content (`note`, `tag`) keep theirs.
#[test]
fn whitespace_in_element_only_content_is_not_part_of_a_typed_tree() {
    let main = root_template(
        r#"<xsl:value-of select="count(/orders/node()), count(/orders/order[1]/node()),
                                 count(/orders/order[2]/memo/text()),
                                 string-length(/orders/order[1]/note),
                                 string-length(/orders/order[1]/tag)"/>"#,
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out>4 4 2 2 2</out>"
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Skip)),
        "<out>9 9 2 2 2</out>"
    );
}

/// §4.4: "If an element in a source document has a type annotation that is a simple type or a
/// complex type with simple content, then any whitespace text nodes among its children are
/// preserved, regardless of any xsl:strip-space declarations" — `note` (`xs:string`) and `tag`
/// (simple content) keep their two spaces under `xsl:strip-space elements="*"`; untyped, they lose
/// them.
#[test]
fn strip_space_spares_elements_with_simple_content() {
    let main = sheet(
        r#"<xsl:strip-space elements="*"/>
           <xsl:template match="/"><out><xsl:value-of select="
               string-length(/orders/order[1]/note), string-length(/orders/order[1]/tag),
               count(/orders/order[2]/memo/text())"/></out></xsl:template>"#,
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out>2 2 1</out>"
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Skip)),
        "<out>0 0 1</out>"
    );
}

// ── §4.3 input-type-annotations ─────────────────────────────────────────────────────────

const STRIP_PROBE: &str = r#"<xsl:template match="/"><out><xsl:for-each select="/orders/order[1]">
   <xsl:value-of select=". instance of element(*, xs:untyped),
                         @price instance of attribute(*, xs:untypedAtomic),
                         data(qty) instance of xs:untypedAtomic,
                         nilled(when),
                         exists(id('o2')),
                         count(/orders/node())"/>
 </xsl:for-each></out></xsl:template>"#;

/// §4.3: "Stripping of type annotations takes place if at least one stylesheet module in the
/// stylesheet specifies input-type-annotations="strip"" — every element becomes `xs:untyped`, every
/// attribute `xs:untypedAtomic`, "The typed value of every element and attribute node is set to be
/// the same as its string value, as an instance of xs:untypedAtomic", "The is-nilled property of
/// every element node is set to false", and "The values of the is-id and is-idrefs properties are
/// not changed" (`id('o2')` still finds the order whose `xs:ID` attribute says `o2`). The
/// whitespace of element-only content, gone when the typed tree was built, stays gone (the Note:
/// "Stripping type annotations does not necessarily return the document to the state it would be in
/// had validation not taken place").
#[test]
fn input_type_annotations_strip_makes_a_validated_source_untyped() {
    let main = sheet_with(r#"input-type-annotations="strip""#, STRIP_PROBE);
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out>true true true false true 4</out>"
    );
}

/// `preserve` and `unspecified` (the default) keep the annotations.
#[test]
fn input_type_annotations_preserve_and_unspecified_keep_them() {
    for attrs in [
        r#"input-type-annotations="preserve""#,
        r#"input-type-annotations="unspecified""#,
        "",
    ] {
        let main = sheet_with(attrs, STRIP_PROBE);
        assert_eq!(
            out(run_over(&main, ORDERS, SourceValidation::Strict)),
            "<out>false false false true true 4</out>",
            "{attrs}"
        );
    }
}

/// §4.3: "at least one stylesheet module" — `strip` in an imported module strips the source of the
/// whole transformation, and `doc()` results too ("The source trees to which this applies are the
/// same as those affected by xsl:strip-space and xsl:preserve-space").
#[test]
fn strip_in_an_imported_module_applies_to_every_source_tree() {
    let main = sheet(
        r#"<xsl:import href="strip.xsl"/>
           <xsl:template match="/"><out><xsl:value-of select="
               data(/orders/order[1]/qty) instance of xs:untypedAtomic,
               data(doc('a.xml')/orders/order[1]/qty) instance of xs:untypedAtomic"/></out>
           </xsl:template>"#,
    );
    let strip = sheet_with(r#"input-type-annotations="strip""#, "");
    let files = [("file:///m4b/strip.xsl", strip.as_str())];
    let result = run_with(&main, &files, TransformOptions::default(), |run| {
        run.register_source_with("file:///m4b/a.xml", ORDERS, SourceValidation::Strict);
        run.transform_source_with(SOURCE, ORDERS, SourceValidation::Strict)
    });
    assert_eq!(out(result), "<out>true true</out>");
}

/// §4.4: "Stripping of type annotations happens before stripping of whitespace text nodes, so this
/// situation will not occur if input-type-annotations="strip" is specified" — with both, `note` and
/// `tag` lose their whitespace like untyped elements.
#[test]
fn annotations_are_stripped_before_whitespace() {
    let main = sheet_with(
        r#"input-type-annotations="strip""#,
        r#"<xsl:strip-space elements="*"/>
           <xsl:template match="/"><out><xsl:value-of select="
               string-length(/orders/order[1]/note), string-length(/orders/order[1]/tag)"/></out>
           </xsl:template>"#,
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out>0 0</out>"
    );
}

// ── F10: typed keys, sort keys and grouping keys (atomization only) ──────────────────────

const F10_SHEET: &str = r#"<xsl:key name="k" match="order" use="qty"/>
  <xsl:template match="/"><out>
    <key><xsl:value-of select="key('k', 3)/@id, '|', key('k', '3')/@id"/></key>
    <sort><xsl:for-each select="/orders/order"><xsl:sort select="qty"/><xsl:value-of select="@id"/></xsl:for-each></sort>
    <group><xsl:for-each-group select="/orders/order" group-by="qty"><xsl:value-of select="count(current-group())"/></xsl:for-each-group></group>
  </out></xsl:template>"#;

/// §16.3.2 (`key`), §13.1.2 (sort keys), §14.3 (`group-by`): the values are atomized, so a typed
/// `qty` gives integers — `key('k', 3)` finds `03` and `3`, the sort is numeric (3, 3, 9, 10), and
/// `03` and `3` form one group; untyped, the same stylesheet compares strings (`key('k', '3')`
/// finds only `3`, `"03" < "10" < "3" < "9"`, four groups) exactly as a basic processor does (D-13).
#[test]
fn typed_keys_sort_keys_and_grouping_keys_see_typed_values() {
    let main = sheet(F10_SHEET);
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Strict)),
        "<out><key>o1 o4 |</key><sort>o1o4o3o2</sort><group>211</group></out>"
    );
    assert_eq!(
        out(run_over(&main, ORDERS, SourceValidation::Skip)),
        "<out><key>| o4</key><sort>o1o2o4o3</sort><group>1111</group></out>"
    );
}

// ── §3.13: host schemas in the context ──────────────────────────────────────────────────

/// §3.13's "implementation-defined mechanism": the host schema's components are in-scope schema
/// components (D-2, D-7) — a type name of it in `instance of` compiles without any
/// `xsl:import-schema` — and the context lists the host schemas it loaded.
#[test]
fn host_schemas_are_in_scope_for_the_stylesheet() {
    let main =
        root_template("<xsl:value-of select=\"'AB' castable as code, 'ab' castable as code\"/>");
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = context_for(&resolver, &functions);
    assert_eq!(context.host_schemas(), [ORDERS_XSD_URI.to_string()]);
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source(SOURCE, "<doc/>")
        .unwrap();
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>true false</out>"
    );
}

/// §3.14: "The namespace attribute indicates that a schema for the given namespace is required by
/// the stylesheet. This information may be enough on its own to enable an implementation to locate
/// the required schema components" — a host schema for the namespace is found; and a location
/// import of the document the host already gave loads it once (no duplicate components).
#[test]
fn imports_are_satisfied_by_host_schemas() {
    const HATS: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:hats">
        <xs:simpleType name="size"><xs:restriction base="xs:integer"><xs:maxInclusive value="10"/></xs:restriction></xs:simpleType>
      </xs:schema>"#;
    for import in [
        r#"<xsl:import-schema namespace="urn:hats"/>"#,
        r#"<xsl:import-schema namespace="urn:hats" schema-location="hats.xsd"/>"#,
    ] {
        let main = sheet(&format!(
            r#"{import}<xsl:template match="/"><out><xsl:value-of xmlns:h="urn:hats" select="11 castable as h:size"/></out></xsl:template>"#
        ));
        let resolver = resolver(&main, &[("file:///m4b/hats.xsd", HATS)]);
        let functions = XsltFunctions::new();
        let context = SchemaContext::builder()
            .with_resolver(Arc::clone(&resolver))
            .schema_uri("file:///m4b/hats.xsd")
            .stylesheet_uri(MAIN, &functions)
            .unwrap()
            .build()
            .unwrap_or_else(|e| panic!("{import}: {e}"));
        assert!(context.imports()[0].found(), "{import}");
        let compiled = Compiler::schema_aware(&context, &functions)
            .with_resolver(Arc::clone(&resolver))
            .compile_uri(MAIN)
            .unwrap();
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source(SOURCE, "<doc/>")
            .unwrap();
        assert_eq!(
            common::body(compiled.serialize_primary(&outcome).unwrap()),
            "<out>false</out>",
            "{import}"
        );
        if import.contains("schema-location") {
            assert_eq!(
                context.imports()[0].source(),
                &ImportSource::Location("file:///m4b/hats.xsd".to_string())
            );
        }
    }
}

/// A host schema given as text (`schema_str`), and one that is not a valid schema or cannot be read:
/// the in-scope schema components cannot be assembled, XTSE0220 (§3.14's code for a synthetic schema
/// document that "does not satisfy the constraints described in \[XML Schema Part 1\]").
#[test]
fn host_schemas_from_text_and_their_errors() {
    let main = root_template("<xsl:value-of select=\"'AB' castable as code\"/>");
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .schema_str(ORDERS_XSD, "urn:host-orders")
        .stylesheet_uri(MAIN, &functions)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(context.host_schemas(), ["urn:host-orders".to_string()]);
    let bad = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .schema_str("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"a\" type=\"nope\"/><xs:bogus/></xs:schema>", "urn:bad")
        .build()
        .expect_err("not a valid schema");
    assert_eq!(bad.code, codes::XTSE0220, "{bad}");
    let missing = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .schema_uri("file:///m4b/missing.xsd")
        .build()
        .expect_err("cannot be read");
    assert_eq!(missing.code, codes::XTSE0220, "{missing}");
    assert!(
        missing.message.contains("missing.xsd"),
        "{}",
        missing.message
    );
}

// ── Typed trees a host builds itself (D-2, D-13) ────────────────────────────────────────

/// D-13: a document typed against the context's own schema set and handed to the store by the host
/// is accepted as it is (a schema-aware processor has no XTDE1665, §21.2).
#[test]
fn a_host_built_tree_of_the_contexts_set_is_accepted() {
    let main = root_template(
        "<xsl:value-of select=\"data(/orders/order[1]/qty) instance of xs:integer\"/>",
    );
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = context_for(&resolver, &functions);
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let doc = build_typed_document(
        ORDERS.as_bytes(),
        &arena,
        context.schema_set(),
        BufferDocumentOptions::default(),
    )
    .unwrap();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run.store().borrow_mut().insert(doc, Some(SOURCE));
    let outcome = run.transform_document(id).unwrap();
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>true</out>"
    );
    assert!(run.take_warnings().is_empty());
}

/// D-2: "A document typed against another set (only possible through `DocumentStore::insert`) is
/// stripped with a warning" — its annotations name components of a set the stylesheet does not
/// know, so the run sees it untyped (§4.3's stripping) and says so.
#[test]
fn a_host_built_tree_of_another_set_is_stripped_with_a_warning() {
    let other = SchemaSetBuilder::new()
        .add_source(ORDERS_XSD, "file:///elsewhere/orders.xsd")
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let main = root_template(
        "<xsl:value-of select=\"data(/orders/order[1]/qty) instance of xs:untypedAtomic, id('o2')/@id\"/>",
    );
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = context_for(&resolver, &functions);
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let doc = build_typed_document(
        ORDERS.as_bytes(),
        &arena,
        &other,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run.store().borrow_mut().insert(doc, Some(SOURCE));
    let outcome = run.transform_document(id).unwrap();
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>true o2</out>"
    );
    let warnings = run.take_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("another schema set"),
        "{}",
        warnings[0]
    );
}

/// §21.1, XTDE1665: "This error will not arise if the input-type-annotations attribute is set to
/// strip" — a basic processor given a typed source tree strips it instead of refusing it; without
/// `strip` it refuses it, as before.
#[test]
fn a_basic_processor_strips_a_typed_source_under_input_type_annotations_strip() {
    let set = SchemaSetBuilder::new()
        .add_source(ORDERS_XSD, "file:///elsewhere/orders.xsd")
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let probe =
        "<xsl:value-of select=\"data(/orders/order[1]/qty) instance of xs:untypedAtomic\"/>";
    for (attrs, expected) in [
        (r#"input-type-annotations="strip""#, Ok("<out>true</out>")),
        ("", Err(codes::XTDE1665)),
    ] {
        let compiled = Compiler::new(&names, &functions)
            .compile_str(
                &sheet_with(
                    attrs,
                    &format!("<xsl:template match=\"/\"><out>{probe}</out></xsl:template>"),
                ),
                MAIN,
            )
            .unwrap();
        let arena = Bump::new();
        let doc = build_typed_document(
            ORDERS.as_bytes(),
            &arena,
            &set,
            BufferDocumentOptions::default(),
        )
        .unwrap();
        let run = compiled.transformation(&arena, TransformOptions::default());
        let id = run.store().borrow_mut().insert(doc, Some(SOURCE));
        let result = run
            .transform_document(id)
            .map(|outcome| common::body(compiled.serialize_primary(&outcome).unwrap()));
        match expected {
            Ok(text) => assert_eq!(result.unwrap(), text, "{attrs}"),
            Err(code) => assert_eq!(result.unwrap_err().code, code, "{attrs}"),
        }
    }
}

/// A stylesheet parameter holding a node of a tree typed against another schema set cannot be
/// stripped in place (§4.4: parameters are not source trees, and a copy would change the node's
/// identity): it is refused rather than processed with annotations the stylesheet cannot read.
#[test]
fn a_parameter_typed_against_another_set_is_refused() {
    let other = SchemaSetBuilder::new()
        .add_source(ORDERS_XSD, "file:///elsewhere/orders.xsd")
        .unwrap()
        .compile()
        .unwrap()
        .into_schema_set();
    let main = sheet(
        r#"<xsl:param name="p"/><xsl:template match="/"><out><xsl:value-of select="count($p//qty)"/></out></xsl:template>"#,
    );
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = context_for(&resolver, &functions);
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)
        .unwrap();
    let arena = Bump::new();
    let doc = build_typed_document(
        ORDERS.as_bytes(),
        &arena,
        &other,
        BufferDocumentOptions::default(),
    )
    .unwrap();
    let doc: &xsd_schema::document::BufferDocument<'_> = arena.alloc(doc);
    let value = xsd_schema::xpath::functions::XPathValue::from_node(doc.create_navigator());
    let run = compiled
        .transformation(&arena, TransformOptions::default())
        .with_param(xslt2::QName::local("p"), value);
    let e = run.transform_source(SOURCE, "<doc/>").expect_err("refused");
    assert_eq!(e.code, xslt2::error::codes_internal::INTERNAL, "{e}");
    assert!(e.message.contains("another schema set"), "{}", e.message);
}
