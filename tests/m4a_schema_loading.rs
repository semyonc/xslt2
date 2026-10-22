//! M4a — schema loading and the schema-aware mode (`xslt_doc/M4_PLAN.md` §4 M4a, decisions D-1 … D-13).
//!
//! A schema-aware compilation is two calls (D-1): a caller-owned [`SchemaContext`], built before the
//! stylesheet compiles from a pre-scan of its `xsl:import-schema` declarations, and
//! `Compiler::schema_aware(&context, &functions)`, which borrows the context's schema set and its name
//! table. A basic compilation stays `Compiler::new(&names, &functions)` (D-6).
//!
//! Every test goes through the public API and cites the rule it checks (`xslt20.html`): §3.13
//! Built-in Types, §3.14 Importing Schema Components, §3.12 (the `use-when` static context), §16.6.5
//! `system-property`, §18.1.4 `type-available`, §19.2 (the static checks of `[xsl:]type` /
//! `[xsl:]validation`), §21.1 (the basic processor's restrictions).

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
use xsd_schema::namespace::NameTable;

use xslt2::error::codes;
use xslt2::{
    Compiler, ImportSource, MemoryResolver, Resolver, SchemaContext, TransformOptions, XsltError,
    XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///m4a/main.xsl";
const SOURCE: &str = "file:///m4a/in.xml";
const HATS: &str = "file:///m4a/hats.xsd";

/// A schema for `urn:hats` with simple types (§3.14's "top-level type definitions"), a complex type,
/// an element and an attribute declaration; it includes `parts/sizes.xsd` by a relative location.
const HATS_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    targetNamespace="urn:hats" xmlns:h="urn:hats" elementFormDefault="qualified">
  <xs:include schemaLocation="parts/sizes.xsd"/>
  <xs:simpleType name="hatsize">
    <xs:restriction base="xs:integer">
      <xs:minInclusive value="4"/>
      <xs:maxInclusive value="10"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="vreg">
    <xs:restriction base="xs:string">
      <xs:pattern value="[a-z]{3}[0-9]{3}"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:complexType name="hatType">
    <xs:sequence><xs:element name="size" type="h:hatsize"/></xs:sequence>
  </xs:complexType>
  <xs:element name="hat" type="h:hatType"/>
  <xs:attribute name="brand" type="xs:string"/>
</xs:schema>"#;

const SIZES: &str = "file:///m4a/parts/sizes.xsd";
const SIZES_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
    targetNamespace="urn:hats">
  <xs:simpleType name="bigsize">
    <xs:restriction base="xs:integer"><xs:minInclusive value="100"/></xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

/// The files every schema-aware test can reach.
fn hat_files() -> Vec<(&'static str, &'static str)> {
    vec![(HATS, HATS_XSD), (SIZES, SIZES_XSD)]
}

/// A stylesheet module with the usual namespace declarations; `top` is its top-level content.
fn sheet(top: &str) -> String {
    sheet_with("", top)
}

/// The same, with extra attributes on `xsl:stylesheet`.
fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:h=\"urn:hats\" \
           exclude-result-prefixes=\"xs h\" {attrs}>{top}</xsl:stylesheet>"
    )
}

/// The `xsl:import-schema` of `urn:hats` by location.
const IMPORT_HATS: &str = r#"<xsl:import-schema namespace="urn:hats" schema-location="hats.xsd"/>"#;

/// Which step a run stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Building the schema context (the pre-scan and the schema set).
    Context,
    /// `Compiler::compile_*`.
    Compile,
    /// The transformation or its serialization.
    Run,
}

type Outcome = Result<String, (Stage, XsltError)>;

fn resolver(main: &str, files: &[(&str, &str)]) -> Arc<dyn Resolver> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    for (uri, text) in files {
        mem.insert(*uri, *text);
    }
    Arc::new(mem)
}

/// Schema-aware: builds the context from `main`, compiles `main` against it and runs it over `<doc/>`;
/// returns the serialized primary result without the default XML declaration.
fn run_sa(main: &str, files: &[(&str, &str)]) -> Outcome {
    run_sa_over(main, files, "<doc/>")
}

fn run_sa_over(main: &str, files: &[(&str, &str)], source: &str) -> Outcome {
    let resolver = resolver(main, files);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .and_then(|builder| builder.build())
        .map_err(|e| (Stage::Context, e))?;
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
        .map_err(|e| (Stage::Compile, e))?;
    assert!(compiled.is_schema_aware());
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source(SOURCE, source)
        .map_err(|e| (Stage::Run, e))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| (Stage::Run, e))?;
    Ok(common::body(text))
}

/// Basic: `Compiler::new`, as every test before M4a.
fn run_basic(main: &str, files: &[(&str, &str)]) -> Outcome {
    let resolver = resolver(main, files);
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
        .map_err(|e| (Stage::Compile, e))?;
    assert!(!compiled.is_schema_aware());
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source(SOURCE, "<doc/>")
        .map_err(|e| (Stage::Run, e))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| (Stage::Run, e))?;
    Ok(common::body(text))
}

/// The code (and the step) a run stopped with.
fn failure(outcome: Outcome) -> (Stage, String) {
    match outcome {
        Ok(text) => panic!("expected an error, got the result {text:?}"),
        Err((stage, e)) => (stage, e.code.clone()),
    }
}

/// The milestone a `XSLT2-NOTIMPL` names (the trailing `(…)` of its message).
fn fenced(outcome: Outcome) -> (Stage, String) {
    match outcome {
        Ok(text) => panic!("expected XSLT2-NOTIMPL, got the result {text:?}"),
        Err((stage, e)) => {
            assert!(e.is_not_implemented(), "expected XSLT2-NOTIMPL, got {e}");
            let message = e.message.clone();
            let open = message.rfind('(').expect("a milestone in parentheses");
            (stage, message[open + 1..message.len() - 1].to_string())
        }
    }
}

/// A template matching `/` whose body is `body`.
fn root_template(body: &str) -> String {
    format!(r#"<xsl:template match="/">{body}</xsl:template>"#)
}

// ── §21.1: the basic processor ──────────────────────────────────────────

/// > \[ERR XTSE1650\] A basic XSLT processor must signal a static error if the stylesheet includes an
/// > `xsl:import-schema` declaration.
///
/// Every form of the declaration — by location, by namespace only, inline, and empty.
#[test]
fn a_basic_processor_rejects_every_import_schema_with_xtse1650() {
    for import in [
        IMPORT_HATS,
        r#"<xsl:import-schema namespace="urn:hats"/>"#,
        r#"<xsl:import-schema/>"#,
        r#"<xsl:import-schema><xs:schema targetNamespace="urn:i"/></xsl:import-schema>"#,
    ] {
        let main = sheet(&format!("{import}{}", root_template("<out/>")));
        assert_eq!(
            failure(run_basic(&main, &hat_files())),
            (Stage::Compile, codes::XTSE1650.to_string()),
            "{import}"
        );
    }
}

/// §3.12: an element that `use-when` excludes is as if it were not there, so an excluded
/// `xsl:import-schema` is no XTSE1650 — the usual way to write a stylesheet for both kinds of
/// processor.
#[test]
fn an_import_schema_excluded_by_use_when_is_no_error_for_a_basic_processor() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats" schema-location="hats.xsd"
              use-when="system-property('xsl:is-schema-aware') = 'yes'"/>
           {}"#,
        root_template(
            "<out><xsl:value-of select=\"system-property('xsl:is-schema-aware')\"/></out>"
        )
    ));
    assert_eq!(run_basic(&main, &hat_files()).unwrap(), "<out>no</out>");
    // The same stylesheet, schema-aware: the declaration stays and is loaded.
    assert_eq!(run_sa(&main, &hat_files()).unwrap(), "<out>yes</out>");
}

/// §21.1: "The static context … includes these atomic types" — the §3.13 list — so a basic processor
/// answers `type-available('xs:short')` with false (unchanged by M4a).
#[test]
fn a_basic_processor_keeps_its_type_set_and_says_it_is_not_schema_aware() {
    let main = sheet(&root_template(
        "<out a=\"{system-property('xsl:is-schema-aware')}\" b=\"{type-available('xs:integer')}\" \
              c=\"{type-available('xs:short')}\" d=\"{function-available('xs:short')}\"/>",
    ));
    assert_eq!(
        run_basic(&main, &[]).unwrap(),
        r#"<out a="no" b="true" c="false" d="false"/>"#
    );
}

/// §21.1's XTSE1660 is about a value **other than** `strip`, so `xsl:validation="strip"` on a literal
/// result element and `validation="strip"` on an instruction are accepted by a basic processor — and
/// run (before M4a the literal result element's spelling was a NOT-IMPL).
#[test]
fn a_basic_processor_runs_validation_strip() {
    let main = sheet(&root_template(
        r#"<out xsl:validation="strip"><xsl:element name="e" validation="strip"/><xsl:copy-of select="/" validation="strip"/></out>"#,
    ));
    assert_eq!(run_basic(&main, &[]).unwrap(), "<out><e/><doc/></out>");
}

/// > \[ERR XTSE1660\] A basic XSLT processor must signal a static error if the stylesheet includes an
/// > `[xsl:]type` attribute, or an `[xsl:]validation` or `default-validation` attribute with a value
/// > other than `strip`.
#[test]
fn a_basic_processor_still_rejects_type_and_non_strip_validation_with_xtse1660() {
    for body in [
        r#"<xsl:element name="e" type="xs:integer"/>"#,
        r#"<out xsl:validation="strict"/>"#,
        r#"<xsl:copy-of select="." validation="preserve"/>"#,
    ] {
        let main = sheet(&root_template(body));
        assert_eq!(
            failure(run_basic(&main, &[])),
            (Stage::Compile, codes::XTSE1660.to_string()),
            "{body}"
        );
    }
    let main = sheet_with(r#"default-validation="preserve""#, &root_template("<out/>"));
    assert_eq!(
        failure(run_basic(&main, &[])),
        (Stage::Compile, codes::XTSE1660.to_string())
    );
}

// ── §16.6.5, §18.1.4, §3.13: the schema-aware processor without a schema ─

/// §16.6.5: `xsl:is-schema-aware` "returns the string "yes" in the case of a processor that claims
/// conformance as a schema-aware XSLT processor" — in an expression and in a `use-when` expression
/// alike.
#[test]
fn a_schema_aware_processor_says_so_in_expressions_and_in_use_when() {
    let main = sheet(
        r#"<xsl:template match="/" use-when="system-property('xsl:is-schema-aware') = 'no'"><wrong/></xsl:template>
           <xsl:template match="/" use-when="system-property('xsl:is-schema-aware') = 'yes'">
             <out><xsl:value-of select="system-property('xsl:is-schema-aware')"/></out>
           </xsl:template>"#,
    );
    assert_eq!(run_sa(&main, &[]).unwrap(), "<out>yes</out>");
}

/// §3.13: "A schema-aware XSLT processor additionally supports: All other built-in types defined in
/// \[XML Schema Part 2\]" — `type-available-0147`'s list: every XSD built-in type (`xs:NOTATION`,
/// `xs:ID`, the list types included), the §3.13 ones, and nothing that is not a type.
#[test]
fn a_schema_aware_processor_has_every_built_in_type() {
    let names = [
        ("xs:integer", true),
        ("xs:string", true),
        ("xs:dayTimeDuration", true),
        ("xs:untypedAtomic", true),
        ("xs:anyType", true),
        ("xs:untyped", true),
        ("xs:anyAtomicType", true),
        ("xs:int", true),
        ("xs:short", true),
        ("xs:NCName", true),
        ("xs:ID", true),
        ("xs:NOTATION", true),
        ("xs:ENTITIES", true),
        ("xs:IDREFS", true),
        ("xs:NMTOKENS", true),
        ("xs:unknownType", false),
        ("unknownType", false),
        ("xsl:unknownType", false),
    ];
    let body: String = names
        .iter()
        .map(|(name, _)| {
            format!("<t n=\"{name}\"><xsl:value-of select=\"type-available('{name}')\"/></t>")
        })
        .collect();
    let expected: String = names
        .iter()
        .map(|(name, available)| format!("<t n=\"{name}\">{available}</t>"))
        .collect();
    let main = sheet(&root_template(&format!("<out>{body}</out>")));
    assert_eq!(
        run_sa(&main, &[]).unwrap(),
        format!("<out>{expected}</out>")
    );
}

/// XPath 2.0 §3.10.4: "For every atomic type in the in-scope schema types (except `xs:NOTATION` and
/// `xs:anyAtomicType` …) a constructor function is implicitly defined", and §18.1.1's
/// `function-available` answers for the in-scope functions — so in schema-aware mode the constructor
/// of every built-in atomic type is available (§21.1 limits only a basic processor's set).
#[test]
fn a_schema_aware_processor_has_the_constructor_of_every_built_in_atomic_type() {
    let main = sheet(&root_template(
        "<out a=\"{function-available('xs:short')}\" b=\"{function-available('xs:NCName', 1)}\" \
              c=\"{function-available('xs:NOTATION')}\" d=\"{function-available('xs:anyType')}\" \
              e=\"{xs:short('7') instance of xs:short}\"/>",
    ));
    assert_eq!(
        run_sa(&main, &[]).unwrap(),
        r#"<out a="true" b="true" c="false" d="false" e="true"/>"#
    );
}

/// §3.12's static context for `use-when`: "In scope type definitions: The type definitions that would
/// be available in the absence of any `xsl:import-schema` declaration" — the built-in ones, never an
/// imported one — while §18.1.4 answers for the imported types everywhere else.
#[test]
fn use_when_sees_the_built_in_types_but_not_the_imported_ones() {
    let main = sheet(&format!(
        r#"{IMPORT_HATS}
           <xsl:template match="/">
             <out>
               <a xsl:use-when="type-available('xs:short')"/>
               <b xsl:use-when="type-available('h:hatsize')"/>
               <c><xsl:value-of select="type-available('h:hatsize')"/></c>
             </out>
           </xsl:template>"#
    ));
    assert_eq!(
        run_sa(&main, &hat_files()).unwrap(),
        "<out><a/><c>true</c></out>"
    );
}

// ── §3.14: imports by location ──────────────────────────────────────────

/// §3.14: "the names of top-level element and attribute declarations and type definitions from this
/// namespace … become available for use within XPath expressions in the stylesheet" — `castable as`,
/// `cast as`, `instance of` and the constructor function of a user-defined atomic type (XPath 2.0
/// §3.10.2–§3.10.4; upstream C37), and §18.1.4 for both simple and complex types.
#[test]
fn a_location_import_puts_the_user_types_in_scope() {
    let main = sheet(&format!(
        r#"{IMPORT_HATS}
           <xsl:template match="/">
             <out>
               <a><xsl:value-of select="(4 castable as h:hatsize, 11 castable as h:hatsize)"/></a>
               <b><xsl:value-of select="'abc123' castable as h:vreg, 'ABC123' castable as h:vreg"/></b>
               <c><xsl:value-of select="('7' cast as h:hatsize) instance of h:hatsize"/></c>
               <d><xsl:value-of select="h:hatsize('8') instance of xs:integer, h:hatsize('8') + 1"/></d>
               <e><xsl:value-of select="for $t in ('h:hatsize', 'h:hatType', 'h:bigsize', 'h:nothing') return type-available($t)"/></e>
             </out>
           </xsl:template>"#
    ));
    assert_eq!(
        run_sa(&main, &hat_files()).unwrap(),
        "<out><a>true false</a><b>true false</b><c>true</c><d>true 9</d><e>true true true false</e></out>"
    );
}

/// §9.3 / XPath 2.0 §3.1.5 with a user-defined atomic type in `as`: an `xs:untypedAtomic` value is cast
/// to it; one outside its facets is the type error of the construct (XTTE0570), not a silent pass.
#[test]
fn the_as_attribute_converts_to_a_user_atomic_type() {
    let main = sheet(&format!(
        r#"{IMPORT_HATS}
           <xsl:variable name="ok" as="h:hatsize"><v>7</v></xsl:variable>
           <xsl:template match="/">
             <out><xsl:value-of select="$ok instance of h:hatsize, $ok instance of xs:untypedAtomic"/></out>
           </xsl:template>"#
    ));
    assert_eq!(
        run_sa(&main, &hat_files()).unwrap(),
        "<out>true false</out>"
    );

    let main = sheet(&format!(
        r#"{IMPORT_HATS}
           <xsl:variable name="bad" as="h:hatsize"><v>11</v></xsl:variable>
           <xsl:template match="/"><out><xsl:value-of select="$bad"/></out></xsl:template>"#
    ));
    assert_eq!(
        failure(run_sa(&main, &hat_files())),
        (Stage::Run, codes::XTTE0570.to_string())
    );
}

/// §3.14: "The base URI of the `xs:import` element is the same as the base URI of the
/// `xsl:import-schema` declaration" — so `xml:base` on the declaration moves where `schema-location`
/// resolves; the schema's own `xs:include` resolves against the schema document.
#[test]
fn schema_location_resolves_against_the_base_uri_of_the_declaration() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats" schema-location="hats.xsd" xml:base="sub/"/>
           {}"#,
        root_template("<out><xsl:value-of select=\"type-available('h:hatsize'), type-available('h:bigsize')\"/></out>")
    ));
    let files = [
        ("file:///m4a/sub/hats.xsd", HATS_XSD),
        ("file:///m4a/sub/parts/sizes.xsd", SIZES_XSD),
    ];
    assert_eq!(run_sa(&main, &files).unwrap(), "<out>true true</out>");
}

/// §3.14: "it is not intrinsically an error if no schema document can be located for a namespace
/// identified in an `xsl:import-schema` declaration. This will cause an error only if it results in
/// the stylesheet containing references to names that have not been imported."
#[test]
fn a_schema_document_that_cannot_be_found_is_no_error_in_itself() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats" schema-location="nowhere.xsd"/>
           <xsl:import-schema namespace="urn:nonexistent"/>
           {}"#,
        root_template("<out><xsl:value-of select=\"type-available('h:hatsize')\"/></out>")
    ));
    assert_eq!(run_sa(&main, &[]).unwrap(), "<out>false</out>");

    // A reference to a name that was not imported is the reference's own error: XPath's XPST0051
    // for an unknown atomic type in `cast as`.
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats" schema-location="nowhere.xsd"/>
           {}"#,
        root_template("<out><xsl:value-of select=\"'5' cast as h:hatsize\"/></out>")
    ));
    assert_eq!(
        failure(run_sa(&main, &[])),
        (Stage::Compile, "XPST0051".to_string())
    );
}

/// §3.14 defines the declarations by a synthetic schema document of `xs:import`s whose `namespace` is
/// the declaration's: a schema document whose target namespace differs breaks src-import (XML Schema
/// §4.2.3 / XSD 1.1 §4.2.6.2), and
/// > \[ERR XTSE0220\] It is a static error if the synthetic schema document does not satisfy the
/// > constraints described in \[XML Schema Part 1\] (section 5.1, Errors in Schema Construction and
/// > Structure).
///
/// Both ways: another namespace, and no `namespace` attribute for a schema that has one.
#[test]
fn a_schema_document_of_another_target_namespace_is_xtse0220() {
    for import in [
        r#"<xsl:import-schema namespace="urn:other" schema-location="hats.xsd"/>"#,
        r#"<xsl:import-schema schema-location="hats.xsd"/>"#,
    ] {
        let main = sheet(&format!("{import}{}", root_template("<out/>")));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Context, codes::XTSE0220.to_string()),
            "{import}"
        );
    }
}

/// XTSE0220 for an invalid schema document found at the location: "This includes, without loss of
/// generality, conflicts such as multiple definitions of the same name."
#[test]
fn an_invalid_schema_document_is_xtse0220() {
    let broken = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:hats">
        <xs:element name="hat"/><xs:element name="hat"/></xs:schema>"#;
    let main = sheet(&format!("{IMPORT_HATS}{}", root_template("<out/>")));
    let (stage, code) = failure(run_sa(&main, &[(HATS, broken)]));
    assert_eq!((stage, code.as_str()), (Stage::Context, codes::XTSE0220));
}

// ── §3.14: imports by namespace only ────────────────────────────────────

/// §3.14: "The namespace attribute indicates that a schema for the given namespace is required by the
/// stylesheet. This information may be enough on its own to enable an implementation to locate the
/// required schema components" — for the XML namespace this processor knows the schema (`xml.xsd`,
/// embedded upstream), so `schema-attribute(xml:space)` names an in-scope declaration; without the
/// import it does not (XPath 2.0 XPST0008).
#[test]
fn a_namespace_only_import_of_the_xml_namespace_uses_the_known_schema() {
    let probe = root_template(
        "<out><xsl:value-of select=\"exists(/*/@xml:space[. instance of schema-attribute(xml:space)])\"/></out>",
    );
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="http://www.w3.org/XML/1998/namespace"/>{probe}"#
    ));
    assert_eq!(run_sa(&main, &[]).unwrap(), "<out>false</out>");
    let main = sheet(&probe);
    assert_eq!(
        failure(run_sa(&main, &[])),
        (Stage::Compile, "XPST0008".to_string())
    );
}

// ── §3.14: inline schemas (D-9, the slice approach) ─────────────────────

/// §3.14: "If there is a contained `xs:schema` element, the effective value of the `schemaLocation`
/// attribute is a URI referencing a document containing a copy of the `xs:schema` element" — a copy
/// keeps the element's in-scope namespaces, so a prefix declared only on `xsl:stylesheet` (`xs`, and
/// `h` in a QName-valued attribute) still works inside it.
#[test]
fn an_inline_schema_keeps_the_namespace_bindings_it_inherits() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats">
             <xs:schema targetNamespace="urn:hats">
               <xs:simpleType name="yes-no">
                 <xs:restriction base="xs:string">
                   <xs:enumeration value="yes"/><xs:enumeration value="no"/>
                 </xs:restriction>
               </xs:simpleType>
               <xs:simpleType name="short"><xs:restriction base="h:yes-no"><xs:maxLength value="2"/></xs:restriction></xs:simpleType>
             </xs:schema>
           </xsl:import-schema>
           <xsl:variable name="c" select="h:yes-no('yes')" as="h:yes-no"/>
           {}"#,
        root_template(
            "<out><xsl:value-of select=\"$c instance of h:yes-no, $c, 'maybe' castable as h:yes-no, \
             'yes' castable as h:short, 'no' castable as h:short\"/></out>"
        )
    ));
    assert_eq!(
        run_sa(&main, &[]).unwrap(),
        "<out>true yes false false true</out>"
    );
}

/// §3.14's three allowed combinations: an inline schema without `namespace` takes the namespace "as
/// given on the `xs:schema` element", and both absent is a no-namespace schema (`type-available-0149`:
/// a simple, a complex, a list and a union type).
#[test]
fn an_inline_schema_without_namespace_attribute_imports_its_target_namespace() {
    let main = sheet(&format!(
        r#"<xsl:import-schema>
             <xs:schema targetNamespace="urn:hats"><xs:simpleType name="t"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>
           </xsl:import-schema>
           <xsl:import-schema>
             <xs:schema>
               <xs:simpleType name="shortString"><xs:restriction base="xs:string"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
               <xs:complexType name="monotony"><xs:sequence minOccurs="0" maxOccurs="unbounded"><xs:element name="a"/></xs:sequence></xs:complexType>
               <xs:simpleType name="list"><xs:list itemType="xs:integer"/></xs:simpleType>
               <xs:simpleType name="union"><xs:union memberTypes="xs:gYear xs:gYearMonth"/></xs:simpleType>
             </xs:schema>
           </xsl:import-schema>
           {}"#,
        root_template(
            "<out><xsl:value-of select=\"for $t in ('h:t', 'shortString', 'monotony', 'list', 'union', 'none') return type-available($t)\"/></out>"
        )
    ));
    assert_eq!(
        run_sa(&main, &[]).unwrap(),
        "<out>true true true true true false</out>"
    );
}

/// §3.14: "The base URI of the `xs:import` element is the same as the base URI of the
/// `xsl:import-schema` declaration" — so a relative `xs:include` inside an inline schema resolves
/// against the stylesheet module; and a schema's own `xs:import` of the XML namespace without a location
/// finds the schema this processor knows for it.
#[test]
fn an_inline_schema_resolves_its_directives_against_the_module() {
    let main = sheet(&format!(
        r#"<xsl:import-schema namespace="urn:hats">
             <xs:schema targetNamespace="urn:hats">
               <xs:include schemaLocation="parts/sizes.xsd"/>
               <xs:import namespace="http://www.w3.org/XML/1998/namespace"/>
               <xs:element name="hat"><xs:complexType><xs:attribute ref="xml:lang"/></xs:complexType></xs:element>
             </xs:schema>
           </xsl:import-schema>
           {}"#,
        root_template(
            "<out><xsl:value-of select=\"type-available('h:bigsize'), 120 castable as h:bigsize, 99 castable as h:bigsize\"/></out>"
        )
    ));
    assert_eq!(
        run_sa(&main, &[(SIZES, SIZES_XSD)]).unwrap(),
        "<out>true true false</out>"
    );
}

/// An error in an inline schema is XTSE0220, reported in the stylesheet module, at a line inside the
/// inline schema (D-9: the slice keeps the module's line numbers).
#[test]
fn an_error_in_an_inline_schema_is_xtse0220_at_its_line_in_the_module() {
    let main = sheet(
        "\n<xsl:import-schema namespace=\"urn:dup\">\n  <xs:schema targetNamespace=\"urn:dup\">\n    \
         <xs:element name=\"temp\"/>\n    <xs:element name=\"temp\"/>\n  </xs:schema>\n</xsl:import-schema>\n\
         <xsl:template match=\"/\"><out/></xsl:template>",
    );
    match run_sa(&main, &[]) {
        Err((Stage::Context, e)) => {
            assert_eq!(e.code, codes::XTSE0220, "{e}");
            let location = e.location.clone().expect("a location");
            assert_eq!(location.module_uri.as_deref(), Some(MAIN), "{e}");
            assert!(
                (3..=6).contains(&location.line),
                "the line {} is not inside the inline schema (lines 3-6): {e}",
                location.line
            );
        }
        other => panic!("expected XTSE0220 while building the context, got {other:?}"),
    }
}

/// > \[ERR XTSE0215\] It is a static error if an `xsl:import-schema` element that contains an
/// > `xs:schema` element has a `schema-location` attribute, or if it has a `namespace` attribute that
/// > conflicts with the target namespace of the contained schema.
#[test]
fn an_inline_schema_with_a_location_or_a_conflicting_namespace_is_xtse0215() {
    for import in [
        r#"<xsl:import-schema namespace="urn:a" schema-location="hats.xsd"><xs:schema targetNamespace="urn:a"/></xsl:import-schema>"#,
        r#"<xsl:import-schema namespace="urn:a"><xs:schema targetNamespace="urn:b"/></xsl:import-schema>"#,
        r#"<xsl:import-schema namespace="urn:a"><xs:schema/></xsl:import-schema>"#,
    ] {
        let main = sheet(&format!("{import}{}", root_template("<out/>")));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Context, codes::XTSE0215.to_string()),
            "{import}"
        );
    }
}

/// The content of `xsl:import-schema` is `xs:schema?` (§3.14's syntax box), so two schemas, any other
/// element, an `xs:schema` in another namespace (`import-schema-156`'s mis-bound prefix) or text is
/// XTSE0010 (§3.4).
#[test]
fn import_schema_content_other_than_one_xs_schema_is_xtse0010() {
    for content in [
        r#"<xs:schema targetNamespace="urn:a"/><xs:schema targetNamespace="urn:a"/>"#,
        r#"<xs:element name="e"/>"#,
        r#"<w:schema xmlns:w="urn:wrong" targetNamespace="urn:a"/>"#,
        r#"text"#,
    ] {
        let main = sheet(&format!(
            "<xsl:import-schema namespace=\"urn:a\">{content}</xsl:import-schema>{}",
            root_template("<out/>")
        ));
        assert_eq!(
            failure(run_sa(&main, &[])),
            (Stage::Context, codes::XTSE0010.to_string()),
            "{content}"
        );
    }
}

// ── §3.14: import precedence ────────────────────────────────────────────

const A_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:hats">
  <xs:simpleType name="fromA"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>"#;
const B_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:hats">
  <xs:simpleType name="fromB"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>"#;

fn module(top: &str) -> String {
    sheet(top)
}

/// > If two `xsl:import-schema` declarations specify the same namespace, or if both specify no
/// > namespace, then only the one with highest import precedence is used.
#[test]
fn only_the_import_of_highest_precedence_is_used_for_a_namespace() {
    let imported = module(r#"<xsl:import-schema namespace="urn:hats" schema-location="a.xsd"/>"#);
    let main = sheet(&format!(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:import-schema namespace="urn:hats" schema-location="b.xsd"/>
           {}"#,
        root_template("<out><xsl:value-of select=\"type-available('h:fromA'), type-available('h:fromB')\"/></out>")
    ));
    let files = [
        ("file:///m4a/imported.xsl", imported.as_str()),
        ("file:///m4a/a.xsd", A_XSD),
        ("file:///m4a/b.xsd", B_XSD),
    ];
    assert_eq!(run_sa(&main, &files).unwrap(), "<out>false true</out>");
}

/// > If this leaves more than one, then all the declarations at the highest import precedence are
/// > used — an included module's declarations have the includer's precedence (§3.10.2).
#[test]
fn imports_of_a_namespace_at_the_same_precedence_are_all_used() {
    let included = module(r#"<xsl:import-schema namespace="urn:hats" schema-location="a.xsd"/>"#);
    let main = sheet(&format!(
        r#"<xsl:include href="included.xsl"/>
           <xsl:import-schema namespace="urn:hats" schema-location="b.xsd"/>
           {}"#,
        root_template("<out><xsl:value-of select=\"type-available('h:fromA'), type-available('h:fromB')\"/></out>")
    ));
    let files = [
        ("file:///m4a/included.xsl", included.as_str()),
        ("file:///m4a/a.xsd", A_XSD),
        ("file:///m4a/b.xsd", B_XSD),
    ];
    assert_eq!(run_sa(&main, &files).unwrap(), "<out>true true</out>");
}

// ── §19.2: the static checks of [xsl:]type and [xsl:]validation ─────────

/// > \[ERR XTSE1505\] It is a static error if both the `[xsl:]type` and `[xsl:]validation` attributes
/// > are present on the `xsl:element`, `xsl:attribute`, `xsl:copy`, `xsl:copy-of`, `xsl:document`, or
/// > `xsl:result-document` instructions, or on a literal result element.
#[test]
fn type_and_validation_together_are_xtse1505() {
    for body in [
        r#"<e xsl:validation="strict" xsl:type="xs:anyType"/>"#,
        r#"<xsl:element name="e" validation="preserve" type="xs:anyType"/>"#,
        r#"<e><xsl:attribute name="a" validation="strip" type="xs:string"/></e>"#,
        r#"<xsl:copy validation="strip" type="xs:anyType"/>"#,
        r#"<xsl:copy-of select="." validation="strip" type="xs:anyType"/>"#,
        r#"<xsl:document validation="strip" type="xs:anyType"/>"#,
        r#"<xsl:result-document validation="strip" type="xs:anyType"/>"#,
    ] {
        let main = sheet(&format!("{IMPORT_HATS}{}", root_template(body)));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Compile, codes::XTSE1505.to_string()),
            "{body}"
        );
    }
}

/// > \[ERR XTSE1520\] It is a static error if the value of the type attribute … is not a valid QName, or
/// > if it uses a prefix that is not defined in an in-scope namespace declaration, or if the QName is
/// > not the name of a type definition included in the in-scope schema components for the stylesheet.
///
/// `error-1520a` / `-b` / `-c`, plus an unprefixed name: §19.2.1.2, "If the QName has no prefix, it is
/// expanded using the default namespace established using the effective
/// `[xsl:]xpath-default-namespace`".
#[test]
fn a_type_that_is_not_an_in_scope_type_is_xtse1520() {
    for body in [
        r#"<e xsl:type="h:absent-type"/>"#,
        r#"<e xsl:type="your:absent-type"/>"#,
        r#"<e xsl:type="your::absent-type"/>"#,
        r#"<xsl:element name="e" type="hatsize"/>"#,
        r#"<xsl:copy-of select="." type="h:hat"/>"#,
    ] {
        let main = sheet(&format!("{IMPORT_HATS}{}", root_template(body)));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Compile, codes::XTSE1520.to_string()),
            "{body}"
        );
    }
    // With the default namespace for types, the unprefixed name is found (and the fence of the later
    // sub-milestone is what stops it, once evaluated).
    let main = sheet(&format!(
        "{IMPORT_HATS}{}",
        root_template(
            r#"<xsl:element name="e" type="hatsize" xpath-default-namespace="urn:hats"/>"#
        )
    ));
    assert_eq!(
        fenced(run_sa(&main, &hat_files())),
        (Stage::Run, "M4f".to_string())
    );
}

/// > \[ERR XTSE1530\] It is a static error if the value of the type attribute of an `xsl:attribute`
/// > instruction refers to a complex type definition.
///
/// `error-1530a` (a user complex type, unprefixed under `xpath-default-namespace`) and `xs:anyType`.
#[test]
fn a_complex_type_on_xsl_attribute_is_xtse1530() {
    for attribute in [
        r#"<xsl:attribute name="d" type="hatType" xpath-default-namespace="urn:hats">x</xsl:attribute>"#,
        r#"<xsl:attribute name="d" type="xs:anyType">x</xsl:attribute>"#,
    ] {
        let main = sheet(&format!(
            "{IMPORT_HATS}{}",
            root_template(&format!("<e>{attribute}</e>"))
        ));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Compile, codes::XTSE1530.to_string()),
            "{attribute}"
        );
    }
}

/// XTSE0020 (§2.2): `[xsl:]validation` takes `strict`, `lax`, `preserve` or `strip` (§19.2.1.1), and
/// `default-validation` only `preserve` or `strip` (§3.6's syntax box; `validation-0110`).
#[test]
fn a_validation_value_that_is_not_permitted_is_xtse0020() {
    for body in [
        r#"<xsl:element name="e" validation="bogus"/>"#,
        r#"<e xsl:validation="none"/>"#,
        r#"<xsl:copy-of select="." validation="STRICT"/>"#,
    ] {
        let main = sheet(&root_template(body));
        assert_eq!(
            failure(run_sa(&main, &[])),
            (Stage::Compile, codes::XTSE0020.to_string()),
            "{body}"
        );
    }
    for value in ["strict", "lax", "bogus"] {
        let main = sheet_with(
            &format!("default-validation=\"{value}\""),
            &root_template("<out/>"),
        );
        assert_eq!(
            failure(run_sa(&main, &[])),
            (Stage::Compile, codes::XTSE0020.to_string()),
            "default-validation={value}"
        );
    }
}

// ── The fences: what later sub-milestones own ───────────────────────────

/// A non-`strip` `[xsl:]validation`, and `[xsl:]type`, are accepted when the stylesheet is compiled and
/// raise `XSLT2-NOTIMPL` naming the owning sub-milestone when the instruction is evaluated (M4_PLAN §4):
/// M4d constructed elements and attributes, M4e copies and documents, M4f `type` on elements and
/// copies and `lax` everywhere. `type` on `xsl:attribute` is M4d.
#[test]
fn a_non_strip_validation_is_fenced_with_its_owner_when_evaluated() {
    let cases = [
        (r#"<e xsl:validation="strict"/>"#, "M4d"),
        (r#"<e xsl:validation="preserve"/>"#, "M4d"),
        (r#"<xsl:element name="e" validation="strict"/>"#, "M4d"),
        (
            r#"<e><xsl:attribute name="a" validation="strict">1</xsl:attribute></e>"#,
            "M4d",
        ),
        (
            r#"<e><xsl:attribute name="a" type="h:hatsize">7</xsl:attribute></e>"#,
            "M4d",
        ),
        (r#"<xsl:copy-of select="." validation="strict"/>"#, "M4e"),
        (r#"<xsl:copy-of select="." validation="preserve"/>"#, "M4e"),
        (r#"<xsl:copy validation="strict"/>"#, "M4e"),
        (
            r#"<xsl:document validation="strict"><e/></xsl:document>"#,
            "M4e",
        ),
        (
            r#"<xsl:result-document validation="strict"><e/></xsl:result-document>"#,
            "M4e",
        ),
        (r#"<e xsl:type="h:hatType"/>"#, "M4f"),
        (r#"<xsl:element name="e" type="h:hatType"/>"#, "M4f"),
        (r#"<xsl:copy-of select="." type="h:hatType"/>"#, "M4f"),
        (r#"<xsl:document type="h:hatType"/>"#, "M4f"),
        (r#"<e xsl:validation="lax"/>"#, "M4f"),
        (
            r#"<e><xsl:attribute name="a" validation="lax">1</xsl:attribute></e>"#,
            "M4f",
        ),
        (r#"<xsl:copy-of select="." validation="lax"/>"#, "M4f"),
    ];
    for (body, owner) in cases {
        let main = sheet(&format!("{IMPORT_HATS}{}", root_template(body)));
        assert_eq!(
            fenced(run_sa(&main, &hat_files())),
            (Stage::Run, owner.to_string()),
            "{body}"
        );
    }
}

/// The fence is at evaluation: an instruction that is never evaluated costs nothing, and an explicit
/// `strip` runs (§19.2.1.1: "The value strip indicates that the new node and each of the contained
/// nodes … will have the type annotation `xs:untyped`" — what every tree has before M4d).
#[test]
fn an_unevaluated_or_strip_validation_runs() {
    let main = sheet(&format!(
        "{IMPORT_HATS}{}",
        root_template(
            r#"<out xsl:validation="strip">
                 <xsl:if test="false()"><e xsl:validation="strict"/><g xsl:type="h:hatType"/></xsl:if>
                 <xsl:if test="false()"><xsl:copy-of select="." validation="preserve"/></xsl:if>
                 <xsl:element name="f" validation="strip"><xsl:copy-of select="/doc" validation="strip"/></xsl:element>
               </out>"#
        )
    ));
    assert_eq!(
        run_sa(&main, &hat_files()).unwrap(),
        "<out><f><doc/></f></out>"
    );
}

/// §3.6: `default-validation` "defines the default value of the validation attribute of all
/// `xsl:document`, `xsl:element`, `xsl:attribute`, `xsl:copy`, `xsl:copy-of`, and
/// `xsl:result-document` instructions, and of the `xsl:validation` attribute of all literal result
/// elements … This default applies within the stylesheet module: it does not extend to included or
/// imported stylesheet modules."
#[test]
fn default_validation_is_the_default_of_its_own_module_only() {
    // The imported module says `preserve`; its template is called and constructs an element: M4d.
    let imported = sheet_with(
        r#"default-validation="preserve""#,
        r#"<xsl:template name="make"><made/></xsl:template>"#,
    );
    let main = sheet(&format!(
        r#"<xsl:import href="imported.xsl"/>{}"#,
        root_template(r#"<out><xsl:call-template name="make"/></out>"#)
    ));
    let files = [("file:///m4a/imported.xsl", imported.as_str())];
    assert_eq!(
        fenced(run_sa(&main, &files)),
        (Stage::Run, "M4d".to_string())
    );

    // The principal module's own `strip` default is not changed by the imported module.
    let main = sheet(&format!(
        r#"<xsl:import href="imported.xsl"/>{}"#,
        root_template("<out/>")
    ));
    assert_eq!(run_sa(&main, &files).unwrap(), "<out/>");

    // An explicit attribute overrides the module default.
    let main = sheet_with(
        r#"default-validation="preserve""#,
        &root_template(
            r#"<out xsl:validation="strip"><xsl:element name="e" validation="strip"/></out>"#,
        ),
    );
    match run_sa(&main, &[]) {
        // The final result tree (§3.6: "It also determines the validation applied to the implicit
        // final result tree") is still under `preserve`: that is M4e's.
        Err(e) => assert_eq!(fenced(Err(e)), (Stage::Run, "M4e".to_string())),
        Ok(text) => panic!("the implicit final result tree under preserve is fenced, got {text:?}"),
    }
}

/// Typed patterns are M4c's (M4_PLAN §4 M4c): the fence names it, in a schema-aware stylesheet as in a
/// basic one.
#[test]
fn a_typed_pattern_is_fenced_with_m4c() {
    for pattern in [
        "element(*, h:hatType)",
        "schema-element(h:hat)",
        "attribute(*, h:hatsize)",
    ] {
        let main = sheet(&format!(
            r#"{IMPORT_HATS}<xsl:template match="{pattern}"/>{}"#,
            root_template("<out/>")
        ));
        assert_eq!(
            fenced(run_sa(&main, &hat_files())),
            (Stage::Compile, "M4c".to_string()),
            "{pattern}"
        );
    }
}

/// §21.1's XTDE1665 is a basic processor's rule. A schema-aware processor accepts typed input (M4b,
/// typed source documents; M4 plan D-13): a tree the host typed against the context's own schema set
/// is processed with its annotations (before M4b this was the M4b fence).
#[test]
fn a_typed_input_tree_of_the_contexts_set_is_accepted_in_schema_aware_mode() {
    let main = sheet(
        r#"<xsl:import-schema><xs:schema><xs:element name="n" type="xs:integer"/></xs:schema></xsl:import-schema>
           <xsl:template match="/"><out><xsl:value-of select="n + 1, data(n) instance of xs:integer"/></out></xsl:template>"#,
    );
    let resolver = resolver(&main, &[]);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)
        .and_then(|b| b.build())
        .expect("the context builds");
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
        .expect("the stylesheet compiles");
    let arena = Bump::new();
    let doc = build_typed_document(
        "<n>5</n>".as_bytes(),
        &arena,
        context.schema_set(),
        BufferDocumentOptions::default(),
    )
    .expect("a typed document");
    assert!(doc.has_type_annotations());
    let run = compiled.transformation(&arena, TransformOptions::default());
    let id = run
        .store()
        .borrow_mut()
        .insert(doc, Some("file:///m4a/typed.xml"));
    let outcome = run
        .transform_document(id)
        .expect("a typed input tree is accepted");
    assert_eq!(
        common::body(compiled.serialize_primary(&outcome).unwrap()),
        "<out>6 true</out>"
    );
}

// ── The context (D-1) ───────────────────────────────────────────────────

/// D-1: the context is built from the stylesheet it is used with. Compiling a stylesheet whose
/// `xsl:import-schema` the context did not load is refused — not silently compiled without the
/// imported types.
#[test]
fn a_context_not_built_from_the_stylesheet_is_refused() {
    let main = sheet(&format!("{IMPORT_HATS}{}", root_template("<out/>")));
    let resolver = resolver(&main, &hat_files());
    let functions = XsltFunctions::new();
    let context = SchemaContext::new();
    let error = match Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
    {
        Err(e) => e,
        Ok(_) => panic!("the context lacks the stylesheet's import"),
    };
    assert!(error.is_internal(), "{error}");
    assert!(error.message.contains("xsl:import-schema"), "{error}");

    // A stylesheet without imports compiles against any context, the empty one included.
    let main = sheet(&root_template("<out/>"));
    let resolver = self::resolver(&main, &[]);
    Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN)
        .expect("no import, nothing to cover");
}

/// What the context loaded, for the caller: one entry per declaration that won its namespace, with its
/// source and whether a schema document was found (§3.14: not finding one is not an error).
#[test]
fn the_context_lists_the_declarations_it_loaded() {
    let imported = module(r#"<xsl:import-schema namespace="urn:hats" schema-location="a.xsd"/>"#);
    let main = sheet(&format!(
        r#"<xsl:import href="imported.xsl"/>
           {IMPORT_HATS}
           <xsl:import-schema namespace="urn:missing" schema-location="missing.xsd"/>
           <xsl:import-schema namespace="http://www.w3.org/XML/1998/namespace"/>
           <xsl:import-schema><xs:schema targetNamespace="urn:inline"/></xsl:import-schema>
           {}"#,
        root_template("<out/>")
    ));
    let mut files = hat_files();
    files.push(("file:///m4a/imported.xsl", imported.as_str()));
    files.push(("file:///m4a/a.xsd", A_XSD));
    let resolver = resolver(&main, &files);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(resolver)
        .stylesheet_uri(MAIN, &functions)
        .and_then(|b| b.build())
        .expect("the context builds");
    let summary: Vec<(Option<&str>, String, bool)> = context
        .imports()
        .iter()
        .map(|import| {
            let source = match import.source() {
                ImportSource::Location(uri) => format!("location {uri}"),
                ImportSource::Inline { system_id } => format!("inline {system_id}"),
                ImportSource::NamespaceOnly => "namespace".to_string(),
            };
            (import.namespace(), source, import.found())
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (Some("urn:hats"), format!("location {HATS}"), true),
            (
                Some("urn:missing"),
                "location file:///m4a/missing.xsd".to_string(),
                false
            ),
            (
                Some("http://www.w3.org/XML/1998/namespace"),
                "namespace".to_string(),
                true
            ),
            (
                Some("urn:inline"),
                "inline file:///m4a/main.xsl#schema-1".to_string(),
                true
            ),
        ]
    );
    // The schema set holds the imported components, under the context's own name table.
    let set = context.schema_set();
    let ns = context.names().add("urn:hats");
    assert!(set
        .lookup_type(Some(ns), context.names().add("hatsize"))
        .is_some());
    assert!(set
        .lookup_type(Some(ns), context.names().add("fromA"))
        .is_none());
}

/// Decision D-A (user, 2026-10-20; `type-functions-0503`). §10.3's `override` "defines what happens
/// if this function has the same name and arity as a function provided by the implementer or made
/// available in the static context using an implementation-defined mechanism" — and a user type's
/// constructor function is neither: XPath 2.0 §3.10.4 defines it ("For every atomic type in the
/// in-scope schema types … a constructor function is implicitly defined"), from the standard
/// `xsl:import-schema`. XPath 2.0 §2.1.1: "Each function is uniquely identified by its expanded QName
/// and its arity", and "The function signatures include the signatures of constructor functions" —
/// so a stylesheet function with the name and arity of one is the clash XSLT 2.0 names XTSE0770 ("two
/// or more functions with the same expanded-QName, the same arity, and the same import precedence"),
/// whatever `override` says. Another arity, or the name of a list or union type (which has no
/// constructor function), is no clash.
#[test]
fn a_stylesheet_function_with_the_name_and_arity_of_a_constructor_function_is_xtse0770() {
    let function = |name: &str, attrs: &str, arity: usize| {
        let params: String = (0..arity)
            .map(|i| format!(r#"<xsl:param name="p{i}" as="xs:string"/>"#))
            .collect();
        format!(
            r#"<xsl:function name="{name}" as="xs:string"{attrs}>{params}<xsl:sequence select="'!'"/></xsl:function>"#
        )
    };
    for attrs in ["", r#" override="yes""#, r#" override="no""#] {
        let main = sheet(&format!(
            "{IMPORT_HATS}{}{}",
            function("h:vreg", attrs, 1),
            root_template("<out><xsl:value-of select=\"h:vreg('abc123')\"/></out>")
        ));
        assert_eq!(
            failure(run_sa(&main, &hat_files())),
            (Stage::Compile, codes::XTSE0770.to_string()),
            "{attrs}"
        );
    }
    let lists = r#"<xsl:import-schema namespace="urn:hats">
          <xs:schema targetNamespace="urn:hats">
            <xs:simpleType name="sizes"><xs:list itemType="xs:integer"/></xs:simpleType>
            <xs:simpleType name="either"><xs:union memberTypes="xs:integer xs:date"/></xs:simpleType>
            <xs:simpleType name="code"><xs:restriction base="xs:string"/></xs:simpleType>
          </xs:schema>
        </xsl:import-schema>"#;
    let main = sheet(&format!(
        "{lists}{}{}{}{}",
        function("h:sizes", "", 1),
        function("h:either", "", 1),
        function("h:code", "", 2),
        root_template(
            "<out><xsl:value-of select=\"h:sizes('1'), h:either('x'), h:code('a', 'b'), h:code('a')\"/></out>"
        )
    ));
    assert_eq!(run_sa(&main, &[]).unwrap(), "<out>! ! ! a</out>");
}

/// The constructor functions of the built-in types (`xs:integer#1` …) cannot meet a stylesheet
/// function at all: §3.2 lists the XML Schema namespace among the reserved namespaces — "Reserved
/// namespaces must not be used, however, in the names of stylesheet-defined objects such as variables
/// and stylesheet functions" — so the declaration is XTSE0080 (§10.3: "The prefix must not refer to a
/// reserved namespace"; catalog `function-1021`), in a basic and a schema-aware processor alike.
#[test]
fn a_stylesheet_function_in_the_xml_schema_namespace_is_xtse0080() {
    let main = sheet(&format!(
        r#"<xsl:function name="xs:integer" as="xs:string"><xsl:param name="in"/><xsl:sequence select="'!'"/></xsl:function>{}"#,
        root_template("<out/>")
    ));
    assert_eq!(
        failure(run_basic(&main, &[])),
        (Stage::Compile, codes::XTSE0080.to_string())
    );
    assert_eq!(
        failure(run_sa(&main, &[])),
        (Stage::Compile, codes::XTSE0080.to_string())
    );
}
