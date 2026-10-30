//! Keys (`xsl:key`, `key()`, §16.3) and `id()` / `key()` patterns (§5.5.2).
//!
//! Every test quotes the sentence of XSLT 2.0 it checks. The sections: the
//! declaration and its static errors, `key()` with two and three arguments and
//! its dynamic errors, the comparison rules, several documents and temporary
//! trees, circularity, the dynamic context of `match` / `use`, the patterns,
//! backwards-compatible behaviour, the places `key()` is called from, indexes by
//! typed value, and the stack guard of nested key builds.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use xslt2::collation::HTML_ASCII_CASE_INSENSITIVE_URI;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltError, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Wraps top-level content in a stylesheet of the given `version` with the
/// `xs` and `f` prefixes bound.
fn sheet_v(version: &str, body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="{version}" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{body}</xsl:stylesheet>"#
    )
}

fn sheet(body: &str) -> String {
    sheet_v("2.0", body)
}

const TEST_STACK: usize = 256 * 1024 * 1024;

/// Compiles `main` (with the extra resources `files`, URI → text, available to
/// `xsl:import` and `document()`) and runs it over `source`.
fn run_full(files: &[(&str, &str)], main: &str, source: &str) -> Result<String, XsltError> {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(u, t)| (u.to_string(), t.to_string()))
        .collect();
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let mut mem = MemoryResolver::new();
        for (uri, text) in &files {
            mem.insert(uri.as_str(), text.as_str());
        }
        let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
        let compiled = Compiler::new(&names, &functions)
            .with_resolver(Arc::clone(&resolver))
            .compile_str(&main, "file:///keys/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .with_resolver(resolver)
            .transform_source("file:///keys/in.xml", &source)?;
        compiled.serialize_primary(&outcome).map(common::body)
    })
}

fn run(main: &str, source: &str) -> Result<String, XsltError> {
    run_full(&[], main, source)
}

fn out(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => text,
        Err(e) => panic!("the transformation failed: {e}"),
    }
}

fn code_of(main: &str, source: &str) -> String {
    match run(main, source) {
        Ok(text) => panic!("expected an error, got {text:?}"),
        Err(e) => e.code.to_string(),
    }
}

const ITEMS: &str = r#"<doc>
  <item id="i1" code="a" n="5"/>
  <item id="i2" code="b" n="05"/>
  <item id="i3" code="a" n="7"/>
  <group><item id="i4" code="b c" n="5"/></group>
</doc>"#;

// ── §16.3.1 The xsl:key declaration ─────────────────────────────────────────

/// §16.3.1: "The `match` attribute is a Pattern; an `xsl:key` element applies
/// to all nodes that match the pattern specified in the `match` attribute."
/// §16.3.2: "The result of the function is a sequence of nodes, in document
/// order and with duplicates removed".
#[test]
fn a_key_finds_every_matching_node_in_document_order() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'a')/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>i1 i3</out>");
}

/// §16.3.1: "[ERR XTSE1205] It is a static error if an `xsl:key` declaration
/// has a `use` attribute and has non-empty content, or if it has empty content
/// and no `use` attribute."
#[test]
fn use_and_content_are_exclusive_and_one_is_required() {
    let both = sheet(
        r#"<xsl:key name="k" match="item" use="@code"><xsl:value-of select="@id"/></xsl:key>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&both, ITEMS), "XTSE1205");
    let neither = sheet(
        r#"<xsl:key name="k" match="item"/>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&neither, ITEMS), "XTSE1205");
}

/// §16.3.1: "Similarly, if a sequence constructor is present, it is used to
/// determine the values of the key. The sequence constructor will be evaluated
/// with the node that matches the pattern as the context node. The result of
/// evaluating the sequence constructor is atomized."
#[test]
fn a_sequence_constructor_gives_the_key_values() {
    let main = sheet(
        r#"<xsl:key name="k" match="item"><xsl:sequence select="concat(@code, '-', @n)"/></xsl:key>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'a-7')/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>i3</out>");
}

/// §3.2: "[ERR XTSE0080] It is a static error to use a reserved namespace in
/// the name of … a key".
#[test]
fn a_key_name_in_a_reserved_namespace_is_xtse0080() {
    let main = sheet(
        r#"<xsl:key name="xs:k" match="item" use="@code"/>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&main, ITEMS), "XTSE0080");
}

/// §16.3.1: "[ERR XTSE1210] It is a static error if the `xsl:key` declaration
/// has a `collation` attribute whose value (after resolving against the base
/// URI) is not a URI recognized by the implementation as referring to a
/// collation."
#[test]
fn an_unknown_collation_is_xtse1210() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code" collation="urn:no-such-collation"/>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(code_of(&main, ITEMS), "XTSE1210");
}

/// §16.3.1: "[ERR XTSE1220] It is a static error if there are several
/// `xsl:key` declarations in the stylesheet with the same key name and
/// different effective collations." — the effective collation of a
/// declaration without the attribute is "the default collation that is in
/// scope for the `xsl:key` declaration".
#[test]
fn declarations_of_one_key_must_agree_on_the_collation() {
    let explicit = sheet(&format!(
        r#"<xsl:key name="k" match="item" use="@code" collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"/>
           <xsl:key name="k" match="group" use="@id"/>
           <xsl:template match="/"><out/></xsl:template>"#
    ));
    assert_eq!(code_of(&explicit, ITEMS), "XTSE1220");
    // The same collation reached once explicitly and once as the default
    // collation in scope is the same effective collation.
    let agreeing = sheet(&format!(
        r#"<xsl:key name="k" match="item" use="@code" collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"/>
           <xsl:key name="k" match="group" use="@id" default-collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'A'))"/></out></xsl:template>"#
    ));
    assert_eq!(out(&agreeing, ITEMS), "<out>2</out>");
}

/// §16.3.1: "[Definition: A key is defined as a set of `xsl:key` declarations
/// in the stylesheet that share the same name.]" and "It is possible to have:
/// multiple `xsl:key` declarations with the same name; a node that matches the
/// match patterns of several different `xsl:key` declarations".
#[test]
fn several_declarations_of_one_name_are_unioned() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:key name="k" match="group" use="'a'"/>
           <xsl:key name="k" match="item[@n = '5']" use="'a'"/>
           <xsl:template match="/"><out><xsl:for-each select="key('k', 'a')"><xsl:value-of select="name()"/><xsl:value-of select="@id"/>;</xsl:for-each></out></xsl:template>"#,
    );
    // i1 matches twice (by code and by the third declaration) and appears once.
    assert_eq!(out(&main, ITEMS), "<out>itemi1;itemi3;group;itemi4;</out>");
}

/// §16.3.1: "An `xsl:key` declaration with higher import precedence does not
/// override another of lower import precedence; all the `xsl:key` declarations
/// in the stylesheet are effective regardless of their import precedence."
#[test]
fn import_precedence_does_not_matter_for_keys() {
    let imported = sheet(r#"<xsl:key name="k" match="item" use="@n"/>"#);
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:key name="k" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', ('7', 'b'))/@id"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_full(&[("file:///keys/imported.xsl", &imported)], &main, ITEMS).unwrap(),
        "<out>i2 i3</out>"
    );
}

// ── §16.3.2 The key function ────────────────────────────────────────────────

/// §16.3.2: "Note that the key specifier in general returns a sequence of
/// values, and any one of these may be used to locate the node" and "Each of
/// the resulting atomic values is considered as a requested key value."
#[test]
fn a_multi_valued_specifier_and_several_requested_values() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="tokenize(@code, ' ')"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'c')/@id"/>|<xsl:value-of select="key('k', ('c', 'b', 'c'))/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>i4|i2 i4</out>");
}

/// §16.3.2: "If the second argument is an empty sequence, the result of the
/// function will be an empty sequence."
#[test]
fn an_empty_second_argument_gives_the_empty_sequence() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', ()))"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>0</out>");
}

/// §16.3.2: "If the argument is present, the selected subtree is the set of
/// nodes that have `$top` as an ancestor-or-self node."
#[test]
fn the_third_argument_restricts_the_result_to_a_subtree() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@n"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', '5', /doc/group)/@id"/>|<xsl:value-of select="key('k', '5', //item[@id='i1'])/@id"/>|<xsl:value-of select="key('k', '5', /)/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>i4|i1|i1 i4</out>");
}

/// §16.3.2: "[ERR XTDE1260] It is a non-recoverable dynamic error if the value
/// is not a valid QName, or if there is no namespace declaration in scope for
/// the prefix of the QName, or if the name obtained by expanding the QName is
/// not the same as the expanded name of any `xsl:key` declaration in the
/// stylesheet."
#[test]
fn an_unknown_or_malformed_key_name_is_xtde1260() {
    for call in ["key('nokey', 'a')", "key('1bad', 'a')", "key('p:k', 'a')"] {
        let main = sheet(&format!(
            r#"<xsl:key name="k" match="item" use="@code"/>
               <xsl:template match="/"><out><xsl:value-of select="{call}"/></out></xsl:template>"#
        ));
        assert_eq!(code_of(&main, ITEMS), "XTDE1260", "{call}");
    }
    // A prefixed name expands through the namespaces in scope of the call.
    let main = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}" xmlns:p="urn:p" xmlns:q="urn:p"
             exclude-result-prefixes="p q">
           <xsl:key name="p:k" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('q:k', 'b')/@id"/></out></xsl:template>
         </xsl:stylesheet>"#
    );
    assert_eq!(out(&main, ITEMS), "<out>i2</out>");
}

/// §16.3.2: "[ERR XTDE1270] It is a non-recoverable dynamic error to call the
/// key function with two arguments if there is no context node, or if the root
/// of the tree containing the context node is not a document node; or to call
/// the function with three arguments if the root of the tree containing the
/// node supplied in the third argument is not a document node."
#[test]
fn no_document_to_search_is_xtde1270() {
    // A stylesheet function has no focus (§10.3), so a two-argument call in
    // its body has no context node.
    let in_function = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:function name="f:f"><xsl:sequence select="key('k', 'a')"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="count(f:f())"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&in_function, ITEMS), "XTDE1270");
    // A parentless element: the root of its tree is not a document node.
    let parentless = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:variable name="e" as="element()"><e><item code="a"/></e></xsl:variable>
           <xsl:template match="/"><out><xsl:value-of select="count($e/key('k', 'a'))"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&parentless, ITEMS), "XTDE1270");
    let third = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:variable name="e" as="element()"><e><item code="a"/></e></xsl:variable>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'a', $e))"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&third, ITEMS), "XTDE1270");
    // With a third argument a function body may search any document.
    let with_top = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:function name="f:f"><xsl:param name="top"/><xsl:sequence select="key('k', 'a', $top)"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:f(/)/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&with_top, ITEMS), "<out>i1 i3</out>");
}

// ── The comparison ──────────────────────────────────────────────────────────

/// §16.3.2: "… a value that is equal to one of these requested key values,
/// under the rules appropriate to the XPath `eq` operator for the two values
/// in question … No error is reported if two values are encountered that are
/// not comparable; they are regarded for the purposes of this function as
/// being not equal." So a key value of type `xs:integer` matches
/// `key('k', 5)` but not `key('k', '5')`.
#[test]
fn typed_key_values_compare_by_eq() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="xs:integer(@n)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 5)/@id"/>|<xsl:value-of select="count(key('k', '5'))"/>|<xsl:value-of select="key('k', 5.0e0)/@id"/>|<xsl:value-of select="key('k', 7.0)/@id"/></out></xsl:template>"#,
    );
    // "05" and "5" are the same integer.
    assert_eq!(out(&main, ITEMS), "<out>i1 i2 i4|0|i1 i2 i4|i3</out>");
}

/// §16.3.2 Note: "Under the rules for the `eq` operator, untyped atomic values
/// are converted to strings, not to the type of the other operand."
#[test]
fn untyped_key_values_compare_as_strings() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@n"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 5))"/>|<xsl:value-of select="key('k', '5')/@id"/>|<xsl:value-of select="key('k', //item[@id='i3']/@n)/@id"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>0|i1 i4|i3</out>");
}

/// §16.3.1: "two values `$a` and `$b` are considered equal if the result of
/// the function call `compare($a, $b, $collation)` is zero."
#[test]
fn strings_compare_under_the_key_collation() {
    let main = sheet(&format!(
        r#"<xsl:key name="k" match="item" use="@code" collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"/>
           <xsl:key name="plain" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'A')/@id"/>|<xsl:value-of select="count(key('plain', 'A'))"/></out></xsl:template>"#
    ));
    assert_eq!(out(&main, ITEMS), "<out>i1 i3|0</out>");
    // The default collation in scope at the declaration is its effective one.
    let defaulted = sheet(&format!(
        r#"<xsl:key name="k" match="item" use="@code" default-collation="{HTML_ASCII_CASE_INSENSITIVE_URI}"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'B')/@id"/></out></xsl:template>"#
    ));
    assert_eq!(out(&defaulted, ITEMS), "<out>i2</out>");
}

/// §16.3.2: "the atomized value … includes a value that compares equal … under
/// the rules of the `eq` operator" — for every type, not only strings and
/// numbers: dates compare as points in time (XPath 2.0 `op:date-equal`), and
/// `NaN` equals nothing.
#[test]
fn values_of_other_types_compare_by_eq() {
    let main = sheet(
        r#"<xsl:key name="d" match="e" use="xs:date(@d)"/>
           <xsl:key name="b" match="e" use="@d = '2002-01-01Z'"/>
           <xsl:key name="n" match="e" use="number(@n)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('d', xs:date('2002-01-01+00:00'))/@id"/>|<xsl:value-of select="key('b', true())/@id"/>|<xsl:value-of select="count(key('n', number('NaN')))"/>|<xsl:value-of select="key('n', 1)/@id"/></out></xsl:template>"#,
    );
    let source =
        r#"<doc><e id="e1" d="2002-01-01Z" n="x"/><e id="e2" d="2002-01-02Z" n="1"/></doc>"#;
    assert_eq!(out(&main, source), "<out>e1|e1|0|e2</out>");
}

/// What a key can match: any node a pattern can match, the document node and
/// attributes included (§16.3.1: "an `xsl:key` element applies to all nodes
/// that match the pattern specified in the `match` attribute").
#[test]
fn keys_index_every_kind_of_node() {
    let main = sheet(
        r#"<xsl:key name="root" match="/" use="'r'"/>
           <xsl:key name="attr" match="@code" use="."/>
           <xsl:key name="text" match="text()" use="."/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('root', 'r')/self::document-node())"/>|<xsl:value-of select="key('attr', 'b')/../@id"/>|<xsl:value-of select="name(key('text', 'hello')/..)"/></out></xsl:template>"#,
    );
    let source = r#"<doc><item id="i1" code="a"/><item id="i2" code="b"/><p>hello</p></doc>"#;
    assert_eq!(out(&main, source), "<out>1|i2|p</out>");
}

/// §5.5.4 applies to a key's `match` pattern like any other: "Any dynamic error
/// or type error that occurs during the evaluation of a pattern against a
/// particular node is treated as a recoverable error … The optional recovery
/// action is to treat the pattern as not matching that node."
#[test]
fn an_error_in_a_key_pattern_is_recovered() {
    let main = sheet(
        r#"<xsl:key name="k" match="item[xs:integer(@n) gt 4]" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', ('a', 'b'))/@id"/></out></xsl:template>"#,
    );
    // One item per parent: the predicate of a pattern step is evaluated over the
    // step's siblings (§5.5.3's `child-or-top::item[…]`), so an error raised
    // for one sibling is an error in matching each of them.
    let source = r#"<doc><g><item id="i1" code="a" n="5"/></g><g><item id="i2" code="b" n="x"/></g><g><item id="i3" code="b" n="9"/></g></doc>"#;
    assert_eq!(out(&main, source), "<out>i1 i3</out>");
}

// ── Several documents and temporary trees ───────────────────────────────────

/// §16.3.1 Note: "Keys can be used to locate nodes within any source document
/// (including temporary trees), but each use of the key function searches one
/// document only." — the per-document index.
#[test]
fn each_document_has_its_own_index() {
    let other = r#"<doc><item id="o1" code="a"/><item id="o2" code="z"/></doc>"#;
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:variable name="tree"><doc><item id="t1" code="a"/></doc></xsl:variable>
           <xsl:template match="/">
             <out>
               <xsl:value-of select="key('k', 'a')/@id"/>|<xsl:value-of select="document('other.xml')/key('k', 'a')/@id"/>|<xsl:value-of select="key('k', 'a', document('other.xml'))/@id"/>|<xsl:value-of select="key('k', 'a', $tree)/@id"/>|<xsl:value-of select="$tree/key('k', 'a')/@id"/>|<xsl:value-of select="count(key('k', 'z'))"/>
             </out>
           </xsl:template>"#,
    );
    assert_eq!(
        run_full(&[("file:///keys/other.xml", other)], &main, ITEMS).unwrap(),
        "<out>i1 i3|o1|o1|t1|t1|0</out>"
    );
}

// ── §9.8 Circularity ────────────────────────────────────────────────────────

/// §9.8: "[Definition: A circularity is said to exist if a construct such as a
/// global variable, an attribute set, or a key is defined in terms of itself.]"
/// and "[ERR XTDE0640] In general, a circularity in a stylesheet is a
/// non-recoverable dynamic error."
#[test]
fn a_key_defined_in_terms_of_itself_is_xtde0640() {
    let through_use = sheet(
        r#"<xsl:key name="k" match="item" use="key('k', @code)/@id"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'a')"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&through_use, ITEMS), "XTDE0640");
    // Two keys defined in terms of each other.
    let mutual = sheet(
        r#"<xsl:key name="k1" match="item" use="key('k2', @code)"/>
           <xsl:key name="k2" match="item" use="key('k1', @code)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k1', 'a')"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&mutual, ITEMS), "XTDE0640");
    // Through the match pattern. §5.5.4 makes an error in a pattern
    // recoverable, but §2.9: "It is implementation-defined whether the error
    // is signaled or the recovery action is taken" — a circularity is
    // signalled.
    let through_match = sheet(
        r#"<xsl:key name="k" match="key('k', '12')" use="@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'a')"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&through_match, ITEMS), "XTDE0640");
    // Through a global variable (the §9.8 example "Circularity involving
    // Variables and Keys").
    let through_variable = sheet(
        r#"<xsl:key name="k" match="item[@code = $x]" use="@id"/>
           <xsl:variable name="x" select="string(key('k', 'i1')/@code)"/>
           <xsl:template match="/"><out><xsl:value-of select="$x"/></out></xsl:template>"#,
    );
    assert_eq!(code_of(&through_variable, ITEMS), "XTDE0640");
}

/// A key may use *another* key over the same document: that is not a
/// circularity.
#[test]
fn a_key_may_use_another_key() {
    let main = sheet(
        r#"<xsl:key name="byid" match="item" use="@id"/>
           <xsl:key name="k" match="ref" use="key('byid', @to)/@code"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'b')/@n"/></out></xsl:template>"#,
    );
    let source = r#"<doc><item id="i1" code="a"/><item id="i2" code="b"/><ref to="i2" n="r1"/><ref to="i1" n="r2"/></doc>"#;
    assert_eq!(out(&main, source), "<out>r1</out>");
}

// ── The dynamic context of match and use ────────────────────────────────────

/// §16.6.1: "If the current function is used within a pattern, its value is
/// the node that is being matched against the pattern." and "For an outermost
/// expression … the current item is always the same as the context item."
#[test]
fn current_inside_match_and_use() {
    let main = sheet(
        r#"<xsl:key name="k" match="item[current()/@code = 'a']" use="current()/@n"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', '7')/@id"/>|<xsl:value-of select="count(key('k', '05'))"/></out></xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>i3|0</out>");
}

/// §16.3.1: "The sequence constructor will be evaluated with the node that
/// matches the pattern as the context node" — a singleton focus (§16.3.2:
/// "When the key specifier of that xsl:key declaration is evaluated with a
/// singleton focus based on $N"); and §6.5: "While evaluating global
/// variables and parameters, and the sequence constructor contained in
/// `xsl:key` or `xsl:sort`, the current mode is set to the default mode."
#[test]
fn the_specifier_has_a_singleton_focus_and_the_default_mode() {
    let main = sheet(
        r##"<xsl:key name="k" match="item"><xsl:sequence select="concat(position(), last())"/><xsl:apply-templates select="@code" mode="#current"/></xsl:key>
           <xsl:template match="@code"><xsl:sequence select="concat('D', .)"/></xsl:template>
           <xsl:template match="@code" mode="m"><xsl:sequence select="concat('M', .)"/></xsl:template>
           <xsl:template match="/"><xsl:apply-templates select="." mode="m"/></xsl:template>
           <xsl:template match="/" mode="m"><out><xsl:value-of select="key('k', '11')/@id"/>|<xsl:value-of select="key('k', 'Da')/@id"/>|<xsl:value-of select="count(key('k', 'Ma'))"/></out></xsl:template>"##,
    );
    assert_eq!(out(&main, ITEMS), "<out>i1 i2 i3 i4|i1 i3|0</out>");
}

/// §6.7: "when evaluating a sequence constructor contained in an xsl:sort or
/// xsl:key element … the current template rule becomes null"; and "[ERR
/// XTDE0560] It is a non-recoverable dynamic error if `xsl:apply-imports` or
/// `xsl:next-match` is evaluated when the current template rule is null." —
/// even when `key()` is called from inside a template rule.
#[test]
fn the_specifier_has_no_current_template_rule() {
    for instruction in ["<xsl:apply-imports/>", "<xsl:next-match/>"] {
        let main = sheet(&format!(
            r#"<xsl:key name="k" match="item">{instruction}<xsl:sequence select="string(@code)"/></xsl:key>
               <xsl:template match="/"><out><xsl:value-of select="key('k', 'a')/@id"/></out></xsl:template>"#
        ));
        assert_eq!(code_of(&main, ITEMS), "XTDE0560", "{instruction}");
    }
}

// ── §5.5.2 id() and key() patterns ──────────────────────────────────────────

/// §5.5.2: "Patterns may start with an id or key function call, provided that
/// the value to be matched is supplied as either a literal or a reference to a
/// variable or parameter, and the key name (in the case of the key function)
/// is supplied as a string literal."
#[test]
fn key_patterns_with_continuations() {
    let main = sheet(
        r#"<xsl:key name="k" match="item|group" use="@code"/>
           <xsl:variable name="v" select="'b'"/>
           <xsl:template match="/"><out><xsl:apply-templates select="//item|//group"/></out></xsl:template>
           <xsl:template match="key('k', 'a')">[A<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="key('k', $v)">[V<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="key('k', 'g')/item">[G/<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="key('k', 'h')//item">[H//<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="item|group">[-<xsl:value-of select="@id"/>]</xsl:template>"#,
    );
    let source = r#"<doc>
      <item id="i1" code="a"/><item id="i2" code="b"/>
      <group code="g" id="g1"><item id="i3"/></group>
      <group code="h" id="h1"><x><item id="i4"/></x></group>
    </doc>"#;
    assert_eq!(
        out(&main, source),
        "<out>[Ai1][Vi2][-g1][G/i3][-h1][H//i4]</out>"
    );
}

/// §5.5.2: an `id()` pattern, with a literal, a variable, and `/` / `//`
/// continuations.
#[test]
fn id_patterns_with_continuations() {
    let main = sheet(
        r#"<xsl:variable name="v" select="'b'"/>
           <xsl:template match="/"><out><xsl:apply-templates select="//*"/></out></xsl:template>
           <xsl:template match="id('a')">[A<xsl:value-of select="name()"/>]</xsl:template>
           <xsl:template match="id($v)">[V<xsl:value-of select="name()"/>]</xsl:template>
           <xsl:template match="id('c')/p">[C/<xsl:value-of select="name()"/>]</xsl:template>
           <xsl:template match="id('c')//q">[C//<xsl:value-of select="name()"/>]</xsl:template>
           <xsl:template match="*">[-<xsl:value-of select="name()"/>]</xsl:template>"#,
    );
    let source = r#"<doc><x xml:id="a"/><y xml:id="b"/><z xml:id="c"><p/><r><q/></r></z></doc>"#;
    assert_eq!(
        out(&main, source),
        "<out>[-doc][Ax][Vy][-z][C/p][-r][C//q]</out>"
    );
}

/// §5.5.2: `IdKeyPattern (('/' | '//') RelativePathPattern)?` with several
/// steps — the shape of the suite's `id-034` ("Try multiple child steps after
/// id() in match patterns"), whose source declares its ID attribute in a DTD
/// (an upstream gap); here the ID is an `xml:id`. §5.5.3 reads the pattern
/// right to left: `id("abc")/l1/v` matches a `v` whose parent is an `l1` whose
/// parent is the element with ID `abc`.
#[test]
fn an_id_pattern_with_several_steps() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
           <xsl:template match='id("abc")/l1/v'>[l1/v:<xsl:value-of select="."/>]</xsl:template>
           <xsl:template match='id("abc")/l2/v'>[l2/v:<xsl:value-of select="."/>]</xsl:template>
           <xsl:template match='id("abc")/l1/l2/w'>[l1/l2/w:<xsl:value-of select="."/>]</xsl:template>
           <xsl:template match='id("abc")/l3/v'>[l3/v:<xsl:value-of select="."/>]</xsl:template>
           <xsl:template match="v">[v:<xsl:value-of select="."/>]</xsl:template>
           <xsl:template match="text()"/>"#,
    );
    let source =
        r#"<doc xml:id="abc"><l1><v>1</v><l2><v>2</v><l3><v>3</v></l3><w>W</w></l2></l1></doc>"#;
    assert_eq!(
        out(&main, source),
        "<out>[l1/v:1][v:2][v:3][l1/l2/w:W]</out>"
    );
}

/// §6.4: an `IdKeyPattern` falls under "Otherwise, the priority is 0.5" — it
/// beats a plain name test (priority 0).
#[test]
fn an_id_key_pattern_has_priority_one_half() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="key('k', 'a')">[K<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="item">[-<xsl:value-of select="@id"/>]</xsl:template>"#,
    );
    let source = r#"<doc><item id="i1" code="a"/><item id="i2" code="b"/></doc>"#;
    assert_eq!(out(&main, source), "<out>[Ki1][-i2]</out>");
}

/// §5.5.2 restricts the arguments: "`IdValue ::= StringLiteral | VarRef`",
/// "`KeyValue ::= Literal | VarRef`", and the key name is a `StringLiteral`
/// (XTSE0340 for anything else, §5.5.2: "it is a static error if the pattern
/// does not match the production Pattern").
#[test]
fn id_key_pattern_arguments_are_restricted() {
    for pattern in [
        "id('a', $x)",
        "id(@ref)",
        "id(1)",
        "key('k', 1+1)",
        "key($name, 'a')",
        "key('k', 'a')[1]",
        "key('k')",
    ] {
        let main = sheet(&format!(
            r#"<xsl:key name="k" match="item" use="@code"/>
               <xsl:variable name="x" select="1"/><xsl:variable name="name" select="'k'"/>
               <xsl:template match="{pattern}"/>
               <xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_eq!(code_of(&main, ITEMS), "XTSE0340", "{pattern}");
    }
    // A numeric literal is a Literal, so `key('k', 5)` is a pattern.
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="xs:integer(@n)"/>
           <xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="key('k', 5)">[<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="item"/>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>[i1][i2][i4]</out>");
}

/// §5.5.2: "These patterns will never match a node in a tree whose root is not
/// a document node." §5.5.4: "for a PathPattern that starts with / or // or
/// with an IdKeyPattern, the result of testing this pattern against a node in
/// a tree whose root is not a document node must be a non-match, rather than a
/// dynamic error."
#[test]
fn id_key_patterns_never_match_in_a_parentless_tree() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code"/>
           <xsl:variable name="e" as="element()"><e><item code="a" xml:id="a"/></e></xsl:variable>
           <xsl:template match="/"><out><xsl:apply-templates select="$e/item"/></out></xsl:template>
           <xsl:template match="key('k', 'a')">K</xsl:template>
           <xsl:template match="id('a')">I</xsl:template>
           <xsl:template match="item">-</xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>-</out>");
}

// ── Backwards-compatible behaviour ──────────────────────────────────────────

/// §16.3.2: "A key … is processed in backwards compatible mode if any of the
/// `xsl:key` elements in the definition of the key enables backwards
/// compatible behavior. When a key is processed in backwards compatible mode,
/// then: The result of evaluating the key specifier … is converted after
/// atomization to a sequence of strings … the value of the second argument is
/// converted after atomization to a sequence of strings … The values are then
/// compared as strings." (J.1.3: "The XSLT 1.0 behavior is retained if any of
/// the `xsl:key` elements making up the key definition enables
/// backwards-compatible behavior.")
#[test]
fn a_backwards_compatible_key_compares_strings() {
    let body = r#"<xsl:key name="k" match="item" use="xs:integer(@n)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', '5')/@id"/>|<xsl:value-of select="count(key('k', 5))"/></out></xsl:template>"#;
    // XSLT 2.0: an integer key value does not equal the string '5'.
    assert_eq!(out(&sheet(body), ITEMS), "<out>|3</out>");
    // One compatible declaration makes the whole key string-compared: the
    // integer 5 becomes "5" (i1, i4; "05" cast to an integer and back to a
    // string is "5" too, so i2 as well), and the requested integer 5 becomes
    // "5".
    let mixed = sheet(
        r#"<xsl:key name="k" match="item" use="xs:integer(@n)"/>
           <xsl:key name="k" match="nothing" use="." version="1.0"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', '5')/@id"/>|<xsl:value-of select="count(key('k', 5))"/></out></xsl:template>"#,
    );
    assert_eq!(out(&mixed, ITEMS), "<out>i1 i2 i4|3</out>");
    // Untyped values stay distinct as strings: "05" is not "5".
    let untyped = sheet_v(
        "1.0",
        r#"<xsl:key name="k" match="item" use="@n"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 5))"/></out></xsl:template>"#,
    );
    assert_eq!(out(&untyped, ITEMS), "<out>2</out>");
}

// ── key() from every XPath site ─────────────────────────────────────────────

/// `key()` is an XPath function like any other (§16.3.2): it is reachable from
/// a pattern predicate, an attribute value template, a sort key and a
/// stylesheet function given a third argument.
#[test]
fn key_is_callable_from_every_xpath_site() {
    let main = sheet(
        r#"<xsl:key name="byid" match="item" use="@id"/>
           <xsl:template match="/">
             <out a="{key('byid', 'i3')/@code}">
               <xsl:apply-templates select="//ref">
                 <xsl:sort select="key('byid', @to)/@n" data-type="number"/>
               </xsl:apply-templates>
               <xsl:value-of select="f:code(/, 'i2')"/>
             </out>
           </xsl:template>
           <xsl:template match="ref[key('byid', @to)/@code = 'a']">A<xsl:value-of select="@to"/>;</xsl:template>
           <xsl:template match="ref">-<xsl:value-of select="@to"/>;</xsl:template>
           <xsl:function name="f:code"><xsl:param name="doc"/><xsl:param name="id"/><xsl:sequence select="string(key('byid', $id, $doc)/@code)"/></xsl:function>"#,
    );
    let source = r#"<doc><item id="i1" code="a" n="3"/><item id="i2" code="b" n="1"/><item id="i3" code="a" n="2"/>
      <ref to="i1"/><ref to="i2"/><ref to="i3"/></doc>"#;
    assert_eq!(out(&main, source), r#"<out a="a">-i2;Ai3;Ai1;b</out>"#);
}

// ── Indexes by typed value, failures of an index build ──────────────────────

/// Like [`run`], but returns the `xsl:message` texts as well as the output.
fn run_with_messages(main: &str, source: &str) -> Result<(String, Vec<String>), XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///keys/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///keys/in.xml", &source)?;
        let messages = outcome.messages.iter().map(|m| m.content.clone()).collect();
        Ok((
            compiled.serialize_primary(&outcome).map(common::body)?,
            messages,
        ))
    })
}

/// `n` sibling `item`s; item `i` has `xml:id="i{i}"`, `@code="c{i}"`,
/// `@type` `x` for even and `y` for odd `i`, and a distinct date `@d`.
fn many_items(n: usize) -> String {
    let mut s = String::with_capacity(n * 80 + 16);
    s.push_str("<doc>");
    for i in 0..n {
        let (year, month, day) = (1000 + i / 336, (i / 28) % 12 + 1, i % 28 + 1);
        let kind = if i % 2 == 0 { "x" } else { "y" };
        s.push_str(&format!(
            r#"<item xml:id="i{i}" code="c{i}" type="{kind}" d="{year:04}-{month:02}-{day:02}"/>"#
        ));
    }
    s.push_str("</doc>");
    s
}

/// Asserts that running `main(n)` over [`many_items`]`(n)` is linear in `n`,
/// from `n` to `8 n` (`common::scaling`), the result at each size checked by
/// `expected`.
fn assert_linear_in_items(
    what: &str,
    n: usize,
    main: impl Fn(usize) -> String,
    expected: impl Fn(usize) -> String,
) {
    common::scaling::assert_linear(what, n, 8, |n| {
        let (main, source) = (main(n), many_items(n));
        let mut result = String::new();
        let elapsed = common::scaling::timed(|| result = out(&main, &source));
        assert_eq!(result, expected(n), "{what}");
        elapsed
    });
}

/// §16.3.1: the key "provides a hint to the implementation that access to
/// the nodes by means of these values needs to be efficient (many
/// implementations are likely to construct an index or hash table to achieve
/// this)" — and §16.3.2's Note uses a date-valued key as its example. One
/// `key()` call per node with that node's own date: linear, not quadratic.
/// (With every date in one bucket, 10,000 lookups took 94 s in a debug
/// build — 13 s in a release build; hashed, 0.3 s in debug.)
#[test]
fn lookups_in_a_date_key_use_the_index() {
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="xs:date(@d)"/>
           <xsl:template match="/"><out><xsl:value-of select="count(for $i in //item return key('k', xs:date($i/@d)))"/></out></xsl:template>"#,
    );
    assert_linear_in_items(
        "date-key lookups",
        1_250,
        |_| main.clone(),
        |n| format!("<out>{n}</out>"),
    );
}

/// The other types whose values are hashed by what `eq` compares: QNames,
/// durations, binary values and the `xs:g*` types, each with one lookup per
/// node. (With one shared bucket, 10,000 lookups took 34 s in a debug build;
/// hashed, 1.4 s.)
#[test]
fn lookups_in_keys_of_other_types_use_the_index() {
    let main = sheet(
        r#"<xsl:key name="q" match="item" use="QName('urn:q', string(@code))"/>
           <xsl:key name="dur" match="item" use="xs:dayTimeDuration(concat('PT', substring(@code, 2), 'S'))"/>
           <xsl:key name="bin" match="item" use="xs:hexBinary(string-join(for $c in string-to-codepoints(@code) return concat(substring('0123456789ABCDEF', $c idiv 16 + 1, 1), substring('0123456789ABCDEF', $c mod 16 + 1, 1)), ''))"/>
           <xsl:key name="g" match="item" use="xs:gYearMonth(substring(@d, 1, 7))"/>
           <xsl:template match="/"><out><xsl:value-of select="count(for $i in //item return key('q', QName('urn:q', string($i/@code))))"/>|<xsl:value-of select="count(for $i in //item return key('dur', xs:dayTimeDuration(concat('PT', substring($i/@code, 2), 'S'))))"/>|<xsl:value-of select="count(key('bin', xs:hexBinary('6337')))"/>|<xsl:value-of select="count(for $i in //item[position() le 100] return key('g', xs:gYearMonth(substring($i/@d, 1, 7))))"/></out></xsl:template>"#,
    );
    // Each gYearMonth is shared by the 28 items of one month; the first 100
    // items fall in four months (28 + 28 + 28 + 16 of them), each lookup finding
    // its month's 28 items.
    assert_linear_in_items(
        "QName and duration key lookups",
        1_250,
        |_| main.clone(),
        |n| format!("<out>{n}|{n}|1|2800</out>"),
    );
}

/// dates, times and dateTimes written in different timezones are one key
/// value when `eq` equates them (XPath 2.0 §B.2 maps `A eq B` on two `xs:date`,
/// `xs:time`, `xs:dateTime` values to `op:date-equal`, `op:time-equal`,
/// `op:dateTime-equal`).
#[test]
fn equal_instants_in_different_timezones_match() {
    let main = sheet(
        r#"<xsl:key name="dt" match="e" use="xs:dateTime(@dt)"/>
           <xsl:key name="d" match="e" use="xs:date(@d)"/>
           <xsl:key name="t" match="e" use="xs:time(@t)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('dt', xs:dateTime('2002-01-01T08:00:00Z'))/@id"/>|<xsl:value-of select="key('d', xs:date('2001-12-31-10:00'))/@id"/>|<xsl:value-of select="key('t', xs:time('02:00:00+02:00'))/@id"/>|<xsl:value-of select="count(key('dt', xs:dateTime('2002-01-01T10:00:00Z')))"/>|<xsl:value-of select="count(key('t', xs:time('23:30:00-00:30')))"/>=<xsl:value-of select="xs:time('23:30:00-00:30') eq xs:time('00:00:00Z')"/></out></xsl:template>"#,
    );
    let source = r#"<doc>
      <e id="e1" dt="2002-01-01T10:00:00+02:00" d="2002-01-01+14:00" t="00:00:00Z"/>
      <e id="e2" dt="2002-01-01T09:00:00+01:00" d="2002-01-01Z" t="00:00:00+00:00"/>
      <e id="e3" dt="2002-01-01T10:00:00.000+02:00" d="2001-12-31-10:00" t="01:00:00+01:00"/>
    </doc>"#;
    // The last pair: 23:30-00:30 is midnight UTC of the *next* day, which `eq`
    // on two times does not equate with 00:00Z — and neither does the key.
    assert_eq!(
        out(&main, source),
        "<out>e1 e2 e3|e1 e3|e1 e2 e3|0|0=false</out>"
    );
}

/// a value without a timezone equals one with a timezone exactly when
/// `eq` says so — normalized with the implicit timezone (XPath 2.0 §2.1.2:
/// "This is the timezone to be used when a date, time, or dateTime value that
/// does not have a timezone is used in a comparison or arithmetic
/// operation."). The expectation is computed with `eq`
/// itself, in whatever implicit timezone the test runs; `adjust-*-to-timezone`
/// with one argument attaches the implicit timezone, so those pairs are equal
/// in every timezone, in both directions.
#[test]
fn values_without_a_timezone_match_exactly_as_eq_says() {
    let main = sheet(
        r#"<xsl:key name="dt" match="e" use="xs:dateTime(@dt)"/>
           <xsl:key name="d" match="e" use="xs:date(@d)"/>
           <xsl:key name="t" match="e" use="xs:time(@t)"/>
           <xsl:template match="/">
             <xsl:variable name="dts" select="for $v in //e/@dt return xs:dateTime($v)"/>
             <xsl:variable name="ds" select="for $v in //e/@d return xs:date($v)"/>
             <xsl:variable name="ts" select="for $v in //e/@t return xs:time($v)"/>
             <xsl:variable name="probes-dt" select="($dts, for $v in $dts return adjust-dateTime-to-timezone($v), for $v in $dts return adjust-dateTime-to-timezone($v, ()))"/>
             <xsl:variable name="probes-d" select="($ds, for $v in $ds return adjust-date-to-timezone($v), for $v in $ds return adjust-date-to-timezone($v, ()))"/>
             <xsl:variable name="probes-t" select="($ts, for $v in $ts return adjust-time-to-timezone($v), for $v in $ts return adjust-time-to-timezone($v, ()))"/>
             <out>
               <xsl:value-of select="every $p in $probes-dt satisfies (key('dt', $p) is-same //e[xs:dateTime(@dt) eq $p])"/>
             </out>
           </xsl:template>"#,
    );
    // `is-same` is not XPath; spell the comparison out instead.
    let main = main.replace(
        "(key('dt', $p) is-same //e[xs:dateTime(@dt) eq $p])",
        "(count(key('dt', $p)) eq count(//e[xs:dateTime(@dt) eq $p]) and empty(key('dt', $p) except //e[xs:dateTime(@dt) eq $p]))",
    );
    let main = main.replace(
        "</out>",
        r#"|<xsl:value-of select="every $p in $probes-d satisfies (count(key('d', $p)) eq count(//e[xs:date(@d) eq $p]) and empty(key('d', $p) except //e[xs:date(@d) eq $p]))"/>|<xsl:value-of select="every $p in $probes-t satisfies (count(key('t', $p)) eq count(//e[xs:time(@t) eq $p]) and empty(key('t', $p) except //e[xs:time(@t) eq $p]))"/>|<xsl:value-of select="count(key('dt', adjust-dateTime-to-timezone(xs:dateTime('2002-03-04T05:06:07'))))"/>|<xsl:value-of select="count(key('dt', adjust-dateTime-to-timezone(xs:dateTime('2002-03-04T05:06:07Z'), ())))"/></out>"#,
    );
    let source = r#"<doc>
      <e dt="2002-03-04T05:06:07" d="2002-03-04" t="05:06:07"/>
      <e dt="2002-03-04T05:06:07Z" d="2002-03-04Z" t="05:06:07Z"/>
      <e dt="2002-03-04T07:06:07+02:00" d="2002-03-05+14:00" t="07:06:07+02:00"/>
      <e dt="2002-03-03T23:36:07-05:30" d="2002-03-03-10:00" t="23:36:07-05:30"/>
      <e dt="2002-03-04T05:06:07.5" d="1999-12-31" t="00:00:00"/>
    </doc>"#;
    // The last two counts: the timezone-less 05:06:07 found from its
    // implicit-timezone twin, and the zoned 05:06:07Z found from its
    // timezone-less local reading — one row each in every timezone, plus the
    // rows `eq` makes equal to them there (computed, not assumed: see `every`).
    let result = out(&main, source);
    let parts: Vec<&str> = result
        .trim_start_matches("<out>")
        .trim_end_matches("</out>")
        .split('|')
        .map(str::trim)
        .collect();
    assert_eq!(&parts[..3], ["true", "true", "true"], "{result}");
    assert!(parts[3].parse::<u32>().unwrap() >= 1, "{result}");
    assert!(parts[4].parse::<u32>().unwrap() >= 1, "{result}");
}

/// durations compare by (months, seconds) whatever their type and
/// spelling; QNames by namespace URI and local name, not prefix; binary
/// values by their octets, never across the two binary types.
#[test]
fn typed_values_are_hashed_on_what_eq_compares() {
    let main = sheet(
        r#"<xsl:key name="dur" match="e" use="xs:duration(@dur)"/>
           <xsl:key name="q" match="e" use="resolve-QName(string(@q), .)"/>
           <xsl:key name="hex" match="e" use="xs:hexBinary(@hex)"/>
           <xsl:key name="g" match="e" use="xs:gDay(@g)"/>
           <xsl:template match="/"><out><xsl:value-of select="key('dur', xs:yearMonthDuration('P1Y'))/@id"/>|<xsl:value-of select="key('dur', xs:dayTimeDuration('PT3600S'))/@id"/>|<xsl:value-of select="key('dur', xs:dayTimeDuration('PT0S'))/@id"/>|<xsl:value-of select="key('dur', xs:dayTimeDuration('PT1.50S'))/@id"/>|<xsl:value-of select="key('q', QName('urn:a', 'z:local'))/@id"/>|<xsl:value-of select="key('hex', xs:hexBinary('0a1b'))/@id"/>|<xsl:value-of select="count(key('hex', xs:base64Binary('Chs=')))"/>|<xsl:value-of select="key('g', xs:gDay('---05'))/@id"/></out></xsl:template>"#,
    );
    let source = r#"<doc xmlns:p="urn:a" xmlns:r="urn:a">
      <e id="e1" dur="P12M" q="p:local" hex="0A1B" g="---05"/>
      <e id="e2" dur="PT60M" q="r:local" hex="0a1b" g="---05Z"/>
      <e id="e3" dur="P0M" q="local" hex="0A1C" g="---06"/>
      <e id="e4" dur="PT1.5S" q="p:other" hex="" g="---05"/>
    </doc>"#;
    assert_eq!(
        out(&main, source),
        "<out>e1|e2|e3|e4|e1 e2|e1 e2|0|e1 e4</out>"
    );
}

/// §5.5.2: the arguments of an IdKeyPattern are "either a literal or a
/// reference to a variable or parameter", so the nodes the call selects in a
/// document are the same for every candidate of that document. Matching N
/// candidates is linear, and each document still has its own selection. (The
/// call evaluated and its result scanned per candidate made it quadratic:
/// 112 s for 6,000 candidates in a debug build; selected once, 0.2 s.) Each
/// selection grows with the document: `n`/2 of the `n` items.
#[test]
fn id_and_key_patterns_select_once_per_document() {
    let main = |n: usize| {
        sheet(&format!(
            r#"<xsl:key name="k" match="item" use="@type"/>
               <xsl:variable name="ids" select="string-join(for $i in 0 to {} return concat('i', 2 * $i + 1), ' ')"/>
               <xsl:variable name="tree"><doc><item type="x"/><item type="y"/><item type="x"/></doc></xsl:variable>
               <xsl:template match="/"><out><xsl:apply-templates select="//item" mode="k"/>|<xsl:apply-templates select="//item" mode="id"/>|<xsl:apply-templates select="$tree//item" mode="k"/></out></xsl:template>
               <xsl:template match="key('k', 'x')" mode="k">x</xsl:template>
               <xsl:template match="item" mode="k"/>
               <xsl:template match="id($ids)" mode="id">i</xsl:template>
               <xsl:template match="item" mode="id"/>"#,
            n / 2 - 1
        ))
    };
    assert_linear_in_items("candidates of id() and key() patterns", 750, main, |n| {
        format!("<out>{}|{}|xx</out>", "x".repeat(n / 2), "i".repeat(n / 2))
    });
}

/// §16.3.2's signature is `key($key-name as xs:string, $key-value as
/// xs:anyAtomicType*, $top as node())`, and XPath 2.0 §3.1.5's function
/// conversion rules apply: "If, after the above conversions, the resulting
/// value does not match the expected type according to the rules for
/// SequenceType Matching, a type error is raised [err:XPTY0004]."
#[test]
fn the_arguments_of_key_are_type_checked() {
    let call = |expr: &str| {
        sheet(&format!(
            r#"<xsl:key name="k" match="item" use="@code"/>
               <xsl:template match="/"><out><xsl:value-of select="count({expr})"/></out></xsl:template>"#
        ))
    };
    // `$top as node()`: exactly one node.
    assert_eq!(code_of(&call("key('k', 'a', //item)"), ITEMS), "XPTY0004");
    assert_eq!(code_of(&call("key('k', 'a', ())"), ITEMS), "XPTY0004");
    assert_eq!(code_of(&call("key('k', 'a', 'x')"), ITEMS), "XPTY0004");
    // `$key-name as xs:string`: exactly one string.
    assert_eq!(code_of(&call("key(('k', 'k'), 'a')"), ITEMS), "XPTY0004");
    assert_eq!(code_of(&call("key((), 'a')"), ITEMS), "XPTY0004");
    assert_eq!(code_of(&call("key(1, 'a')"), ITEMS), "XPTY0004");
    assert_eq!(
        code_of(&call("key(QName('', 'k'), 'a')"), ITEMS),
        "XPTY0004"
    );
    // What the conversion rules accept: an untyped value is cast to
    // xs:string, an xs:anyURI is promoted, a type derived from xs:string is
    // one, and the second argument is atomized.
    assert_eq!(
        out(
            &call("key(//kn/@name, 'a')"),
            "<doc><kn name='k'/><item code='a'/></doc>"
        ),
        "<out>1</out>"
    );
    assert_eq!(
        out(&call("key(xs:anyURI('k'), 'a')"), ITEMS),
        "<out>2</out>"
    );
    assert_eq!(
        out(&call("key(xs:NCName('k'), 'a')"), ITEMS),
        "<out>2</out>"
    );
    assert_eq!(
        out(&call("key('k', (//item)[1]/@code, /doc)"), ITEMS),
        "<out>2</out>"
    );
    // With XPath 1.0 compatibility mode, §3.1.5 first takes `V[1]` of an
    // argument that calls for a single item, and `fn:string(V)` for an
    // xs:string.
    let compat = |expr: &str| {
        sheet(&format!(
            r#"<xsl:key name="k" match="item" use="@code"/>
               <xsl:template match="/"><out><xsl:value-of select="count({expr})" version="1.0"/></out></xsl:template>"#
        ))
    };
    assert_eq!(out(&compat("key(('k', 'x'), 'b')"), ITEMS), "<out>1</out>");
    assert_eq!(
        out(&compat("key('k', 'b', (//group, /doc))"), ITEMS),
        "<out>0</out>"
    );
    assert_eq!(
        out(&compat("key('k', 'a', (/doc, //group))"), ITEMS),
        "<out>2</out>"
    );
}

/// An index whose build fails fails the same way for the whole run —
/// the key's patterns and specifiers see only the node, the globals and the
/// document — so the failure is remembered and raised again rather than the
/// build repeated for every candidate of a pattern that calls `key()` (§5.5.4
/// recovers each of those errors as a non-match).
#[test]
fn a_failed_index_build_is_not_repeated() {
    let main = sheet(
        r#"<xsl:key name="k" match="item"><xsl:message>building</xsl:message><xsl:sequence select="xs:integer(@n)"/></xsl:key>
           <xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="item[key('k', 1)]">K</xsl:template>
           <xsl:template match="item">-</xsl:template>"#,
    );
    let source = r#"<doc><g><item n="x"/></g><g><item n="1"/></g><g><item n="1"/></g><g><item n="2"/></g></doc>"#;
    let (result, messages) = run_with_messages(&main, source).expect("the run succeeds");
    assert_eq!(result, "<out>----</out>");
    assert_eq!(messages, ["building"], "the index was built more than once");
}

/// §5.5.4: "Any dynamic error or type error that occurs during the
/// evaluation of a pattern against a particular node is treated as a
/// recoverable error" — including an error the stylesheet raises itself with
/// `fn:error`, whatever its name. Only the processor's own circularity
/// (XTDE0640, §9.8) is signalled.
#[test]
fn a_user_error_named_xtde0640_in_a_pattern_is_recovered() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates select="//item"/></out></xsl:template>
           <xsl:template match="item[error(QName('http://www.w3.org/2005/xqt-errors', 'XTDE0640'))]">E</xsl:template>
           <xsl:template match="item">-</xsl:template>"#,
    );
    assert_eq!(out(&main, ITEMS), "<out>----</out>");
}

/// a host collation **without** a sort key still gives collated
/// answers (all its strings share one bucket, every candidate compared with
/// the collation), including one whose equality is far coarser than
/// codepoints. §16.3.1: "two values `$a` and `$b` are considered equal if the
/// result of the function call `compare($a, $b, $collation)` is zero."
#[test]
fn a_host_collation_without_a_sort_key_still_compares_by_the_collation() {
    use std::cmp::Ordering;
    use std::rc::Rc;
    use xslt2::collation::Collation;

    /// Equal iff the same number of characters; no sort key.
    struct ByLength;
    impl Collation for ByLength {
        fn compare(&self, a: &str, b: &str) -> Ordering {
            a.chars().count().cmp(&b.chars().count())
        }
    }
    let main = sheet(
        r#"<xsl:key name="k" match="item" use="@code" collation="urn:by-length"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 'z')/@id"/>|<xsl:value-of select="key('k', 'zzz')/@id"/>|<xsl:value-of select="count(key('k', 'zz'))"/></out></xsl:template>"#,
    );
    let result = xslt2::run_on_large_stack(TEST_STACK, move || {
        let mut functions = XsltFunctions::new();
        functions.register_collation("urn:by-length", Rc::new(ByLength));
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///keys/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///keys/in.xml", ITEMS)?;
        compiled.serialize_primary(&outcome).map(common::body)
    });
    assert_eq!(
        result.expect("the run succeeds"),
        "<out>i1 i2 i3|i4|0</out>"
    );
}

/// numeric key values of the four numeric types find each other
/// exactly when `eq` (with numeric promotion, XPath 2.0 §B.1) says they are
/// equal. Promotion goes towards the other operand's type: the decimal `0.1`
/// becomes the double `0.1` against a double and the float `0.1` against a
/// float, so it equals both, while the float `0.1` promoted to `xs:double` is
/// not the double `0.1`.
#[test]
fn numeric_key_values_of_different_types_match_as_eq_says() {
    let main = sheet(
        r#"<xsl:key name="k" match="e" use="(xs:decimal(@dec), xs:double(@dbl), xs:float(@flt), xs:integer(@int))"/>
           <xsl:template match="/"><out><xsl:value-of select="key('k', 5)/@id"/>|<xsl:value-of select="key('k', xs:float(2.5))/@id"/>|<xsl:value-of select="key('k', 0.1)/@id"/>|<xsl:value-of select="key('k', xs:float('0.1'))/@id"/>|<xsl:value-of select="key('k', -0.0e0)/@id"/>|<xsl:value-of select="for $i in //e, $v in (xs:decimal($i/@dec), xs:double($i/@dbl), xs:float($i/@flt), xs:integer($i/@int)), $p in (0.1, xs:float('0.1')) return (if ($v eq $p) then concat($i/@id, '=', $p) else ())"/></out></xsl:template>"#,
    );
    let source = r#"<doc>
      <e id="e1" dec="5.00" dbl="1" flt="1" int="1"/>
      <e id="e2" dec="1" dbl="5e0" flt="2.5" int="1"/>
      <e id="e3" dec="0.1" dbl="2" flt="2" int="0"/>
      <e id="e4" dec="2" dbl="0.1" flt="0.1" int="5"/>
    </doc>"#;
    // The last field lists every pair `eq` equates, computed by `eq` itself:
    // the key results must agree with it.
    assert_eq!(
        out(&main, source),
        "<out>e1 e2 e4|e2|e3 e4|e3 e4|e3|e3=0.1 e3=0.1 e4=0.1 e4=0.1 e4=0.1</out>"
    );
}

// ── The stack guard of nested key builds, static errors, comments ───────────

/// Compiles `main` and runs it over `source` on a thread of `stack` bytes that
/// `run_on_large_stack` did *not* start — an ordinary host thread, whose run
/// gets `OTHER_THREAD_STACK_LIMIT` unless `options` say otherwise.
fn run_on_thread(
    stack: usize,
    options: TransformOptions,
    main: &str,
    source: &str,
) -> Result<String, XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let functions = XsltFunctions::new();
            let names = NameTable::new();
            let compiled =
                Compiler::new(&names, &functions).compile_str(&main, "file:///keys/main.xsl")?;
            let arena = Bump::new();
            let outcome = compiled
                .transformation(&arena, options)
                .transform_source("file:///keys/in.xml", &source)?;
            compiled.serialize_primary(&outcome).map(common::body)
        })
        .expect("a test thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// Like [`run_on_thread`], on a thread `run_on_large_stack` started with
/// `stack` bytes, whose limit the run derives from that size.
fn run_on_large(stack: usize, main: &str, source: &str) -> Result<String, XsltError> {
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(stack, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let compiled =
            Compiler::new(&names, &functions).compile_str(&main, "file:///keys/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, TransformOptions::default())
            .transform_source("file:///keys/in.xml", &source)?;
        compiled.serialize_primary(&outcome).map(common::body)
    })
}

/// Asserts that `result` is the depth error of `TransformOptions::stack_limit`
/// — an error the caller gets back, where without the guard the process
/// aborts with a stack overflow.
fn assert_depth_error(result: Result<String, XsltError>, what: &str) {
    let error = match result {
        Ok(out) => panic!("{what}: expected the depth error, got {out:?}"),
        Err(e) => e,
    };
    assert_eq!(error.code, "XSLT2-INTERNAL", "{what}: {error}");
    assert!(
        error.message.contains("recursing without end"),
        "{what}: expected the depth error, got {error}"
    );
}

/// `n` keys `k1` … `kn` over `item`s, each `k{i}` depending on `k{i+1}`
/// through `link(i + 1)` — in its `use` expression or its `match` pattern;
/// `kn` is a plain key on `@v`. The template looks up `k1`, so building it
/// builds the whole chain, one index inside the other.
fn dependent_keys(n: usize, link: impl Fn(usize) -> String) -> String {
    let mut body = String::new();
    for i in 1..n {
        body.push_str(&link(i + 1));
    }
    body.push_str(&format!(r#"<xsl:key name="k{n}" match="item" use="@v"/>"#));
    body.push_str(
        r#"<xsl:template match="/"><out><xsl:value-of select="count(key('k1', 'a'))"/></out></xsl:template>"#,
    );
    sheet(&body)
}

/// How key `k{next - 1}` is declared so that it depends on `k{next}`.
type Link = fn(usize) -> String;

/// The three ways one key's build can need another's index: its `use`
/// expression, a predicate of its `match` pattern, its `match` pattern being
/// a `key()` pattern.
fn dependency_links() -> [(&'static str, Link); 3] {
    [
        ("use", |next| {
            format!(
                r#"<xsl:key name="k{}" match="item" use="key('k{next}', 'a')/@v"/>"#,
                next - 1
            )
        }),
        ("a predicate of match", |next| {
            format!(
                r#"<xsl:key name="k{}" match="item[key('k{next}', 'a')]" use="@v"/>"#,
                next - 1
            )
        }),
        ("a key() pattern", |next| {
            format!(
                r#"<xsl:key name="k{}" match="key('k{next}', 'a')" use="@v"/>"#,
                next - 1
            )
        }),
    ]
}

const ONE_ITEM: &str = r#"<doc><item v="a" d="0"/></doc>"#;

/// §16.3.1: "Keys can be used to locate nodes within any
/// source document (including temporary trees), but each use of the key
/// function searches one document only" — so building one key's index over a
/// document can need another key's index first, which is built right there,
/// inside the first build. A chain of such builds nests on the host stack with
/// no template invocation or function call in between, and nothing checked
/// the stack guard: 1 000 dependent keys aborted the process on an ordinary
/// 2 MiB thread, and 20 000 on the default 256 MiB `run_on_large_stack`
/// thread. §9.8: "Implementations are required to signal circularity as a
/// dynamic error, but they are not required to detect non-terminating
/// recursion" — this processor promises the depth error instead of an abort
/// (`TransformOptions::stack_limit`), and now every key build is checked.
#[test]
fn a_chain_of_dependent_key_builds_ends_in_the_depth_error() {
    for (how, link) in dependency_links() {
        // A short chain works, on either kind of thread. (A debug build uses
        // about 57 KB of stack a level, so only a few levels fit under the
        // 1 MiB an ordinary thread's run gets by default.)
        assert_eq!(
            run_on_thread(
                4 << 20,
                TransformOptions::default(),
                &dependent_keys(5, link),
                ONE_ITEM
            )
            .unwrap(),
            "<out>1</out>",
            "{how}"
        );
        assert_eq!(
            run_on_large(16 << 20, &dependent_keys(20, link), ONE_ITEM).unwrap(),
            "<out>1</out>",
            "{how}"
        );
        // 2 000 levels need far more than 16 MiB of stack (about 11–14 KB a
        // level in a release build, several times that in a debug build).
        let long = dependent_keys(2_000, link);
        assert_depth_error(
            run_on_thread(4 << 20, TransformOptions::default(), &long, ONE_ITEM),
            &format!("{how}, an ordinary thread"),
        );
        assert_depth_error(
            run_on_large(16 << 20, &long, ONE_ITEM),
            &format!("{how}, run_on_large_stack"),
        );
    }
}

/// the same key over a new temporary tree at each level — a
/// runaway recursion through key builds alone, each (key, document) pair new,
/// so no circularity (§9.8 "Circularity is not the same as recursion"). It
/// aborted on every stack; it now ends in the depth error on an ordinary
/// thread and on the default `run_on_large_stack` thread. The same recursion
/// with a stylesheet function between the builds was already stopped (the
/// function call checks the guard and counts toward
/// `max_recursion_depth`), and still is.
#[test]
fn a_key_rebuilt_over_new_trees_without_end_ends_in_the_depth_error() {
    let through_key_alone = sheet(
        r#"<xsl:key name="k" match="item">
             <xsl:variable name="t"><item d="{xs:integer(@d) + 1}"/></xsl:variable>
             <xsl:sequence select="if (exists(key('k', 'x', $t))) then 'x' else 'y'"/>
           </xsl:key>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'x'))"/></out></xsl:template>"#,
    );
    assert_depth_error(
        run_on_thread(
            4 << 20,
            TransformOptions::default(),
            &through_key_alone,
            ONE_ITEM,
        ),
        "an ordinary thread",
    );
    assert_depth_error(
        run_on_large(xslt2::DEFAULT_STACK_SIZE, &through_key_alone, ONE_ITEM),
        "run_on_large_stack",
    );
    // Bounded, the same recursion completes.
    let bounded = sheet(
        r#"<xsl:key name="k" match="item">
             <xsl:variable name="d" select="xs:integer(@d)"/>
             <xsl:choose>
               <xsl:when test="$d lt 10">
                 <xsl:variable name="t"><item d="{$d + 1}"/></xsl:variable>
                 <xsl:sequence select="if (exists(key('k', 'x', $t))) then 'x' else 'y'"/>
               </xsl:when>
               <xsl:otherwise><xsl:sequence select="'x'"/></xsl:otherwise>
             </xsl:choose>
           </xsl:key>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k', 'x'))"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_on_large(16 << 20, &bounded, ONE_ITEM).unwrap(),
        "<out>1</out>"
    );
    // A key whose `use` recurses through a stylesheet function that calls
    // `key()` over a new tree, and one whose `use` recurses through a function
    // alone.
    for (what, body) in [
        (
            "through a function and key()",
            r#"<xsl:key name="k" match="item" use="f:value(.)"/>
               <xsl:function name="f:value">
                 <xsl:param name="item"/>
                 <xsl:variable name="t"><item d="{xs:integer($item/@d) + 1}"/></xsl:variable>
                 <xsl:sequence select="if (exists(key('k', 'x', $t))) then 'x' else 'y'"/>
               </xsl:function>"#,
        ),
        (
            "through a function alone",
            r#"<xsl:key name="k" match="item" use="f:down(1000000)"/>
               <xsl:function name="f:down">
                 <xsl:param name="n"/>
                 <xsl:sequence select="if ($n le 0) then 'x' else f:down($n - 1)"/>
               </xsl:function>"#,
        ),
    ] {
        let main = sheet(&format!(
            r#"{body}<xsl:template match="/"><out><xsl:value-of select="count(key('k', 'x'))"/></out></xsl:template>"#
        ));
        assert_depth_error(
            run_on_thread(4 << 20, TransformOptions::default(), &main, ONE_ITEM),
            &format!("{what}, an ordinary thread"),
        );
        assert_depth_error(
            run_on_large(xslt2::DEFAULT_STACK_SIZE, &main, ONE_ITEM),
            &format!("{what}, run_on_large_stack"),
        );
    }
}

/// the depth error does not replace XTDE0640. §9.8: "[ERR
/// XTDE0640] In general, a circularity in a stylesheet is a non-recoverable
/// dynamic error." The circularity test comes before the stack check, so a
/// key defined in terms of itself is XTDE0640 on any thread — here through a
/// chain of three keys. (A cycle longer than the stack can hold ends in the
/// depth error before the cycle is closed.)
#[test]
fn circularity_stays_xtde0640_under_the_stack_guard() {
    let cycle = sheet(
        r#"<xsl:key name="k1" match="item" use="key('k2', 'a')/@v"/>
           <xsl:key name="k2" match="item[key('k3', 'a')]" use="@v"/>
           <xsl:key name="k3" match="key('k1', 'a')" use="@v"/>
           <xsl:template match="/"><out><xsl:value-of select="count(key('k1', 'a'))"/></out></xsl:template>"#,
    );
    let on_thread =
        run_on_thread(4 << 20, TransformOptions::default(), &cycle, ONE_ITEM).unwrap_err();
    assert_eq!(on_thread.code, "XTDE0640", "{on_thread}");
    let on_large = run_on_large(16 << 20, &cycle, ONE_ITEM).unwrap_err();
    assert_eq!(on_large.code, "XTDE0640", "{on_large}");
    // A global variable defined in terms of itself, likewise.
    let variables = sheet(
        r#"<xsl:variable name="x" select="$y + 1"/><xsl:variable name="y" select="$x + 1"/>
           <xsl:template match="/"><out><xsl:value-of select="$x"/></out></xsl:template>"#,
    );
    let error =
        run_on_thread(4 << 20, TransformOptions::default(), &variables, ONE_ITEM).unwrap_err();
    assert_eq!(error.code, "XTDE0640", "{error}");
}

/// A key's `use` can read a global
/// variable whose value reads another (§9.5: a global is evaluated when it is
/// first needed), and a chain of globals nests on the stack just like a chain
/// of keys, with no check: 1 000 of them aborted on an ordinary 2 MiB thread.
/// The evaluation of a global now checks the stack guard too.
#[test]
fn a_chain_of_global_variables_ends_in_the_depth_error() {
    let chain = |n: usize| {
        let mut body = String::new();
        for i in 1..n {
            body.push_str(&format!(
                r#"<xsl:variable name="g{i}" select="$g{} + 1"/>"#,
                i + 1
            ));
        }
        body.push_str(&format!(r#"<xsl:variable name="g{n}" select="0"/>"#));
        body.push_str(r#"<xsl:key name="k" match="item" use="$g1"/>"#);
        body.push_str(
            r#"<xsl:template match="/"><out><xsl:value-of select="key('k', $g1)/@v"/></out></xsl:template>"#,
        );
        sheet(&body)
    };
    assert_eq!(
        run_on_thread(2 << 20, TransformOptions::default(), &chain(20), ONE_ITEM).unwrap(),
        "<out>a</out>"
    );
    // About 7 KB a level in a release build: 400 levels need more than 2 MiB.
    assert_depth_error(
        run_on_thread(2 << 20, TransformOptions::default(), &chain(400), ONE_ITEM),
        "an ordinary 2 MiB thread",
    );
}

/// An attribute set that uses another
/// expands it right there (§10.2), so a chain of them nests on the stack with
/// no check; 50 000 aborted on an ordinary 2 MiB thread. Each nested
/// expansion now checks the stack guard. (Here a small thread and limit stand
/// in for a long chain.)
#[test]
fn a_chain_of_attribute_sets_ends_in_the_depth_error() {
    let chain = |n: usize| {
        let mut body = String::new();
        for i in 1..n {
            body.push_str(&format!(
                r#"<xsl:attribute-set name="a{i}" use-attribute-sets="a{}"/>"#,
                i + 1
            ));
        }
        body.push_str(&format!(
            r#"<xsl:attribute-set name="a{n}"><xsl:attribute name="last" select="{n}"/></xsl:attribute-set>"#
        ));
        body.push_str(
            r#"<xsl:template match="/"><out xsl:use-attribute-sets="a1"/></xsl:template>"#,
        );
        sheet(&body)
    };
    let small_limit = || TransformOptions {
        stack_limit: Some(256 * 1024),
        ..Default::default()
    };
    assert_eq!(
        run_on_thread(1 << 20, small_limit(), &chain(20), ONE_ITEM).unwrap(),
        r#"<out last="20"/>"#
    );
    // About 600 bytes a level in a release build: 3 000 levels need more than
    // the 1 MiB thread has.
    assert_depth_error(
        run_on_thread(1 << 20, small_limit(), &chain(3_000), ONE_ITEM),
        "a 1 MiB thread",
    );
}

/// Compiles `main` (with `files` available to `xsl:import`) without running
/// it: the static errors only.
fn compile_error(files: &[(&str, &str)], main: &str) -> Option<XsltError> {
    let functions = XsltFunctions::new();
    let names = NameTable::new();
    let mut mem = MemoryResolver::new();
    for &(uri, text) in files {
        mem.insert(uri, text);
    }
    Compiler::new(&names, &functions)
        .with_resolver(Arc::new(mem))
        .compile_str(main, "file:///keys/main.xsl")
        .err()
}

/// §2.9: "A static error must be signaled even if it occurs
/// in a part of the stylesheet that is never evaluated." §10.1.1: "[ERR
/// XTSE0680] In the case of xsl:call-template, it is a static error to pass a
/// non-tunnel parameter named x to a template that does not have a template
/// parameter named x, unless backwards compatible behavior is enabled for the
/// xsl:call-template instruction." That check runs over every sequence
/// constructor of the stylesheet once the declarations are compiled — and the
/// content of `xsl:key` was not among them: a key never used compiled without
/// an error, and a used one reported it only when its index was built, from
/// the run-time safety net ("has no parameter named y"). Now it is a static
/// error in every case — whatever module the key is in, wherever in its
/// content the call stands, and for every declaration of a key whose
/// declarations are unioned (§16.3.1 "A key is defined as a set of xsl:key
/// declarations in the stylesheet that share the same name").
#[test]
fn xtse0680_in_a_key_constructor_is_static_whether_or_not_the_key_is_used() {
    let call_y =
        r#"<xsl:call-template name="t"><xsl:with-param name="y" select="1"/></xsl:call-template>"#;
    let template_t = r#"<xsl:template name="t"><xsl:param name="x"/>v</xsl:template>"#;
    let not_used = r#"<xsl:template match="/"><out/></xsl:template>"#;
    let used = r#"<xsl:template match="/"><out><xsl:value-of select="count(key('k', 'v'))"/></out></xsl:template>"#;
    let cases = [
        (
            "an unused key",
            sheet(&format!(
                r#"{template_t}<xsl:key name="k" match="item">{call_y}</xsl:key>{not_used}"#
            )),
        ),
        (
            "an unused key, the call nested in instructions",
            sheet(&format!(
                r#"{template_t}<xsl:key name="k" match="item"><xsl:if test="true()"><xsl:for-each select="."><xsl:variable name="v">{call_y}</xsl:variable><xsl:sequence select="string($v)"/></xsl:for-each></xsl:if></xsl:key>{not_used}"#
            )),
        ),
        (
            "a used key",
            sheet(&format!(
                r#"{template_t}<xsl:key name="k" match="item">{call_y}</xsl:key>{used}"#
            )),
        ),
        (
            "the declaration of a used key that matches nothing",
            sheet(&format!(
                r#"{template_t}<xsl:key name="k" match="item" use="'v'"/><xsl:key name="k" match="absent">{call_y}</xsl:key>{used}"#
            )),
        ),
    ];
    for (what, main) in &cases {
        let error = compile_error(&[], main)
            .unwrap_or_else(|| panic!("{what}: expected XTSE0680 at compile time"));
        assert_eq!(error.code, "XTSE0680", "{what}: {error}");
        // The static check's wording, not the run-time safety net's.
        assert!(
            error.message.contains("no non-tunnel parameter named y"),
            "{what}: {error}"
        );
    }
    // A key declared in an imported module, never used.
    let imported = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"><xsl:key name="k" match="item">{call_y}</xsl:key></xsl:stylesheet>"#
    );
    let main = sheet(&format!(
        r#"<xsl:import href="imp.xsl"/>{template_t}{not_used}"#
    ));
    let error = compile_error(&[("file:///keys/imp.xsl", imported.as_str())], &main)
        .expect("XTSE0680 from the imported module");
    assert_eq!(error.code, "XTSE0680", "{error}");
    assert_eq!(
        error
            .location
            .as_ref()
            .and_then(|l| l.module_uri.as_deref()),
        Some("file:///keys/imp.xsl"),
        "{error}"
    );
    // Not errors: a tunnel parameter, and backwards compatible behaviour on
    // the key (§10.1.1 "unless backwards compatible behavior is enabled").
    let tunnel = sheet(&format!(
        r#"{template_t}<xsl:key name="k" match="item"><xsl:call-template name="t"><xsl:with-param name="y" select="1" tunnel="yes"/></xsl:call-template></xsl:key>{used}"#
    ));
    assert_eq!(out(&tunnel, ITEMS), "<out>4</out>");
    let compat = sheet(&format!(
        r#"{template_t}<xsl:key name="k" match="item" version="1.0">{call_y}</xsl:key>{used}"#
    ));
    assert_eq!(out(&compat, ITEMS), "<out>4</out>");
}

/// XPath 2.0 §2.6: "Comments are lexical constructs only, and
/// do not affect expression processing. … Comments may be nested. A comment may
/// be used anywhere ignorable whitespace is allowed (see A.2.4.1 Default
/// Whitespace Handling)." §5.5.2: "Every pattern is a legal XPath expression",
/// and the constructs of the Pattern grammar "are part of the XPath expression
/// language, and are defined in \[XPath 2.0\]". A comment outside a predicate
/// made the pattern scanner see a node test, a kind test or a broken
/// IdKeyPattern (XTSE0340) — in `key()` and `id()` patterns and in every other
/// pattern alike (the suite case `match-246b`).
#[test]
fn xpath_comments_are_allowed_wherever_a_pattern_allows_whitespace() {
    let source =
        r#"<doc><item xml:id="i1" code="a"><b/></item><item xml:id="i2" code="(: a :)"/></doc>"#;
    for (pattern, expected) in [
        ("key('k', 'a') (: c :)", "[i1]"),
        ("key( (: c :) 'k', 'a')", "[i1]"),
        ("key (: c :) ('k' (: d :), $a (: e :))", "[i1]"),
        ("key('k', '(: a :)')", "[i2]"),
        ("id('i2') (: c :)", "[i2]"),
        ("id('i1') (: c :) / b", "[b]"),
        ("id( (: c :) 'i1')//(: d :)b", "[b]"),
        ("(: c :) item (: d :) [@code = 'a'] (: e :)", "[i1]"),
        ("item[@code = '(: a :)']", "[i2]"),
        ("item (: x (: nested :) y :) / b", "[b]"),
        ("doc/(: c :)item[2] | (: | :) b", "[b][i2]"),
        ("child (: c :) :: item[(: d :) 1]", "[i1]"),
        ("element( (: c :) item ) [ (: d :) 2 ]", "[i2]"),
    ] {
        let main = sheet(&format!(
            r#"<xsl:key name="k" match="item (: the items :)" use="@code (: their codes :)"/>
               <xsl:variable name="a" select="'a'"/>
               <xsl:template match="/"><out><xsl:apply-templates select="//*" mode="m"/></out></xsl:template>
               <xsl:template match="{pattern}" mode="m" priority="10">[<xsl:value-of select="(@xml:id, name())[1]"/>]</xsl:template>
               <xsl:template match="*" mode="m"/>"#
        ));
        assert_eq!(
            out(&main, source),
            format!("<out>{expected}</out>"),
            "{pattern}"
        );
    }
    // Not patterns: a comment that is not closed, a pattern that is only a
    // comment, and a comment inside a QName (XPath 2.0 A.1.2: "comments are
    // not permissible on either side of the colon").
    for pattern in [
        "item (: c",
        "item[1 (: c]",
        "item (: x (: nested :) y",
        "(: c :)",
        "xs:(: c :)item",
    ] {
        let main = sheet(&format!(
            r#"<xsl:template match="{pattern}"/><xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_eq!(code_of(&main, ITEMS), "XTSE0340", "{pattern}");
    }
}
