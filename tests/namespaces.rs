//! Namespace correctness: namespace nodes and undeclarations, the prefix lists,
//! `xsl:namespace-alias`, namespace fixup, `inherit-namespaces`, standalone
//! namespace nodes, and what a namespace URI may contain.
//!
//! The through-line is a single modelling rule: a namespace
//! **undeclaration** (`xmlns=""`) is the *absence* of a default-namespace
//! binding, never a namespace node named `""` with value `""`. XSLT 2.0
//! §5.7.1 talks about "a namespace node with no name" — a node that maps the
//! empty prefix to a **namespace**; there is no such thing as a namespace node
//! mapping the empty prefix to the empty URI. Everything the engine needs the
//! undeclaration for on the way out is re-derived by namespace fixup (§5.7.3).
//!
//! Every test names the W3C XSLT test suite case it reproduces.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Runs `sheet` (a whole stylesheet module) over `source` and returns the
/// serialized primary result.
fn run_sheet(sheet: &str, source: &str) -> Result<String, XsltError> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///namespaces/in.xml", source);
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(sheet, "file:///namespaces/main.xsl")?;
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///namespaces/in.xml", source)?;
    compiled.serialize_primary(&outcome).map(common::body)
}

/// The same, for a stylesheet body wrapped in a plain `xsl:stylesheet`.
fn run(body: &str, source: &str) -> Result<String, XsltError> {
    run_sheet(
        &format!("<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\">{body}</xsl:stylesheet>"),
        source,
    )
}

const IN: &str = "<doc/>";

// ═══ `xmlns=""` is not a namespace node ═══════════════════════════════════

/// `namespace-3141`: `<xsl:element name="foo" xmlns="">` with a literal
/// result element inside it. The LRE's in-scope namespaces used to include
/// the pair `("", "")` coming from the undeclaration; pushing it as a
/// namespace node tripped the engine's own (correct) XTDE0440 guard —
/// §5.7.1: "it is an error to define a default namespace when the element is
/// in no namespace". Nothing is being *defined* here.
#[test]
fn undeclaration_is_not_a_namespace_node() {
    let out = run(
        r#"<xsl:template match="/"><out><xsl:element name="foo" xmlns=""><yyy/></xsl:element></out></xsl:template>"#,
        IN,
    )
    .expect("an xmlns=\"\" on xsl:element is not a default-namespace declaration");
    assert_eq!(out, "<out><foo><yyy/></foo></out>");
}

/// `namespace-0906`: the undeclaration sits on the literal result element
/// itself, under a stylesheet element that declares a default namespace.
#[test]
fn undeclaration_on_a_literal_result_element() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns="http://example.com/">
                 <xsl:template match="/"><out xmlns=""><e xmlns=""/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("an undeclared default namespace leaves the element in no namespace");
    assert_eq!(out, "<out><e/></out>");
}

/// `namespace-2616` (spec bug 5857), and the same shape in `-3002`, `-3003`,
/// `-3004`. §5.7.1's last rule:
///
/// > If the newly constructed node is an element node, and if namespaces are
/// > inherited, then each namespace node of the newly constructed element
/// > (including any produced as a result of the namespace fixup process) is
/// > copied to each descendant element of the newly constructed element,
/// > unless that element or an intermediate element already has a namespace
/// > node with the same name (or absence of a name) or that descendant
/// > element or an intermediate element is in no namespace and the namespace
/// > node has no name.
///
/// `s:element` is in a namespace and has **no** namespace node with no name
/// — the `xmlns=""` is not one — so it inherits `xmlns="http://t.com/"`. The
/// suite's own description: "namespace undeclaration on an LRE is not copied
/// through to the output, because of the rules for namespace inheritance".
#[test]
fn inheritance_survives_an_undeclaration_on_a_child_in_a_namespace() {
    let out = run(
        r#"<xsl:template match="/">
             <s:complexType xmlns:s="http://www.w3.org/2001/XMLSchema" xmlns="http://t.com/">
               <s:element ref="abcd" xmlns=""/>
             </s:complexType>
           </xsl:template>"#,
        IN,
    )
    .expect("an undeclaration on a child in a namespace does not reach the output");
    assert!(
        !out.contains("xmlns=\"\""),
        "the undeclaration must not be serialized: {out}"
    );
    assert!(
        out.contains(r#"<s:element ref="abcd"/>"#),
        "the child inherits the default namespace and declares nothing: {out}"
    );
}

/// `namespace-3001`, the other side of the same rule and the reason the
/// undeclaration cannot simply be dropped everywhere. A **deep copy** attached
/// to a *document* node inherits nothing — a document node has no namespace
/// nodes — so the copied inner element, which has no default-namespace node of
/// its own while its copied parent has one, must still be serialized with
/// `xmlns=""`. §5.7.1: the nodes that propagate are those of "the newly
/// constructed element", and propagation stops at an "intermediate element"
/// that has a namespace node with the same name.
#[test]
fn a_deep_copy_under_a_document_node_keeps_its_undeclaration() {
    let out = run(
        r#"<xsl:template match="/"><xsl:copy-of select="."/></xsl:template>"#,
        r#"<s:complexType xmlns:s="http://www.w3.org/2001/XMLSchema" xmlns="http://t.com/"><s:element xmlns="" ref="abcd"/></s:complexType>"#,
    )
    .expect("a deep copy keeps the source's namespace nodes");
    assert!(
        out.contains(r#"<s:element xmlns="" ref="abcd"/>"#),
        "the inner element has no default-namespace node: {out}"
    );
}

/// The same deep copy, attached to a literal result element that *does* have a
/// default namespace: now the copy root inherits it (`namespace-3004`), and
/// the copy has no intermediate element carrying a default of its own, so
/// nothing is undeclared.
#[test]
fn a_deep_copy_in_element_content_inherits_the_default_namespace() {
    let out = run(
        r#"<xsl:template match="/"><s:complexType xmlns:s="http://www.w3.org/2001/XMLSchema" xmlns="http://t.com/"><xsl:copy-of select="/*/*"/></s:complexType></xsl:template>"#,
        r#"<s:complexType xmlns:s="http://www.w3.org/2001/XMLSchema" xmlns="http://t.com/"><s:element xmlns="" ref="abcd"/></s:complexType>"#,
    )
    .expect("a deep copy in element content inherits the default namespace");
    assert!(!out.contains("xmlns=\"\""), "nothing is undeclared: {out}");
    assert!(out.contains(r#"<s:element ref="abcd"/>"#), "{out}");
}

/// The other half of the same rule, and the reason the engine must still be
/// able to write `xmlns=""`: a descendant **in no namespace** does not
/// inherit a default namespace, so fixup re-derives the undeclaration.
#[test]
fn an_element_in_no_namespace_does_not_inherit_a_default_namespace() {
    let out = run(
        r#"<xsl:template match="/"><p xmlns="http://t.com/"><xsl:element name="q" namespace=""/></p></xsl:template>"#,
        IN,
    )
    .expect("an element in no namespace undeclares the inherited default");
    assert_eq!(out, r#"<p xmlns="http://t.com/"><q xmlns=""/></p>"#);
}

// ═══ the codes the two prefix lists raise ═════════════════════════════════

/// `error-0808a`. XTSE1430 is scoped to `[xsl:]extension-element-prefixes`
/// (§18.2.1); §11.1.3 gives `exclude-result-prefixes` its own code.
///
/// > \[ERR XTSE0808\] It is a static error if a namespace prefix is used
/// > within the `[xsl:]exclude-result-prefixes` attribute and there is no
/// > namespace binding in scope for that prefix.
#[test]
fn an_unbound_exclude_result_prefix_is_xtse0808() {
    let err = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" exclude-result-prefixes="undeclared">
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect_err("an unbound prefix in exclude-result-prefixes is an error");
    assert_eq!(err.code, xslt2::error::codes::XTSE0808, "{err}");
}

/// `error-0808b`. §11.1.3's list is separated by **whitespace**; a comma
/// belongs to the token, so `one,` is simply not a prefix in scope.
#[test]
fn a_comma_in_a_prefix_list_is_part_of_the_token() {
    let err = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:one="http://one/"
                 exclude-result-prefixes="one, two">
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect_err("`one,` is not a prefix");
    assert_eq!(err.code, xslt2::error::codes::XTSE0808, "{err}");
}

/// `error-0809a`.
///
/// > \[ERR XTSE0809\] It is a static error if the value `#default` is used
/// > within the `[xsl:]exclude-result-prefixes` attribute and the parent
/// > element of the `[xsl:]exclude-result-prefixes` attribute has no default
/// > namespace.
#[test]
fn hash_default_with_no_default_namespace_is_xtse0809() {
    let err = run(
        r##"<xsl:template match="/"><out xsl:exclude-result-prefixes="#default"/></xsl:template>"##,
        IN,
    )
    .expect_err("#default needs a default namespace");
    assert_eq!(err.code, xslt2::error::codes::XTSE0809, "{err}");
}

/// The same, where the default namespace is present but **undeclared** on the
/// bearing element: an `xmlns=""` leaves no default-namespace binding, so
/// `#default` designates nothing. This is the prefix-list half of
#[test]
fn hash_default_after_an_undeclaration_is_xtse0809() {
    let err = run_sheet(
        &format!(
            r##"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns="http://example.com/">
                 <xsl:template match="/">
                   <out xmlns="" xsl:exclude-result-prefixes="#default"/>
                 </xsl:template>
               </xsl:stylesheet>"##
        ),
        IN,
    )
    .expect_err("an undeclared default namespace is no default namespace");
    assert_eq!(err.code, xslt2::error::codes::XTSE0809, "{err}");
}

/// `extension-element-prefixes` keeps XTSE1430, for both halves (§18.2.1).
#[test]
fn an_unbound_extension_element_prefix_stays_xtse1430() {
    let err = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" extension-element-prefixes="undeclared">
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect_err("an unbound extension-element prefix is an error");
    assert_eq!(err.code, xslt2::error::codes::XTSE1430, "{err}");
}

/// `error-0812a`. §11.1.4: the code for an unbound prefix on
/// `xsl:namespace-alias` is XTSE0812, not §5.1's generic XTSE0280.
#[test]
fn an_unbound_alias_prefix_is_xtse0812() {
    let err = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:a="http://a/">
                 <xsl:namespace-alias stylesheet-prefix="nosuch" result-prefix="a"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect_err("an unbound alias prefix is an error");
    assert_eq!(err.code, xslt2::error::codes::XTSE0812, "{err}");
}

/// `error-0810a`/`-0810b`.
///
/// > \[ERR XTSE0810\] It is a static error if there is more than one such
/// > declaration with the same literal namespace URI and the same import
/// > precedence and different values for the target namespace URI, unless
/// > there is also an `xsl:namespace-alias` declaration with the same literal
/// > namespace URI and a higher import precedence.
///
/// Note that `-0810b` reaches the same literal URI through **two different
/// prefixes** bound to it, which is why the alias table has to be keyed by URI.
#[test]
fn two_conflicting_aliases_at_one_precedence_are_xtse0810() {
    let err = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:my="http://my.com/"
                 xmlns:your="http://my.com/" xmlns:a="http://a/" xmlns:b="http://b/">
                 <xsl:namespace-alias stylesheet-prefix="my" result-prefix="a"/>
                 <xsl:namespace-alias stylesheet-prefix="your" result-prefix="b"/>
                 <xsl:template match="/"><my:out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect_err("two aliases of one literal URI disagree");
    assert_eq!(err.code, xslt2::error::codes::XTSE0810, "{err}");
}

/// Two aliases that agree are not an error, however they are spelled.
#[test]
fn two_agreeing_aliases_are_not_an_error() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:my="http://my.com/"
                 xmlns:your="http://my.com/" xmlns:a="http://a/">
                 <xsl:namespace-alias stylesheet-prefix="my" result-prefix="a"/>
                 <xsl:namespace-alias stylesheet-prefix="your" result-prefix="a"/>
                 <xsl:template match="/"><my:out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("agreeing aliases");
    assert_eq!(out, r#"<a:out xmlns:a="http://a/"/>"#);
}

// ═══ `#default` on xsl:namespace-alias ════════════════════════════════════

/// `namespace-alias-1904`. §11.1.4:
///
/// > The default namespace (as declared by `xmlns`) may be specified by using
/// > `#default` instead of a prefix. **If no default namespace is in force,
/// > specifying `#default` denotes the null namespace URI.** This allows
/// > elements that are in no namespace in the stylesheet to acquire a
/// > namespace in the result document, or vice versa.
///
/// XTSE0812 exempts `#default` explicitly, so this is never an error.
#[test]
fn hash_default_as_stylesheet_prefix_denotes_no_namespace() {
    let out = run(
        r##"<xsl:namespace-alias stylesheet-prefix="#default" result-prefix="xsl"/>
           <xsl:template match="/"><stylesheet proc="bah">boo</stylesheet></xsl:template>"##,
        IN,
    )
    .expect("#default with no default namespace denotes the null namespace URI");
    assert_eq!(
        out,
        format!(r#"<xsl:stylesheet xmlns:xsl="{NS}" proc="bah">boo</xsl:stylesheet>"#)
    );
}

/// `namespace-alias-1908`: the same, the other way round — a namespaced
/// element in the stylesheet becomes one in no namespace in the result.
#[test]
fn hash_default_as_result_prefix_denotes_no_namespace() {
    let out = run_sheet(
        &format!(
            r##"<xsl:transform version="2.0" xmlns:xsl="{NS}" xmlns:a="http://a/com/">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="#default"/>
                 <xsl:template match="/"><a:foo a:bar="baz"/></xsl:template>
               </xsl:transform>"##
        ),
        IN,
    )
    .expect("#default as result-prefix denotes the null namespace URI");
    assert_eq!(out, r#"<foo bar="baz"/>"#);
}

// ═══ the alias is a stylesheet-wide mapping ═══════════════════════════════

/// `namespace-alias-4201`, `bug-0303`. The alias is declared in the principal
/// module and used by a literal result element in an **included** one, and by
/// one compiled before the declaration was reached. §11.1.4 makes the mapping
/// a property of the stylesheet, not of a module or of declaration order, so
/// it is now collected in a pass of its own before anything is compiled.
#[test]
fn an_alias_reaches_an_included_module() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    mem.insert("file:///namespaces/in.xml", IN);
    mem.insert(
        "file:///namespaces/inc.xsl",
        &*format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:ax="http://alias/">
                 <xsl:template match="doc"><ax:inner/></xsl:template>
               </xsl:stylesheet>"#
        ),
    );
    let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
    let sheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:ax="http://alias/">
             <xsl:include href="inc.xsl"/>
             <xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
             <xsl:namespace-alias stylesheet-prefix="ax" result-prefix="xsl"/>
           </xsl:stylesheet>"#
    );
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::clone(&resolver))
        .compile_str(&sheet, "file:///namespaces/main.xsl")
        .expect("compiles");
    let arena = Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .with_resolver(resolver)
        .transform_source("file:///namespaces/in.xml", IN)
        .expect("runs");
    let out = compiled
        .serialize_primary(&outcome)
        .map(common::body)
        .expect("serializes");
    assert!(
        out.contains(&format!(r#"xmlns:xsl="{NS}""#)) && !out.contains("http://alias/"),
        "the included module's literal result element is aliased too: {out}"
    );
}

// ═══ an explicit namespace node wins (§11.7) ══════════════════════════════

/// §11.7's own example, which is `namespace-2615`'s stylesheet:
///
/// > If a namespace prefix is mapped to a particular namespace URI using the
/// > `xsl:namespace` instruction, or by using `xsl:copy` or `xsl:copy-of` to
/// > copy a namespace node, this prevents the namespace fixup process (and
/// > hence the serializer) from using the same prefix for a different
/// > namespace URI on the same element.
/// >
/// > `<xsl:element name="p:item" xmlns:p="http://www.example.com/p">`
/// > `<xsl:namespace name="p">http://www.example.com/q</xsl:namespace>`
/// > `</xsl:element>`
/// >
/// > a possible serialization of the result tree is:
/// > `<ns0:item xmlns:ns0="http://www.example.com/p" xmlns:p="http://www.example.com/q"/>`
///
/// The engine used to drop the explicit namespace node and keep `p` for the
/// element. Both halves are asserted: the node survives, and the element's own
/// prefix is re-derived. §5.7.3 makes the invented spelling
/// implementation-dependent; this one matches the suite's reference results
/// (`namespace-2614` pins `p_0`, `namespace-alias-1903` pins `ns_0`).
#[test]
fn an_explicit_namespace_node_displaces_the_elements_prefix() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="p:item" xmlns:p="http://p.uri/"><xsl:namespace name="p">http://q.uri/</xsl:namespace></xsl:element></xsl:template>"#,
        IN,
    )
    .expect("the conflict is resolved by inventing a prefix, not by dropping the node");
    assert_eq!(
        out,
        r#"<p_0:item xmlns:p="http://q.uri/" xmlns:p_0="http://p.uri/"/>"#
    );
}

/// The same conflict produced by **copying** a namespace node
/// (`namespace-2001`), which §11.7 lists alongside `xsl:namespace`.
#[test]
fn a_copied_namespace_node_displaces_the_elements_prefix() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="ns:e" namespace="http://nsone.uri/"><xsl:copy-of select="doc/namespace::ns"/></xsl:element></xsl:template>"#,
        r#"<doc xmlns:ns="http://nstwo.uri/"/>"#,
    )
    .expect("a copied namespace node also constrains fixup");
    assert_eq!(
        out,
        r#"<ns_0:e xmlns:ns="http://nstwo.uri/" xmlns:ns_0="http://nsone.uri/"/>"#
    );
}

/// No conflict, no invention: an explicit namespace node that agrees with the
/// element's own prefix changes nothing.
#[test]
fn an_agreeing_namespace_node_leaves_the_prefix_alone() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="p:item" xmlns:p="http://p.uri/"><xsl:namespace name="p">http://p.uri/</xsl:namespace></xsl:element></xsl:template>"#,
        IN,
    )
    .expect("an agreeing namespace node is a duplicate, not a conflict");
    assert_eq!(out, r#"<p:item xmlns:p="http://p.uri/"/>"#);
}

// ═══ a target namespace URI is always copied ══════════════════════════════

/// `namespace-alias-1902`. §11.1.4:
///
/// > A namespace node whose string value is a **target namespace URI** is
/// > copied to the result tree, **whether or not the URI identifies an excluded
/// > namespace**.
///
/// Here the target is the XSLT namespace, which §11.1.2 otherwise always
/// excludes from a literal result element, so `xmlns:xsl` was being dropped.
#[test]
fn a_target_namespace_uri_is_copied_even_when_excluded() {
    let out = run_sheet(
        &format!(
            r##"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns="{NS}" xmlns:x="http://temp/">
                  <xsl:namespace-alias stylesheet-prefix="x" result-prefix="#default"/>
                  <xsl:template match="/"><x:stylesheet version="1.0"/></xsl:template>
                </xsl:stylesheet>"##
        ),
        IN,
    )
    .expect("the target namespace is copied");
    assert!(
        out.contains(&format!(r#"xmlns:xsl="{NS}""#)),
        "the xsl prefix binds a target namespace URI, so it survives: {out}"
    );
}

/// The literal half of the same pair, and the reason the literal rule is
/// applied first: `namespace-alias-1006` is a circular chain in which every URI
/// is both a literal and a target URI, and each result element must carry only
/// the declaration its own aliased name needs.
#[test]
fn a_literal_namespace_uri_is_not_copied() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b"
                   xmlns:a="http://a.com/" xmlns:b="http://b.com/"/>
                 <xsl:namespace-alias stylesheet-prefix="b" result-prefix="a"
                   xmlns:a="http://a.com/" xmlns:b="http://b.com/"/>
                 <xsl:template match="/"><out><v xmlns="http://a.com/"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("a circular pair of aliases");
    assert_eq!(out, r#"<out><b:v xmlns:b="http://b.com/"/></out>"#);
}

// ═══ inherit-namespaces ═══════════════════════════════════════════════════

/// `namespace-2608`. §11.1 lists `[xsl:]inherit-namespaces` among a literal
/// result element's own attributes:
///
/// > A literal result element may have an optional `xsl:inherit-namespaces`
/// > attribute, with the value `yes` or `no`. The default value is `yes`. If
/// > the value is set to `yes`, or is omitted, then the namespace nodes created
/// > for the newly constructed element are copied to the children and
/// > descendants of the newly constructed element … If the value is set to
/// > `no`, then these namespace nodes are not automatically copied.
///
/// The allowed-attribute set simply lacked it, so the case died with XTSE0805.
#[test]
fn inherit_namespaces_is_allowed_on_a_literal_result_element() {
    let out = run(
        r#"<xsl:template match="/"><a xmlns:n="http://n/" xsl:inherit-namespaces="no"><b/></a></xsl:template>"#,
        IN,
    )
    .expect("xsl:inherit-namespaces is a literal-result-element attribute");
    assert_eq!(out, r#"<a xmlns:n="http://n/"><b/></a>"#);
}

/// The part of `inherit-namespaces="no"` that XML 1.0 output *can* express: a
/// child in a namespace no longer inherits the parent's default namespace, so
/// the default has to be undeclared on it. (A prefixed binding cannot be
/// undeclared in XML 1.0 output, so there the child looks as if it had
/// inherited it — §5.7.3's note: "namespace declarations written for the
/// parent element will be inherited by its child elements as if the
/// corresponding namespace nodes were present on the child element". On the
/// tree it is withheld as well: `tests/ns_inherit.rs`.)
///
/// The child is built with `xsl:element`, which — unlike a literal result
/// element — copies no namespace nodes from the stylesheet (§11.2), so the only
/// namespace node it could have with no name is an inherited one. A literal
/// result element written under `xmlns="http://a/"` has a default-namespace node
/// of its own and is therefore unaffected, which is correct and is asserted too.
#[test]
fn inherit_namespaces_no_undeclares_the_default_namespace() {
    let child = r#"<xsl:element name="p:b" namespace="http://p/"/>"#;
    let inherited = run(
        &format!(r#"<xsl:template match="/"><a xmlns="http://a/">{child}</a></xsl:template>"#),
        IN,
    )
    .expect("the default case");
    assert_eq!(
        inherited,
        r#"<a xmlns="http://a/"><p:b xmlns:p="http://p/"/></a>"#
    );
    let not_inherited = run(
        &format!(
            r#"<xsl:template match="/"><a xmlns="http://a/" xsl:inherit-namespaces="no">{child}</a></xsl:template>"#
        ),
        IN,
    )
    .expect("inherit-namespaces=no");
    assert_eq!(
        not_inherited,
        r#"<a xmlns="http://a/"><p:b xmlns="" xmlns:p="http://p/"/></a>"#
    );
    // A literal result element keeps the stylesheet's own default-namespace
    // node, so there is nothing to undeclare.
    let lre_child = run(
        r#"<xsl:template match="/"><a xmlns="http://a/" xmlns:p="http://p/" xsl:inherit-namespaces="no"><p:b/></a></xsl:template>"#,
        IN,
    )
    .expect("a literal result element child");
    assert_eq!(
        lre_child,
        r#"<a xmlns="http://a/" xmlns:p="http://p/"><p:b/></a>"#
    );
}

/// The same attribute on `xsl:element` (§11.2) and `xsl:copy` (§11.9.1), which
/// the compiler already parsed and the runtime then ignored.
#[test]
fn inherit_namespaces_no_on_xsl_element_and_xsl_copy() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="a" namespace="http://a/" inherit-namespaces="no"><p:b xmlns:p="http://p/"/></xsl:element></xsl:template>"#,
        IN,
    )
    .expect("inherit-namespaces on xsl:element");
    assert!(
        out.contains(r#"xmlns="""#),
        "the child does not inherit the default namespace: {out}"
    );
    let copied = run(
        r#"<xsl:template match="/*"><xsl:copy inherit-namespaces="no"><xsl:element name="p:b" namespace="http://p/"/></xsl:copy></xsl:template>"#,
        r#"<a xmlns="http://a/"/>"#,
    )
    .expect("inherit-namespaces on xsl:copy");
    assert!(
        copied.contains(r#"xmlns="""#),
        "the child does not inherit the copied default namespace: {copied}"
    );
}

// ═══ a namespace node on its own in a sequence ════════════════════════════

/// `namespace-3005`. §11.7's result is "a single node, the newly constructed
/// namespace node", and §6.3 lets a newly constructed node land in a sequence
/// parentless, so `<xsl:variable as="node()"><xsl:namespace …/></xsl:variable>`
/// is legal. The engine used to answer `XSLT2-INTERNAL: a namespace node cannot
/// stand on its own in a sequence`.
///
/// The unnamed case is the one that forced the holder out of the normalizer:
/// XTDE0440 forbids a default namespace on an element in no namespace, and the
/// synthetic holder is in no namespace — but it is not a constructed element, so
/// §5.7.1 has nothing to say about it.
#[test]
fn a_namespace_node_can_stand_alone_in_a_sequence() {
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="n1" as="node()"><xsl:namespace name="p">http://p/</xsl:namespace></xsl:variable>
             <xsl:variable name="n2" as="node()"><xsl:namespace name="">http://d/</xsl:namespace></xsl:variable>
             <out count="{count(($n1, $n2))}" kinds="{string-join(for $n in ($n1, $n2) return string($n), '|')}"/>
           </xsl:template>"#,
        IN,
    )
    .expect("a parentless namespace node is a legal sequence item");
    assert_eq!(out, r#"<out count="2" kinds="http://p/|http://d/"/>"#);
}

/// The named half on its own, checked through `fn:name()` and the node's string
/// value rather than through `deep-equal` (which upstream does not yet
/// implement for namespace nodes — see the report).
#[test]
fn a_standalone_namespace_node_keeps_its_name_and_value() {
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="n" as="node()"><xsl:namespace name="p">http://p/</xsl:namespace></xsl:variable>
             <out name="{name($n)}" value="{string($n)}"/>
           </xsl:template>"#,
        IN,
    )
    .expect("a parentless namespace node keeps its name and value");
    assert_eq!(out, r#"<out name="p" value="http://p/"/>"#);
}

// ═══ xsl:document base URI in a sequence (§11.5) ═══════════════════════════

/// A document node built by `xsl:document` **inside a sequence** has the base
/// URI of the instruction that built it: §11.5's document node is the same
/// shape as §9.4's temporary tree.
#[test]
fn xsl_document_in_a_sequence_has_the_stylesheet_base_uri() {
    let out = run(
        r#"<xsl:template match="/">
             <xsl:variable name="d" as="document-node()"><xsl:document><e/></xsl:document></xsl:variable>
             <out base="{base-uri($d)}"/>
           </xsl:template>"#,
        IN,
    )
    .expect("xsl:document in a sequence");
    assert_eq!(out, r#"<out base="file:///namespaces/main.xsl"/>"#);
}

// ═══ a URI that is both alias source and alias target ═══════════════════════

/// §11.1.4's three sentences about the namespace nodes of a literal result
/// element:
///
/// > A namespace node whose string value is a **literal namespace URI** is not
/// > copied to the result tree.
/// >
/// > A namespace node whose string value is a **target namespace URI** is copied
/// > to the result tree, whether or not the URI identifies an excluded
/// > namespace.
/// >
/// > In the event that the same URI is used as a literal namespace URI and a
/// > target namespace URI, the second of these rules takes precedence.
///
/// In the chain `a → b`, `b → c` the URI bound to `b` is **both**, so the second
/// rule decides and its namespace node is copied; applying the first rule first
/// would drop it.
///
/// The literal result element here is in **no** namespace, so nothing but the
/// copy rules can put `b` on it — namespace fixup (§5.7.3) has no name to fix up.
#[test]
fn a_uri_that_is_both_alias_source_and_target_is_copied() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:a="http://a/" xmlns:b="http://b/" xmlns:c="http://c/">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b"/>
                 <xsl:namespace-alias stylesheet-prefix="b" result-prefix="c"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("a chain of aliases");
    // `a` is a literal URI only — rule 1 drops it. `b` is both — rule 2 wins.
    // `c` is a target only — rule 2 copies it.
    assert_eq!(out, r#"<out xmlns:b="http://b/" xmlns:c="http://c/"/>"#);
}

/// The same, read off the namespace **axis** of the constructed element rather
/// than off the serialization, and with the `a` binding *absent*: the node for
/// `b` can then only come from the copy rules, never from rewriting the `a` one.
#[test]
fn the_namespace_axis_of_the_constructed_element_carries_the_both_role_uri() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:b="http://b/" xmlns:c="http://c/">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b" xmlns:a="http://a/"/>
                 <xsl:namespace-alias stylesheet-prefix="b" result-prefix="c"/>
                 <xsl:template match="/">
                   <xsl:variable name="e" as="element()"><out/></xsl:variable>
                   <xsl:variable name="ns" select="$e/namespace::*[name() != 'xml']"/>
                   <xsl:value-of select="concat(
                     count($ns), ' ',
                     string($ns[name() = 'b']), ' ',
                     string($ns[name() = 'c']))"/>
                 </xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("a chain of aliases, read off the namespace axis");
    // Two namespace nodes besides the implicit `xml` one: the both-role URI
    // under its own result-prefix, and the target-only URI.
    assert_eq!(out, "2 http://b/ http://c/");
}

/// `namespace-alias-1006`: a circular chain of five aliases in which every URI
/// is both a literal and a target URI. Its prefix bindings sit on the
/// `xsl:namespace-alias` elements, so the **only** namespace node in scope on
/// each literal result element is the *default* one — and no alias has
/// `result-prefix="#default"`, so §11.1.4's note ("an in-scope namespace node
/// that binds **the `result-prefix`** to the target namespace URI, provided that
/// the namespace declaration associating this prefix with this URI is in scope
/// … for the literal result element") does not protect it. Rule 1 drops it, and
/// each result element carries only what namespace fixup gives its aliased name.
#[test]
fn a_circular_chain_copies_only_what_fixup_needs() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b" xmlns:a="http://a.com/" xmlns:b="http://b.com/"/>
                 <xsl:namespace-alias stylesheet-prefix="b" result-prefix="c" xmlns:b="http://b.com/" xmlns:c="http://c.com/"/>
                 <xsl:namespace-alias stylesheet-prefix="c" result-prefix="d" xmlns:c="http://c.com/" xmlns:d="http://d.com/"/>
                 <xsl:namespace-alias stylesheet-prefix="d" result-prefix="e" xmlns:d="http://d.com/" xmlns:e="http://e.com/"/>
                 <xsl:namespace-alias stylesheet-prefix="e" result-prefix="a" xmlns:e="http://e.com/" xmlns:a="http://a.com/"/>
                 <xsl:template match="/"><out>
                   <v xmlns="http://a.com/"/><w xmlns="http://b.com/"/><x xmlns="http://c.com/"/>
                   <y xmlns="http://d.com/"/><z xmlns="http://e.com/"/></out></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("a circular chain of aliases");
    assert_eq!(
        out,
        concat!(
            "<out>",
            r#"<b:v xmlns:b="http://b.com/"/>"#,
            r#"<c:w xmlns:c="http://c.com/"/>"#,
            r#"<d:x xmlns:d="http://d.com/"/>"#,
            r#"<e:y xmlns:e="http://e.com/"/>"#,
            r#"<a:z xmlns:a="http://a.com/"/>"#,
            "</out>"
        )
    );
}

/// The same circular chain with every `result-prefix` → target binding **in
/// scope on the literal result element**. §11.1.4's note then applies to all
/// five, so all five are copied: that is what the note guarantees, and the
/// difference from the case above is exactly the proviso it states.
#[test]
fn a_circular_chain_with_the_bindings_in_scope_copies_them_all() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:a="http://a.com/" xmlns:b="http://b.com/" xmlns:c="http://c.com/"
                 xmlns:d="http://d.com/" xmlns:e="http://e.com/">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b"/>
                 <xsl:namespace-alias stylesheet-prefix="b" result-prefix="c"/>
                 <xsl:namespace-alias stylesheet-prefix="c" result-prefix="d"/>
                 <xsl:namespace-alias stylesheet-prefix="d" result-prefix="e"/>
                 <xsl:namespace-alias stylesheet-prefix="e" result-prefix="a"/>
                 <xsl:template match="/"><a:v/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("a circular chain with every binding in scope");
    assert_eq!(
        out,
        concat!(
            r#"<b:v xmlns:a="http://a.com/" xmlns:b="http://b.com/" xmlns:c="http://c.com/" "#,
            r#"xmlns:d="http://d.com/" xmlns:e="http://e.com/"/>"#
        )
    );
}

/// Rule 2's own clause: a target namespace URI is copied "**whether or not** the
/// URI identifies an excluded namespace".
#[test]
fn a_target_namespace_survives_being_excluded() {
    let out = run_sheet(
        &format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:a="http://a/" xmlns:b="http://b/" exclude-result-prefixes="a b">
                 <xsl:namespace-alias stylesheet-prefix="a" result-prefix="b"/>
                 <xsl:template match="/"><out/></xsl:template>
               </xsl:stylesheet>"#
        ),
        IN,
    )
    .expect("an excluded target namespace");
    // `a` is excluded *and* a literal URI: gone either way. `b` is excluded but
    // is the target, so it is copied anyway.
    assert_eq!(out, r#"<out xmlns:b="http://b/"/>"#);
}

// ═══ what is in the lexical space of xs:anyURI ═════════════════════════════
//
// XTDE0835 / XTDE0865 / XTDE0905 all say "not in the lexical space of the
// xs:anyURI data type … or … the string http://www.w3.org/2000/xmlns/".
// XML Schema 1.1 Part 2 §3.3.17.2 makes that lexical space every finite-length
// sequence of XML `Char`s, and says outright that the RFC syntax "is not part of
// this datatype as defined here" — so a control-character test had no basis.

/// A tab and a U+0085 are XML `Char`s (`#x9`, and `#x85` is inside
/// `[#x20-#xD7FF]`), so a URI containing one is in the lexical space. Saxon and
/// the conformance baseline accept them; a `char::is_control` test would not.
#[test]
fn a_namespace_uri_may_contain_a_tab_or_u0085() {
    for (label, escape) in [("tab", "&#9;"), ("U+0085", "&#133;"), ("newline", "&#10;")] {
        let out = run(
            &format!(
                r#"<xsl:template match="/"><out><xsl:namespace name="p">urn:a{escape}b</xsl:namespace></out></xsl:template>"#
            ),
            IN,
        )
        .unwrap_or_else(|e| panic!("a {label} in a namespace URI is in the lexical space: {e}"));
        assert!(out.starts_with("<out xmlns:p=\"urn:a"), "{label}: {out}");
    }
    // The same through xsl:element's namespace attribute (XTDE0835).
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="e" namespace="urn:a&#9;b"/></xsl:template>"#,
        IN,
    )
    .expect("a tab in xsl:element/@namespace");
    assert!(out.contains("urn:a"), "{out}");
}

/// The two rules that do survive. The `#` one is XSD **1.0**'s reading of
/// `anyURI` (a URI reference per RFC 2396 has at most one fragment separator),
/// which is what the suite selects for a processor that is not XSD 1.1:
/// `error-0905a` feeds `####` to `xsl:namespace` and requires XTDE0905, under
/// `<feature value="XSD_1.1" satisfied="false"/>`.
#[test]
fn a_namespace_uri_with_two_hashes_or_the_xmlns_namespace_is_rejected() {
    let ns = |body: &str| {
        run(
            &format!(
                r#"<xsl:template match="/"><out><xsl:namespace name="p">{body}</xsl:namespace></out></xsl:template>"#
            ),
            IN,
        )
    };
    // `error-0905a`.
    assert_eq!(
        ns("####").expect_err("two fragment separators").code,
        "XTDE0905"
    );
    // One `#` is a perfectly good fragment separator.
    assert_eq!(
        ns("urn:a#b").expect("one fragment separator"),
        r#"<out xmlns:p="urn:a#b"/>"#
    );
    // `error-0905c`: the XMLNS namespace is named outright by the error.
    assert_eq!(
        ns("http://www.w3.org/2000/xmlns/")
            .expect_err("the XMLNS namespace")
            .code,
        "XTDE0905"
    );
    // XTDE0865 for xsl:attribute, on the same sentence.
    assert_eq!(
        run(
            r#"<xsl:template match="/"><out><xsl:attribute name="a" namespace="http://www.w3.org/2000/xmlns/">v</xsl:attribute></out></xsl:template>"#,
            IN,
        )
        .expect_err("the XMLNS namespace on xsl:attribute")
        .code,
        "XTDE0865"
    );
}

/// The empty string is a namespace URI wherever a namespace URI is optional:
/// §3.3.17.2's lexical space is "zero or more characters", and `xsl:element`
/// with `namespace=""` builds an element in no namespace.
#[test]
fn the_empty_namespace_uri_is_accepted() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="e" namespace=""><xsl:attribute name="a" namespace="">v</xsl:attribute></xsl:element></xsl:template>"#,
        IN,
    )
    .expect("an empty namespace URI puts the name in no namespace");
    assert_eq!(out, r#"<e a="v"/>"#);
}
