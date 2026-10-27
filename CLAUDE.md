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
| `src/pattern.rs` | Pattern grammar (§5.5.2), the §6.4 default priorities, the dispatch-index key; the typed node tests (M4c) — `element(N\|*, T[?])`, `attribute(N\|*, T)`, `schema-element(N)`, `schema-attribute(N)`, inside `document-node(…)` too — carry `xsd-schema`'s resolved `ItemType`, which the compiler's `PatternSink::type_test` / `schema_test` build (`compile/patterns.rs`) |
| `src/compile/checks.rs` | The **static-checking table**: one `ElementSpec` row per XSLT 2.0 element (category, required/optional attributes, AVTs, content model) transcribed from the spec's syntax summaries, and the single pass that validates a module against it before compilation. A new element = a new row here |
| `src/compile/` | `Compiler`: modules → declarations and instructions. `mod.rs` (builder core), `declarations`, `decimal_format` (`xsl:decimal-format`), `output` (`xsl:output`, `xsl:character-map`), `instructions`, `literal`, `expressions`, `avt`, `patterns`, `modes`, `attributes`, `tree`, `functions` (`xsl:function`), `keys` (`xsl:key`), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`), `result_document` (`xsl:result-document`: written attribute values, the static XTDE1460 / XTSE1590), `number` (`xsl:number`: XTSE0975, `level`, the `count` / `from` patterns, the conversion attributes — fixed values checked here, XTSE0020). Walk stylesheet children **only** through `Module::children()` / `Builder::children()` — it applies §3.12 `use-when` exclusion by construction |
| `src/runtime/` | `Transformation`: `mod.rs` (entry points), `context` (§5.4; the §19.1 output state is `XsltContext::final_output`: only the initial context is final, `empty()` contexts are temporary, and the listed constructs clear it for their content), `dispatch` (§6), `patterns`, `eval` (instructions), `content`, `variables`, `documents` (loading, §4.4 admission, the admission of host-built typed trees and typed parameters), `validate` (M4d: what `xsl:attribute` validates — `type`, `strict`, `lax`, XTTE1540 / 1510 / 1512 / 1515 / 1545 —, the boundary each element constructor announces, and `Validating`, the validating receiver in front of every tree builder: the annotation rule and `strict` element validation through the push API; M4e: the boundaries of copies and document nodes, `copy_validation`, strict copies of attributes, XTTE0950, and document-level validation in `Validating`; M4f: `type` and `lax` on every constructor — a run per boundary rooted
as `xsd-schema`'s `ValidationRoot` says; see "Construction validation", "Copies and documents" and "Type-driven and lax
validation" below), `typed_sources` (M4b: a source validated as it is parsed, in one pass — `xsd-schema`'s `TypedBuilderHandler` as it is, which since `wave-b` `d5f2a8e` drops the whitespace text of element-only content as XDM 1.0 §6.7.4 says (C67; this crate's own `SourceBuild` handler that did it went in the batch (t) follow-up) and makes defaulted attributes and element defaults nodes (C39); this crate's sink; one `SchemaValidator` per run, identity constraints on; the typed admission copy for §4.3 `input-type-annotations="strip"` (is-id and is-idrefs kept by re-registering: `register_xml_id` for the ids an element answers to by its attributes and by its own typed value, `register_idrefs`) and §4.4 with the simple-content exemption), `functions` (the §16 evaluator), `format` (§16.4 / §16.5 glue: the function conversion rules for `$value`, `$picture`, `$language`, `$calendar`, `$country`; the decimal-format name → XTDE1280; `$calendar` expanded with no default namespace, format-date-037), `expressions`, `serialize` (a result tree through `src/serialize/`), `sort` (`xsl:sort`, `xsl:perform-sort`), `calls` (stylesheet functions), `keys` (key indexes), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`, the run's regex cache), `results` (`xsl:result-document`, the base output URI, XTDE1480 / XTDE1490, the §2.4 primary result; `SecondaryResult`), `number` (`xsl:number`: §12.1 `value` with the backwards-compatible NaN rule, XTDE0980; §12.2 the place marker of a node — single / multiple / any, the implied count pattern, walks only up and forward because the navigator's previous-sibling move scans from the first child; XTTE0990 / XTTE1000; XTDE0030 for computed conversion attributes; the nodes before $S are counted by a `Walk` — the tree in document order for `any`, a parent's children for `single` / `multiple` — that counts every implied pattern at once without `count` (`Census`, by node kind and expanded name), keeps an index of the numbers it gave once a node before its frontier is asked for (`Index`), and is kept per instruction (`NumberMemo`; `Walks`, by owner: every tree and every parent — a walk past 32 nodes always, the shorter ones until more than 1,024 walks are kept) while the local variables the patterns read keep their values (`Stability`, compared exactly: nodes by identity, strings / booleans / integers / doubles by type and bits) — numbering is linear in the nodes walked and the evaluations, in any order, over any number of names, namespaces, trees and parents; a pattern calling a stylesheet function counts afresh every time, see "`xsl:number` cost" below) |
| `src/receiver.rs` | `Receiver` (with the `validation_boundary` event, M4d — also before an attribute a `strip` copy made, M4e — and `append_copy` with the copy's `CopyValidation`, M4e; `element_id`, `attribute_idrefs`, `element_idrefs`: the is-id and is-idrefs a `strip` copy keeps without a binding, batch (t) follow-up; `attribute_copied_from`, `element_copied_from`: the source of a `preserve` copy's node, read only by a receiver that strips its binding, its fix round 1), `ComplexContentNormalizer` (§5.7.1; `copy_selected`: the copy of one selected node under its validation), `ResultBuilder` (builds bindings as they arrive; `file_id`; `keep_attribute` / `keep_open_element`: is-id and is-idrefs of a node a `strip` boundary untyped, as `Validating` decides them), `SequenceCollector` (copies and parentless nodes built through `Validating` too), `TemporaryOutputState` (the content of `xsl:document`: d-o-e dropped, §11.5/§20.2) |
| `src/store.rs` | `DocumentStore`: arena-allocated documents, URI identity, `generate-id()` |
| `src/output.rs` | `OutputDefinition`: the `xsl:output` attributes merged by import precedence (§20), `cdata-section-elements` united, `use-character-maps` concatenated and expanded; `compile/output.rs` reads `xsl:output` / `xsl:character-map` (XTSE1560, XTSE1570, XTSE1580, XTSE1590, XTSE1600, XTSE0020 parameter domains) |
| `src/serialize/` | The serializer (M3a; Serialization 1.0 2E, read at w3.org): `params` (an output definition resolved against a tree — §20's default method, per-method defaults, SEPM/SESU checks), `markup` (the `xml`, `xhtml`, `html` methods, iterative), `html` (HTML 4.01 / XHTML tables, URI escaping), `encoding` (UTF-8, UTF-16, US-ASCII, ISO-8859-1; anything else SESU0007), `charmap`, `marks` (`disable-output-escaping` kept beside the tree, §20.2); the text method in `mod.rs`. The result is a `String` (the encoding phase skipped, Ser. §4); `serialize_primary_to_bytes` encodes |
| `src/numbering.rs` | §12.3 number to string conversion, **pure** (no XPath or tree types; M3d's `format-date` presentation modifiers reuse it through `FormatToken::format_in_width`, the §16.5.1 width rules — zero padding in the token's digit family, the year's high-order digits dropped, spaces for the other tokens; a width modifier counts an ordinal's suffix, the token's own width does not): format tokenization (alphanumeric = L* and N* through `regexml`'s `\p{L}\p{N}`), decimal-digit tokens of every Unicode `Nd` family (a generated table of the 68 zeros) — both **Unicode 15.1**, `regexml`'s version, so a 16.0 digit or letter separates tokens (the table follows `regexml`: `numbering::tests::the_digit_table_is_the_unicode_version_of_the_regexes`), `A`/`a` (bijective base 26), `I`/`i` (1–3999), English `w`/`W`/`Ww` words and ordinals as Saxon 9.1 writes them (`Twenty-First`, cardinals unhyphenated; to `u64::MAX`), the 17 sequences of consecutive code points of the catalog's `combinations_for_numbering` (`SEQUENCES`: circled digits ⓪–㊿ over three blocks, `🄀 ⒈`–`⒛`, `⓿ ❶`–`⓴`, `🄋 ➀`–`➉`, `🄌 ➊`–`➓`, `⑴`–`⒇`, `⓵`–`⓾`, `㈠`–`㈩`, `㊀`–`㊉`, Aegean / Coptic epact / Rumi / Brahmi / Sinhala archaic 1–10, `🄁 🄂`–`🄊`, counting-rod units and Mende Kikakui 1–9; the Greek pair not — user), grouping, the fallback to the token `1` (unsupported tokens, out-of-range numbers); `check_attribute` (the permitted values of `lang`, `letter-value`, `grouping-separator`, `grouping-size`) |
| `src/format_number.rs` | §16.4 `format-number`, **pure** (M3d): `DecimalFormat` (the ten §16.4.1 variables and their defaults; XTSE1300's distinct picture characters, XTSE1295's zero digit), `Picture::parse` (§16.4.2's rules → the XTDE1310 message, §16.4.3's variables per sub-picture), `Picture::format` (§16.4.4) over a `Numeric` — `xs:double` / `xs:float` (×100 / ×1000 in the value's own type, then the shortest round-trip digits, `{:e}`) or an `xs:decimal` / `xs:integer` as its exact lexical digits — rounded half-to-even on the decimal digits. Readings decided where the text and the catalog differ: the suffix is the passive characters after the last active one of the whole sub-picture (string-121 `00%`). Where the text and practice differ, the literal XSLT 2.0 text is kept (lead decision): zero with a decimal-separator-sign and no zero-digit-sign (`format-number(0, '#.#')`, `'#,###.##'`) is the **empty string** — §16.4.3 sets the minimum-integer-part-size to one only "if the sub-picture contains no zero-digit-sign and no decimal-separator-sign", §16.4.4: "(The value zero will at this stage be represented by a decimal-separator-sign on its own.)", and that separator is then removed. F&O 3.1 §4.7.4 later sets the minimum-fractional-part-size to 1 when both minimums are zero (`.0`); Saxon 9.1 gives `0`; no catalog case decides it (`format_number::tests::zero_without_a_zero_digit_is_the_empty_string_in_xslt_2_0`) |
| `src/format_date.rs` | §16.5 `format-date` / `-time` / `-dateTime`, **pure** (M3d): the picture (`[[`, `]]`, markers, whitespace ignored, XTDE1340), components and their availability (XTDE1350), presentation modifiers through `numbering::FormatToken::format_in_width` (decimal families, `A a I i W w Ww`, the sequences, `o`; `t` changes nothing in English), `N n Nn` names (English months, days, `am`/`pm` — `a.m.` when a minimum width of 4 asks, never padded, date-064 —, `AD`/`BC`, `AD`/`ISO`, a few timezone abbreviations), width modifiers (§16.5.1, in characters — a token's own width such as `01` counts digits, as for `xsl:number`, so `[D01o]` is `03rd` and `[D1o,2]` `3rd`; a minimum above the maximum is XTDE1340; the year drops high-order digits, leaving room for an ordinal suffix; fractional seconds rounded half-to-even, XSLT 2.0, a carry out of the first digit dropped (`.96` with `[f,1-1]` is `0`, Saxon 9.1); a name longer than the maximum is abbreviated — three letters when that is no shorter than the minimum, else right-truncated to the maximum (`[FNn,4-4]` `Tues`) — and never padded; a full representation shorter than the minimum is padded, with zero digits for numbers and spaces otherwise), timezones (`±HH:MM`; `[z,2-2]` / `[z0]` drop zero minutes), ISO weeks for `W` / `w` / `F` in both calendars, years as XML Schema 1.0 reads them (no year zero: `-0055` is 55 BC, as the upstream arithmetic agrees); a lexical `0000`, which upstream accepts, is 1 BC as XML Schema 1.1 reads it (`Y` 1 and `E` BC in the `AD` calendar, like `-0001`; the `ISO` calendar writes the lexical year). The §16.5.2 fallbacks: another language → `[Language: en]`, another valid calendar (a designator of the table, or any prefixed name) → `[Calendar: AD]`; an invalid calendar is XTDE1340 (F&O 3.0's FOFD1340) |
| `src/compile/decimal_format.rs` | `xsl:decimal-format` (§16.4.1): values recorded per attribute with their import precedence, merged attribute by attribute after the declaration pass (XTSE1290 for two values at the top precedence), then XTSE1295 / XTSE1300 on the merged format; `char` attributes untrimmed, one character (XTSE0020). `CompiledStylesheet::decimal_format(name)` |
| `src/functions.rs` | The §16 function library as an XPath catalog, plus the `use-when` evaluator. No stub is left since M3d (`stubs` holds only §3.8's fallback error function) |
| `src/collation.rs`, `src/collation/uca.rs`, `src/collation/uca/search.rs` | The collation registry (`CollationResolver`): codepoint, HTML ASCII case-insensitive, host-registered; `CaseOrderCollation` for `xsl:sort lang` / `case-order`; with the `icu-collation` feature the F&O 3.1 §5.3.3 UCA URIs and locale-tailored sorting on ICU4X 1.5 (`uca.rs`) and the substring search under them (`uca/search.rs`) |
| `src/schema.rs` | The schema-aware mode (M4a; M4 plan D-1, D-7, D-9): `SchemaContext` (caller-owned; owns the `SchemaSet`, whose `NameTable` is the stylesheet's) and `SchemaContextBuilder` — a **pre-scan** of the module graph with the compiler's own `Loader` (schema-aware `use-when`), §3.14's highest-precedence `xsl:import-schema` per namespace (`plan`), loaded by location through the compiler's `Resolver` (`ResolverLoader`; a `file:` URI is handed to `xsd-schema` as its path — `schema_location`, the one spelling of the context's locations in diagnostics — and turned back into a `file:` URI for the resolver; since `wave-b` `98ab11a` (C15a) `xsd-schema` resolves every directive by RFC 3986, normalizes it and identifies a document however it is spelled, so `ResolverLoader::request` passes a URI with a scheme on as it is (its own dot-segment removal went in the Wave B follow-up) and a relative location on relative, never absolutized against the working directory), by namespace only (the embedded `xml.xsd` / `xlink.xsd`, also the fallback when a location of those namespaces cannot be read; the built-in `xsi:` declarations; the Schema for Schemas is not available, so an import of the XML Schema namespace is not `found()`), or inline as a **slice** of the module text with the inherited namespace bindings injected after `<xs:schema`, the elements §3.12 `use-when` excludes blanked out byte for byte (`blank_excluded`), and the location `<base URI>#schema-N` (N from 1 per module; a module reached twice loads its inline schemas once, `DeclarationId`; errors mapped back to the module line). A document that cannot be read is no error (§3.14); one that is not a valid schema, or has another target namespace than the declaration's, is XTSE0220; XTSE0215 for an inline schema with `schema-location` or a conflicting `namespace`; XTSE0010 for content other than one `xs:schema`. `check_coverage`: `Compiler::schema_aware` refuses a stylesheet whose imports the context did not load (`XSLT2-INTERNAL`) — a namespace-only import is satisfied by a context that holds the namespace's components (`SchemaContext::covers`). `in_scope_type` / `built_in_type` / `has_user_constructor`: the in-scope types for XTSE1520 / XTSE1530, `type-available` and `function-available` (`functions::TypeScope`). **Host schemas** (M4b, §3.13's implementation-defined mechanism, D-2): `SchemaContextBuilder::schema_uri` / `schema_file` / `schema_str`, loaded before the imports into the same set (a namespace-only import of their namespace is `found()`, a location import of the same document loads it once; unreadable or invalid → XTSE0220), listed by `SchemaContext::host_schemas`. **XML Schema version** (M4g, D-3): `SchemaContextBuilder::xsd_version` (default 1.0; 1.1 = `SchemaSet::with_version(V1_1)` for every document of the context), `SchemaContext::xsd_version` |
| `src/regex.rs` | §15 regular expressions: `regexml`'s XPath 2.0 dialect (`Regex::xpath20`, the constructor `xsd-schema`'s `fn:matches` uses too), XTDE1140 / XTDE1145 / XTDE1150 by `ErrorKind`, and the engine's 64-level nesting limit as `XSLT2-INTERNAL` |

## Build

```bash
cargo build
cargo clippy --all-targets         # 0 warnings expected
cargo test                         # unit tests + every tests/*.rs except the W3C harness (it skips itself in debug): m0b…m2a, m2c, m2d_sorting, m2e_functions, m2f_keys, m2g_grouping, m2g_review, m2h_regex, m2i_core_defects, m2l_pattern_predicates, ns_inherit, q33_depth, …
                                   # + fn_error + m2b_namespaces + m2c_static_checks + m3a_serialization + m3a_review + m3a_review2
                                   # + m3b_result_documents + m3b_review + m3b_review2 + m3c_numbering + m3c_followup
                                   # + m3d_formatting + m3e_compat + m4a_schema_loading + m4a_review
                                   # + m4b_typed_sources + m4b_review + m4c_typed_patterns
                                   # + m4d_construction_validation + m4e_copies_documents
                                   # + m4f_type_lax + batch_t_followup + batch_t_namespaces
                                   # + m4g_xsd11_residue + wave_b_followup
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
gone.) `--xsd11` (M4g) satisfies `XSD_1.1` and builds every schema context as XML Schema 1.1 (`engine::context_version`,
which also gives a 1.1 context to a case whose context schemas declare `xsd-version="1.1"`). Run it beside the default
run and diff each against its own baseline (the first schema-aware one is
`../../baselines/xslts-sa-5794-at-xsd-27180f2-xslt2-8340257.xml`, the first `--xsd11` one
`../../baselines/xslts-sa11-6301-at-xsd-7608d3a-xslt2-6c6f9ba.xml`):

```bash
cargo test --release --test xslts -- -s ../../xslt30-test --skip unicode-90 --schema-aware --report /tmp/sa.xml
cargo test --release --test xslts -- -s ../../xslt30-test --skip unicode-90 --schema-aware --xsd11 --report /tmp/sa11.xml
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
An `assert-serialization` without `method` is compared under the method the stylesheet used; an expected-result
file that is not UTF-8 is decoded by its BOM or declared encoding (`resolver::decode_xml`; `validation-0201`'s
ISO-8859-1 `schvalid001.out`, M4f), a UTF-8 one as before.
Secondary results (`xsl:result-document`) are kept the same way in `RunOutput::secondary`, each under the
absolute URI it was written to, with the same serialization-failure policy; the adapter sets the base output
URI to the test-set directory (with a trailing slash), and `assert-result-document`'s relative `uri` matches
the tail of that absolute URI at a path-segment boundary. An `assert` over a secondary result sees its URI as
the re-parsed document's base URI (`base-uri()`, `result-document-0102`).
`assert-string-value` normalizes unless `normalize-space="false"` (the catalog's `default="true"`), with
XML whitespace only. `assert-true` / `-false` / `-count` / `-type` / `-deep-eq` / `-permutation` need a
raw sequence result, which an XSLT 2.0 run never delivers: `harnessError`.

**Exclusions are decided last** (M4g, `filter::applicability`): a case whose dependencies the configuration does
not satisfy stays not run, so a schema-aware case on `EXCLUDED` is "not run" in a basic run and "excluded" only in
the schema-aware runs (the 64 basic exclusions are unaffected: the default run is identical case by case). M4g added
13 schema-aware entries, each a decision for the user (`../../XSLT2_M4G_REPORT.md`, part 2; a `--schema-aware` run
counts 12 of them as excluded: `strip-space-009` needs `XSD_1.1`, so without `--xsd11` it is not run). **`assert-serialization`
under indentation** (M4g, `compare::indented_serializations_equal`): with the `xhtml` or `html` method and
`indent="yes"` in effect (`ResultDocument::indent`) two serializations are equal when they differ only in whitespace
Serialization 1.0 §6.1.3 / §7.4.3 lets an indenter add or remove — markup identical, formatted elements (`pre`,
`script`, `style`, `textarea`) exact; whitespace may be added or removed only where it touches the start or end tag of
an element that is not inline (`INLINE`: XHTML 1.0 / HTML 4.01 `%inline` with `script`, XHTML 1.1 `ruby`), or lies
outside the document element; everywhere else (next to an inline element's tag, a comment, a PI, a CDATA section, or
inside text) a whitespace run must stay a whitespace run (fix round 1, review R1/R2); the catalog lets a driver "ignore
differences in the serialization that are known to be irrelevant (that is, capable of being produced by a conformant
implementation)" (`validation-0201`). **The implicit timezone** (M4g fix round 1, review R12; the Wave B follow-up):
UTC, the catalog's assumption ("we currently assume that an absent timezone means UTC", `date-012` … `date-018`,
`date-042`) — every case's transformation gets `TransformOptions::implicit_timezone` UTC (`engine::implicit_timezone`),
and `driver::main` still sets `TZ=UTC` before the worker thread starts, as a guard for the XPath the option does not
reach, which reads the host's offset: the harness's own `assert` and parameter evaluation (the XPath engine outside a
transformation), XML Schema 1.1 assertions and type alternatives evaluated during validation (upstream C103), and
`use-when`. `--host-timezone` keeps the host's for both; `--self-test` checks both pins. Baselines taken before
2026-10-25 ran under CEST (+02:00): `date-012` fails there, passes under UTC.

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
absent one) and takes effect when the instruction is **evaluated**; `strip` everywhere, `preserve` and `strict` on
element constructors, `strict` / `lax` / `type` on `xsl:attribute` and the implicit final result tree under
`default-validation` run since M4d, `preserve` and `strict` on copies, `xsl:document` and `xsl:result-document` since
M4e, `type` and `lax` on every constructor since M4f (below), the XPath 2.0 types of §3.13 (`xs:untypedAtomic`,
`xs:anyAtomicType`, `xs:dayTimeDuration`, `xs:yearMonthDuration`) in `type` in both XML Schema versions since the Wave B
follow-up (upstream `SchemaSet::xpath_type`, C72 / C92; M4g had a workaround and two fences, "XML Schema 1.1 and the
XPath 2.0 types" below). No construct is fenced any more but M2k's UCA collations without their feature. (M4a's
`validation_fence` / `Validation::owner` table went with M4f.) (Typed patterns were fenced with M4c when the
pattern compiled, until M4c.) A stylesheet function with the name and arity 1 of a user
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
does it since `66e3e3f`, C66); whitespace text in element-only content is not in the tree (XDM 1.0 §6.7.4 — the typed
build's rule since `d5f2a8e`, C67; this crate's own drop went in the batch (t) follow-up, behaviour unchanged:
`tests/batch_t_followup.rs`, `tests/m4b_typed_sources.rs`); defaulted attributes are attribute nodes and a valid empty
element that takes its `default` / `fixed` value has it as its text node (XDM 1.0 §6.2.4; C39, `d5f2a8e`); an element
whose typed value is a single `xs:ID` is in the id index (C62) and elements and attributes whose typed value holds an
`xs:IDREF` have the is-idrefs property, which `fn:idref` reads (C54). An invalid document is an error with the
loader's code for an unparsable one (FODC0002 for `doc()` / `document()`, F&O 1.0 §15.5.4; XTDE1170 for the principal
source), the message naming the first errors and their nodes (B8 instance paths); `Lax` with an undeclared root is
XML Schema 1.1 §5.2's lax wildcard validation (upstream C36 `ValidationRoot::Lax`, M4f: no error for the root, its
declared descendants validated; the typed build types it `xs:anyType` when a descendant or attribute was assessed and
leaves it `xs:untyped` otherwise — XDM 1.0 §3.3.1.1's "partial" and "none", as for every lax-wildcard element of a
typed source: upstream C85, `wave-b` `7608d3a`); a basic processor asked to validate is XTDE1665. A typed document atomizes to typed
values and an untyped one exactly as before (one flow, `has_type_annotations`): keys, sort keys and grouping keys see
typed values. `input-type-annotations="strip"` in any module (§4.3) strips every source tree at admission — untyped,
nilled false, is-id and is-idrefs unchanged ("The values of the is-id and is-idrefs properties are not changed": the
ids an element answers to by its attributes and by its own typed value re-registered on the copy with
`register_xml_id`, the own one read off the source's id index for an element whose binding can make it an ID —
`receiver::kept_of_element`; every is-idrefs element and attribute marked with `register_idrefs`; batch (t)
follow-up) —, before §4.4's whitespace stripping, which spares the children of an element with a simple type or
simple content; the typed admission copy keeps nilled from the PSVI (`xsi:nil`), comments and PIs in a nilled element
included. **Defaults are kept** (§4.3's Note: "any defaulted elements and attributes that were added to the tree by
the validation process will still be present"): they are nodes of the typed tree since C39, so the strip copy copies
them, and §4.4 strips a defaulted element's whitespace text only once `strip` took its annotation (`validation-0701`
needs the defaulted `select` of the schema for XSLT 2.0; `tests/m4b_review.rs` F3, pinned to the old behaviour until
`d5f2a8e`, now checks them). A tree the host built and inserted
(`DocumentStore::insert`) is admitted (`admit_stored`, a private `StoredDocument::admitted` mark) wherever it enters —
`transform_document` or `doc()` / `document()` of its URI: taken as it is when typed against the context's set,
stripped under `strip`, stripped with a warning (`Transformation::take_warnings`) when typed against another set
(D-2); on a basic processor XTDE1665 unless `strip` (§21.1); not whitespace-stripped (as for an untyped one). A stylesheet parameter typed against another
set is `XSLT2-INTERNAL`. Known upstream limit met here: `fn:resolve-uri` refuses a relative reference with a space
(C63). Fixed on `wave-b` `66e3e3f`: QName / NOTATION in the value space (C33), `schema-for-xslt20.xsd` (C53), the
normalized string value (C66); fixed on `wave-b` `d5f2a8e` (batch (t)): defaults as nodes (C39), whitespace in
element-only content (C67), a union-typed node's typed value of its member type (C79), elements in the id index (C62),
`fn:idref` and is-idrefs (C54). Cost (release, a 5.1 MB source of 40,000
records, `load_document_with`, median of 5, on `wave-b` `66e3e3f`): untyped 45 ms (113 MB/s), validated and typed
126–130 ms (40 MB/s, ×2.8; the normalized string values of C66 cost about 3 %, the PSVI typed values left off save
about 10 %); with `xsl:strip-space` 124 ms untyped / 154–156 ms typed; validated under
`input-type-annotations="strip"` 153–158 ms. Schema-aware run on `66e3e3f`: 5,960 of 6,355, profile 217 of 579.
Batch (t) follow-up (`d5f2a8e`; `../../XSLT2_BATCH_T_FOLLOWUP_REPORT.md`): a 10,000-row validated source (1.08 MB,
`load_document_with`, median of 21, three rounds) 31.0–31.7 ms before and 31.1–32.1 ms after the host's whitespace
drop went (no cost, no gain: the typed build drops the same text); under `input-type-annotations="strip"` 37.5–38.0 →
38.2–38.3 ms (keeping element ids and is-idrefs on the copy: a binding check per element, a flag read per node).
Schema-aware run: 6,304 of 6,355, profile 561 of 579.

**Construction validation** (M4d; M4 plan D-5, D-10, D-12, D-13; `tests/m4d_construction_validation.rs`,
`../../XSLT2_M4D_REPORT.md`). `runtime/validate.rs` validates what `xsl:attribute` constructs: `type` → the type
(`get_built_in_type_by_qname` / `lookup_type`, and `SchemaSet::xpath_type` for the XPath 2.0 types of §3.13 in either
XML Schema version — `validate::resolve_type`; `xs:untypedAtomic` is the unvalidated annotation), XTTE1545 for a type
built from `xs:QName` /
`xs:NOTATION` (`namespace_sensitive`, the walk of upstream's private `type_has_qname_values`), `validate_simple_type`
→ XTTE1540; `strict` → the top-level declaration (`lookup_attribute`; none → XTTE1512), its type (none →
`xs:anySimpleType`), XTTE1545, `validate_simple_type` and the `fixed` value (`fixed_values_equal`, XML Schema 1.1
§3.2.4.1 clause 4) → XTTE1510; the binding is the type (and the declaration for `strict`), the string value the
normalized value; `strip` / `preserve` → untyped; `lax` (decision D-d1) as `strict` with a declaration (XTTE1515),
untyped without one. **Boundaries**: every element / document constructor announces its validation with
`Receiver::validation_boundary` (`receiver::Boundary::{Strip, Preserve, Strict}`; `Lax`, `Type`, `Untyped` since
M4f) just before `start_element` / `start_document` — literal result elements and `xsl:element`
(`Transformation::element_boundary`), `xsl:copy` and `xsl:document` (`strip`; the rest M4e), `xsl:result-document` (`strip`), temporary trees (`preserve`,
§9.4 / §19.2.2), the implicit final result tree (the principal module's `default-validation`, §3.6; M4a's M4e fence on
`preserve` is gone, decision D-d3). The normalizer passes a boundary on with the element's start tag, and a document
node opened in element content just before its first child (never, if it stays empty — an attribute may follow it).
**`Validating`** (the validating receiver, D-5) sits between the normalizer and every `ResultBuilder` of the
transformation's tree pipelines (`Transformation::validating`; `SequenceCollector` groups through `with_validator`,
the collectors made by `Transformation::collector`) and applies §19.2.1.1's rule — the outermost enclosing boundary
that is not `preserve` decides (`Decider`): under `strip` a node is untyped and not nilled, is-id and is-idrefs as it had
them (`ResultBuilder::keep_attribute`: an `xs:ID` attribute files its element under the value, `file_id`, duplicates
ignored, an `xs:IDREF` one keeps is-idrefs; `keep_open_element`, just before an element ends: its own id and
is-idrefs) — by the typed values of the outermost run inside the boundary (made with the node's in-scope namespaces;
`kept_of_info`), for a `preserve` copy (`Carry::All`, the only nodes that arrive annotated without a run) its
source's (`attribute_copied_from` / `element_copied_from`, read only then — `receiver::kept_of_attribute` /
`kept_of_element`), and for an attribute an instruction validated by its type, which holds no QName (XTTE1545;
`kept_of_binding`) — batch (t) follow-up and its fix round 1; under `strict` it
has what that boundary's validation gave it (`xs:anyType` / untyped where the validation assessed nothing); with only
`preserve` boundaries a new element is `xs:anyType` (mixed content) and every other node keeps the binding it arrives
with. A **`strict` element boundary** is one `SchemaValidator::start_run` (the run's one validator, made only when the
stylesheet names `strict`, `lax` or a `type` somewhere — `CompiledStylesheet::validates`, D-12), rooted at the
element, XTTE1512 first when `lookup_element` finds no top-level declaration; every event of its subtree goes to every
open run in the parsed-document driver's order (`validate_element` with the in-scope namespaces as a snapshot and
`xsi:type` / `xsi:nil`, `validate_attribute` …, `validate_end_of_attributes`, `validate_text` / `validate_whitespace`,
`validate_end_element`), the start tag held until its last attribute and a text node until it ends, only while a run
is open; nilled from the PSVI. **Values** (§19.2.1.3's serialize–parse–validate equivalence; the fix round of
`../../XSLT2_M4D_REPORT.md`): the runs open at a node apply to it innermost first, each to what the inner ones made of
it (`Validating::through_runs`; §19.2.1.1: "the validation requested for a child … is applied before the validation
requested for its parent") — an attribute's value becomes its normalized value, defaulted attributes from
`get_default_attributes` (the value constraint's {lexical form}, normalized by the type, XML Schema 1.1
§3.4.5.1 / §3.2.2.1; an outer run validates them as attributes; a prefix found in scope or declared; each QName /
NOTATION in it — `DefaultAttribute::typed_value`, the {value} read with the schema element's bindings — written with
a prefix the element binds to its namespace, upstream's order D-f3: the schema's own prefix where the element binds it
the same, unprefixed under a matching default namespace, another bound prefix, else a declared `ns_dN`; a QName in no
namespace keeps its lexical form, D-f2; `Validating::qname_lexical`; each run's namespace snapshot is taken after the
runs inside it declared theirs, so an outer run reads the value as written — batch (t) follow-up fix round 1), the
text of an element a run assessed with simple content its normalized value or, empty, the declaration's default /
fixed value (a QName in it written with a prefix in scope at the element from the run's typed value — the {value};
with none in scope a prefix declared on the element, whose start tag is held for that — only such an element's, read
with upstream's `element_value_constraint_for_type` (C89), "XML Schema 1.1 and the XPath 2.0 types" below)
(the content held until the element ends,
comments and PIs kept in order; `disable-output-escaping` kept only where the text did not change), whitespace text in
element-only / empty content no text node (XDM 1.0 §6.7.4, `validate::element_only` — the typed source build's rule
too, upstream's own there since `d5f2a8e`, C67). The tree gets what the outermost run made of each node, whatever decides the annotations — under `strip` too
(§19.2.1.1: strip changes the typed value "to be the same as its string value", not the string value). A document node
in element content (`xsl:document`, `xsl:copy` of a document node) is transparent for this (§5.7.1: replaced by its
children): its text and children are the enclosing element's content; under `strip`, is-id and is-idrefs are what the
outermost run inside the boundary gave the node. When the root ends,
`end_validation`, and any error but `cvc-id.1` / `cvc-id.2` (§19.2.1.3: no "Validation Root Valid (ID/IDREF)"; C38 is
the optional upstream flag) is XTTE1510 — inner runs end first, so the innermost failure is reported (D-10);
identity constraints on. Trees with no schema set (a basic processor) pass straight through. In a schema-aware run
every tree is built with the context's set (`DocumentStore::with_schema_set`) and `append_item` copies carry the
source's PSVI (`Annotations::of_copies`), so a typed node sequenced into a temporary tree keeps its annotation, nilled
and is-id; `append_copy` (`xsl:copy-of`, `xsl:copy`) copies untyped. Simple content (§5.7.2, `runtime/content.rs`
`SimpleContentCollector`): an element or document node built in the content of `xsl:value-of`, `xsl:attribute`,
`xsl:comment`, `xsl:processing-instruction` or `xsl:namespace` goes through the same pipeline (so a `strict` one is
validated) and contributes its **typed value**, each atomic value cast to a string (a list type's items joined by the
separator; element-only content FOTY0012) — as an existing node sequenced into the content does (the same atomization
as through `select`) and a parentless attribute `xsl:attribute` validated (its type's value), while a copy contributes
its string value (untyped); any item that is not a text node separates the text around it, even one that atomizes to
nothing (a nilled element, an empty list); an `xsl:message`'s document is built through `Validating` too (decision
D-d11 reversed in the fix round). Fixed upstream on `wave-b` `cfe061e` (batch (s)): the embedded `xml.xsd` is the
current W3C edition (`xml:id`, an empty `xml:lang` — `attribute-1501`, `-1504`, C71), and `get_default_attributes` reports
every defaulted attribute with its type (`DefaultAttribute::type_key`; local declarations, groups, inherited uses, C74).
Harness: an `assert` over a primary result tree that has type annotations is evaluated on the
tree itself while it exists (`engine.rs` → `compare::typed_asserts`, `ResultDocument::typed_asserts`; the catalog
evaluates `assert` "with the document node of the result document as the context item"). Cost (release, 10,000 rows
of three typed children, transform only, median of 7): no validation 23 ms, `strict` on the whole table 60–61 ms, on
every row 65–66 ms (a run costs about 0.5 µs to start; the fix round's chained values about 0.6 µs a row); a basic
stylesheet's tree building 1–4 % slower than before M4d (48.6–49.6 ms against 47.6–48.3) — accepted by the user, a
performance item. Schema-aware run on `66e3e3f`: 6,115 of
6,355, profile 372 of 579.

**Copies and documents** (M4e; M4 plan D-2, D-5, D-10; `tests/m4e_copies_documents.rs`, `../../XSLT2_M4E_REPORT.md`).
`xsl:copy-of` applies its validation to each element, attribute and document node it selects (§11.9.2) —
`runtime/validate.rs` `copy_validation` → `receiver::CopyValidation`, passed with `Receiver::append_copy` to
`ComplexContentNormalizer::copy_selected`: `strip` copies untyped **in the normalizer** (`Carry::Ids`: no binding, nilled
false), so that a `strip` copy is untyped whichever receivers build it — the public pipeline of a normalizer over a bare
`ResultBuilder` too (fix round) —, and keeps is-id and is-idrefs (§19.2.1.1: "The values of the is-id and is-idrefs
properties are unchanged"; §11.9.1 for a copied attribute), read off the source tree (which upstream classified
with the node's in-scope namespaces), never classified again: an attribute that is an `xs:ID` in its source
(`receiver::kept_of_attribute`: a binding that can make it an ID, `may_hold_id`, and its element — the physical one,
a parentless attribute's holder too — answering to its value in the source's id index) is followed by
`Receiver::element_id`, an element that is one by its own typed value (`receiver::kept_of_element`: its string value
looked up in the source's id index, only for a binding that can make it an ID; upstream C62) sends `element_id` after
its attributes,
and a node with the is-idrefs property (a node flag, `DomNavigator::is_idrefs`; upstream C54) `attribute_idrefs` /
`element_idrefs` (batch (t) follow-up) — `ResultBuilder` files them (`file_id`, `register_idrefs`) and `Validating`
passes them on unless runs are open (their assessment decides); the copy is no boundary; `preserve` copies every binding and nilled with no boundary (an
enclosing `strip` / `strict` still decides; each copied node names its source — `attribute_copied_from` /
`element_copied_from` — so that an enclosing `strip` keeps the is-id and is-idrefs the source has, read only then;
a node without a binding keeps its is-idrefs flag at once); `strict` puts a `Boundary::Strict` on the copy of an element (element
validation, M4d's run) and opens the copy of a document node as a document with `Boundary::Strict`; an attribute with
`strict` is validated by `Transformation::copy_attribute_node` through `validate_attribute` (XTTE1512 / 1510 / 1545) and
passed on as an attribute. `xsl:copy` of an element announces `element_boundary` (`preserve` → `xs:anyType`), of a
document node `document_boundary`, of an attribute `copy_attribute_node`. XTTE0950
(`check_namespace_sensitive_copy`, before a `preserve` copy): a node "has namespace-sensitive content if its typed
value contains an item of type xs:QName or xs:NOTATION" — read off the typed value's items; checked for an attribute
copied on its own whatever `copy-namespaces` says, and with `copy-namespaces="no"` for every element and attribute
`xsl:copy-of` copies (only the node itself for `xsl:copy`). A `preserve` copy of a node typed against another schema
set is `XSLT2-INTERNAL` (`check_same_schema_set`; unreachable through the public API, D-2). Copies landing in a
sequence (`SequenceCollector::push_copy`) and in simple content (`SimpleContentCollector::append_copy`: `preserve` →
the original's typed value, `strict` → built and validated, then its typed value) go through `Validating` too.
**Document-level validation** (§19.2.2): a document node opened with `Boundary::Strict` (`xsl:document`,
`xsl:result-document`, a strict copy of a document node; in element content too) gets a `DocumentCheck` in
`Validating`: a text child or a second element child is XTTE1550 at once, no element child at its end (an empty one
in element content never reaches `Validating`: the normalizer raises it, `receiver::xtte1550`); its element child is
the root of a run of its own (`Run::document`, outside the element's own `strict` run), XTTE1512 without a top-level
declaration, the annotations decided by the outermost non-`preserve` boundary as everywhere — where nothing outside
decides, the document's frame decides (`Decider::Document`) for everything that opens inside it up to its element child
(a `strip` document node or copy in between does not take its place, fix round), and its run from there; the text,
comments and processing instructions next to such a document node are not its children (the normalizer passes pending
text on before it opens a document node, fix round: §5.7.1 replaces the document node by its children only after it
was constructed and validated); that run keeps
`cvc-id.1` / `cvc-id.2` — XTTE1555 — and any other error is XTTE1510; its verdict (and a deferred XTTE1512) waits on
the document's frame until the document ends, after the structure rule (`end_run`, `end_document`); consecutive
document frames of document content count as one (`document_parents`). Not validated as documents: variables' trees
(§9.4, `preserve`), `xsl:message` documents (§19.2.2; an element validated inside one still is), the implicit final
result tree (`default-validation` is `preserve` or `strip`). A defaulted attribute in the XML namespace takes the
prefix `xml` (`finish_start_tag`; was an invented `ns_dN`, which made `validation-0202`'s output unparseable). Cost
(release, a validated 10,000-row source, transform only, median of 7): `copy-of` of the table into a temporary tree
`strip` 39 ms (35 ms before M4e: keeping is-id costs about 0.36 µs per `xs:ID` attribute — `Validating::id_value`, which
validates again only a type that can hold an ID, `may_hold_id`; a tree without IDs copies as fast as before),
`preserve` 42 ms (nilled read off the typed value, upstream P21), `strict` 71 ms; `xsl:result-document` of the same
table `strip` / `preserve` 38–39 ms, `strict` (document level) 70–71 ms, about what `strict` on the table as an
element costs. Schema-aware run on `87f9b49`: 6,163 of 6,355, profile 420 of 579.

**Type-driven and lax validation** (M4f; upstream C36; `tests/m4f_type_lax.rs`, `../../XSLT2_M4F_REPORT.md`).
`Transformation::{element_boundary, document_boundary, copy_validation}` (`runtime/validate.rs`) map `[xsl:]type` and
`lax` to `receiver::Boundary::{Type(TypeKey), Lax, Untyped}` / `CopyValidation::{Type, Lax, Untyped}` (the type
resolved by `resolve_validation_type`; for `xsl:copy-of` at the first element, attribute or document node it copies —
§11.9.2: "These attributes are ignored when copying an item that is not an element, attribute or document node", so
`xsl:copy` of a text node and atomic items take no validation). In `Validating` each `strict` / `lax` / `type`
boundary is one run whose root is assessed as its `RunMode` says, through `ValidationRuntime::set_validation_root`:
`Strict` → `ValidationRoot::Declaration` after the XTTE1512 check, `Lax` → `ValidationRoot::Lax` (no declaration: lax
assessment, no error; the run's **lax region** — the undeclared root and, recursively, every undeclared child of an
element in it, "validated, again with lax validation", a per-run stack `Run::lax_region` pushed in `finish_start_tag`
and popped in `end_element` — gets no `xsi:type` / `xsi:nil`: §19.2.1.1's note, "the presence of an xsi:type attribute
will not of itself cause an element to be validated", decision D-f2 as revised in M4f's fix round 1; a declared
element ends the region, and below it XML Schema decides), `Type(T)` →
`ValidationRoot::Type(T)` (§19.2.1.2's "processor-stipulated type definition": an `xsi:type` overrides only when
validly derived and not blocked, an abstract type needs a concrete `xsi:type`, never nilled); a run's errors are
XTTE1510 / XTTE1515 / XTTE1540 by mode (`end_run`), `cvc-id.*` filtered as for `strict`; what a run assessed nothing of
is `xs:anyType` / `xs:untypedAtomic` (§19.2.1.1: "Any nodes whose validation outcome is a validity property of
notKnown are annotated as xs:anyType"). A document node with `lax` or `type` is document-level validation
(`DocumentCheck::mode`; §19.2.2 "when validation is set to strict or lax, or when a type attribute is present"), its
element child the root of a run of that mode (XTTE1540 / XTTE1515 for its invalidity, XTTE1555 for ID/IDREF).
`type="xs:untyped"` (`Boundary::Untyped`, decision D-f3) is `strip` on an element and document-level validation with
an untyped element child on a document node. Attributes copied with `lax` / `type` are validated by
`copy_attribute_node` (`attribute_against_type`: String Valid; a complex type is XTTE1540, decision D-f4 — XSLT 3.0's
XTTE1535 does not exist in 2.0). Copies with `lax` / `type` in simple content are built and validated, contributing
their typed value. Cost (release, transform only, median of 7, three rounds; `../../XSLT2_M4F_REPORT.md`): 10,000 rows
with no validation 24–25 ms; `type` on the table 60–63 ms (`strict` 61–65 ms), on every row 68 ms (`strict` 68 ms);
`lax` over an undeclared 10,000-row tree 57 ms (one run), 62–63 ms (every row a lax root) — 49 / 56–57 ms before
fix round 1's top-level-declaration lookup per region element (about 0.2 µs an element). Schema-aware run on
`bde3377`: 6,280 of 6,355, profile 537 of 579.

**Implementation-defined choices of the schema-aware mode** (written up in M4g; `../../XSLT2_M4G_REPORT.md` part 2):
§3.13's mechanisms are the host schemas (`SchemaContextBuilder::schema_uri` / `schema_file` / `schema_str`, one set
with the imports, D-2) and the XML Schema 1.1 option (below); the whole set is in scope (D-7), so §3.14's
"These rules do not cause names to be imported transitively" is not enforced (a name of the set is usable without an
`xsl:import-schema` of its namespace); a location import whose document has another target namespace than the
declaration is XTSE0220 unless the set already holds a schema document of that namespace (a host schema's, or an
earlier import's), when it is a hint passed over (`found()` false; decision D-g11, XML Schema 1.1 §4.2.6.2: "it is
open to applications to ignore all but the first <import> for a given namespace"; `import-schema-186`, `-187`) — so
order-dependent (an earlier import counts only when it was loaded first), and a document an earlier import's own
`xs:import` brings in does not count (directives are resolved after every declaration); each location passed over, or
not readable, is a `SchemaContext::warnings` entry (fix round 1, review R8). In a 1.1 context the in-scope types add
Part 2 1.1's `xs:dateTimeStamp` (`schema::XSD11_PART_2_ADDITIONS`, with its constructor function), not Structures'
`xs:error` (review R6/R7);
source validation is the host's choice per loader and document, default `skip`, stylesheet modules read by
`document('')` not validated by the default (D-4, D-V), and in the harness a source with no `validation` attribute is
`strict` when its environment has a `source-reference` schema (D-M). In an XML Schema 1.1 context `xs:anyURI` is
1.1's for XTDE0835 / XTDE0865 / XTDE0905 (`eval::check_namespace_uri`: no at-most-one-`#` rule; `error-0905b`).

**XML Schema 1.1 and the XPath 2.0 types** (M4g; M4 plan D-3; `tests/m4g_xsd11_residue.rs`, `../../XSLT2_M4G_REPORT.md`;
the Wave B follow-up, `tests/wave_b_followup.rs`, `../../XSLT2_WAVE_B_FOLLOWUP_REPORT.md`).
`SchemaContextBuilder::xsd_version(XsdVersion::V1_1)` (`xslt2::XsdVersion` is upstream's; default 1.0 — XSLT 2.0's
normative reference is XML Schema 1.0) builds the context's one set with `SchemaSet::with_version` (`Loading::new`):
every schema document of the context is read as XML Schema 1.1, and the run's validators (`typed_sources::
validation_flags`: the source validator `Transformation::validator`, and `construction_validator` for constructed trees)
add `ValidationFlags::PROCESS_ASSERTIONS` for a 1.1 set (upstream B9: inline fragment buffering, through the push API
too) — §3.13's implementation-defined mechanisms. `SchemaContext::xsd_version`, `SchemaContext::with_version`. Harness:
`--schema-aware --xsd11` satisfies `XSD_1.1` and builds every context as 1.1; without `--xsd11` a case whose context
schemas declare `xsd-version="1.1"` gets a 1.1 context too (`engine::context_version`; today only `strip-space-009`,
which needs `XSD_1.1` anyway); its baseline is `../../baselines/xslts-sa11-6301-at-xsd-7608d3a-xslt2-6c6f9ba.xml` (the
untouched M4g tree, 1.0 contexts).
- **The XPath 2.0 types** (upstream C72 / C92, `wave-b` `5d00750`): `[xsl:]type="xs:…"` resolves through
  `validate::resolve_type` — the set's by-name lookup, else `SchemaSet::xpath_type`, which has `xs:anyAtomicType`,
  `xs:untypedAtomic`, `xs:dayTimeDuration` and `xs:yearMonthDuration` in a 1.0 set too and `xs:untypedAtomic` in a 1.1
  set (whose schema documents cannot name it) — to an ordinary `Boundary::Type` / `CopyValidation::Type`: validated
  against the type, annotated with it, an attribute typed `xs:untypedAtomic` unbound (D-d7). Constructed trees are
  validated by `Transformation::construction_validator`, the source validator's flags plus
  `ValidationFlags::XPATH_TYPES`, so an `xsi:type` in them may name the four too (the derivation is upstream's: under
  `xs:anyAtomicType`, `xs:anySimpleType` is `cvc-elt.4.2` → XTTE1540, the other three govern); a source is validated
  without the flag (decision FU-1: XML Schema's verdicts for sources; the content models are built once more when a
  run validates both). M4g's workaround — `receiver::XPathType`, `Boundary/CopyValidation::XPathType`, `RunMode::XPath`,
  `Named`, the duration fence (`DurationPolicy`), the C92 fence and the C72 warnings — went in the Wave B follow-up;
  `validation-0108` passes in the 1.0 schema-aware run.
- **A QName element default that needs a namespace declaration** (`Frame::deferred`, `Tag`,
  `Validating::{finish_start_tag, needs_a_new_prefix, start_in_run, emit_start, complete_deferred, end_deferred}`,
  `tag_frame`, `Ending::declare`): `finish_start_tag` hands the start tag to the runs innermost first (`start_in_run`)
  and reads, for each, the value constraint of the element's declaration under the run's governing type
  (`SchemaSet::element_value_constraint_for_type`, upstream C89 `9b2be85`, kept per transformation in
  `validate::ValueConstraints`); when its value holds a QName / NOTATION in a namespace that neither a prefix nor the
  default namespace of the element's written scope binds (`needs_a_new_prefix`), it stops: the runs outside it and the
  builder do not see the start tag yet. At the element's end (`end_deferred`) each run, innermost first, sees the start
  tag if it has not, then the held text and the end (`run_text`); a run that applies the default writes it with
  `qname_lexical(…, None)`, declaring `ns_dN` on the element (`prefix_for` on `tag_frame` / the innermost frame); then
  `emit_start`, the content, the end. An element child (`start_element`) completes the start first
  (`complete_deferred`) and releases the held content. Every other start tag goes on at once (M4g held every element
  a run gave a QName-capable value constraint). Why still a hold (decision FU-2): declaring the prefix on every such
  start tag at once would leave a namespace node on an element that then gets content, and §5.7.3 says "a namespace
  node must not be added to the tree unless the namespace node is necessary". The text an empty element takes is the
  value constraint's {lexical form} (`View::Simple::constraint`). An `XSLT2-INTERNAL` guards the case the hold should
  have caught. Cost: none measurable (M4d's 10,000-row table with a QName-default child per row 98.7–101.6 ms, before
  98.7–100.8 ms; `../../XSLT2_WAVE_B_FOLLOWUP_REPORT.md`). A 1.1 context validates a source as fast as a 1.0 one
  (31–34 ms for 10,000 rows); an `xs:assert` per row costs about 8 µs a row.
- **The implicit timezone** (upstream C94, `wave-b` `be5fe3d`): `TransformOptions::implicit_timezone` (`xslt2::
  TimezoneOffset`, upstream's; -14:00 … +14:00, else FODT0003 when the run starts), or the host's offset, read once:
  `Transformation::clock` takes the current dateTime from the engine with that timezone, and every dynamic context the
  run builds gets both (`set_clock`; `eval_expr`, `apply_probe`); the processor's own comparisons use
  `Transformation::implicit_timezone` (`sort.rs` `value_lt`, `keys.rs` `placement_of` / `key_values_equal`,
  grouping) through `eval_binary_with_timezone`. The `xs:g*` types share one key slot per type, confirmed by `eq`
  (upstream compares them by starting instant with the implicit timezone since batch (x), C5; the component slot
  missed equal values).
- **Base URIs** (upstream C7, batch (x): `DomNavigator::base_uri` is unresolved, the consumer resolves):
  `stylesheet::node_base_uri` is XDM's `base-uri` — the `xml:base` chain resolved against the document's base URI, and
  for a parentless element with its own `xml:base` against the static base URI, as upstream's `fn:base-uri` does —
  used by `document()` for a node argument and `$base-node` (it read the raw `xml:base` before) and by `xsl:copy`; a
  parentless `xsl:copy-of` of a document node, or of an element without its own `xml:base`, takes the original's base
  URI (`SequenceCollector::push_copy`), and a parentless literal result element or `xsl:element` its instruction's
  (`Receiver::element_base_uri`, §11.1 / §11.2) — so `base-uri()` answers the same in every module. Not yet: a
  parentless element with a relative `xml:base` of its own, whose base `fn:base-uri` resolves against the asking
  expression's static base URI (upstream; proposed row after C117).
Schema-aware run on `9b2be85` (the Wave B follow-up, `TZ=UTC` and the option): 6,313 of 6,343 runnable (76 excluded),
profile 567 of 567; `--xsd11` 6,312 of 6,342, profile 569 of 569; default 5,762, icu 5,764 (baselines
`../../baselines/*-at-xsd-9b2be85-xslt2-fu-uncommitted.xml`).
**Typed patterns** (M4c; M4 plan D-1, D-13; `tests/m4c_typed_patterns.rs`). A pattern step may use `element(*, T)`,
`element(N, T)`, `element(N, T?)`, `attribute(*, T)`, `attribute(N, T)`, `schema-element(N)`, `schema-attribute(N)`
and `document-node(…)` around an element test, in every pattern (`match` of templates and keys, `xsl:number count` /
`from`, `group-starting-with` / `-ending-with`). The whole pattern is compiled as an XPath expression first, so the
binder's XPST0008 (an undeclared TypeName or schema-test name, upstream C10) comes before the pattern is read; then
`pattern::parse` asks its `PatternSink` for the resolved test (`type_test` / `schema_test`, `compile/patterns.rs`):
a TypeName in the XML Schema namespace is `TypeTest::Untyped` or `BuiltIn(code)`, any other the set's `TypeKey`, names
interned in the stylesheet's name table (the set's own, D-1) — the binder's own resolution rule. **Matching is
`xsd-schema`'s** (`ItemType::matches_item` in `CompiledStylesheet::match_context`, the matcher `instance of` runs):
derives-from over the annotation (none = `xs:untyped` / `xs:untypedAtomic`), the nilled rule (`T?`; for
`schema-element` the declaration's `nillable`), the substitution group; this crate decides only axis and node kind
(`runtime/patterns.rs` `node_test_matches`) — no derivation code here. §6.4: `element(*,T)` / `attribute(*,T)` 0,
the named forms and `schema-element` / `schema-attribute` 0.25, `?` ignored, `document-node(E)` = `E`'s. Dispatch
index (DESIGN §8.5): `element(N,T)` under `N`, `attribute(N,T)` / `schema-attribute(N)` under `N`, `element(*,T)` /
`attribute(*,T)` in `other` (tried for every node, like `*[…]`), **`schema-element(N)` under `N` and every global
element declaration in its substitution group, transitively** (`IndexKey::Elements`; the member list is computed at
compile time from the set's public tables — `namespaces[ns].elements`, `resolved_substitution_groups` — and only
selects candidates; upstream still decides). Cost (release, 100,000 validated elements of 200 names, median of 5,
transform only): name patterns 55–76 ms; 200 `element(eᵢ, tᵢ)` 69–75 ms and 200 `schema-element(eᵢ)` 74–91 ms (keyed,
independent of the rule count); one `schema-element(h)` over a 200-member group 85–89 ms; 200 `element(*, tᵢ)`
1.9 s (~95 ns per node and rule, linear in both; 1.2 s with `tᵢ?` — upstream reads the nilled property by computing
the typed value, proposed P21), against 3.0–3.6 s for the same rules written `*[self::eᵢ]` / `*[. instance of
element(*, tᵢ)]`. Fixed upstream on `wave-b` `cfe061e` (C69): XPath's derives-from no longer counts a member type of a union as
derived from the union (XPath 2.0 §2.5.4: restriction / extension only; Structures' *Type Derivation OK (Simple)*
clause 2.2.4 stays validation's rule), so `element(*, U)` no longer matches an element typed by a member of `U` —
`match-197` passes. `document-node(E)` in a pattern needs exactly one element child and no text child, comments and
PIs allowed (XPath §2.5.4.2; since M4g — it tested the first element child only, `import-schema-055`). Schema-aware run on `66e3e3f`: 6,017 of 6,355, profile 274 of 579.

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
