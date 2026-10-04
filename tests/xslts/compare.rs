//! Assertion evaluation (design sketch §2.4) against a `RunOutput`.
//!
//! `assert` XPaths are evaluated with the `xsd-schema` XPath 2.0 engine over the serialized
//! result re-parsed with roxmltree (`RoXmlNavigator`), so the comparison is independent of the
//! engine's own tree type. A result that is not a well-formed document (text only, several
//! top-level elements) is re-parsed the same way and its document node rebuilt from the
//! top-level nodes (see `ebv_over_serialized`). Assertions written with the XPath 3.0
//! constructs listed in `xpath30` are rewritten exactly first. `serialization-matches` uses
//! `fn:matches`.
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
use std::path::{Path, PathBuf};

use bumpalo::Bump;
use roxmltree::{Document, Node, NodeType};
use xsd_schema::document::{
    BufferDocNavigator, BufferDocument, BufferDocumentBuilder, BufferDocumentError,
    BufferDocumentOptions,
};
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::table::NameTable;
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{XPathContext, XPathExpr};
use xsd_schema::RoXmlNavigator;

use crate::catalog::{Assertion, NsBindings};
use crate::runner::{ResultDocument, RunOutput, TestRun};

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

#[derive(Clone, Copy)]
pub struct CompareContext<'a> {
    /// Test-set directory, for `file=` attributes on assertions.
    pub set_dir: &'a Path,
    /// `--lenient-xml`: compare `assert-xml` as if every assertion carried
    /// `ignore-prefixes="true"`. Triage aid; not the catalog's semantics.
    pub lenient_xml: bool,
    /// What the case feeds the processor (for the CRLF rule of `serializations_equal`).
    pub inputs: &'a CaseInputs,
    /// The result an `assert` is evaluated over may be an external parsed entity rather than a
    /// well-formed document: the case declares `<output well-formed="no"/>` (or is listed in
    /// `filter::UNDECLARED_NOT_WELL_FORMED`), or the result is a message. Otherwise a result
    /// that is not a well-formed document fails every `assert` over it (see
    /// `ebv_over_serialized`).
    pub entity_results: bool,
}

/// What a case feeds the processor, as far as the comparison needs to know it: the CRLF rule
/// of `serializations_equal` may only be applied when no input can put a carriage return into
/// the result. Built from the catalog's own references; nothing is read until the rule asks.
#[derive(Debug, Default, Clone)]
pub struct CaseInputs {
    /// Stylesheet modules the catalog names; the modules they include or import are followed
    /// when the files are read.
    pub stylesheets: Vec<PathBuf>,
    /// Source documents, collection members and resources the catalog names, as files.
    pub documents: Vec<PathBuf>,
    /// Inline texts: the `<content>` of sources, the `select` of parameters.
    pub texts: Vec<String>,
}

impl CaseInputs {
    /// Everything the catalog names for one prepared run.
    pub fn of(run: &TestRun<'_>) -> Self {
        let mut inputs = CaseInputs::default();
        inputs.stylesheets.extend(run.stylesheet.iter().cloned());
        for s in &run.case.test.stylesheets {
            inputs
                .stylesheets
                .extend(s.file.as_ref().map(|f| run.set.dir.join(f)));
        }
        for s in &run.env.stylesheets {
            inputs.stylesheets.extend(s.path.iter().cloned());
        }
        let sources = run
            .env
            .sources
            .iter()
            .chain(run.env.collections.iter().flat_map(|c| c.sources.iter()));
        for s in sources {
            inputs.documents.extend(s.path.iter().cloned());
            inputs.texts.extend(s.content.iter().cloned());
        }
        for r in &run.env.resources {
            inputs.documents.extend(r.path.iter().cloned());
        }
        for p in run.env.params.iter().chain(run.case.test.params.iter()) {
            inputs.texts.extend(p.select.iter().cloned());
        }
        inputs
    }

    /// Whether any input could put a carriage return into the result: a raw CR or a character
    /// reference to one in any input, or, in stylesheet modules and inline texts, a call that
    /// brings in characters this scan cannot see. An input that cannot be read counts as one
    /// that could.
    fn may_produce_cr(&self) -> bool {
        let read = |p: &Path| {
            std::fs::read(p)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        if self.texts.iter().any(|t| code_may_produce_cr(t)) {
            return true;
        }
        for d in &self.documents {
            match read(d) {
                Some(text) if !has_cr(&text) => {}
                _ => return true,
            }
        }
        let mut seen = BTreeSet::new();
        let mut todo = self.stylesheets.clone();
        while let Some(module) = todo.pop() {
            if !seen.insert(module.clone()) {
                continue;
            }
            let Some(text) = read(&module) else {
                return true;
            };
            if code_may_produce_cr(&text) {
                return true;
            }
            match module_references(&text) {
                Some(hrefs) => {
                    let dir = module.parent().unwrap_or(Path::new("."));
                    todo.extend(hrefs.into_iter().map(|h| dir.join(h)));
                }
                None => return true,
            }
        }
        false
    }
}

/// A raw carriage return, or a character reference to one: `&#xD;` / `&#13;`, in any case and
/// with any number of leading zeros.
fn has_cr(text: &str) -> bool {
    if text.contains('\r') {
        return true;
    }
    text.match_indices("&#").any(|(at, _)| {
        let rest = &text[at + 2..];
        let (digits, radix) = match rest.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (rest, 10),
        };
        let end = digits
            .find(|c: char| !c.is_digit(radix))
            .unwrap_or(digits.len());
        digits[end..].starts_with(';')
            && end > 0
            && u32::from_str_radix(digits[..end].trim_start_matches('0'), radix).ok() == Some(13)
    })
}

/// `has_cr`, or a call of a function that can produce a carriage return the scan cannot see:
/// `codepoints-to-string` (any character at all), `unparsed-text` (external text with its own
/// line ends), `doc`, `document` and `collection` (documents the catalog does not name).
fn code_may_produce_cr(text: &str) -> bool {
    const CALLS: &[&str] = &[
        "codepoints-to-string",
        "unparsed-text",
        "doc",
        "document",
        "collection",
    ];
    has_cr(text)
        || CALLS.iter().any(|name| {
            text.match_indices(name).any(|(at, _)| {
                text[at + name.len()..]
                    .trim_start_matches([' ', '\t', '\n', '\r'])
                    .starts_with('(')
            })
        })
}

/// The `href` of every `xsl:include` / `xsl:import` of a stylesheet module, or `None` when the
/// module cannot be parsed or a reference is not a plain relative path.
fn module_references(text: &str) -> Option<Vec<String>> {
    let doc = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .ok()?;
    let mut hrefs = Vec::new();
    for n in doc.descendants().filter(|n| {
        n.is_element()
            && n.tag_name().namespace() == Some(XSLT_NS)
            && matches!(n.tag_name().name(), "include" | "import")
    }) {
        let href = n.attribute("href")?;
        if href.contains([':', '?', '#', '%']) || href.starts_with('/') {
            return None;
        }
        hrefs.push(href.to_string());
    }
    Some(hrefs)
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
    // The harness could not run the case as specified: no assertion is checked at all — in
    // particular `<error code="*"/>` is not satisfied by the refusal.
    if let Some(m) = &out.harness_error {
        return Verdict::harness(format!("harness: {m}"));
    }
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
        Not(inner) => {
            let v = evaluate(inner, out, cx);
            match v.outcome {
                // A result that is not the well-formed document the case declares fails every
                // assertion over it, negated or not.
                Outcome::Fail if is_not_well_formed(&v) => v,
                Outcome::Pass => {
                    Verdict::fail(format!("negated assertion <{}> passed", inner.label()))
                }
                Outcome::HarnessError => {
                    Verdict::harness(format!("cannot evaluate negated <{}>", inner.label()))
                }
                _ => Verdict::pass(),
            }
        }
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
        Assert { xpath, ns } => ebv_over_primary(out, xpath, ns, cx, || {
            format!("assert `{}` is false", one_line(xpath))
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
        AssertSerialization {
            expected,
            file,
            method,
            ..
        } => {
            let expected = match load_text(expected, file, cx) {
                Ok(t) => t,
                Err(m) => return Verdict::harness(m),
            };
            match &out.primary {
                None => Verdict::fail("no primary result"),
                Some(p) => {
                    if serializations_equal(
                        trim_trailing_newlines(&expected),
                        trim_trailing_newlines(&p.serialized),
                        method.as_deref(),
                        || !cx.inputs.may_produce_cr(),
                    ) {
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
            // §17: "The xsl:message instruction causes the creation of a new document"; "In
            // many cases, the XML document produced using xsl:message will consist of a
            // document node owning a single text node. However, it may contain a more complex
            // structure." The tree "is not technically a final result tree", so the case's
            // `<output well-formed>` does not describe it: a message may always be an entity.
            let cx = &CompareContext {
                entity_results: true,
                ..*cx
            };
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
        AssertEq { xpath, ns } => xpath_over_result(out, &format!("$result eq ({xpath})"), ns, cx),
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

fn xpath_over_result(
    out: &RunOutput,
    xpath: &str,
    ns: &NsBindings,
    cx: &CompareContext<'_>,
) -> Verdict {
    ebv_over_primary(out, xpath, ns, cx, || {
        format!("`{}` is false", one_line(xpath))
    })
}

/// The effective boolean value of an assertion XPath over the primary result: `Pass`, `Fail`
/// with `false_message`, or `HarnessError` when it cannot be evaluated.
fn ebv_over_primary(
    out: &RunOutput,
    xpath: &str,
    ns: &NsBindings,
    cx: &CompareContext<'_>,
    false_message: impl FnOnce() -> String,
) -> Verdict {
    let Some(p) = &out.primary else {
        return Verdict::fail("no primary result");
    };
    match ebv_over_serialized(&p.serialized, xpath, ns, cx.entity_results) {
        Ok(true) => Verdict::pass(),
        Ok(false) => Verdict::fail(false_message()),
        Err(v) => v,
    }
}

/// The marker every "the result is not the well-formed document the case declares" failure
/// starts with, so that `<not>` can pass it through instead of inverting it.
pub const NOT_WELL_FORMED: &str = "result is not a well-formed document:";

fn is_not_well_formed(v: &Verdict) -> bool {
    v.message
        .as_deref()
        .is_some_and(|m| m.starts_with(NOT_WELL_FORMED))
}

/// Re-parse a serialized result and evaluate `xpath` over its document node; `Err` is the
/// verdict when it cannot be evaluated (a `harnessError`, or the not-well-formed failure).
///
/// The catalog evaluates `assert` "with the document node of the result document as the
/// context item" (`admin/catalog-schema.xsd`). A result that is a well-formed document is
/// re-parsed as it is. One that is not — a text-only result, several top-level elements (the
/// `<output well-formed="no"/>` cases) — is a well-formed *external parsed entity* (XML 1.0
/// §4.3.2), and its document node has exactly the entity's top-level nodes as children. That
/// node cannot be represented in roxmltree, whose document always has one root element, so the
/// harness parses the text inside a `<harness-wrap>` element (`parseable_text`) and rebuilds a
/// document from the wrapper's *children* with `xsd-schema`'s `BufferDocumentBuilder`, which
/// accepts any sequence of children under the document node. The wrapper itself never reaches
/// the XPath (it used to, as the document element, so every `/`-rooted assertion saw it).
///
/// That rebuild is only done when the case allows it (`entity_results`): the catalog's
/// `well-formed` attribute "Determines whether the output will be well-formed XML. The default
/// is yes." Without it, a result that is not a well-formed document is not what the case
/// expects, and that is a failure of every `assert` over it — otherwise a stray top-level node
/// next to the expected element (`<out>ok</out>junk` against `/out = 'ok'`) would pass.
fn ebv_over_serialized(
    serialized: &str,
    xpath: &str,
    ns: &NsBindings,
    entity_results: bool,
) -> Result<bool, Verdict> {
    let (text, wrapped) = parse_text(serialized);
    let doc = Document::parse(&text)
        .map_err(|e| Verdict::harness(format!("primary result is not parseable XML: {e}")))?;
    if !wrapped {
        return eval_xpath(xpath, ns, RoXmlNavigator::new(&doc), ebv).map_err(Verdict::harness);
    }
    if !entity_results {
        let top = doc.root_element().children().count();
        let elements = doc
            .root_element()
            .children()
            .filter(Node::is_element)
            .count();
        return Err(Verdict::fail(format!(
            "{NOT_WELL_FORMED} {top} top-level node(s), {elements} of them element(s) (XML 1.0 \
             §2.1: \"There is exactly one element, called the root\"), and the case does not \
             declare <output well-formed=\"no\"/>: got {:?}",
            clip(serialized)
        )));
    }
    let arena = Bump::new();
    let names = NameTable::new();
    let tree =
        entity_document(&arena, &names, &text, doc.root_element()).map_err(Verdict::harness)?;
    eval_xpath(xpath, ns, BufferDocNavigator::new(&tree, tree.root()), ebv)
        .map_err(Verdict::harness)
}

/// The document node of an external parsed entity whose text was parsed inside a wrapper
/// element: the wrapper's children become the document node's children, node for node — names,
/// lexical prefixes, namespace declarations, attributes, text, comments and processing
/// instructions as roxmltree reports them.
fn entity_document<'a>(
    arena: &'a Bump,
    names: &'a NameTable,
    text: &str,
    wrapper: Node,
) -> Result<BufferDocument<'a>, String> {
    let err = |e: BufferDocumentError| format!("cannot rebuild the result document: {e}");
    let mut b = BufferDocumentBuilder::new(arena, names, None, BufferDocumentOptions::default())
        .map_err(err)?;
    for child in wrapper.children() {
        push_node(&mut b, text, child, wrapper).map_err(err)?;
    }
    b.finalize().map_err(err)
}

fn push_node(
    b: &mut BufferDocumentBuilder<'_>,
    text: &str,
    n: Node,
    parent: Node,
) -> Result<(), BufferDocumentError> {
    match n.node_type() {
        NodeType::Element => {
            // The declarations made on this element: its in-scope bindings that its parent
            // does not have. `xmlns=""` (an undeclaration) is kept as roxmltree reports it, as
            // the empty URI on the default prefix, which is how the builder's own parser passes
            // it on.
            let inherited: BTreeSet<(Option<&str>, &str)> = parent
                .namespaces()
                .map(|ns| (ns.name(), ns.uri()))
                .collect();
            let declared: Vec<(&str, &str)> = n
                .namespaces()
                .filter(|ns| ns.name() != Some("xml"))
                .filter(|ns| !inherited.contains(&(ns.name(), ns.uri())))
                .map(|ns| (ns.name().unwrap_or(""), ns.uri()))
                .collect();
            let tag = n.tag_name();
            b.start_element(
                tag.name(),
                tag.namespace().unwrap_or(""),
                &element_prefix(text, n),
                &declared,
            )?;
            for a in n.attributes() {
                b.attribute(
                    a.name(),
                    a.namespace().unwrap_or(""),
                    &attribute_prefix(text, &a),
                    a.value(),
                )?;
            }
            b.end_of_attributes();
            for c in n.children() {
                push_node(b, text, c, n)?;
            }
            b.end_element()
        }
        NodeType::Text => {
            b.text(n.text().unwrap_or(""));
            Ok(())
        }
        NodeType::Comment => b.comment(n.text().unwrap_or("")),
        NodeType::PI => match n.pi() {
            Some(pi) => b.processing_instruction(pi.target, pi.value.unwrap_or("")),
            None => Ok(()),
        },
        // The wrapper's children are never document nodes.
        NodeType::Root => Ok(()),
    }
}

fn ebv<N: xsd_schema::xpath::DomNavigator>(v: &XPathValue<N>) -> Result<bool, String> {
    effective_boolean_value(v).map_err(|e| format!("effective boolean value: {e}"))
}

/// Compile and evaluate an assertion XPath with the result document node as context item and
/// bound to `$result`. Namespaces: the assertion's in-scope bindings plus `xs`, `fn`, `xsl`.
///
/// An assertion the XPath 2.0 engine cannot compile gets one more chance: if it uses one of the
/// XPath 3.0 constructs `xpath30::translate` rewrites *exactly* (an EQName, a
/// `F(…) ! string()` simple map), the rewrite is compiled instead. Anything else — or a rewrite
/// that does not compile either — is reported with the original expression's compile error.
fn eval_xpath<N: xsd_schema::xpath::DomNavigator, T>(
    xpath: &str,
    ns: &NsBindings,
    nav: N,
    f: impl FnOnce(&XPathValue<N>) -> Result<T, String>,
) -> Result<T, String> {
    let names = NameTable::new();
    let compile = |text: &str, ns: &NsBindings| {
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
        XPathExpr::compile_with_vars(text, &ctx, &["result"]).map(|expr| (expr, ctx))
    };
    let (expr, ctx) = match compile(xpath, ns) {
        Ok(compiled) => compiled,
        Err(e) => {
            let original = format!("cannot compile assertion XPath `{}`: {e}", one_line(xpath));
            let translated = crate::xpath30::translate(xpath, ns).ok_or(original.clone())?;
            let mut scope = ns.clone();
            scope.extend(translated.extra_ns);
            compile(&translated.xpath, &scope).map_err(|_| original)?
        }
    };
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
    parse_text(xml).0
}

/// [`parseable_text`], and whether the wrapper was added.
fn parse_text(xml: &str) -> (String, bool) {
    let mut s = xml.trim_start_matches('\u{feff}');
    if let Some(rest) = s.strip_prefix("<?xml") {
        if let Some(end) = rest.find("?>") {
            s = &rest[end + 2..];
        }
    }
    if Document::parse(s).is_ok() {
        (s.to_string(), false)
    } else {
        (format!("<{WRAP}>{s}</{WRAP}>"), true)
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

/// `assert-serialization`: "In principle, the serialization must match exactly. Test drivers are
/// free to ignore differences in the serialization that are known to be irrelevant (that is,
/// capable of being produced by a conformant implementation)" (`admin/catalog-schema.xsd`).
///
/// The one difference ignored here is the representation of a newline. XSLT 2.0 and XQuery 1.0
/// Serialization (Second Edition) (REC-xslt-xquery-serialization-20101214, the edition
/// `xslt20.html` cites) §8, text output method: "A newline character in the instance of the data
/// model MAY be output using any character sequence that is conventionally used to represent a
/// line ending in the chosen system environment"; §5.1.2, XML output method: "the serializer is
/// free to represent it using any character sequence that will be normalized to a newline
/// character by an XML parser". Some expected files were written with CRLF line endings
/// throughout; this processor writes LF.
///
/// The rule is kept narrow, so that it cannot hide a carriage return the result really needs.
/// It applies only to an assertion whose `method` is `text` or `xml` (the two sections quoted),
/// only when the expected text is CRLF *throughout* (every LF preceded by a CR and no other CR),
/// and only when the actual serialization contains no CR at all. That alone is not enough: the
/// text method writes a carriage return of the data model as it is, so a result string
/// `"a\r\nb"` serializes exactly like a CRLF-written newline, and an engine that lost the CR
/// would look like one that chose LF. So the rule also requires `inputs_cannot_produce_cr` —
/// no input of the case can put a CR into the result (`CaseInputs::may_produce_cr`); it is
/// asked last, because it reads files. Then each CRLF of the expected text must correspond to
/// one LF of the actual one, character for character.
fn serializations_equal(
    expected: &str,
    actual: &str,
    method: Option<&str>,
    inputs_cannot_produce_cr: impl FnOnce() -> bool,
) -> bool {
    if expected == actual {
        return true;
    }
    matches!(method, Some("text") | Some("xml"))
        && crlf_throughout(expected)
        && !actual.contains('\r')
        && expected.replace("\r\n", "\n") == actual
        && inputs_cannot_produce_cr()
}

/// At least one CRLF, every LF preceded by a CR, and no CR anywhere else.
fn crlf_throughout(s: &str) -> bool {
    let crlf = s.matches("\r\n").count();
    crlf > 0 && s.matches('\n').count() == crlf && s.matches('\r').count() == crlf
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
    /// A case that names no inputs at all.
    static NO_INPUTS: CaseInputs = CaseInputs {
        stylesheets: Vec::new(),
        documents: Vec::new(),
        texts: Vec::new(),
    };

    /// The default context: a case that declares well-formed output (the catalog default).
    fn cx() -> CompareContext<'static> {
        CompareContext {
            set_dir: Path::new("."),
            lenient_xml: false,
            inputs: &NO_INPUTS,
            entity_results: false,
        }
    }
    /// The `--lenient-xml` context.
    fn lenient_cx() -> CompareContext<'static> {
        CompareContext {
            lenient_xml: true,
            ..cx()
        }
    }
    /// A case that declares `<output well-formed="no"/>`.
    fn entity_cx() -> CompareContext<'static> {
        CompareContext {
            entity_results: true,
            ..cx()
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

    /// A plain `<assert>` with no namespace bindings of its own.
    fn assert_(xpath: &str) -> Assertion {
        Assertion::Assert {
            xpath: xpath.into(),
            ns: vec![],
        }
    }

    /// M2j (E-C7): a result that is not a well-formed document is a document node whose
    /// children are its top-level nodes. `/` must reach them directly — the `<harness-wrap>`
    /// element the parser needs must never be visible — and nothing about them may be lost on
    /// the way: names, prefixes, namespaces, attributes, text, comments, PIs.
    fn assert_over_a_non_well_formed_result_sees_the_document_node() {
        // whitespace-019: a document node owning one text node.
        let text = out("\n      x\n      ");
        for x in [
            "/child::text()",
            "count(/child::node()) = 1",
            r"matches(., '\n\s+x\n\s+')",
            "not(/*)",
            "$result instance of document-node()",
        ] {
            check(evaluate(&assert_(x), &text, &entity_cx()), Outcome::Pass);
        }
        // The wrapper used to be the document element.
        check(evaluate(&assert_("/*"), &text, &entity_cx()), Outcome::Fail);
        check(
            evaluate(&assert_("exists(//harness-wrap)"), &text, &entity_cx()),
            Outcome::Fail,
        );
        // xsl-document-0501: several top-level elements.
        let roots = out("<foo>one</foo><foo>two</foo>");
        check(
            evaluate(
                &assert_(r#"/foo[2] = "two" and count(/*) = 2"#),
                &roots,
                &entity_cx(),
            ),
            Outcome::Pass,
        );
        check(
            evaluate(&assert_(r#"/foo[1] = "two""#), &roots, &entity_cx()),
            Outcome::Fail,
        );
        // version-017: an element followed by a text node.
        check(
            evaluate(
                &assert_("/b[@id='3'][following-sibling::text()[.='Another message']]"),
                &out("<b id='3'/>Another message"),
                &entity_cx(),
            ),
            Outcome::Pass,
        );
        // Prefixes, namespaces and an undeclaration survive the rebuild …
        let ns = out(r#"<p:a xmlns:p="urn:p" p:x="1"/><b xmlns="urn:d"><c xmlns=""/></b>"#);
        for x in [
            "/*[1][name() = 'p:a' and namespace-uri() = 'urn:p' and @*[name() = 'p:x'] = '1']",
            "/*[2][local-name() = 'b' and namespace-uri() = 'urn:d']",
            "/*[2]/*[local-name() = 'c' and namespace-uri() = '']",
            "in-scope-prefixes(/*[1]) = 'p'",
        ] {
            check(evaluate(&assert_(x), &ns, &entity_cx()), Outcome::Pass);
        }
        check(
            evaluate(
                &assert_("/*[1][namespace-uri() = 'urn:q']"),
                &ns,
                &entity_cx(),
            ),
            Outcome::Fail,
        );
        // … and so do comments, processing instructions and their order.
        let misc = out("<!--c--><?pi d?>t<e/>");
        check(
            evaluate(
                &assert_(
                    "/comment() = 'c' and /processing-instruction('pi') = 'd' and /text() = 't' \
                     and /node()[1] instance of comment() and /node()[4] is /e",
                ),
                &misc,
                &entity_cx(),
            ),
            Outcome::Pass,
        );
        // message-0312: the same rule applies to a message, whatever the case declares about
        // its output (§17: a message "is not technically a final result tree").
        let mut o = out("<a/>");
        o.messages
            .push("<smart>Smart element</smart><dummy>dummy element</dummy>".into());
        let a = Assertion::AssertMessage(Box::new(Assertion::AllOf(vec![
            assert_("/smart = 'Smart element'"),
            assert_("/dummy = 'dummy element'"),
        ])));
        check(evaluate(&a, &o, &cx()), Outcome::Pass);
        // A result that is not even an external parsed entity stays a harness error.
        check(
            evaluate(&assert_("true()"), &out("a < b"), &cx()),
            Outcome::HarnessError,
        );
    }

    /// M2j (L-HARNESS-EQNAME, E-C8): assertions in XPath 3.0 syntax are rewritten exactly or
    /// not evaluated at all. The rewritten form must keep namespaces strict — a wrong namespace
    /// is a failure, never a pass.
    fn xpath30_assertions_are_rewritten_exactly() {
        let doc = out(r#"<p:doc xmlns:p="urn:x" p:a="v" a="w"><b>ab</b></p:doc>"#);
        for (x, want) in [
            ("/Q{urn:x}doc/@Q{urn:x}a = 'v'", Outcome::Pass),
            ("/Q{urn:x}doc/@Q{}a = 'w'", Outcome::Pass),
            ("/Q{urn:y}doc", Outcome::Fail),
            ("/Q{}doc", Outcome::Fail),
            ("/Q{urn:x}doc/@Q{urn:y}a = 'v'", Outcome::Fail),
            (
                r#"string-join(string-to-codepoints(/*/b[1])!string(), " ") = "97 98""#,
                Outcome::Pass,
            ),
            (
                r#"string-join(string-to-codepoints(/*/b[1])!string(), " ") = "97 99""#,
                Outcome::Fail,
            ),
            // Not a supported form: the original compile error is reported.
            ("/*!string()", Outcome::HarnessError),
            ("Q{}true()", Outcome::HarnessError),
        ] {
            check(evaluate(&assert_(x), &doc, &cx()), want);
        }
        // The assertion's own bindings still apply next to the invented ones.
        let a = Assertion::Assert {
            xpath: "/x:doc/@Q{urn:x}a = 'v'".into(),
            ns: vec![("x".into(), "urn:x".into())],
        };
        check(evaluate(&a, &doc, &cx()), Outcome::Pass);
        // The original compile error is what a harness error reports.
        let v = evaluate(&assert_("/*!string()"), &doc, &cx());
        assert!(
            v.message
                .unwrap_or_default()
                .contains("cannot compile assertion XPath `/*!string()`"),
            "the original expression must be reported"
        );
    }

    /// M2j (L-CRLF-OUTFILE): an expected serialization written with CRLF throughout matches
    /// LF newlines, for the text and xml methods only, and never hides a carriage return.
    fn crlf_expected_serialization() {
        let text = Some("text");
        assert!(serializations_equal("a\r\nb", "a\nb", text, || true));
        assert!(serializations_equal("a\r\nb", "a\nb", Some("xml"), || true));
        assert!(serializations_equal("a\r\nb", "a\r\nb", text, || true));
        // Other methods, or no method given: exact.
        assert!(!serializations_equal("a\r\nb", "a\nb", None, || true));
        assert!(!serializations_equal(
            "a\r\nb",
            "a\nb",
            Some("html"),
            || true
        ));
        assert!(!serializations_equal(
            "a\r\nb",
            "a\nb",
            Some("xhtml"),
            || true
        ));
        // Mixed line endings, a lone CR, a doubled CR: exact.
        assert!(!serializations_equal("a\r\nb\nc", "a\nb\nc", text, || true));
        assert!(!serializations_equal("a\r\r\nb", "a\nb", text, || true));
        assert!(!serializations_equal("a\r\r\nb", "a\r\nb", text, || true));
        assert!(!serializations_equal("a\rb\r\nc", "a\rb\nc", text, || true));
        // The actual side gets no latitude: a CR it produced must be expected.
        assert!(!serializations_equal("a\nb", "a\r\nb", text, || true));
        assert!(!serializations_equal("a\r\nb", "a\r\nb\nc", text, || true));
        // Content still has to match.
        assert!(!serializations_equal("a\r\nb", "a\nc", text, || true));
        let a = Assertion::AssertSerialization {
            expected: "\r\n  This should be directly at the top.\r\n".into(),
            file: None,
            method: Some("text".into()),
            ignore_prefixes: false,
        };
        check(
            evaluate(&a, &out("\n  This should be directly at the top.\n"), &cx()),
            Outcome::Pass,
        );
        check(
            evaluate(
                &a,
                &out("\n  This should be directly at the bottom.\n"),
                &cx(),
            ),
            Outcome::Fail,
        );
    }

    /// P2-5: the rebuilt document node is for cases that say their result may not be a
    /// well-formed document. Elsewhere such a result fails every `assert` over it — negated or
    /// not — instead of letting a stray top-level node pass unseen.
    fn a_non_well_formed_result_must_be_declared() {
        let junk = out("<out>ok</out>junk");
        let v = evaluate(&assert_("/out = 'ok'"), &junk, &cx());
        check(v.clone(), Outcome::Fail);
        assert!(
            is_not_well_formed(&v),
            "unexpected message: {:?}",
            v.message
        );
        check(
            evaluate(&assert_("/out = 'ok'"), &junk, &entity_cx()),
            Outcome::Pass,
        );
        // `<not>` does not turn the failure into a pass.
        let negated = Assertion::Not(Box::new(assert_("/out = 'ko'")));
        check(evaluate(&negated, &junk, &cx()), Outcome::Fail);
        check(evaluate(&negated, &junk, &entity_cx()), Outcome::Pass);
        // Text only, several roots, an empty result: the same.
        for (result, xpath) in [
            ("abc", "/text() = 'abc'"),
            ("<a/><b/>", "/a and /b"),
            ("", "empty(/node())"),
        ] {
            check(
                evaluate(&assert_(xpath), &out(result), &cx()),
                Outcome::Fail,
            );
            check(
                evaluate(&assert_(xpath), &out(result), &entity_cx()),
                Outcome::Pass,
            );
        }
        // assert-eq goes through the same gate.
        let eq = Assertion::AssertEq {
            xpath: "'abc'".into(),
            ns: vec![],
        };
        check(evaluate(&eq, &out("abc"), &cx()), Outcome::Fail);
        check(evaluate(&eq, &out("abc"), &entity_cx()), Outcome::Pass);
        // A well-formed document is unaffected, prolog and epilog whitespace included.
        for result in [
            "<out>ok</out>",
            "\n<out>ok</out>\n",
            "<?xml version=\"1.0\"?><out>ok</out>",
        ] {
            check(
                evaluate(&assert_("/out = 'ok'"), &out(result), &cx()),
                Outcome::Pass,
            );
        }
        // A secondary result document is held to the same declaration.
        let mut o = out("<a/>");
        o.secondary.push((
            "file:///t/x.xml".into(),
            ResultDocument {
                serialized: "<r/>junk".into(),
            },
        ));
        let rd = Assertion::AssertResultDocument {
            uri: "x.xml".into(),
            inner: Box::new(assert_("/r")),
        };
        check(evaluate(&rd, &o, &cx()), Outcome::Fail);
        // Where the catalog's attribute is read: `<output well-formed="no"/>`, or the one
        // documented correction.
        let case = |name: &str, well_formed: Option<bool>| crate::catalog::TestCase {
            name: name.into(),
            description: String::new(),
            dependencies: Vec::new(),
            environment: None,
            test: crate::catalog::TestSpec {
                output: Some(crate::catalog::OutputSpec {
                    well_formed,
                    ..Default::default()
                }),
                ..Default::default()
            },
            result: Assertion::AllOf(Vec::new()),
            line: 0,
        };
        assert!(crate::filter::result_may_be_entity(&case(
            "whitespace-019",
            Some(false)
        )));
        assert!(!crate::filter::result_may_be_entity(&case("x", None)));
        assert!(!crate::filter::result_may_be_entity(&case("x", Some(true))));
        assert!(crate::filter::result_may_be_entity(&case(
            "xsl-document-0501",
            None
        )));
    }

    /// P2-3: what the harness refuses is never an engine error; in particular `<error
    /// code="*"/>` is not satisfied by it, and neither is a negated or alternative assertion.
    fn harness_refusals_are_harness_errors() {
        let refused = RunOutput {
            harness_error: Some(
                "the catalog source uri \"../x.xml\" is not a plain relative path".into(),
            ),
            ..Default::default()
        };
        for a in [
            Assertion::Error { code: "*".into() },
            Assertion::Error {
                code: "XSLT2-INTERNAL".into(),
            },
            Assertion::Not(Box::new(Assertion::Error {
                code: "XTSE0010".into(),
            })),
            Assertion::AnyOf(vec![
                Assertion::Error { code: "*".into() },
                Assertion::AssertEmpty,
            ]),
            assert_("true()"),
        ] {
            let v = evaluate(&a, &refused, &cx());
            check(v.clone(), Outcome::HarnessError);
            assert!(
                v.message
                    .unwrap_or_default()
                    .contains("not a plain relative path"),
                "the refusal's reason must be reported"
            );
        }
    }

    /// P2-1: the CRLF rule needs inputs that cannot put a carriage return into the result —
    /// the text method writes a data CR as it is, so `"a\r\nb"` from the data model and a
    /// CRLF-written newline look the same.
    fn crlf_rule_needs_inputs_that_cannot_produce_cr() {
        // The data could hold a CR: exact comparison, so a lost CR is a failure.
        assert!(!serializations_equal(
            "a\r\nb",
            "a\nb",
            Some("text"),
            || false
        ));
        assert!(!serializations_equal("a\r\nb", "a\nb", Some("xml"), || {
            false
        }));
        assert!(serializations_equal(
            "a\r\nb",
            "a\r\nb",
            Some("text"),
            || false
        ));
        // The input scan is asked only when everything else already holds.
        assert!(!serializations_equal(
            "a\r\nb",
            "a\nc",
            Some("text"),
            || panic!("asked")
        ));
        // What counts as an input that can produce a CR.
        let texts = |t: &str| CaseInputs {
            texts: vec![t.to_string()],
            ..Default::default()
        };
        for t in [
            "<d>a&#13;</d>",
            "<d>a&#xD;</d>",
            "<d>a&#xd;</d>",
            "<d>a&#x000D;</d>",
            "<d>a&#0013;</d>",
            "<d>a\r</d>",
            "codepoints-to-string((13, 10))",
            "unparsed-text('lines.txt')",
            "doc ('other.xml')",
            "document('other.xml')",
            "collection()",
        ] {
            assert!(texts(t).may_produce_cr(), "{t:?} can produce a CR");
        }
        for t in [
            "<d>a&#10;b</d>",
            "<d>&#130;&#x10D;&#xD</d>",
            "doc-available('x')",
            "'documentation'",
        ] {
            assert!(!texts(t).may_produce_cr(), "{t:?} cannot produce a CR");
        }
        // An input that cannot be read is one that could.
        let missing = CaseInputs {
            documents: vec![PathBuf::from("/nonexistent/xslts-self-test/source.xml")],
            ..Default::default()
        };
        assert!(missing.may_produce_cr());
        let missing = CaseInputs {
            stylesheets: vec![PathBuf::from("/nonexistent/xslts-self-test/a.xsl")],
            ..Default::default()
        };
        assert!(missing.may_produce_cr());
        assert!(!NO_INPUTS.may_produce_cr());
        // Included and imported modules are followed; a reference that is not a plain relative
        // path cannot be, so it counts as a module that could produce a CR.
        let module = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
            <xsl:import href="base.xsl"/><xsl:include href="sub/inc.xsl"/>
            <include href="not-xslt.xsl"/></xsl:stylesheet>"#;
        assert_eq!(
            module_references(module),
            Some(vec!["base.xsl".to_string(), "sub/inc.xsl".to_string()])
        );
        assert_eq!(
            module_references(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:include href="http://x/a.xsl"/></xsl:stylesheet>"#
            ),
            None
        );
        // End to end: the same CRLF expectation passes over clean inputs and fails when a
        // source holds a carriage return.
        let a = Assertion::AssertSerialization {
            expected: "a\r\nb".into(),
            file: None,
            method: Some("text".into()),
            ignore_prefixes: false,
        };
        check(evaluate(&a, &out("a\nb"), &cx()), Outcome::Pass);
        let with_cr = texts("<d>a&#13;\nb</d>");
        let cr_cx = CompareContext {
            inputs: &with_cr,
            ..cx()
        };
        check(evaluate(&a, &out("a\nb"), &cr_cx), Outcome::Fail);
    }

    /// P2-2: an EQName's braced URI may hold quotes, and a rewrite never turns an assertion
    /// that is invalid in XPath 3.0 into a valid XPath 2.0 one.
    fn xpath30_rewrites_respect_braced_uris_and_validity() {
        let quoted = |content: &str| out(&format!(r#"<x xmlns="urn:it's">{content}</x>"#));
        let a = assert_("/Q{urn:it's}x = 'Q{urn:y}z'");
        // XPath 3.0: false over `eqname2:z`, true over `Q{urn:y}z`.
        check(evaluate(&a, &quoted("eqname2:z"), &cx()), Outcome::Fail);
        check(evaluate(&a, &quoted("Q{urn:y}z"), &cx()), Outcome::Pass);
        let doc = out("<a>4</a>");
        for x in ["/a Q{}div 2", "/Q{}child::a", "/Q{}xs:a"] {
            let v = evaluate(&assert_(x), &doc, &cx());
            check(v.clone(), Outcome::HarnessError);
            assert!(
                v.message
                    .unwrap_or_default()
                    .contains(&format!("cannot compile assertion XPath `{x}`")),
                "the original expression's compile error must be reported for {x:?}"
            );
        }
    }

    /// The numbers `--self-test` reports.
    pub struct Summary {
        /// Self-test functions in this module (each holds several checks).
        pub compare_functions: usize,
        /// Individual checks in `xpath30::self_test`.
        pub xpath30_checks: usize,
        /// Individual checks in `engine::self_test`.
        pub engine_checks: usize,
    }

    /// Run every self-test; panics on the first failure.
    pub fn run() -> Summary {
        let functions: &[fn()] = &[
            assert_xpath_over_result,
            assert_xml_ignores_attribute_order,
            assert_xml_fragments_and_text,
            error_assertions,
            string_value_and_serialization_matches,
            result_document_and_message,
            namespaces_from_assertion_scope,
            assert_xml_element_prefix_is_significant,
            assert_xml_extra_namespace_declaration_is_significant,
            assert_xml_attribute_prefix_is_significant,
            assert_xml_namespace_declaration_order_is_not_significant,
            assert_xml_mismatch_after_multibyte_text,
            panic_in_assertion_evaluation_is_a_harness_error,
            unverifiable_assertions_are_harness_errors,
            error_codes_compare_by_local_part,
            raised_error_codes_compare_the_namespace_too,
            notimpl_messages_name_their_owner,
            assert_over_a_non_well_formed_result_sees_the_document_node,
            xpath30_assertions_are_rewritten_exactly,
            crlf_expected_serialization,
            a_non_well_formed_result_must_be_declared,
            harness_refusals_are_harness_errors,
            crlf_rule_needs_inputs_that_cannot_produce_cr,
            xpath30_rewrites_respect_braced_uris_and_validity,
        ];
        for f in functions {
            f();
        }
        Summary {
            compare_functions: functions.len(),
            xpath30_checks: crate::xpath30::self_test(),
            engine_checks: crate::engine::self_test(),
        }
    }
}
