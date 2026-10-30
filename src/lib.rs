//! A schema-aware XSLT 2.0 processor, built on
//! [xsd-schema](https://github.com/semyonc/xsd-schema).
//!
//! `xslt2` compiles XSLT 2.0 stylesheets, runs them over XML documents, and
//! serializes the results with the `xml`, `xhtml`, `html` and `text` output
//! methods. It is a basic XSLT processor and a schema-aware one (§21).
//! [`xsd_schema`] supplies the XPath 2.0 engine, the data model, XML Schema
//! 1.0 and 1.1 validation and the in-memory document; this crate adds XSLT:
//! stylesheet modules, patterns and template rules, the instructions, the
//! XSLT functions, serialization and the schema-aware mode.
//!
//! The specification is [XSL Transformations (XSLT) Version 2.0 (Second
//! Edition)](https://www.w3.org/TR/2021/REC-xslt20-20210330/); a section number
//! such as §20 in this documentation is a section of that document.
//!
//! # The API
//!
//! | To | Use |
//! |---|---|
//! | compile a stylesheet | [`Compiler`] — [`Compiler::new`] for a basic processor, [`Compiler::schema_aware`] for a schema-aware one — with an [`XsltFunctions`] and a [`NameTable`]; it makes a [`CompiledStylesheet`] |
//! | run it | [`CompiledStylesheet::transformation`] makes a [`Transformation`], set up by [`TransformOptions`] and by [`Transformation::with_param`], [`with_initial_template`](Transformation::with_initial_template), [`with_initial_mode`](Transformation::with_initial_mode) ([`ModeKey`]), [`with_resolver`](Transformation::with_resolver) and [`with_message_sink`](Transformation::with_message_sink); [`transform_source`](Transformation::transform_source), [`transform_uri`](Transformation::transform_uri), [`transform_document`](Transformation::transform_document) and [`transform_without_source`](Transformation::transform_without_source) run it |
//! | read the results | [`Outcome`]: the primary result, the [`SecondaryResult`]s of `xsl:result-document`, the [`Message`]s of `xsl:message` |
//! | serialize them | [`CompiledStylesheet::serialize_primary`], [`serialize_secondary`](CompiledStylesheet::serialize_secondary) and their `_to_bytes` forms; the output definitions and the resolved serialization parameters are in [`output`] |
//! | read stylesheet modules and documents | the [`Resolver`] trait, [`FileResolver`] (the default), [`MemoryResolver`]; URI helpers in [`resolver`] |
//! | validate against schemas | [`SchemaContext`], built by a [`SchemaContextBuilder`] ([`SchemaImport`], [`ImportSource`], [`XsdVersion`]); [`SourceValidation`] for the source documents |
//! | handle errors | [`XsltError`] with its [`ErrorKind`], [`ErrorOrigin`] and [`Location`]; the error codes in [`error::codes`] and [`error::codes_internal`] |
//! | add collations | [`XsltFunctions::register_collation`] and the [`collation`] module |
//! | pass nodes and values | [`XPathValue`] (a parameter's value), [`Nav`] (a node), [`DocumentStore`] and [`DocId`] (the documents of a run), [`TimezoneOffset`] |
//! | recurse deeply | [`run_on_large_stack`], [`DEFAULT_STACK_SIZE`], [`DEFAULT_MAX_RECURSION_DEPTH`], [`OTHER_THREAD_STACK_LIMIT`] |
//!
//! The crate re-exports [`xsd_schema`] and [`bumpalo`], whose types appear in
//! its API, so that a program can depend on `xslt2` alone and always get the
//! versions it was built with.
//!
//! **Ownership.** The caller owns what a compilation borrows — the name table
//! (or, in schema-aware mode, the [`SchemaContext`], which owns its own) and
//! the [`XsltFunctions`] — and the arena ([`bumpalo::Bump`]) a transformation
//! builds its documents in. A [`CompiledStylesheet`] is immutable and can run
//! any number of transformations, one after the other or each with its own
//! arena; the result trees of a run live in its arena. A [`Compiler`], a
//! [`CompiledStylesheet`], a [`Transformation`] and an [`Outcome`] are not
//! `Send`: a job that moves to another thread moves whole (see the last
//! example).
//!
//! # Examples
//!
//! ## Compile and transform a string
//!
//! ```
//! use xslt2::{Compiler, NameTable, TransformOptions, XsltFunctions};
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let sheet = Compiler::new(&names, &functions).compile_str(
//!     r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!          <xsl:template match="/">
//!            <total><xsl:value-of select="sum(//item/@price)"/></total>
//!          </xsl:template>
//!        </xsl:stylesheet>"#,
//!     "urn:example:sheet", // the stylesheet's base URI
//! )?;
//!
//! let arena = xslt2::bumpalo::Bump::new();
//! let run = sheet.transformation(&arena, TransformOptions::default());
//! let outcome = run.transform_source(
//!     "urn:example:order",
//!     r#"<order><item price="2.5"/><item price="4"/></order>"#,
//! )?;
//! assert_eq!(
//!     sheet.serialize_primary(&outcome)?,
//!     r#"<?xml version="1.0" encoding="UTF-8"?><total>6.5</total>"#
//! );
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## Files, and the results of `xsl:result-document`
//!
//! A transformation writes nothing anywhere: every result tree comes back in
//! the [`Outcome`], under its URI, and the caller serializes and stores it. The
//! URIs are resolved against the base output URI
//! ([`TransformOptions::base_output_uri`]).
//!
//! ```
//! use std::fs;
//! use xslt2::resolver::{path_to_uri, uri_to_path};
//! use xslt2::{Compiler, NameTable, TransformOptions, XsltFunctions};
//!
//! # let dir = std::env::temp_dir().join(format!("xslt2-doc-{}", std::process::id()));
//! # fs::create_dir_all(&dir)?;
//! // `dir` is a scratch directory with a stylesheet and a source document.
//! fs::write(
//!     dir.join("split.xsl"),
//!     r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!          <xsl:output omit-xml-declaration="yes"/>
//!          <xsl:template match="/">
//!            <index>
//!              <xsl:for-each select="book/chapter">
//!                <xsl:result-document href="chapter-{position()}.xml" omit-xml-declaration="yes">
//!                  <xsl:copy-of select="."/>
//!                </xsl:result-document>
//!                <entry href="chapter-{position()}.xml"/>
//!              </xsl:for-each>
//!            </index>
//!          </xsl:template>
//!        </xsl:stylesheet>"#,
//! )?;
//! fs::write(dir.join("book.xml"), "<book><chapter>One</chapter><chapter>Two</chapter></book>")?;
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let sheet = Compiler::new(&names, &functions).compile_file(&dir.join("split.xsl"))?;
//! let arena = xslt2::bumpalo::Bump::new();
//! let options = TransformOptions {
//!     // A directory: the trailing `/` makes `href="chapter-1.xml"` a file in it.
//!     base_output_uri: Some(format!("{}/", path_to_uri(&dir.join("out")))),
//!     ..TransformOptions::default()
//! };
//! let outcome = sheet
//!     .transformation(&arena, options)
//!     .transform_uri(&path_to_uri(&dir.join("book.xml")))?;
//!
//! assert_eq!(
//!     sheet.serialize_primary(&outcome)?,
//!     r#"<index><entry href="chapter-1.xml"/><entry href="chapter-2.xml"/></index>"#
//! );
//! assert_eq!(outcome.secondary.len(), 2);
//! let first = &outcome.secondary[0];
//! assert!(first.uri.ends_with("/out/chapter-1.xml"));
//! assert_eq!(sheet.serialize_secondary(first)?, "<chapter>One</chapter>");
//!
//! // Storing the results is the caller's business: here, as files.
//! for result in &outcome.secondary {
//!     let path = uri_to_path(&result.uri).expect("a file: URI");
//!     fs::create_dir_all(path.parent().expect("a directory"))?;
//!     fs::write(&path, sheet.serialize_secondary_to_bytes(result)?)?;
//! }
//! # fs::remove_dir_all(&dir)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Parameters, the initial template and the initial mode
//!
//! ```
//! use xslt2::{Compiler, ModeKey, NameTable, QName, TransformOptions, XPathValue, XsltFunctions};
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let sheet = Compiler::new(&names, &functions).compile_str(
//!     r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!          <xsl:output omit-xml-declaration="yes"/>
//!          <xsl:param name="greeting" select="'Hello'"/>
//!          <xsl:template name="main"><out><xsl:value-of select="$greeting"/></out></xsl:template>
//!          <xsl:template match="/"><full/></xsl:template>
//!          <xsl:template match="/" mode="brief"><brief/></xsl:template>
//!        </xsl:stylesheet>"#,
//!     "urn:example:sheet",
//! )?;
//! let arena = xslt2::bumpalo::Bump::new();
//!
//! // A named template as the entry point, with no source document, and a
//! // stylesheet parameter (§9.5) set by the host.
//! let outcome = sheet
//!     .transformation(&arena, TransformOptions::default())
//!     .with_param(QName::local("greeting"), XPathValue::from("Good morning"))
//!     .with_initial_template(QName::local("main"))
//!     .transform_without_source()?;
//! assert_eq!(sheet.serialize_primary(&outcome)?, "<out>Good morning</out>");
//!
//! // The template rules of a mode other than the default one (§6.5).
//! let outcome = sheet
//!     .transformation(&arena, TransformOptions::default())
//!     .with_initial_mode(ModeKey::Named(QName::local("brief")))
//!     .transform_source("urn:example:in", "<doc/>")?;
//! assert_eq!(sheet.serialize_primary(&outcome)?, "<brief/>");
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## Resources from memory
//!
//! The compiler reads `xsl:include` and `xsl:import` modules, and a
//! transformation reads `document()` and `doc()` targets, through a
//! [`Resolver`]. [`MemoryResolver`] serves text given in advance, and can fall
//! back to another resolver for everything else.
//!
//! ```
//! use std::sync::Arc;
//! use xslt2::{Compiler, MemoryResolver, NameTable, TransformOptions, XsltFunctions};
//!
//! let resolver = Arc::new(
//!     MemoryResolver::new()
//!         .with(
//!             "http://example.com/xsl/main.xsl",
//!             r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!                  <xsl:include href="common.xsl"/>
//!                  <xsl:output omit-xml-declaration="yes"/>
//!                  <xsl:template match="/">
//!                    <out><xsl:value-of select="document('prices.xml')/prices/@currency"/></out>
//!                  </xsl:template>
//!                </xsl:stylesheet>"#,
//!         )
//!         .with(
//!             "http://example.com/xsl/common.xsl",
//!             r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"/>"#,
//!         )
//!         .with("http://example.com/xsl/prices.xml", r#"<prices currency="EUR"/>"#),
//! );
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let sheet = Compiler::new(&names, &functions)
//!     .with_resolver(resolver.clone())
//!     .compile_uri("http://example.com/xsl/main.xsl")?;
//! let arena = xslt2::bumpalo::Bump::new();
//! let outcome = sheet
//!     .transformation(&arena, TransformOptions::default())
//!     .with_resolver(resolver) // for document()
//!     .transform_source("urn:example:in", "<doc/>")?;
//! assert_eq!(sheet.serialize_primary(&outcome)?, "<out>EUR</out>");
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## The schema-aware mode
//!
//! A schema-aware compilation takes two steps: a [`SchemaContext`], built
//! from the stylesheet's `xsl:import-schema` declarations and any schemas of
//! the host's own, and [`Compiler::schema_aware`], which compiles against it.
//! A source document validated against the context's schemas has typed
//! nodes.
//!
//! ```
//! use xslt2::{Compiler, SchemaContext, SourceValidation, TransformOptions, XsltFunctions};
//!
//! let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
//!   <xs:element name="order">
//!     <xs:complexType>
//!       <xs:sequence>
//!         <xs:element name="item" maxOccurs="unbounded">
//!           <xs:complexType><xs:attribute name="price" type="xs:decimal" use="required"/></xs:complexType>
//!         </xs:element>
//!       </xs:sequence>
//!     </xs:complexType>
//!   </xs:element>
//! </xs:schema>"#;
//! let stylesheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
//!                         xmlns:xs="http://www.w3.org/2001/XMLSchema" exclude-result-prefixes="xs">
//!   <xsl:output omit-xml-declaration="yes"/>
//!   <xsl:template match="/">
//!     <out><xsl:value-of select="data(//item[1]/@price) instance of xs:decimal"/></out>
//!   </xsl:template>
//! </xsl:stylesheet>"#;
//!
//! let functions = XsltFunctions::new();
//! let context = SchemaContext::builder()
//!     .schema_str(schema, "urn:example:order.xsd") // a schema of the host's own
//!     .stylesheet_str(stylesheet, "urn:example:sheet", &functions)? // its xsl:import-schema
//!     .build()?;
//! let sheet = Compiler::schema_aware(&context, &functions)
//!     .compile_str(stylesheet, "urn:example:sheet")?;
//!
//! let arena = xslt2::bumpalo::Bump::new();
//! let options = TransformOptions {
//!     source_validation: SourceValidation::Strict,
//!     ..TransformOptions::default()
//! };
//! let run = sheet.transformation(&arena, options);
//! let outcome = run.transform_source("urn:example:in", r#"<order><item price="2.50"/></order>"#)?;
//! assert_eq!(sheet.serialize_primary(&outcome)?, "<out>true</out>");
//!
//! // A document that is not valid is an error.
//! assert!(run.transform_source("urn:example:bad", r#"<order><item price="cheap"/></order>"#).is_err());
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## Matching an error code
//!
//! Every error carries a code: one of XSLT 2.0's (Appendix E, in
//! [`error::codes`]), an XPath, F&O or serialization code passed through, or
//! the name the stylesheet gave `fn:error`.
//!
//! ```
//! use xslt2::error::codes;
//! use xslt2::{Compiler, ErrorKind, ErrorOrigin, NameTable, TransformOptions, XsltFunctions};
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let compile = |body: &str| {
//!     Compiler::new(&names, &functions).compile_str(
//!         &format!(
//!             r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">{body}</xsl:stylesheet>"#
//!         ),
//!         "urn:example:sheet",
//!     )
//! };
//!
//! // A static error: an xsl:template with neither `match` nor `name` (§6.1).
//! let error = compile("<xsl:template/>").err().expect("a static error");
//! assert_eq!(error.code, codes::XTSE0500);
//! assert_eq!(error.kind(), ErrorKind::Static);
//! assert!(error.location.is_some());
//!
//! // A dynamic error the stylesheet raises itself, identified by a QName.
//! let sheet = compile(
//!     r#"<xsl:template match="/" xmlns:e="urn:example:errors">
//!          <xsl:sequence select="error(QName('urn:example:errors', 'e:no-total'), 'no total')"/>
//!        </xsl:template>"#,
//! )?;
//! let arena = xslt2::bumpalo::Bump::new();
//! let error = sheet
//!     .transformation(&arena, TransformOptions::default())
//!     .transform_source("urn:example:in", "<doc/>")
//!     .err()
//!     .expect("the stylesheet raises an error");
//! assert_eq!(error.origin(), ErrorOrigin::Raised);
//! assert_eq!(error.error_qname(), Some(("urn:example:errors", "no-total")));
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## Registering a collation
//!
//! A collation is identified by a URI (§13.1.3). The host registers its own
//! with [`XsltFunctions::register_collation`] before compiling; `xsl:sort`,
//! `xsl:key`, `xsl:for-each-group` and the XPath functions that take a
//! collation can then name it.
//!
//! ```
//! use std::cmp::Ordering;
//! use std::rc::Rc;
//! use xslt2::collation::Collation;
//! use xslt2::{Compiler, NameTable, TransformOptions, XsltFunctions};
//!
//! /// Orders strings by length, then by code points.
//! struct ByLength;
//!
//! impl Collation for ByLength {
//!     fn compare(&self, a: &str, b: &str) -> Ordering {
//!         a.chars().count().cmp(&b.chars().count()).then_with(|| a.cmp(b))
//!     }
//!     // A sort key lets keys, grouping and `=` hash instead of compare.
//!     fn sort_key(&self, s: &str) -> Option<Vec<u8>> {
//!         let mut key = (s.chars().count() as u64).to_be_bytes().to_vec();
//!         key.extend_from_slice(s.as_bytes());
//!         Some(key)
//!     }
//! }
//!
//! let mut functions = XsltFunctions::new();
//! functions.register_collation("urn:example:by-length", Rc::new(ByLength));
//!
//! let names = NameTable::new();
//! let sheet = Compiler::new(&names, &functions).compile_str(
//!     r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!          <xsl:output omit-xml-declaration="yes"/>
//!          <xsl:template match="/">
//!            <out>
//!              <xsl:for-each select="//w">
//!                <xsl:sort select="." collation="urn:example:by-length"/>
//!                <xsl:value-of select="."/><xsl:text> </xsl:text>
//!              </xsl:for-each>
//!            </out>
//!          </xsl:template>
//!        </xsl:stylesheet>"#,
//!     "urn:example:sheet",
//! )?;
//! let arena = xslt2::bumpalo::Bump::new();
//! let outcome = sheet
//!     .transformation(&arena, TransformOptions::default())
//!     .transform_source("urn:example:in", "<l><w>ccc</w><w>a</w><w>bb</w></l>")?;
//! assert_eq!(sheet.serialize_primary(&outcome)?, "<out>a bb ccc </out>");
//! # Ok::<(), xslt2::XsltError>(())
//! ```
//!
//! ## Running on a large stack
//!
//! Templates and stylesheet functions recurse on the native stack. A run
//! measures the stack it uses and stops with an error instead of overflowing
//! ([`TransformOptions::stack_limit`], [`TransformOptions::max_recursion_depth`]);
//! on a thread whose stack size it cannot know it allows itself
//! [`OTHER_THREAD_STACK_LIMIT`]. A deeply recursive job runs whole — compile,
//! transform, serialize — on a thread with a large stack: the compiled
//! stylesheet borrows the name table and the arena, which are not `Sync`, so
//! it is not `Send`; the closure owns them all and returns owned values.
//!
//! ```
//! use xslt2::{Compiler, NameTable, TransformOptions, XsltError, XsltFunctions};
//!
//! let text = xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || -> Result<String, XsltError> {
//!     let names = NameTable::new();
//!     let functions = XsltFunctions::new();
//!     let sheet = Compiler::new(&names, &functions).compile_str(
//!         r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!              <xsl:output omit-xml-declaration="yes"/>
//!              <xsl:template name="count">
//!                <xsl:param name="n" select="1000"/>
//!                <xsl:choose>
//!                  <xsl:when test="$n gt 0">
//!                    <xsl:call-template name="count"><xsl:with-param name="n" select="$n - 1"/></xsl:call-template>
//!                  </xsl:when>
//!                  <xsl:otherwise><done/></xsl:otherwise>
//!                </xsl:choose>
//!              </xsl:template>
//!            </xsl:stylesheet>"#,
//!         "urn:example:deep",
//!     )?;
//!     let arena = xslt2::bumpalo::Bump::new();
//!     let outcome = sheet
//!         .transformation(&arena, TransformOptions::default())
//!         .with_initial_template(xslt2::QName::local("count"))
//!         .transform_without_source()?;
//!     sheet.serialize_primary(&outcome)
//! })?;
//! assert_eq!(text, "<done/>");
//! # Ok::<(), XsltError>(())
//! ```
//!
//! # Cargo features
//!
//! - **`icu-collation`** (off by default) adds the UCA collations of F&O 3.1
//!   §5.3.3 (`http://www.w3.org/2013/collation/UCA?…`) and locale-tailored
//!   `xsl:sort lang` / `case-order`, from ICU4X 1.5. Without it the collations
//!   are the Unicode codepoint collation, the HTML ASCII case-insensitive
//!   collation and those the host registers, and a UCA collation URI is the
//!   error `XSLT2-NOTIMPL`. The feature changes existing results, not only
//!   adds new ones: `xsl:sort lang="…"` sorts by the language's tailoring
//!   instead of the built-in case-order collation (see the [`collation`]
//!   module).
//!
//! # Limits
//!
//! In brief (the README's [Known limits](https://github.com/semyonc/xslt2#known-limits)
//! and [Conformance](https://github.com/semyonc/xslt2#conformance) sections say
//! more):
//!
//! - result trees stay in memory, and each one costs about 118 KB of the run's
//!   arena however small it is;
//! - no extension instructions; `xsl:number` and the date formatting functions
//!   write English, and the dates are Gregorian;
//! - the output encodings are UTF-8, UTF-16, US-ASCII and ISO-8859-1;
//! - deep recursion needs a large stack (above).

#![warn(missing_docs)]

pub mod collation;
mod compile;
pub mod error;
mod format_date;
mod format_number;
mod functions;
mod model;
mod names;
mod numbering;
pub mod output;
mod pattern;
mod receiver;
mod regex;
pub mod resolver;
mod runtime;
mod schema;
mod serialize;
mod store;
mod stylesheet;
mod whitespace;

/// The arena a transformation builds its documents in ([`bumpalo::Bump`]).
pub use bumpalo;
/// The XPath 2.0 engine, the data model and XML Schema validation this crate
/// is built on.
pub use xsd_schema;

pub use compile::Compiler;
pub use error::{ErrorKind, ErrorOrigin, Location, XsltError};
pub use functions::XsltFunctions;
pub use model::{CompiledStylesheet, ModeKey};
pub use names::QName;
pub use output::{OutputDefinition, OutputMethod};
pub use receiver::Nav;
pub use resolver::{FileResolver, MemoryResolver, Resolver};
pub use runtime::{
    run_on_large_stack, Message, Outcome, SecondaryResult, SourceValidation, TransformOptions,
    Transformation, DEFAULT_MAX_RECURSION_DEPTH, DEFAULT_STACK_SIZE, OTHER_THREAD_STACK_LIMIT,
};
pub use schema::{ImportSource, SchemaContext, SchemaContextBuilder, SchemaImport};
pub use store::{DocId, DocumentStore};
/// The table of interned names a compilation and its transformations share
/// (a [`SchemaContext`] has its own).
pub use xsd_schema::namespace::NameTable;
/// A timezone offset, in minutes from UTC ([`TransformOptions::implicit_timezone`]).
pub use xsd_schema::types::TimezoneOffset;
/// A value of the XPath data model: a stylesheet parameter's value
/// ([`Transformation::with_param`]); its nodes are [`Nav`]s.
pub use xsd_schema::xpath::functions::XPathValue;
/// The version of XML Schema a [`SchemaContext`] implements ([`SchemaContextBuilder::xsd_version`]).
pub use xsd_schema::XsdVersion;
