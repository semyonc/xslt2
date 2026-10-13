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
| `src/compile/` | `Compiler`: modules → declarations and instructions. `mod.rs` (builder core), `declarations`, `output` (`xsl:output`, `xsl:character-map`), `instructions`, `literal`, `expressions`, `avt`, `patterns`, `modes`, `attributes`, `tree`, `functions` (`xsl:function`), `keys` (`xsl:key`), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`), `result_document` (`xsl:result-document`: written attribute values, the static XTDE1460 / XTSE1590), `number` (`xsl:number`: XTSE0975, `level`, the `count` / `from` patterns, the conversion attributes — fixed values checked here, XTSE0020). Walk stylesheet children **only** through `Module::children()` / `Builder::children()` — it applies §3.12 `use-when` exclusion by construction |
| `src/runtime/` | `Transformation`: `mod.rs` (entry points), `context` (§5.4; the §19.1 output state is `XsltContext::final_output`: only the initial context is final, `empty()` contexts are temporary, and the listed constructs clear it for their content), `dispatch` (§6), `patterns`, `eval` (instructions), `content`, `variables`, `documents` (loading, §4.4 admission), `functions` (the §16 evaluator), `expressions`, `serialize` (a result tree through `src/serialize/`), `sort` (`xsl:sort`, `xsl:perform-sort`), `calls` (stylesheet functions), `keys` (key indexes), `grouping` (`xsl:for-each-group`), `analyze` (`xsl:analyze-string`, the run's regex cache), `results` (`xsl:result-document`, the base output URI, XTDE1480 / XTDE1490, the §2.4 primary result; `SecondaryResult`), `number` (`xsl:number`: §12.1 `value` with the backwards-compatible NaN rule, XTDE0980; §12.2 the place marker of a node — single / multiple / any, the implied count pattern, walks only up and forward because the navigator's previous-sibling move scans from the first child; XTTE0990 / XTTE1000; XTDE0030 for computed conversion attributes; the nodes before $S are counted by a `Walk` — the tree in document order for `any`, a parent's children for `single` / `multiple` — that counts every implied pattern at once without `count` (`Census`, by node kind and expanded name), keeps an index of the numbers it gave once a node before its frontier is asked for (`Index`), and is kept per instruction (`NumberMemo`; `Walks`, by owner: every tree and every parent — a walk past 32 nodes always, the shorter ones until more than 1,024 walks are kept) while the local variables the patterns read keep their values (`Stability`, compared exactly: nodes by identity, strings / booleans / integers / doubles by type and bits) — numbering is linear in the nodes walked and the evaluations, in any order, over any number of names, namespaces, trees and parents; a pattern calling a stylesheet function counts afresh every time, see "`xsl:number` cost" below) |
| `src/receiver.rs` | `Receiver`, `ComplexContentNormalizer` (§5.7.1), `ResultBuilder`, `SequenceCollector`, `TemporaryOutputState` (the content of `xsl:document`: d-o-e dropped, §11.5/§20.2) |
| `src/store.rs` | `DocumentStore`: arena-allocated documents, URI identity, `generate-id()` |
| `src/output.rs` | `OutputDefinition`: the `xsl:output` attributes merged by import precedence (§20), `cdata-section-elements` united, `use-character-maps` concatenated and expanded; `compile/output.rs` reads `xsl:output` / `xsl:character-map` (XTSE1560, XTSE1570, XTSE1580, XTSE1590, XTSE1600, XTSE0020 parameter domains) |
| `src/serialize/` | The serializer (M3a; Serialization 1.0 2E, read at w3.org): `params` (an output definition resolved against a tree — §20's default method, per-method defaults, SEPM/SESU checks), `markup` (the `xml`, `xhtml`, `html` methods, iterative), `html` (HTML 4.01 / XHTML tables, URI escaping), `encoding` (UTF-8, UTF-16, US-ASCII, ISO-8859-1; anything else SESU0007), `charmap`, `marks` (`disable-output-escaping` kept beside the tree, §20.2); the text method in `mod.rs`. The result is a `String` (the encoding phase skipped, Ser. §4); `serialize_primary_to_bytes` encodes |
| `src/numbering.rs` | §12.3 number to string conversion, **pure** (no XPath or tree types; M3d's `format-date` presentation modifiers are meant to reuse it): format tokenization (alphanumeric = L* and N* through `regexml`'s `\p{L}\p{N}`), decimal-digit tokens of every Unicode `Nd` family (a generated table of the 68 zeros) — both **Unicode 15.1**, `regexml`'s version, so a 16.0 digit or letter separates tokens (the table follows `regexml`: `numbering::tests::the_digit_table_is_the_unicode_version_of_the_regexes`), `A`/`a` (bijective base 26), `I`/`i` (1–3999), English `w`/`W`/`Ww` words and ordinals as Saxon 9.1 writes them (`Twenty-First`, cardinals unhyphenated; to `u64::MAX`), the 17 sequences of consecutive code points of the catalog's `combinations_for_numbering` (`SEQUENCES`: circled digits ⓪–㊿ over three blocks, `🄀 ⒈`–`⒛`, `⓿ ❶`–`⓴`, `🄋 ➀`–`➉`, `🄌 ➊`–`➓`, `⑴`–`⒇`, `⓵`–`⓾`, `㈠`–`㈩`, `㊀`–`㊉`, Aegean / Coptic epact / Rumi / Brahmi / Sinhala archaic 1–10, `🄁 🄂`–`🄊`, counting-rod units and Mende Kikakui 1–9; the Greek pair not — user), grouping, the fallback to the token `1` (unsupported tokens, out-of-range numbers); `check_attribute` (the permitted values of `lang`, `letter-value`, `grouping-separator`, `grouping-size`) |
| `src/functions.rs` | The §16 function library as an XPath catalog, plus the `use-when` evaluator |
| `src/collation.rs`, `src/collation/uca.rs`, `src/collation/uca/search.rs` | The collation registry (`CollationResolver`): codepoint, HTML ASCII case-insensitive, host-registered; `CaseOrderCollation` for `xsl:sort lang` / `case-order`; with the `icu-collation` feature the F&O 3.1 §5.3.3 UCA URIs and locale-tailored sorting on ICU4X 1.5 (`uca.rs`) and the substring search under them (`uca/search.rs`) |
| `src/regex.rs` | §15 regular expressions: the XPath 2.0 dialect check (a copy of `xsd-schema`'s private pre-pass) in front of `regexml`, and XTDE1140 / XTDE1145 / XTDE1150 |

## Build

```bash
cargo build
cargo clippy --all-targets         # 0 warnings expected
cargo test                         # unit tests + every tests/*.rs except the W3C harness (it skips itself in debug): m0b…m2a, m2c, m2d_sorting, m2e_functions, m2f_keys, m2g_grouping, m2g_review, m2h_regex, m2i_core_defects, m2l_pattern_predicates, ns_inherit, q33_depth, …
                                   # + fn_error + m2b_namespaces + m2c_static_checks + m3a_serialization + m3a_review + m3a_review2
                                   # + m3b_result_documents + m3b_review + m3b_review2 + m3c_numbering + m3c_followup
                                   # (tests/common/mod.rs: `common::body` drops the default XML
                                   # declaration for the tests written before M3a)
                                   # (the W3C harness skips itself in a debug build)
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps   # must exit 0; the local regexml fork's own
                                                 # rustc warning is a dependency's and does not fail it
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

A deeply recursive stylesheet needs `xslt2::run_on_large_stack(DEFAULT_STACK_SIZE, || …)` around
the whole job (compile included): everything involved is `!Send`, so the closure has to own it.

## Local `regexml` fork (optional, untracked)

`regexml` (third-party, comes in through `xsd-schema`) has a fork at `../regexml` for regex work
(`xsl:analyze-string`, category memoization). A developer machine may have an **untracked**
`.cargo/config.toml` here with `[patch.crates-io] regexml = { path = "../regexml/regexml" }`; it is
listed in `.git/info/exclude`. Check with `cargo tree -i regexml`. While it is active `Cargo.lock`
shows as modified (the `regexml` entry loses its registry source and checksum) — **never commit
`Cargo.lock` in that state**; stage files by name, not with `git add -A`/`-u`. To switch back,
delete the config and `git checkout Cargo.lock`.

A test that needs a fix only the fork has (so far: `m2h_regex`'s
`a_backtracked_alternative_does_not_erase_an_earlier_capture`, capture restoration on backtracking)
probes `regexml` on its own pattern first and, on the crates.io release, skips with an `eprintln!`
naming the fix instead of failing; the fork's own tests assert the fix unconditionally.

## Upstream (`../xsd-schema`)

`xslt2` depends on it by path. Upstream changes are **releases**: additive only until 0.3.0. After
touching `src/xpath/*` re-run the XQTS selection (8,047/8,047); after touching `src/document/*` or
`src/navigator/*` re-run both XSD suites and compare the totals.
