# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-31

First release: an XSLT 2.0 processor written in Rust, basic and schema-aware
(XSLT 2.0 §21). It compiles stylesheets, runs them over XML documents and
serializes the results. It is built on
[xsd-schema](https://github.com/semyonc/xsd-schema) 0.3, which supplies the
XPath 2.0 engine, the data model, XML Schema 1.0 and 1.1 validation and the
in-memory document.

### XSLT 2.0

- **Stylesheets**: the module graph of `xsl:include` and `xsl:import` with
  import precedence, `use-when`, simplified stylesheet modules, the static
  checks of every XSLT 2.0 element, and the error codes of the specification.
- **Templates**: template rules with the §6.4 default priorities and conflict
  resolution, the built-in rules, modes, `xsl:apply-imports`,
  `xsl:next-match`, named templates, variables and parameters, tunnel
  parameters included.
- **Instructions**: the node constructors, literal result elements, attribute
  sets, attribute value templates, `xsl:copy` and `xsl:copy-of`,
  `xsl:message`, `xsl:sort` and `xsl:perform-sort` with collations,
  `xsl:function`, `xsl:key` with `key()` and the `id()` / `key()` patterns,
  `xsl:for-each-group`, `xsl:analyze-string` with `regex-group()`,
  `xsl:result-document`, and `xsl:number` (every level, the decimal-digit
  families of Unicode, seventeen sequences of consecutive Unicode characters,
  English words and ordinals).
- **Functions**: the XSLT 2.0 function library of §16 on top of the XPath 2.0
  functions, including `format-number` with `xsl:decimal-format`, and
  `format-date`, `format-time` and `format-dateTime` (English, in the Gregorian
  `AD` and `ISO` calendars).
- **Backwards compatibility** (§3.8): a `version="1.0"` stylesheet, module or
  element runs with XPath 1.0 compatibility mode, first-item attribute value
  templates and `xsl:value-of`, and the 1.0 rules for `xsl:number`,
  `xsl:sort` and keys.
- **Forwards compatibility** (§3.9): in a module or element whose version is
  above 2.0, XSLT elements that 2.0 does not allow at the top level are ignored
  with their content, unknown attributes are ignored, an unknown instruction is
  replaced by its `xsl:fallback` children, and an XPath static error is
  reported only if the expression is evaluated.

### Schema-aware processing

- `xsl:import-schema` by location, by namespace or inline, together with the
  host's own schemas, in one caller-owned `SchemaContext`; the schemas of the
  XML and XLink namespaces are built in.
- Imported types in every XPath expression and `as` attribute, and the
  constructor functions of user-defined atomic types.
- Typed source documents (`TransformOptions::source_validation`: skip,
  strict or lax), parsed and validated in one pass, and
  `input-type-annotations`.
- Validation of what the stylesheet constructs: `[xsl:]validation` and
  `[xsl:]type` on literal result elements, `xsl:element`, `xsl:attribute`,
  `xsl:copy`, `xsl:copy-of`, `xsl:document` and `xsl:result-document`,
  `default-validation`, and document-level validation (§19.2).
- Typed patterns: `element(*, T)`, `element(N, T)`, `attribute(…)`,
  `schema-element(N)` with its substitution group, `schema-attribute(N)` and
  `document-node(…)`.
- XML Schema 1.0 by default, as XSLT 2.0 specifies, or XML Schema 1.1
  (`SchemaContextBuilder::xsd_version`).

### Output

- The `xml`, `xhtml`, `html` and `text` output methods, `xsl:output` merged by
  import precedence, character maps and `disable-output-escaping`, in UTF-8,
  UTF-16, US-ASCII and ISO-8859-1.
- Results stay in memory: the primary result and every `xsl:result-document`
  come back in the `Outcome`, and nothing is written to disk.

### API

`Compiler` compiles a stylesheet (`Compiler::new` for a basic processor,
`Compiler::schema_aware` for a schema-aware one) into a `CompiledStylesheet`,
which runs any number of `Transformation`s, each set up by `TransformOptions`,
parameters, an initial template or mode, a resolver and a message sink, and
started from a string, a URI, a document already loaded, or no source at all.
The `Outcome` holds the primary result, the secondary results and the
messages; `CompiledStylesheet::serialize_primary` and `serialize_secondary`
(and their `_to_bytes` forms) serialize them. Stylesheet modules and documents
are read through a `Resolver` (`FileResolver`, the default, or
`MemoryResolver`), errors are `XsltError` with the specification's codes, and
hosts can register collations. `run_on_large_stack` runs a deeply recursive job
on a thread with a large stack. The crate re-exports `xsd_schema` and
`bumpalo`, so it is the only dependency a program needs. See the
[API documentation](https://docs.rs/xslt2).

- A schema-aware compiler reads through the resolver its `SchemaContext` was
  built with, and `MemoryResolver`'s `Debug` output lists its URIs, not their
  texts.
- Deep nesting ends in an error, not in a stack overflow: a run checks its
  stack limit at every instruction as well as at every template and function,
  and a compilation checks the same limit at every instruction and every
  included or imported module, so a deeply nested stylesheet ends in an
  `XSLT2-INTERNAL` error the host can handle. Copying,
  whitespace-stripping and serializing a document need no more stack for a
  deep document than for a shallow one, and compiling nested instructions
  takes time linear in their depth.

### Cargo features and Rust version

- `icu-collation` (off by default) adds the UCA collations
  (`http://www.w3.org/2013/collation/UCA?…`) and locale-tailored
  `xsl:sort lang` / `case-order` from ICU4X 1.5; enabling it changes the
  results of `xsl:sort lang` and `case-order`.
- The minimum supported Rust version is 1.85.

### Conformance

On the cases of the W3C XSLT 3.0 test suite that apply to an XSLT 2.0
processor, the basic processor passes 5,762 of 5,792 runnable cases and the
schema-aware processor 6,313 of 6,343 (the figures are in the
[README](https://github.com/semyonc/xslt2/blob/main/README.md#conformance)).
[Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md)
explains every case that does not pass and every exclusion, and
[Limits and untrusted input](https://github.com/semyonc/xslt2/blob/main/doc/LIMITS.md)
what a transformation can read and which resources a host has to bound.

Known issue: `unparsed-text()` ignores its `$encoding` argument and decodes a
resource without a byte order mark or an XML declaration as UTF-8
([Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md#known-bugs-no-test-case-exercises)).

[Unreleased]: https://github.com/semyonc/xslt2/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/semyonc/xslt2/releases/tag/v0.1.0
