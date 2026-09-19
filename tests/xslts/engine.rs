//! The conformance harness's adapter to the real processor.
//!
//! Everything here goes through `xslt2`'s public API — `Compiler`,
//! `CompiledStylesheet`, `Transformation` — which is the point: if a test case
//! needs something the adapter cannot express, the public API is missing it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bumpalo::Bump;
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
            Err(message) => RunOutput {
                error: Some(XsltError::internal(message)),
                ..Default::default()
            },
        }
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
            // A source given as inline `<content>` has neither a file nor a
            // URI; it still needs one, because the document store keys
            // documents by URI.
            //
            // The synthetic URI has to sit **inside** the test-set directory:
            // it is the base URI of every node of that document, so §16.1
            // resolves a relative reference read out of the source against it
            // (`document-0601` and friends). `path_to_uri` of a directory has
            // no trailing slash, and without one the reference would resolve
            // against the *parent* directory.
            let uri = source_uri(entry.uri.as_deref(), entry.path.as_deref()).or_else(|| {
                Some(format!(
                    "{}/#inline-source-{index}",
                    xslt2::resolver::path_to_uri(&run.base_dir)
                ))
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
        })
    }

    fn execute(self) -> RunOutput {
        let names = NameTable::new();
        let functions = XsltFunctions::new();

        let mut memory = MemoryResolver::with_fallback(Arc::new(xslt2::FileResolver));
        for (uri, text) in &self.extra {
            memory.insert(uri.clone(), text.clone());
        }
        if let Some((uri, text)) = &self.source {
            memory.insert(uri.clone(), text.clone());
        }
        let resolver: Arc<dyn Resolver> = Arc::new(memory);

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
                Err(e) => return failure(e),
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
                match compiled.serialize_primary(&outcome) {
                    Ok(serialized) => RunOutput {
                        primary: Some(ResultDocument { serialized }),
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
/// the literal values the suite uses.
fn eval_param(
    names: &NameTable,
    functions: &XsltFunctions,
    select: &str,
    as_type: Option<&str>,
) -> Result<XPathValue<xslt2::Nav<'static>>, XsltError> {
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
    let expr = XPathExpr::compile(&source, &ctx).map_err(|e| {
        XsltError::internal(format!("cannot compile the parameter {select:?}: {e}"))
    })?;
    expr.evaluator(&ctx)
        .run::<xslt2::Nav<'static>>()
        .map_err(|e| XsltError::internal(format!("cannot evaluate the parameter {select:?}: {e}")))
}

fn source_uri(uri: Option<&str>, path: Option<&Path>) -> Option<String> {
    if let Some(uri) = uri {
        if !uri.trim().is_empty() {
            return Some(uri.to_string());
        }
    }
    path.map(xslt2::resolver::path_to_uri)
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
