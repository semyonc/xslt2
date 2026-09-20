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
| runnable (dependencies satisfied) | 5,751 |
| pass | 4,160 |
| fail / wrongError / harnessError | 254 / 29 / 5 |
| not implemented (`XSLT2-NOTIMPL`, M2–M4) | 1,303 |
| pass rate over runnable | 72.34% |
| headline (basic profile, non-regex families) | 3,025 / 4,233 = **71.5%** |

`assert-xml` is compared the way the catalog defines it — Canonical-XML equivalence: element and
attribute prefixes and the in-scope namespaces of every element are significant unless the
assertion says `ignore-prefixes="true"`. `--lenient-xml` switches that off for triage; a failure
that exists only under the strict comparison starts with `assert-xml namespace mismatch:` (8 cases
today, all real namespace-fixup / inheritance / `xsl:namespace-alias` bugs). Figures are from
2026-09-19, after the M1 review fixes (`../../XSLT2_M1_REVIEW_FIXES.md`) and the M2a groundwork
(`../../XSLT2_M2A_REPORT.md`), which re-labelled 114 cases from "fail" to "not implemented": they
need `format-number` / `format-date`, which are stubs until M3d. They are measured against the
sibling `../xsd-schema` checkout with its current additive XPath fixes (navigation, `/` evaluation,
operator errors, regex dialect, string literals, linear general comparison): 3,977 → 4,160.

The sixteen test-sets M1 targets:

| test-set | runnable | pass | fail | wrongErr | notImpl | rate |
|---|---:|---:|---:|---:|---:|---:|
| template | 6 | 6 | 0 | 0 | 0 | 100.0% |
| built-in-templates | 4 | 4 | 0 | 0 | 0 | 100.0% |
| choose | 54 | 53 | 1 | 0 | 0 | 98.1% |
| call-template | 35 | 34 | 0 | 0 | 1 | 97.1% |
| lre | 26 | 25 | 1 | 0 | 0 | 96.2% |
| variable | 102 | 96 | 3 | 0 | 3 | 94.1% |
| include | 14 | 13 | 1 | 0 | 0 | 92.9% |
| apply-templates | 36 | 35 | 0 | 1 | 0 | 97.2% |
| element | 19 | 17 | 2 | 0 | 0 | 89.5% |
| version | 33 | 28 | 2 | 0 | 3 | 84.8% |
| param | 26 | 22 | 0 | 0 | 4 | 84.6% |
| attribute | 22 | 17 | 1 | 0 | 3 | 77.3% |
| copy | 72 | 58 | 4 | 0 | 10 | 80.6% |
| sequence | 88 | 70 | 11 | 0 | 7 | 79.5% |
| message | 26 | 20 | 3 | 0 | 3 | 76.9% |
| output | 140 | 9 | 24 | 0 | 107 | 6.4% |

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

- `XSLT2_DESIGN_SKETCH.md` — architecture, roadmap M0–M5, open questions (start here)
- `XSLT2_M1_REPORT.md` — what M1 built, the classified failures, the upstream changes
- `XSLT2_FEASIBILITY.md` — the original feasibility memo
- `xslt20.html` — W3C XSLT 2.0 (Second Edition), the source of truth; cite sections as §N.N
