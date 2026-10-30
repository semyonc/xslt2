//! Every public type implements `Debug` (the crate warns on
//! `missing_debug_implementations`). The types that hold a compiled stylesheet,
//! a run or result trees show what identifies them — URIs, counts, the output
//! method, the options — and never a tree or an instruction table; closures and
//! `dyn` objects are placeholders.

use std::sync::Arc;

use xslt2::{Compiler, MemoryResolver, NameTable, SchemaContext, TransformOptions, XsltFunctions};

const SHEET: &str = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:param name="p" select="1"/>
  <xsl:template match="/">
    <xsl:message>a message</xsl:message>
    <xsl:result-document href="part.xml" method="text">part-text</xsl:result-document>
    <primary-element>primary-text</primary-element>
  </xsl:template>
  <xsl:function name="f:id" xmlns:f="urn:f"><xsl:param name="x"/><xsl:sequence select="$x"/></xsl:function>
</xsl:stylesheet>"#;

#[test]
fn the_debug_output_identifies_without_dumping_trees() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let functions_debug = format!("{functions:?}");
    assert!(
        functions_debug.starts_with("XsltFunctions { signatures: "),
        "{functions_debug}"
    );
    assert!(
        functions_debug.contains("collations: CollationRegistry"),
        "{functions_debug}"
    );

    let compiler = Compiler::new(&names, &functions).with_resolver(Arc::new(
        MemoryResolver::new().with("urn:example:sheet", SHEET),
    ));
    assert_eq!(
        format!("{compiler:?}"),
        "Compiler { schema_aware: false, xsd_version: None, resolver: <dyn Resolver>, .. }"
    );
    let sheet = compiler.compile_uri("urn:example:sheet").expect("compiles");
    assert_eq!(
        format!("{sheet:?}"),
        "CompiledStylesheet { base_uri: \"urn:example:sheet\", modules: 1, templates: 1, \
         stylesheet_functions: 1, global_variables: 1, schema_aware: false, \
         resolver: <dyn Resolver>, .. }"
    );

    let options = TransformOptions {
        base_output_uri: Some("urn:example:out/".to_string()),
        ..TransformOptions::default()
    };
    let options_debug = format!("{options:?}");
    assert!(
        options_debug.contains("base_output_uri: Some(\"urn:example:out/\")"),
        "{options_debug}"
    );
    assert!(!options_debug.contains("stack_size"), "{options_debug}");

    let arena = xslt2::bumpalo::Bump::new();
    let run = sheet
        .transformation(&arena, options)
        .with_message_sink(Box::new(|_| {}));
    let outcome = run
        .transform_source(
            "urn:example:in",
            "<source-element>source-text</source-element>",
        )
        .expect("runs");
    let run_debug = format!("{run:?}");
    assert!(
        run_debug.starts_with(
            "Transformation { stylesheet: \"urn:example:sheet\", options: TransformOptions {"
        ),
        "{run_debug}"
    );
    assert!(
        run_debug.ends_with(
            "params: 0, documents: 3, resolver: <dyn Resolver>, message_sink: Some(<closure>), .. }"
        ),
        "{run_debug}"
    );
    let store_debug = format!("{:?}", run.store().borrow());
    assert_eq!(
        store_debug,
        "DocumentStore { documents: 3, uris: [\"urn:example:in\"], schema_aware: false, .. }"
    );

    let outcome_debug = format!("{outcome:?}");
    assert_eq!(
        outcome_debug,
        "Outcome { primary: Some(<document>), base_output_uri: \"urn:example:out/\", \
         secondary: [\"urn:example:out/part.xml\"], messages: 1, .. }"
    );
    let secondary_debug = format!("{:?}", outcome.secondary[0]);
    assert_eq!(
        secondary_debug,
        "SecondaryResult { uri: \"urn:example:out/part.xml\", document: <document>, \
         method: Some(Text), .. }"
    );
    assert_eq!(
        format!("{:?}", outcome.messages[0]),
        "Message { content: \"a message\", terminate: false }"
    );

    // No tree content anywhere.
    for text in [&run_debug, &store_debug, &outcome_debug, &secondary_debug] {
        for content in [
            "source-element",
            "source-text",
            "primary-element",
            "primary-text",
            "part-text",
        ] {
            assert!(!text.contains(content), "{content} in {text}");
        }
    }
}

#[test]
fn a_schema_aware_compiler_shows_its_xml_schema_version() {
    let functions = XsltFunctions::new();
    let context = SchemaContext::with_version(xslt2::XsdVersion::V1_1);
    let compiler = Compiler::schema_aware(&context, &functions);
    assert_eq!(
        format!("{compiler:?}"),
        "Compiler { schema_aware: true, xsd_version: Some(V1_1), resolver: <dyn Resolver>, .. }"
    );
}

#[test]
fn a_failed_compilation_unwraps_its_error() {
    // `Result::unwrap_err` needs `Debug` on the success type.
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let error = Compiler::new(&names, &functions)
        .compile_str(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template/></xsl:stylesheet>"#,
            "urn:example:sheet",
        )
        .unwrap_err();
    assert_eq!(error.code, "XTSE0500");
}
