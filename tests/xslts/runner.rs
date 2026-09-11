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

/// A result tree, as the engine serialized it (default parameters unless the test's
/// `<output>` says otherwise): method xml, indent no, omit-xml-declaration yes.
#[derive(Debug, Clone, Default)]
pub struct ResultDocument {
    pub serialized: String,
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
        RunOutput { not_implemented: true, ..Default::default() }
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
    TestRun { set, case, env, stylesheet, base_dir: set.dir.clone(), schema_aware, strict_recoverable }
}

fn pick(refs: &[StylesheetRef]) -> Option<&StylesheetRef> {
    refs.iter()
        .find(|s| s.role.as_deref() == Some("principal"))
        .or_else(|| refs.iter().find(|s| s.role.is_none()))
        .or_else(|| refs.first())
}

fn principal_stylesheet(case: &TestCase, env: &ResolvedEnvironment, set: &TestSet) -> Option<PathBuf> {
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
        Ok(output) => Execution { output, duration: start.elapsed(), panic: None },
        Err(payload) => Execution { output: RunOutput::default(), duration: start.elapsed(), panic: Some(panic_message(payload)) },
    }
}

fn panic_message(p: Box<dyn Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
