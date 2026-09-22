//! Assertion evaluation (design sketch §2.4) against a `RunOutput`.
//!
//! `assert` XPaths are evaluated with the `xsd-schema` XPath 2.0 engine over the serialized
//! result re-parsed with roxmltree (`RoXmlNavigator`), so the comparison is independent of the
//! engine's own tree type. `serialization-matches` uses `fn:matches`.
//!
//! `assert-xml` follows the catalog's own definition (`admin/catalog-schema.xsd`, the
//! `assert-xml` documentation): *parse and canonicalize both sides and compare byte-for-byte*,
//! with `ignore-prefixes` defaulting to **false**. Canonical XML byte equality is reproduced
//! here as two layers:
//!
//! * the **lenient** layer — expanded names, attributes sorted by (namespace, local name),
//!   merged/escaped text — which is all that `ignore-prefixes="true"` requires, and
//! * the **strict** layer, applied by default, which additionally requires the lexical prefix
//!   of every element name and every attribute name to match and the set of in-scope namespace
//!   bindings of every element to match.
//!
//! `--lenient-xml` on the driver forces the lenient layer globally; it exists for triage only.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use roxmltree::{Document, Node, NodeType};
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::table::NameTable;
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{XPathContext, XPathExpr};
use xsd_schema::RoXmlNavigator;

use crate::catalog::{Assertion, NsBindings};
use crate::runner::{ResultDocument, RunOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Outcome {
    Pass,
    Fail,
    /// An error was expected and an error occurred, but with a different code.
    WrongError,
    /// The harness could not evaluate the assertion (unsupported assertion, bad XPath, …).
    HarnessError,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub outcome: Outcome,
    pub message: Option<String>,
}

impl Verdict {
    fn pass() -> Self {
        Verdict {
            outcome: Outcome::Pass,
            message: None,
        }
    }
    fn fail(m: impl Into<String>) -> Self {
        Verdict {
            outcome: Outcome::Fail,
            message: Some(m.into()),
        }
    }
    fn wrong_error(m: impl Into<String>) -> Self {
        Verdict {
            outcome: Outcome::WrongError,
            message: Some(m.into()),
        }
    }
    fn harness(m: impl Into<String>) -> Self {
        Verdict {
            outcome: Outcome::HarnessError,
            message: Some(m.into()),
        }
    }
}

pub struct CompareContext<'a> {
    /// Test-set directory, for `file=` attributes on assertions.
    pub set_dir: &'a Path,
    /// `--lenient-xml`: compare `assert-xml` as if every assertion carried
    /// `ignore-prefixes="true"`. Triage aid; not the catalog's semantics.
    pub lenient_xml: bool,
}

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
const FN_NS: &str = "http://www.w3.org/2005/xpath-functions";
const XSLT_NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Evaluate the assertions of one case, turning a panic inside assertion evaluation into a
/// `harnessError` for that case instead of ending the whole run — the same guard
/// `runner::execute` puts around the engine call, which does not extend to this side of it.
pub fn evaluate_guarded(a: &Assertion, out: &RunOutput, cx: &CompareContext<'_>) -> Verdict {
    guard(|| evaluate(a, out, cx))
}

/// The panic guard itself, separated so the self-tests can exercise it.
fn guard(f: impl FnOnce() -> Verdict) -> Verdict {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(p) => Verdict::harness(format!(
            "panic while evaluating assertions: {}",
            crate::runner::panic_message(p)
        )),
    }
}

pub fn evaluate(a: &Assertion, out: &RunOutput, cx: &CompareContext<'_>) -> Verdict {
    use Assertion::*;
    match a {
        // An empty `<result>` (or a `<test-case>` with no `<result>` at all) asserts nothing:
        // reporting it as a pass would be a free pass for a case nobody checks.
        AllOf(list) if list.is_empty() => {
            Verdict::harness("no assertion to check (empty or missing <result>)")
        }
        AllOf(list) => {
            for inner in list {
                let v = evaluate(inner, out, cx);
                if v.outcome != Outcome::Pass {
                    return v;
                }
            }
            Verdict::pass()
        }
        AnyOf(list) => {
            let mut worst: Option<Verdict> = None;
            for inner in list {
                let v = evaluate(inner, out, cx);
                if v.outcome == Outcome::Pass {
                    return v;
                }
                worst = Some(match worst {
                    Some(w) if w.outcome <= v.outcome => Verdict {
                        message: join(w.message, v.message),
                        ..w
                    },
                    Some(w) => Verdict {
                        outcome: v.outcome,
                        message: join(w.message, v.message),
                    },
                    None => v,
                });
            }
            worst.unwrap_or_else(|| Verdict::fail("empty any-of"))
        }
        // A negated assertion must not be satisfied merely because the transformation blew up:
        // without this guard `<not><assert>…</assert></not>` passes on any unexpected error,
        // because the inner assertion then fails for a reason that has nothing to do with it.
        Not(inner) if out.error.is_some() && !mentions_error(inner) => unexpected_error(out),
        Not(inner) => match evaluate(inner, out, cx).outcome {
            Outcome::Pass => Verdict::fail(format!("negated assertion <{}> passed", inner.label())),
            Outcome::HarnessError => {
                Verdict::harness(format!("cannot evaluate negated <{}>", inner.label()))
            }
            _ => Verdict::pass(),
        },
        // `@code` is optional in the catalog schema; an absent one used to default to `*`,
        // which matches any error at all.
        Error { code } | AssertSerializationError { code } if code.is_empty() => Verdict::harness(
            format!("<{}> has no @code, so nothing is asserted", a.label()),
        ),
        Error { code } => match &out.error {
            Some(e) if code_matches(code, e) => Verdict::pass(),
            Some(e) => Verdict::wrong_error(format!(
                "expected error {code}, got {}: {}",
                e.code, e.message
            )),
            None => Verdict::fail(format!(
                "expected error {code} but the transformation succeeded"
            )),
        },
        AssertSerializationError { code } => match &out.error {
            Some(e) if code_matches(code, e) => Verdict::pass(),
            Some(e) => Verdict::wrong_error(format!(
                "expected serialization error {code}, got {}: {}",
                e.code, e.message
            )),
            None => Verdict::fail(format!(
                "expected serialization error {code} but serialization succeeded"
            )),
        },
        _ if out.error.is_some() => unexpected_error(out),
        Assert { xpath, ns } => with_primary(out, |doc| match eval_xpath(xpath, ns, doc, ebv) {
            Ok(true) => Verdict::pass(),
            Ok(false) => Verdict::fail(format!("assert `{}` is false", one_line(xpath))),
            Err(m) => Verdict::harness(m),
        }),
        AssertXml {
            expected,
            file,
            xml_version,
            ignore_prefixes,
        } => {
            if xml_version.as_deref() == Some("1.1") {
                return Verdict::harness("assert-xml with xml-version 1.1");
            }
            let expected = match load_text(expected, file, cx) {
                Ok(t) => t,
                Err(m) => return Verdict::harness(m),
            };
            // The catalog default is ignore-prefixes="false" = full Canonical-XML equivalence.
            let strict = !*ignore_prefixes && !cx.lenient_xml;
            match &out.primary {
                None => Verdict::fail("no primary result"),
                Some(p) => match compare_xml(&expected, &p.serialized, strict) {
                    Ok(()) => Verdict::pass(),
                    Err(m) => Verdict::fail(m),
                },
            }
        }
        AssertStringValue {
            expected,
            normalize_space: normalize,
        } => match &out.primary {
            None => Verdict::fail("no primary result"),
            Some(p) => {
                let actual = string_value_of(&p.serialized);
                let (e, a) = if *normalize {
                    (normalize_space(expected), normalize_space(&actual))
                } else {
                    (expected.clone(), actual)
                };
                if e == a {
                    Verdict::pass()
                } else {
                    Verdict::fail(format!(
                        "string value mismatch: expected {:?}, got {:?}",
                        clip(&e),
                        clip(&a)
                    ))
                }
            }
        },
        SerializationMatches { regex, flags, file } => {
            let regex = match load_text(regex, file, cx) {
                Ok(t) => t,
                Err(m) => return Verdict::harness(m),
            };
            match &out.primary {
                None => Verdict::fail("no primary result"),
                Some(p) => match regex_matches(&p.serialized, &regex, flags) {
                    Ok(true) => Verdict::pass(),
                    Ok(false) => Verdict::fail(format!(
                        "serialization does not match /{}/{}",
                        clip(&regex),
                        flags
                    )),
                    Err(m) => Verdict::harness(m),
                },
            }
        }
        AssertSerialization { expected, file, .. } => {
            let expected = match load_text(expected, file, cx) {
                Ok(t) => t,
                Err(m) => return Verdict::harness(m),
            };
            match &out.primary {
                None => Verdict::fail("no primary result"),
                Some(p) => {
                    if trim_trailing_newlines(&expected) == trim_trailing_newlines(&p.serialized) {
                        Verdict::pass()
                    } else {
                        Verdict::fail(format!(
                            "serialization mismatch: expected {:?}, got {:?}",
                            clip(&expected),
                            clip(&p.serialized)
                        ))
                    }
                }
            }
        }
        AssertResultDocument { uri, inner } => {
            let found = out
                .secondary
                .iter()
                .find(|(u, _)| result_document_uri_matches(u, uri));
            match found {
                None => Verdict::fail(format!(
                    "no result document {uri} (have: {:?})",
                    out.secondary
                        .iter()
                        .map(|(u, _)| u.as_str())
                        .collect::<Vec<_>>()
                )),
                Some((_, doc)) => {
                    let view = RunOutput {
                        primary: Some(doc.clone()),
                        ..Default::default()
                    };
                    evaluate(inner, &view, cx)
                }
            }
        }
        AssertMessage(inner) => {
            if out.messages.is_empty() {
                return Verdict::fail("no xsl:message output");
            }
            let mut last = Verdict::fail("no message satisfied the assertion");
            for m in &out.messages {
                let view = RunOutput {
                    primary: Some(ResultDocument {
                        serialized: m.clone(),
                    }),
                    ..Default::default()
                };
                let v = evaluate(inner, &view, cx);
                if v.outcome == Outcome::Pass {
                    return v;
                }
                last = v;
            }
            last
        }
        // The harness sees only the serialized result, never the processor's warnings, so
        // there is nothing here to check; reporting a pass would be reporting a check that
        // never happened.
        AssertWarning => {
            Verdict::harness("assert-warning: the harness does not capture processor warnings")
        }
        AssertEmpty => match &out.primary {
            None => Verdict::pass(),
            // Only a trailing newline is forgiven: whitespace in the result is a text node,
            // which is emphatically not an empty sequence.
            Some(p) if trim_trailing_newlines(&p.serialized).is_empty() => Verdict::pass(),
            Some(p) => Verdict::fail(format!(
                "expected empty result, got {:?}",
                clip(&p.serialized)
            )),
        },
        AssertTrue => bool_result(out, true),
        AssertFalse => bool_result(out, false),
        // `$result` is the *document node* of the re-parsed serialization, so atomizing it
        // yields the result's string value: right for a text-only result, an honest failure
        // otherwise. The three below cannot be answered that way at all — see the audit in
        // `HARNESS_FIX_REPORT.md`.
        AssertEq { xpath, ns } => xpath_over_result(out, &format!("$result eq ({xpath})"), ns),
        AssertDeepEq { .. } => Verdict::harness("assert-deep-eq needs a raw sequence result"),
        // `$result instance of document-node()` is true whatever the real result was.
        AssertType { xpath, .. } => {
            Verdict::harness(format!("assert-type {xpath} needs a raw sequence result"))
        }
        // The catalog schema says so itself: "For XSLT tests, the result is typically a
        // document node, in which case the length is always 1."
        AssertCount(n) => Verdict::harness(format!("assert-count {n} needs a raw sequence result")),
        AssertPermutation { .. } => {
            Verdict::harness("assert-permutation needs a raw sequence result")
        }
        AssertPostureAndSweep => {
            Verdict::harness("assert-posture-and-sweep is a streaming (XSLT 3.0) assertion")
        }
        Unknown(name) => Verdict::harness(format!("unknown assertion <{name}>")),
    }
}

fn unexpected_error(out: &RunOutput) -> Verdict {
    match &out.error {
        Some(e) => Verdict::fail(format!("unexpected error {}: {}", e.code, e.message)),
        None => Verdict::harness("unexpected_error called without an error"),
    }
}

/// Does this assertion (or anything nested in it) say something about errors? Used to decide
/// whether `<not>` may legitimately be satisfied by a transformation that failed.
fn mentions_error(a: &Assertion) -> bool {
    use Assertion::*;
    match a {
        Error { .. } | AssertSerializationError { .. } => true,
        AllOf(l) | AnyOf(l) => l.iter().any(mentions_error),
        Not(i) | AssertMessage(i) => mentions_error(i),
        AssertResultDocument { inner, .. } => mentions_error(inner),
        _ => false,
    }
}

/// `uri` is relative to the base output URI, so it must match the tail of the absolute URI the
/// engine wrote to — but only at a path-segment boundary: `out.xml` is not `checkout.xml`.
fn result_document_uri_matches(actual: &str, want: &str) -> bool {
    if actual == want {
        return true;
    }
    match actual.strip_suffix(want) {
        Some(head) => head.ends_with('/'),
        None => false,
    }
}

fn join(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, None) => a,
        (None, b) => b,
    }
}

/// `*` matches any code; a prefixed code (`err:XTSE0010`) or an EQName
/// (`Q{http://example.com/err}XXXX8888`) is compared by local part.
///
/// One exception: when the catalog writes an EQName **and** the actual error
/// carries an error QName namespace of its own — which only `fn:error` produces,
/// so only `XsltError::error_namespace` can report it — both parts must match.
/// Comparing local names alone would let `Q{urn:a}boom` pass on a
/// `Q{urn:b}boom`, which is a different error. Where the expected code carries
/// no namespace information at all (`XTDE0040`, `err:XPTY0004`, `*`), or the
/// actual error is one the processor raised, nothing changes.
fn code_matches(expected: &str, actual: &xslt2::XsltError) -> bool {
    if expected.trim() == "*" {
        return true;
    }
    if let (Some(want_ns), Some(got_ns)) = (eqname_namespace(expected), actual.error_namespace()) {
        return want_ns == got_ns && local_part(expected) == actual.code;
    }
    local_part(expected) == local_part(&actual.code)
}

/// The local part of an expected or actual error code, however it is spelled.
fn local_part(code: &str) -> &str {
    let code = code.trim();
    match code.strip_prefix("Q{") {
        // The namespace URI of an EQName contains `:`; the local part is what
        // follows the closing brace.
        Some(rest) => rest.rsplit('}').next().unwrap_or(rest),
        None => code.rsplit(':').next().unwrap_or(code),
    }
}

/// The namespace URI of an expected code written as the EQName `Q{uri}local`;
/// `None` for a bare or prefixed code, which says nothing about a namespace.
fn eqname_namespace(expected: &str) -> Option<&str> {
    expected
        .trim()
        .strip_prefix("Q{")?
        .split_once('}')
        .map(|(ns, _)| ns)
}

fn bool_result(out: &RunOutput, want: bool) -> Verdict {
    match &out.primary {
        None => Verdict::fail("no primary result"),
        Some(p) => {
            let got = p.serialized.trim();
            if got == if want { "true" } else { "false" } {
                Verdict::pass()
            } else {
                Verdict::fail(format!("expected {want}, got {:?}", clip(got)))
            }
        }
    }
}

fn xpath_over_result(out: &RunOutput, xpath: &str, ns: &NsBindings) -> Verdict {
    with_primary(out, |doc| match eval_xpath(xpath, ns, doc, ebv) {
        Ok(true) => Verdict::pass(),
        Ok(false) => Verdict::fail(format!("`{}` is false", one_line(xpath))),
        Err(m) => Verdict::harness(m),
    })
}

/// Parse the primary result (leniently: fragments are wrapped) and hand it to `f`.
fn with_primary(out: &RunOutput, f: impl FnOnce(&Document<'_>) -> Verdict) -> Verdict {
    let Some(p) = &out.primary else {
        return Verdict::fail("no primary result");
    };
    let text = parseable_text(&p.serialized);
    match Document::parse(&text) {
        Ok(doc) => f(&doc),
        Err(e) => Verdict::harness(format!("primary result is not parseable XML: {e}")),
    }
}

fn ebv<N: xsd_schema::xpath::DomNavigator>(v: &XPathValue<N>) -> Result<bool, String> {
    effective_boolean_value(v).map_err(|e| format!("effective boolean value: {e}"))
}

/// Compile and evaluate an assertion XPath with the result document node as context item and
/// bound to `$result`. Namespaces: the assertion's in-scope bindings plus `xs`, `fn`, `xsl`.
fn eval_xpath<'d, T>(
    xpath: &str,
    ns: &NsBindings,
    doc: &'d Document<'d>,
    f: impl FnOnce(&XPathValue<RoXmlNavigator<'d>>) -> Result<T, String>,
) -> Result<T, String> {
    let names = NameTable::new();
    let mut bindings = Vec::new();
    let have = |p: &str| ns.iter().any(|(q, _)| q == p);
    for (p, u) in ns {
        bindings.push((names.add(p), names.add(u)));
    }
    for (p, u) in [("xs", XSD_NS), ("fn", FN_NS), ("xsl", XSLT_NS)] {
        if !have(p) {
            bindings.push((names.add(p), names.add(u)));
        }
    }
    let ctx = XPathContext::new(&names).with_namespaces(NamespaceContextSnapshot {
        default_ns: None,
        bindings,
    });
    let expr = XPathExpr::compile_with_vars(xpath, &ctx, &["result"])
        .map_err(|e| format!("cannot compile assertion XPath `{}`: {e}", one_line(xpath)))?;
    let nav = RoXmlNavigator::new(doc);
    let bound = nav.clone();
    let value = expr
        .evaluator(&ctx)
        .run_with_node_and_setup(Some(nav), |ev| {
            let _ = ev.set_variable_by_name("result", XPathValue::from_node(bound));
        })
        .map_err(|e| format!("assertion XPath `{}` raised {e}", one_line(xpath)))?;
    f(&value)
}

fn regex_matches(input: &str, regex: &str, flags: &str) -> Result<bool, String> {
    let names = NameTable::new();
    let ctx = XPathContext::new(&names);
    let expr =
        XPathExpr::compile_with_vars("matches($s, $re, $flags)", &ctx, &["s", "re", "flags"])
            .map_err(|e| format!("cannot compile matches(): {e}"))?;
    let (s, re, fl) = (input.to_string(), regex.to_string(), flags.to_string());
    let value = expr
        .evaluator(&ctx)
        .run_with_node_and_setup::<RoXmlNavigator<'_>, _>(None, |ev| {
            let _ = ev.set_variable_by_name("s", XPathValue::string(s));
            let _ = ev.set_variable_by_name("re", XPathValue::string(re));
            let _ = ev.set_variable_by_name("flags", XPathValue::string(fl));
        })
        .map_err(|e| format!("serialization-matches regex /{}/ raised {e}", clip(regex)))?;
    ebv(&value)
}

fn load_text(
    inline: &str,
    file: &Option<String>,
    cx: &CompareContext<'_>,
) -> Result<String, String> {
    match file {
        Some(f) => {
            let p = cx.set_dir.join(f);
            std::fs::read_to_string(&p)
                .map_err(|e| format!("cannot read expected file {}: {e}", p.display()))
        }
        None => Ok(inline.to_string()),
    }
}

// ───────────────────────── XML canonical comparison ─────────────────────────

const WRAP: &str = "harness-wrap";

/// Strip BOM and XML declaration; wrap in a root element when the text is not a single
/// well-formed document (fragments, text-only results, several top-level elements).
fn parseable_text(xml: &str) -> String {
    let mut s = xml.trim_start_matches('\u{feff}');
    if let Some(rest) = s.strip_prefix("<?xml") {
        if let Some(end) = rest.find("?>") {
            s = &rest[end + 2..];
        }
    }
    if Document::parse(s).is_ok() {
        s.to_string()
    } else {
        format!("<{WRAP}>{s}</{WRAP}>")
    }
}

fn top_level<'a, 'i>(doc: &'a Document<'i>) -> Vec<Node<'a, 'i>> {
    let root = doc.root_element();
    if root.tag_name().name() == WRAP && root.tag_name().namespace().is_none() {
        root.children().collect()
    } else {
        doc.root().children().collect()
    }
}

/// The marker every strict-only (namespace/prefix) failure message starts with, so that the
/// flips introduced by strict comparison can be told apart from ordinary content mismatches.
pub const NS_MISMATCH: &str = "assert-xml namespace mismatch:";

/// Compare an expected and an actual result as the catalog defines `assert-xml`.
///
/// `strict` = `ignore-prefixes` is absent or `false` (the catalog default). The lenient layer
/// runs first either way, so a content difference is always reported as an ordinary
/// `assert-xml mismatch`; only two results that are *equal* leniently and differ in prefixes
/// or namespace bindings produce the [`NS_MISMATCH`] message.
fn compare_xml(expected: &str, actual: &str, strict: bool) -> Result<(), String> {
    let e_text = parseable_text(expected);
    let a_text = parseable_text(actual);
    let e_doc =
        Document::parse(&e_text).map_err(|e| format!("expected XML does not parse: {e}"))?;
    let a_doc =
        Document::parse(&a_text).map_err(|e| format!("actual result does not parse: {e}"))?;
    let (e_top, a_top) = (top_level(&e_doc), top_level(&a_doc));
    let mut e_canon = String::new();
    let mut a_canon = String::new();
    for n in &e_top {
        canonicalize(*n, &mut e_canon);
    }
    for n in &a_top {
        canonicalize(*n, &mut a_canon);
    }
    if e_canon != a_canon {
        let at = e_canon
            .bytes()
            .zip(a_canon.bytes())
            .position(|(x, y)| x != y)
            .unwrap_or(e_canon.len().min(a_canon.len()));
        let ctx = |s: &str| clip(tail_from(s, at.saturating_sub(30)));
        return Err(format!(
            "assert-xml mismatch at byte {at}: expected …{}, got …{}",
            ctx(&e_canon),
            ctx(&a_canon)
        ));
    }
    if strict {
        if let Some(diff) = namespace_diff(&e_text, &e_top, &a_text, &a_top, "") {
            return Err(format!("{NS_MISMATCH} {diff}"));
        }
    }
    Ok(())
}

/// Largest char boundary of `s` at or below `at` (and at most `s.len()`). Comparison offsets
/// are byte offsets into escaped canonical text, so they land mid-character routinely.
fn tail_from(s: &str, at: usize) -> &str {
    let mut i = at.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[i..]
}

// ───────────────────── strict layer: prefixes and namespace bindings ─────────────────────

/// A namespace binding: the prefix (`None` = the default namespace) and the URI it is bound to.
type Binding = (Option<String>, String);

/// The in-scope namespace bindings of an element.
///
/// roxmltree's `Node::namespaces()` returns the *resolved* scope — an element's own
/// declarations merged over its ancestors', with shadowing applied — and it never includes the
/// implicit `xml` binding (the parser drops `xmlns:xml`), which is exactly what is wanted
/// here. `xmlns=""` survives as a binding of the default prefix to the empty URI; that is an
/// *un*declaration, i.e. the absence of a default namespace, so it is dropped: a document with
/// `xmlns=""` in scope and one with no default namespace at all are the same document.
fn in_scope(n: Node) -> BTreeSet<Binding> {
    n.namespaces()
        .filter(|ns| !ns.uri().is_empty() && ns.name() != Some("xml"))
        .map(|ns| (ns.name().map(str::to_string), ns.uri().to_string()))
        .collect()
}

/// The lexical prefix of an element name (`""` when there is none).
///
/// roxmltree 0.19 exposes only the expanded name, but `Node::range()` starts at the `<` of the
/// start tag, so the QName as written can be read straight out of the parsed text. A URI may be
/// bound to several prefixes, so deriving the prefix from the namespace instead would be
/// ambiguous.
fn element_prefix(text: &str, n: Node) -> String {
    let rest = text.get(n.range().start..).unwrap_or_default();
    prefix_of_qname(rest.strip_prefix('<').unwrap_or(rest))
}

/// The lexical prefix of an attribute name (`""` when there is none).
/// `Attribute::position()` points at the first character of the attribute name.
fn attribute_prefix(text: &str, a: &roxmltree::Attribute<'_, '_>) -> String {
    prefix_of_qname(text.get(a.position()..).unwrap_or_default())
}

/// Read a QName off the front of `s` and return its prefix, or `""` if it is unprefixed.
fn prefix_of_qname(s: &str) -> String {
    let end = s
        .find(|c: char| c.is_whitespace() || matches!(c, '>' | '/' | '='))
        .unwrap_or(s.len());
    match s[..end].split_once(':') {
        Some((p, _)) => p.to_string(),
        None => String::new(),
    }
}

/// Attribute prefixes by expanded name, which is the key the lenient layer already matched on.
fn attribute_prefixes(text: &str, n: Node) -> BTreeMap<String, String> {
    n.attributes()
        .map(|a| {
            (
                expanded_name(a.namespace(), a.name()),
                attribute_prefix(text, &a),
            )
        })
        .collect()
}

fn expanded_name(ns: Option<&str>, local: &str) -> String {
    match ns {
        Some(u) => format!("{{{u}}}{local}"),
        None => local.to_string(),
    }
}

/// Children that the lenient layer renders, so the two sides stay in step during the walk.
/// (roxmltree merges adjacent text and CDATA, so an empty text node can only come from empty
/// CDATA, which canonicalizes to nothing.)
fn significant<'a, 'i>(n: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    n.children()
        .filter(|c| match c.node_type() {
            NodeType::Text => !c.text().unwrap_or("").is_empty(),
            _ => true,
        })
        .collect()
}

/// Walk two lenient-equal node lists in step and report the first prefix or namespace
/// difference. Returns the message tail; the caller prefixes [`NS_MISMATCH`].
fn namespace_diff(
    e_text: &str,
    e: &[Node],
    a_text: &str,
    a: &[Node],
    path: &str,
) -> Option<String> {
    // Only reachable if the two sides canonicalized identically, so this should not happen;
    // if it somehow does, the lenient layer's verdict stands rather than inventing a failure.
    if e.len() != a.len() {
        return None;
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (en, an) in e.iter().zip(a.iter()) {
        if !en.is_element() || !an.is_element() {
            continue;
        }
        let expanded = expanded_name(en.tag_name().namespace(), en.tag_name().name());
        let nth = seen.entry(expanded).and_modify(|c| *c += 1).or_insert(1);
        let e_prefix = element_prefix(e_text, *en);
        let here = format!("{path}/{}[{nth}]", qname(&e_prefix, en.tag_name().name()));

        let a_prefix = element_prefix(a_text, *an);
        if e_prefix != a_prefix {
            return Some(format!(
                "element prefix at {here}: expected {}, got {}",
                show(&e_prefix),
                show(&a_prefix)
            ));
        }

        let (e_attrs, a_attrs) = (
            attribute_prefixes(e_text, *en),
            attribute_prefixes(a_text, *an),
        );
        for (name, e_pfx) in &e_attrs {
            if let Some(a_pfx) = a_attrs.get(name) {
                if e_pfx != a_pfx {
                    return Some(format!(
                        "attribute prefix at {here}/@{name}: expected {}, got {}",
                        show(e_pfx),
                        show(a_pfx)
                    ));
                }
            }
        }

        let (e_ns, a_ns) = (in_scope(*en), in_scope(*an));
        if e_ns != a_ns {
            return Some(in_scope_message(&here, &e_ns, &a_ns));
        }

        if let Some(m) = namespace_diff(e_text, &significant(*en), a_text, &significant(*an), &here)
        {
            return Some(m);
        }
    }
    None
}

fn qname(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

fn show(prefix: &str) -> String {
    if prefix.is_empty() {
        "(no prefix)".to_string()
    } else {
        format!("{prefix:?}")
    }
}

/// `xmlns:p="urn:p"`-style rendering of a binding set, in a deterministic order (the default
/// namespace first, then by prefix).
fn render_bindings(set: &BTreeSet<Binding>) -> String {
    if set.is_empty() {
        return "(none)".to_string();
    }
    set.iter()
        .map(|(p, u)| match p {
            Some(p) => format!("xmlns:{p}={u:?}"),
            None => format!("xmlns={u:?}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The whole point of this message is triage, so it carries both scopes *and* the delta: the
/// delta is what says which namespace the engine invented or dropped.
fn in_scope_message(path: &str, e: &BTreeSet<Binding>, a: &BTreeSet<Binding>) -> String {
    let extra: BTreeSet<Binding> = a.difference(e).cloned().collect();
    let missing: BTreeSet<Binding> = e.difference(a).cloned().collect();
    let mut m = format!(
        "in-scope namespaces at {path}: expected [{}], got [{}]",
        clip(&render_bindings(e)),
        clip(&render_bindings(a)),
    );
    // Only the non-empty side of the delta is named, so that "actual has extra" always means
    // there *is* something extra — this is the clause the namespace breakdown is grouped by.
    if !extra.is_empty() {
        m.push_str(&format!("; actual has extra [{}]", render_bindings(&extra)));
    }
    if !missing.is_empty() {
        m.push_str(&format!("; actual lacks [{}]", render_bindings(&missing)));
    }
    m
}

/// Canonical form: expanded names, attributes sorted by (namespace, local name), text escaped,
/// adjacent text merged by the parser, namespace declarations ignored (prefixes are irrelevant).
/// This is the *lenient* layer — everything `ignore-prefixes="true"` asks for, and the
/// structural comparison the strict layer is then laid over.
fn canonicalize(n: Node, out: &mut String) {
    match n.node_type() {
        NodeType::Element => {
            out.push('<');
            push_expanded(out, n.tag_name().namespace(), n.tag_name().name());
            let mut attrs: Vec<_> = n
                .attributes()
                .map(|a| {
                    (
                        a.namespace().unwrap_or("").to_string(),
                        a.name().to_string(),
                        a.value().to_string(),
                    )
                })
                .collect();
            attrs.sort();
            for (ns, name, value) in attrs {
                out.push(' ');
                push_expanded(out, if ns.is_empty() { None } else { Some(&ns) }, &name);
                out.push_str("=\"");
                escape_into(&value, out, true);
                out.push('"');
            }
            out.push('>');
            for c in n.children() {
                canonicalize(c, out);
            }
            out.push_str("</>");
        }
        NodeType::Text => escape_into(n.text().unwrap_or(""), out, false),
        NodeType::Comment => {
            out.push_str("<!--");
            out.push_str(n.text().unwrap_or(""));
            out.push_str("-->");
        }
        NodeType::PI => {
            if let Some(pi) = n.pi() {
                out.push_str("<?");
                out.push_str(pi.target);
                if let Some(v) = pi.value {
                    out.push(' ');
                    out.push_str(v);
                }
                out.push_str("?>");
            }
        }
        NodeType::Root => {
            for c in n.children() {
                canonicalize(c, out);
            }
        }
    }
}

fn push_expanded(out: &mut String, ns: Option<&str>, local: &str) {
    if let Some(ns) = ns {
        out.push('{');
        out.push_str(ns);
        out.push('}');
    }
    out.push_str(local);
}

fn escape_into(s: &str, out: &mut String, attr: bool) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
}

/// String value of a result: concatenated text nodes if it parses as XML, else the raw text.
fn string_value_of(serialized: &str) -> String {
    let text = parseable_text(serialized);
    match Document::parse(&text) {
        Ok(doc) => doc
            .root()
            .descendants()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect(),
        Err(_) => serialized.to_string(),
    }
}

pub fn normalize_space(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_trailing_newlines(s: &str) -> &str {
    s.trim_end_matches(['\n', '\r'])
}

fn one_line(s: &str) -> String {
    clip(&normalize_space(s))
}

fn clip(s: &str) -> String {
    const MAX: usize = 120;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let cut: String = s.chars().take(MAX).collect();
        format!("{cut}…")
    }
}

/// Harness self-tests, run with `--self-test` (a `harness = false` binary never runs `#[test]`).
pub mod self_test {
    use super::*;

    fn out(xml: &str) -> RunOutput {
        RunOutput {
            primary: Some(ResultDocument {
                serialized: xml.to_string(),
            }),
            ..Default::default()
        }
    }

    fn check(v: Verdict, want: Outcome) {
        assert_eq!(v.outcome, want, "verdict message: {:?}", v.message);
    }
    fn cx() -> CompareContext<'static> {
        CompareContext {
            set_dir: Path::new("."),
            lenient_xml: false,
        }
    }
    /// The `--lenient-xml` context.
    fn lenient_cx() -> CompareContext<'static> {
        CompareContext {
            set_dir: Path::new("."),
            lenient_xml: true,
        }
    }

    fn assert_xml(expected: &str, ignore_prefixes: bool) -> Assertion {
        Assertion::AssertXml {
            expected: expected.into(),
            file: None,
            ignore_prefixes,
            xml_version: None,
        }
    }

    /// A strict-only failure must be recognisable as one, and name the kind and the values.
    fn check_ns_mismatch(v: Verdict, kind: &str, needles: &[&str]) {
        assert_eq!(v.outcome, Outcome::Fail, "expected a failure, got {:?}", v);
        let m = v.message.unwrap_or_default();
        assert!(
            m.starts_with(NS_MISMATCH),
            "message does not start with the marker: {m}"
        );
        assert!(
            m.contains(kind),
            "message does not name the kind {kind:?}: {m}"
        );
        for n in needles {
            assert!(m.contains(n), "message does not mention {n:?}: {m}");
        }
    }

    fn assert_xpath_over_result() {
        let a = Assertion::Assert {
            xpath: "/doc/a = 'x' and count(//b) = 2".into(),
            ns: vec![],
        };
        let v = evaluate(&a, &out("<doc><a>x</a><b/><b/></doc>"), &cx());
        check(v.clone(), Outcome::Pass);
        let a = Assertion::Assert {
            xpath: "$result/doc/a = 'y'".into(),
            ns: vec![],
        };
        check(
            evaluate(&a, &out("<doc><a>x</a></doc>"), &cx()),
            Outcome::Fail,
        );
    }

    /// Attribute order, the XML declaration and adjacent-text merging are never significant —
    /// that part of the old `assert_xml_ignores_prefixes_and_attribute_order` check is
    /// unchanged. Its *other* half asserted that prefixes are ignored by default, which is the
    /// defect being fixed; it now lives in `assert_xml_element_prefix_is_significant`.
    fn assert_xml_ignores_attribute_order() {
        let a = assert_xml(r#"<doc xmlns="urn:x" b="2" a="1">t<!--c--></doc>"#, false);
        let actual = r#"<?xml version="1.0"?><doc xmlns="urn:x" a="1" b="2">t<!--c--></doc>"#;
        check(evaluate(&a, &out(actual), &cx()), Outcome::Pass);
        let v = evaluate(&a, &out(r#"<doc xmlns="urn:x" a="1" b="3">t</doc>"#), &cx());
        check(v.clone(), Outcome::Fail);
        assert!(
            v.message
                .unwrap_or_default()
                .starts_with("assert-xml mismatch"),
            "content mismatch must not be reported as a namespace mismatch"
        );
    }

    /// Defect A: `<p:r xmlns:p="urn:r"/>` and `<q:r xmlns:q="urn:r"/>` are *not* the same
    /// canonical XML, and the catalog's default is `ignore-prefixes="false"`.
    fn assert_xml_element_prefix_is_significant() {
        let expected = r#"<p:r xmlns:p="urn:r"/>"#;
        let actual = r#"<q:r xmlns:q="urn:r"/>"#;
        check_ns_mismatch(
            evaluate(&assert_xml(expected, false), &out(actual), &cx()),
            "element prefix",
            &["\"p\"", "\"q\""],
        );
        check(
            evaluate(&assert_xml(expected, true), &out(actual), &cx()),
            Outcome::Pass,
        );
        check(
            evaluate(&assert_xml(expected, false), &out(actual), &lenient_cx()),
            Outcome::Pass,
        );
        // A prefix against no prefix at all, in both directions.
        check_ns_mismatch(
            evaluate(
                &assert_xml(r#"<p:r xmlns:p="urn:r"/>"#, false),
                &out(r#"<r xmlns="urn:r"/>"#),
                &cx(),
            ),
            "element prefix",
            &["(no prefix)"],
        );
    }

    /// An extra namespace declaration changes the canonical form even when nothing uses it.
    fn assert_xml_extra_namespace_declaration_is_significant() {
        let expected = "<r><a/></r>";
        let actual = r#"<r xmlns:xs="http://www.w3.org/2001/XMLSchema"><a/></r>"#;
        check_ns_mismatch(
            evaluate(&assert_xml(expected, false), &out(actual), &cx()),
            "in-scope namespaces",
            &["actual has extra [xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"]"],
        );
        check(
            evaluate(&assert_xml(expected, true), &out(actual), &cx()),
            Outcome::Pass,
        );
        check(
            evaluate(&assert_xml(expected, false), &out(actual), &lenient_cx()),
            Outcome::Pass,
        );
        // Inherited bindings count too: the declaration is on the parent, the report is on the
        // child whose scope it entered (the parent is reported first, so check the child alone).
        let v = evaluate(
            &assert_xml("<r><a/></r>", false),
            &out(r#"<r><a xmlns:foo="urn:foo"/></r>"#),
            &cx(),
        );
        check_ns_mismatch(
            v,
            "in-scope namespaces",
            &["/r[1]/a[1]", "extra [xmlns:foo=\"urn:foo\"]"],
        );
        // …and a *missing* one is reported as such.
        let v = evaluate(
            &assert_xml(r#"<r xmlns:foo="urn:foo"/>"#, false),
            &out("<r/>"),
            &cx(),
        );
        check_ns_mismatch(
            v,
            "in-scope namespaces",
            &["actual lacks [xmlns:foo=\"urn:foo\"]"],
        );
        // A default-namespace difference, which is the other half of the same story.
        let v = evaluate(
            &assert_xml(r#"<r xmlns="urn:d"><a/></r>"#, false),
            &out(r#"<r xmlns="urn:d"><a xmlns=""/></r>"#),
            &cx(),
        );
        assert_eq!(
            v.outcome,
            Outcome::Fail,
            "an undeclared default namespace changes the expanded name"
        );
    }

    fn assert_xml_attribute_prefix_is_significant() {
        let expected = r#"<r xmlns:p="urn:p" xmlns:q="urn:p" p:a="1"/>"#;
        let actual = r#"<r xmlns:p="urn:p" xmlns:q="urn:p" q:a="1"/>"#;
        check_ns_mismatch(
            evaluate(&assert_xml(expected, false), &out(actual), &cx()),
            "attribute prefix",
            &["{urn:p}a", "\"p\"", "\"q\""],
        );
        check(
            evaluate(&assert_xml(expected, true), &out(actual), &cx()),
            Outcome::Pass,
        );
    }

    /// The bindings are a *set*: the order they were written in is not part of the document.
    fn assert_xml_namespace_declaration_order_is_not_significant() {
        let expected = r#"<p:r xmlns:p="urn:p" xmlns:q="urn:q" xmlns:z="urn:z"/>"#;
        let actual = r#"<p:r xmlns:z="urn:z" xmlns:q="urn:q" xmlns:p="urn:p"/>"#;
        check(
            evaluate(&assert_xml(expected, false), &out(actual), &cx()),
            Outcome::Pass,
        );
        // Redeclaring an inherited binding on a child is likewise invisible.
        let a = assert_xml(r#"<p:r xmlns:p="urn:p"><p:c/></p:r>"#, false);
        check(
            evaluate(
                &a,
                &out(r#"<p:r xmlns:p="urn:p"><p:c xmlns:p="urn:p"/></p:r>"#),
                &cx(),
            ),
            Outcome::Pass,
        );
    }

    fn assert_xml_fragments_and_text() {
        check(
            evaluate(&assert_xml("<a/><b/>", false), &out("<a/><b/>"), &cx()),
            Outcome::Pass,
        );
        check(
            evaluate(&assert_xml("hello", false), &out("hello"), &cx()),
            Outcome::Pass,
        );
        // The wrapper the harness adds around a fragment must not hide or invent a namespace
        // difference: these two fragments are prefixed, multi-root, and equal.
        let frag = r#"<p:a xmlns:p="urn:p"/>text<q:b xmlns:q="urn:q"/>"#;
        check(
            evaluate(&assert_xml(frag, false), &out(frag), &cx()),
            Outcome::Pass,
        );
        // …and a prefix difference inside a fragment is still caught, at the right path.
        let v = evaluate(
            &assert_xml(frag, false),
            &out(r#"<p:a xmlns:p="urn:p"/>text<r:b xmlns:r="urn:q"/>"#),
            &cx(),
        );
        check_ns_mismatch(v, "element prefix", &["/q:b[1]"]);
    }

    /// Defect B: the comparison offset is a byte offset into escaped canonical text, so it
    /// lands inside a multi-byte character whenever the results differ after one.
    fn assert_xml_mismatch_after_multibyte_text() {
        let emoji = "🙂".repeat(20);
        let a = assert_xml(&format!("<r>{emoji}A</r>"), false);
        let v = evaluate(&a, &out(&format!("<r>{emoji}B</r>")), &cx());
        check(v.clone(), Outcome::Fail);
        let m = v.message.unwrap_or_default();
        assert!(
            m.starts_with("assert-xml mismatch at byte "),
            "unexpected message: {m}"
        );
        // Also exercise the helper directly at every offset in and around a 4-byte character.
        let s = format!("ab{emoji}cd");
        for at in 0..=s.len() + 4 {
            let _ = tail_from(&s, at);
        }
    }

    /// A panic while evaluating assertions is one case's `harnessError`, not the end of the run.
    fn panic_in_assertion_evaluation_is_a_harness_error() {
        // The default hook would print this deliberate panic and make the run look broken.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let v = guard(|| panic!("boom in an assertion"));
        std::panic::set_hook(hook);
        check(v.clone(), Outcome::HarnessError);
        let m = v.message.unwrap_or_default();
        assert!(
            m.contains("panic while evaluating assertions"),
            "unexpected message: {m}"
        );
        assert!(
            m.contains("boom in an assertion"),
            "the panic message is lost: {m}"
        );
        // The guard must be transparent when nothing panics.
        check(guard(Verdict::pass), Outcome::Pass);
    }

    /// Task C: an assertion the harness cannot really check must never be reported as a pass.
    fn unverifiable_assertions_are_harness_errors() {
        let o = out("<a/>");
        for a in [
            Assertion::AssertWarning,
            Assertion::AssertCount("2".into()),
            Assertion::AssertType {
                xpath: "document-node()".into(),
                ns: vec![],
            },
            Assertion::AssertDeepEq {
                xpath: "1234,5678".into(),
                ns: vec![],
            },
            Assertion::AssertPermutation {
                xpath: "1,2".into(),
                ns: vec![],
            },
            Assertion::AllOf(Vec::new()),
            Assertion::Error {
                code: String::new(),
            },
        ] {
            check(evaluate(&a, &o, &cx()), Outcome::HarnessError);
        }
        // `<not>` must not be satisfied by an unrelated failure of the transformation.
        let err = RunOutput {
            error: Some(xslt2::XsltError::new("XTDE0030", "boom")),
            ..Default::default()
        };
        let n = Assertion::Not(Box::new(Assertion::Assert {
            xpath: "true()".into(),
            ns: vec![],
        }));
        check(evaluate(&n, &err, &cx()), Outcome::Fail);
        // …but it still works when it is an error the case is talking about.
        let n = Assertion::Not(Box::new(Assertion::Error {
            code: "XTSE0010".into(),
        }));
        check(evaluate(&n, &err, &cx()), Outcome::Pass);
        // A whitespace-only result is a text node, not an empty sequence.
        check(
            evaluate(&Assertion::AssertEmpty, &out("   "), &cx()),
            Outcome::Fail,
        );
        check(
            evaluate(&Assertion::AssertEmpty, &out("\n"), &cx()),
            Outcome::Pass,
        );
        // A relative result-document URI matches only at a path-segment boundary.
        assert!(result_document_uri_matches("file:///t/out.xml", "out.xml"));
        assert!(result_document_uri_matches("out.xml", "out.xml"));
        assert!(!result_document_uri_matches(
            "file:///t/checkout.xml",
            "out.xml"
        ));
    }

    fn error_assertions() {
        let err = RunOutput {
            error: Some(xslt2::XsltError::new("XTSE0010", "bad")),
            ..Default::default()
        };
        check(
            evaluate(
                &Assertion::Error {
                    code: "XTSE0010".into(),
                },
                &err,
                &cx(),
            ),
            Outcome::Pass,
        );
        check(
            evaluate(&Assertion::Error { code: "*".into() }, &err, &cx()),
            Outcome::Pass,
        );
        check(
            evaluate(
                &Assertion::Error {
                    code: "XTSE0020".into(),
                },
                &err,
                &cx(),
            ),
            Outcome::WrongError,
        );
        check(
            evaluate(
                &Assertion::Error {
                    code: "XTSE0010".into(),
                },
                &out("<a/>"),
                &cx(),
            ),
            Outcome::Fail,
        );
        let a = Assertion::Assert {
            xpath: "true()".into(),
            ns: vec![],
        };
        check(evaluate(&a, &err, &cx()), Outcome::Fail);
        let any = Assertion::AnyOf(vec![
            Assertion::Error {
                code: "XTSE0020".into(),
            },
            Assertion::Error {
                code: "XTSE0010".into(),
            },
        ]);
        check(evaluate(&any, &err, &cx()), Outcome::Pass);
    }

    fn string_value_and_serialization_matches() {
        let a = Assertion::AssertStringValue {
            expected: " a  b ".into(),
            normalize_space: true,
        };
        check(
            evaluate(&a, &out("<d>a <e>b</e></d>"), &cx()),
            Outcome::Pass,
        );
        let a = Assertion::SerializationMatches {
            regex: "^<d>a\\s".into(),
            flags: "".into(),
            file: None,
        };
        let v = evaluate(&a, &out("<d>a <e>b</e></d>"), &cx());
        check(v.clone(), Outcome::Pass);
        let a = Assertion::SerializationMatches {
            regex: "^<D/".into(),
            flags: "i".into(),
            file: None,
        };
        check(evaluate(&a, &out("<d/>"), &cx()), Outcome::Pass);
    }

    fn result_document_and_message() {
        let mut o = out("<a/>");
        o.secondary.push((
            "file:///tmp/out/x.xml".into(),
            ResultDocument {
                serialized: "<r>1</r>".into(),
            },
        ));
        o.messages.push("<m>hi</m>".into());
        let a = Assertion::AssertResultDocument {
            uri: "x.xml".into(),
            inner: Box::new(Assertion::Assert {
                xpath: "/r = 1".into(),
                ns: vec![],
            }),
        };
        check(evaluate(&a, &o, &cx()), Outcome::Pass);
        let a = Assertion::AssertMessage(Box::new(Assertion::AssertStringValue {
            expected: "hi".into(),
            normalize_space: false,
        }));
        check(evaluate(&a, &o, &cx()), Outcome::Pass);
    }

    fn namespaces_from_assertion_scope() {
        let a = Assertion::Assert {
            xpath: "/x:doc/@x:a = 'v'".into(),
            ns: vec![("x".into(), "urn:x".into())],
        };
        let v = evaluate(&a, &out(r#"<p:doc xmlns:p="urn:x" p:a="v"/>"#), &cx());
        check(v.clone(), Outcome::Pass);
    }

    /// An expected code may be written `err:CODE`, `Q{uri}CODE` or bare; all name the same error.
    fn error_codes_compare_by_local_part() {
        let processor = |code: &str| xslt2::XsltError::new(code, "m");
        assert!(code_matches("XTSE0010", &processor("XTSE0010")));
        assert!(code_matches("err:XTSE0010", &processor("XTSE0010")));
        assert!(code_matches(
            "Q{http://www.w3.org/2005/xqt-errors}XTDE1420",
            &processor("XTDE1420")
        ));
        assert!(code_matches(
            "Q{http://example.com/err}XXXX8888",
            &processor("my:XXXX8888")
        ));
        assert!(code_matches("Q{}XX99", &processor("XX99")));
        assert!(code_matches("*", &processor("anything")));
        assert!(!code_matches(
            "Q{http://www.w3.org/2005/xqt-errors}XTDE1420",
            &processor("XTDE1425")
        ));
        assert!(!code_matches("", &processor("XTSE0010")));
    }

    /// When the catalog writes an EQName and the error carries an error QName of
    /// its own — which only `fn:error` produces — both halves must match.
    fn raised_error_codes_compare_the_namespace_too() {
        let raised = |ns: &str, local: &str| xslt2::XsltError::raised(ns, local, "why");

        // Same namespace, same local name.
        assert!(code_matches("Q{urn:e}boom", &raised("urn:e", "boom")));
        // Same local name, different namespace: a different error.
        assert!(!code_matches("Q{urn:e}boom", &raised("urn:other", "boom")));
        // Same namespace, different local name.
        assert!(!code_matches("Q{urn:e}boom", &raised("urn:e", "bang")));
        // An error QName in no namespace against `Q{}local`.
        assert!(code_matches("Q{}bare", &raised("", "bare")));
        assert!(!code_matches("Q{urn:e}bare", &raised("", "bare")));
        // An expected code with no namespace information still compares by
        // local part alone, whatever namespace the error carries.
        assert!(code_matches("boom", &raised("urn:e", "boom")));
        assert!(code_matches("err:boom", &raised("urn:e", "boom")));
        assert!(code_matches("*", &raised("urn:e", "boom")));
        // And `fn:error`'s own default QName keeps matching its bare code.
        assert!(code_matches(
            "FOER0000",
            &raised("http://www.w3.org/2005/xqt-errors", "FOER0000")
        ));
        assert!(code_matches(
            "Q{http://www.w3.org/2005/xqt-errors}FOER0000",
            &raised("http://www.w3.org/2005/xqt-errors", "FOER0000")
        ));
    }

    /// The per-owner `notImplemented` table reads the owner off the engine's own
    /// message, so the two formats must agree; a message without an owner is
    /// not guessed at.
    fn notimpl_messages_name_their_owner() {
        use crate::report::notimpl_owner;
        let engine = xslt2::XsltError::not_implemented("xsl:sort", "M2d");
        assert_eq!(notimpl_owner(&engine.message), Some("M2d"));
        let engine = xslt2::XsltError::not_implemented("the output method \"x (y)\"", "M3a");
        assert_eq!(notimpl_owner(&engine.message), Some("M3a"));
        assert_eq!(
            notimpl_owner("xsl:sort is not implemented (M4)"),
            Some("M4")
        );
        assert_eq!(notimpl_owner("engine not implemented"), None);
        assert_eq!(notimpl_owner("xsl:sort is not implemented (soon)"), None);
        assert_eq!(notimpl_owner("xsl:sort is not implemented ()"), None);
    }

    /// Run every self-test; panics on the first failure. Returns the number of checks run.
    pub fn run() -> usize {
        assert_xpath_over_result();
        assert_xml_ignores_attribute_order();
        assert_xml_fragments_and_text();
        error_assertions();
        string_value_and_serialization_matches();
        result_document_and_message();
        namespaces_from_assertion_scope();
        assert_xml_element_prefix_is_significant();
        assert_xml_extra_namespace_declaration_is_significant();
        assert_xml_attribute_prefix_is_significant();
        assert_xml_namespace_declaration_order_is_not_significant();
        assert_xml_mismatch_after_multibyte_text();
        panic_in_assertion_evaluation_is_a_harness_error();
        unverifiable_assertions_are_harness_errors();
        error_codes_compare_by_local_part();
        raised_error_codes_compare_the_namespace_too();
        notimpl_messages_name_their_owner();
        17
    }
}
