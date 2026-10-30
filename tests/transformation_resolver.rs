//! The resolver a transformation reads documents through: by default the one
//! its stylesheet was compiled with (`Compiler::with_resolver`), so a host that
//! compiles from memory does not read `document()`, `doc()` or
//! `unparsed-text()` targets from the file system; `Transformation::with_resolver`
//! overrides it; a compiler given no resolver reads files, and so do its
//! transformations.

use std::sync::Arc;

use xslt2::{
    Compiler, MemoryResolver, NameTable, SchemaContext, SourceValidation, TransformOptions,
    XsltFunctions,
};

const MAIN_URI: &str = "http://example.com/xsl/main.xsl";

/// Reads the second document, and a text resource, in every way a run can:
/// `document()`, `doc()`, `doc-available()`, `unparsed-text()` and
/// `unparsed-text-available()`, each by a URI relative to the module.
const READS_OTHER: &str = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="text"/>
  <xsl:template match="/">
    <xsl:value-of select="document('other.xml')/other/@from,
                          doc('other.xml')/other/@from,
                          doc-available('other.xml'),
                          doc-available('missing.xml'),
                          unparsed-text('note.txt'),
                          unparsed-text-available('note.txt')" separator="|"/>
  </xsl:template>
</xsl:stylesheet>"#;

/// Asks only whether the second document and the text resource are there.
const PROBES_OTHER: &str = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="text"/>
  <xsl:template match="/">
    <xsl:value-of select="doc-available('other.xml'), unparsed-text-available('note.txt')" separator="|"/>
  </xsl:template>
</xsl:stylesheet>"#;

/// A resolver that holds the stylesheet, the second document and the text
/// resource, the last two saying they come `from` the given name.
fn memory(from: &str) -> MemoryResolver {
    MemoryResolver::new()
        .with(MAIN_URI, READS_OTHER)
        .with(
            "http://example.com/xsl/other.xml",
            format!(r#"<other from="{from}"/>"#),
        )
        .with("http://example.com/xsl/note.txt", format!("note {from}"))
}

#[test]
fn a_transformation_reads_through_the_resolver_its_stylesheet_was_compiled_with() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let sheet = Compiler::new(&names, &functions)
        .with_resolver(Arc::new(memory("memory")))
        .compile_uri(MAIN_URI)
        .expect("compiles from memory");
    let arena = xslt2::bumpalo::Bump::new();
    // No `with_resolver`: the compiler's resolver is the default.
    let outcome = sheet
        .transformation(&arena, TransformOptions::default())
        .transform_source("urn:example:in", "<doc/>")
        .expect("every read is served from memory");
    assert_eq!(
        sheet.serialize_primary(&outcome).expect("serializes"),
        "memory|memory|true|false|note memory|true"
    );
}

#[test]
fn with_resolver_overrides_the_compilers_resolver() {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiler = Compiler::new(&names, &functions).with_resolver(Arc::new(memory("compiler")));
    let sheet = compiler
        .compile_uri(MAIN_URI)
        .expect("compiles from memory");
    let arena = xslt2::bumpalo::Bump::new();
    let outcome = sheet
        .transformation(&arena, TransformOptions::default())
        .with_resolver(Arc::new(memory("transformation")))
        .transform_source("urn:example:in", "<doc/>")
        .expect("every read is served from memory");
    assert_eq!(
        sheet.serialize_primary(&outcome).expect("serializes"),
        "transformation|transformation|true|false|note transformation|true"
    );

    // A resolver that serves nothing: the compiler's resources are not
    // reached around it.
    let probe = compiler
        .compile_str(PROBES_OTHER, MAIN_URI)
        .expect("compiles");
    let outcome = probe
        .transformation(&arena, TransformOptions::default())
        .with_resolver(Arc::new(MemoryResolver::new()))
        .transform_source("urn:example:in", "<doc/>")
        .expect("runs");
    assert_eq!(
        probe.serialize_primary(&outcome).expect("serializes"),
        "false|false"
    );
}

#[test]
fn a_compiler_without_a_resolver_reads_files_and_so_do_its_transformations() {
    let dir = std::env::temp_dir().join(format!(
        "xslt2-transformation-resolver-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::write(dir.join("main.xsl"), READS_OTHER).expect("writes");
    std::fs::write(dir.join("other.xml"), r#"<other from="file"/>"#).expect("writes");
    std::fs::write(dir.join("note.txt"), "note file").expect("writes");

    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let result = Compiler::new(&names, &functions)
        .compile_file(&dir.join("main.xsl"))
        .and_then(|sheet| {
            let arena = xslt2::bumpalo::Bump::new();
            let outcome = sheet
                .transformation(&arena, TransformOptions::default())
                .transform_source("urn:example:in", "<doc/>")?;
            sheet.serialize_primary(&outcome)
        });
    std::fs::remove_dir_all(&dir).expect("removes the scratch directory");
    assert_eq!(
        result.expect("every read is served by the file system"),
        "file|file|true|false|note file|true"
    );
}

#[test]
fn a_schema_aware_transformation_reads_and_validates_through_the_compilers_resolver() {
    let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="other">
        <xs:complexType><xs:attribute name="from" type="xs:string"/></xs:complexType>
      </xs:element>
    </xs:schema>"#;
    // A typed attribute is an `xs:string`; an untyped one would be
    // `xs:untypedAtomic`.
    let stylesheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                            xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xsl:output method="text"/>
      <xsl:template match="/">
        <xsl:value-of select="doc('other.xml')/other/@from,
                              data(doc('other.xml')/other/@from) instance of xs:string,
                              unparsed-text('note.txt')" separator="|"/>
      </xsl:template>
    </xsl:stylesheet>"#;
    let functions = XsltFunctions::new();
    let resolver = Arc::new(memory("memory").with(MAIN_URI, stylesheet));
    let context = SchemaContext::builder()
        .with_resolver(resolver.clone())
        .schema_str(schema, "http://example.com/xsd/other.xsd")
        .stylesheet_uri(MAIN_URI, &functions)
        .expect("pre-scans from memory")
        .build()
        .expect("builds");
    let sheet = Compiler::schema_aware(&context, &functions)
        .with_resolver(resolver)
        .compile_uri(MAIN_URI)
        .expect("compiles from memory");
    let arena = xslt2::bumpalo::Bump::new();
    let options = TransformOptions {
        source_validation: SourceValidation::Strict,
        ..TransformOptions::default()
    };
    // `doc('other.xml')` is read through the compiler's resolver and validated
    // strictly against the context's schema; the principal source is not
    // validated.
    let outcome = sheet
        .transformation(&arena, options)
        .transform_source_with("urn:example:in", "<doc/>", SourceValidation::Skip)
        .expect("every read is served from memory");
    assert_eq!(
        sheet.serialize_primary(&outcome).expect("serializes"),
        "memory|true|note memory"
    );
}
