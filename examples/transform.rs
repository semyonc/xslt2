//! Transforms an XML file with an XSLT 2.0 stylesheet.
//!
//! ```text
//! cargo run --example transform -- STYLESHEET INPUT [NAME=VALUE ...]
//! ```
//!
//! Each `NAME=VALUE` sets the stylesheet parameter `NAME` (a name in no
//! namespace) to the string `VALUE`. The primary result is written to standard
//! output in its output encoding. The `xsl:message` output and the URIs of the
//! documents `xsl:result-document` created go to standard error: the
//! transformation returns those documents instead of writing them, and this
//! example only lists them. Their URIs are resolved against the current
//! directory (the base output URI).

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use xslt2::resolver::path_to_uri;
use xslt2::{Compiler, NameTable, QName, TransformOptions, XPathValue, XsltFunctions};

type Failure = Box<dyn Error + Send + Sync>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args[0].starts_with('-') {
        eprintln!("usage: transform STYLESHEET INPUT [NAME=VALUE ...]");
        return ExitCode::from(2);
    }
    // A stylesheet may recurse deeply, and templates recurse on the native
    // stack: the whole job runs on a thread with a large one. Everything the
    // job builds stays on that thread; only the outcome comes back.
    match xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), Failure> {
    let (stylesheet, input, params) = (&args[0], &args[1], &args[2..]);

    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let sheet = Compiler::new(&names, &functions).compile_file(Path::new(stylesheet))?;
    if sheet.is_backwards_compatible() {
        // §3.6: a processor "should output a warning" for a 1.0 stylesheet.
        eprintln!("warning: the stylesheet runs with XSLT 1.0 behaviour (version=\"1.0\")");
    }

    let arena = xslt2::bumpalo::Bump::new();
    let options = TransformOptions {
        base_output_uri: Some(format!("{}/", path_to_uri(&std::env::current_dir()?))),
        ..TransformOptions::default()
    };
    let mut run = sheet.transformation(&arena, options);
    for param in params {
        let (name, value) = param
            .split_once('=')
            .ok_or_else(|| format!("a parameter is NAME=VALUE, not {param:?}"))?;
        run = run.with_param(QName::local(name), XPathValue::from(value));
    }

    let result = run.transform_uri(&path_to_uri(Path::new(input)));
    // The messages of a run that failed (xsl:message terminate="yes") count too.
    for message in run.take_messages() {
        eprintln!("xsl:message: {}", message.content);
    }
    let outcome = result?;
    for message in &outcome.messages {
        eprintln!("xsl:message: {}", message.content);
    }

    std::io::stdout().write_all(&sheet.serialize_primary_to_bytes(&outcome)?)?;
    for result in &outcome.secondary {
        let bytes = sheet.serialize_secondary_to_bytes(result)?;
        eprintln!("result document: {} ({} bytes)", result.uri, bytes.len());
    }
    Ok(())
}
