//! W3C XSLT conformance harness for `xslt2`.
//!
//! Runs the XSLT 2.0-applicable subset of the public W3C XSLT 3.0 test suite
//! (https://github.com/w3c/xslt30-test, checked out at `../../xslt30-test`) — see
//! `XSLT2_DESIGN_SKETCH.md` §2 for the suite landscape, dependency policy and assertion
//! semantics. Always run in `--release`:
//!
//! ```text
//! cargo test --test xslts --release -- -s ../../xslt30-test
//! cargo test --test xslts --release -- -g value-of -v
//! cargo test --test xslts --release -- --report /tmp/xslts-results.xml
//! ```
//!
//! Module layout mirrors `xsd-schema/tests/xqts`: catalog / filter / environment / runner /
//! compare / report, pulled in with `#[path]`; `xpath30` rewrites the XPath 3.0 constructs some
//! assertions use into exact XPath 2.0 equivalents.

#[path = "catalog.rs"]
mod catalog;
#[path = "compare.rs"]
mod compare;
#[path = "engine.rs"]
mod engine;
#[path = "environment.rs"]
mod environment;
#[path = "filter.rs"]
mod filter;
#[path = "report.rs"]
mod report;
#[path = "runner.rs"]
mod runner;
#[path = "xpath30.rs"]
mod xpath30;

use std::path::PathBuf;
use std::process::exit;
use std::time::Instant;

use compare::{CompareContext, Outcome};
use filter::{Applicability, Policy};
use report::{CaseRecord, Report, Status};
use runner::{Engine, StubEngine};

struct Options {
    suite: PathBuf,
    groups: Vec<String>,
    tests: Vec<String>,
    all: bool,
    max: Option<usize>,
    verbose: bool,
    failed_only: bool,
    list: bool,
    schema_aware: bool,
    xsd11: bool,
    strict: bool,
    report: Option<PathBuf>,
    expect_pass: Option<usize>,
    /// Run against the M0 `StubEngine` instead of the processor.
    stub: bool,
    /// Run the suite even in a debug build.
    force_debug: bool,
    /// Test-sets to leave out of the run entirely (by name substring).
    skip: Vec<String>,
    /// Compare `assert-xml` leniently everywhere (as if `ignore-prefixes="true"`).
    lenient_xml: bool,
}

fn print_usage() {
    println!("W3C XSLT conformance harness (XSLT 2.0-applicable subset of xslt30-test)");
    println!();
    println!("Usage: cargo test --test xslts --release -- [OPTIONS]");
    println!();
    println!("Options:");
    println!(
        "  -s, --suite PATH      Path to the xslt30-test checkout (default: ../../xslt30-test)"
    );
    println!("  -g, --group NAME      Only test-sets whose name contains NAME (repeatable)");
    println!("  -t, --test NAME       Only the test case named NAME (repeatable, exact)");
    println!("  -a, --all             Also run cases on the exclusion list");
    println!("  -m, --max N           Stop after N runnable cases");
    println!("  -v, --verbose         Print every case with its status");
    println!("  -f, --failed          Print only fail / wrongError / harnessError cases");
    println!("  -l, --list            List test-sets with applicable/runnable counts and exit");
    println!("      --schema-aware    Enable the schema_aware feature (runs SA cases)");
    println!("      --xsd11           With --schema-aware: XSD 1.1 schemas are available");
    println!("      --strict          Recoverable errors are signalled (on-multiple-match=error)");
    println!("      --stub            Run the M0 stub engine instead of the processor");
    println!("      --debug           Run the suite in a debug build (slow; normally skipped)");
    println!("      --skip NAME       Leave out test-sets whose name contains NAME (repeatable)");
    println!("      --lenient-xml     Compare assert-xml ignoring prefixes and namespace");
    println!("                        declarations everywhere, as if every assertion carried");
    println!("                        ignore-prefixes=\"true\". The catalog default is false");
    println!("                        (Canonical-XML equivalence); this is for triage only.");
    println!("      --report FILE     Write results in the W3C xslt30-test-results XML format");
    println!("      --expect-pass N   Exit with status 1 if fewer than N cases pass");
    println!(
        "      --self-test       Run the harness's own checks (assertion evaluation) and exit"
    );
    println!("  -h, --help            Show this help");
}

fn parse_args() -> Options {
    let mut o = Options {
        suite: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../xslt30-test"),
        groups: Vec::new(),
        tests: Vec::new(),
        all: false,
        max: None,
        verbose: false,
        failed_only: false,
        list: false,
        schema_aware: false,
        xsd11: false,
        strict: false,
        report: None,
        expect_pass: None,
        stub: false,
        force_debug: false,
        skip: Vec::new(),
        lenient_xml: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| {
            eprintln!("{flag} needs a value");
            exit(2)
        })
    };
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "--suite" => o.suite = PathBuf::from(value(&mut i, "--suite")),
            "-g" | "--group" => o.groups.push(value(&mut i, "--group")),
            "-t" | "--test" => o.tests.push(value(&mut i, "--test")),
            "-a" | "--all" => o.all = true,
            "-m" | "--max" => {
                o.max = Some(value(&mut i, "--max").parse().unwrap_or_else(|_| {
                    eprintln!("--max needs a number");
                    exit(2)
                }))
            }
            "-v" | "--verbose" => o.verbose = true,
            "-f" | "--failed" => o.failed_only = true,
            "-l" | "--list" => o.list = true,
            "--schema-aware" => o.schema_aware = true,
            "--xsd11" => o.xsd11 = true,
            "--strict" => o.strict = true,
            "--stub" => o.stub = true,
            "--debug" => o.force_debug = true,
            "--skip" => o.skip.push(value(&mut i, "--skip")),
            "--lenient-xml" => o.lenient_xml = true,
            "--report" => o.report = Some(PathBuf::from(value(&mut i, "--report"))),
            "--expect-pass" => {
                o.expect_pass = Some(value(&mut i, "--expect-pass").parse().unwrap_or_else(|_| {
                    eprintln!("--expect-pass needs a number");
                    exit(2)
                }))
            }
            "--self-test" => {
                let n = compare::self_test::run();
                println!(
                    "xslts: self-tests passed: {} compare functions, {} xpath30 checks, \
                     {} engine checks",
                    n.compare_functions, n.xpath30_checks, n.engine_checks
                );
                exit(0)
            }
            "-h" | "--help" => {
                print_usage();
                exit(0)
            }
            // `cargo test` forwards its own filters/flags when invoked without `--`; ignore them.
            other if other.starts_with("--") => {}
            other => eprintln!("ignoring argument {other:?}"),
        }
        i += 1;
    }
    o
}

fn suite_version(root: &std::path::Path) -> String {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).unwrap_or_default();
    let head = head.trim();
    let commit = if let Some(r) = head.strip_prefix("ref: ") {
        std::fs::read_to_string(root.join(".git").join(r)).unwrap_or_default()
    } else {
        head.to_string()
    };
    let commit = commit.trim();
    if commit.is_empty() {
        "unknown".to_string()
    } else {
        commit.chars().take(12).collect()
    }
}

fn main() {
    // Q33: no tail-call elimination in M1, so a deeply recursive named
    // template needs a bigger stack than a thread gets by default. The whole
    // run happens on one such thread; per-case panics are still caught by the
    // runner.
    let stack_size = std::env::var("XSLT2_STACK_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(xslt2::DEFAULT_STACK_SIZE);
    xslt2::run_on_large_stack(stack_size, run_suite);
}

fn run_suite() {
    let opts = parse_args();
    let started = Instant::now();

    // The suite is a release-mode tool: a debug build is roughly twenty times
    // slower and its stack frames are large enough that a deeply recursive
    // stylesheet exhausts even the big worker stack. A plain `cargo test`
    // therefore reports the harness as skipped rather than spending minutes to
    // produce numbers nobody reads.
    if cfg!(debug_assertions) && !opts.force_debug {
        println!(
            "xslts: skipped — build with --release (or pass --debug to run it anyway):\n  \
             cargo test --test xslts --release -- -s {}",
            opts.suite.display()
        );
        return;
    }

    if !opts.suite.join("catalog.xml").is_file() {
        eprintln!(
            "xslts: test suite not found at {} — clone https://github.com/w3c/xslt30-test there or pass -s PATH. Nothing run.",
            opts.suite.display()
        );
        exit(0);
    }
    let catalog = match catalog::load_catalog(&opts.suite) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xslts: {e}");
            exit(2)
        }
    };
    let policy = Policy {
        schema_aware: opts.schema_aware,
        xsd11: opts.xsd11,
        strict_recoverable: opts.strict,
        run_excluded: opts.all,
        // The processor itself answers `combinations_for_numbering`, also under `--stub`: the
        // dependency is about the processor, whichever engine runs the cases.
        numbering: engine::numbering_probe,
    };
    let engine: Box<dyn Engine> = if opts.stub {
        Box::new(StubEngine)
    } else {
        Box::new(engine::XsltEngine)
    };

    let mut report = Report {
        engine: engine.name().to_string(),
        engine_version: engine.version(),
        suite_version: suite_version(&catalog.root),
        schema_aware: opts.schema_aware,
        ..Default::default()
    };

    println!(
        "xslts: suite {} ({}), {} test-sets, engine {} {}",
        catalog.root.display(),
        report.suite_version,
        catalog.test_sets.len(),
        report.engine,
        report.engine_version
    );
    if opts.list {
        println!(
            "{:<34} {:>6} {:>10} {:>8}  file",
            "test-set", "cases", "applicable", "runnable"
        );
    }

    let mut load_errors = 0usize;
    let mut ran = 0usize;
    'sets: for set_ref in &catalog.test_sets {
        if !opts.groups.is_empty()
            && !opts
                .groups
                .iter()
                .any(|g| set_ref.name.contains(g.as_str()))
        {
            continue;
        }
        if opts.skip.iter().any(|s| set_ref.name.contains(s.as_str())) {
            continue;
        }
        let set = match catalog::load_test_set(set_ref) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("xslts: {e}");
                load_errors += 1;
                continue;
            }
        };
        let family = report::family_of(&set.rel_file, &set.name);

        if opts.list {
            let mut applicable = 0;
            let mut runnable = 0;
            for case in &set.cases {
                match filter::applicability(&set, case, &policy) {
                    Applicability::Run => {
                        applicable += 1;
                        runnable += 1
                    }
                    Applicability::NotXslt20(_) => {}
                    _ => applicable += 1,
                }
            }
            println!(
                "{:<34} {:>6} {:>10} {:>8}  {}",
                set.name,
                set.cases.len(),
                applicable,
                runnable,
                set.rel_file
            );
            continue;
        }

        for case in &set.cases {
            if !opts.tests.is_empty() && !opts.tests.iter().any(|t| t == &case.name) {
                continue;
            }
            let profile = filter::profile_of(&set, case);
            let mut rec = CaseRecord {
                set: set.name.clone(),
                name: case.name.clone(),
                family: family.clone(),
                profile,
                status: Status::NotRun,
                message: None,
                micros: 0,
            };
            match filter::applicability(&set, case, &policy) {
                Applicability::NotXslt20(spec) => {
                    rec.status = Status::NotApplicable;
                    rec.message = Some(spec);
                }
                Applicability::Unsatisfied(reason) => {
                    rec.status = Status::NotRun;
                    rec.message = Some(reason);
                }
                Applicability::Excluded(reason) => {
                    rec.status = Status::Excluded;
                    rec.message = Some(reason.to_string());
                }
                Applicability::Run => {
                    if let Some(max) = opts.max {
                        if ran >= max {
                            break 'sets;
                        }
                    }
                    ran += 1;
                    let env = match environment::resolve(case, &set) {
                        Ok(env) => env,
                        Err(e) => {
                            rec.status = Status::HarnessError;
                            rec.message = Some(e);
                            print_case(&opts, &rec);
                            report.record(rec);
                            continue;
                        }
                    };
                    let run = runner::prepare(&set, case, env, opts.schema_aware, opts.strict);
                    let exec = runner::execute(engine.as_ref(), &run);
                    rec.micros = exec.duration.as_micros();
                    if let Some(p) = exec.panic {
                        rec.status = Status::HarnessError;
                        rec.message = Some(format!("engine panicked: {p}"));
                    } else if exec.output.not_implemented {
                        rec.status = Status::NotImplemented;
                        rec.message = Some(match &exec.output.error {
                            Some(e) => e.message.clone(),
                            None => "engine not implemented".to_string(),
                        });
                    } else {
                        let inputs = compare::CaseInputs::of(&run);
                        let cx = CompareContext {
                            set_dir: &set.dir,
                            lenient_xml: opts.lenient_xml,
                            inputs: &inputs,
                            entity_results: filter::result_may_be_entity(case),
                        };
                        let v = compare::evaluate_guarded(&case.result, &exec.output, &cx);
                        let failed = matches!(v.outcome, Outcome::Fail | Outcome::WrongError)
                            && !filter::expects_only_static_errors(case);
                        if let Some(message) = exec
                            .output
                            .untyped_input_fence(failed, v.message.as_deref())
                        {
                            // M4b fence (M4a review R7): the case failed on an untyped tree
                            // where it asked for a validated one.
                            rec.status = Status::NotImplemented;
                            rec.message = Some(message);
                        } else {
                            rec.status = match v.outcome {
                                Outcome::Pass => Status::Pass,
                                Outcome::Fail => Status::Fail,
                                Outcome::WrongError => Status::WrongError,
                                Outcome::HarnessError => Status::HarnessError,
                            };
                            rec.message = v.message;
                        }
                    }
                }
            }
            print_case(&opts, &rec);
            report.record(rec);
        }
    }
    report.seconds = started.elapsed().as_secs_f64();

    if opts.list {
        return;
    }
    report.print_summary();
    if load_errors > 0 {
        println!("  test-set files that failed to load: {load_errors}");
    }
    if let Some(path) = &opts.report {
        match report.write_w3c_xml(path) {
            Ok(()) => println!("  results written to {}", path.display()),
            Err(e) => eprintln!("xslts: cannot write {}: {e}", path.display()),
        }
    }
    if let Some(n) = opts.expect_pass {
        if report.totals.pass < n {
            eprintln!(
                "xslts: expected at least {n} passing cases, got {}",
                report.totals.pass
            );
            exit(1);
        }
    }
    if load_errors > 0 {
        exit(2);
    }
}

fn print_case(opts: &Options, rec: &CaseRecord) {
    let failing = matches!(
        rec.status,
        Status::Fail | Status::WrongError | Status::HarnessError | Status::NotImplemented
    );
    if !(opts.verbose || (opts.failed_only && failing)) {
        return;
    }
    let tag = match rec.status {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::WrongError => "WRONG-ERROR",
        Status::HarnessError => "HARNESS-ERROR",
        Status::NotRun => "NOT-RUN",
        Status::NotApplicable => "N/A",
        Status::Excluded => "EXCLUDED",
        Status::NotImplemented => "NOT-IMPL",
    };
    match &rec.message {
        Some(m) => println!("  {tag:<13} {}/{}: {m}", rec.set, rec.name),
        None => println!("  {tag:<13} {}/{}", rec.set, rec.name),
    }
}
