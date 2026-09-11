//! Assertion evaluation (design sketch §2.4) against a `RunOutput`.
//!
//! `assert` XPaths are evaluated with the `xsd-schema` XPath 2.0 engine over the serialized
//! result re-parsed with roxmltree (`RoXmlNavigator`), so the comparison is independent of the
//! engine's own tree type. `assert-xml` compares canonical forms (expanded names, sorted
//! attributes, merged text); `serialization-matches` uses `fn:matches`.

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
        Verdict { outcome: Outcome::Pass, message: None }
    }
    fn fail(m: impl Into<String>) -> Self {
        Verdict { outcome: Outcome::Fail, message: Some(m.into()) }
    }
    fn wrong_error(m: impl Into<String>) -> Self {
        Verdict { outcome: Outcome::WrongError, message: Some(m.into()) }
    }
    fn harness(m: impl Into<String>) -> Self {
        Verdict { outcome: Outcome::HarnessError, message: Some(m.into()) }
    }
}

pub struct CompareContext<'a> {
    /// Test-set directory, for `file=` attributes on assertions.
    pub set_dir: &'a Path,
}

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
const FN_NS: &str = "http://www.w3.org/2005/xpath-functions";
const XSLT_NS: &str = "http://www.w3.org/1999/XSL/Transform";

pub fn evaluate(a: &Assertion, out: &RunOutput, cx: &CompareContext<'_>) -> Verdict {
    use Assertion::*;
    match a {
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
                    Some(w) if w.outcome <= v.outcome => Verdict { message: join(w.message, v.message), ..w },
                    Some(w) => Verdict { outcome: v.outcome, message: join(w.message, v.message) },
                    None => v,
                });
            }
            worst.unwrap_or_else(|| Verdict::fail("empty any-of"))
        }
        Not(inner) => match evaluate(inner, out, cx).outcome {
            Outcome::Pass => Verdict::fail(format!("negated assertion <{}> passed", inner.label())),
            Outcome::HarnessError => Verdict::harness(format!("cannot evaluate negated <{}>", inner.label())),
            _ => Verdict::pass(),
        },
        Error { code } => match &out.error {
            Some(e) if code_matches(code, &e.code) => Verdict::pass(),
            Some(e) => Verdict::wrong_error(format!("expected error {code}, got {}: {}", e.code, e.message)),
            None => Verdict::fail(format!("expected error {code} but the transformation succeeded")),
        },
        AssertSerializationError { code } => match &out.error {
            Some(e) if code_matches(code, &e.code) => Verdict::pass(),
            Some(e) => Verdict::wrong_error(format!("expected serialization error {code}, got {}: {}", e.code, e.message)),
            None => Verdict::fail(format!("expected serialization error {code} but serialization succeeded")),
        },
        _ if out.error.is_some() => {
            let e = out.error.as_ref().unwrap();
            Verdict::fail(format!("unexpected error {}: {}", e.code, e.message))
        }
        Assert { xpath, ns } => with_primary(out, |doc| match eval_xpath(xpath, ns, doc, ebv) {
            Ok(true) => Verdict::pass(),
            Ok(false) => Verdict::fail(format!("assert `{}` is false", one_line(xpath))),
            Err(m) => Verdict::harness(m),
        }),
        AssertXml { expected, file, xml_version, .. } => {
            if xml_version.as_deref() == Some("1.1") {
                return Verdict::harness("assert-xml with xml-version 1.1");
            }
            let expected = match load_text(expected, file, cx) {
                Ok(t) => t,
                Err(m) => return Verdict::harness(m),
            };
            match &out.primary {
                None => Verdict::fail("no primary result"),
                Some(p) => match compare_xml(&expected, &p.serialized) {
                    Ok(()) => Verdict::pass(),
                    Err(m) => Verdict::fail(m),
                },
            }
        }
        AssertStringValue { expected, normalize_space: normalize } => match &out.primary {
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
                    Verdict::fail(format!("string value mismatch: expected {:?}, got {:?}", clip(&e), clip(&a)))
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
                    Ok(false) => Verdict::fail(format!("serialization does not match /{}/{}", clip(&regex), flags)),
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
                        Verdict::fail(format!("serialization mismatch: expected {:?}, got {:?}", clip(&expected), clip(&p.serialized)))
                    }
                }
            }
        }
        AssertResultDocument { uri, inner } => {
            let found = out.secondary.iter().find(|(u, _)| u == uri || u.ends_with(uri.as_str()));
            match found {
                None => Verdict::fail(format!("no result document {uri} (have: {:?})", out.secondary.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>())),
                Some((_, doc)) => {
                    let view = RunOutput { primary: Some(doc.clone()), ..Default::default() };
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
                let view = RunOutput { primary: Some(ResultDocument { serialized: m.clone() }), ..Default::default() };
                let v = evaluate(inner, &view, cx);
                if v.outcome == Outcome::Pass {
                    return v;
                }
                last = v;
            }
            last
        }
        AssertWarning => Verdict::pass(),
        AssertEmpty => match &out.primary {
            None => Verdict::pass(),
            Some(p) if p.serialized.trim().is_empty() => Verdict::pass(),
            Some(p) => Verdict::fail(format!("expected empty result, got {:?}", clip(&p.serialized))),
        },
        AssertTrue => bool_result(out, true),
        AssertFalse => bool_result(out, false),
        AssertEq { xpath, ns } => xpath_over_result(out, &format!("$result eq ({xpath})"), ns),
        AssertDeepEq { xpath, ns } => xpath_over_result(out, &format!("deep-equal($result, ({xpath}))"), ns),
        AssertType { xpath, ns } => xpath_over_result(out, &format!("$result instance of {xpath}"), ns),
        AssertCount(n) => xpath_over_result(out, &format!("count($result) = {n}"), &Vec::new()),
        AssertPermutation { .. } => Verdict::harness("assert-permutation needs a raw sequence result"),
        AssertPostureAndSweep => Verdict::harness("assert-posture-and-sweep is a streaming (XSLT 3.0) assertion"),
        Unknown(name) => Verdict::harness(format!("unknown assertion <{name}>")),
    }
}

fn join(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, None) => a,
        (None, b) => b,
    }
}

/// `*` matches any code; a prefixed code (`err:XTSE0010`) is compared by local part.
fn code_matches(expected: &str, actual: &str) -> bool {
    let local = |s: &str| s.rsplit(':').next().unwrap_or(s).trim().to_string();
    expected == "*" || local(expected) == local(actual)
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
    let ctx = XPathContext::new(&names).with_namespaces(NamespaceContextSnapshot { default_ns: None, bindings });
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
    let expr = XPathExpr::compile_with_vars("matches($s, $re, $flags)", &ctx, &["s", "re", "flags"])
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

fn load_text(inline: &str, file: &Option<String>, cx: &CompareContext<'_>) -> Result<String, String> {
    match file {
        Some(f) => {
            let p = cx.set_dir.join(f);
            std::fs::read_to_string(&p).map_err(|e| format!("cannot read expected file {}: {e}", p.display()))
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

fn compare_xml(expected: &str, actual: &str) -> Result<(), String> {
    let e_text = parseable_text(expected);
    let a_text = parseable_text(actual);
    let e_doc = Document::parse(&e_text).map_err(|e| format!("expected XML does not parse: {e}"))?;
    let a_doc = Document::parse(&a_text).map_err(|e| format!("actual result does not parse: {e}"))?;
    let mut e_canon = String::new();
    let mut a_canon = String::new();
    for n in top_level(&e_doc) {
        canonicalize(n, &mut e_canon);
    }
    for n in top_level(&a_doc) {
        canonicalize(n, &mut a_canon);
    }
    if e_canon == a_canon {
        Ok(())
    } else {
        let at = e_canon.bytes().zip(a_canon.bytes()).position(|(x, y)| x != y).unwrap_or(e_canon.len().min(a_canon.len()));
        let ctx = |s: &str| clip(&s[at.saturating_sub(30).min(s.len())..]);
        Err(format!("assert-xml mismatch at byte {at}: expected …{}, got …{}", ctx(&e_canon), ctx(&a_canon)))
    }
}

/// Canonical form: expanded names, attributes sorted by (namespace, local name), text escaped,
/// adjacent text merged by the parser, namespace declarations ignored (prefixes are irrelevant).
fn canonicalize(n: Node, out: &mut String) {
    match n.node_type() {
        NodeType::Element => {
            out.push('<');
            push_expanded(out, n.tag_name().namespace(), n.tag_name().name());
            let mut attrs: Vec<_> = n.attributes().map(|a| (a.namespace().unwrap_or("").to_string(), a.name().to_string(), a.value().to_string())).collect();
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
        Ok(doc) => doc.root().descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect(),
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
        RunOutput { primary: Some(ResultDocument { serialized: xml.to_string() }), ..Default::default() }
    }

    fn check(v: Verdict, want: Outcome) {
        assert_eq!(v.outcome, want, "verdict message: {:?}", v.message);
    }
    fn cx() -> CompareContext<'static> {
        CompareContext { set_dir: Path::new(".") }
    }

    fn assert_xpath_over_result() {
        let a = Assertion::Assert { xpath: "/doc/a = 'x' and count(//b) = 2".into(), ns: vec![] };
        let v = evaluate(&a, &out("<doc><a>x</a><b/><b/></doc>"), &cx());
        check(v.clone(), Outcome::Pass);
        let a = Assertion::Assert { xpath: "$result/doc/a = 'y'".into(), ns: vec![] };
        check(evaluate(&a, &out("<doc><a>x</a></doc>"), &cx()), Outcome::Fail);
    }

    fn assert_xml_ignores_prefixes_and_attribute_order() {
        let a = Assertion::AssertXml {
            expected: r#"<p:doc xmlns:p="urn:x" b="2" a="1">t<!--c--></p:doc>"#.into(),
            file: None,
            ignore_prefixes: false,
            xml_version: None,
        };
        let actual = r#"<?xml version="1.0"?><doc xmlns="urn:x" a="1" b="2">t<!--c--></doc>"#;
        check(evaluate(&a, &out(actual), &cx()), Outcome::Pass);
        let v = evaluate(&a, &out(r#"<doc xmlns="urn:x" a="1" b="3">t</doc>"#), &cx());
        check(v.clone(), Outcome::Fail);
    }

    fn assert_xml_fragments_and_text() {
        let a = Assertion::AssertXml { expected: "<a/><b/>".into(), file: None, ignore_prefixes: false, xml_version: None };
        check(evaluate(&a, &out("<a/><b/>"), &cx()), Outcome::Pass);
        let a = Assertion::AssertXml { expected: "hello".into(), file: None, ignore_prefixes: false, xml_version: None };
        check(evaluate(&a, &out("hello"), &cx()), Outcome::Pass);
    }

    fn error_assertions() {
        let err = RunOutput { error: Some(xslt2::XsltError::new("XTSE0010", "bad")), ..Default::default() };
        check(evaluate(&Assertion::Error { code: "XTSE0010".into() }, &err, &cx()), Outcome::Pass);
        check(evaluate(&Assertion::Error { code: "*".into() }, &err, &cx()), Outcome::Pass);
        check(evaluate(&Assertion::Error { code: "XTSE0020".into() }, &err, &cx()), Outcome::WrongError);
        check(evaluate(&Assertion::Error { code: "XTSE0010".into() }, &out("<a/>"), &cx()), Outcome::Fail);
        let a = Assertion::Assert { xpath: "true()".into(), ns: vec![] };
        check(evaluate(&a, &err, &cx()), Outcome::Fail);
        let any = Assertion::AnyOf(vec![Assertion::Error { code: "XTSE0020".into() }, Assertion::Error { code: "XTSE0010".into() }]);
        check(evaluate(&any, &err, &cx()), Outcome::Pass);
    }

    fn string_value_and_serialization_matches() {
        let a = Assertion::AssertStringValue { expected: " a  b ".into(), normalize_space: true };
        check(evaluate(&a, &out("<d>a <e>b</e></d>"), &cx()), Outcome::Pass);
        let a = Assertion::SerializationMatches { regex: "^<d>a\\s".into(), flags: "".into(), file: None };
        let v = evaluate(&a, &out("<d>a <e>b</e></d>"), &cx());
        check(v.clone(), Outcome::Pass);
        let a = Assertion::SerializationMatches { regex: "^<D/".into(), flags: "i".into(), file: None };
        check(evaluate(&a, &out("<d/>"), &cx()), Outcome::Pass);
    }

    fn result_document_and_message() {
        let mut o = out("<a/>");
        o.secondary.push(("file:///tmp/out/x.xml".into(), ResultDocument { serialized: "<r>1</r>".into() }));
        o.messages.push("<m>hi</m>".into());
        let a = Assertion::AssertResultDocument { uri: "x.xml".into(), inner: Box::new(Assertion::Assert { xpath: "/r = 1".into(), ns: vec![] }) };
        check(evaluate(&a, &o, &cx()), Outcome::Pass);
        let a = Assertion::AssertMessage(Box::new(Assertion::AssertStringValue { expected: "hi".into(), normalize_space: false }));
        check(evaluate(&a, &o, &cx()), Outcome::Pass);
    }

    fn namespaces_from_assertion_scope() {
        let a = Assertion::Assert { xpath: "/x:doc/@x:a = 'v'".into(), ns: vec![("x".into(), "urn:x".into())] };
        let v = evaluate(&a, &out(r#"<p:doc xmlns:p="urn:x" p:a="v"/>"#), &cx());
        check(v.clone(), Outcome::Pass);
    }

    /// Run every self-test; panics on the first failure. Returns the number of checks run.
    pub fn run() -> usize {
        assert_xpath_over_result();
        assert_xml_ignores_prefixes_and_attribute_order();
        assert_xml_fragments_and_text();
        error_assertions();
        string_value_and_serialization_matches();
        result_document_and_message();
        namespaces_from_assertion_scope();
        7
    }
}
