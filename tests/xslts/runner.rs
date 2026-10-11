#![allow(dead_code)] // the model is consumed by the engine adapter, not yet by the stub
//! The engine boundary of the harness: what one test run needs (`TestRun`), what it produces
//! (`RunOutput`), the `Engine` trait, and the M0 `StubEngine` that implements nothing.
//!
//! When the processor exists, `XsltEngine` implements `Engine` here and the rest of the
//! harness is untouched. Engine panics are caught per test so one bug cannot end the run.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use xslt2::XsltError;

use crate::catalog::{StylesheetRef, TestCase, TestSet};
use crate::environment::ResolvedEnvironment;

/// Everything an engine needs to execute one test case.
pub struct TestRun<'a> {
    pub set: &'a TestSet,
    pub case: &'a TestCase,
    pub env: ResolvedEnvironment,
    /// Principal stylesheet: `test/stylesheet` (role `principal` first), else the environment's.
    pub stylesheet: Option<PathBuf>,
    /// Test-set directory: base for relative URIs and `xsl:result-document` hrefs.
    pub base_dir: PathBuf,
    pub schema_aware: bool,
    pub strict_recoverable: bool,
}

/// A result tree, in the two textual forms the catalog's assertions are defined over.
///
/// The harness cannot keep the tree itself — it lives in the engine's arena, which ends with
/// the run — so it keeps two serializations of it, and each assertion reads the one the
/// catalog (`admin/catalog-schema.xsd`) defines it over; see `compare::evaluate`.
#[derive(Debug, Clone)]
pub struct ResultDocument {
    /// The tree as the stylesheet's own `xsl:output` serializes it, which is what
    /// `serialization-matches` ("The result of the query must be serialized using the
    /// serialization options specified within the query") and `assert-serialization` compare.
    pub serialized: String,
    /// The tree written with the catalog's default serialization parameters — method `xml`,
    /// indent `no`, omit-xml-declaration `yes`, nothing else — or why it could not be.
    /// Every assertion about the result *tree* re-parses this text: `assert` is "evaluated
    /// with the document node of the result document as the context item", and `assert-xml`
    /// compares "a serialization of the expression result using the default serialization
    /// parameters". Indentation, the text method or an output encoding of the stylesheet
    /// never reach it, so a re-parse sees the tree's own nodes and nothing else — except
    /// whitespace-only text directly under the document node, which no XML parse keeps.
    pub tree: Result<String, String>,
    /// Why the stylesheet's own serialization failed, when the case does not ask for
    /// serialization (`<output serialize="yes"/>`; the catalog: "Determines whether or not
    /// serialization takes place. The default is no.") and so the failure is not the run's
    /// error: the tree assertions are still evaluated, and `serialized` is empty.
    pub serialization_error: Option<XsltError>,
    /// The output method the stylesheet's serialization used (`xml`, `xhtml`, `html`, `text`),
    /// for an `assert-serialization` that does not name one: its `method` "Identifies the
    /// output method used for serialization, which affects the way in which the actual results
    /// can be compared with the expected results".
    pub method: Option<String>,
}

impl ResultDocument {
    /// A result whose two forms are the same text: an `xsl:message` document, which the
    /// processor writes with the default parameters already (`xslt2::receiver::serialize_document`;
    /// the catalog's `assert-message` asserts on the message "considered as an XML document"),
    /// or a self-test fixture.
    pub fn same(text: impl Into<String>) -> Self {
        let text = text.into();
        ResultDocument {
            serialized: text.clone(),
            tree: Ok(text),
            serialization_error: None,
            method: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct RunOutput {
    pub primary: Option<ResultDocument>,
    /// `xsl:result-document` outputs by the URI they were written to (absolute or as given).
    pub secondary: Vec<(String, ResultDocument)>,
    /// `xsl:message` outputs, each serialized as a document.
    pub messages: Vec<String>,
    /// The error that ended the transformation, if any.
    pub error: Option<XsltError>,
    /// The engine does not implement what the test needs (M0: everything).
    pub not_implemented: bool,
    /// The harness could not run the case as the catalog specifies it (an unsupported catalog
    /// construct, a catalog parameter it cannot evaluate, …). Never an engine error: an
    /// `<error code="*"/>` assertion must not be satisfied by the harness refusing the case, so
    /// `compare::evaluate` turns this into a `harnessError` before looking at any assertion.
    pub harness_error: Option<String>,
}

pub trait Engine {
    fn name(&self) -> &'static str;
    fn version(&self) -> String;
    fn run(&self, run: &TestRun<'_>) -> RunOutput;
}

/// M0 engine: every test is "not implemented", so every runnable case fails honestly.
pub struct StubEngine;

impl Engine for StubEngine {
    fn name(&self) -> &'static str {
        "xslt2-stub"
    }
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
    fn run(&self, _run: &TestRun<'_>) -> RunOutput {
        RunOutput {
            not_implemented: true,
            ..Default::default()
        }
    }
}

pub struct Execution {
    pub output: RunOutput,
    pub duration: Duration,
    /// Panic message if the engine panicked.
    pub panic: Option<String>,
}

pub fn prepare<'a>(
    set: &'a TestSet,
    case: &'a TestCase,
    env: ResolvedEnvironment,
    schema_aware: bool,
    strict_recoverable: bool,
) -> TestRun<'a> {
    let stylesheet = principal_stylesheet(case, &env, set);
    TestRun {
        set,
        case,
        env,
        stylesheet,
        base_dir: set.dir.clone(),
        schema_aware,
        strict_recoverable,
    }
}

fn pick(refs: &[StylesheetRef]) -> Option<&StylesheetRef> {
    refs.iter()
        .find(|s| s.role.as_deref() == Some("principal"))
        .or_else(|| refs.iter().find(|s| s.role.is_none()))
        .or_else(|| refs.first())
}

fn principal_stylesheet(
    case: &TestCase,
    env: &ResolvedEnvironment,
    set: &TestSet,
) -> Option<PathBuf> {
    if let Some(s) = pick(&case.test.stylesheets) {
        return s.file.as_ref().map(|f| set.dir.join(f));
    }
    env.stylesheets
        .iter()
        .find(|s| s.role.as_deref() == Some("principal"))
        .or_else(|| env.stylesheets.iter().find(|s| s.role.is_none()))
        .or_else(|| env.stylesheets.first())
        .and_then(|s| s.path.clone())
}

pub fn execute(engine: &dyn Engine, run: &TestRun<'_>) -> Execution {
    let start = Instant::now();
    match catch_unwind(AssertUnwindSafe(|| engine.run(run))) {
        Ok(output) => Execution {
            output,
            duration: start.elapsed(),
            panic: None,
        },
        Err(payload) => Execution {
            output: RunOutput::default(),
            duration: start.elapsed(),
            panic: Some(panic_message(payload)),
        },
    }
}

/// The text of a caught panic. Shared with `compare`, which guards assertion evaluation the
/// same way `execute` guards the engine call.
pub fn panic_message(p: Box<dyn Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
