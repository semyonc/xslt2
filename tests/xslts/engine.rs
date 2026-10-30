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
    Compiler, MemoryResolver, ModeKey, QName, Resolver, SchemaContext, SourceValidation,
    TimezoneOffset, TransformOptions, XsdVersion, XsltError, XsltFunctions,
};

use crate::catalog::QName as CatalogQName;
use crate::runner::{Engine, ResultDocument, RunOutput, TestRun};

/// The implicit timezone of every case's transformation (`TransformOptions::implicit_timezone`):
/// UTC, the value the catalog assumes ("we currently assume that an absent timezone means UTC",
/// `date-012` … `date-018`, `date-042`); with `--host-timezone`, `None` — the host's local offset.
/// `driver::main` also pins the process's `TZ` to UTC, as a guard for what the option does not
/// reach (see there).
pub fn implicit_timezone() -> Option<TimezoneOffset> {
    static HOST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let host = *HOST.get_or_init(|| std::env::args().any(|arg| arg == "--host-timezone"));
    (!host).then_some(TimezoneOffset::UTC)
}

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
    /// How the initial context document is validated (`source/@validation`; schema-aware runs).
    source_validation: SourceValidation,
    /// Every other source the case declares, so `document()` can find them.
    extra: Vec<(String, String)>,
    /// How each of `extra` is validated, in the same order.
    extra_validation: Vec<SourceValidation>,
    params: Vec<(QName, String, Option<String>)>,
    initial_template: Option<QName>,
    initial_mode: Option<ModeKey>,
    strict_recoverable: bool,
    /// `<output serialize="yes"/>`: the case asks for the result to be serialized, so a
    /// failure of the stylesheet's serialization is the run's error.
    serialize: bool,
    /// The base output URI (XSLT 2.0 §2.3) the run's `xsl:result-document` hrefs resolve
    /// against: the test-set directory. The catalog (`admin/catalog-schema.xsd`,
    /// `assert-result-document/@uri`): "It is supplied as a relative URI, interpreted as being
    /// relative to an implementation defined base output URI as chosen by the test driver".
    base_output_uri: String,
    /// `--schema-aware`: compile with a schema-aware processor (XSLT 2.0 §21.2), against a
    /// `SchemaContext` built from the stylesheet's own `xsl:import-schema` declarations and the
    /// environment's schemas (`schemas`).
    schema_aware: bool,
    /// Schema-aware runs: the environment's `<schema>` documents the context is given as host
    /// schemas (XSLT 2.0 §3.13's implementation-defined mechanism; `context_schema`).
    schemas: Vec<PathBuf>,
    /// Schema-aware runs: the version of XML Schema the context implements (`context_version`).
    xsd_version: XsdVersion,
    /// Schema-aware runs: the case's expected result, whose `assert`s are evaluated on a typed
    /// primary result tree while it exists (`ResultDocument::typed_asserts`).
    assertion: crate::catalog::Assertion,
}

/// Whether an environment `<schema>` of this `role` goes into the schema context of a
/// schema-aware run (M4b).
///
/// The catalog (`admin/catalog-schema.xsd`): "An element which provides information about a schema
/// to be used to validate a source document", with the roles `source-reference`,
/// `stylesheet-import` and `secondary`. A `source-reference` schema, and one with no role, is the
/// one the sources are validated against: it goes in. A `secondary` one is a document another
/// schema of the environment includes or imports: it is loaded transitively, through that
/// schema's directives, not on its own.
///
/// A `stylesheet-import` one is the schema the stylesheet imports (`xsl:import-schema`), and it
/// goes in too (M4b report, decision D-N): where the stylesheet's own import names the same
/// document the context loads it once, and `import-schema-185a` / `b` / `c` find their
/// namespace's components only there (their `schema-location` does not exist). Measured: of the
/// 431 schema-aware cases with such a schema, these three change (to M4f's fence) and nothing
/// else.
fn context_schema(role: Option<&str>) -> bool {
    matches!(
        role,
        None | Some("source-reference") | Some("stylesheet-import")
    )
}

/// The version of XML Schema the schema context of a schema-aware run implements (M4g; M4 plan
/// D-3: one version per context, 1.0 by default): **1.1** with `--xsd11` — the catalog's `XSD_1.1`
/// feature is then satisfied, and the cases that need it expect XML Schema 1.1 behaviour (W3C XSD
/// 1.1 namespace fixup in `import-schema-164`, assertions in `strip-space-009`, type alternatives
/// in `validation-1301`) — and also when one of the environment's schemas that go into the
/// context declares `xsd-version="1.1"` (`admin/catalog-schema.xsd`: `xsd-version`, a decimal,
/// default `1.0`): the schema document is an XML Schema 1.1 document, which a 1.0 set cannot read.
/// Otherwise 1.0. (A schema declared `1.0` is read by a 1.1 context too, as XML Schema 1.1
/// reads 1.0 documents: `import-schema-164`'s `schema092.xsd` says `1.0` and needs 1.1.)
fn context_version(run: &TestRun) -> XsdVersion {
    let declares_1_1 = run.env.schemas.iter().any(|schema| {
        context_schema(schema.role.as_deref())
            && schema
                .xsd_version
                .as_deref()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .is_some_and(|v| v >= 1.1)
    });
    if run.schema_aware && (run.xsd11 || declares_1_1) {
        XsdVersion::V1_1
    } else {
        XsdVersion::V1_0
    }
}

/// The validation of a source document in a run: the catalog's `source/@validation` ("whether
/// the source document is to be validated against the schema defined for that environment, and if
/// so whether validation is to be strict or lax"; values `strict`, `lax`, `skip`) in a
/// schema-aware run, `skip` in a basic one (a basic processor validates nothing, §21.1).
///
/// **No `validation` attribute** (`has_source_schema`: the environment has a `source-reference`
/// schema): strict when the environment has a `source-reference` schema — "a schema to be used to
/// validate a source document" (the catalog's `schema` element) — and skip otherwise. The
/// catalog's own documentation of the default ("The default is "strip"", which is no permitted
/// value) does not settle it; the seven applicable cases with a `source-reference` schema and no
/// `validation` attribute do: two expect typed values from their source (`type-functions-0101`,
/// `-0401`: "arguments coming from typed nodes in input source"), and the other five give the same
/// result either way (M4b report, decision D-M).
fn source_validation(
    validation: Option<&str>,
    has_source_schema: bool,
    schema_aware: bool,
) -> SourceValidation {
    match validation {
        _ if !schema_aware => SourceValidation::Skip,
        Some("strict") => SourceValidation::Strict,
        Some("lax") => SourceValidation::Lax,
        None if has_source_schema => SourceValidation::Strict,
        _ => SourceValidation::Skip,
    }
}

impl Job {
    fn from(run: &TestRun<'_>) -> Result<Self, String> {
        let stylesheet = run
            .stylesheet
            .clone()
            .ok_or_else(|| "the test case names no stylesheet".to_string())?;

        let has_source_schema = run
            .env
            .schemas
            .iter()
            .any(|schema| schema.role.as_deref() == Some("source-reference"));
        let mut source = None;
        let mut source_mode = SourceValidation::Skip;
        let mut extra = Vec::new();
        let mut extra_validation = Vec::new();
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
            let mode = source_validation(
                entry.validation.as_deref(),
                has_source_schema,
                run.schema_aware,
            );
            if entry.role.as_deref() == Some(".") {
                source = Some((uri, text));
                source_mode = mode;
            } else {
                extra.push((uri, text));
                extra_validation.push(mode);
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

        let schemas = if run.schema_aware {
            run.env
                .schemas
                .iter()
                .filter(|schema| context_schema(schema.role.as_deref()))
                .filter_map(|schema| schema.path.clone())
                .collect()
        } else {
            Vec::new()
        };

        Ok(Self {
            stylesheet,
            source,
            source_validation: source_mode,
            extra,
            extra_validation,
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
            // `path_to_uri` of a directory has no trailing slash, and without one a relative
            // href would resolve against the directory's parent.
            base_output_uri: format!("{}/", xslt2::resolver::path_to_uri(&run.base_dir)),
            schema_aware: run.schema_aware,
            xsd_version: context_version(run),
            schemas,
            assertion: if run.schema_aware {
                run.case.result.clone()
            } else {
                crate::catalog::Assertion::AllOf(Vec::new())
            },
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
        let functions = XsltFunctions::new();
        let resolver = self.resolver();

        // Schema-aware (M4 plan D-1): the context first — a pre-scan of the stylesheet's
        // `xsl:import-schema` declarations, loaded into one schema set — then the stylesheet,
        // compiled with the set's name table. A basic run keeps its own table.
        let basic_names = NameTable::new();
        let context = if self.schema_aware {
            let builder = self.schemas.iter().fold(
                SchemaContext::builder().xsd_version(self.xsd_version),
                |builder, path| builder.schema_file(path),
            );
            match builder
                .with_resolver(Arc::clone(&resolver))
                .stylesheet_file(&self.stylesheet, &functions)
                .and_then(|builder| builder.build())
            {
                Ok(context) => Some(context),
                Err(e) => return failure(e),
            }
        } else {
            None
        };
        let compiler = match &context {
            Some(context) => Compiler::schema_aware(context, &functions),
            None => Compiler::new(&basic_names, &functions),
        };
        let compiled = match compiler
            .with_resolver(Arc::clone(&resolver))
            .compile_file(&self.stylesheet)
        {
            Ok(compiled) => compiled,
            Err(e) => return failure(e),
        };
        let names = compiled.names();

        let arena = Bump::new();
        let options = TransformOptions {
            initial_template: self.initial_template.clone(),
            initial_mode: self.initial_mode.clone().unwrap_or(ModeKey::Default),
            strict_recoverable: self.strict_recoverable,
            base_output_uri: Some(self.base_output_uri.clone()),
            implicit_timezone: implicit_timezone(),
            ..Default::default()
        };
        let mut transformation = compiled
            .transformation(&arena, options)
            .with_resolver(Arc::clone(&resolver));

        for (name, select, as_type) in &self.params {
            match eval_param(names, &functions, select, as_type.as_deref()) {
                Ok(value) => transformation = transformation.with_param(name.clone(), value),
                // The harness evaluates catalog parameters itself; a failure there is the
                // harness's, not the engine's.
                Err(message) => return harness_refusal(message),
            }
        }

        // Every source the case declares is registered so that `document()`
        // and `doc()` resolve them without touching the file system, each with
        // its own validation.
        for ((uri, text), validation) in self.extra.iter().zip(&self.extra_validation) {
            transformation.register_source_with(uri, text, *validation);
        }

        let outcome = match &self.source {
            Some((uri, text)) => {
                transformation.transform_source_with(uri, text, self.source_validation)
            }
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
                let results = primary_result(&compiled, &outcome, self.serialize, &self.assertion)
                    .and_then(|primary| {
                        let secondary = outcome
                            .secondary
                            .iter()
                            .map(|tree| {
                                secondary_result(&compiled, tree, self.serialize)
                                    .map(|doc| (tree.uri.clone(), doc))
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok((primary, secondary))
                    });
                match results {
                    Ok((primary, secondary)) => RunOutput {
                        primary: Some(primary),
                        secondary,
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
    assertion: &crate::catalog::Assertion,
) -> Result<ResultDocument, XsltError> {
    let (serialized, serialization_error) = match compiled.serialize_primary(outcome) {
        Ok(text) => (text, None),
        Err(e) if serialize => return Err(e),
        Err(e) => (String::new(), Some(e)),
    };
    let (tree, method, indent) = match outcome.primary {
        Some(doc) => {
            // The method `serialize_primary` used — with §20's rule that an XHTML result of a 1.0
            // stylesheet defaults to `xml` — and whether it indented.
            let params = compiled
                .primary_serialization_params(outcome)
                .ok()
                .flatten();
            let method = params.as_ref().map(|p| method_name(p.method).to_string());
            let indent = params.is_some_and(|p| p.indent);
            (default_serialization(doc), method, indent)
        }
        // No primary result: `serialize_primary` wrote nothing either.
        None => (Ok(String::new()), None, false),
    };
    let typed_asserts = match (outcome.primary, compiled.schema_set()) {
        (Some(doc), Some(set)) if doc.has_type_annotations() => {
            crate::compare::typed_asserts(doc, set, assertion)
        }
        _ => Vec::new(),
    };
    Ok(ResultDocument {
        serialized,
        tree,
        serialization_error,
        method,
        indent,
        base_uri: None,
        typed_asserts,
    })
}

/// A secondary result (`xsl:result-document`, XSLT 2.0 §19.1) in the same two forms as the
/// primary, under the same policy: serialized with its own effective output definition
/// (`CompiledStylesheet::serialize_secondary`) — a failure of which ends the case only when the
/// case asks for serialization — and written with the catalog's default parameters. Its base
/// URI is the URI it was written to, which `assert` over it can observe through `base-uri()`.
fn secondary_result(
    compiled: &xslt2::CompiledStylesheet<'_>,
    result: &xslt2::SecondaryResult<'_>,
    serialize: bool,
) -> Result<ResultDocument, XsltError> {
    let (serialized, serialization_error) = match compiled.serialize_secondary(result) {
        Ok(text) => (text, None),
        Err(e) if serialize => return Err(e),
        Err(e) => (String::new(), Some(e)),
    };
    let params = compiled.secondary_serialization_params(result).ok();
    let method = params.as_ref().map(|p| method_name(p.method).to_string());
    let indent = params.is_some_and(|p| p.indent);
    Ok(ResultDocument {
        serialized,
        tree: default_serialization(result.document),
        serialization_error,
        method,
        indent,
        base_uri: Some(result.uri.clone()),
        typed_asserts: Vec::new(),
    })
}

fn method_name(method: xslt2::output::ResolvedMethod) -> &'static str {
    use xslt2::output::ResolvedMethod::*;
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

thread_local! {
    /// The answers of [`numbering_probe`], per request, for the run (the whole harness runs on
    /// one thread).
    static NUMBERING_ANSWERS: std::cell::RefCell<
        std::collections::HashMap<crate::filter::NumberingRequest, Result<(), String>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Asks the processor, through the public API, whether it formats every number of `request`
/// with the numbering sequence of the request's token: compiles and runs
/// `<xsl:number value="." format="{token}" letter-value="…"/>` for each of them, and answers
/// `Err` naming the first number that came out as the token `1` would write it (XSLT 2.0 §12.3:
/// an unsupported token "must use a format token of 1", and "Numbers that fall outside this
/// range must be formatted using the format token 1"). The `filter::Policy::numbering` of a
/// real run; each request is asked once.
pub fn numbering_probe(request: &crate::filter::NumberingRequest) -> Result<(), String> {
    if let Some(answer) = NUMBERING_ANSWERS.with(|a| a.borrow().get(request).cloned()) {
        return answer;
    }
    let answer = ask_numbering(request);
    NUMBERING_ANSWERS.with(|a| a.borrow_mut().insert(request.clone(), answer.clone()));
    answer
}

fn ask_numbering(request: &crate::filter::NumberingRequest) -> Result<(), String> {
    let numbers: Vec<String> = request.numbers.iter().map(u64::to_string).collect();
    let letter_value = match &request.letter_value {
        Some(value) => format!(r#" letter-value="{value}""#),
        None => String::new(),
    };
    let select = numbers.join(", ");
    let token = request.token as u32;
    let sheet = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
             <xsl:output method="text"/>
             <xsl:template match="/">
               <xsl:for-each select="({select})"><xsl:number value="." format="&#x{token:X};"{letter_value}/>|</xsl:for-each>
             </xsl:template>
           </xsl:stylesheet>"#
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let failed = |e: XsltError| format!("the probe stylesheet failed: {} {}", e.code, e.message);
    let compiled = Compiler::new(&names, &functions)
        .compile_str(&sheet, "urn:harness-numbering-probe")
        .map_err(failed)?;
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run
        .transform_source("urn:harness-numbering-probe-source", "<probe/>")
        .map_err(failed)?;
    let text = compiled.serialize_primary(&outcome).map_err(failed)?;
    let formatted: Vec<&str> = text.split('|').collect();
    if formatted.len() != numbers.len() + 1 {
        return Err(format!(
            "the probe wrote {text:?} for {} numbers",
            numbers.len()
        ));
    }
    match numbers.iter().zip(&formatted).find(|(n, f)| n == f) {
        Some((n, _)) => Err(format!(
            "the processor formats {n} with the token 1 (U+{token:04X} gives {n:?})"
        )),
        None => Ok(()),
    }
}

/// The adapter's own checks, run by `--self-test`: how catalog sources are named and served,
/// and that what the harness refuses is a `harnessError`. Returns the number of checks.
pub fn self_test() -> usize {
    use std::collections::HashMap;

    use crate::catalog::{Assertion, TestCase, TestSet, TestSpec};
    use crate::environment::{ResolvedEnvironment, ResolvedSchema, ResolvedSource};

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
        xsd11: false,
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

    // M4b: in a schema-aware run each source is validated as its `validation` says (`skip` and no
    // value: not at all); a basic run validates nothing.
    let validated = |validation: &str, role: Option<&str>| ResolvedSource {
        validation: Some(validation.to_string()),
        ..inline(role, None, "<a/>")
    };
    let job_of =
        |sources: Vec<ResolvedSource>, schemas: Vec<ResolvedSchema>, schema_aware: bool| {
            let run = TestRun {
                schema_aware,
                env: ResolvedEnvironment {
                    sources,
                    schemas,
                    ..Default::default()
                },
                ..probe_run(Vec::new())
            };
            Job::from(&run).expect("the probe job builds")
        };
    let modes = job_of(
        vec![
            validated("strict", Some(".")),
            validated("lax", None),
            validated("skip", None),
            inline(None, None, "<b/>"),
        ],
        Vec::new(),
        true,
    );
    check(
        modes.source_validation == SourceValidation::Strict
            && modes.extra_validation
                == [
                    SourceValidation::Lax,
                    SourceValidation::Skip,
                    SourceValidation::Skip,
                ],
        "strict, lax, skip and no validation attribute in a schema-aware run",
    );
    let basic = job_of(
        vec![validated("strict", Some(".")), validated("lax", None)],
        Vec::new(),
        false,
    );
    check(
        basic.source_validation == SourceValidation::Skip
            && basic.extra_validation == [SourceValidation::Skip],
        "a basic run validates nothing",
    );
    // … and the context gets the environment's `source-reference` and `stylesheet-import`
    // schemas and those with no role, never a `secondary` one (loaded through the directives of
    // the schema that includes it); a basic run none.
    let schema = |file: &str, role: Option<&str>| ResolvedSchema {
        path: Some(dir.join(file)),
        uri: None,
        role: role.map(str::to_string),
        xsd_version: None,
    };
    let schemas = || {
        vec![
            schema("a.xsd", Some("source-reference")),
            schema("b.xsd", None),
            schema("c.xsd", Some("secondary")),
            schema("d.xsd", Some("stylesheet-import")),
        ]
    };
    check(
        job_of(Vec::new(), schemas(), true).schemas
            == [dir.join("a.xsd"), dir.join("b.xsd"), dir.join("d.xsd")],
        "source-reference, stylesheet-import and role-less schemas go into the context",
    );
    // A source without `validation` is validated strictly when the environment has a
    // `source-reference` schema (decision D-M), and not otherwise.
    let unvalidated = || vec![inline(Some("."), None, "<a/>")];
    check(
        job_of(unvalidated(), schemas(), true).source_validation == SourceValidation::Strict
            && job_of(unvalidated(), vec![schema("b.xsd", None)], true).source_validation
                == SourceValidation::Skip
            && job_of(unvalidated(), schemas(), false).source_validation == SourceValidation::Skip,
        "no validation attribute: strict with a source-reference schema, else skip",
    );
    check(
        job_of(Vec::new(), schemas(), false).schemas.is_empty(),
        "a basic run has no schema context",
    );
    // M4g: the context is XML Schema 1.1 with `--xsd11`, or when a schema that goes into it
    // declares `xsd-version="1.1"`; 1.0 otherwise (a `secondary` 1.1 schema does not count: it
    // is loaded through another one's directives).
    let versioned = |role: Option<&str>, version: Option<&str>| ResolvedSchema {
        xsd_version: version.map(str::to_string),
        ..schema("v.xsd", role)
    };
    let version_of = |schemas: Vec<ResolvedSchema>, schema_aware: bool, xsd11: bool| {
        let run = TestRun {
            schema_aware,
            xsd11,
            env: ResolvedEnvironment {
                schemas,
                ..Default::default()
            },
            ..probe_run(Vec::new())
        };
        Job::from(&run).expect("the probe job builds").xsd_version
    };
    check(
        version_of(schemas(), true, false) == XsdVersion::V1_0
            && version_of(schemas(), true, true) == XsdVersion::V1_1
            && version_of(vec![versioned(None, Some("1.1"))], true, false) == XsdVersion::V1_1
            && version_of(vec![versioned(None, Some("1.0"))], true, true) == XsdVersion::V1_1
            && version_of(vec![versioned(Some("secondary"), Some("1.1"))], true, false)
                == XsdVersion::V1_0
            && version_of(vec![versioned(None, Some("1.1"))], false, true) == XsdVersion::V1_0,
        "the schema context's XML Schema version: --xsd11, or a context schema of xsd-version 1.1",
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
        primary_result(
            &compiled,
            &outcome,
            true,
            &crate::catalog::Assertion::AllOf(Vec::new()),
        )
        .expect("the probe result serializes")
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
        primary_result(
            &compiled,
            &outcome,
            serialize,
            &crate::catalog::Assertion::AllOf(Vec::new()),
        )
        .map(|r| {
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

    // M3b: the base output URI is the test-set directory, so a relative href lands inside it,
    // where `assert-result-document`'s relative `uri` expects it.
    check(
        job.base_output_uri == format!("{}/", file_uri("/suite/tests/fn/accessor")),
        "the base output URI is the test-set directory",
    );
    // M3b: each secondary result in the same two forms as the primary, under the same F8
    // policy, with its absolute URI as its base URI.
    let secondary = |output: &str, serialize: bool| {
        let compiled = Compiler::new(&names, &functions)
            .compile_str(
                &format!(
                    r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                         <xsl:template match="/">
                           <xsl:result-document href="r.xml" {output}><r>x</r></xsl:result-document>
                         </xsl:template>
                       </xsl:stylesheet>"#
                ),
                "urn:harness-probe",
            )
            .expect("the probe stylesheet compiles");
        let arena = Bump::new();
        let options = TransformOptions {
            base_output_uri: Some("file:///out/".into()),
            ..Default::default()
        };
        let run = compiled.transformation(&arena, options);
        let outcome = run
            .transform_source("urn:harness-probe-source", "<in/>")
            .expect("the probe stylesheet runs");
        let tree = &outcome.secondary[0];
        secondary_result(&compiled, tree, serialize).map(|r| (tree.uri.clone(), r))
    };
    check(
        matches!(
            secondary(r#"method="text""#, true),
            Ok((uri, r)) if uri == "file:///out/r.xml"
                && r.serialized == "x"
                && r.tree.as_deref() == Ok("<r>x</r>")
                && r.method.as_deref() == Some("text")
                && r.base_uri.as_deref() == Some("file:///out/r.xml")
        ),
        "a secondary result is kept in both forms under its absolute URI",
    );
    check(
        matches!(secondary(r#"encoding="XXX-xx""#, true), Err(e) if e.code == "SESU0007"),
        "serialize=\"yes\": a secondary result's serialization failure is the run's error",
    );
    check(
        matches!(
            secondary(r#"encoding="XXX-xx""#, false),
            Ok((_, r)) if r.serialized.is_empty()
                && r.tree.as_deref() == Ok("<r>x</r>")
                && r.serialization_error.as_ref().map(|e| e.code.as_str()) == Some("SESU0007")
        ),
        "serialize absent: a secondary result's tree survives a serialization failure",
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

    // M3c follow-up: `combinations_for_numbering` is asked of the processor, number by number
    // (`numbering_probe`), and the dependency's `range` must lie inside what it supports.
    use crate::catalog::Dependency;
    use crate::filter::{
        applicability, numbering_request, range_numbers, Applicability, NumberingRequest, Policy,
    };
    let ask = |token: char, numbers: std::ops::RangeInclusive<u64>| {
        numbering_probe(&NumberingRequest {
            token,
            letter_value: None,
            numbers: numbers.collect(),
        })
    };
    check(
        ask('\u{2460}', 0..=50).is_ok(),
        "the probe: circled digits 0 to 50",
    );
    check(
        matches!(ask('\u{2460}', 0..=51), Err(m) if m.contains("formats 51 ")),
        "the probe names the first number formatted with the token 1 (circled 51)",
    );
    check(
        matches!(ask('\u{2474}', 0..=20), Err(m) if m.contains("formats 0 ")),
        "the probe: parenthesized digits have no zero",
    );
    check(
        ask('\u{2474}', 1..=20).is_ok() && ask('\u{1D360}', 1..=9).is_ok(),
        "the probe: parenthesized 1 to 20, counting rods 1 to 9",
    );
    check(
        ask('\u{1D360}', 1..=10).is_err() && ask('\u{B9}', 1..=1).is_err(),
        "the probe: counting rods stop at 9; superscript one is not a sequence",
    );
    check(
        numbering_probe(&NumberingRequest {
            token: '\u{3B1}',
            letter_value: Some("traditional".into()),
            numbers: vec![1],
        })
        .is_err(),
        "the probe: Greek alpha with letter-value is the token 1",
    );
    check(
        range_numbers(Some("0 to 20")) == Ok((0..=20).collect())
            && range_numbers(Some(" 1 \n to  9 ")) == Ok((1..=9).collect()),
        "a range \"a to b\" is every number from a to b",
    );
    check(
        matches!(range_numbers(None), Ok(n) if n.len() == 1001 && n[..1000] == *(1..=1000).collect::<Vec<u64>>() && n[1000] == u64::from(u32::MAX))
            && range_numbers(None) == range_numbers(Some("unbounded")),
        "an absent range is unbounded: 1 to 1000 and 2^32 - 1",
    );
    check(
        range_numbers(Some("0 to 100000")).is_ok_and(|n| n.len() == 1001 && n[1000] == 100000),
        "a range longer than 1000 is sampled, its ends included",
    );
    for bad in ["9 to 1", "1-9", "to 9", "1 to", "a to b", ""] {
        check(range_numbers(Some(bad)).is_err(), bad);
    }
    let numbering = |value: &str, range: Option<&str>, satisfied: bool| Dependency {
        kind: "combinations_for_numbering".into(),
        value: value.into(),
        satisfied,
        range: range.map(str::to_string),
    };
    check(
        numbering_request(&numbering(
            "GREEK SMALL LETTER ALPHA (TRADITIONAL)",
            None,
            true,
        ))
        .is_ok_and(|r| r.token == '\u{3B1}' && r.letter_value.as_deref() == Some("traditional")),
        "a parenthesized qualifier is the letter-value",
    );
    check(
        matches!(numbering_request(&numbering("KLINGON DIGIT ONE", Some("1 to 9"), true)), Err(m) if m.contains("KLINGON")),
        "an unknown name is refused, named",
    );
    let policy = Policy {
        schema_aware: false,
        xsd11: false,
        strict_recoverable: false,
        run_excluded: false,
        numbering: numbering_probe,
    };
    let with = |dependency: Dependency| TestCase {
        name: "numbering-probe".into(),
        description: String::new(),
        dependencies: vec![dependency],
        environment: None,
        test: TestSpec::default(),
        result: Assertion::AllOf(Vec::new()),
        line: 0,
    };
    let verdict = |value: &str, range: Option<&str>, satisfied: bool| {
        applicability(&set, &with(numbering(value, range, satisfied)), &policy)
    };
    for (value, range) in [
        ("CIRCLED DIGIT ONE", "0 to 50"),
        ("CIRCLED DIGIT ONE", "1 to 9"),
        ("DIGIT ONE FULL STOP", "0 to 10"),
        ("DINGBAT NEGATIVE CIRCLED DIGIT ONE", "1 to 20"),
        ("DIGIT ONE COMMA", "0 to 9"),
        ("MENDE KIKAKUI DIGIT ONE", "1 to 9"),
    ] {
        check(
            verdict(value, Some(range), true) == Applicability::Run,
            &format!("runnable: {value} {range}"),
        );
    }
    for (value, range) in [
        ("CIRCLED DIGIT ONE", Some("0 to 1000")),
        ("PARENTHESIZED DIGIT ONE", Some("0 to 20")),
        ("AEGEAN NUMBER ONE", Some("1 to 50")),
        ("CIRCLED DIGIT ONE", None),
        ("GREEK SMALL LETTER ALPHA (ALPHABETIC)", None),
        ("SUBSCRIPT ONE", Some("1 to 9")),
    ] {
        check(
            matches!(verdict(value, range, true), Applicability::Unsatisfied(m) if m.contains("not supported")),
            &format!("not runnable: {value} {range:?}"),
        );
    }
    check(
        matches!(verdict("CIRCLED DIGIT ONE", Some("1 to 20"), false), Applicability::Unsatisfied(m) if m.contains("needs its absence")),
        "satisfied=\"false\" of a supported sequence is not runnable",
    );
    check(
        verdict("CIRCLED DIGIT ONE", Some("1 to 1000"), false) == Applicability::Run,
        "satisfied=\"false\" of an unsupported range is runnable",
    );

    // The implicit timezone the harness pins (review R12; the Wave B follow-up): UTC, the value the
    // catalog assumes ("we currently assume that an absent timezone means UTC", `date-012` …
    // `date-018`, `date-042`) — for `implicit-timezone()`, for `current-dateTime()`, and for comparing a
    // value without a timezone with one that has one. Twice: through the option every case's
    // transformation gets (`implicit_timezone`), and without it, which reads the host's offset —
    // UTC only because `driver::main` pins the process's `TZ` (the guard).
    let sheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
          xmlns:xs="http://www.w3.org/2001/XMLSchema">
        <xsl:output method="text"/>
        <xsl:template match="/"><xsl:value-of select="implicit-timezone(),
            timezone-from-dateTime(current-dateTime()),
            xs:dateTime('1996-12-12T13:13:00') = xs:dateTime('1996-12-12T13:13:00+02:00'),
            xs:dateTime('1996-12-12T13:13:00') = xs:dateTime('1996-12-12T13:13:00Z')"/></xsl:template>
      </xsl:stylesheet>"#;
    let compiled = Compiler::new(&names, &functions)
        .compile_str(sheet, "urn:harness-timezone-probe")
        .expect("the timezone probe compiles");
    for (implicit_timezone, how) in [
        (implicit_timezone(), "the option the cases get"),
        (None, "the host's offset under the TZ pin"),
    ] {
        let arena = Bump::new();
        let options = TransformOptions {
            implicit_timezone,
            ..Default::default()
        };
        let outcome = compiled
            .transformation(&arena, options)
            .transform_source("urn:harness-timezone-probe-source", "<in/>")
            .expect("the timezone probe runs");
        let text = compiled
            .serialize_primary(&outcome)
            .expect("the timezone probe serializes");
        check(
            text == "PT0S PT0S false true",
            &format!("the implicit timezone is UTC by {how} (the timezone probe wrote {text:?})"),
        );
    }
    checks
}
