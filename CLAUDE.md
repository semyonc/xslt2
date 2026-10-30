# CLAUDE.md — xslt2

Notes for coding agents (and people) working on this repository.

## What this crate is

`xslt2` is a schema-aware XSLT 2.0 processor in Rust. It is built on
[`xsd-schema`](https://github.com/semyonc/xsd-schema) ([crates.io](https://crates.io/crates/xsd-schema)), which supplies
the XPath 2.0 engine, the XDM values and types, XML Schema 1.0 / 1.1 validation with the PSVI, and the document
model (`BufferDocument`). This crate adds XSLT: loading and compiling stylesheet modules, patterns and template
dispatch, the instructions, serialization, the XSLT functions and the formatting functions, and the schema-aware
mode (§21.2).

## Source of truth

The W3C Recommendations are the specification. Cite sections as §N.N and quote the exact wording instead of relying
on memory. A local copy of a Recommendation may be used.

- XSLT 2.0 (Second Edition): https://www.w3.org/TR/2021/REC-xslt20-20210330/ — `§N.N` without a prefix in this
  repository means this document.
- XPath 2.0 (Second Edition): https://www.w3.org/TR/2010/REC-xpath20-20101214/
- XQuery 1.0 and XPath 2.0 Functions and Operators (Second Edition): https://www.w3.org/TR/2010/REC-xpath-functions-20101214/
- XQuery 1.0 and XPath 2.0 Data Model (Second Edition): https://www.w3.org/TR/2010/REC-xpath-datamodel-20101214/
- XSLT 2.0 and XQuery 1.0 Serialization (Second Edition): https://www.w3.org/TR/2010/REC-xslt-xquery-serialization-20101214/

## Build and test

The minimum supported Rust version is 1.85 (`rust-version` in `Cargo.toml`); do not use newer standard-library APIs.

```bash
cargo build
cargo clippy --all-targets                    # 0 warnings expected
cargo test                                    # unit and integration tests (the W3C harness skips itself in a debug build)
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
```

The `icu-collation` feature (off by default) is checked in both configurations:

```bash
cargo clippy --all-targets --features icu-collation
cargo test --features icu-collation
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --features icu-collation
cargo tree --features icu-collation -d        # no second `regexml` or `icu_*` version
```

The feature adds ICU4X 1.5's `icu_collator`: the UCA collation URIs of F&O 3.1 §5.3.3 and locale-tailored
`xsl:sort lang` / `case-order`. It must stay on the ICU4X 1.5 line that `regexml` already brings in. **It is not
additive**: it changes the results of `xsl:sort lang` / `case-order` (UCA tailoring instead of the built-in case-order
collation), and Cargo's feature unification turns it on for every user of the crate in a build once any dependent
enables it. Some tests run only with the feature, others only without it.

**Never run a bare `cargo test --release`**: in a release build the conformance harness (below) runs the whole suite,
the `unicode-90` test-set included, which takes about 25 minutes on its own. Run the harness target with
`--skip unicode-90` instead.

### Building against a local `xsd-schema` checkout

`xslt2` depends on `xsd-schema` from crates.io. To build against a local checkout (to test an upstream change), add an
**untracked** `.cargo/config.toml` (the `.cargo/` directory is git-ignored):

```toml
[patch.crates-io]
xsd-schema = { path = "/path/to/xsd-schema" }   # your checkout
```

The checkout's version must satisfy the requirement in `Cargo.toml` (`0.3`): otherwise Cargo uses the crates.io release
and only warns that the patch "was not used in the crate graph". Check with `cargo tree -i xsd-schema`, which shows
the path of a patched crate. The patch rewrites `Cargo.lock`; restore it (`git checkout Cargo.lock`) before
committing, and never commit the patch.
`regexml` (the crates.io package `regexml-fork`, renamed in `Cargo.toml`) is required with the same version as
`xsd-schema` requires it, so the graph holds one regex engine.

## W3C conformance harness (`tests/xslts`)

The XSLT 2.0 test suite was never published. The harness runs the cases of the public W3C XSLT 3.0 test suite that
apply to an XSLT 2.0 processor: those whose `spec` dependency is `XSLT10+`, `XSLT20`, `XSLT20+` or `XSLT10 XSLT20`, or
that have none (`tests/xslts/filter.rs`, `APPLICABLE_SPECS`).

Get the suite from https://github.com/w3c/xslt30-test. The harness looks for it at `../../xslt30-test` (two levels
above the crate root); pass `-s PATH` for another place. Without a suite the target prints where it looked and passes.

**Always run it in `--release`**: a debug build is about twenty times slower and its stack frames are large enough to
exhaust the worker stack, so in a debug build the harness skips itself unless `--debug` is given. Always pass
`--skip unicode-90`: that test-set (1,460 cases, 16 of them excluded) checks regular-expression character classes
against every code point and takes about 25 minutes, and its cases compare Unicode 9.0 data with the regex engine's
newer tables.

```bash
cargo test --release --test xslts -- --skip unicode-90                      # basic processor
cargo test --release --test xslts -- --skip unicode-90 --schema-aware       # schema-aware processor
cargo test --release --test xslts -- --skip unicode-90 --schema-aware --xsd11
cargo test --release --features icu-collation --test xslts -- --skip unicode-90
cargo test --release --test xslts -- -g value-of -f    # test-sets whose name contains value-of, failures only
cargo test --release --test xslts -- -t lre-027 -v     # one case, by name
cargo test --release --test xslts -- -l                # list test-sets with applicable / runnable counts
cargo test --release --test xslts -- --self-test       # the harness's own checks
cargo test --release --test xslts -- --help            # every option
```

The harness refuses an argument it does not know (exit status 2, nothing run): a mistyped option, or several options
passed as one argument, would otherwise run the whole suite, `unicode-90` included. It accepts and ignores the options
of the standard test runner that `cargo test -- …` forwards to every target (`--nocapture`, `--test-threads`, …), and
a skipped debug build ignores everything, because `cargo test NAME` forwards its filter to every target.

- `--schema-aware`: the `schema_aware` feature is satisfied, so the 584 cases that need a schema-aware processor run
  and the 16 that need a basic one do not. Every case is compiled with a schema-aware processor
  (`SchemaContext::builder()…stylesheet_file(…)…build()`, then `Compiler::schema_aware`): every basic case must keep
  the status it has in the basic run. The environment's schemas with the role `source-reference` or
  `stylesheet-import`, or with no role, join the context as host schemas (a `secondary` one is reached through the
  directives of the schema that includes it), and each source is validated as its `validation` attribute says; with
  none, `strict` when the environment has a `source-reference` schema, `skip` otherwise.
- `--xsd11` (with `--schema-aware`): the `XSD_1.1` cases run, and every schema context is XML Schema 1.1.
- `--report FILE` writes the results in the W3C results format
  (`http://www.w3.org/2012/11/xslt30-test-results`). `python3 tools/suite_diff.py OLD.xml NEW.xml` compares two
  reports case by case, status and failure message; use it to show that a change turns no passing case into a
  non-passing one. Compare each configuration with a report of the same configuration.
- Save verbose output to a file and grep it rather than re-running:
  `cargo test --release --test xslts -- --skip unicode-90 -v > /tmp/xslts.txt 2>&1`, then
  `grep -E "^  (FAIL|WRONG-ERROR|HARNESS-ERROR|NOT-IMPL)" /tmp/xslts.txt`.
- `--host-timezone`: by default the harness pins the implicit timezone to UTC, the catalog's assumption
  (`TransformOptions::implicit_timezone` for every case, and `TZ=UTC` for the process, which covers XPath evaluated
  outside a transformation); this option keeps the host's.
- `--lenient-xml` compares `assert-xml` ignoring prefixes and namespace declarations, for triage only; the default is
  the catalog's Canonical-XML equivalence.

How the harness decides (read the module docs before changing it):
- **Applicability and exclusions**: `tests/xslts/filter.rs`. `dependency_supported` is the one table of what the
  processor claims; `EXCLUDED` lists the cases whose expected result is wrong for a conformant XSLT 2.0 processor, or
  pins a choice the specification leaves open, and every entry quotes the text that makes it so. Exclusions are
  decided after dependencies, so an excluded case the configuration cannot run is reported as not run.
  `combinations_for_numbering` is answered by the processor itself (`engine::numbering_probe` runs `xsl:number`
  through the public API).
- **The adapter**: `tests/xslts/engine.rs` uses only the public API.
- **Assertions**: `tests/xslts/compare.rs`. Every result tree is kept twice: serialized with the stylesheet's own
  `xsl:output` (read only by `serialization-matches` and `assert-serialization`) and serialized with the catalog's
  default parameters and re-parsed (read by every assertion about the tree). An `assert` over a result tree that has
  type annotations is evaluated on the tree itself.
- **Statuses**: pass, fail, wrongError, harnessError (the harness cannot run or judge the case), notImplemented (the
  processor raised `XSLT2-NOTIMPL`: a missing feature, not a wrong answer), notRun (a dependency is not satisfied),
  excluded.

## Module map

Public modules are marked **pub**; every other module is private to the crate, and the items the API needs from
it are re-exported at the crate root.

| Path | What it holds |
|---|---|
| `src/lib.rs` | The façade: the module declarations, the re-exports, and the crate documentation with its examples |
| `src/error.rs`, `src/error/codes.rs` (**pub** `error`) | `XsltError`, `ErrorKind`, `ErrorOrigin`, `Location`; the Appendix E error codes (`error::codes`); `error::codes_internal` |
| `src/names.rs` | `QName`, the XSLT namespace, the `yes` / `no` attribute spellings of §3.5 |
| `src/whitespace.rs` | XML whitespace (§4): the only four characters XSLT calls whitespace |
| `src/resolver.rs` (**pub**) | `Resolver`, `FileResolver`, `MemoryResolver`; URI resolution (RFC 3986), `path_to_uri` / `uri_to_path`, and decoding of non-UTF-8 resources |
| `src/stylesheet.rs` | Loading modules (§3.10), import precedence, `use-when` (§3.12), stylesheet whitespace (§4.2), lexical static contexts (§5.4.1) |
| `src/compile/` | `Compiler`: modules to declarations and instructions; `checks.rs` is the static-checking table, one row per XSLT element; one file per declaration or instruction family |
| `src/model.rs`, `src/model/tests.rs` | `CompiledStylesheet`, `ModeKey`: the instruction tree, the declaration tables, serialization of results |
| `src/pattern.rs` | Patterns (§5.5): the grammar, the default priorities of §6.4, the dispatch-index key |
| `src/runtime/` | `Transformation`, `TransformOptions`, `Outcome`: the dynamic context (§5.4), template dispatch (§6), instruction evaluation, variables, sorting, keys, grouping, `xsl:analyze-string`, `xsl:number`, result documents, loading and validating sources, validating constructed nodes |
| `src/receiver.rs`, `src/receiver/tests/` | The push pipeline: `Receiver`, `ComplexContentNormalizer` (§5.7.1), `ResultBuilder`, `SequenceCollector`; `Nav`, the node type of the API; the pipeline's unit tests |
| `src/store.rs`, `src/store/tests.rs` | `DocumentStore`: the documents of a run, document identity by URI, `generate-id()`; `DocId` |
| `src/output.rs` (**pub**) | `OutputDefinition`: `xsl:output` merged by import precedence, character maps (§20); re-exports the resolved `SerializationParams` and their types |
| `src/serialize/` | The serializer: the `xml`, `xhtml`, `html` and `text` methods, output encodings, character maps, `disable-output-escaping`; `SerializationParams` |
| `src/numbering.rs` | Number to string conversion (§12.3), pure |
| `src/format_number.rs` | `format-number` and `xsl:decimal-format` variables (§16.4), pure |
| `src/format_date.rs` | `format-date`, `format-time`, `format-dateTime` (§16.5), pure |
| `src/functions.rs` | `XsltFunctions`: the §16 function library as an XPath function catalog, and the `use-when` evaluator |
| `src/collation.rs` (**pub**), `src/collation/` | The collation registry, the collations of `xsl:sort lang` / `case-order`, the UCA collations of the `icu-collation` feature |
| `src/schema.rs` | The schema-aware mode: `SchemaContext`, `SchemaContextBuilder`, `xsl:import-schema` (§3.14) |
| `src/regex.rs` | Regular expressions (§15) in `regexml`'s XPath 2.0 dialect |
| `src/scaling.rs` | Test-only (`#[cfg(test)]`, and included by `tests/common/mod.rs`): the check of the tests that guard against a quadratic cost |
| `examples/` | `transform`, `schema_aware`, `in_memory`: small programs on the public API |
| `tests/*.rs`, `tests/common/` | Integration tests (public API only, one file per subject, below) and their shared helpers |
| `tests/xslts/` | The W3C conformance harness: `driver`, `catalog`, `filter`, `environment`, `runner`, `engine`, `compare`, `report`, `xpath30` |
| `tools/suite_diff.py` | Per-case diff of two harness reports |
| `CHANGELOG.md` | The changes of each release |
| `doc/CONFORMANCE.md` | Every W3C case that does not pass and every excluded one, with the reason. Update it, with the baselines, whenever a case changes status |
| `doc/LIMITS.md` | What a transformation can read under each resolver, the resource limits, and advice for untrusted input |

The integration tests, by subject:

| Subject | Files |
|---|---|
| The core | `end_to_end`, `basic_semantics`, `dynamic_context`, `architecture_invariants`, `conformance_regressions`, `static_checks`, `use_when_and_not_implemented`, `compatibility`, `recursion_depth`, `stack_guard` |
| Namespaces | `namespaces`, `ns_inherit` |
| Patterns, keys, functions | `pattern_predicates`, `pattern_recovery_and_untyped_tests`, `keys`, `stylesheet_functions`, `fn_error` |
| Sorting, grouping, collations | `sorting`, `grouping`, `grouping_patterns_and_keys`, `uca_collations`, `collations_without_icu`, `collation_indexes_and_uca_parameters`, `uca_case_order_and_substrings` |
| Regular expressions, numbering, formatting | `regular_expressions`, `numbering`, `numbering_sequences_and_order`, `number_and_date_formatting` |
| Serialization and result documents | `serialization`, `serialization_details`, `serialization_corner_cases`, `result_documents`, `result_uris_and_initial_result`, `zero_length_results_and_rootless_uris` |
| The schema-aware mode | `schema_loading`, `schema_loading_details`, `typed_sources`, `typed_source_details`, `typed_patterns`, `construction_validation`, `copy_and_document_validation`, `type_driven_and_lax_validation`, `typed_ids_and_defaults`, `typed_qname_values`, `xml_schema_1_1`, `xpath_types_timezone_and_locations` |
| The API | `default_resolvers`, `debug_impls` |

## Conventions

- **The public API is the façade in `src/lib.rs`**: the root re-exports and the four public modules (`error`,
  `collation`, `resolver`, `output`). Every other module is private; an item inside one may be `pub`, which only makes
  it visible to the crate. A new public item is re-exported from `src/lib.rs` (or added to a public module) on purpose
  and documented: `#![warn(missing_docs)]` is on, and the count is 0. Every public type implements `Debug`
  (`#![warn(missing_debug_implementations)]`; a manual impl shows URIs, counts and options, never a tree, and ends in
  `finish_non_exhaustive()`). Public enums, and public structs whose fields a program only reads, are
  `#[non_exhaustive]`; `TransformOptions` is not, because hosts build it with `..Default::default()`. A test that
  needs an internal item is a unit test in the module that owns it, not an integration test.
- **Error codes.** Every error carries a code: an Appendix E code of XSLT 2.0 (constants in `src/error/codes.rs`), or
  an XPath, F&O or serialization code passed through unchanged. `src/error/codes.rs` was generated from the
  specification, but the generator is not in the repository: keep its rustdoc escapes (`\[xsl:…\]`, `<URL>`) when
  editing it by hand.
- **This crate's own two codes** (`error::codes_internal`): `XSLT2-NOTIMPL` for a construct the processor recognizes
  but does not implement (`XsltError::not_implemented(feature, owner)`; the message ends in
  `is not implemented (<owner>)`, the owner being what would provide it — a Cargo feature, `icu-collation` — and the
  harness counts such cases as notImplemented, grouped by the owner), and
  `XSLT2-INTERNAL` for a broken invariant or a lower-layer failure that has no specification code
  (`XsltError::internal`). An error the stylesheet raises with `fn:error` is never one of these, whatever its name
  (`ErrorOrigin`).
- **Cite the specification.** Comments and docs name the section (§N.N) and quote the sentence a rule comes from.
- **Whitespace** is XML whitespace: use `crate::whitespace`, never `char::is_whitespace`, `str::trim*` or
  `split_whitespace`, on XSLT or XPath values.
- **Stylesheet children** are walked only through `Module::children()` (or `Builder::children()` in the compiler),
  which applies `use-when` exclusion by construction.
- **A new XSLT element** is a new row of the static-checking table in `src/compile/checks.rs`.
- **Rustdoc**: public docs must not link to private items (`rustdoc::private_intra_doc_links`); name them in a code
  span instead.
- **Recursion** uses the native stack, guarded by a depth and stack limit (`TransformOptions::max_recursion_depth`,
  `TransformOptions::stack_limit`): a run checks the stack at every template, stylesheet function and instruction
  (`Transformation::check_stack`), a compilation at every instruction, every element of the static checks and every
  module of an include / import chain (`StackGuard::check_compilation`), both with the thread's limit
  (`thread_stack_limit`). A new recursion over the stylesheet or over a document checks the guard on each level; a
  walk over a whole tree (a copy, whitespace stripping, the serializer) is a loop with a cursor, not a recursion.
  `tests/stack_guard.rs` runs each deep case in a child process on a 2 MiB thread. A job that recurses deeply runs
  whole (compile, transform, serialize) inside `xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || …)`: the
  compiled stylesheet and the arena are `!Send`, so the closure has to own them.
- **Tests name what they test.** A test file, a test and a comment say what the behaviour is and cite the rule;
  they do not name the work item, plan or review that produced them.
- **Timing tests compare work, not time.** A test that guards against a quadratic (or worse) cost uses
  `scaling::assert_linear` (or `assert_quadratic`): the work at `8 n` against the work at `n` repeated eight times,
  never a wall-clock bound, so that it neither fails on a busy machine nor passes a regression on a fast one.
- **Results stay in memory.** A transformation writes nothing to disk: `Outcome::primary` and `Outcome::secondary`
  hold the trees, and `CompiledStylesheet::serialize_primary` / `serialize_secondary` (and their `_to_bytes` forms)
  serialize them.
