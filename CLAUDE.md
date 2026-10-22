# CLAUDE.md - xslt2

Schema-aware XSLT 2.0 processor on top of `../xsd-schema` (XPath 2.0 engine, XDM values,
PSVI, `BufferDocument`). Separate git repository.

## Source of truth

- `../../xslt20.html` — W3C XSLT 2.0 (Second Edition). Cite sections as §N.N and quote the text.
- `../../xslt_doc/XSLT2_DESIGN.md` — the architecture (section numbers are those of the old sketch).
- `../../xslt_doc/XSLT2_ROADMAP.md` — §1 Status is the one place for current numbers and commits; milestones
  M0–M5, §5 working method (agent-brief rules, verification battery), open questions Q1–Q43.
- `../../xslt_doc/XSLT2_LOG.md` — append-only review history. `../../XSLT2_DESIGN_SKETCH.md` is frozen; do not edit it.
- `../../XSLT2_M1_REPORT.md` — what M1 built, the per-set numbers and the classified failures.
- `../../XSLT2_M1_REVIEW_FIXES.md`, `../../XSLT2_M1_HARNESS_FIX_REPORT.md` — the 16 defects an
  independent review found in M0–M1, their root causes and fixes; regression tests in `tests/m1_review.rs`.
- `../../XSLT2_FEASIBILITY.md` — the original memo.
- Every error carries an Appendix E code: constants in `src/error/codes.rs` (generated from the
  spec), plus two of this crate's own in `error::codes_internal` (`XSLT2-NOTIMPL`,
  `XSLT2-INTERNAL`).

## Module map

| File | Responsibility |
|---|---|
| `src/error.rs`, `src/error/codes.rs` | `XsltError`, the 163 Appendix E codes, the two internal ones |
| `src/names.rs` | `QName`, prefix resolution, the `yes`/`no` spelling of §3.5 |
| `src/whitespace.rs` | XML whitespace (§4): the only four characters XSLT calls whitespace. Never use `char::is_whitespace`, `str::trim*` or `split_whitespace` on XSLT or XPath values |
| `src/resolver.rs` | `Resolver` (file, in-memory), URI arithmetic, non-UTF-8 decoding; `syntax_normalized` (RFC 3986 §6.2.2 after `iri-to-uri` escaping — the form result URIs are compared in, XTDE1490); dot segments: RFC 3986 §5.2.4's two-buffer algorithm transcribed under a scheme (`example:a/../c` → `example:/c`), a segment stack keeping a relative path relative without one (`a/../c` → `c`, `../a` kept) |
| `src/stylesheet.rs` | §3.10 module graph, import precedence, §5.4.1 lexical static contexts, §4.2 whitespace |
| `src/model.rs` | The `Instruction` tree (§7.4), declaration tables (§7.3), `CompiledStylesheet` |
| `src/pattern.rs` | Pattern grammar (§5.5.2), the §6.4 default priorities, the dispatch-index key |
| `src/compile/checks.rs` | The **static-checking table**: one `ElementSpec` row per XSLT 2.0 element (category, required/optional attributes, AVTs, content model) transcribed from the spec's syntax summaries, and the single pass that validates a module against it before compilation. A new element = a new row here |
| `src/compile/` | `Compiler`: modules → declarations and instructions. `mod.rs` (builder core), `declarations`, `decimal_format` (`xsl:decimal-format`), `output` (`xsl:output`, `xsl:character-map`), `instructions`, `literal`, `expressions`, `avt`, `patterns`, `modes`, `attributes`, `tree`, `functions` (`xsl:function`), `keys` (`xsl:key`), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`), `result_document` (`xsl:result-document`: written attribute values, the static XTDE1460 / XTSE1590), `number` (`xsl:number`: XTSE0975, `level`, the `count` / `from` patterns, the conversion attributes — fixed values checked here, XTSE0020). Walk stylesheet children **only** through `Module::children()` / `Builder::children()` — it applies §3.12 `use-when` exclusion by construction |
| `src/runtime/` | `Transformation`: `mod.rs` (entry points), `context` (§5.4; the §19.1 output state is `XsltContext::final_output`: only the initial context is final, `empty()` contexts are temporary, and the listed constructs clear it for their content), `dispatch` (§6), `patterns`, `eval` (instructions), `content`, `variables`, `documents` (loading, §4.4 admission, the admission of host-built typed trees and typed parameters), `typed_sources` (M4b: a source validated as it is parsed, in one pass — `xsd-schema`'s `TypedBuilderHandler` inside this crate's `SourceBuild` handler, which drops the whitespace text of element-only content as XDM 1.0 §6.7.4 says; this crate's sink; one `SchemaValidator` per run, identity constraints on; the typed admission copy for §4.3 `input-type-annotations="strip"` (is-id kept by re-registering ids) and §4.4 with the simple-content exemption), `functions` (the §16 evaluator), `format` (§16.4 / §16.5 glue: the function conversion rules for `$value`, `$picture`, `$language`, `$calendar`, `$country`; the decimal-format name → XTDE1280; `$calendar` expanded with no default namespace, format-date-037), `expressions`, `serialize` (a result tree through `src/serialize/`), `sort` (`xsl:sort`, `xsl:perform-sort`), `calls` (stylesheet functions), `keys` (key indexes), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`, the run's regex cache), `results` (`xsl:result-document`, the base output URI, XTDE1480 / XTDE1490, the §2.4 primary result; `SecondaryResult`), `number` (`xsl:number`: §12.1 `value` with the backwards-compatible NaN rule, XTDE0980; §12.2 the place marker of a node — single / multiple / any, the implied count pattern, walks only up and forward because the navigator's previous-sibling move scans from the first child; XTTE0990 / XTTE1000; XTDE0030 for computed conversion attributes; the nodes before $S are counted by a `Walk` — the tree in document order for `any`, a parent's children for `single` / `multiple` — that counts every implied pattern at once without `count` (`Census`, by node kind and expanded name), keeps an index of the numbers it gave once a node before its frontier is asked for (`Index`), and is kept per instruction (`NumberMemo`; `Walks`, by owner: every tree and every parent — a walk past 32 nodes always, the shorter ones until more than 1,024 walks are kept) while the local variables the patterns read keep their values (`Stability`, compared exactly: nodes by identity, strings / booleans / integers / doubles by type and bits) — numbering is linear in the nodes walked and the evaluations, in any order, over any number of names, namespaces, trees and parents; a pattern calling a stylesheet function counts afresh every time, see "`xsl:number` cost" below) |
| `src/receiver.rs` | `Receiver`, `ComplexContentNormalizer` (§5.7.1), `ResultBuilder`, `SequenceCollector`, `TemporaryOutputState` (the content of `xsl:document`: d-o-e dropped, §11.5/§20.2) |
| `src/store.rs` | `DocumentStore`: arena-allocated documents, URI identity, `generate-id()` |
| `src/output.rs` | `OutputDefinition`: the `xsl:output` attributes merged by import precedence (§20), `cdata-section-elements` united, `use-character-maps` concatenated and expanded; `compile/output.rs` reads `xsl:output` / `xsl:character-map` (XTSE1560, XTSE1570, XTSE1580, XTSE1590, XTSE1600, XTSE0020 parameter domains) |
| `src/serialize/` | The serializer (M3a; Serialization 1.0 2E, read at w3.org): `params` (an output definition resolved against a tree — §20's default method, per-method defaults, SEPM/SESU checks), `markup` (the `xml`, `xhtml`, `html` methods, iterative), `html` (HTML 4.01 / XHTML tables, URI escaping), `encoding` (UTF-8, UTF-16, US-ASCII, ISO-8859-1; anything else SESU0007), `charmap`, `marks` (`disable-output-escaping` kept beside the tree, §20.2); the text method in `mod.rs`. The result is a `String` (the encoding phase skipped, Ser. §4); `serialize_primary_to_bytes` encodes |
| `src/numbering.rs` | §12.3 number to string conversion, **pure** (no XPath or tree types; M3d's `format-date` presentation modifiers reuse it through `FormatToken::format_in_width`, the §16.5.1 width rules — zero padding in the token's digit family, the year's high-order digits dropped, spaces for the other tokens; a width modifier counts an ordinal's suffix, the token's own width does not): format tokenization (alphanumeric = L* and N* through `regexml`'s `\p{L}\p{N}`), decimal-digit tokens of every Unicode `Nd` family (a generated table of the 68 zeros) — both **Unicode 15.1**, `regexml`'s version, so a 16.0 digit or letter separates tokens (the table follows `regexml`: `numbering::tests::the_digit_table_is_the_unicode_version_of_the_regexes`), `A`/`a` (bijective base 26), `I`/`i` (1–3999), English `w`/`W`/`Ww` words and ordinals as Saxon 9.1 writes them (`Twenty-First`, cardinals unhyphenated; to `u64::MAX`), the 17 sequences of consecutive code points of the catalog's `combinations_for_numbering` (`SEQUENCES`: circled digits ⓪–㊿ over three blocks, `🄀 ⒈`–`⒛`, `⓿ ❶`–`⓴`, `🄋 ➀`–`➉`, `🄌 ➊`–`➓`, `⑴`–`⒇`, `⓵`–`⓾`, `㈠`–`㈩`, `㊀`–`㊉`, Aegean / Coptic epact / Rumi / Brahmi / Sinhala archaic 1–10, `🄁 🄂`–`🄊`, counting-rod units and Mende Kikakui 1–9; the Greek pair not — user), grouping, the fallback to the token `1` (unsupported tokens, out-of-range numbers); `check_attribute` (the permitted values of `lang`, `letter-value`, `grouping-separator`, `grouping-size`) |
| `src/format_number.rs` | §16.4 `format-number`, **pure** (M3d): `DecimalFormat` (the ten §16.4.1 variables and their defaults; XTSE1300's distinct picture characters, XTSE1295's zero digit), `Picture::parse` (§16.4.2's rules → the XTDE1310 message, §16.4.3's variables per sub-picture), `Picture::format` (§16.4.4) over a `Numeric` — `xs:double` / `xs:float` (×100 / ×1000 in the value's own type, then the shortest round-trip digits, `{:e}`) or an `xs:decimal` / `xs:integer` as its exact lexical digits — rounded half-to-even on the decimal digits. Readings decided where the text and the catalog differ: the suffix is the passive characters after the last active one of the whole sub-picture (string-121 `00%`). Where the text and practice differ, the literal XSLT 2.0 text is kept (lead decision): zero with a decimal-separator-sign and no zero-digit-sign (`format-number(0, '#.#')`, `'#,###.##'`) is the **empty string** — §16.4.3 sets the minimum-integer-part-size to one only "if the sub-picture contains no zero-digit-sign and no decimal-separator-sign", §16.4.4: "(The value zero will at this stage be represented by a decimal-separator-sign on its own.)", and that separator is then removed. F&O 3.1 §4.7.4 later sets the minimum-fractional-part-size to 1 when both minimums are zero (`.0`); Saxon 9.1 gives `0`; no catalog case decides it (`format_number::tests::zero_without_a_zero_digit_is_the_empty_string_in_xslt_2_0`) |
| `src/format_date.rs` | §16.5 `format-date` / `-time` / `-dateTime`, **pure** (M3d): the picture (`[[`, `]]`, markers, whitespace ignored, XTDE1340), components and their availability (XTDE1350), presentation modifiers through `numbering::FormatToken::format_in_width` (decimal families, `A a I i W w Ww`, the sequences, `o`; `t` changes nothing in English), `N n Nn` names (English months, days, `am`/`pm` — `a.m.` when a minimum width of 4 asks, never padded, date-064 —, `AD`/`BC`, `AD`/`ISO`, a few timezone abbreviations), width modifiers (§16.5.1, in characters — a token's own width such as `01` counts digits, as for `xsl:number`, so `[D01o]` is `03rd` and `[D1o,2]` `3rd`; a minimum above the maximum is XTDE1340; the year drops high-order digits, leaving room for an ordinal suffix; fractional seconds rounded half-to-even, XSLT 2.0, a carry out of the first digit dropped (`.96` with `[f,1-1]` is `0`, Saxon 9.1); a name longer than the maximum is abbreviated — three letters when that is no shorter than the minimum, else right-truncated to the maximum (`[FNn,4-4]` `Tues`) — and never padded; a full representation shorter than the minimum is padded, with zero digits for numbers and spaces otherwise), timezones (`±HH:MM`; `[z,2-2]` / `[z0]` drop zero minutes), ISO weeks for `W` / `w` / `F` in both calendars, years as XML Schema 1.0 reads them (no year zero: `-0055` is 55 BC, as the upstream arithmetic agrees); a lexical `0000`, which upstream accepts, is 1 BC as XML Schema 1.1 reads it (`Y` 1 and `E` BC in the `AD` calendar, like `-0001`; the `ISO` calendar writes the lexical year). The §16.5.2 fallbacks: another language → `[Language: en]`, another valid calendar (a designator of the table, or any prefixed name) → `[Calendar: AD]`; an invalid calendar is XTDE1340 (F&O 3.0's FOFD1340) |
| `src/compile/decimal_format.rs` | `xsl:decimal-format` (§16.4.1): values recorded per attribute with their import precedence, merged attribute by attribute after the declaration pass (XTSE1290 for two values at the top precedence), then XTSE1295 / XTSE1300 on the merged format; `char` attributes untrimmed, one character (XTSE0020). `CompiledStylesheet::decimal_format(name)` |
| `src/functions.rs` | The §16 function library as an XPath catalog, plus the `use-when` evaluator. No stub is left since M3d (`stubs` holds only §3.8's fallback error function) |
| `src/collation.rs`, `src/collation/uca.rs`, `src/collation/uca/search.rs` | The collation registry (`CollationResolver`): codepoint, HTML ASCII case-insensitive, host-registered; `CaseOrderCollation` for `xsl:sort lang` / `case-order`; with the `icu-collation` feature the F&O 3.1 §5.3.3 UCA URIs and locale-tailored sorting on ICU4X 1.5 (`uca.rs`) and the substring search under them (`uca/search.rs`) |
| `src/schema.rs` | The schema-aware mode (M4a; M4 plan D-1, D-7, D-9): `SchemaContext` (caller-owned; owns the `SchemaSet`, whose `NameTable` is the stylesheet's) and `SchemaContextBuilder` — a **pre-scan** of the module graph with the compiler's own `Loader` (schema-aware `use-when`), §3.14's highest-precedence `xsl:import-schema` per namespace (`plan`), loaded by location through the compiler's `Resolver` (`ResolverLoader`; a `file:` URI is handed to `xsd-schema` as its path, so C15a is avoided **for `file:` URIs only**: under another base `ResolverLoader::request` removes the dot segments `xsd-schema` keeps, and passes a relative location on relative, never absolutized against the working directory — but `xsd-schema` still keys a document by its own spelling, so one non-`file:` document under two spellings loads twice, C15a), by namespace only (the embedded `xml.xsd` / `xlink.xsd`, also the fallback when a location of those namespaces cannot be read; the built-in `xsi:` declarations; the Schema for Schemas is not available, so an import of the XML Schema namespace is not `found()`), or inline as a **slice** of the module text with the inherited namespace bindings injected after `<xs:schema`, the elements §3.12 `use-when` excludes blanked out byte for byte (`blank_excluded`), and the location `<base URI>#schema-N` (N from 1 per module; a module reached twice loads its inline schemas once, `DeclarationId`; errors mapped back to the module line). A document that cannot be read is no error (§3.14); one that is not a valid schema, or has another target namespace than the declaration's, is XTSE0220; XTSE0215 for an inline schema with `schema-location` or a conflicting `namespace`; XTSE0010 for content other than one `xs:schema`. `check_coverage`: `Compiler::schema_aware` refuses a stylesheet whose imports the context did not load (`XSLT2-INTERNAL`) — a namespace-only import is satisfied by a context that holds the namespace's components (`SchemaContext::covers`). `in_scope_type` / `built_in_type` / `has_user_constructor`: the in-scope types for XTSE1520 / XTSE1530, `type-available` and `function-available` (`functions::TypeScope`). **Host schemas** (M4b, §3.13's implementation-defined mechanism, D-2): `SchemaContextBuilder::schema_uri` / `schema_file` / `schema_str`, loaded before the imports into the same set (a namespace-only import of their namespace is `found()`, a location import of the same document loads it once; unreadable or invalid → XTSE0220), listed by `SchemaContext::host_schemas` |
| `src/regex.rs` | §15 regular expressions: `regexml`'s XPath 2.0 dialect (`Regex::xpath20`, the constructor `xsd-schema`'s `fn:matches` uses too), XTDE1140 / XTDE1145 / XTDE1150 by `ErrorKind`, and the engine's 64-level nesting limit as `XSLT2-INTERNAL` |

## Build

```bash
cargo build
cargo clippy --all-targets         # 0 warnings expected
cargo test                         # unit tests + every tests/*.rs except the W3C harness (it skips itself in debug): m0b…m2a, m2c, m2d_sorting, m2e_functions, m2f_keys, m2g_grouping, m2g_review, m2h_regex, m2i_core_defects, m2l_pattern_predicates, ns_inherit, q33_depth, …
                                   # + fn_error + m2b_namespaces + m2c_static_checks + m3a_serialization + m3a_review + m3a_review2
                                   # + m3b_result_documents + m3b_review + m3b_review2 + m3c_numbering + m3c_followup
                                   # + m3d_formatting + m3e_compat + m4a_schema_loading + m4a_review
                                   # + m4b_typed_sources + m4b_review
                                   # (tests/common/mod.rs: `common::body` drops the default XML
                                   # declaration for the tests written before M3a)
                                   # (the W3C harness skips itself in a debug build)
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps   # must exit 0
```

The off-by-default **`icu-collation`** feature (ICU4X 1.5 `icu_collator`; UCA collation URIs, locale-tailored
`xsl:sort lang` / `case-order` — DESIGN §9.5, M2k) is checked in both configurations: `cargo clippy --all-targets
--features icu-collation`, `cargo test --features icu-collation` (runs `tests/m2k_collations.rs` and the UCA halves of
`tests/m2k_review.rs` and `tests/m2k_review2.rs`; the default build runs `tests/m2k_feature_off.rs` instead), `RUSTDOCFLAGS='-D warnings' cargo
doc --no-deps --features icu-collation`, and the harness with `cargo test --release --features icu-collation --test
xslts -- …`. ICU must stay on the 1.5 line `regexml` uses: `cargo tree --features icu-collation -d` shows no second
`icu_*` version. **The feature is not additive**: it changes `xsl:sort lang` / `case-order` results (UCA tailoring
instead of `CaseOrderCollation`), and Cargo feature unification turns it on for every user of the crate in a build
once any dependent enables it. A UCA URI's absent parameters take the language's CLDR defaults (Thai `alternate=shifted`,
`fr-CA` `backwards`, Danish upper-first); substring functions under UCA match collation-equal substrings at character
boundaries — `collation/uca/search.rs` reads the collation data to narrow the candidates (primary filter, count filter,
pruning at safe boundaries) and gives up with FOCH0004 beyond a work budget (`search::BUDGET`, about a second); its
differential test against the unpruned definition is `uca::tests::the_search_agrees_with_the_definition`.

`src/error/codes.rs` was generated from the spec, but the generator is not in the repository: keep its
rustdoc escapes (`\[xsl:…\]`, `<URL>`) when editing it by hand. Public docs must not link to private items
(`rustdoc::private_intra_doc_links`) — name them in a code span instead.

## W3C conformance harness (`tests/xslts`)

The XSLT 2.0 suite is W3C-member-only; the public XSLT 3.0 suite marks 2.0-applicable cases
(`spec` = `XSLT20+`, `XSLT20`, `XSLT10+`, or absent). It is checked out at `../../xslt30-test`.

**Always run in `--release`** — a debug build is ~20× slower and its stack frames are large
enough to exhaust the worker stack, so the driver skips itself there unless `--debug` is given.

```bash
cargo test --test xslts --release                       # whole applicable suite
cargo test --test xslts --release -- -g value-of -f     # one test-set, failures only
cargo test --test xslts --release -- -t lre-027 -v      # one case, by name
cargo test --test xslts --release -- --stub             # the M0 stub engine
cargo test --test xslts --release -- --self-test        # the harness's own checks (compare, xpath30, engine)
cargo test --test xslts --release -- --lenient-xml      # triage: ignore prefixes/namespaces in assert-xml
cargo test --test xslts --release -- --report /tmp/x.xml
cargo test --test xslts --release -- -l                 # list test-sets with counts
```

Save verbose output to a file and grep it:

```bash
cargo test --test xslts --release -- -v > /tmp/xslts.txt 2>&1
grep -E "^  (FAIL|WRONG-ERROR|HARNESS-ERROR|NOT-IMPL)" /tmp/xslts.txt
```

**Schema-aware runs** (`--schema-aware`, M4a): `feature=schema_aware` is satisfied, so the 584 schema-aware cases
run and the 16 basic twins do not; the adapter then compiles **every** case with a schema-aware processor —
`SchemaContext::builder().stylesheet_file(…).build()` from the stylesheet's own `xsl:import-schema` declarations,
then `Compiler::schema_aware` — so every basic case must keep its default-run status (§2.8, M4 plan §2.2). Since
M4b the context also gets the environment's `<schema>` documents with the roles `source-reference` and
`stylesheet-import` and those with no role (`engine::context_schema`; a `secondary` one is loaded through the
directives of the schema that includes it), as host schemas (`SchemaContextBuilder::schema_file`), and each source is
loaded with `transform_source_with` / `register_source_with` as its `validation` says (`engine::source_validation`:
`strict`, `lax`, `skip`; **no attribute** is `strict` when the environment has a `source-reference` schema, `skip`
otherwise — decision D-M of `../../XSLT2_M4B_REPORT.md`); a basic run validates nothing. (The M4a "M4b fence" is
gone.) Run it beside the default run and diff each against its own baseline (the first schema-aware one is
`../../baselines/xslts-sa-5794-at-xsd-27180f2-xslt2-8340257.xml`):

```bash
cargo test --release --test xslts -- -s ../../xslt30-test --skip unicode-90 --schema-aware --report /tmp/sa.xml
```

**Per-case diff against a baseline** — the zero pass→non-pass gate. Baseline reports and the XSD FAIL-name
lists live outside the repositories in `../../baselines/` (see its `README.md`); never overwrite one, add a new
file named after the commits it was taken at:

```bash
cargo test --test xslts --release -- --skip unicode-90 --report /tmp/new.xml
python3 tools/suite_diff.py ../../baselines/<current baseline>.xml /tmp/new.xml   # see ../../baselines/README.md
```

`tools/suite_diff.py` compares status **and** failure comment per case; a comment-only change is listed
separately. Always pass `--skip unicode-90` (the set runs for hours).

**What each assertion reads** (`tests/xslts/compare.rs`, module docs). The adapter keeps every result
tree in two forms (`runner::ResultDocument`): `serialized`, written with the stylesheet's own
`xsl:output`, which only `serialization-matches` and `assert-serialization` read; and `tree`, written with
the catalog's default parameters (method xml, indent no, omit-xml-declaration yes), which every
assertion about the tree reads — `assert`, `assert-xml`, `assert-string-value`, `assert-eq`,
`assert-empty` — re-parsed with roxmltree, so `indent="yes"`, the text method or an output encoding
never add or remove a node. A failure of the stylesheet's own serialization is the run's error only
when the case asks for serialization (`<output serialize="yes"/>`; the catalog's default is `no`);
otherwise the tree assertions still run and only the serialization assertions see the failure (M3a).
An `assert-serialization` without `method` is compared under the method the stylesheet used.
Secondary results (`xsl:result-document`) are kept the same way in `RunOutput::secondary`, each under the
absolute URI it was written to, with the same serialization-failure policy; the adapter sets the base output
URI to the test-set directory (with a trailing slash), and `assert-result-document`'s relative `uri` matches
the tail of that absolute URI at a path-segment boundary. An `assert` over a secondary result sees its URI as
the re-parsed document's base URI (`base-uri()`, `result-document-0102`).
`assert-string-value` normalizes unless `normalize-space="false"` (the catalog's `default="true"`), with
XML whitespace only. `assert-true` / `-false` / `-count` / `-type` / `-deep-eq` / `-permutation` need a
raw sequence result, which an XSLT 2.0 run never delivers: `harnessError`.

`NOT-IMPL` is this processor's own `XSLT2-NOTIMPL` code: the case is runnable and fails, but it
fails because a feature belongs to a later milestone, not because the answer is wrong. Dependency
policy lives in `tests/xslts/filter.rs` (`dependency_supported`); the exclusion list (`EXCLUDED`)
must justify every entry. `combinations_for_numbering` is asked of the processor itself: the value
(a Unicode name, `NUMBERING_NAMES` — the catalog schema's vocabulary, not a second table of what is
supported) and every number of its `range` (absent = `unbounded`, probed at 1–1000 and 2^32 − 1) go
to `engine::numbering_probe`, which runs `<xsl:number value="." format="…"/>` through the public API
and refuses the case if any number comes out as the token `1` would write it (cached per request;
`--self-test` checks it). `tests/xslts/engine.rs` is the adapter, and it uses only the public API.

## Running one stylesheet by hand

```rust
let names = NameTable::new();
let functions = XsltFunctions::new();
let sheet = Compiler::new(&names, &functions).compile_file(Path::new("a.xsl"))?;
let arena = bumpalo::Bump::new();
let options = TransformOptions {
    base_output_uri: Some("file:///…/out/".into()), // default: the principal stylesheet module's URI
    ..Default::default()
};
let run = sheet.transformation(&arena, options);
let outcome = run.transform_uri("file:///…/in.xml")?;
println!("{}", sheet.serialize_primary(&outcome)?);      // an xsl:result-document's format if one made it
for tree in &outcome.secondary {                          // every other xsl:result-document, in evaluation order
    println!("{}: {}", tree.uri, sheet.serialize_secondary(tree)?);   // or serialize_secondary_to_bytes
}
```

Nothing is written to disk: `Outcome::secondary` holds each tree with its URI (`href` resolved against the base
output URI — absolute only when that is: `compile_str(text, "")` and no option give an empty base, relative result
URIs and the primary at `""`; a directory base needs its trailing `/`, or RFC 3986 replaces its last segment), its
escaping marks and its effective `OutputDefinition`; `Outcome::result_document(uri)` finds a secondary one by any
RFC 3986 §6.2.2 spelling of its URI (the primary is `outcome.primary`, at `outcome.base_output_uri`;
`sheet.primary_output_definition(&outcome)` is its definition). `outcome.primary` is `None` when the initial
template's result was empty, some `xsl:result-document` was evaluated, and none of them wrote to the base output URI
(§2.4); with no `xsl:result-document` at all an empty result is an empty document. Any item counts as a non-empty
result — `<xsl:text/>` and `<xsl:document/>` too — so next to an `xsl:result-document` without `href` it is XTDE1490,
also when the zero-length text node comes out of a template with `as` or `xsl:perform-sort`. A `BufferDocument` cannot
hold a text node with an empty value (upstream `BufferDocumentBuilder` adds one only for non-empty text), so `SequenceCollector` counts
such nodes instead of keeping them (`zero_length_text_nodes`) and those two pass them on as zero-length `text` events;
a typed variable, parameter or function result still loses them (`count()`, `as="text()"` see one item fewer).
A parentless namespace node (`xsl:namespace`, or a copy of a namespace node, in a sequence) is built on a hidden holder
element, and the upstream navigator has no cursor for a namespace node without an owning element (`receiver.rs`
`Locate::Namespace`): `..`, `ancestor::`, `root()` and `xsl:number` see the holder as its parent (`count($ns/..)` is 1,
`<xsl:number select="$ns" count="*"/>` gives `1`; XDM and Saxon: no parent, empty). The fix is a parentless namespace
cursor upstream (M3c review F2).

**Memory**: every result tree (and every temporary tree) costs about 118 KB of the run's arena however small it is
(the upstream `BufferDocumentBuilder` reserves a 4,096-node page and a 4,096-entry namespace page per tree), kept until
the arena is dropped — tens of thousands of result documents need gigabytes (10,000 ≈ 1.2 GB). The fix is upstream
(xsd-schema Wave B); the streaming result-document handler, which lets a host drop each tree, is planned for M5.

**`xsl:number` cost** (`src/runtime/number.rs` module docs; release, flat documents of N elements each with a text
child, or nested; times with compile, parse and serialize; baseline `name()` 0.028–0.031 s at N = 50,000): linear in
the nodes walked and the evaluations, in any order of evaluation, since the M3c follow-up and its review fixes
(`../../XSLT2_M3C_FOLLOWUP_REPORT.md`, `../../XSLT2_M3C_FOLLOWUP_REVIEW_FIXES.md`): a kept walk visits each node at
most twice, and an evaluation that needs a dropped short walk again (walks of at most 32 nodes are dropped together
once more than 1,024 walks are kept) walks at most 32 nodes of it again — N = 50,000 in 0.033–0.056 s in document
order at every level; an implied count over 200 / 1,000 / 5,000 random names 0.037–0.042 s (was `any` 0.47 / 1.8 /
8.3 s); reverse document order 0.036–0.060 s (was, at 10,000, `any` 2.5 s); a pseudo-random `xsl:sort` order 0.070 s
(was `single` 5.4 s); `count="*[$yes]"` reading a local variable 0.046–0.11 s (was, at 10,000, 12.1 s); item k of each
of 33 / 50 sections in turn 0.064 / 0.26 s for 49,500 / 200,000 items (was 0.77 / 8.0 s with 32 parents kept); 33 / 40
trees in turn 0.108 / 0.132 s (was 3.7 / 4.4 s with 32 trees kept); 50,000 elements of one local name in 50,000
namespaces 0.056–0.061 s (was 1.8 s). Memory: 16 bytes per node a walk indexed, a census entry per kind and name a walk
met, and at most 1,024 walks (168 bytes each), or twice the walks past 32 nodes. Still quadratic, each evaluation
counting afresh (N = 10,000, `any`): a `count` / `from` pattern calling a stylesheet function (it may construct new
nodes at each call) 19.0 s; a local variable the pattern reads that changes at every evaluation (the walks are reset)
20.8 s, and one holding a decimal, date, QName … (compared only by a lossy equality, so never kept).
`runtime::number::tests` guard the linear shapes by counting the nodes the walks test (and the census probes).

**Compatibility modes** (§3.8, §3.9; DESIGN §9.10; M3e, `tests/m3e_compat.rs`). `StaticContext::compat` is lexical
(the nearest `[xsl:]version`). Backwards-compatible: XPath 1.0 compatibility mode and the fallback error function
(`compile/expressions.rs`), `Avt::first_item_only` (§5.6, set when the AVT is compiled), `ValueOf::first_item_only`,
the `xsl:number` / `xsl:sort` / key rules, and §20's `xml` default method for an XHTML result when the principal
module's `xsl:stylesheet` says `version="1.0"` and the tree is created implicitly (`principal_version_1_0`; a 1.0
included module or template does not count). Forwards-compatible: unknown top-level XSLT elements — and XSLT 2.0
elements that are not declarations — ignored with their content, decided from the element's own `version` alone
(`checks::own_compat`, in both the checks pass and `declarations.rs` `declaration`, before its static context reads any
other standard attribute); unknown attributes ignored, `xsl:`-prefixed ones included; in a sequence constructor an
XSLT element that is not an instruction falls back to its `xsl:fallback` children or is the static error — including
an `xsl:param` / `xsl:sort` after the **leading run** of a template, function, `for-each`, `for-each-group` or
`perform-sort` (`instructions::leading_run_ends`: only the leading run declares parameters or sort keys;
`Builder::falls_back`); an XPath static error other than XPST0017 / XPST0081 on a function name is deferred (§5.3) —
the expression compiles to `()` with the error in `CompiledExpr::deferred`, which `eval_expr` returns when it is
evaluated, so nothing a stylesheet writes can forge it; `unknown_function_call` re-checks every function call of an
expression that parses, so an undeclared variable cannot hide an unknown function (§18.1.1 note); pattern
predicates are compiled strictly. Extension instructions (§18.2): `InstructionKind::ExtensionInstruction` keeps the expanded name and
the concatenated `xsl:fallback` bodies; with none it is XTDE1450 when evaluated, never at compile time. §5.7.2's
"adjacent text nodes … merged" applies to `select` values and AVTs too (`Transformation::simple_strings`); the
backwards-compatible first-item rules take the first *atomized* item without merging.

A deeply recursive stylesheet needs `xslt2::run_on_large_stack(DEFAULT_STACK_SIZE, || …)` around
the whole job (compile included): everything involved is `!Send`, so the closure has to own it.

**Schema-aware mode** (§21.2, M4a; `tests/m4a_schema_loading.rs`). `Compiler::new` is a basic processor:
`xsl:import-schema` is XTSE1650 (checked in `compile/checks.rs`, so a `use-when`-excluded one is not), `[xsl:]type`
and a non-`strip` `[xsl:]validation` / `default-validation` XTSE1660, typed input XTDE1665. A schema-aware stylesheet
is compiled with `Compiler::schema_aware(&context, &functions)` against a `SchemaContext` built from **the same**
stylesheet first (`SchemaContext::builder().with_resolver(…).stylesheet_file(…)?.build()?`): the module graph is
parsed twice (the pre-scan costs about 3 % of a compile — 4.2 ms of 162 ms for a 460 KB, 3,000-template module — and
a schema set 9–27 µs). Every XPath static context gets the set (`with_schema_set`), so the imported types work in
`instance of`, `cast as`, `castable as`, constructor functions (upstream C37) and `as`; `use-when` sees every
built-in type but no imported one (§3.12). The static checks of `[xsl:]type` (XTSE1520, XTSE1530 — the whole set is
in scope, D-7), `[xsl:]validation` / `default-validation` values (XTSE0020) and XTSE1505 are in `compile/checks.rs`;
the value is read into `model::Validation` on every node constructor (the module's `default-validation` filling an
absent one) and **fenced at evaluation** (`runtime/eval.rs` `validation_fence`, `Validation::owner`): anything but
`strip` is `XSLT2-NOTIMPL` naming M4d (constructed elements and attributes, `type` on `xsl:attribute`), M4e (copies,
`xsl:document`, `xsl:result-document`, the implicit final result tree under the principal module's
`default-validation="preserve"`) or M4f (`type` elsewhere, `lax`); typed patterns with M4c (when the pattern
**compiles**: a stylesheet with one does not compile). A stylesheet function with the name and arity 1 of a user
atomic type's constructor function is XTSE0770, whatever `override` says (decision D-A, `compile/functions.rs`); one
in the XML Schema namespace is XTSE0080 (§3.2). (The upstream limits M4a met — QName / NOTATION enumerations compared
by lexical form, C33; a restriction of an enumerated type losing the enumeration, C44 — are fixed on `wave-b`
`66e3e3f` / `5fcc42b`.)

**Typed source documents** (M4b; M4 plan D-2, D-4, D-13; `tests/m4b_typed_sources.rs`). `TransformOptions::
source_validation` (`SourceValidation::{Skip, Strict, Lax}`, default `Skip`) is the mode of every source-tree loader —
`transform_source`, `transform_uri`, `document()`, `doc()`, registered sources —, and `transform_source_with`,
`load_document_with`, `register_source_with` and `set_source_validation` (a URI the resolver serves, no text) override
it per document; a stylesheet module read by `document()` / `doc()` (`document('')`) is not validated by the default
(decision D-V; `CompiledStylesheet::is_module_uri`), only by a mode set for its URI. A validated document is read once by
`xsd-schema`'s validation driver building the typed tree (C14), against the context's one schema set (the
stylesheet's imports and the host schemas; `xsi:schemaLocation` hints are not followed), with identity constraints
and without the PSVI typed values nothing reads (`BUILD_PSVI_TYPED_VALUES` off); a valid attribute and a valid
simple-content element have their [schema normalized value] as string value (XDM 1.0 §6.2.4, §6.3.4 — the typed build
does it since `66e3e3f`, C66); whitespace text in element-only content is not in the tree (XDM 1.0 §6.7.4 — this
crate's `SourceBuild` drops it until the typed build does, C67). An invalid document is an error with the
loader's code for an unparsable one (FODC0002 for `doc()` / `document()`, F&O 1.0 §15.5.4; XTDE1170 for the principal
source), the message naming the first errors and their nodes (B8 instance paths); `Lax` with an undeclared root is
NOT-IMPL (M4f, upstream C36); a basic processor asked to validate is XTDE1665. A typed document atomizes to typed
values and an untyped one exactly as before (one flow, `has_type_annotations`): keys, sort keys and grouping keys see
typed values. `input-type-annotations="strip"` in any module (§4.3) strips every source tree at admission — untyped,
nilled false, the ids the attributes give kept (re-registered on the copy; an element's own `xs:ID` waits for C62,
is-idrefs for C54) —, before §4.4's whitespace stripping, which spares the children of an element with a simple type
or simple content; the typed admission copy keeps nilled from the PSVI (`xsi:nil`), comments and PIs in a nilled
element included. **Defaults are not kept, because they are never there**: the typed build creates no text node for
an element's `default` / `fixed` value (its typed value is the default, its string value empty; under `strip` the
default is gone) and no defaulted attribute — upstream C39 (extended); no catalog schema declares an element default,
one M4b case needs defaulted attributes since C53 lets it run (`validation-0701`: the schema for XSLT 2.0 defaults
`xsl:apply-templates/@select` to `child::node()`); `tests/m4b_review.rs` pins it. A tree the host built and inserted
(`DocumentStore::insert`) is admitted (`admit_stored`, a private `StoredDocument::admitted` mark) wherever it enters —
`transform_document` or `doc()` / `document()` of its URI: taken as it is when typed against the context's set,
stripped under `strip`, stripped with a warning (`Transformation::take_warnings`) when typed against another set
(D-2); on a basic processor XTDE1665 unless `strip` (§21.1); not whitespace-stripped (as for an untyped one). A stylesheet parameter typed against another
set is `XSLT2-INTERNAL`. Known upstream limits met here: defaults (C39, above), `fn:idref` (C54), an element whose
typed value is an `xs:ID` is not in the id index (C62), `fn:resolve-uri` refuses a relative reference with a space
(C63), whitespace in element-only content kept by the typed build (C67, dropped here). Fixed on `wave-b` `66e3e3f`:
QName / NOTATION in the value space (C33), `schema-for-xslt20.xsd` (C53), the normalized string value (C66). Cost (release, a 5.1 MB source of 40,000
records, `load_document_with`, median of 5, on `wave-b` `66e3e3f`): untyped 45 ms (113 MB/s), validated and typed
126–130 ms (40 MB/s, ×2.8; the normalized string values of C66 cost about 3 %, the PSVI typed values left off save
about 10 %); with `xsl:strip-space` 124 ms untyped / 154–156 ms typed; validated under
`input-type-annotations="strip"` 153–158 ms. Schema-aware run on `66e3e3f`: 5,960 of 6,355, profile 217 of 579.

## The `regexml` engine

`regexml` is the crates.io package **`regexml-fork`** (0.3; the maintained fork of Paligo's `regexml`, repository
`semyonc/regexml`), renamed back in `Cargo.toml` (`regexml = { package = "regexml-fork", … }`) so the code says
`regexml`. `xsd-schema` depends on it the same way, so the graph holds one engine and one ICU4X 1.5 line: `cargo tree
-d` (also with `--features icu-collation`) shows no second `regexml` or `icu_*` version. No local `[patch]` is needed;
an old untracked `.cargo/config.toml` with `[patch.crates-io] regexml = { path = "../regexml/regexml" }` is obsolete —
a patch only replaces a package of its own name, so it no longer applies — and should be deleted (then `git checkout
Cargo.lock` if it rewrote the lockfile).

The tests that need the fork's fixes assert them unconditionally (`m2h_regex`'s
`a_backtracked_alternative_does_not_erase_an_earlier_capture`, capture restoration on backtracking).

## Upstream (`../xsd-schema`)

`xslt2` depends on it by path. Since 2026-10-18 `xslt2` `main` builds against the `xsd-schema` branch **`wave-b`**
(the breaking 0.3.0 work; keep `../xsd-schema` checked out on it): `XPathError` / `ItemType` / `FunctionId` are
`#[non_exhaustive]`, an `XsltError` travels through the engine as the host payload of `XPathError::Raised`
(`XsltError::into_xpath_error`, recovered by `raised_error_to_xslt`), `fn:error` is the engine's built-in, and QName /
NOTATION values print their lexical form (`to_string_value`). Upstream changes are **releases**. After
touching `src/xpath/*` re-run the XQTS selection (8,047/8,047); after touching `src/document/*` or
`src/navigator/*` re-run both XSD suites and compare the totals.
