# xslt2

Schema-aware XSLT 2.0 processor in Rust, built on the
[`xsd-schema`](../xsd-schema) validator and its XPath 2.0 engine.

Status: **M1 — core transformation**. Stylesheets are loaded, compiled and run: the module
graph with import precedence, the lexical static contexts of §5.4.1, template rules with the
§6.4 priorities and conflict resolution, the built-in rules, `xsl:apply-imports` and
`xsl:next-match`, variables and parameters (tunnel included), attribute value templates,
attribute sets, the node constructors, `xsl:copy`/`xsl:copy-of`, `xsl:message`, and the XML and
text output methods. `xsl:sort`, `xsl:function`, `xsl:key`, `xsl:for-each-group`,
`xsl:analyze-string`, `xsl:number`, `format-number` and the HTML/XHTML output methods belong to
M2–M4 and report the internal `XSLT2-NOTIMPL` code rather than a wrong answer.

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

## Conformance

The XSLT 2.0 test suite (XSLTS 1.0.4) was never published; the public W3C XSLT 3.0 suite
(https://github.com/w3c/xslt30-test, checked out at `../../xslt30-test`) marks the cases an
XSLT 2.0 processor must run (`spec` = `XSLT20+`, `XSLT20`, `XSLT10+`, or absent).

Whole applicable suite, minus the 1,460-case `unicode-90` set (see below):

| | |
|---|---|
| XSLT 2.0-applicable | 6,619 |
| excluded (documented in `tests/xslts/filter.rs`) | 26 |
| runnable (dependencies satisfied) | 5,725 |
| pass | 4,522 |
| fail / wrongError / harnessError | 59 / 0 / 3 |
| not implemented (`XSLT2-NOTIMPL`, M2–M4) | 1,141 |
| pass rate over runnable | 78.99% |
| headline (basic profile, non-regex families) | 3,378 / 4,215 = **80.1%** |

`assert-xml` is compared the way the catalog defines it — Canonical-XML equivalence: element and
attribute prefixes and the in-scope namespaces of every element are significant unless the
assertion says `ignore-prefixes="true"`. `--lenient-xml` switches that off for triage; a failure
that exists only under the strict comparison starts with `assert-xml namespace mismatch:`
(`attribute-0004`, which pins an invented prefix that §5.7.3 leaves implementation-dependent, is excluded). Figures
were first taken on 2026-09-19, after the M1 review fixes (`../../XSLT2_M1_REVIEW_FIXES.md`) and the M2a groundwork
(`../../XSLT2_M2A_REPORT.md`), which re-labelled 114 cases from "fail" to "not implemented": they
need `format-number` / `format-date`, which are stubs until M3d. They are measured against the
sibling `../xsd-schema` checkout with its current additive XPath fixes (navigation, `/` evaluation,
operator errors, regex dialect, string literals, linear general comparison): 3,977 → 4,160; M2b
(namespace correctness) and M2c (one table-driven static-checking pass, `src/compile/checks.rs`) then
took it to 4,297, and two further upstream fixes (`fn:deep-equal` on namespace nodes, atomization of a
namespace node) to 4,299. The 0.2.1 release fixes, M2i (core defects in what M1 delivered,
`../../XSLT2_M2I_REPORT.md`) and M2j (harness fixes and documented exclusions, `../../XSLT2_M2J_REPORT.md`)
took it to 4,369 of 5,725 on 2026-10-04: 26 cases whose expectation is wrong for an XSLT 2.0 processor are
excluded with the spec text that says so, and 3 are harness errors until the public API can start a
transformation at a node other than the document. M2d (sorting: `xsl:sort`, `xsl:perform-sort` and the collation
registry, `../../XSLT2_M2D_REPORT.md`) took it to 4,522 on 2026-10-04.

The sixteen test-sets M1 targets:

| test-set | runnable | pass | fail | wrongErr | harnessErr | notImpl | rate |
|---|---:|---:|---:|---:|---:|---:|---:|
| template | 6 | 6 | 0 | 0 | 0 | 0 | 100.0% |
| built-in-templates | 4 | 4 | 0 | 0 | 0 | 0 | 100.0% |
| choose | 54 | 53 | 0 | 0 | 0 | 1 | 98.1% |
| call-template | 35 | 33 | 0 | 0 | 1 | 1 | 94.3% |
| lre | 26 | 26 | 0 | 0 | 0 | 0 | 100.0% |
| variable | 101 | 99 | 0 | 0 | 0 | 2 | 98.0% |
| include | 14 | 13 | 1 | 0 | 0 | 0 | 92.9% |
| apply-templates | 36 | 36 | 0 | 0 | 0 | 0 | 100.0% |
| element | 19 | 18 | 1 | 0 | 0 | 0 | 94.7% |
| version | 33 | 30 | 0 | 0 | 0 | 3 | 90.9% |
| param | 26 | 23 | 0 | 0 | 0 | 3 | 88.5% |
| attribute | 21 | 19 | 0 | 0 | 0 | 2 | 90.5% |
| copy | 72 | 62 | 2 | 0 | 0 | 8 | 86.1% |
| sequence | 87 | 82 | 0 | 0 | 0 | 5 | 94.3% |
| message | 26 | 22 | 2 | 0 | 0 | 2 | 84.6% |
| output | 137 | 10 | 21 | 0 | 0 | 106 | 7.3% |

`output` is dominated by the `html`, `xhtml`, `doctype-*` and character-map serialization
parameters, which are M3.

```bash
cargo test --test xslts --release                       # whole applicable suite
cargo test --test xslts --release -- -g value-of -f     # one test-set, failures only
cargo test --test xslts --release -- --stub             # the M0 stub engine
cargo test --test xslts --release -- --report /tmp/x.xml
cargo test --test xslts --release -- -l                 # list test-sets and counts
cargo test --test xslts --release -- --self-test        # the harness's own checks
```

`unicode-90` is left out of the figures above with `--skip unicode-90`: 1,444 of its cases evaluate
`$validrange[not(. = $c)]` — a general comparison against a large sequence, repeated about 1.1
million times over a 53 MB source — which takes 30–60 s per case. That is a performance limit,
not a conformance answer (it needs a comparison index cached across evaluations); the other 16
cases use XPath 3.0 syntax. The set is in the `misc/regex` family, outside the headline
denominator.

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
