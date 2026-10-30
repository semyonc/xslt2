# Limits and untrusted input

This document is for a host that runs stylesheets or transforms documents it does not trust. It says what a
transformation can read, which resources are bounded and how, and what a host has to bound itself.

XSLT 2.0 leaves both to the implementation. Appendix B.1: "Several XSLT instructions may cause arbitrary URIs to be
dereferenced. In this case, the security issues of [RFC3986], section 7, should be considered." §21: "A conforming
processor may impose limits on the processing resources consumed by the processing of a stylesheet."

The measurements below were taken with a release build on an arm64 Mac, on a thread with Rust's default 2 MiB stack
unless they say otherwise.

## What a transformation can read

### One resolver for everything

Every resource a compilation or a transformation reads by URI comes from a `Resolver`
(`xslt2::resolver::Resolver`): `resolve_text(uri)` returns the text, `available(uri)` says whether it could. A relative
reference is resolved against its base URI first (RFC 3986), so the resolver always sees an absolute URI. The library
itself writes no files and opens no network connections; nothing in its dependency graph is a network client.

| What is read | Through |
|---|---|
| The principal stylesheet module (`Compiler::compile_uri`, and `compile_file`, which turns the path into a `file:` URI), every `xsl:include` and `xsl:import` | the compiler's resolver: `Compiler::with_resolver`, `FileResolver` by default |
| `document()`, `doc()`, `doc-available()`, `Transformation::transform_uri` | the transformation's resolver: the compiler's, unless `Transformation::with_resolver` gives the run another |
| `unparsed-text()`, `unparsed-text-available()` | the transformation's resolver |
| `collection()` | nothing: it always returns the empty sequence |
| Schema-aware mode: the pre-scan of the stylesheet (`SchemaContextBuilder::stylesheet_file`, `stylesheet_uri`), each `xsl:import-schema schema-location`, the host's schemas by location (`schema_uri`, `schema_file`), and every `xs:include`, `xs:import`, `xs:redefine` and `xs:override` in those schemas | the builder's resolver: `SchemaContextBuilder::with_resolver`, `FileResolver` by default |
| The schemas of the XML and XLink namespaces | built-in copies, for an import by namespace only and whenever a location given for them cannot be read |
| `xsi:schemaLocation` hints in a source document | not followed: a source is validated against the stylesheet's schema context only |
| `xsl:result-document` | writes nothing: every result tree is returned in `Outcome::secondary`, and the host decides what to store where |

A schema-aware compiler (`Compiler::schema_aware`) starts with the resolver its `SchemaContext` was built with, so
one resolver given to `SchemaContextBuilder::with_resolver` governs the pre-scan, the schemas, the compilation and,
by default, every transformation. `Compiler::with_resolver` still overrides it. A context made with
`SchemaContext::new()` has the `FileResolver`.

Text the host hands over does not go through the resolver: `compile_str`, `transform_source`, `schema_str`, sources
registered with `Transformation::register_source`, and trees inserted into the run's `DocumentStore`. `doc()` and
`document()` find a registered source or an inserted tree by its URI before they ask the resolver.

### `FileResolver`

`FileResolver`, the default, reads **any file the process can read**:

- a `file:` URI, whatever its authority: `file://example.com/etc/hosts` reads the local `/etc/hosts`;
- a path without a scheme;
- a relative reference resolved against a `file:` base, `..` segments included, so a stylesheet can climb to the
  root of the file system.

It reads no other scheme: `http:` and `https:` URIs are errors (FODC0002 for `doc()`, XTDE1170 for
`unparsed-text()`), and `doc-available()` and `unparsed-text-available()` return false for them. It reads a whole file
into memory, with no size limit.

### `MemoryResolver`

- `MemoryResolver::new()` serves only the texts registered with `insert` or `with`, by exact URI; anything else is an
  error, and `doc-available()` and `unparsed-text-available()` return false.
- `MemoryResolver::with_fallback(next)` asks `next` for what it does not hold. With a `FileResolver` as `next`, the
  whole file system is readable again.

A `MemoryResolver` without a fallback is the simplest way to run an untrusted stylesheet: it can read only what the
host put there. Compile with `compile_uri` or `compile_str`; `compile_file` asks the resolver for the file's `file:`
URI.

### A resolver of your own

A host implements `Resolver` to decide per URI: an allow-list of directories or URI prefixes, a size cap, a count of
resources, a catalog, or `http:` access through a client of its choice. The resolver is also the place to check a
resource before the processor parses it (see [Parsing](#parsing)).

### DTDs and entities

The XML parser reads a document type declaration and processes none of its declarations, in sources, stylesheet
modules and schema documents alike:

- it never reads an external DTD subset or an external entity (`<!ENTITY e SYSTEM "file:///etc/hosts">` reads nothing);
- a reference to any entity other than `&amp;`, `&lt;`, `&gt;`, `&apos;` and `&quot;` makes the document not
  well-formed, so no entity is ever expanded;
- attribute types and default values declared in the DTD are not applied (an attribute declared `ID` there is not an
  ID).

So a document cannot make the processor read a file through an entity, or expand entities without bound. The other
side of it: documents that rely on DTD entities cannot be read ([Conformance](https://github.com/semyonc/xslt2/blob/main/doc/CONFORMANCE.md#entities-declared-in-a-dtd-not-implemented-8-cases)).

### Advice

- For an untrusted stylesheet, give the compiler (and, in schema-aware mode, the `SchemaContextBuilder`) a
  `MemoryResolver` without a fallback, or a resolver of your own that allows only what you allow. Its transformations
  inherit it; a `Transformation::with_resolver` replaces it for one run, so give that one the same care.
- Never fall back to `FileResolver` for untrusted input: it reads every file the process can read.
- Run untrusted work in a process whose own permissions are as narrow as you can make them; the resolver is the only
  gate inside the library.

## Resource limits

### Recursion and the native stack

Templates, stylesheet functions and nested instructions recurse on the native stack, and so does the compilation of
nested instructions. A stack overflow aborts the whole process; Rust cannot catch it. A run and a compilation
therefore measure the stack they use (a run also counts its nesting), and stop with an `XSLT2-INTERNAL` error that
names the option at whichever limit comes first:

| Limit | Default |
|---|---|
| `TransformOptions::max_recursion_depth` | `DEFAULT_MAX_RECURSION_DEPTH` = 40,000: a template invocation counts 1, a stylesheet-function call 8 (so 5,000 nested function calls) |
| `TransformOptions::stack_limit` | on a thread started by `run_on_large_stack`: its stack size, less what is in use when the run starts, less a reserve of one eighth (at least 1 MiB, at most half): about 224 MiB on `DEFAULT_STACK_SIZE` (256 MiB). On any other thread: `OTHER_THREAD_STACK_LIMIT` = 1 MiB. A compilation has no options; it always takes this default |

Where the stack is checked:

- **a run**: before every instruction (a literal result element, `xsl:if`, `xsl:for-each`, `xsl:variable` …), on
  entry to every template (built-in rules included) and stylesheet function, and before every key index build,
  every evaluation of a global variable, every attribute set that another one uses, and every compilation of a
  computed `xsl:analyze-string` regular expression;
- **a compilation**: at every instruction it compiles, at every element the static checks visit, and at every module
  of an `xsl:include` / `xsl:import` chain.

Copying a tree (`xsl:copy-of`, or nodes that `xsl:sequence` adds to a result or a temporary tree, with or without
validation), stripping whitespace from a source (`xsl:strip-space`), admitting a typed source, and serializing a
result are loops, not recursions: they need no more stack for a document 100,000 elements deep than for a shallow
one.

What runs between two checks uses the room the limit leaves: one instruction's own work, and the evaluation of one
XPath expression. XPath expressions nest at most 256 levels (see [Parsing](#parsing)), and the deepest one the parser
accepts (253 nested `for`) still evaluated in a release build at the innermost of 104 nested literal result elements,
where the run had used almost its whole limit, on a 2 MiB thread.

Measured in a release build on a 2 MiB thread, with its 1 MiB limit:

| Input | Runs up to | Deeper |
|---|---|---|
| nested instructions or literal result elements in one template body (`xsl:if`, `xsl:for-each`, `xsl:analyze-string`, `<e>` …) | 103 to 104 levels (94 of `xsl:for-each-group`) | the error, at run time |
| nested `xsl:variable` holding a temporary tree, one in each of nested literal result elements | 35 levels | the error, at run time |
| nested instructions in a template that is never called (compilation only) | 234 levels | the error, at compile time |
| a chain of `xsl:include` / `xsl:import`, each module including the next | 850 / 897 modules | the error, at compile time |
| a small recursive named template (`xsl:choose` around `xsl:call-template`) | 67 calls | the error |
| an identity transformation (`xsl:copy` and `xsl:apply-templates`) | a source 38 elements deep | the error |
| the built-in template rules | a source 701 elements deep | the error |
| `xsl:copy-of`, `xsl:strip-space`, a typed source, serialization | 100,000 elements deep, tried | — |

The main thread of a program is "any other thread" too, so the same numbers hold there. On a
`run_on_large_stack(DEFAULT_STACK_SIZE, …)` thread, 10,000 nested instructions, a chain of 10,000 included modules and
a source 100,000 elements deep compiled and ran; nested temporary trees stopped with the error between 5,000 and
10,000 levels, and an identity transformation between 5,000 and 10,000 levels of source.

Compiling takes time linear in the nesting depth: 10,000 nested `xsl:if` compiled in 56 ms and 20,000 in 118 ms on
the large stack.

**A debug build** (the profile `cargo test` uses, for this crate as a dependency too) uses several times more stack per
level: on a 2 MiB thread 19 nested `xsl:if` ran and 20 stopped with the error, as did 8 nested temporary trees, the
compilation of 25 nested instructions, and an identity transformation of a source 7 elements deep. The XPath engine
(`xsd-schema`) does not check the stack, and in a debug build its frames are large enough that an expression nested
about 25 levels (26 nested parentheses ran, 27 overflowed; 23 nested predicates ran, 24 overflowed) overflows a 2 MiB
thread on its own, well inside the parser's limit. Run deep work in a debug build on a large stack.

**Advice.** Run every job whose depth you do not control whole (compile, transform, serialize) inside
`xslt2::run_on_large_stack(xslt2::DEFAULT_STACK_SIZE, || …)`: in a release build the processor stops with an error
instead of overflowing on any thread, but only a large stack lets deep but legitimate stylesheets and documents
through. A host that wants
smaller limits bounds the nesting depth of stylesheet modules and source documents before the processor sees them
([Parsing](#parsing)), or sets `TransformOptions::stack_limit`.

### Memory per tree

Every result tree and every temporary tree (a variable's document, `xsl:document`, an `xsl:result-document`) takes about
115 KB of the transformation's arena however small it is, and the arena keeps it until it is dropped. Measured: 1,000
one-element result documents, or 1,000 one-element temporary trees, used 115.6 MB of the arena, and the arena had
reserved 218 MB (it grows in chunks that double). Tens of thousands of trees need gigabytes. Dropping the `Outcome`,
the `Transformation` and the arena frees everything a run built.

### Regular expressions and collations

- **Nesting.** A regular expression may nest groups and character classes 64 levels deep. A deeper one is refused
  before it is compiled: `fn:matches`, `fn:replace` and `fn:tokenize` raise FORX0002, and `xsl:analyze-string`
  raises `XSLT2-INTERNAL`, naming the limit.
- **Time.** Matching has no time budget. The nested-quantifier patterns we tried (`^((a+)+)[b-z]$`,
  `^(a|aa)*[b-z]$` over strings of up to 30 characters) finished in under a millisecond, but the processor does not
  bound the time of a match.
- **Collations** (`icu-collation` feature): under a UCA collation, `contains()` and the other substring functions give
  up with FOCH0004 after a work budget of about a second, on pathological input such as long runs of characters that
  chain into contractions.

### Parsing

`xsd-schema`, the parser, has optional parse limits (`xslt2::xsd_schema::xml::ParseLimits`: element depth, name length,
namespace-URI length, attributes and namespace declarations per element, namespaces in scope, attribute-value size,
text size and node count), all off by default. **`xslt2` sets none of them and has no option for them**: every source
document and stylesheet module is parsed without limits. Parsing itself does not recurse (a source 100,000 elements
deep parses in about 10 ms on a 2 MiB thread), and neither do copying and serializing it; templates applied to a deep
document and deeply nested stylesheets do, and stop at the stack limit ([above](#recursion-and-the-native-stack)).

A host that needs the limits checks the text itself before the processor parses it: in its `Resolver`, and before it
calls `transform_source` or `compile_str`. `xslt2::xsd_schema::document::BufferDocumentBuilder::with_parse_limits`
followed by `build` parses with the limits and refuses a document that breaks one.

Two limits do apply:

- **XPath expressions** may nest 256 levels (`xslt2::xsd_schema::xpath::MAX_EXPRESSION_DEPTH`); a deeper expression
  is a static error naming the limit (130 nested parentheses already count 263 levels). The parser accepted 126
  nested parentheses or function calls, 252 nested `if`, 253 nested `for` and 83 nested predicates, and a pattern
  with 84 nested predicates (a deeper pattern is XTSE0340).
- **Schema documents** are read with `xsd-schema`'s own nesting limits on elements, components and
  `xs:include` / `xs:import` chains.

### What is not limited

Nothing in the library bounds:

- the total memory of a run: trees, messages (`Outcome::messages`), the text a resolver returns, the documents a run
  loads;
- the total time of a run, or of a compilation; there is no way to cancel a run once it has started;
- the size of the output, or the number of result documents.

A host bounds them from outside:

- **Time.** Call `run_on_large_stack` from a thread of your own and stop waiting for that thread after a timeout (a
  channel's `recv_timeout`; `run_on_large_stack` itself waits for its job). The job cannot be stopped: Rust has no
  way to kill a thread, so it runs until the transformation ends. A separate process, which the host can kill, is
  the only hard bound.
- **Memory.** One arena per run, dropped after the run; a process with an operating-system memory limit for untrusted
  stylesheets.
- **Input.** A resolver that caps the size and the number of the resources it serves, and checks their depth.
