#![allow(dead_code)] // the model is consumed by the engine adapter, not yet by the stub
//! Catalog model and parser for the W3C XSLT 3.0 test suite
//! (`catalog.xml` + `tests/**/_*-test-set.xml`).
//!
//! Vocabulary: namespace `http://www.w3.org/2012/10/xslt-test-catalog`, documented in
//! `admin/catalog-schema.xsd` of the suite. Only what the XSLT 2.0-applicable cases use
//! is modelled in detail; everything else is recorded as a flag so the filter can skip it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use roxmltree::{Document, Node};

pub const CATALOG_NS: &str = "http://www.w3.org/2012/10/xslt-test-catalog";

#[derive(Debug)]
pub struct Catalog {
    pub root: PathBuf,
    pub test_sets: Vec<TestSetRef>,
}

#[derive(Debug, Clone)]
pub struct TestSetRef {
    pub name: String,
    /// Absolute path of the `_<name>-test-set.xml` file.
    pub file: PathBuf,
    /// Path as written in `catalog.xml` (`tests/<family>/<set>/_…`), used for family grouping.
    pub rel_file: String,
}

#[derive(Debug)]
pub struct TestSet {
    pub name: String,
    pub file: PathBuf,
    /// Directory of the test-set file: base for every relative `file` attribute.
    pub dir: PathBuf,
    pub rel_file: String,
    pub description: String,
    pub dependencies: Vec<Dependency>,
    pub environments: HashMap<String, Environment>,
    pub cases: Vec<TestCase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// Local name of the dependency element: `spec`, `feature`, `unicode-version`, …
    pub kind: String,
    pub value: String,
    /// `satisfied="false"` means the test requires the dependency to be *absent*.
    pub satisfied: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub name: Option<String>,
    pub sources: Vec<Source>,
    pub stylesheets: Vec<StylesheetRef>,
    pub schemas: Vec<SchemaRef>,
    pub resources: Vec<Resource>,
    pub collations: Vec<Collation>,
    pub collections: Vec<Collection>,
    pub params: Vec<Param>,
    pub has_package: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Source {
    /// `"."` = initial context item; `"$name"` = bound to a variable; absent = available to `doc()` only.
    pub role: Option<String>,
    pub file: Option<String>,
    pub uri: Option<String>,
    /// Inline document text (`<content>` child, usually CDATA).
    pub content: Option<String>,
    pub validation: Option<String>,
    pub xml_version: Option<String>,
    pub streaming: bool,
    pub select: Option<String>,
    pub defines_stylesheet: bool,
}

#[derive(Debug, Clone, Default)]
pub struct StylesheetRef {
    pub file: Option<String>,
    pub uri: Option<String>,
    pub role: Option<String>,
    pub xml_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SchemaRef {
    pub file: Option<String>,
    pub uri: Option<String>,
    pub role: Option<String>,
    pub xsd_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Resource {
    pub file: Option<String>,
    pub uri: Option<String>,
    pub media_type: Option<String>,
    pub encoding: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Collation {
    pub uri: String,
    pub default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Collection {
    pub uri: Option<String>,
    pub sources: Vec<Source>,
}

/// A QName as written in the catalog, with the prefix resolved against the in-scope namespaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QName {
    pub prefix: Option<String>,
    pub local: String,
    pub namespace: Option<String>,
}

impl std::fmt::Display for QName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.namespace, &self.prefix) {
            (Some(ns), _) => write!(f, "{{{}}}{}", ns, self.local),
            (None, Some(p)) => write!(f, "{}:{}", p, self.local),
            (None, None) => write!(f, "{}", self.local),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: QName,
    /// XPath expression giving the value.
    pub select: Option<String>,
    pub as_type: Option<String>,
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub enum EnvRef {
    Ref(String),
    Inline(Environment),
}

#[derive(Debug)]
pub struct TestCase {
    pub name: String,
    pub description: String,
    pub dependencies: Vec<Dependency>,
    pub environment: Option<EnvRef>,
    pub test: TestSpec,
    pub result: Assertion,
    /// Line of the `<test-case>` element in the test-set file.
    pub line: u32,
}

#[derive(Debug, Default)]
pub struct TestSpec {
    pub stylesheets: Vec<StylesheetRef>,
    pub initial_template: Option<QName>,
    pub initial_mode: Option<QName>,
    pub initial_mode_select: Option<String>,
    pub initial_function: Option<QName>,
    pub params: Vec<Param>,
    pub output: Option<OutputSpec>,
    pub has_package: bool,
    pub has_posture_and_sweep: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OutputSpec {
    pub file: Option<String>,
    pub tree: Option<bool>,
    pub serialize: Option<bool>,
    pub well_formed: Option<bool>,
    pub result_var: Option<String>,
}

/// Prefix → URI bindings in scope at an assertion element. The default namespace is never
/// included: the catalog defines unprefixed names in assertion XPaths as "no namespace".
pub type NsBindings = Vec<(String, String)>;

#[derive(Debug, Clone)]
pub enum Assertion {
    AllOf(Vec<Assertion>),
    AnyOf(Vec<Assertion>),
    Not(Box<Assertion>),
    /// XPath whose effective boolean value must be true; context item = result document node.
    Assert { xpath: String, ns: NsBindings },
    AssertXml { expected: String, file: Option<String>, ignore_prefixes: bool, xml_version: Option<String> },
    AssertStringValue { expected: String, normalize_space: bool },
    Error { code: String },
    SerializationMatches { regex: String, flags: String, file: Option<String> },
    AssertSerialization { expected: String, file: Option<String>, method: Option<String>, ignore_prefixes: bool },
    AssertSerializationError { code: String },
    AssertResultDocument { uri: String, inner: Box<Assertion> },
    AssertMessage(Box<Assertion>),
    AssertWarning,
    AssertType { xpath: String, ns: NsBindings },
    AssertEq { xpath: String, ns: NsBindings },
    AssertDeepEq { xpath: String, ns: NsBindings },
    AssertPermutation { xpath: String, ns: NsBindings },
    AssertCount(String),
    AssertEmpty,
    AssertTrue,
    AssertFalse,
    AssertPostureAndSweep,
    Unknown(String),
}

// ───────────────────────── parsing ─────────────────────────

pub fn load_catalog(root: &Path) -> Result<Catalog, String> {
    let path = root.join("catalog.xml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc = Document::parse(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    let mut test_sets = Vec::new();
    for n in elems(doc.root_element(), "test-set") {
        let name = n.attribute("name").unwrap_or("").to_string();
        let rel_file = n.attribute("file").unwrap_or("").to_string();
        test_sets.push(TestSetRef { name, file: root.join(&rel_file), rel_file });
    }
    if test_sets.is_empty() {
        return Err(format!("{} contains no <test-set> entries", path.display()));
    }
    Ok(Catalog { root: root.to_path_buf(), test_sets })
}

pub fn load_test_set(r: &TestSetRef) -> Result<TestSet, String> {
    let text = std::fs::read_to_string(&r.file).map_err(|e| format!("cannot read {}: {e}", r.file.display()))?;
    let doc = Document::parse(&text).map_err(|e| format!("cannot parse {}: {e}", r.file.display()))?;
    let root = doc.root_element();
    if root.tag_name().namespace() != Some(CATALOG_NS) {
        return Err(format!("{} is not in the catalog namespace", r.file.display()));
    }
    let dir = r.file.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = root.attribute("name").unwrap_or(&r.name).to_string();
    let description = elems(root, "description").next().map(text_of).unwrap_or_default();
    let dependencies = parse_dependencies(root);
    let mut environments = HashMap::new();
    for e in elems(root, "environment") {
        let env = parse_environment(e);
        if let Some(n) = env.name.clone() {
            environments.insert(n, env);
        }
    }
    let mut cases = Vec::new();
    for tc in elems(root, "test-case") {
        cases.push(parse_test_case(tc, &doc));
    }
    Ok(TestSet { name, file: r.file.clone(), dir, rel_file: r.rel_file.clone(), description, dependencies, environments, cases })
}

fn elems<'a, 'i: 'a>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children().filter(move |c| c.is_element() && c.tag_name().name() == name)
}

/// Concatenated text of all direct text/CDATA children.
fn text_of(n: Node) -> String {
    n.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect()
}

fn attr(n: Node, name: &str) -> Option<String> {
    n.attribute(name).map(str::to_string)
}

fn attr_bool(n: Node, name: &str) -> Option<bool> {
    match n.attribute(name) {
        Some("true") | Some("yes") | Some("1") => Some(true),
        Some("false") | Some("no") | Some("0") => Some(false),
        _ => None,
    }
}

fn qname_of(n: Node, value: &str) -> QName {
    match value.split_once(':') {
        Some((p, l)) => QName {
            prefix: Some(p.to_string()),
            local: l.to_string(),
            namespace: n.lookup_namespace_uri(Some(p)).map(str::to_string),
        },
        None => QName { prefix: None, local: value.to_string(), namespace: None },
    }
}

fn ns_bindings(n: Node) -> NsBindings {
    let mut out: NsBindings = n
        .namespaces()
        .filter_map(|ns| ns.name().map(|p| (p.to_string(), ns.uri().to_string())))
        .filter(|(p, _)| p != "xml")
        .collect();
    out.sort();
    out.dedup();
    out
}

fn parse_dependencies(n: Node) -> Vec<Dependency> {
    let mut out = Vec::new();
    for d in elems(n, "dependencies") {
        for c in d.children().filter(|c| c.is_element()) {
            out.push(Dependency {
                kind: c.tag_name().name().to_string(),
                value: c.attribute("value").unwrap_or("").to_string(),
                satisfied: attr_bool(c, "satisfied").unwrap_or(true),
            });
        }
    }
    out
}

fn parse_source(n: Node) -> Source {
    Source {
        role: attr(n, "role"),
        file: attr(n, "file"),
        uri: attr(n, "uri"),
        content: elems(n, "content").next().map(text_of),
        validation: attr(n, "validation"),
        xml_version: attr(n, "xml-version"),
        streaming: attr_bool(n, "streaming").unwrap_or(false),
        select: attr(n, "select"),
        defines_stylesheet: attr_bool(n, "defines-stylesheet").unwrap_or(false),
    }
}

fn parse_param(n: Node) -> Param {
    Param {
        name: qname_of(n, n.attribute("name").unwrap_or("")),
        select: attr(n, "select"),
        as_type: attr(n, "as"),
        is_static: attr_bool(n, "static").unwrap_or(false),
    }
}

fn parse_environment(n: Node) -> Environment {
    Environment {
        name: attr(n, "name"),
        sources: elems(n, "source").map(parse_source).collect(),
        stylesheets: elems(n, "stylesheet")
            .map(|s| StylesheetRef { file: attr(s, "file"), uri: attr(s, "uri"), role: attr(s, "role"), xml_version: attr(s, "xml-version") })
            .collect(),
        schemas: elems(n, "schema")
            .map(|s| SchemaRef { file: attr(s, "file"), uri: attr(s, "uri"), role: attr(s, "role"), xsd_version: attr(s, "xsd-version") })
            .collect(),
        resources: elems(n, "resource")
            .map(|s| Resource { file: attr(s, "file"), uri: attr(s, "uri"), media_type: attr(s, "media-type"), encoding: attr(s, "encoding") })
            .collect(),
        collations: elems(n, "collation")
            .map(|c| Collation { uri: c.attribute("uri").unwrap_or("").to_string(), default: attr_bool(c, "default").unwrap_or(false) })
            .collect(),
        collections: elems(n, "collection")
            .map(|c| Collection { uri: attr(c, "uri"), sources: elems(c, "source").map(parse_source).collect() })
            .collect(),
        params: elems(n, "param").map(parse_param).collect(),
        has_package: elems(n, "package").next().is_some(),
    }
}

fn parse_test(n: Node) -> TestSpec {
    let mut spec = TestSpec {
        stylesheets: elems(n, "stylesheet")
            .map(|s| StylesheetRef { file: attr(s, "file"), uri: attr(s, "uri"), role: attr(s, "role"), xml_version: attr(s, "xml-version") })
            .collect(),
        params: elems(n, "param").map(parse_param).collect(),
        has_package: elems(n, "package").next().is_some(),
        has_posture_and_sweep: elems(n, "posture-and-sweep").next().is_some(),
        ..Default::default()
    };
    if let Some(t) = elems(n, "initial-template").next() {
        spec.initial_template = t.attribute("name").map(|v| qname_of(t, v));
    }
    if let Some(m) = elems(n, "initial-mode").next() {
        spec.initial_mode = m.attribute("name").map(|v| qname_of(m, v));
        spec.initial_mode_select = attr(m, "select");
    }
    if let Some(f) = elems(n, "initial-function").next() {
        spec.initial_function = f.attribute("name").map(|v| qname_of(f, v));
    }
    if let Some(o) = elems(n, "output").next() {
        spec.output = Some(OutputSpec {
            file: attr(o, "file"),
            tree: attr_bool(o, "tree"),
            serialize: attr_bool(o, "serialize"),
            well_formed: attr_bool(o, "well-formed"),
            result_var: attr(o, "result-var"),
        });
    }
    spec
}

fn parse_assertions(n: Node) -> Vec<Assertion> {
    n.children().filter(|c| c.is_element()).map(parse_assertion).collect()
}

fn one_or_all(mut list: Vec<Assertion>) -> Assertion {
    match list.len() {
        0 => Assertion::AllOf(Vec::new()),
        1 => list.remove(0),
        _ => Assertion::AllOf(list),
    }
}

fn parse_assertion(n: Node) -> Assertion {
    use Assertion::*;
    match n.tag_name().name() {
        "all-of" => AllOf(parse_assertions(n)),
        "any-of" => AnyOf(parse_assertions(n)),
        "not" => Not(Box::new(one_or_all(parse_assertions(n)))),
        "assert" => Assert { xpath: text_of(n), ns: ns_bindings(n) },
        "assert-xml" => AssertXml {
            expected: text_of(n),
            file: attr(n, "file"),
            ignore_prefixes: attr_bool(n, "ignore-prefixes").unwrap_or(false),
            xml_version: attr(n, "xml-version"),
        },
        "assert-string-value" => AssertStringValue {
            expected: text_of(n),
            normalize_space: attr_bool(n, "normalize-space").unwrap_or(false),
        },
        // `@code` is optional in the schema; keep an absent one distinguishable from the
        // documented `code="*"` ("any error code is allowed") instead of silently conflating
        // the two, which would make a codeless <error> match every error.
        "error" => Error { code: n.attribute("code").unwrap_or("").to_string() },
        "serialization-matches" => SerializationMatches {
            regex: text_of(n),
            flags: n.attribute("flags").unwrap_or("").to_string(),
            file: attr(n, "file"),
        },
        "assert-serialization" => AssertSerialization {
            expected: text_of(n),
            file: attr(n, "file"),
            method: attr(n, "method"),
            ignore_prefixes: attr_bool(n, "ignore-prefixes").unwrap_or(false),
        },
        "assert-serialization-error" => AssertSerializationError { code: n.attribute("code").unwrap_or("").to_string() },
        "assert-result-document" => AssertResultDocument {
            uri: n.attribute("uri").unwrap_or("").to_string(),
            inner: Box::new(one_or_all(parse_assertions(n))),
        },
        "assert-message" => AssertMessage(Box::new(one_or_all(parse_assertions(n)))),
        "assert-warning" => AssertWarning,
        "assert-type" => AssertType { xpath: text_of(n), ns: ns_bindings(n) },
        "assert-eq" => AssertEq { xpath: text_of(n), ns: ns_bindings(n) },
        "assert-deep-eq" => AssertDeepEq { xpath: text_of(n), ns: ns_bindings(n) },
        "assert-permutation" => AssertPermutation { xpath: text_of(n), ns: ns_bindings(n) },
        // Kept verbatim: a malformed count used to become 0 silently, and the harness cannot
        // check the count anyway (see `compare::evaluate`), so the text is only ever reported.
        "assert-count" => AssertCount(text_of(n).trim().to_string()),
        "assert-empty" => AssertEmpty,
        "assert-true" => AssertTrue,
        "assert-false" => AssertFalse,
        "assert-posture-and-sweep" => AssertPostureAndSweep,
        other => Unknown(other.to_string()),
    }
}

fn parse_test_case(n: Node, doc: &Document) -> TestCase {
    let environment = elems(n, "environment").next().map(|e| match e.attribute("ref") {
        Some(r) => EnvRef::Ref(r.to_string()),
        None => EnvRef::Inline(parse_environment(e)),
    });
    TestCase {
        name: n.attribute("name").unwrap_or("").to_string(),
        description: elems(n, "description").next().map(text_of).unwrap_or_default(),
        dependencies: parse_dependencies(n),
        environment,
        test: elems(n, "test").next().map(parse_test).unwrap_or_default(),
        result: elems(n, "result").next().map(|r| one_or_all(parse_assertions(r))).unwrap_or(Assertion::AllOf(Vec::new())),
        line: doc.text_pos_at(n.range().start).row,
    }
}

impl Assertion {
    /// Short label for reports.
    pub fn label(&self) -> &'static str {
        use Assertion::*;
        match self {
            AllOf(_) => "all-of",
            AnyOf(_) => "any-of",
            Not(_) => "not",
            Assert { .. } => "assert",
            AssertXml { .. } => "assert-xml",
            AssertStringValue { .. } => "assert-string-value",
            Error { .. } => "error",
            SerializationMatches { .. } => "serialization-matches",
            AssertSerialization { .. } => "assert-serialization",
            AssertSerializationError { .. } => "assert-serialization-error",
            AssertResultDocument { .. } => "assert-result-document",
            AssertMessage(_) => "assert-message",
            AssertWarning => "assert-warning",
            AssertType { .. } => "assert-type",
            AssertEq { .. } => "assert-eq",
            AssertDeepEq { .. } => "assert-deep-eq",
            AssertPermutation { .. } => "assert-permutation",
            AssertCount(_) => "assert-count",
            AssertEmpty => "assert-empty",
            AssertTrue => "assert-true",
            AssertFalse => "assert-false",
            AssertPostureAndSweep => "assert-posture-and-sweep",
            Unknown(_) => "unknown",
        }
    }
}
