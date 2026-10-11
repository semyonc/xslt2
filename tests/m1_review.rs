//! One regression test per defect found by the independent review of M1.
//!
//! Every case is the reviewer's own reproduction, ported to assert the
//! **correct** result rather than the observed one. The helper mirrors the
//! review probe exactly: an in-memory resolver serving `file:///review/in.xml`
//! plus whatever extra modules a case needs, a stylesheet compiled at
//! `file:///review/main.xsl`, and the serialized primary result.

mod common;

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn sheet(body: &str) -> String {
    format!("<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>")
}

/// Runs one stylesheet over one source document and returns the serialized
/// primary result.
fn run(
    body: &str,
    source: &str,
    extra: &[(&str, &str)],
    by_uri: bool,
) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///review/in.xml", source);
    for (uri, text) in extra {
        mem.insert(*uri, *text);
    }
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet(body), "file:///review/main.xsl")?;
    let arena = bumpalo::Bump::new();
    let transformation = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver);
    let outcome = if by_uri {
        transformation.transform_uri("file:///review/in.xml")?
    } else {
        transformation.transform_source("file:///review/in.xml", source)?
    };
    compiled.serialize_primary(&outcome).map(common::body)
}

/// `run` with no extra modules, the source supplied inline.
fn transform(body: &str, source: &str) -> Result<String, XsltError> {
    run(body, source, &[], false)
}

// ── P1-1 xsl:copy-of constructs new nodes, even in a sequence ────────────

/// §11.9.2: "If the item is an element node, a **new** element is constructed
/// and appended to the result sequence." A copy collected into a sequence is a
/// new, parentless node, so it is not the node it was copied from.
#[test]
fn p1_1_copy_of_in_a_sequence_constructs_a_new_parentless_node() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="element()"><xsl:copy-of select="root/item"/></xsl:variable>
             <out same="{$v is root/item}" parents="{count($v/parent::node())}"/>
           </xsl:template>"#,
        "<root><item/></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out same="false" parents="0"/>"#);
}

/// The same for `xsl:copy` of an element, and for the node kinds that reach
/// the receiver's `append_item` fallback: attribute, text, comment and PI.
#[test]
fn p1_1_xsl_copy_of_every_node_kind_constructs_a_new_node() {
    // xsl:copy of an element.
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="element()">
               <xsl:for-each select="root/item"><xsl:copy/></xsl:for-each>
             </xsl:variable>
             <out same="{$v is root/item}" parents="{count($v/parent::node())}"/>
           </xsl:template>"#,
        "<root><item/></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out same="false" parents="0"/>"#);

    // xsl:copy of an attribute — the kind that fell through to append_item.
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="attribute()">
               <xsl:for-each select="root/item/@a"><xsl:copy/></xsl:for-each>
             </xsl:variable>
             <out same="{$v is root/item/@a}" parents="{count($v/parent::node())}"/>
           </xsl:template>"#,
        r#"<root><item a="1"/></root>"#,
    )
    .unwrap();
    assert_eq!(out, r#"<out same="false" parents="0"/>"#);

    // xsl:copy-of of a text node.
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="text()"><xsl:copy-of select="root/item/text()"/></xsl:variable>
             <out same="{$v is root/item/text()}" parents="{count($v/parent::node())}"/>
           </xsl:template>"#,
        "<root><item>t</item></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out same="false" parents="0"/>"#);

    // xsl:copy-of of a comment and of a processing instruction.
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="c" as="comment()"><xsl:copy-of select="root/comment()"/></xsl:variable>
             <xsl:variable name="p" as="processing-instruction()"><xsl:copy-of select="root/processing-instruction()"/></xsl:variable>
             <out c="{count($c/parent::node())}{$c is root/comment()}"
                  p="{count($p/parent::node())}{$p is root/processing-instruction()}"/>
           </xsl:template>"#,
        "<root><!--c--><?pi d?></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out c="0false" p="0false"/>"#);

    // xsl:copy-of of a document node makes a new document node.
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="document-node()"><xsl:copy-of select="/"/></xsl:variable>
             <out same="{$v is /}" root="{name($v/*)}"/>
           </xsl:template>"#,
        "<root><item/></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out same="false" root="root"/>"#);
}

/// The invariant the fix must not break: `xsl:sequence` keeps node identity.
#[test]
fn p1_1_xsl_sequence_still_keeps_identity() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="element()"><xsl:sequence select="root/item"/></xsl:variable>
             <out same="{$v is root/item}" parents="{count($v/parent::node())}"/>
           </xsl:template>"#,
        "<root><item/></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out same="true" parents="1"/>"#);
}

// ── P1-2 union patterns get one default priority per alternative ─────────

/// §6.4: "If the pattern contains multiple alternatives separated by `|`, then
/// the template rule is treated equivalently to a set of template rules, one
/// for each alternative."
#[test]
fn p1_2_each_union_alternative_has_its_own_default_priority() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/b"/></out></xsl:template>
           <xsl:template match="b">RIGHT</xsl:template>
           <xsl:template match="a|*">WRONG</xsl:template>"#,
        "<root><b/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// An explicit `priority` attribute applies to every alternative (§6.4: "The
/// priority of a template rule is specified by the priority attribute").
#[test]
fn p1_2_an_explicit_priority_applies_to_every_alternative() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/b"/></out></xsl:template>
           <xsl:template match="b">WRONG</xsl:template>
           <xsl:template match="a|*" priority="3">RIGHT</xsl:template>"#,
        "<root><b/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// §6.4: "However, it is not an error if a node matches more than one of the
/// alternatives" — two alternatives of the *same* template are not XTRE0540.
#[test]
fn p1_2_two_alternatives_of_one_template_are_not_a_conflict() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(
                r#"<xsl:template match="/"><out><xsl:apply-templates select="root/b"/></out></xsl:template>
                   <xsl:template match="b|*">ONCE</xsl:template>"#,
            ),
            "file:///review/main.xsl",
        )
        .unwrap();
    let arena = bumpalo::Bump::new();
    // `strict_recoverable` turns XTRE0540 into a hard error instead of taking
    // the recovery action, which is what makes the absence of a conflict
    // observable.
    let options = TransformOptions {
        strict_recoverable: true,
        ..TransformOptions::default()
    };
    let outcome = compiled
        .transformation(&arena, options)
        .transform_source("file:///review/in.xml", "<root><b/></root>")
        .unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out>ONCE</out>"
    );
}

/// `xsl:next-match` sees the per-alternative priorities too: the `*`
/// alternative of the second rule is lower-priority than `b`, so it is what
/// `xsl:next-match` from `match="b"` reaches.
#[test]
fn p1_2_next_match_uses_the_per_alternative_priority() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/b"/></out></xsl:template>
           <xsl:template match="b">1<xsl:next-match/></xsl:template>
           <xsl:template match="a|*">2</xsl:template>"#,
        "<root><b/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>12</out>");
}

// ── P1-3 a variable is identified by its expanded QName ──────────────────

/// §5.1/§9.1: variables are identified by expanded QName, so the prefix used
/// at the reference need not be the prefix used at the declaration.
#[test]
fn p1_3_a_variable_is_found_through_any_prefix_bound_to_its_namespace() {
    let out = transform(
        r#"<xsl:variable xmlns:a="urn:a" name="a:v" select="'RIGHT'"/>
           <xsl:template match="/" xmlns:b="urn:a"><out><xsl:value-of select="$b:v"/></out></xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    // §11.1.2 copies the template's in-scope namespaces onto the literal
    // result element, which is why `xmlns:b` appears in the output.
    assert_eq!(out, r#"<out xmlns:b="urn:a">RIGHT</out>"#);
}

/// The worse half of the same defect: an unrelated expression in a context
/// where the *declaration's* prefix is unbound must still compile.
#[test]
fn p1_3_a_variable_in_an_unspellable_namespace_does_not_break_other_expressions() {
    let out = transform(
        r#"<xsl:variable xmlns:a="urn:a" name="a:v" select="'x'"/>
           <xsl:template match="/"><out>ok</out></xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, "<out>ok</out>");
}

/// Shadowing stays by expanded name: the inner `b:v` hides the outer `a:v`.
#[test]
fn p1_3_shadowing_is_by_expanded_name() {
    let out = transform(
        r#"<xsl:variable xmlns:a="urn:a" name="a:v" select="'outer'"/>
           <xsl:template match="/" xmlns:b="urn:a">
             <xsl:variable name="b:v" select="'inner'"/>
             <out><xsl:value-of select="$b:v"/></out>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out xmlns:b="urn:a">inner</out>"#);
}

// ── P2-4 use-when excludes the element and its subtree ───────────────────

/// §3.12: "If the attribute is present and the effective boolean value of the
/// expression is false, then the element, together with all the nodes having
/// that element as an ancestor, is effectively excluded from the stylesheet
/// module … the stylesheet module has the same effect as if the node were not
/// there." An excluded `xsl:include` therefore never loads its module.
#[test]
fn p2_4_a_disabled_include_is_never_loaded() {
    let out = transform(
        r#"<xsl:include href="missing.xsl" use-when="false()"/>
           <xsl:template match="/"><ok/></xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, "<ok/>");
}

/// The same for an `xsl:when`, which is not an instruction of a sequence
/// constructor and so was never consulted.
#[test]
fn p2_4_a_disabled_when_is_not_a_branch_of_the_choose() {
    let out = transform(
        r#"<xsl:template match="/">
             <out><xsl:choose>
               <xsl:when use-when="false()" test="true()">WRONG</xsl:when>
               <xsl:when test="true()">RIGHT</xsl:when>
             </xsl:choose></out>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// The other places an excluded element can sit: `xsl:param`, `xsl:with-param`,
/// `xsl:otherwise`, an `xsl:attribute` of an attribute set, and a literal
/// result element carrying `xsl:use-when`.
#[test]
fn p2_4_use_when_reaches_every_kind_of_element() {
    let out = transform(
        r#"<xsl:attribute-set name="s">
             <xsl:attribute name="gone" use-when="false()">x</xsl:attribute>
             <xsl:attribute name="kept">y</xsl:attribute>
           </xsl:attribute-set>
           <xsl:template match="/">
             <out xsl:use-attribute-sets="s">
               <xsl:call-template name="t">
                 <xsl:with-param name="p" select="'WRONG'" use-when="false()"/>
               </xsl:call-template>
               <gone xsl:use-when="false()"/>
             </out>
           </xsl:template>
           <xsl:template name="t">
             <xsl:param name="dead" use-when="false()" required="yes"/>
             <xsl:param name="p" select="'RIGHT'"/>
             <xsl:value-of select="$p"/>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out kept="y">RIGHT</out>"#);
}

/// §3.12: "If the xsl:stylesheet or xsl:transform element itself is
/// effectively excluded, the effect is to exclude all the children of the
/// xsl:stylesheet or xsl:transform element, but not the xsl:stylesheet or
/// xsl:transform element or its attributes."
#[test]
fn p2_4_an_excluded_stylesheet_element_keeps_itself_and_loses_its_children() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let source = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" use-when=\"false()\">\
           <xsl:template match=\"/\"><never/></xsl:template>\
         </xsl:stylesheet>"
    );
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&source, "file:///review/main.xsl")
        .unwrap();
    let arena = bumpalo::Bump::new();
    // No template rule survives, so the built-in rules run and the result is
    // the string value of the source — not an error about the missing module.
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///review/in.xml", "<root>text</root>")
        .unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "text"
    );
}

// ── P2-5 relative URIs resolve against the expression's base URI ─────────

/// §16.1: a relative URI reference "is resolved against the base URI of
/// `$base-node` if supplied, or against the base URI from the static context
/// otherwise (this will usually be the base URI of the stylesheet module)".
#[test]
fn p2_5_document_resolves_against_the_module_that_contains_the_call() {
    let out = run(
        r#"<xsl:import href="sub/part.xsl"/>"#,
        "<root/>",
        &[
            (
                "file:///review/sub/part.xsl",
                &sheet(
                    r#"<xsl:template match="/"><out><xsl:value-of select="document('data.xml')/value"/></out></xsl:template>"#,
                ),
            ),
            ("file:///review/sub/data.xml", "<value>RIGHT</value>"),
            ("file:///review/data.xml", "<value>WRONG</value>"),
        ],
        false,
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// `doc()`, `unparsed-text()` and `unparsed-text-available()` take the same
/// base URI, and `static-base-uri()` reports it.
#[test]
fn p2_5_doc_and_unparsed_text_resolve_against_the_same_base_uri() {
    let out = run(
        r#"<xsl:import href="sub/part.xsl"/>"#,
        "<root/>",
        &[
            (
                "file:///review/sub/part.xsl",
                &sheet(
                    r#"<xsl:template match="/"><out d="{doc('data.xml')/value}"
                                                    t="{unparsed-text('plain.txt')}"
                                                    a="{unparsed-text-available('plain.txt')}"
                                                    s="{static-base-uri()}"/></xsl:template>"#,
                ),
            ),
            ("file:///review/sub/data.xml", "<value>RIGHT</value>"),
            ("file:///review/sub/plain.txt", "TEXT"),
            ("file:///review/data.xml", "<value>WRONG</value>"),
        ],
        false,
    )
    .unwrap();
    assert_eq!(
        out,
        r#"<out d="RIGHT" t="TEXT" a="true" s="file:///review/sub/part.xsl"/>"#
    );
}

/// `xml:base` on the stylesheet element moves the base URI with it (§5.8).
#[test]
fn p2_5_xml_base_moves_the_base_uri_of_the_expression() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///review/sub/data.xml", "<value>RIGHT</value>");
    mem.insert("file:///review/data.xml", "<value>WRONG</value>");
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let source = format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" xml:base=\"sub/\">\
           <xsl:template match=\"/\"><out><xsl:value-of select=\"document('data.xml')/value\"/></out></xsl:template>\
         </xsl:stylesheet>"
    );
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&source, "file:///review/main.xsl")
        .unwrap();
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///review/in.xml", "<root/>")
        .unwrap();
    assert_eq!(
        compiled
            .serialize_primary(&outcome)
            .map(common::body)
            .unwrap(),
        "<out>RIGHT</out>"
    );
}

// ── P2-6 a document keeps its URI ────────────────────────────────────────

/// The base URI and document URI of the principal source document are its own
/// URI, not the stylesheet's.
#[test]
fn p2_6_the_principal_source_document_keeps_its_uri() {
    let out = transform(
        r#"<xsl:template match="/"><out base="{base-uri(/*)}" doc="{document-uri(/)}"/></xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(
        out,
        r#"<out base="file:///review/in.xml" doc="file:///review/in.xml"/>"#
    );
}

/// The same for a document loaded by `doc()`.
#[test]
fn p2_6_a_loaded_document_keeps_its_uri() {
    let out = run(
        r#"<xsl:template match="/"><out base="{base-uri(doc('extra.xml')/*)}" doc="{document-uri(doc('extra.xml'))}"/></xsl:template>"#,
        "<root/>",
        &[("file:///review/extra.xml", "<root/>")],
        false,
    )
    .unwrap();
    assert_eq!(
        out,
        r#"<out base="file:///review/extra.xml" doc="file:///review/extra.xml"/>"#
    );
}

/// `xml:base` inside a source document still wins over the document URI.
#[test]
fn p2_6_xml_base_inside_a_source_document_still_wins() {
    let out = transform(
        r#"<xsl:template match="/"><out base="{base-uri(/root/child)}"/></xsl:template>"#,
        r#"<root xml:base="http://example.com/x/"><child/></root>"#,
    )
    .unwrap();
    assert_eq!(out, r#"<out base="http://example.com/x/"/>"#);
}

/// §9.4: "The base URI of the document node is taken from the base URI of the
/// variable binding element in the stylesheet … The document-uri and
/// unparsed-entities properties of the new document node are set to empty."
#[test]
fn p2_6_a_temporary_tree_has_the_stylesheet_base_uri_and_no_document_uri() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="t"><a/></xsl:variable>
             <out base="{base-uri($t)}" doc="{document-uri($t)}"/>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out base="file:///review/main.xsl" doc=""/>"#);
}

// ── P2-7 every source document is stripped ───────────────────────────────

/// §4.4: "For the purposes of this section, the term source tree means the
/// document containing the initial context node, and any document returned by
/// the functions document, doc, or collection."
#[test]
fn p2_7_stripping_applies_however_the_source_document_arrives() {
    let strip = r#"<xsl:strip-space elements="*"/>
                   <xsl:template match="/"><out><xsl:value-of select="count(/root/text())"/></out></xsl:template>"#;
    // Supplied inline — this one already worked.
    assert_eq!(
        run(strip, "<root> <child/> </root>", &[], false).unwrap(),
        "<out>0</out>"
    );
    // Loaded by URI.
    assert_eq!(
        run(strip, "<root> <child/> </root>", &[], true).unwrap(),
        "<out>0</out>"
    );
    // Loaded by document().
    let by_document = r#"<xsl:strip-space elements="*"/>
        <xsl:template match="/"><out><xsl:value-of select="count(document('extra.xml')/root/text())"/></out></xsl:template>"#;
    assert_eq!(
        run(
            by_document,
            "<root/>",
            &[("file:///review/extra.xml", "<root> <child/> </root>")],
            false
        )
        .unwrap(),
        "<out>0</out>"
    );
    // And by doc().
    let by_doc = r#"<xsl:strip-space elements="*"/>
        <xsl:template match="/"><out><xsl:value-of select="count(doc('extra.xml')/root/text())"/></out></xsl:template>"#;
    assert_eq!(
        run(
            by_doc,
            "<root/>",
            &[("file:///review/extra.xml", "<root> <child/> </root>")],
            false
        )
        .unwrap(),
        "<out>0</out>"
    );
}

/// §4.4: a whitespace text node is preserved when "an ancestor element of the
/// text node has an xml:space attribute with a value of preserve, and no
/// closer ancestor element has xml:space with a value of default".
#[test]
fn p2_7_xml_space_preserve_survives_a_uri_load() {
    let strip = r#"<xsl:strip-space elements="*"/>
        <xsl:template match="/"><out><xsl:value-of select="count(//text())"/></out></xsl:template>"#;
    assert_eq!(
        run(
            strip,
            r#"<root xml:space="preserve"> <keep> </keep> <inner xml:space="default"> <drop/> </inner> </root>"#,
            &[],
            true
        )
        .unwrap(),
        "<out>4</out>"
    );
}

// ── P2-8 only the four XML whitespace characters are whitespace ──────────

/// §4: "A whitespace text node is a text node whose content consists entirely
/// of whitespace characters (that is, #x09, #x0A, #x0D, or #x20)." A
/// no-break space is not one of them.
#[test]
fn p2_8_a_non_breaking_space_is_not_xml_whitespace() {
    let out = transform(
        r#"<xsl:strip-space elements="*"/>
           <xsl:template match="/"><out><xsl:value-of select="string-length(root)"/></out></xsl:template>"#,
        "<root>\u{00a0}</root>",
    )
    .unwrap();
    assert_eq!(out, "<out>1</out>");
    // The same in a stylesheet text node (§4.2).
    let out = transform(
        "<xsl:template match=\"/\"><out>\u{00a0}</out></xsl:template>",
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, "<out>\u{00a0}</out>");
}

/// The list-valued attributes split on XML whitespace only, so a no-break
/// space is part of the token, not a separator.
#[test]
fn p2_8_list_valued_attributes_split_on_xml_whitespace_only() {
    // A `use-attribute-sets` token containing a no-break space names one set
    // that does not exist, rather than two that do.
    let err = transform(
        "<xsl:attribute-set name=\"a\"><xsl:attribute name=\"x\">1</xsl:attribute></xsl:attribute-set>\
         <xsl:attribute-set name=\"b\"><xsl:attribute name=\"y\">2</xsl:attribute></xsl:attribute-set>\
         <xsl:template match=\"/\"><out xsl:use-attribute-sets=\"a\u{00a0}b\"/></xsl:template>",
        "<root/>",
    );
    assert!(
        err.is_err(),
        "a\u{a0}b is one token, and no set is named that"
    );
    // With a real space, both sets apply.
    let out = transform(
        "<xsl:attribute-set name=\"a\"><xsl:attribute name=\"x\">1</xsl:attribute></xsl:attribute-set>\
         <xsl:attribute-set name=\"b\"><xsl:attribute name=\"y\">2</xsl:attribute></xsl:attribute-set>\
         <xsl:template match=\"/\"><out xsl:use-attribute-sets=\"a b\"/></xsl:template>",
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out x="1" y="2"/>"#);
}

// ── P2-9 current() inside a pattern is the node being matched ────────────

/// §16.6.1: "If the current function is used within a pattern, its value is
/// the node that is being matched against the pattern."
#[test]
fn p2_9_current_in_a_pattern_is_the_node_being_matched() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/group/item"/></out></xsl:template>
           <xsl:template match="group[@id = current()/@parent]/item">RIGHT</xsl:template>
           <xsl:template match="item">WRONG</xsl:template>"#,
        "<root><group id='G'><item parent='G'/></group></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// The same when the predicate is on the last step, where the candidate
/// sequence is the matched node's siblings.
#[test]
fn p2_9_current_is_the_matched_node_in_a_last_step_predicate() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/item"/></out></xsl:template>
           <xsl:template match="item[@n = current()/@n]">Y</xsl:template>
           <xsl:template match="item">n</xsl:template>"#,
        "<root><item n='1'/><item n='2'/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>YY</out>");
}

// ── P2-10 generate-id() defaults to the XPath context item ───────────────

/// §16.6.4: "If the argument is omitted, it defaults to the context node" —
/// the XPath context item, which inside a predicate is not the focus of the
/// containing instruction.
#[test]
fn p2_10_generate_id_with_no_argument_uses_the_xpath_context_item() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:value-of select="count(root/item[generate-id() = generate-id(../item[1])])"/></out></xsl:template>"#,
        "<root><item/><item/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>1</out>");
}

/// `current()` must keep taking the XSLT-level current item, not the XPath
/// context item — the distinction P2-10 turns on.
#[test]
fn p2_10_current_still_takes_the_xslt_current_item() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:value-of select="count(root/item[generate-id(current()) = generate-id(/)])"/></out></xsl:template>"#,
        "<root><item/><item/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>2</out>");
}

// ── P2-11 mode="#all" covers every mode the stylesheet mentions ──────────

/// §6.5: `#all` means "the default mode and every mode that is named in an
/// xsl:apply-templates instruction or xsl:template declaration anywhere in the
/// stylesheet".
#[test]
fn p2_11_all_covers_a_mode_named_only_by_an_apply_templates() {
    let out = transform(
        r##"<xsl:template match="/"><out><xsl:apply-templates select="root/b" mode="m"/></out></xsl:template>
            <xsl:template match="b" mode="#all">RIGHT</xsl:template>"##,
        "<root><b>WRONG</b></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// `#current` chains reach the same rules.
#[test]
fn p2_11_all_covers_a_mode_reached_through_current() {
    let out = transform(
        r##"<xsl:template match="/"><out><xsl:apply-templates select="root" mode="m"/></out></xsl:template>
            <xsl:template match="root" mode="m"><xsl:apply-templates select="b" mode="#current"/></xsl:template>
            <xsl:template match="b" mode="#all">RIGHT</xsl:template>"##,
        "<root><b>WRONG</b></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

/// XTDE0045 still fires for an initial mode that no `xsl:template` names —
/// `#all` is not an expanded QName and so does not name a mode.
#[test]
fn p2_11_an_initial_mode_no_template_names_is_still_xtde0045() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(
            &sheet(r##"<xsl:template match="b" mode="#all">x</xsl:template>"##),
            "file:///review/main.xsl",
        )
        .unwrap();
    let arena = bumpalo::Bump::new();
    let result = compiled
        .transformation(&arena, TransformOptions::default())
        .with_initial_mode(xslt2::ModeKey::Named(xslt2::QName::local("nowhere")))
        .transform_source("file:///review/in.xml", "<root/>");
    let err = match result {
        Ok(_) => panic!("an initial mode no template names must be XTDE0045"),
        Err(e) => e,
    };
    assert_eq!(err.code, "XTDE0045");
}

// ── P2-12 a namespace node of an orphan element keeps its parent ─────────

/// A parentless constructed element still owns its namespace nodes, and a
/// namespace node's parent is the element that owns it.
#[test]
fn p2_12_a_namespace_node_of_a_parentless_element_has_a_parent() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="element()"><e xmlns:p="urn:p"/></xsl:variable>
             <out><xsl:value-of select="count($v/namespace::p/parent::*)"/></out>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, "<out>1</out>");
}

/// The attribute axis of a parentless element behaves the same way, and
/// `root()` of a namespace node of an orphan is the orphan itself.
#[test]
fn p2_12_attributes_and_root_of_a_parentless_element() {
    let out = transform(
        r#"<xsl:template match="/">
             <xsl:variable name="v" as="element()"><e xmlns:p="urn:p" a="1"/></xsl:variable>
             <out attr="{count($v/@a/parent::*)}"
                  anc="{count($v/namespace::p/ancestor::*)}"
                  root="{name(root($v/namespace::p))}"/>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out attr="1" anc="1" root="e"/>"#);
}

// ── P2-13 copy-namespaces="no" copies no namespace node at all ───────────

/// §11.9.2: "If it takes the value no, then none of the namespace nodes are
/// copied: however, namespace nodes will still be created in the result tree
/// as required by the namespace fixup process."
#[test]
fn p2_13_copy_namespaces_no_drops_even_locally_declared_namespaces() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:copy-of select="root/item" copy-namespaces="no"/></out></xsl:template>"#,
        "<root><item xmlns:p='urn:unused'/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out><item/></out>");
}

/// "This attribute affects all elements copied by this instruction: both
/// elements selected directly by the select expression, and elements that are
/// descendants of nodes selected by the select expression." And namespace
/// fixup still declares what the names need.
#[test]
fn p2_13_copy_namespaces_no_reaches_descendants_but_keeps_fixup() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:copy-of select="root/item" copy-namespaces="no"/></out></xsl:template>"#,
        "<root xmlns:q='urn:q'><item xmlns:p='urn:unused'><q:kid xmlns:r='urn:unused2'/></item></root>",
    )
    .unwrap();
    assert_eq!(out, r#"<out><item><q:kid xmlns:q="urn:q"/></item></out>"#);
}

/// §11.9.1 says the same of `xsl:copy`: "If the value is set to no, then the
/// namespace nodes are not copied."
#[test]
fn p2_13_xsl_copy_with_copy_namespaces_no() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:for-each select="root/item"><xsl:copy copy-namespaces="no"/></xsl:for-each></out></xsl:template>"#,
        "<root><item xmlns:p='urn:unused'/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out><item/></out>");
}

// ── P2-14 a prefix resolves only in its own static context ───────────────

/// §16.6.5: the `$property-name` argument "is expanded as described in §5.1
/// Qualified Names", i.e. against the in-scope namespaces of the element the
/// expression is written on. [ERR XTDE1390] if the prefix is not bound there.
#[test]
fn p2_14_a_prefix_bound_on_another_template_does_not_leak() {
    let err = transform(
        r#"<xsl:template name="unused" xmlns:p="http://www.w3.org/1999/XSL/Transform"><xsl:text/></xsl:template>
           <xsl:template match="/"><out><xsl:value-of select="system-property('p:version')"/></out></xsl:template>"#,
        "<root/>",
    )
    .unwrap_err();
    assert_eq!(err.code, "XTDE1390");
}

/// The prefix bound where the expression *is* written still resolves. (The
/// XSLT functions themselves live in the standard *function* namespace, §16,
/// so `function-available` is asked about an unprefixed name.)
#[test]
fn p2_14_a_prefix_in_scope_here_still_resolves() {
    let out = transform(
        r#"<xsl:template match="/" xmlns:p="http://www.w3.org/1999/XSL/Transform">
             <out v="{system-property('p:version')}"
                  e="{element-available('p:if')}"
                  f="{function-available('current')}"/>
           </xsl:template>"#,
        "<root/>",
    )
    .unwrap();
    assert_eq!(out, r#"<out v="2.0" e="true" f="true"/>"#);
}

/// A pattern predicate is compiled in the static context of the
/// `xsl:template` element that carries the pattern, so a prefix bound there
/// resolves inside the predicate.
#[test]
fn p2_14_a_pattern_predicate_uses_its_own_static_context() {
    let out = transform(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="root/item"/></out></xsl:template>
           <xsl:template match="item[system-property('p:version') = '2.0']"
                         xmlns:p="http://www.w3.org/1999/XSL/Transform">RIGHT</xsl:template>"#,
        "<root><item/></root>",
    )
    .unwrap();
    assert_eq!(out, "<out>RIGHT</out>");
}

// ── M1-review leftover: a character the output encoding cannot represent ──
//
// XSLT 2.0 and XQuery 1.0 Serialization (Second Edition), §5.1.2 "XML Output
// Method: the encoding Parameter" (not local; read at w3.org):
//
// > … if the character occurs in a context where XML recognizes character
// > references (that is, in the value of an attribute node or text node), then
// > the character MUST be output as a character reference. A serialization
// > error [err:SERE0008] occurs if such a character appears in a context where
// > character references are not allowed (for example, if the character occurs
// > in the name of an element). The serializer MUST signal the error.
// >
// > For example, if a text node contains the character LATIN SMALL LETTER E
// > WITH ACUTE (#xE9), and the value of the encoding parameter is US-ASCII, the
// > character MUST be serialized as a character reference. If a comment node
// > contains the same character, a serialization error [err:SERE0008] results.
//
// §8.1.2 (the text method): "If the instance of the data model contains a
// character that cannot be represented in the encoding that the serializer is
// using for output, the serializer MUST signal a serialization error
// [err:SERE0008]." The escaping pass used to track only "inside a tag / inside
// quotes", so a comment or PI took the character raw — or as a reference once
// a quote or `>` had gone by — a name took it raw, and the text method got
// references.

const ASCII_OUTPUT: &str = r#"<xsl:output encoding="us-ascii" omit-xml-declaration="yes"/>"#;

fn ascii(template_body: &str) -> Result<String, XsltError> {
    transform(
        &format!(r#"{ASCII_OUTPUT}<xsl:template match="/">{template_body}</xsl:template>"#),
        "<r/>",
    )
}

#[track_caller]
fn sere0008(template_body: &str) {
    match ascii(template_body) {
        Ok(out) => panic!("expected SERE0008, got {out:?} for {template_body}"),
        Err(e) => assert_eq!(e.code, "SERE0008", "{template_body}: {e}"),
    }
}

/// Text and attribute values: a character reference, whatever came before.
#[test]
fn unrepresentable_text_and_attribute_values_become_character_references() {
    assert_eq!(ascii("<out>a\u{E9}b</out>").unwrap(), "<out>a&#233;b</out>");
    assert_eq!(
        ascii(r#"<out x="a&#xE9;b" y="it's &#xE9;"/>"#).unwrap(),
        r#"<out x="a&#233;b" y="it's &#233;"/>"#
    );
    // Markup-looking characters in data never move the scanner out of step:
    // after a comment and a PI whose data hold a quote and a `>`, text and
    // attribute values are still escaped, and a representable name is kept.
    assert_eq!(
        ascii(
            r#"<out><xsl:comment>it's &gt; "x"</xsl:comment><xsl:processing-instruction name="p">a &gt; 'b</xsl:processing-instruction>t&#xE9;<y a="&#xE9;"/></out>"#
        )
        .unwrap(),
        r#"<out><!--it's > "x"--><?p a > 'b?>t&#233;<y a="&#233;"/></out>"#
    );
}

/// Comments, processing instructions and names do not recognize character
/// references, so the character is SERE0008 there — including after a quote or
/// a `>` in the same comment, which used to turn it into a reference.
#[test]
fn unrepresentable_characters_where_references_are_not_allowed_are_sere0008() {
    sere0008("<out><xsl:comment>c\u{E9}</xsl:comment></out>");
    sere0008("<out><xsl:comment>it's \u{E9}</xsl:comment></out>");
    sere0008("<out><xsl:comment>a &gt; \u{E9}</xsl:comment></out>");
    sere0008(
        r#"<out><xsl:processing-instruction name="p">d&#xE9;</xsl:processing-instruction></out>"#,
    );
    sere0008(
        r#"<out><xsl:processing-instruction name="p">it's &gt; &#xE9;</xsl:processing-instruction></out>"#,
    );
    sere0008(r#"<xsl:element name="e&#xE9;"/>"#);
    sere0008(r#"<out><xsl:attribute name="a&#xE9;">v</xsl:attribute></out>"#);
    sere0008(
        r#"<out><xsl:processing-instruction name="p&#xE9;">d</xsl:processing-instruction></out>"#,
    );
    // A comment is not a tag: an unbalanced quote in one used to leave the
    // scanner "inside an attribute value" for the rest of the document, so a
    // later element name was turned into a character reference.
    sere0008(r#"<out><xsl:comment>it's</xsl:comment><xsl:element name="e&#xE9;"/></out>"#);
}

/// §8.1.2: the text method has no character references at all.
#[test]
fn the_text_method_reports_an_unrepresentable_character() {
    let text = |content: &str| {
        transform(
            &format!(
                r#"<xsl:output method="text" encoding="us-ascii"/>
                   <xsl:template match="/"><out>{content}</out></xsl:template>"#
            ),
            "<r/>",
        )
    };
    let e = text("a\u{E9}b").expect_err("the text method has no character references");
    assert_eq!(e.code, "SERE0008", "{e}");
    // The negative control: representable text, `<` and quotes included, is
    // written as it stands.
    assert_eq!(text("a &lt; it's").unwrap(), "a < it's");
}
