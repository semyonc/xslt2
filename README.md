# xslt2

Schema-aware XSLT 2.0 processor in Rust, built on the
[`xsd-schema`](../xsd-schema) validator and its XPath 2.0 engine.

Status: **M4 — schema-aware**, in progress (M1–M3 done; M4a, schema loading and the schema-aware mode, M4b,
typed source documents, M4c, typed patterns, and M4d, validation of constructed elements and attributes, done — see
"Schema-aware mode" below). Stylesheets are loaded,
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

## Schema-aware mode

`Compiler::new` is a *basic* XSLT processor (§21.1: `xsl:import-schema` is XTSE1650, `[xsl:]type` and a
`[xsl:]validation` / `default-validation` other than `strip` are XTSE1660). A *schema-aware* compilation (§21.2) is
two steps: a caller-owned `SchemaContext`, built before the stylesheet compiles from a pre-scan of its
`xsl:import-schema` declarations (by location, by namespace only — the XML and XLink namespaces' schemas are built in
—, or inline), and `Compiler::schema_aware(&context, &functions)`, which compiles against the context's schema set
and its name table:

```rust
let functions = XsltFunctions::new();
let context = SchemaContext::builder()
    .with_resolver(resolver.clone())
    .stylesheet_file("report.xsl".as_ref(), &functions)?
    .build()?;                                   // XTSE0215, XTSE0220 (and the module errors) here
let sheet = Compiler::schema_aware(&context, &functions)
    .with_resolver(resolver)
    .compile_file("report.xsl".as_ref())?;
```

The imported types are in scope for every XPath expression and `as` attribute (`cast as`, `castable as`,
`instance of`, constructor functions of user-defined atomic types), `type-available()` and
`system-property('xsl:is-schema-aware')` answer as a schema-aware processor, and the static rules of `[xsl:]type`
and `[xsl:]validation` (XTSE1505, XTSE1520, XTSE1530, XTSE0020) apply.

**Typed source documents** (M4b). The host can add schemas of its own to the context (§3.13's implementation-defined
mechanism) and ask for source documents to be validated against the one schema set, as a default for every
source-tree loader or per document:

```rust
let context = SchemaContext::builder()
    .with_resolver(resolver.clone())
    .stylesheet_file("report.xsl".as_ref(), &functions)?
    .schema_file("orders.xsd".as_ref())          // a host schema, in scope for the stylesheet too
    .build()?;
let sheet = Compiler::schema_aware(&context, &functions).with_resolver(resolver).compile_file("report.xsl".as_ref())?;
let arena = bumpalo::Bump::new();
let options = TransformOptions { source_validation: SourceValidation::Strict, ..Default::default() };
let run = sheet.transformation(&arena, options);        // doc() / document() validate too
let outcome = run.transform_source_with("file:///in.xml", &xml, SourceValidation::Strict)?;
```

A validated document is parsed and typed in one pass; its nodes atomize to typed values (so keys, sort keys and
grouping keys compare typed values), whitespace in element-only content is not in the tree (XDM 1.0 §6.7.4), and an
invalid document is an error naming the offending nodes. `Skip` (the default) leaves documents untyped, exactly as
before. `input-type-annotations="strip"` strips the annotations of every source tree (§4.3; the ids attributes give
are kept), and `xsl:strip-space` spares elements with simple content (§4.4). A valid attribute or simple-content
element has its whitespace normalized as its type says (XDM 1.0 §6.2.4, §6.3.4). Not there yet, upstream: an element's
`default` / `fixed` value as a text node and defaulted attributes (so `strip` cannot keep them), and an element whose
own typed value is an `xs:ID` in the `id()` index. A stylesheet module read with `document('')` is not validated by
the transformation-wide default; `set_source_validation(uri, …)` sets the mode of any one URI.

**Typed patterns** (M4c). Every pattern — `match` of `xsl:template` and `xsl:key`, `count` / `from` of `xsl:number`,
`group-starting-with` / `group-ending-with` — may use the XPath 2.0 kind tests that name schema components:
`element(*, T)`, `element(N, T)`, `element(N, T?)`, `attribute(*, T)`, `attribute(N, T)`, `schema-element(N)` (which
matches the members of `N`'s substitution group too), `schema-attribute(N)`, and `document-node(…)` around an
element test. A node matches exactly when `instance of` with the same test is true (XPath 2.0 §2.5.4: the type
annotation must be the type or derive from it, a nilled element needs `T?`, an unvalidated element is `xs:untyped`
and an unvalidated attribute `xs:untypedAtomic`), and the default priorities are §6.4's (0 for `element(*, T)` /
`attribute(*, T)`, 0.25 for the forms with a name and for `schema-element` / `schema-attribute`; a `?` changes
nothing). A type or declaration the stylesheet's schemas do not have is XPST0008; a basic processor accepts the
built-in types (`element(*, xs:untyped)` matches every element of its untyped trees). Template rules of
`schema-element(N)` are indexed under every name of the substitution group, so dispatch stays a hash lookup; a rule
that matches by type only (`element(*, T)`) is tried for every element, like `*[…]`. Known upstream limit: a member
type of a union counts as derived from the union (`element(*, U)` matches an element of a member type of `U`,
proposed Wave B item C68).

**Constructed nodes** (M4d). `validation="strict"` on a literal result element or `xsl:element` validates the new
element against the top-level declaration of its name, through `xsd-schema`'s streaming validation as the tree is
built (§19.2.1.1): no such declaration is XTTE1512, an invalid element XTTE1510 (with nested validations, the innermost
failure is the one reported); the validation annotates the element and everything in it, creates defaulted attributes
and an empty element's default value, sets nilled, makes the text of a valid simple-content element and the value of
a valid attribute their normalized values, drops whitespace text in element-only content, and checks identity
constraints but not ID uniqueness or dangling IDREFs (§19.2.1.3). Nested validations run innermost first, each
validating what the ones inside it produced, and what they produced stays when the annotations go: `strip` around a
validated element removes its annotations, not its defaults or normalized values. An element constructed in the
content of `xsl:value-of`, `xsl:attribute`, `xsl:comment`, `xsl:processing-instruction` or `xsl:namespace`, or in an
`xsl:message`, is validated the same way, and contributes its typed value (§5.7.2). `type`, `validation="strict"`
and `validation="lax"` on `xsl:attribute` validate the new attribute (§19.2.1.1, §19.2.1.2): against the named type,
or against the top-level declaration of the attribute's name (its type and its `fixed` value; with `lax`, no
declaration means no validation); an invalid value is XTTE1540 / XTTE1510 / XTTE1515, a name with no top-level
declaration under `strict` XTTE1512, a type built from `xs:QName` or `xs:NOTATION` XTTE1545. The valid attribute is annotated with the type and its string
value is the normalized value. Whether an annotation stays is decided by the constructors around the node — the
outermost one whose validation is not `preserve` decides: under `strip` (the default) everything is untyped (an
`xs:ID` attribute still identifies its element for `id()`), under `preserve` a new element is `xs:anyType` and what
it contains keeps its annotations, under `strict` the validation's annotations stand. A variable's temporary tree is a
`preserve` document (typed nodes sequenced into it keep their annotations, nilled and ids), and `default-validation`
applies per module and, for the principal module, to the implicit final result tree. Upstream items it needed —
the current `xml.xsd` (C71) and the defaults of local attribute declarations (C74) — are fixed on `wave-b` `cfe061e`.

Not implemented yet, each raising `XSLT2-NOTIMPL` naming the sub-milestone (`../../xslt_doc/M4_PLAN.md` §4) when the
construct is **evaluated** — a stylesheet that never reaches one runs: `preserve` / `strict` on copies and document
nodes (M4e), and `type` on elements and `lax` on elements — and lax validation of a source whose root has no
declaration — (M4f). XML Schema 1.0 only (1.1 is M4g).

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
| pass | 5,759 |
| fail / wrongError / harnessError | 28 / 0 / 3 |
| not implemented (`XSLT2-NOTIMPL`: M2k without its feature) | 2 |
| pass rate over runnable | 99.43% |
| headline (basic profile, non-regex families) | 4,284 / 4,313 = **99.3%** |

With `--features icu-collation` the two M2k cases pass too: 5,761.

**Schema-aware run** (`--schema-aware`: the 584 cases that need a schema-aware processor become runnable and the 16
that need a basic one are not run; every case is compiled schema-aware, against the stylesheet's imports and the
environment's schemas, and every source is validated as the catalog says): 6,115 of 6,355 runnable pass, the
schema-aware profile 372 of 579 (M4d on `xsd-schema` `wave-b` `66e3e3f`, 2026-10-23; 274 after M4c, 217 after M4b,
120 after M4a, 51 before), and every basic case keeps its status; of the 175 not implemented, 173 are M4e's (62) and
M4f's (111) and 2 M2k's. An `assert` over a primary result tree that carries type annotations is evaluated on the tree
itself (the catalog: "with the document node of the result document as the context item"; a re-parse of the
serialization would be untyped), every other one over the re-parse.

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
report. `xsd-schema` Wave B batch (a) (the `wave-b` branch: `xs:QName` / `xs:NOTATION` values convert to their
lexical form) took it to 5,758 on 2026-10-18 (`element-0308`, `message-0306`, `type-0143`), and M4a (schema loading and
the schema-aware mode, `../../XSLT2_M4A_REPORT.md`) to 5,759 on 2026-10-20: `import-schema-191`, which expects a basic
processor to reject `xsl:import-schema` with XTSE1650.

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
