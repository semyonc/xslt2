//! W3C XSLT conformance harness for `xslt2`.
//!
//! Runs the XSLT 2.0-applicable subset of the public W3C XSLT 3.0 test suite
//! (https://github.com/w3c/xslt30-test, checked out at `../../xslt30-test`); `filter.rs` holds
//! the dependency policy and the exclusions, `compare.rs` the assertion semantics. Always run in
//! `--release`:
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
    /// Run against the `StubEngine`, which implements nothing, instead of the processor.
    stub: bool,
    /// Run the suite even in a debug build.
    force_debug: bool,
    /// Test-sets to leave out of the run entirely (by name substring).
    skip: Vec<String>,
    /// Compare `assert-xml` leniently everywhere (as if `ignore-prefixes="true"`).
    lenient_xml: bool,
    /// Arguments that are neither the harness's options nor the test-runner options `cargo test`
    /// may forward to every test target: refused when the suite would run (`run_suite`).
    unknown: Vec<String>,
}

/// The options of the standard test runner that `cargo test -- …` forwards to every test target,
/// this one included: accepted and ignored. (`--list`, `--skip` and `--test` are the harness's
/// own.) The first group takes no value, the second one value, as `--flag=value` or
/// `--flag value`.
const TEST_RUNNER_FLAGS: &[&str] = &[
    "--include-ignored",
    "--ignored",
    "--force-run-in-process",
    "--exclude-should-panic",
    "--bench",
    "--nocapture",
    "--no-capture",
    "-q",
    "--quiet",
    "--exact",
    "--show-output",
    "--report-time",
    "--ensure-time",
    "--shuffle",
];
const TEST_RUNNER_OPTIONS: &[&str] = &[
    "--logfile",
    "--test-threads",
    "--color",
    "--format",
    "-Z",
    "--shuffle-seed",
];

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
    println!("      --xsd11           With --schema-aware: XSD 1.1 is satisfied and every schema context is XML Schema 1.1");
    println!("      --strict          Recoverable errors are signalled (on-multiple-match=error)");
    println!("      --stub            Run a stub engine that reports every case not implemented");
    println!("      --debug           Run the suite in a debug build (slow; normally skipped)");
    println!("      --skip NAME       Leave out test-sets whose name contains NAME (repeatable)");
    println!("      --lenient-xml     Compare assert-xml ignoring prefixes and namespace");
    println!("                        declarations everywhere, as if every assertion carried");
    println!("                        ignore-prefixes=\"true\". The catalog default is false");
    println!("                        (Canonical-XML equivalence); this is for triage only.");
    println!("      --host-timezone   Keep the host's implicit timezone (default: TZ=UTC, the catalog's)");
    println!("      --report FILE     Write results in the W3C xslt30-test-results XML format");
    println!("      --expect-pass N   Exit with status 1 if fewer than N cases pass");
    println!(
        "      --self-test       Run the harness's own checks (assertion evaluation) and exit"
    );
    println!("  -h, --help            Show this help");
}

fn parse_args() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    parse(&args)
}

/// Reads the command line `args` (without the program name). `--help` and `--self-test` act at
/// once; a missing or malformed value exits with status 2; anything unknown is collected in
/// `Options::unknown`.
fn parse(args: &[String]) -> Options {
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
        unknown: Vec::new(),
    };
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
            // Read in `main` (the implicit timezone).
            "--host-timezone" => {}
            "--report" => o.report = Some(PathBuf::from(value(&mut i, "--report"))),
            "--expect-pass" => {
                o.expect_pass = Some(value(&mut i, "--expect-pass").parse().unwrap_or_else(|_| {
                    eprintln!("--expect-pass needs a number");
                    exit(2)
                }))
            }
            "--self-test" => {
                let n = compare::self_test::run();
                let driver_checks = self_test();
                println!(
                    "xslts: self-tests passed: {} compare functions, {} xpath30 checks, \
                     {} engine checks, {} driver checks",
                    n.compare_functions, n.xpath30_checks, n.engine_checks, driver_checks
                );
                exit(0)
            }
            "-h" | "--help" => {
                print_usage();
                exit(0)
            }
            flag if TEST_RUNNER_FLAGS.contains(&flag) => {}
            option if TEST_RUNNER_OPTIONS.contains(&option) => {
                value(&mut i, option);
            }
            option
                if TEST_RUNNER_OPTIONS
                    .iter()
                    .any(|name| option.starts_with(&format!("{name}="))) => {}
            // An unknown option, or a positional argument (`cargo test NAME` forwards its
            // test-name filter to every target): kept, and refused if the suite would run.
            other => o.unknown.push(other.to_string()),
        }
        i += 1;
    }
    o
}

/// The checks of the command line: the harness's options, the test runner's forwarded options
/// and what is refused. Returns how many were made; panics on the first failure.
fn self_test() -> usize {
    let parse_str = |line: &[&str]| parse(&line.iter().map(|a| a.to_string()).collect::<Vec<_>>());
    let mut checks = 0;
    let mut check = |ok: bool, what: &str| {
        assert!(ok, "driver self-test: {what}");
        checks += 1;
    };
    let o = parse_str(&[
        "-t",
        "lre-027",
        "--test",
        "lre-028",
        "-g",
        "value-of",
        "--schema-aware",
    ]);
    check(
        o.tests == ["lre-027", "lre-028"] && o.groups == ["value-of"] && o.schema_aware,
        "the harness's options",
    );
    check(o.unknown.is_empty(), "the harness's options are known");
    let o = parse_str(&[
        "--nocapture",
        "--test-threads",
        "1",
        "--test-threads=2",
        "--color=never",
        "-q",
        "--skip",
        "unicode-90",
    ]);
    check(
        o.unknown.is_empty() && o.skip == ["unicode-90"],
        "the test runner's forwarded options are accepted",
    );
    // One argument holding several options (a shell variable passed as one word) is not
    // `-t`: refused, not read as a request to run everything.
    let o = parse_str(&["-t lre-027 -t lre-028"]);
    check(
        o.unknown == ["-t lre-027 -t lre-028"] && o.tests.is_empty(),
        "several options in one argument are refused",
    );
    let o = parse_str(&["--no-such-option", "lre-027"]);
    check(
        o.unknown == ["--no-such-option", "lre-027"],
        "an unknown option and a positional argument are refused",
    );
    checks
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
    // The implicit timezone. XSLT 2.0 §5.4.3.2: "The implicit timezone is
    // implementation-defined"; this processor's default is the host's local offset, so a run's
    // results would depend on where and when it runs (`date-012` fails under CEST, +02:00, and
    // passes under CET). The harness pins UTC, the value the catalog assumes ("we currently assume
    // that an absent timezone means UTC", `date-012` … `date-018`, `date-042`): every case's
    // transformation gets it as `TransformOptions::implicit_timezone` (`engine::implicit_timezone`),
    // and the process's `TZ` is set to UTC here too, as a guard for
    // the XPath the option does not reach, which reads the host's offset: the harness's own
    // `assert` evaluation and catalog parameters (the XPath engine, outside any transformation),
    // XML Schema 1.1 assertions and type alternatives evaluated during validation,
    // and `use-when` (evaluated at compile time). `--host-timezone` keeps the host's for both. Set
    // here, before the worker thread starts.
    if !std::env::args().any(|arg| arg == "--host-timezone") {
        std::env::set_var("TZ", "UTC");
    }
    // No tail-call elimination, so a deeply recursive named
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

    // An argument the harness does not know would otherwise run the whole suite, `unicode-90`
    // included: refuse it. (A skipped debug build ignores it: `cargo test NAME` forwards its
    // filter to every target.)
    if !opts.unknown.is_empty() {
        eprintln!(
            "xslts: unknown argument(s) {:?}; nothing run. `--help` lists the options.",
            opts.unknown
        );
        exit(2);
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
                    let run = runner::prepare(
                        &set,
                        case,
                        env,
                        opts.schema_aware,
                        opts.xsd11,
                        opts.strict,
                    );
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
