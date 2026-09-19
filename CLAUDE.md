# CLAUDE.md - xslt2

Schema-aware XSLT 2.0 processor on top of `../xsd-schema` (XPath 2.0 engine, XDM values,
PSVI, `BufferDocument`). Separate git repository.

## Source of truth

- `../../xslt20.html` — W3C XSLT 2.0 (Second Edition). Cite sections as §N.N and quote the text.
- `../../XSLT2_DESIGN_SKETCH.md` — architecture, roadmap (M0–M5), open questions Q1–Q43.
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
| `src/resolver.rs` | `Resolver` (file, in-memory), URI arithmetic, non-UTF-8 decoding |
| `src/stylesheet.rs` | §3.10 module graph, import precedence, §5.4.1 lexical static contexts, §4.2 whitespace |
| `src/model.rs` | The `Instruction` tree (§7.4), declaration tables (§7.3), `CompiledStylesheet` |
| `src/pattern.rs` | Pattern grammar (§5.5.2), the §6.4 default priorities, the dispatch-index key |
| `src/compile/` | `Compiler`: modules → declarations and instructions. `mod.rs` (builder core), `declarations`, `instructions`, `literal`, `expressions`, `avt`, `patterns`, `modes`, `attributes`, `tree`. Walk stylesheet children **only** through `Module::children()` / `Builder::children()` — it applies §3.12 `use-when` exclusion by construction |
| `src/runtime/` | `Transformation`: `mod.rs` (entry points), `context` (§5.4), `dispatch` (§6), `patterns`, `eval` (instructions), `content`, `variables`, `documents` (loading, §4.4 admission), `functions` (the §16 evaluator), `expressions`, `serialize` |
| `src/receiver.rs` | `Receiver`, `ComplexContentNormalizer` (§5.7.1), `ResultBuilder`, `SequenceCollector` |
| `src/store.rs` | `DocumentStore`: arena-allocated documents, URI identity, `generate-id()` |
| `src/output.rs` | `xsl:output` merged by import precedence → `SerializeOptions` |
| `src/functions.rs` | The §16 function library as an XPath catalog, plus the `use-when` evaluator |

## Build

```bash
cargo build
cargo clippy --all-targets         # 0 warnings expected
cargo test                         # unit + m0b_invariants + m1_context + m1_smoke + m1_review + m2a_groundwork + m2a_review
                                   # (the W3C harness skips itself in a debug build)
```

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
cargo test --test xslts --release -- --self-test        # the harness's own checks (15)
cargo test --test xslts --release -- --lenient-xml      # triage: ignore prefixes/namespaces in assert-xml
cargo test --test xslts --release -- --report /tmp/x.xml
cargo test --test xslts --release -- -l                 # list test-sets with counts
```

Save verbose output to a file and grep it:

```bash
cargo test --test xslts --release -- -v > /tmp/xslts.txt 2>&1
grep -E "^  (FAIL|WRONG-ERROR|HARNESS-ERROR|NOT-IMPL)" /tmp/xslts.txt
```

`NOT-IMPL` is this processor's own `XSLT2-NOTIMPL` code: the case is runnable and fails, but it
fails because a feature belongs to a later milestone, not because the answer is wrong. Dependency
policy lives in `tests/xslts/filter.rs` (`dependency_supported`); the exclusion list (`EXCLUDED`)
must justify every entry. `tests/xslts/engine.rs` is the adapter, and it uses only the public API.

## Running one stylesheet by hand

```rust
let names = NameTable::new();
let functions = XsltFunctions::new();
let sheet = Compiler::new(&names, &functions).compile_file(Path::new("a.xsl"))?;
let arena = bumpalo::Bump::new();
let run = sheet.transformation(&arena, TransformOptions::default());
let outcome = run.transform_uri("file:///…/in.xml")?;
println!("{}", sheet.serialize_primary(&outcome)?);
```

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

## Upstream (`../xsd-schema`)

`xslt2` depends on it by path. Upstream changes are **releases**: additive only until 0.3.0. After
touching `src/xpath/*` re-run the XQTS selection (8,047/8,047); after touching `src/document/*` or
`src/navigator/*` re-run both XSD suites and compare the totals.
