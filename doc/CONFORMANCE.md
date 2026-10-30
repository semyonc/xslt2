# Conformance: what does not pass, and why

`xslt2` is measured with the cases of the public [W3C XSLT 3.0 test suite](https://github.com/w3c/xslt30-test)
that apply to an XSLT 2.0 processor (the XSLT 2.0 suite itself was never published). The figures for the four runs,
and what "runnable" and "excluded" mean, are in the README's
[Conformance](https://github.com/semyonc/xslt2/blob/main/README.md#conformance) section. In short, at suite commit
`fddf1cf`, without the `unicode-90` test-set:

| Run | Pass | Runnable | Fail | harnessError | notImplemented | Excluded |
|---|---:|---:|---:|---:|---:|---:|
| basic | 5,762 | 5,792 | 25 | 3 | 2 | 64 |
| basic, `icu-collation` | 5,764 | 5,792 | 25 | 3 | 0 | 64 |
| schema-aware | 6,313 | 6,343 | 25 | 3 | 2 | 76 |
| schema-aware, XML Schema 1.1 | 6,312 | 6,342 | 25 | 3 | 2 | 76 |

This document lists every case that does not pass and every case that is excluded, with the reason.

## The cases that do not pass

The same 30 cases do not pass in any of the four runs, except the two UCA collation cases, which pass with the
`icu-collation` feature. None of them needs a schema-aware processor.

| Class | Cases | Meaning |
|---|---:|---|
| known bug | 8 | the processor is wrong |
| not implemented | 18 | a feature this processor leaves out |
| harness limitation | 3 | the test harness cannot express the case through the public API |
| test environment | 1 | the case needs a network resource |
| disputed | 0 | the cases whose expected result contradicts XSLT 2.0 are excluded instead ([below](#excluded-cases)) |

| Case | Class | Reason |
|---|---|---|
| `construct-node-018` | known bug | [zero-length text nodes](#zero-length-text-nodes-known-bug-8-cases) |
| `construct-node-019` | known bug | zero-length text nodes |
| `construct-node-020` | known bug | zero-length text nodes |
| `function-1009` | known bug | zero-length text nodes |
| `match-249` | known bug | zero-length text nodes |
| `match-251` | known bug | zero-length text nodes |
| `message-0304` | known bug | zero-length text nodes |
| `select-2303` | known bug | zero-length text nodes |
| `copy-1201` | not implemented | [entities declared in a DTD](#entities-declared-in-a-dtd-not-implemented-8-cases): the stylesheet's external DTD |
| `copy-1202` | not implemented | entities declared in a DTD: the stylesheet's external DTD |
| `docbook-001` | not implemented | entities declared in a DTD: the DocBook stylesheets' internal subset |
| `docbook-002` | not implemented | entities declared in a DTD: the DocBook stylesheets' internal subset |
| `include-0101` | not implemented | entities declared in a DTD: an included module's external DTD |
| `resolve-uri-016` | not implemented | entities declared in a DTD: external parsed entities in the source |
| `resolve-uri-021` | not implemented | entities declared in a DTD: external parsed entities in the source |
| `select-6101` | not implemented | entities declared in a DTD: the source's internal subset |
| `id-015` | not implemented | [ID attributes declared in a DTD](#id-attributes-declared-in-a-dtd-not-implemented-6-cases) |
| `id-016` | not implemented | ID attributes declared in a DTD |
| `id-018` | not implemented | ID attributes declared in a DTD |
| `id-019` | not implemented | ID attributes declared in a DTD |
| `id-034` | not implemented | ID attributes declared in a DTD |
| `id-037` | not implemented | ID attributes declared in a DTD |
| `choose-0107` | not implemented | [UCA collations](#uca-collations-not-implemented-without-icu-collation-2-cases) without `icu-collation` |
| `for-each-group-086` | not implemented | UCA collations without `icu-collation` |
| `collection-004` | not implemented | [`collection()`](#collection-not-implemented-1-case) |
| `number-2506` | not implemented | [number words in German](#number-words-in-german-not-implemented-1-case) |
| `call-template-0201` | harness limitation | [an initial context node inside the document](#an-initial-context-node-inside-the-document-harness-limitation-3-cases) |
| `mode-1105` | harness limitation | an initial context node inside the document |
| `strip-space-023` | harness limitation | an initial context node inside the document |
| `unparsed-text-2003` | test environment | [a resource on the web](#a-resource-on-the-web-test-environment-1-case) |

### Zero-length text nodes (known bug, 8 cases)

XSLT 2.0 §11.4: "It is possible to construct text nodes whose string value is zero-length. A zero-length text node,
when atomized, produces a zero-length string." `<xsl:text/>` and `<xsl:value-of select="''"/>` make one. In content
(an element, an attribute, `xsl:value-of`) such nodes are dropped, as the specification says, and that works. But when
the sequence becomes a value — a variable or parameter with `as`, or the result of a stylesheet function — this
processor loses them: the document model it builds on cannot hold a text node with an empty value. So:

- `<xsl:variable as="xs:string"><xsl:value-of select="''"/></xsl:variable>` and the same with `as="text()"` raise
  XTTE0570, because the value is empty instead of one node (`construct-node-018`, `-019`, `-020`, `match-249`,
  `match-251`, `message-0304`, `select-2303`);
- `count(f())` of a function whose body is three such instructions is 0, not 3 (`function-1009`).

### Entities declared in a DTD (not implemented, 8 cases)

The XML parser reads a document type declaration but processes none of its declarations: it never reads an external
DTD subset or an external entity, and a reference to an entity other than the five predefined ones (`&amp;`, `&lt;`,
`&gt;`, `&apos;`, `&quot;`) makes the document not well-formed. These cases use such entities without declaring the
catalog's `dtd` dependency (the cases that declare it, 61 in a basic run, are not run):

- a source with an internal-subset entity (`select-6101`: `&eacute;`), or external parsed entities (`resolve-uri-016`,
  `-021`);
- a stylesheet module whose entities come from an external DTD (`include-0101`, `copy-1201`, `copy-1202`) or from its
  internal subset (`docbook-001`, `-002`, the DocBook XSL 1.79.1 stylesheets).

The source or the module cannot be read: XTDE1170 or XTSE0165, naming the undeclared entity. [Limits and untrusted
input](https://github.com/semyonc/xslt2/blob/main/doc/LIMITS.md#dtds-and-entities) says what this means for security.

### ID attributes declared in a DTD (not implemented, 6 cases)

The same parser does not apply the attribute types a DTD declares, so `<!ATTLIST a id ID #REQUIRED>` does not make
`id` an ID attribute, and `id()` finds nothing (`id-015`, `-016`, `-018`, `-019`, `-034`, `-037`). Like the cases above
they omit the `dtd` dependency that 35 other cases of the `id` test-set declare. `xml:id` attributes, and IDs typed by
a schema in schema-aware mode, are found.

### UCA collations (not implemented without `icu-collation`, 2 cases)

`choose-0107` and `for-each-group-086` name the collation `http://www.w3.org/2013/collation/UCA?strength=secondary`.
Without the `icu-collation` feature it is not available, and the processor raises its `XSLT2-NOTIMPL` error. With the
feature both pass.

### `collection()` (not implemented, 1 case)

`collection()` always returns the empty sequence: the API has no way to define a collection. `collection-004` expects
the three documents of a collection that the catalog defines.

### Number words in German (not implemented, 1 case)

`number-2506` formats numbers as words with `lang="de"` and expects German. `xsl:number` writes English words for
every language; XSLT 2.0 §12.3 allows that: "The set of languages for which numbering is supported is
implementation-defined. If a language is requested that is not supported, the processor uses the language that it
would use if the lang attribute were omitted." The case does not declare the catalog's `languages_for_numbering`
dependency.

### An initial context node inside the document (harness limitation, 3 cases)

`call-template-0201`, `mode-1105` and `strip-space-023` start the transformation at a node inside the source document
(`/doc`, `/a/b/text()`). The public API starts a transformation at a document node (`transform_source`,
`transform_uri`, `transform_document`) or with no context item (`transform_without_source`), so the harness reports
these cases as harness errors. XSLT 2.0 §2.3 leaves the interface open: "This document does not specify any
application programming interfaces or other interfaces for initiating a transformation."

### A resource on the web (test environment, 1 case)

`unparsed-text-2003` expects `unparsed-text-available('http://www.w3.org/Consortium/mission.html')` to be true. The
default resolver reads local files only and answers false; the other three results of the case are right. A host that
wants `http:` resources supplies its own resolver
([Limits and untrusted input](https://github.com/semyonc/xslt2/blob/main/doc/LIMITS.md#what-a-transformation-can-read)).

## Excluded cases

An excluded case is one whose expected result is wrong for a conformant XSLT 2.0 processor, or pins a choice the
specification leaves to the implementation. Every entry of `EXCLUDED` in
[`tests/xslts/filter.rs`](https://github.com/semyonc/xslt2/blob/main/tests/xslts/filter.rs) quotes the text that makes
it so. An exclusion applies only to a case the run would otherwise run, so the 13 schema-aware entries are "not run" in
the basic runs; a basic run reports 64 exclusions, and each schema-aware run 76 (`strip-space-009` runs only with XML
Schema 1.1, `regex-syntax-xslt20-0985` only without it).

| Group | Basic | Schema-aware | Total |
|---|---:|---:|---:|
| XSLT 3.0 syntax or behaviour | 24 | 8 | 32 |
| Results that XSLT 2.0 rules out | 6 | 4 | 10 |
| Regular expressions pinned to an older Unicode version | 32 | 0 | 32 |
| Choices the specification leaves to the implementation | 2 | 1 | 3 |
| **All** | **64** | **13** | **77** |

- **XSLT 3.0 syntax or behaviour** (32): the stylesheet uses XPath 3.0 operators (`!`, `||`), XSLT 3.0 instructions,
  declarations, attributes, functions or names (`xsl:mode`, `xsl:iterate`, `xsl:evaluate`, `expand-text`,
  `item-separator`, `html-version`, `current-output-uri()`, the `adaptive` output method, EQNames …), or the expected
  result is XSLT 3.0's: an XSLT 3.0 error code (XTTE0945, XTTE1535, XTDE3160), XSLT 3.0's `xsl:next-match` order, or
  the type annotations XSLT 3.0 keeps on the implicit result tree.
- **Results that XSLT 2.0 rules out** (10): the expected result keeps whitespace that §4.2 strips from the stylesheet,
  drops whitespace that `xsl:text` (§11.4.2) or the source (§4.4, §6.6) contributes, keeps whitespace that §4.4 strips,
  ignores XTDE0040 for an initial template the stylesheet does not have (§2.3), or expects an element validated through
  its `xsi:type` where §19.2.1.1 requires XTTE1512.
- **Regular expressions pinned to an older Unicode version** (32): 29 `regex-classes` cases compare general categories
  with Unicode 3.1, 5.2 or 6.0 data, and three `regex-syntax-xslt20` cases expect `\w`, `\d` and `\c` as older
  Unicode or XML editions defined them; the regular-expression engine implements Unicode 15.1, and the Unicode version
  is implementation-defined.
- **Choices the specification leaves to the implementation** (3): the prefix that namespace fixup invents
  (`attribute-0004`, §5.7.3), whether an unused variable's circularity is detected (`param-0301`, §9.8), and which of
  two errors is reported first (`import-schema-137`, §2.9).

`tests/xslts/filter.rs` also lists the 16 `unicode-90` cases written in XSLT 3.0, in the test-set the figures leave
out (below).

## The `unicode-90` test-set

The `unicode-90` test-set (1,460 cases, 1,444 of them XSLT 2.0 and 16 excluded as XSLT 3.0 stylesheets) checks
regular-expression character classes against every Unicode code point, with expected results computed from Unicode
9.0 data. It takes about 25 minutes, and the regular-expression engine implements Unicode 15.1, the same version
difference that excludes the `regex-classes` cases, so the figures leave it out (`--skip unicode-90`). Run it alone
with `cargo test --release --test xslts -- -g unicode-90`.

## Reproducing

```bash
git clone https://github.com/w3c/xslt30-test ../../xslt30-test   # where the harness looks; -s PATH for elsewhere
cargo test --release --test xslts -- --skip unicode-90 -f                     # fail, wrongError and harnessError cases
cargo test --release --test xslts -- --skip unicode-90 -t select-2303 -v      # one case
cargo test --release --test xslts -- --skip unicode-90 --schema-aware -f
cargo test --release --features icu-collation --test xslts -- --skip unicode-90 -f
cargo test --release --test xslts -- --skip unicode-90 --all -t attribute-0004 -v   # run an excluded case anyway
```
