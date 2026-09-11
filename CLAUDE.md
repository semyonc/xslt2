# CLAUDE.md - xslt2

Schema-aware XSLT 2.0 processor on top of `../xsd-schema` (XPath 2.0 engine, XDM values,
PSVI, `BufferDocument`). Separate git repository.

## Source of truth

- `../../xslt20.html` — W3C XSLT 2.0 (Second Edition). Cite sections as §N.N and quote the text.
- `../../XSLT2_DESIGN_SKETCH.md` — architecture, roadmap (M0–M5), open questions Q1–Q36.
- `../../XSLT2_FEASIBILITY.md` — the original memo.
- Every error carries an Appendix E code: constants in `src/error/codes.rs` (generated from the spec).

## Build

```bash
cargo build
cargo clippy --all-targets
cargo test            # unit tests + the harness (skips itself when the suite is absent)
```

## W3C conformance harness (`tests/xslts`)

The XSLT 2.0 suite is W3C-member-only; the public XSLT 3.0 suite marks 2.0-applicable cases
(`spec` = `XSLT20+`, `XSLT20`, `XSLT10+`, or absent). It is checked out at `../../xslt30-test`.

**Always run in `--release`.** Save verbose output to a file and grep it:

```bash
cargo test --test xslts --release -- -s ../../xslt30-test
cargo test --test xslts --release -- -g value-of -v
cargo test --test xslts --release -- -v 2>&1 > /tmp/xslts.txt   # then grep FAIL / WRONG-ERROR / HARNESS-ERROR
cargo test --test xslts --release -- --schema-aware              # runs the 584 schema_aware cases too
cargo test --test xslts --release -- --report /tmp/xslts.xml     # W3C results format
cargo test --test xslts --release -- -l                          # list test-sets with counts
```

Dependency policy lives in `tests/xslts/filter.rs` (`dependency_supported`); the exclusion list
(`EXCLUDED`) must justify every entry. The headline number is the basic-profile, non-regex pass
rate; the two regex test-sets are 27% of the applicable cases and test `regexml`, not XSLT.
