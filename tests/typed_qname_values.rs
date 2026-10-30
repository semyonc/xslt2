//! QName-valued defaults and the is-id / is-idrefs properties of union values with a QName member:
//! two namespace-sensitive rules of constructed and copied trees.
//!
//! 1. A `default` / `fixed` value of type `xs:QName` / `xs:NOTATION` (alone, in a list, as a union
//!    member) is read with the namespace bindings of the schema element it is written on
//!    (Datatypes §3.3.18: "When QNames appear in an XML context, the bindings to be used in the
//!    ·lexical mapping· are those in the [in-scope namespaces] property of the relevant element";
//!    Structures §3.2.2: the {value} is "the ·actual value· … of the [attribute]" of the
//!    declaration). The defaulted attributes construction validation creates (§19.2.1.1: "the
//!    validation process will where necessary create new nodes containing these default values")
//!    must therefore hold that {value} in the constructed tree: each QName written with a prefix
//!    the constructed element binds to its namespace, declared on the element where none is
//!    (Structures §3.4.5.1 Note: "When a default value of type QName or NOTATION is applied, it is
//!    ·implementation-dependent· whether ·namespace fixup· occurs; if it does not, the prefix used
//!    in the lexical representation … will not necessarily map to the namespace name of the value"
//!    — this processor performs it, as `xsd-schema`'s typed build does). An outer validation sees the
//!    default as an attribute of the element (§19.2.1.3's serialize–parse–validate), with the
//!    prefix it was written with in scope. An element's `default` / `fixed` text likewise, where a
//!    prefix for its namespace is in scope; where none is, the prefix is declared on the element,
//!    whose start tag is held until it ends (only an element whose value needs a new prefix is
//!    held: `SchemaSet::element_value_constraint_for_type` gives the value before the content).
//! 2. Whether a node is an ID or has the is-idrefs property depends on its typed value (XDM 1.0
//!    §6.2.4, §6.3.4), which for a union with a QName member depends on the node's in-scope
//!    namespaces: a `strip` copy keeps the properties its source has ("The values of the is-id and
//!    is-idrefs properties are unchanged", §19.2.1.1; §4.3), read from the source tree, and a node
//!    a `strip` boundary untypes keeps what the validation that typed it gave it.

mod common;

use std::sync::Arc;

use bumpalo::Bump;

use xslt2::{
    Compiler, MemoryResolver, Resolver, SchemaContext, SourceValidation, TransformOptions,
    XsltError, XsltFunctions,
};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";
const MAIN: &str = "file:///tn/main.xsl";
const SOURCE: &str = "file:///tn/in.xml";

/// No namespace; the schema document binds `p` to `urn:p`. `q`: attributes `qa` (`xs:QName`,
/// default `p:value`), `ql` (a list of `xs:QName`, default `p:a p:b`), `qu` (a union of `xs:integer`
/// and `xs:QName`, default `p:u`), `qf` (`xs:QName`, fixed `p:f`); an optional child `qe`
/// (`xs:QName`, default `p:evalue`). `outer`: one `q` (a nested validation re-assesses it).
/// `qtype`: `q`'s type, named (for `type`). `qd`: an `xs:QName` element, default `p:dvalue`, with
/// no attribute (so no defaulted attribute declares a prefix for `urn:p` on it).
const QXSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:p="urn:p">
  <xs:element name="q" type="qtype"/>
  <xs:complexType name="qtype">
    <xs:sequence>
      <xs:element name="qe" type="xs:QName" default="p:evalue" minOccurs="0"/>
    </xs:sequence>
    <xs:attribute name="qa" type="xs:QName" default="p:value"/>
    <xs:attribute name="ql" type="qlist" default="p:a p:b"/>
    <xs:attribute name="qu" type="intOrQName" default="p:u"/>
    <xs:attribute name="qf" type="xs:QName" fixed="p:f"/>
  </xs:complexType>
  <xs:element name="qd" type="xs:QName" default="p:dvalue"/>
  <xs:element name="outer">
    <xs:complexType><xs:sequence><xs:element ref="q"/></xs:sequence></xs:complexType>
  </xs:element>
  <xs:simpleType name="qlist"><xs:list itemType="xs:QName"/></xs:simpleType>
  <xs:simpleType name="intOrQName"><xs:union memberTypes="xs:integer xs:QName"/></xs:simpleType>
</xs:schema>"#;
const QXSD_URI: &str = "file:///tn/q.xsd";

/// `urn:t`, qualified; the schema document binds `t` to `urn:t` and has no default namespace.
/// `qx`: an `xs:QName` enumerated to `t:x`; `qid` = `qx` | `xs:ID`, `qref` = `qx` | `xs:IDREF`. `r`:
/// `u` elements of simple content `qid` with attributes `id` (`qid`) and `ref` (`qref`); an attribute
/// `nn` (`xs:QName`, default `nons`: a name in no namespace).
const TXSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t"
    targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:simpleType name="qx">
    <xs:restriction base="xs:QName"><xs:enumeration value="t:x"/></xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="qid"><xs:union memberTypes="t:qx xs:ID"/></xs:simpleType>
  <xs:simpleType name="qref"><xs:union memberTypes="t:qx xs:IDREF"/></xs:simpleType>
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="u" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType><xs:simpleContent>
            <xs:extension base="t:qid">
              <xs:attribute name="id" type="t:qid"/>
              <xs:attribute name="ref" type="t:qref"/>
            </xs:extension>
          </xs:simpleContent></xs:complexType>
        </xs:element>
      </xs:sequence>
      <xs:attribute name="nn" type="xs:QName" default="nons"/>
    </xs:complexType>
  </xs:element>
</xs:schema>"#;
const TXSD_URI: &str = "file:///tn/t.xsd";

/// Valid against `TXSD` under the default namespace `urn:t`: `u[1]` is `x` everywhere — the `qx`
/// member, `{urn:t}x` (no ID, no IDREF) —, `u[2]` has the `xs:ID` attribute `y`, the `xs:IDREF`
/// attribute `y` and the `xs:ID` content `z`.
const TDOC: &str = r#"<r xmlns="urn:t"><u id="x" ref="x">x</u><u id="y" ref="y">z</u></r>"#;

/// `fn:id` / `fn:idref` over `TDOC`'s nodes in the tree `$t` (`""`: the context document): the
/// counts of `id('x')`, `id('y')`, `id('z')`, `idref('x')`, `idref('y')`. Every typed form of
/// `TDOC` answers [`TRUE_IDS`].
fn ids(tree: &str) -> String {
    let arg = if tree.is_empty() {
        String::new()
    } else {
        format!(", {tree}")
    };
    format!(
        "count(id('x'{arg})), count(id('y'{arg})), count(id('z'{arg})), \
         count(idref('x'{arg})), count(idref('y'{arg}))"
    )
}

const TRUE_IDS: &str = "0 1 1 0 1";

fn sheet_with(attrs: &str, top: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:t=\"urn:t\" \
           exclude-result-prefixes=\"xs t\" {attrs}>{top}</xsl:stylesheet>"
    )
}

/// A schema-aware run of `main` (host schemas `QXSD` and `TXSD`) over `source`, validated as
/// `validation` says: the serialized primary result, without the XML declaration.
fn run(main: &str, source: &str, validation: SourceValidation) -> Result<String, XsltError> {
    let mut mem = MemoryResolver::new();
    mem.insert(MAIN, main);
    mem.insert(QXSD_URI, QXSD);
    mem.insert(TXSD_URI, TXSD);
    let resolver: Arc<dyn Resolver> = Arc::new(mem);
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::clone(&resolver))
        .stylesheet_uri(MAIN, &functions)?
        .schema_uri(QXSD_URI)
        .schema_uri(TXSD_URI)
        .build()?;
    let compiled = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_uri(MAIN)?;
    let arena = Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = transformation.transform_source_with(SOURCE, source, validation)?;
    Ok(common::body(compiled.serialize_primary(&outcome)?))
}

/// The text of the `<out>` element a template for `/` writes: the declarations `top` (variables),
/// then `<xsl:value-of select="{select}"/>`, over `source` validated strictly (the stylesheet
/// attributes `attrs`).
fn answer_over(attrs: &str, source: &str, top: &str, select: &str) -> Result<String, XsltError> {
    let main = sheet_with(
        attrs,
        &format!(
            r#"<xsl:template match="/">{top}<out><xsl:value-of select="{select}"/></out></xsl:template>"#
        ),
    );
    let text = run(&main, source, SourceValidation::Strict)?;
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

fn answer(top: &str, select: &str) -> String {
    answer_over("", TDOC, top, select)
        .unwrap_or_else(|e| panic!("the run failed: {e}\n{top}\n{select}"))
}

/// What a constructed `q` in `$t` holds of its defaulted QName attributes: `qa` as written and its
/// namespace, `ql` as written and its items' namespaces, the namespaces of `qu` and `qf`.
const Q_PROBE: &str = "string($e/@qa), namespace-uri-from-QName(data($e/@qa)), \
     string($e/@ql), for $n in data($e/@ql) return namespace-uri-from-QName($n), \
     namespace-uri-from-QName(data($e/@qu)), namespace-uri-from-QName(data($e/@qf))";

/// [`Q_PROBE`] over the element `$t/{path}` of the temporary tree `variable` builds.
fn q_answer(variable: &str, path: &str) -> String {
    answer(
        &format!(
            r#"<xsl:variable name="t">{variable}</xsl:variable><xsl:variable name="e" select="$t/{path}"/>"#
        ),
        Q_PROBE,
    )
}

// ── 1. QName / NOTATION defaults in constructed trees ───────────────────────────────────────

/// The scenario for defaulted attributes, on a `strict` element: the instance binds `p`
/// to another namespace, or not at all, or binds `urn:p` to another prefix, or to `p` itself.
/// Every QName of the defaults is `urn:p`'s: the schema's prefix where the element binds it to
/// `urn:p`, another prefix bound to `urn:p` in scope, else a prefix declared on the element (the
/// constructed tree's `ns_dN`).
#[test]
fn defaulted_qname_attributes_keep_the_schemas_namespace_under_strict() {
    let p = "urn:p urn:p urn:p urn:p";
    for (scope, written) in [
        (r#"xmlns:p="urn:other""#, "ns_d0:value"),
        ("", "ns_d0:value"),
        (r#"xmlns:z="urn:p""#, "z:value"),
        (r#"xmlns:p="urn:p""#, "p:value"),
    ] {
        let lists = written.replace("value", "a") + " " + &written.replace("value", "b");
        assert_eq!(
            q_answer(&format!(r#"<q xsl:validation="strict" {scope}/>"#), "q"),
            format!("{written} urn:p {lists} {p}"),
            "{scope}"
        );
    }
}

/// The same defaults under `lax` (the element is declared, so it is validated strictly) and under
/// `type` (`xsl:type` names `q`'s type; the element is any name).
#[test]
fn defaulted_qname_attributes_under_lax_and_type() {
    let p = "urn:p urn:p urn:p urn:p";
    assert_eq!(
        q_answer(r#"<q xsl:validation="lax" xmlns:p="urn:other"/>"#, "q"),
        format!("ns_d0:value urn:p ns_d0:a ns_d0:b {p}")
    );
    assert_eq!(
        q_answer(r#"<any xsl:type="qtype" xmlns:z="urn:p"/>"#, "any"),
        format!("z:value urn:p z:a z:b {p}")
    );
}

/// Nested validations (§19.2.1.1: "the validation requested for a child … is applied before the
/// validation requested for its parent"): the inner run creates the defaults, the outer one
/// validates them as attributes of the element — with the prefix they were written with in scope,
/// so it reads the same value, also where the instance binds `p` nowhere (the outer run would
/// otherwise find the prefix unbound: XTTE1510) or elsewhere.
#[test]
fn an_outer_validation_reads_the_defaults_as_written() {
    let p = "urn:p urn:p urn:p urn:p";
    for scope in ["", r#"xmlns:p="urn:other""#] {
        assert_eq!(
            q_answer(
                &format!(
                    r#"<outer xsl:validation="strict" {scope}><q xsl:validation="strict"/></outer>"#
                ),
                "outer/q"
            ),
            format!("ns_d0:value urn:p ns_d0:a ns_d0:b {p}"),
            "{scope}"
        );
    }
}

/// An element's `default` value of type `xs:QName` (§19.2.1.1: the validation creates "new nodes
/// containing these default values" — its text): written with a prefix bound to the {value}'s
/// namespace in scope at the element (`z` here; `p` itself where the instance binds it to `urn:p`;
/// the prefix the parent's defaulted attributes declared for `urn:p`), under `strict`, under `type`
/// (the local declaration of `qtype`'s `qe`) and under a nested validation that re-validates the
/// text.
#[test]
fn an_element_default_qname_is_written_with_a_prefix_in_scope() {
    for (variable, path) in [
        (
            r#"<q xsl:validation="strict" xmlns:z="urn:p"><qe/></q>"#,
            "q/qe",
        ),
        (
            r#"<any xsl:type="qtype" xmlns:z="urn:p"><qe/></any>"#,
            "any/qe",
        ),
        (
            r#"<outer xsl:validation="strict" xmlns:z="urn:p"><q xsl:validation="strict"><qe/></q></outer>"#,
            "outer/q/qe",
        ),
        (
            r#"<q xsl:validation="strict" xmlns:p="urn:p"><qe/></q>"#,
            "q/qe",
        ),
        (
            r#"<q xsl:validation="strict" xmlns:p="urn:other"><qe/></q>"#,
            "q/qe",
        ),
    ] {
        let expected = if variable.contains(r#"xmlns:p="urn:p""#) {
            "p:evalue urn:p"
        } else if variable.contains("urn:other") {
            "ns_d0:evalue urn:p"
        } else {
            "z:evalue urn:p"
        };
        assert_eq!(
            answer(
                &format!(r#"<xsl:variable name="t">{variable}</xsl:variable>"#),
                &format!("string($t/{path}), namespace-uri-from-QName(data($t/{path}))")
            ),
            expected,
            "{variable}"
        );
    }
}

/// Where no prefix for the {value}'s namespace is in scope at the element (the instance binds `p`
/// elsewhere or nowhere), the text needs a namespace declaration on the element. Neither the
/// tree builder nor an outer validation's namespace snapshot can take a declaration after the
/// start tag, so the start tag of an element whose value needs a new prefix is held until it ends
/// (`tests/xml_schema_1_1.rs`, `tests/xpath_types_timezone_and_locations.rs`), and the prefix is
/// declared on it: the tree's QName is the {value}.
#[test]
fn an_element_default_qname_with_no_prefix_in_scope_is_declared() {
    for scope in [r#"xmlns:p="urn:other""#, ""] {
        assert_eq!(
            answer_over(
                "",
                TDOC,
                &format!(
                    r#"<xsl:variable name="t"><qd xsl:validation="strict" {scope}/></xsl:variable>"#
                ),
                "string($t/qd), namespace-uri-from-QName(data($t/qd))",
            )
            .expect("the default is written with a declared prefix"),
            "ns_d0:dvalue urn:p",
            "{scope}"
        );
    }
}

/// A QName in no namespace (`nons`, written unprefixed in a schema document without a default
/// namespace) cannot be written for an element under a default namespace — no prefix denotes no
/// namespace, and undeclaring the default namespace would move the element's own name — so it
/// keeps its lexical form and reads as a name in the default namespace (as in `xsd-schema`'s
/// typed build); pinned.
#[test]
fn a_no_namespace_qname_default_under_a_default_namespace_keeps_its_lexical_form() {
    assert_eq!(
        answer(
            r#"<xsl:variable name="t"><r xmlns="urn:t" xsl:validation="strict"/></xsl:variable>"#,
            "string($t/t:r/@nn), namespace-uri-from-QName(data($t/t:r/@nn))"
        ),
        "nons urn:t"
    );
}

// ── 2. is-id and is-idrefs of union values with a QName member ─────────────────────────────

/// The scenario: `x` under the default namespace `urn:t` is the `qx` member (`{urn:t}x`),
/// neither an ID nor an IDREF; `y` / `z` are. The typed source and its strip admission copy answer
/// the same.
#[test]
fn a_qname_member_value_is_no_id_in_the_source_and_its_strip_admission_copy() {
    assert_eq!(answer_over("", TDOC, "", &ids("")).unwrap(), TRUE_IDS);
    assert_eq!(
        answer_over(r#"input-type-annotations="strip""#, TDOC, "", &ids("")).unwrap(),
        TRUE_IDS
    );
}

/// `strip` copies keep the properties the source has (read from the source tree, which upstream
/// classified with the node's namespaces), not a classification made again without them:
/// `xsl:copy-of` with `strip` (into a temporary tree, and inside a literal result element),
/// `type="xs:untyped"`, and `preserve` as the control.
#[test]
fn strip_copies_keep_the_sources_classification_of_union_values() {
    for variable in [
        r#"<xsl:copy-of select="/" validation="strip"/>"#,
        r#"<Z><xsl:copy-of select="/t:r" validation="strip"/></Z>"#,
        r#"<xsl:copy-of select="/t:r" type="xs:untyped"/>"#,
        r#"<xsl:copy-of select="/t:r" validation="preserve"/>"#,
    ] {
        assert_eq!(
            answer(
                &format!(r#"<xsl:variable name="t">{variable}</xsl:variable>"#),
                &ids("$t")
            ),
            TRUE_IDS,
            "{variable}"
        );
    }
}

/// Nodes a `strip` boundary untypes keep what the node had just before: a typed source node
/// sequenced into the `strip` element (its copy carries the source's properties), and an element
/// a `strict` validation inside the boundary typed (that validation's typed values, made with the
/// element's namespaces).
#[test]
fn a_strip_boundary_keeps_the_validations_classification_of_union_values() {
    assert_eq!(
        answer(
            r#"<xsl:variable name="t"><w xsl:validation="strip"><xsl:sequence select="/t:r"/></w></xsl:variable>"#,
            &ids("$t")
        ),
        TRUE_IDS
    );
    assert_eq!(
        answer(
            r#"<xsl:variable name="t"><w xsl:validation="strip"><r xmlns="urn:t" xsl:validation="strict"><u id="x" ref="x">x</u><u id="y" ref="y">z</u></r></w></xsl:variable>"#,
            &ids("$t")
        ),
        TRUE_IDS
    );
}

/// A parentless `strip` copy of an attribute (a variable `as="attribute()"`) is copied by the
/// normalizer as any selected attribute is, so it keeps is-idrefs for a later copy into an element
/// (§11.9.2: "The new element will have the same values of the is-id, is-idrefs … properties";
/// §19.2.1.1). Its is-id does not survive that second copy: a copy reads is-id from the source's
/// binding and index, and the parentless copy has no binding (pinned).
#[test]
fn a_parentless_strip_copy_of_an_attribute_keeps_is_idrefs() {
    let top = r#"<xsl:variable name="ref" as="attribute()"><xsl:copy-of select="/t:r/t:u[2]/@ref" validation="strip"/></xsl:variable><xsl:variable name="id" as="attribute()"><xsl:copy-of select="/t:r/t:u[2]/@id" validation="strip"/></xsl:variable><xsl:variable name="t"><e><xsl:sequence select="$ref"/></e><f><xsl:sequence select="$id"/></f></xsl:variable>"#;
    assert_eq!(
        answer(
            top,
            "$t/e/@ref instance of attribute(*, xs:untypedAtomic), count(idref('y', $t)), \
             count(id('y', $t))"
        ),
        "true 1 0"
    );
}
