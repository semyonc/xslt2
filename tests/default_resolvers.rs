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

// ── A schema-aware compiler starts with its context's resolver ─────────────

const SCHEMA_MAIN_URI: &str = "http://example.com/xsl/typed.xsl";

/// The principal module of the schema-aware tests: it includes `inc.xsl`
/// (which says where it was read from) and imports `size.xsd` by location,
/// both relative to the module, and reads `other.xml` and `note.txt` at run
/// time.
const SCHEMA_MAIN: &str = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                    xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:s="urn:size"
                    exclude-result-prefixes="xs s">
  <xsl:import-schema namespace="urn:size" schema-location="size.xsd"/>
  <xsl:include href="inc.xsl"/>
  <xsl:output method="text"/>
  <xsl:template match="/">
    <xsl:value-of select="s:origin(), 11 castable as s:size, 7 castable as s:size,
                          doc('other.xml')/other/@from, unparsed-text('note.txt')" separator="|"/>
  </xsl:template>
</xsl:stylesheet>"#;

/// A resolver that holds the schema-aware stylesheet (its included module
/// says it comes `from` the given name), its schema, and the run's second
/// document and text resource.
fn schema_memory(from: &str) -> MemoryResolver {
    let included = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:s="urn:size">
  <xsl:function name="s:origin"><xsl:sequence select="'{from}'"/></xsl:function>
</xsl:stylesheet>"#
    );
    memory(from)
        .with(SCHEMA_MAIN_URI, SCHEMA_MAIN)
        .with("http://example.com/xsl/inc.xsl", included)
        .with(
            "http://example.com/xsl/size.xsd",
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:size">
  <xs:simpleType name="size">
    <xs:restriction base="xs:integer"><xs:maxInclusive value="10"/></xs:restriction>
  </xs:simpleType>
</xs:schema>"#,
        )
}

/// Runs a compiled schema-aware stylesheet over a small source, with the
/// transformation's default resolver.
fn run_text(sheet: &xslt2::CompiledStylesheet<'_>) -> Result<String, xslt2::XsltError> {
    let arena = xslt2::bumpalo::Bump::new();
    let outcome = sheet
        .transformation(&arena, TransformOptions::default())
        .transform_source("urn:example:in", "<doc/>")?;
    sheet.serialize_primary(&outcome)
}

#[test]
fn a_schema_aware_compiler_reads_through_the_resolver_its_context_was_built_with() {
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::new(schema_memory("context")))
        .stylesheet_uri(SCHEMA_MAIN_URI, &functions)
        .expect("pre-scans from memory")
        .build()
        .expect("builds");
    // No `with_resolver` on the compiler: the principal module, the included
    // one, and every read of the run come from the context's resolver.
    let sheet = Compiler::schema_aware(&context, &functions)
        .compile_uri(SCHEMA_MAIN_URI)
        .expect("compiles from the context's resolver");
    assert_eq!(
        run_text(&sheet).expect("runs"),
        "context|false|true|context|note context"
    );
    // The same for a principal module given as text: its `xsl:include` is
    // read through the context's resolver.
    let sheet = Compiler::schema_aware(&context, &functions)
        .compile_str(SCHEMA_MAIN, SCHEMA_MAIN_URI)
        .expect("includes from the context's resolver");
    assert_eq!(
        run_text(&sheet).expect("runs"),
        "context|false|true|context|note context"
    );
}

#[test]
fn with_resolver_overrides_the_contexts_resolver() {
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .with_resolver(Arc::new(schema_memory("context")))
        .stylesheet_uri(SCHEMA_MAIN_URI, &functions)
        .expect("pre-scans from memory")
        .build()
        .expect("builds");
    // Another resolver with the same modules: the compiler reads its included
    // module, and the run its documents, from that one.
    let sheet = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::new(schema_memory("compiler")))
        .compile_uri(SCHEMA_MAIN_URI)
        .expect("compiles from the compiler's resolver");
    assert_eq!(
        run_text(&sheet).expect("runs"),
        "compiler|false|true|compiler|note compiler"
    );
    // A resolver that serves nothing: the context's modules are not reached
    // around it.
    let error = Compiler::schema_aware(&context, &functions)
        .with_resolver(Arc::new(MemoryResolver::new()))
        .compile_uri(SCHEMA_MAIN_URI)
        .unwrap_err();
    assert_eq!(error.code, "XTSE0165", "{error}");
}

#[test]
fn a_context_without_a_resolver_reads_files_and_so_does_its_compiler() {
    let dir = std::env::temp_dir().join(format!(
        "xslt2-schema-context-resolver-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let files = schema_memory("file");
    for name in ["typed.xsl", "inc.xsl", "size.xsd", "other.xml", "note.txt"] {
        let text = xslt2::Resolver::resolve_text(&files, &format!("http://example.com/xsl/{name}"))
            .expect("the resource is in memory");
        std::fs::write(dir.join(name), text).expect("writes");
    }
    let main = dir.join("typed.xsl");

    let functions = XsltFunctions::new();
    // A builder given no resolver reads files, and so does the compiler of
    // its context.
    let built = SchemaContext::builder()
        .stylesheet_file(&main, &functions)
        .and_then(|builder| builder.build())
        .and_then(|context| {
            let sheet = Compiler::schema_aware(&context, &functions).compile_file(&main)?;
            run_text(&sheet)
        });
    // `SchemaContext::new()` and `with_version` have the built-in types only;
    // their compiler reads files too (a stylesheet without
    // `xsl:import-schema`).
    std::fs::write(dir.join("plain.xsl"), READS_OTHER).expect("writes");
    let plain = [
        SchemaContext::new(),
        SchemaContext::with_version(xslt2::XsdVersion::V1_1),
    ]
    .map(|context| {
        let sheet =
            Compiler::schema_aware(&context, &functions).compile_file(&dir.join("plain.xsl"))?;
        run_text(&sheet)
    });
    std::fs::remove_dir_all(&dir).expect("removes the scratch directory");
    assert_eq!(
        built.expect("every read is served by the file system"),
        "file|false|true|file|note file"
    );
    for result in plain {
        assert_eq!(
            result.expect("every read is served by the file system"),
            "file|file|true|false|note file|true"
        );
    }
}
