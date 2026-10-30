//! Transforms an XML file with a schema-aware XSLT 2.0 stylesheet, the source
//! validated against the stylesheet's schemas.
//!
//! ```text
//! cargo run --example schema_aware -- STYLESHEET INPUT [SCHEMA ...] [--xsd11]
//! ```
//!
//! The schemas are those the stylesheet imports with `xsl:import-schema`,
//! together with every `SCHEMA` file given here. The source document is
//! validated strictly against them, so its nodes carry their types, and one
//! that is not valid is an error. `--xsd11` reads every schema as XML Schema
//! 1.1. The primary result is written to standard output; warnings go to
//! standard error.

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use xslt2::resolver::path_to_uri;
use xslt2::{
    Compiler, SchemaContext, SourceValidation, TransformOptions, XsdVersion, XsltFunctions,
};

type Failure = Box<dyn Error + Send + Sync>;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let xsd11 = args.iter().any(|arg| arg == "--xsd11");
    args.retain(|arg| arg != "--xsd11");
    if args.len() < 2 || args[0].starts_with('-') {
        eprintln!("usage: schema_aware STYLESHEET INPUT [SCHEMA ...] [--xsd11]");
        return ExitCode::from(2);
    }
    match xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, move || run(&args, xsd11)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String], xsd11: bool) -> Result<(), Failure> {
    let (stylesheet, input, schemas) = (Path::new(&args[0]), &args[1], &args[2..]);
    let functions = XsltFunctions::new();

    // Step 1: the schema context, from the stylesheet's xsl:import-schema
    // declarations and the host's own schemas. It owns the schema set and its
    // name table, which the compiled stylesheet and every document share.
    let mut builder = SchemaContext::builder();
    if xsd11 {
        builder = builder.xsd_version(XsdVersion::V1_1);
    }
    for schema in schemas {
        builder = builder.schema_file(Path::new(schema));
    }
    let context = builder.stylesheet_file(stylesheet, &functions)?.build()?;
    for warning in context.warnings() {
        eprintln!("warning: {warning}");
    }
    for import in context.imports() {
        if !import.found() {
            let namespace = import.namespace().unwrap_or("(no namespace)");
            eprintln!("warning: no schema found for the import of {namespace}");
        }
    }

    // Step 2: compile against the context, and run.
    let sheet = Compiler::schema_aware(&context, &functions).compile_file(stylesheet)?;
    let arena = xslt2::bumpalo::Bump::new();
    let options = TransformOptions {
        source_validation: SourceValidation::Strict,
        ..TransformOptions::default()
    };
    let run = sheet.transformation(&arena, options);
    let outcome = run.transform_uri(&path_to_uri(Path::new(input)))?;
    for warning in run.take_warnings() {
        eprintln!("warning: {warning}");
    }
    for message in &outcome.messages {
        eprintln!("xsl:message: {}", message.content);
    }
    std::io::stdout().write_all(&sheet.serialize_primary_to_bytes(&outcome)?)?;
    Ok(())
}
