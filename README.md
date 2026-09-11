# xslt2

Schema-aware XSLT 2.0 processor in Rust, built on the
[`xsd-schema`](../xsd-schema) validator and its XPath 2.0 engine.

Status: **M0** — the W3C conformance harness exists and runs the whole applicable suite
against a stub engine (every runnable case fails); the processor itself is not started.
Error codes from Appendix E of the spec are in `src/error/codes.rs`.

## Conformance harness

The XSLT 2.0 test suite (XSLTS 1.0.4) was never published; the public W3C XSLT 3.0 suite
(https://github.com/w3c/xslt30-test, checked out at `../../xslt30-test`) marks the cases an
XSLT 2.0 processor must run (`spec` = `XSLT20+`, `XSLT20`, `XSLT10+`, or absent).

| Catalog-referenced test-sets | 234 (26 more are commented out in `catalog.xml`; 29 files on disk are orphans) |
|---|---|
| Test cases | 14,601 |
| XSLT 2.0-applicable | 8,079 (584 need `schema_aware`; 6,924 need no optional feature) |
| Runnable in basic mode (dependencies satisfied) | 7,211 |
| Headline denominator (basic profile, non-regex families) | 4,233 |

```bash
cargo test --test xslts --release -- -s ../../xslt30-test            # whole applicable suite
cargo test --test xslts --release -- -g value-of -v                  # one test-set, per-case output
cargo test --test xslts --release -- --schema-aware                  # include schema_aware cases
cargo test --test xslts --release -- --report /tmp/xslts.xml         # W3C results XML
cargo test --test xslts --release -- -l                              # list test-sets and counts
cargo test --test xslts --release -- --self-test                     # the harness's own checks
```

Dependency policy: `tests/xslts/filter.rs`. Assertion semantics: `tests/xslts/compare.rs`
(`assert` XPaths are evaluated with the `xsd-schema` engine over the re-parsed result).

## Design and reference material (workspace root, outside this repository)

- `XSLT2_DESIGN_SKETCH.md` — architecture, roadmap M0–M5, open questions (start here)
- `XSLT2_FEASIBILITY.md` — the original feasibility memo
- `xslt20.html` — W3C XSLT 2.0 (Second Edition), the source of truth; cite sections as §N.N
