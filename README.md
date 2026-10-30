# xslt2

`xslt2` is an XSLT 2.0 processor written in Rust, basic and schema-aware (XSLT 2.0 §21). It compiles stylesheets,
runs them over XML documents, and serializes the results with the `xml`, `xhtml`, `html` and `text` output methods.
It is built on [**xsd-schema**](https://github.com/semyonc/xsd-schema), an XML Schema 1.0 / 1.1 validator with an
XPath 2.0 engine, which supplies the XPath evaluation, the data model and the validation. xsd-schema passes 39,461 of
the 39,510 W3C XSD 1.0 tests, 2,313 of 2,319 XSD 1.1 tests and all 8,047 XQTS XPath 2.0 tests.

## Install

```toml
[dependencies]
xslt2 = "0.1"
```

`xslt2` re-exports the crates its API uses — [`xsd-schema`](https://crates.io/crates/xsd-schema) as
`xslt2::xsd_schema` and `bumpalo` as `xslt2::bumpalo` — so it is the only dependency you need, and the versions always
match. The minimum supported Rust version is 1.85. The API documentation, with more examples, is on
[docs.rs](https://docs.rs/xslt2).

## Documentation

- [API documentation](https://docs.rs/xslt2) on docs.rs, with examples
- [Changelog](https://github.com/semyonc/xslt2/blob/main/CHANGELOG.md)
- [Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md): every W3C test case that does not
  pass, and every excluded one, with the reason
- [Limits and untrusted input](https://github.com/semyonc/xslt2/blob/main/doc/LIMITS.md): what a transformation can
  read under each resolver, the resource limits, and how to run stylesheets you do not trust

## Quick start

A stylesheet and a source document given as strings:

```rust
use xslt2::{Compiler, NameTable, TransformOptions, XsltFunctions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let sheet = Compiler::new(&names, &functions).compile_str(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
             <xsl:template match="/">
               <total><xsl:value-of select="sum(//item/@price)"/></total>
             </xsl:template>
           </xsl:stylesheet>"#,
        "urn:example:sheet", // the stylesheet's base URI
    )?;

    let arena = xslt2::bumpalo::Bump::new();
    let run = sheet.transformation(&arena, TransformOptions::default());
    let outcome = run.transform_source(
        "urn:example:order",
        r#"<order><item price="2.5"/><item price="4"/></order>"#,
    )?;
    assert_eq!(
        sheet.serialize_primary(&outcome)?,
        r#"<?xml version="1.0" encoding="UTF-8"?><total>6.5</total>"#
    );
    Ok(())
}
```

Files, and the extra result documents a stylesheet writes with `xsl:result-document`:

```rust
use std::path::Path;
use xslt2::{Compiler, NameTable, TransformOptions, XsltFunctions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let sheet = Compiler::new(&names, &functions).compile_file(Path::new("report.xsl"))?;

    let arena = xslt2::bumpalo::Bump::new();
    let options = TransformOptions {
        // The base output URI: `xsl:result-document href="…"` resolves against it.
        base_output_uri: Some("file:///data/out/".into()),
        ..Default::default()
    };
    let run = sheet.transformation(&arena, options);
    let outcome = run.transform_uri("file:///data/in.xml")?;
    println!("{}", sheet.serialize_primary(&outcome)?);
    for result in &outcome.secondary {
        // Every other result tree, in the order the instructions ran, under its absolute URI.
        println!("{}: {}", result.uri, sheet.serialize_secondary(result)?);
    }
    Ok(())
}
```

Nothing is written to disk: the transformation returns its result trees, and the caller serializes and stores them
(`serialize_primary_to_bytes` and `serialize_secondary_to_bytes` apply the output encoding). The messages of
`xsl:message` are in `outcome.messages`.

## Examples

```bash
cargo run --example transform -- sheet.xsl input.xml [name=value ...]   # the primary result to stdout
cargo run --example schema_aware -- sheet.xsl input.xml [schema.xsd ...] [--xsd11]
cargo run --example in_memory                                          # no files: a MemoryResolver
```

## What is supported

- **Stylesheets**: the module graph of `xsl:include` and `xsl:import` with import precedence, `use-when`, simplified
  stylesheet modules, the static checks of every XSLT 2.0 element, and the error codes of the specification.
- **Templates**: template rules with the §6.4 default priorities and conflict resolution, the built-in rules, modes,
  `xsl:apply-imports`, `xsl:next-match`, named templates, variables and parameters (tunnel parameters included).
- **Instructions**: the node constructors, literal result elements, attribute sets, attribute value templates,
  `xsl:copy` / `xsl:copy-of`, `xsl:message`, `xsl:sort` / `xsl:perform-sort` with collations, `xsl:function`,
  `xsl:key` with `key()` and the `id()` / `key()` patterns, `xsl:for-each-group`, `xsl:analyze-string` with
  `regex-group()`, `xsl:result-document`, and `xsl:number` (every level, the decimal-digit families of Unicode,
  seventeen sequences of consecutive Unicode characters such as circled and parenthesized digits, English words and
  ordinals).
- **Functions**: the XSLT 2.0 function library of §16 on top of the XPath 2.0 functions, including `format-number`
  with `xsl:decimal-format` and `format-date`, `format-time` and `format-dateTime`.
- **Output**: the `xml`, `xhtml`, `html` and `text` methods with character maps and `disable-output-escaping`, in
  UTF-8, UTF-16, US-ASCII and ISO-8859-1.
- **Backwards compatibility** (§3.8): a `version="1.0"` stylesheet, module or element runs with XPath 1.0
  compatibility mode, first-item attribute value templates and `xsl:value-of`, and the 1.0 rules for `xsl:number`,
  `xsl:sort` and keys; when the principal module says `version="1.0"`, an XHTML result created implicitly is written
  with the `xml` method (§20).
- **Forwards compatibility** (§3.9): in a module or element whose version is above 2.0, XSLT elements that 2.0 does
  not allow at the top level are ignored with their content, unknown attributes are ignored, an unknown instruction
  is replaced by its `xsl:fallback` children (or is the static error without one), and an XPath static error is
  reported only if the expression is evaluated — except a call to a function that is not available, and errors in
  patterns.
- **Schema-aware processing** (§21.2): the next section.

## Schema-aware mode

`Compiler::new` is a basic processor (§21.1): `xsl:import-schema` is XTSE1650, `[xsl:]type` and a `[xsl:]validation`
or `default-validation` other than `strip` are XTSE1660. A schema-aware compilation is two steps: a caller-owned
`SchemaContext`, built from a pre-scan of the stylesheet's `xsl:import-schema` declarations (by location, by namespace
only, or inline; the schemas of the XML and XLink namespaces are built in) together with any schemas of the host's
own, and `Compiler::schema_aware`, which compiles against that context's one schema set:

```rust
use std::path::Path;
use xslt2::{Compiler, SchemaContext, SourceValidation, TransformOptions, XsltFunctions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let functions = XsltFunctions::new();
    let context = SchemaContext::builder()
        .stylesheet_file(Path::new("report.xsl"), &functions)? // its xsl:import-schema declarations
        .schema_file(Path::new("orders.xsd"))                   // a schema of the host's own
        .build()?;                                              // XTSE0220 for an invalid schema
    let sheet = Compiler::schema_aware(&context, &functions).compile_file(Path::new("report.xsl"))?;

    let arena = xslt2::bumpalo::Bump::new();
    let options = TransformOptions {
        source_validation: SourceValidation::Strict, // also for doc() and document()
        ..Default::default()
    };
    let run = sheet.transformation(&arena, options);
    let outcome = run.transform_uri("file:///data/orders.xml")?;
    println!("{}", sheet.serialize_primary(&outcome)?);
    Ok(())
}
```

- **Types in expressions**: the imported types are in scope for every XPath expression and `as` attribute (`cast as`,
  `castable as`, `instance of`, the constructor functions of user-defined atomic types), and `type-available()` and
  `system-property('xsl:is-schema-aware')` answer as a schema-aware processor's.
- **Typed source documents**: `TransformOptions::source_validation` (`Skip`, the default, `Strict` or `Lax`) applies to
  every source-tree loader, and `transform_source_with` or `set_source_validation` sets it per document. A validated
  document is parsed and typed in one pass; its nodes atomize to typed values (keys, sort keys and grouping keys
  compare typed values), whitespace in element-only content is not in the tree, defaulted attributes and element
  default values are present (XDM 1.0 §6.2.4), `id()` and `idref()` find typed IDs and IDREFs, and an invalid document
  is an error naming the offending nodes. `input-type-annotations="strip"` strips every source tree (§4.3).
- **Validating what the stylesheet constructs**: `[xsl:]validation` (`strict`, `lax`, `preserve`, `strip`) and
  `[xsl:]type` on literal result elements, `xsl:element`, `xsl:attribute`, `xsl:copy`, `xsl:copy-of`, `xsl:document`
  and `xsl:result-document`, and `default-validation` (§19.2). Elements are validated as the tree is built, nested
  validations innermost first; a document node is validated as a document (§19.2.2: one element child, and ID / IDREF
  checks). The errors are XTTE1510, XTTE1512, XTTE1515, XTTE1540, XTTE1545, XTTE1550 and XTTE1555.
- **Typed patterns**: `element(*, T)`, `element(N, T)`, `element(N, T?)`, `attribute(*, T)`, `attribute(N, T)`,
  `schema-element(N)` (with its substitution group), `schema-attribute(N)` and `document-node(…)` in every pattern,
  with the §6.4 priorities; a node matches exactly when `instance of` the same test is true.
- **XML Schema 1.1**: a context is XML Schema 1.0, the version XSLT 2.0 refers to, unless built with
  `SchemaContextBuilder::xsd_version(XsdVersion::V1_1)`. Then every schema document of the context is read as XML
  Schema 1.1 (`xs:assert`, `xs:alternative`, open content …), validation evaluates assertions, and
  `xs:dateTimeStamp` is in scope.

**Implementation-defined choices** (§3.13 allows "mechanisms that allow additional schema components to be added to
the in-scope schema components"): the host's schemas join the stylesheet's imports in the one schema set, and the set
may be XML Schema 1.1. Every component of the set is in scope, even when no `xsl:import-schema` names its namespace. A
`schema-location` whose document has another target namespace than the declaration is XTSE0220, unless the set
already has a schema document of that namespace, in which case the location is passed over as a hint. Every location
passed over, and every location that cannot be read, is reported in `SchemaContext::warnings()`.

## Cargo features

`icu-collation` (off by default) adds ICU4X 1.5's `icu_collator` with its compiled CLDR data, and with it the UCA
collations of F&O 3.1 §5.3.3 (`http://www.w3.org/2013/collation/UCA?lang=…;strength=…` and the other parameters
ICU4X 1.5 supports: all but `reorder`; `-u-` collation keywords in `lang` too) and locale-tailored `xsl:sort lang` /
`case-order`. A parameter a UCA URI leaves out takes the language's CLDR default (`UCA?lang=th` ignores spaces and
punctuation, `UCA?lang=fr-CA` compares accents backwards, `UCA?lang=da` sorts upper case first). Under a UCA collation
`contains()` and the other substring functions match collation-equal substrings cut at character boundaries, and give
up with FOCH0004 after a work budget of about a second on pathological input.

**Enabling the feature changes existing results**, not only adds new ones: `xsl:sort lang="…"` or `case-order`
without a `collation` attribute then sorts with the language's UCA tailoring instead of the built-in case-order
collation — `('f', 'é', 'e')` with `lang="en"` sorts `e, f, é` without the feature and `e, é, f` with it. Cargo
unifies features across a build, so if any crate in a build enables `icu-collation` on `xslt2`, every user of
`xslt2` in that build gets it.

## Known limits

- **Results stay in memory.** Secondary result documents are returned in `Outcome::secondary` and nothing is written
  to disk. Every result tree and every temporary tree costs about 118 KB of the transformation's arena however small
  it is, kept until the arena is dropped, so tens of thousands of result documents need gigabytes.
- **No extension instructions.** An extension instruction with `xsl:fallback` children evaluates them; one without is
  XTDE1450 when, and only when, it is evaluated; `element-available()` answers false for it.
- **English and Gregorian formatting.** `xsl:number` writes words and ordinals in English whatever `lang` says (the
  §12.3 fallback). `format-date`, `format-time` and `format-dateTime` write English names in the Gregorian `AD` and
  `ISO` calendars; any other language or calendar falls back to these with a `[Language: en]` or `[Calendar: AD]`
  prefix, as §16.5.2 requires.
- **Collations without `icu-collation`.** The built-in collations are the Unicode codepoint collation and HTML ASCII
  case-insensitive, plus any the host registers; a UCA collation URI is the error `XSLT2-NOTIMPL`, and
  `xsl:sort lang` / `case-order` use a built-in case-order collation.
- **Output encodings.** UTF-8, UTF-16, US-ASCII and ISO-8859-1; any other is SESU0007.
- **Deep recursion.** Templates and stylesheet functions recurse on the native stack. A run measures the stack it
  uses and stops with an error instead of overflowing; on a thread whose stack size it cannot know it allows itself
  1 MiB (`OTHER_THREAD_STACK_LIMIT`), about a hundred nested template calls in a release build. Run a deeply
  recursive job whole (compile, transform, serialize) inside
  `xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || …)` (256 MiB), or set `TransformOptions::stack_limit` for a
  thread whose stack you know.
- **The `unicode-90` test-set** is left out of the figures below: it checks regular-expression character classes
  against every code point and runs for about 25 minutes, and its cases compare Unicode 9.0 data with the regex
  engine's Unicode 15.1 tables, the same version difference that excludes the `regex-classes` cases.

## Conformance

The XSLT 2.0 test suite was never published. The cases of the public
[W3C XSLT 3.0 test suite](https://github.com/w3c/xslt30-test) that apply to an XSLT 2.0 processor (`spec` dependency
`XSLT10+`, `XSLT20`, `XSLT20+`, `XSLT10 XSLT20`, or none) are the measure. Suite commit `fddf1cf`, the `unicode-90`
test-set left out (`--skip unicode-90`):

| | basic | basic, `icu-collation` | schema-aware | schema-aware, XML Schema 1.1 |
|---|---:|---:|---:|---:|
| XSLT 2.0-applicable | 6,619 | 6,619 | 6,619 | 6,619 |
| excluded (documented) | 64 | 64 | 76 | 76 |
| not run (a dependency not satisfied) | 763 | 763 | 200 | 201 |
| runnable | 5,792 | 5,792 | 6,343 | 6,342 |
| pass | **5,762** | **5,764** | **6,313** | **6,312** |
| fail | 25 | 25 | 25 | 25 |
| wrongError | 0 | 0 | 0 | 0 |
| harnessError | 3 | 3 | 3 | 3 |
| notImplemented (`XSLT2-NOTIMPL`) | 2 | 0 | 2 | 2 |
| pass rate (pass / runnable) | 99.48% | 99.52% | 99.53% | 99.53% |
| headline (basic profile, non-regex) | 4,287 / 4,313 = 99.40% | 4,289 / 4,313 = 99.44% | 4,271 / 4,297 = 99.39% | 4,271 / 4,297 = 99.39% |
| cases that need a schema-aware processor | — | — | 567 / 567 | 569 / 569 |

- **Headline**: the runnable cases that need no optional feature (no schema-awareness, backwards compatibility or
  serialization), outside the regular-expression and Unicode test-sets (`regex`, `regex-classes`, `regex-syntax`,
  `regex-syntax-xslt20`), which exercise the regex engine more than XSLT.
- The 25 failures and the 3 harness errors are the same cases in all four runs.
  [Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md) gives the reason for each: 8 are a
  known bug (zero-length text nodes in a typed variable or a function result are lost), 14 need DTD processing that
  their catalog entries do not declare, 1 needs `collection()`, 1 German number words, 1 a resource on the web, and
  the 3 harness errors start the transformation at a node inside the source document, where the public API starts
  only at a document node. The 2 notImplemented cases use a UCA collation, which needs `icu-collation`.
- **Excluded**: cases whose expected result is wrong for a conformant XSLT 2.0 processor (XSLT 3.0 syntax or
  behaviour in a 2.0 stylesheet, results that §4.2, §4.4, §5.7.3 or §6.6 rule out), or pins a choice the specification
  leaves to the implementation, including 32 regular-expression cases that pin an older Unicode version. Every entry
  of `EXCLUDED` in [`tests/xslts/filter.rs`](https://github.com/semyonc/xslt2/blob/main/tests/xslts/filter.rs) quotes the text that makes it so; the schema-aware runs add 12 schema-aware
  cases. [Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md#excluded-cases) groups them.
- **Not run** in the basic run: 584 cases that need a schema-aware processor, 61 that need DTD processing, 44 that
  need XML 1.1, 18 that need the `xsl-stylesheet` processing instruction, 12 that expect a recoverable error to be
  signalled (`on-multiple-match=error`; this processor recovers by default), and 44 whose other dependencies (optional
  features, implementation-defined choices) this processor does not match.
- `assert-xml` is compared as the catalog defines it, by Canonical-XML equivalence: prefixes and in-scope namespaces
  are significant unless the assertion says `ignore-prefixes="true"`. The harness pins the implicit timezone to UTC,
  the catalog's assumption.

## Running the conformance harness

```bash
git clone https://github.com/w3c/xslt30-test ../../xslt30-test    # where the harness looks; -s PATH for elsewhere
cargo test --release --test xslts -- --skip unicode-90                      # basic
cargo test --release --features icu-collation --test xslts -- --skip unicode-90
cargo test --release --test xslts -- --skip unicode-90 --schema-aware       # schema-aware
cargo test --release --test xslts -- --skip unicode-90 --schema-aware --xsd11
cargo test --release --test xslts -- --skip unicode-90 --report new.xml      # W3C results format
python3 tools/suite_diff.py old.xml new.xml                                  # per-case differences
cargo test --release --test xslts -- --help                                  # every option
```

Always build the harness with `--release` (a debug build skips it) and pass `--skip unicode-90` (see Known limits).
The suite path is relative to this crate's root: `../../xslt30-test`. `--schema-aware` runs the cases that need a
schema-aware processor and compiles every case with one; `--xsd11` adds the cases that need XML Schema 1.1 and builds
every schema context as 1.1. `tools/suite_diff.py` lists every case whose status or failure message differs between two
reports.

## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](https://github.com/semyonc/xslt2/blob/main/LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](https://github.com/semyonc/xslt2/blob/main/LICENSE-MIT)), at
your option (`MIT OR Apache-2.0`).
