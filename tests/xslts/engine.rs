//! The conformance harness's adapter to the real processor.
//!
//! Everything here goes through `xslt2`'s public API — `Compiler`,
//! `CompiledStylesheet`, `Transformation` — which is the point: if a test case
//! needs something the adapter cannot express, the public API is missing it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::serialize::SerializeOptions;
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use xslt2::{
    Compiler, MemoryResolver, ModeKey, QName, Resolver, TransformOptions, XsltError, XsltFunctions,
};

use crate::catalog::QName as CatalogQName;
use crate::runner::{Engine, ResultDocument, RunOutput, TestRun};

/// The XSLT 2.0 processor under test.
pub struct XsltEngine;

impl Engine for XsltEngine {
    fn name(&self) -> &'static str {
        "xslt2"
    }

    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn run(&self, run: &TestRun<'_>) -> RunOutput {
        match Job::from(run) {
            Ok(job) => job.execute(),
            // The harness refusing a case is not something the engine did: reported as an
            // engine error (`XSLT2-INTERNAL`) it would satisfy `<error code="*"/>`.
            Err(message) => harness_refusal(message),
        }
    }
}

fn harness_refusal(message: String) -> RunOutput {
    RunOutput {
        harness_error: Some(message),
        ..Default::default()
    }
}

/// One test case reduced to owned data.
struct Job {
    stylesheet: PathBuf,
    /// The initial context document: `(uri, text)`, read from a file or from
    /// the catalog's inline `<content>`.
    source: Option<(String, String)>,
    /// Every other source the case declares, so `document()` can find them.
    extra: Vec<(String, String)>,
    params: Vec<(QName, String, Option<String>)>,
    initial_template: Option<QName>,
    initial_mode: Option<ModeKey>,
    strict_recoverable: bool,
    /// `<output serialize="yes"/>`: the case asks for the result to be serialized, so a
    /// failure of the stylesheet's serialization is the run's error.
    serialize: bool,
}

impl Job {
    fn from(run: &TestRun<'_>) -> Result<Self, String> {
        let stylesheet = run
            .stylesheet
            .clone()
            .ok_or_else(|| "the test case names no stylesheet".to_string())?;

        let mut source = None;
        let mut extra = Vec::new();
        for (index, entry) in run.env.sources.iter().enumerate() {
            // `select` makes a node *inside* the source document the initial context item (the
            // catalog: "an XPath expression to select the initial context item within the
            // source document"). The public API starts a run only at a document node
            // (`transform_source` / `transform_document`; `Transformation::run` is private), so
            // ignoring the attribute would run a different case from the one specified — and
            // `call-template-0201` used to pass that way only because `copy-of .` of the
            // document node and of its single element serialize alike.
            if let Some(select) = entry.select.as_deref().filter(|s| !s.trim().is_empty()) {
                return Err(format!(
                    "source/@select={select:?} is not supported: the initial context item would \
                     be a node inside the source document, and the public API has no entry point \
                     for a non-document initial context node"
                ));
            }
            // A source given as inline `<content>` has neither a file nor a
            // URI; it still needs one, because the document store keys
            // documents by URI.
            //
            // The principal source (`role="."`) is text inside the test-set
            // file, so its base URI is that file's URI (RFC 3986 §5.1.2: "the
            // default base URI of a representation is the base URI of the
            // entity in which the representation is encapsulated");
            // `backwards-041` checks exactly that. That makes the inline text
            // the *base* of the document, not the resource at that URI: the
            // resolver must keep serving the real test-set file there (see
            // `Job::resolver`). Any other inline
            // source gets a synthetic URI, because two documents cannot share
            // one key in the store. It too has to sit **inside** the test-set
            // directory: it is the base URI of every node of that document, so
            // §16.1 resolves a relative reference read out of the source
            // against it (`document-0601` and friends). `path_to_uri` of a
            // directory has no trailing slash, and without one the reference
            // would resolve against the *parent* directory.
            let uri = source_uri(entry.uri.as_deref(), entry.path.as_deref(), &run.set.dir)?
                .or_else(|| {
                    if entry.role.as_deref() == Some(".") {
                        Some(xslt2::resolver::path_to_uri(&run.set.file))
                    } else {
                        Some(format!(
                            "{}/#inline-source-{index}",
                            xslt2::resolver::path_to_uri(&run.base_dir)
                        ))
                    }
                });
            let text = match (&entry.content, &entry.path) {
                (Some(content), _) => content.clone(),
                (None, Some(path)) => xslt2::resolver::decode_xml(
                    &std::fs::read(path)
                        .map_err(|e| format!("cannot read {}: {e}", path.display()))?,
                ),
                (None, None) => continue,
            };
            let Some(uri) = uri else { continue };
            if entry.role.as_deref() == Some(".") {
                source = Some((uri, text));
            } else {
                extra.push((uri, text));
            }
        }

        // The same for the initial match selection of `<initial-mode select="…">` (no
        // 2.0-applicable case uses it today).
        if let Some(select) = run.case.test.initial_mode_select.as_deref() {
            return Err(format!(
                "initial-mode/@select={select:?} is not supported: the public API starts a run \
                 only at a document node"
            ));
        }

        let mut params = Vec::new();
        for param in run.env.params.iter().chain(run.case.test.params.iter()) {
            if param.is_static {
                continue;
            }
            let Some(select) = param.select.clone() else {
                continue;
            };
            params.push((qname_of(&param.name), select, param.as_type.clone()));
        }

        Ok(Self {
            stylesheet,
            source,
            extra,
            params,
            initial_template: run.case.test.initial_template.as_ref().map(qname_of),
            initial_mode: run.case.test.initial_mode.as_ref().and_then(mode_of),
            strict_recoverable: run.strict_recoverable,
            serialize: run
                .case
                .test
                .output
                .as_ref()
                .and_then(|o| o.serialize)
                .unwrap_or(false),
        })
    }

    /// The resolver the compiler and the transformation share: the catalog's extra sources by
    /// their URIs, then the file system.
    ///
    /// The principal source is deliberately **not** in it. It reaches the run through
    /// `transform_source(uri, text)`, which is all that its base URI needs; registering its text
    /// here as well would serve it under its URI to everything that reads through the resolver.
    /// For an inline principal that URI is the test-set file's own, so `xsl:include` /
    /// `xsl:import` / `unparsed-text()` of the test-set file would get the inline `<content>`
    /// instead of the file (`error-0150b`, `-0150c` and `-0150e` include their own test-set
    /// file and compiled against an inline `<doc>`). For a principal read from a file the entry
    /// would only repeat what the file system serves.
    fn resolver(&self) -> Arc<dyn Resolver> {
        let mut memory = MemoryResolver::with_fallback(Arc::new(xslt2::FileResolver));
        for (uri, text) in &self.extra {
            memory.insert(uri.clone(), text.clone());
        }
        Arc::new(memory)
    }

    fn execute(self) -> RunOutput {
        let names = NameTable::new();
        let functions = XsltFunctions::new();
        let resolver = self.resolver();

        let compiled = match Compiler::new(&names, &functions)
            .with_resolver(Arc::clone(&resolver))
            .compile_file(&self.stylesheet)
        {
            Ok(compiled) => compiled,
            Err(e) => return failure(e),
        };

        let arena = Bump::new();
        let options = TransformOptions {
            initial_template: self.initial_template.clone(),
            initial_mode: self.initial_mode.clone().unwrap_or(ModeKey::Default),
            strict_recoverable: self.strict_recoverable,
            ..Default::default()
        };
        let mut transformation = compiled
            .transformation(&arena, options)
            .with_resolver(Arc::clone(&resolver));

        for (name, select, as_type) in &self.params {
            match eval_param(&names, &functions, select, as_type.as_deref()) {
                Ok(value) => transformation = transformation.with_param(name.clone(), value),
                // The harness evaluates catalog parameters itself; a failure there is the
                // harness's, not the engine's.
                Err(message) => return harness_refusal(message),
            }
        }

        // Every source the case declares is registered so that `document()`
        // and `doc()` resolve them without touching the file system.
        for (uri, text) in &self.extra {
            transformation.register_source(uri, text);
        }

        let outcome = match &self.source {
            Some((uri, text)) => transformation.transform_source(uri, text),
            None => transformation.transform_without_source(),
        };
        let messages: Vec<String> = transformation
            .take_messages()
            .into_iter()
            .map(|m| m.content)
            .collect();

        match outcome {
            Ok(outcome) => {
                let messages = if messages.is_empty() {
                    outcome.messages.iter().map(|m| m.content.clone()).collect()
                } else {
                    messages
                };
                match primary_result(&compiled, &outcome, self.serialize) {
                    Ok(primary) => RunOutput {
                        primary: Some(primary),
                        messages,
                        ..Default::default()
                    },
                    Err(e) => RunOutput {
                        messages,
                        ..failure(e)
                    },
                }
            }
            Err(e) => RunOutput {
                messages,
                ..failure(e)
            },
        }
    }
}

/// The primary result in both forms `ResultDocument` keeps: serialized with the stylesheet's
/// own `xsl:output` (`CompiledStylesheet::serialize_primary`), and written with the catalog's
/// default parameters (`default_serialization`).
///
/// A failure of the stylesheet's serialization is the run's error only when the case asks for
/// serialization — `<output serialize="yes"/>`; the catalog (`admin/catalog-schema.xsd`,
/// `output/@serialize`): "Determines whether or not serialization takes place. The default is
/// no." Otherwise the transformation succeeded and produced a tree, which is what the case's
/// tree assertions are about: the failure is kept in `serialization_error`, where only a
/// serialization assertion sees it (M3a, harness-review backlog item F8).
fn primary_result(
    compiled: &xslt2::CompiledStylesheet<'_>,
    outcome: &xslt2::Outcome<'_>,
    serialize: bool,
) -> Result<ResultDocument, XsltError> {
    let (serialized, serialization_error) = match compiled.serialize_primary(outcome) {
        Ok(text) => (text, None),
        Err(e) if serialize => return Err(e),
        Err(e) => (String::new(), Some(e)),
    };
    let (tree, method) = match outcome.primary {
        Some(doc) => {
            // The method `serialize_primary` used — with §20's rule that an XHTML result of a 1.0
            // stylesheet defaults to `xml`.
            let method = compiled
                .primary_serialization_params(outcome)
                .ok()
                .flatten()
                .map(|p| method_name(p.method).to_string());
            (default_serialization(doc), method)
        }
        // No primary result: `serialize_primary` wrote nothing either.
        None => (Ok(String::new()), None),
    };
    Ok(ResultDocument {
        serialized,
        tree,
        serialization_error,
        method,
    })
}

fn method_name(method: xslt2::serialize::ResolvedMethod) -> &'static str {
    use xslt2::serialize::ResolvedMethod::*;
    match method {
        Xml => "xml",
        Xhtml => "xhtml",
        Html => "html",
        Text => "text",
    }
}

/// A result tree written with the serialization parameters the catalog defines `assert-xml`
/// over — "the default serialization parameters method="xml" indent="no"
/// omit-xml-declaration="yes"" (`admin/catalog-schema.xsd`) — and nothing else of the
/// stylesheet's `xsl:output`: no indentation, no text method, no encoding escapes.
/// `xsd-schema`'s serializer with these options adds no whitespace and removes none, so a
/// re-parse of the text has the tree's nodes — all but whitespace-only text directly under the
/// document node, which no XML parse keeps (as in `assert-xml`'s parse-and-canonicalize
/// definition).
fn default_serialization(doc: &xsd_schema::document::BufferDocument<'_>) -> Result<String, String> {
    let defaults = SerializeOptions {
        xml_declaration: false,
        standalone: None,
        indent: None,
    };
    xsd_schema::document::serialize::to_string(&doc.create_navigator(), &defaults)
        .map_err(|e| e.to_string())
}

fn failure(e: XsltError) -> RunOutput {
    RunOutput {
        not_implemented: e.is_not_implemented(),
        error: Some(e),
        ..Default::default()
    }
}

/// Evaluates a `<param select="…">` from the catalog.
///
/// §2.4 of the design sketch: the expression has no context item; an `as`
/// attribute is honoured by casting through `treat as`, which is enough for
/// the literal values the suite uses. An error is the harness's (the catalog:
/// "It should not raise an error as param evaluation is API dependent
/// behavior"), so it comes back as a message for `harnessError`.
fn eval_param(
    names: &NameTable,
    functions: &XsltFunctions,
    select: &str,
    as_type: Option<&str>,
) -> Result<XPathValue<xslt2::Nav<'static>>, String> {
    // The catalog writes parameter types as `xs:string`, so the conventional
    // prefixes have to be in scope; a stylesheet would declare them itself.
    let mut namespaces = NamespaceContextSnapshot::default();
    for (prefix, uri) in [
        ("xs", "http://www.w3.org/2001/XMLSchema"),
        ("xsi", "http://www.w3.org/2001/XMLSchema-instance"),
        ("fn", "http://www.w3.org/2005/xpath-functions"),
    ] {
        namespaces
            .bindings
            .push((names.add(prefix), names.add(uri)));
    }
    let ctx = XPathContext::new(names)
        .with_namespaces(namespaces)
        .with_function_catalog(functions.catalog());
    let source = match as_type {
        Some(t) if !t.trim().is_empty() => format!("({select}) treat as {t}"),
        _ => select.to_string(),
    };
    let expr = XPathExpr::compile(&source, &ctx)
        .map_err(|e| format!("cannot compile the catalog parameter {select:?}: {e}"))?;
    expr.evaluator(&ctx)
        .run::<xslt2::Nav<'static>>()
        .map_err(|e| format!("cannot evaluate the catalog parameter {select:?}: {e}"))
}

/// The URI a source is registered under: its `uri` attribute, else its file's URI.
///
/// The catalog: "The `uri` attribute is the URI supplied to the `doc()` function to access the
/// document. This may be an absolute URI or a URI relative to the base URI of the catalog file"
/// (`admin/catalog-schema.xsd`, `source`). A relative one used to be registered as written, so
/// the source's document URI and base URI were the bare relative string, while `doc()` with the
/// same text, resolved against the stylesheet, reached a different document (`accessor-008`).
fn source_uri(
    uri: Option<&str>,
    path: Option<&Path>,
    set_dir: &Path,
) -> Result<Option<String>, String> {
    if let Some(uri) = uri {
        if !uri.trim().is_empty() {
            return resolve_catalog_uri(uri, set_dir).map(Some);
        }
    }
    Ok(path.map(xslt2::resolver::path_to_uri))
}

/// Resolve a catalog `uri` against the test-set file.
///
/// An absolute URI is used as it is. A relative one is resolved only when it is a plain
/// relative path — segments without `.`, `..`, a query, a fragment or percent-escapes — because
/// then RFC 3986 §5.2 resolution against the test-set file is exactly "replace the file name by
/// the reference", which `path_to_uri(dir.join(reference))` computes. Anything else is reported
/// rather than guessed at (every relative `uri` in the 2.0-applicable cases is a plain path).
fn resolve_catalog_uri(uri: &str, set_dir: &Path) -> Result<String, String> {
    let has_scheme = uri
        .split_once(':')
        .map(|(scheme, _)| {
            let mut chars = scheme.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
        .unwrap_or(false);
    if has_scheme {
        return Ok(uri.to_string());
    }
    let plain = !uri.starts_with('/')
        && !uri.contains(['?', '#', '%', '\\'])
        && uri
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if !plain {
        return Err(format!(
            "the catalog source uri {uri:?} is not a plain relative path; the harness does not resolve it"
        ));
    }
    Ok(xslt2::resolver::path_to_uri(&set_dir.join(uri)))
}

fn qname_of(name: &CatalogQName) -> QName {
    match &name.namespace {
        Some(ns) => QName::new(ns.clone(), name.local.clone()),
        None => QName::local(name.local.clone()),
    }
}

/// `initial-mode`, with the two spellings the catalog uses for "the unnamed
/// mode" mapped to it.
fn mode_of(name: &CatalogQName) -> Option<ModeKey> {
    // The catalog writes `#default` and `#unnamed` (the catalog parser keeps
    // the `#`); both name the unnamed mode.
    match name.local.trim_start_matches('#') {
        "default" | "unnamed" if name.namespace.is_none() => Some(ModeKey::Default),
        _ => Some(ModeKey::Named(qname_of(name))),
    }
}

/// The adapter's own checks, run by `--self-test`: how catalog sources are named and served,
/// and that what the harness refuses is a `harnessError`. Returns the number of checks.
pub fn self_test() -> usize {
    use std::collections::HashMap;

    use crate::catalog::{Assertion, TestCase, TestSet, TestSpec};
    use crate::environment::{ResolvedEnvironment, ResolvedSource};

    let mut checks = 0;
    let mut check = |ok: bool, what: &str| {
        assert!(ok, "engine self-test failed: {what}");
        checks += 1;
    };
    let dir = Path::new("/suite/tests/fn/accessor");
    let file_uri = |p: &str| xslt2::resolver::path_to_uri(Path::new(p));

    // B11: a relative catalog `uri` is resolved against the test-set file …
    check(
        resolve_catalog_uri("accessor-004.xml", dir)
            == Ok(file_uri("/suite/tests/fn/accessor/accessor-004.xml")),
        "relative uri",
    );
    check(
        resolve_catalog_uri("docs/a.xml", dir)
            == Ok(file_uri("/suite/tests/fn/accessor/docs/a.xml")),
        "relative uri with a directory",
    );
    // … an absolute one is kept …
    for absolute in [
        "http://example.com/a.xml",
        "file:///x/y.xml",
        "urn:x-test:a",
    ] {
        check(
            resolve_catalog_uri(absolute, dir) == Ok(absolute.to_string()),
            absolute,
        );
    }
    // … and anything that is not a plain relative path is refused, not guessed at.
    for refused in [
        "../x.xml",
        "./x.xml",
        "a//b.xml",
        "/abs.xml",
        "x.xml#f",
        "x.xml?q",
        "a%20b.xml",
    ] {
        check(resolve_catalog_uri(refused, dir).is_err(), refused);
    }

    // E-C11: the inline principal source is named after the test-set file; other inline sources
    // keep distinct synthetic names inside the test-set directory; a `uri` wins over both.
    let set = TestSet {
        name: "accessor".into(),
        file: dir.join("_accessor-test-set.xml"),
        dir: dir.to_path_buf(),
        rel_file: "tests/fn/accessor/_accessor-test-set.xml".into(),
        description: String::new(),
        dependencies: Vec::new(),
        environments: HashMap::new(),
        cases: Vec::new(),
    };
    let case = TestCase {
        name: "probe".into(),
        description: String::new(),
        dependencies: Vec::new(),
        environment: None,
        test: TestSpec::default(),
        result: Assertion::AllOf(Vec::new()),
        line: 0,
    };
    let inline = |role: Option<&str>, uri: Option<&str>, content: &str| ResolvedSource {
        role: role.map(str::to_string),
        uri: uri.map(str::to_string),
        path: None,
        content: Some(content.to_string()),
        validation: None,
        xml_version: None,
        streaming: false,
        select: None,
    };
    let probe_run = |sources: Vec<ResolvedSource>| TestRun {
        set: &set,
        case: &case,
        env: ResolvedEnvironment {
            sources,
            ..Default::default()
        },
        stylesheet: Some(dir.join("probe.xsl")),
        base_dir: dir.to_path_buf(),
        schema_aware: false,
        strict_recoverable: false,
    };
    let run = probe_run(vec![
        inline(Some("."), None, "<a/>"),
        inline(None, None, "<b/>"),
        inline(None, Some("doc.xml"), "<c/>"),
    ]);
    let job = Job::from(&run).expect("the probe job builds");
    let set_uri = file_uri("/suite/tests/fn/accessor/_accessor-test-set.xml");
    check(
        job.source == Some((set_uri.clone(), "<a/>".to_string())),
        "inline principal source",
    );
    check(
        job.extra
            == vec![
                (
                    format!("{}/#inline-source-1", file_uri("/suite/tests/fn/accessor")),
                    "<b/>".to_string(),
                ),
                (
                    file_uri("/suite/tests/fn/accessor/doc.xml"),
                    "<c/>".to_string(),
                ),
            ],
        "other inline sources",
    );

    // P1-1: the inline principal is the *base* of its document, not the resource at the test-set
    // file's URI. The resolver the compiler and the transformation share must not serve it
    // there (the probe's test-set file does not exist, so the honest answer is "unavailable"),
    // while the other catalog sources stay reachable by their URIs.
    let resolver = job.resolver();
    check(
        resolver.resolve_text(&set_uri).is_err() && !resolver.available(&set_uri),
        "the resolver does not serve the inline principal under the test-set URI",
    );
    check(
        resolver.resolve_text(&file_uri("/suite/tests/fn/accessor/doc.xml"))
            == Ok("<c/>".to_string()),
        "the resolver serves the other catalog sources",
    );
    // … end to end through the compiler: a module that includes its own test-set file reads
    // that file (here: fails to load it, XTSE0165), never the inline `<a/>` (XTSE0150, "its
    // document element a", which is what error-0150b/c/e used to report).
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let including = r#"<xsl:stylesheet version="2.0"
        xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
        <xsl:include href="_accessor-test-set.xml"/>
    </xsl:stylesheet>"#;
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(job.resolver())
        .compile_str(including, &file_uri("/suite/tests/fn/accessor/probe.xsl"));
    check(
        matches!(&compiled, Err(e) if e.code == "XTSE0165" && !e.message.contains("element a")),
        "xsl:include of the test-set file does not compile the inline principal",
    );

    // The primary result in its two forms (`primary_result`), end to end through the public
    // API: the stylesheet's `xsl:output` shapes `serialized` only; `tree` is the tree as it is.
    let forms = |sheet: &str| {
        let compiled = Compiler::new(&names, &functions)
            .compile_str(sheet, "urn:harness-probe")
            .expect("the probe stylesheet compiles");
        let arena = Bump::new();
        let run = compiled.transformation(&arena, TransformOptions::default());
        let outcome = run
            .transform_source("urn:harness-probe-source", "<in/>")
            .expect("the probe stylesheet runs");
        primary_result(&compiled, &outcome, true).expect("the probe result serializes")
    };
    let sheet = |output: &str| {
        format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 {output}
                 <xsl:template match="/">
                   <out><test>true</test><ws><xsl:text>  </xsl:text></ws></out>
                 </xsl:template>
               </xsl:stylesheet>"#
        )
    };
    let tree = "<out><test>true</test><ws>  </ws></out>";
    let indented = forms(&sheet(r#"<xsl:output indent="yes"/>"#));
    check(
        indented.tree.as_deref() == Ok(tree),
        "indent=\"yes\" does not reach the default-parameter form",
    );
    check(
        indented.serialized.contains('\n') && indented.serialized != tree,
        "indent=\"yes\" still shapes the stylesheet's own serialization",
    );
    let text = forms(&sheet(r#"<xsl:output method="text"/>"#));
    check(
        text.serialized == "true  " && text.tree.as_deref() == Ok(tree),
        "the text method shapes the stylesheet's serialization only",
    );
    let declared = forms(&sheet(r#"<xsl:output omit-xml-declaration="no"/>"#));
    check(
        declared.serialized.starts_with("<?xml") && declared.tree.as_deref() == Ok(tree),
        "the default form has no XML declaration",
    );

    // M3a (F8): a stylesheet serialization that fails ends the run only when the case asks
    // for serialization; otherwise the tree is kept for the tree assertions and the failure for
    // the serialization assertions.
    let failing = |serialize: bool| {
        let compiled = Compiler::new(&names, &functions)
            .compile_str(
                &sheet(r#"<xsl:output encoding="XXX-xx"/>"#),
                "urn:harness-probe",
            )
            .expect("the probe stylesheet compiles");
        let arena = Bump::new();
        let run = compiled.transformation(&arena, TransformOptions::default());
        let outcome = run
            .transform_source("urn:harness-probe-source", "<in/>")
            .expect("the probe stylesheet runs");
        primary_result(&compiled, &outcome, serialize).map(|r| {
            (
                r.tree.clone(),
                r.serialization_error.map(|e| e.code),
                r.serialized,
            )
        })
    };
    check(
        matches!(failing(true), Err(e) if e.code == "SESU0007"),
        "serialize=\"yes\": the serialization failure is the run's error",
    );
    check(
        failing(false)
            == Ok((
                Ok(tree.to_string()),
                Some("SESU0007".to_string()),
                String::new(),
            )),
        "serialize absent: the tree survives, the failure is kept for serialization assertions",
    );
    check(
        forms(&sheet(r#"<xsl:output method="xhtml"/>"#))
            .method
            .as_deref()
            == Some("xhtml")
            && forms(&sheet("")).method.as_deref() == Some("xml"),
        "the method the stylesheet serialization used is recorded",
    );
    // §20: "if the version attribute of the xsl:stylesheet element of the principal stylesheet
    // module has the value 1.0, and if the result tree is generated implicitly …, then the
    // default output method in this situation is xml" — the harness records what
    // `serialize_primary` did, not the 2.0 default.
    let xhtml_result = |version: &str| {
        forms(&format!(
            r#"<xsl:stylesheet version="{version}" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:template match="/"><html xmlns="http://www.w3.org/1999/xhtml"/></xsl:template>
               </xsl:stylesheet>"#
        ))
        .method
    };
    check(
        xhtml_result("1.0").as_deref() == Some("xml")
            && xhtml_result("2.0").as_deref() == Some("xhtml"),
        "an XHTML result of a 1.0 stylesheet is recorded as the xml method",
    );

    // P2-3: a harness refusal is a harnessError, never an engine error that `<error
    // code="*"/>` would accept.
    let inputs = crate::compare::CaseInputs::default();
    let cx = crate::compare::CompareContext {
        set_dir: dir,
        lenient_xml: false,
        inputs: &inputs,
        entity_results: false,
    };
    let any_error = Assertion::Error { code: "*".into() };
    let refused = XsltEngine.run(&probe_run(vec![inline(
        Some("."),
        Some("../x.xml"),
        "<x/>",
    )]));
    check(
        refused.error.is_none()
            && refused
                .harness_error
                .as_deref()
                .is_some_and(|m| m.contains("not a plain relative path")),
        "a refused catalog uri is a harness refusal, not an engine error",
    );
    check(
        crate::compare::evaluate(&any_error, &refused, &cx).outcome
            == crate::compare::Outcome::HarnessError,
        "<error code=\"*\"/> is not satisfied by a harness refusal",
    );

    // P2-4: `source/@select` cannot be honoured through the public API, so the case is refused
    // rather than run from the document node — whatever its assertions would have said.
    let mut selected = inline(Some("."), None, "<doc/>");
    selected.select = Some("/doc".into());
    let refused = XsltEngine.run(&probe_run(vec![selected]));
    check(
        refused
            .harness_error
            .as_deref()
            .is_some_and(|m| m.contains("source/@select")),
        "source/@select is refused",
    );
    let trivially_true = Assertion::Assert {
        xpath: "true()".into(),
        ns: Vec::new(),
    };
    for a in [&trivially_true, &any_error] {
        check(
            crate::compare::evaluate(a, &refused, &cx).outcome
                == crate::compare::Outcome::HarnessError,
            "a case with source/@select is a harnessError",
        );
    }
    checks
}
