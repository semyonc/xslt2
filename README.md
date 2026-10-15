# xslt2

Schema-aware XSLT 2.0 processor in Rust, built on the
[`xsd-schema`](../xsd-schema) validator and its XPath 2.0 engine.

Status: **M3 — numbering, formatting, serialization, compatibility** (M1 and M2 done; M3a serialization, M3b result
documents, M3c numbering, M3d number and date formatting and M3e compatibility done). Stylesheets are loaded,
compiled and run: the module graph with import precedence, the lexical static contexts of §5.4.1,
template rules with the §6.4 priorities and conflict resolution, the built-in rules,
`xsl:apply-imports` and `xsl:next-match`, variables and parameters (tunnel included), attribute
value templates, attribute sets, the node constructors, `xsl:copy`/`xsl:copy-of`, `xsl:message`,
`xsl:sort` / `xsl:perform-sort` with collations, `xsl:function`, `xsl:key` with `key()` and the `id()` /
`key()` patterns, `xsl:for-each-group`, `xsl:analyze-string` with `regex-group()`, the XML, XHTML, HTML and text
output methods with character maps and `disable-output-escaping`, `xsl:result-document` (secondary results are
returned in memory, `Outcome::secondary`; nothing is written to disk), and `xsl:number` (every level, Unicode
decimal-digit families, seventeen sequences of consecutive Unicode characters — circled, parenthesized, `1.`-style
digits and the like —, English words and ordinals), `format-number` with `xsl:decimal-format` (exact
`xs:decimal` / `xs:integer` digits, the shortest round-trip digits of a double or float, round-half-to-even), and
`format-date`, `format-time` and `format-dateTime` (English names, the Gregorian `AD` and `ISO` calendars; any other
language or calendar falls back to these with a `[Language: en]` / `[Calendar: AD]` prefix, as §16.5.2 requires).
A `version="1.0"` stylesheet, module or element runs with the backwards-compatible behaviour of §3.8 (XPath 1.0
compatibility mode, first-item attribute value templates and `xsl:value-of`, string-compared keys, …), and when the
principal module's `xsl:stylesheet` says `version="1.0"` an XHTML result created implicitly is written with the
`xml` method (§20). A module or element with a version above 2.0 runs with the forwards-compatible behaviour of §3.9:
XSLT elements XSLT 2.0 does not allow at the top level are ignored with their content, unknown attributes are
ignored, an XSLT element XSLT 2.0 does not allow in a sequence constructor is replaced by its `xsl:fallback`
children (or is the static error without one), and an XPath static error in an expression or attribute value
template is reported only if the expression is evaluated — except a call to a function that is not available, which
stays static (§18.1.1), and errors in patterns. No extension instruction is implemented: one with `xsl:fallback`
children evaluates them, one without is XTDE1450 when — and only when — it is evaluated, and `element-available()`
answers false for it.

```rust
use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, TransformOptions, XsltFunctions};

let names = NameTable::new();
let functions = XsltFunctions::new();
let sheet = Compiler::new(&names, &functions).compile_file("report.xsl".as_ref())?;

let arena = bumpalo::Bump::new();
let run = sheet.transformation(&arena, TransformOptions::default());
let outcome = run.transform_uri("file:///tmp/in.xml")?;
println!("{}", sheet.serialize_primary(&outcome)?);
# Ok::<(), xslt2::XsltError>(())
```

**Deep recursion.** Recursive templates and stylesheet functions use the native stack. A run checks
how much stack it has used and stops with a depth error instead of overflowing. On a thread
whose stack size it cannot know — any thread the host started — it allows itself only
`OTHER_THREAD_STACK_LIMIT` (1 MiB): safe on a default 2 MiB thread, but enough for only about a hundred
nested template calls in a release build. For deep recursion, run the whole job (compile,
transform, serialize — the compiled stylesheet and the arena are `!Send`) on a large stack with
`xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || …)` (256 MiB), or set
`TransformOptions::stack_limit` for a thread whose stack size you know.

## Cargo features

`icu-collation` (off by default) adds ICU4X 1.5's `icu_collator` with its compiled CLDR data (UCA 15.1) —
the ICU4X line `regexml` already brings in, so nothing is built twice — and with it the UCA collations of
F&O 3.1 §5.3.3, `http://www.w3.org/2013/collation/UCA?lang=…;strength=…` and the other parameters
ICU4X 1.5 supports (all but `reorder`; `-u-` collation keywords in `lang` too), plus locale-tailored
`xsl:sort lang` / `case-order`. ICU4X 1.5 exposes no collation elements, so under a UCA collation
`contains()` and the other substring functions match collation-equal substrings cut at character
boundaries (a contraction such as Slovak `ch` can be split, an expansion such as `ß` cannot). The
search reads the collation data to tell which characters' weights cannot depend on their neighbours and
narrows the candidates with it, so ordinary text of 100,000 characters is searched in milliseconds; a long
run of characters that chain into contractions or prefix contexts (Danish `aaaa…`, Japanese `カーカー…`)
is searched candidate by candidate within a work budget of about a second, beyond which the function
raises FOCH0004 rather than answer late. `xsl:sort case-order` with a `lang` asking for primary or
secondary strength (`-u-ks-level1`, `-u-ks-level2`) switches on a case level, so case is ordered as asked
while accents stay as the language wants them. Without the feature the built-in collations are
codepoint and HTML ASCII case-insensitive (plus any the host registers), a UCA URI reports
`XSLT2-NOTIMPL`, and `xsl:sort lang` / `case-order` use a dependency-free case-order collation.

A parameter a UCA URI leaves out takes the language's default where CLDR tailors one: `UCA?lang=th`
ignores spaces and punctuation up to the tertiary level (Thai is `alternate=shifted`), `UCA?lang=fr-CA`
compares accents backwards, `UCA?lang=da` sorts upper case first.

**Enabling the feature changes existing results**, not only adds new ones: `xsl:sort lang="…"` or
`case-order` without a `collation` attribute sorts with the language's UCA tailoring instead of the
case-order collation — `('f', 'é', 'e')` with `lang="en"` sorts `e, f, é` without the feature and
`e, é, f` with it. Cargo unifies features across a build, so if any crate in a build enables
`icu-collation` on `xslt2`, every user of `xslt2` in that build gets it.

```bash
cargo test --features icu-collation
cargo test --release --features icu-collation --test xslts -- -s ../../xslt30-test --skip unicode-90
```

## Conformance

The XSLT 2.0 test suite (XSLTS 1.0.4) was never published; the public W3C XSLT 3.0 suite
(https://github.com/w3c/xslt30-test, checked out at `../../xslt30-test`) marks the cases an
XSLT 2.0 processor must run (`spec` = `XSLT20+`, `XSLT20`, `XSLT10+`, or absent).

Whole applicable suite, minus the 1,460-case `unicode-90` set (see below):

| | |
|---|---|
| XSLT 2.0-applicable | 6,619 |
| excluded (documented in `tests/xslts/filter.rs`) | 64 |
| runnable (dependencies satisfied) | 5,792 |
| pass | 5,755 |
| fail / wrongError / harnessError | 31 / 0 / 3 |
| not implemented (`XSLT2-NOTIMPL`: M2k without its feature, M4) | 3 |
| pass rate over runnable | 99.36% |
| headline (basic profile, non-regex families) | 4,280 / 4,313 = **99.2%** |

With `--features icu-collation` the two M2k cases pass too: 5,757.

`assert-xml` is compared the way the catalog defines it — Canonical-XML equivalence: element and
attribute prefixes and the in-scope namespaces of every element are significant unless the
assertion says `ignore-prefixes="true"`. `--lenient-xml` switches that off for triage; a failure
that exists only under the strict comparison starts with `assert-xml namespace mismatch:`
(`attribute-0004`, which pins an invented prefix that §5.7.3 leaves implementation-dependent, is excluded). Figures
were first taken on 2026-09-19, after the M1 review fixes (`../../XSLT2_M1_REVIEW_FIXES.md`) and the M2a groundwork
(`../../XSLT2_M2A_REPORT.md`), which re-labelled 114 cases from "fail" to "not implemented": they
need `format-number` / `format-date`, which were stubs until M3d. They are measured against the
sibling `../xsd-schema` checkout with its current additive XPath fixes (navigation, `/` evaluation,
operator errors, regex dialect, string literals, linear general comparison): 3,977 → 4,160; M2b
(namespace correctness) and M2c (one table-driven static-checking pass, `src/compile/checks.rs`) then
took it to 4,297, and two further upstream fixes (`fn:deep-equal` on namespace nodes, atomization of a
namespace node) to 4,299. The 0.2.1 release fixes, M2i (core defects in what M1 delivered,
`../../XSLT2_M2I_REPORT.md`) and M2j (harness fixes and documented exclusions, `../../XSLT2_M2J_REPORT.md`)
took it to 4,369 of 5,725 on 2026-10-04: 26 cases whose expectation is wrong for an XSLT 2.0 processor are
excluded with the spec text that says so, and 3 are harness errors until the public API can start a
transformation at a node other than the document. M2d (sorting: `xsl:sort`, `xsl:perform-sort` and the collation
registry, `../../XSLT2_M2D_REPORT.md`) took it to 4,522, and M2e (`xsl:function`, `../../XSLT2_M2E_REPORT.md`; four
more documented exclusions) to 4,628, and M2f (`xsl:key`, `key()`, `id()` / `key()` patterns,
`../../XSLT2_M2F_REPORT.md`) to 4,786 of 5,721 on 2026-10-04; M2l (pattern predicates decided per candidate, a
performance change) kept it there, and M2g (`xsl:for-each-group`, `../../XSLT2_M2G_REPORT.md`; two more documented
exclusions) took it to 4,864 of 5,719 on 2026-10-08, and M2h (`xsl:analyze-string`, `regex-group()`,
`../../XSLT2_M2H_REPORT.md`) to 5,013 of 5,690 on 2026-10-10 — 29 `misc/regex-classes` general-category cases are
excluded because their reference results pin Unicode 3.1 / 5.2 / 6.0 and the regex backend implements Unicode 15.1.
On 2026-10-11 the harness began to evaluate assertions about the result tree as the catalog defines them —
on a serialization with the default parameters (method xml, indent no), not on the stylesheet's indented
output (`../../XSLT2_HARNESS_RESULT_TREE_REPORT.md`): 5,014 of 5,689 (`for-each-group-081a` passes;
`for-each-group-085` is excluded — its expected result drops source whitespace that §6.6 and §4.4 keep). M3a
(serialization: the `xml`, `xhtml`, `html` and `text` methods, character maps, `disable-output-escaping`,
`../../XSLT2_M3A_REPORT.md`) took it to 5,181 of 5,688 the same day (`character-map-026`, an XSLT 3.0 case, is
excluded), M3b (`xsl:result-document`, `../../XSLT2_M3B_REPORT.md`) to 5,286 on 2026-10-12, and M3c
(`xsl:number`, `../../XSLT2_M3C_REPORT.md`) to 5,455 on 2026-10-13, and its follow-up (seventeen numbering sequences of
consecutive code points; the harness now asks the processor about the `combinations_for_numbering` dependency and
its `range`, `../../XSLT2_M3C_FOLLOWUP_REPORT.md`) to 5,560 of 5,793 the same day. M3d (`format-number`,
`xsl:decimal-format`, `format-date` / `-time` / `-dateTime`, `../../XSLT2_M3D_REPORT.md`) took it to 5,747 on
2026-10-14: every one of the 187 cases the formatting functions held back passes (5,748 on `xsd-schema` 0.2.3). M3e
(backwards-compatible attribute value templates, adjacent text nodes merged in simple content, extension instructions
and forwards-compatible processing, `../../XSLT2_M3E_REPORT.md`) took it to 5,755 of 5,792 on 2026-10-15
(`next-match-039`, whose expected result is XSLT 3.0's, is excluded); every remaining failure has an owner in that
report.

The sixteen test-sets M1 targets:

| test-set | runnable | pass | fail | wrongErr | harnessErr | notImpl | rate |
|---|---:|---:|---:|---:|---:|---:|---:|
| template | 6 | 6 | 0 | 0 | 0 | 0 | 100.0% |
| built-in-templates | 4 | 4 | 0 | 0 | 0 | 0 | 100.0% |
| choose | 54 | 53 | 0 | 0 | 0 | 1 | 98.1% |
| call-template | 35 | 34 | 0 | 0 | 1 | 0 | 97.1% |
| lre | 26 | 26 | 0 | 0 | 0 | 0 | 100.0% |
| variable | 101 | 101 | 0 | 0 | 0 | 0 | 100.0% |
| include | 14 | 13 | 1 | 0 | 0 | 0 | 92.9% |
| apply-templates | 36 | 36 | 0 | 0 | 0 | 0 | 100.0% |
| element | 19 | 18 | 1 | 0 | 0 | 0 | 94.7% |
| version | 33 | 33 | 0 | 0 | 0 | 0 | 100.0% |
| param | 25 | 25 | 0 | 0 | 0 | 0 | 100.0% |
| attribute | 21 | 21 | 0 | 0 | 0 | 0 | 100.0% |
| copy | 70 | 68 | 2 | 0 | 0 | 0 | 97.1% |
| sequence | 87 | 87 | 0 | 0 | 0 | 0 | 100.0% |
| message | 26 | 24 | 2 | 0 | 0 | 0 | 92.3% |
| output | 137 | 137 | 0 | 0 | 0 | 0 | 100.0% |

`output` passes in full since M3a (serialization) and M3b (`xsl:result-document`).

```bash
cargo test --test xslts --release                       # whole applicable suite
cargo test --test xslts --release -- -g value-of -f     # one test-set, failures only
cargo test --test xslts --release -- --stub             # the M0 stub engine
cargo test --test xslts --release -- --report /tmp/x.xml
cargo test --test xslts --release -- -l                 # list test-sets and counts
cargo test --test xslts --release -- --self-test        # the harness's own checks
```

`unicode-90` is left out of the figures above with `--skip unicode-90`. Its 1,444 XSLT 2.0 cases check a
Unicode general category against every code point: `$validrange[not(. = $c)]` compares each of the
1.1 million legal code points with the category's code points read from a source of up to 51 MB. That
general comparison used to take 30–60 s per case; `xsd-schema` now builds the comparison index once and
reuses it across the evaluations of a loop-invariant operand (Wave A, A-5), and caches compiled regular
expressions (A-6), so 37 of the 38 `Ll` cases take 0.2–5 s each. The set is still skipped because of its size and one shape: the
cache lives for one XPath evaluation, and six of the 38 modes call `matches()` from an `xsl:if` inside an
`xsl:for-each` over up to 1.1 million code points — one evaluation, and so one compilation of the pattern, per
item. With the pattern of `fn-matches20` (four `\P{…}` escapes, two of them in classes, and a back-reference) that is about 60 µs per item,
90% of it compiling (`unicode90-Ll-020`: 64 s; the same million calls inside one expression, `unicode90-Ll-009`,
take 1 s). That is a performance limit, not a conformance answer (M3e report; Wave B P11). The other 16 cases use
XPath 3.0 syntax and are excluded. The set is in the `misc/regex` family, outside the headline denominator.

Dependency policy: `tests/xslts/filter.rs`. Assertion semantics: `tests/xslts/compare.rs`
(`assert` XPaths are evaluated with the `xsd-schema` engine over the re-parsed result). The
engine adapter is `tests/xslts/engine.rs`, and it uses only the public API.

## Design and reference material (workspace root, outside this repository)

- `xslt_doc/XSLT2_DESIGN.md` — the architecture (start here)
- `xslt_doc/XSLT2_ROADMAP.md` — current status, milestones M0–M5, working method, open questions Q1–Q43
- `xslt_doc/XSLT2_LOG.md` — review history; `XSLT2_DESIGN_SKETCH.md` is the frozen original these three replaced
- `XSLT2_M1_REPORT.md` — what M1 built, the classified failures, the upstream changes
- `XSLT2_FEASIBILITY.md` — the original feasibility memo
- `xslt20.html` — W3C XSLT 2.0 (Second Edition), the source of truth; cite sections as §N.N
