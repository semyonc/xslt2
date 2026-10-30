//! The transformation: the dynamic context of §5.4/§9.1, template dispatch
//! (§6), and the evaluation of the instruction tree.
//!
//! Everything a run needs that outlives a single call — the document store,
//! the lazily evaluated globals, the message log —
//! lives behind a `RefCell` on [`Transformation`], whose methods all take
//! `&self`. That is what lets the XSLT function evaluator, which must be
//! handed to the XPath engine as a shared reference, hold the running
//! transformation while an expression is being evaluated. Everything that is
//! genuinely per-call — the variable frame, the focus, the current rule, the
//! tunnel parameters — is passed down explicitly, so no `RefCell` is ever
//! borrowed across an evaluation.

mod analyze;
mod calls;
mod content;
mod context;
mod dispatch;
mod documents;
mod eval;
mod expressions;
mod format;
mod functions;
mod grouping;
mod keys;
mod number;
mod patterns;
mod results;
mod serialize;
mod sort;
mod typed_sources;
pub(crate) mod validate;
mod variables;

pub(crate) use context::{Frame, XsltContext};
pub use results::SecondaryResult;
pub(crate) use serialize::{serialize_result, serialize_result_to_bytes, tree_context};

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::BufferDocument;
use xsd_schema::navigator::DomNavigator;
use xsd_schema::types::{DateTimeValue, TimezoneOffset};
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::XmlItem;

use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::{QName, XSLT_NS};
use crate::receiver::{ComplexContentNormalizer, Nav, Receiver, ResultBuilder};
use crate::resolver::Resolver;
use crate::store::{DocId, DocumentStore};

/// What a caller can change about one run.
#[derive(Debug)]
pub struct TransformOptions {
    /// `xsl:template name="…"` to start from, instead of matching the initial
    /// context node.
    pub initial_template: Option<QName>,
    /// The mode the initial `xsl:apply-templates` uses.
    pub initial_mode: ModeKey,
    /// Signal recoverable errors instead of taking the spec's recovery action.
    pub strict_recoverable: bool,
    /// How deep template invocations and stylesheet-function calls may nest
    /// before the run is abandoned with an `XSLT2-INTERNAL` error.
    ///
    /// There is no tail-call elimination, so every nested call consumes host
    /// stack, and a runaway recursion would otherwise abort the process, which
    /// a caller cannot catch. Each template invocation — named, applied
    /// (including `xsl:apply-imports` and `xsl:next-match`) or the initial one
    /// — counts **1**; each stylesheet-function call counts **8**, because a
    /// call through the XPath engine nests deeper on the host stack. A limit
    /// below 8 therefore makes even a single, non-recursive function call
    /// fail. The default, [`DEFAULT_MAX_RECURSION_DEPTH`], allows 40 000 nested
    /// templates or 5 000 nested function calls.
    ///
    /// A count cannot bound the stack a call uses, which depends on its body;
    /// [`Self::stack_limit`] does, and the run stops at whichever limit is
    /// reached first. Key index builds, global variables and attribute sets,
    /// which can also nest, are not counted, and only [`Self::stack_limit`]
    /// bounds them. A global variable or an attribute set is entered at most
    /// once per declaration on any one chain of them. A key's index is built
    /// at most once per document, current group and current captured
    /// substrings — re-entering a build that is in progress is XTDE0640 — but
    /// that does not make it once per chain: a key's own evaluation can call
    /// `key()` over a document it creates, or, when its content runs
    /// `xsl:for-each-group` or `xsl:analyze-string`, under a current group or
    /// captured substrings of its own, and each of those is a new index to
    /// build. A key that looks itself up that way recurses until the stack
    /// guard stops it.
    pub max_recursion_depth: usize,
    /// How many bytes of host stack the run may use, measured from the point
    /// where it starts, before it is abandoned with the same error as
    /// [`Self::max_recursion_depth`].
    ///
    /// Checked on entry to every template invocation (built-in template rules
    /// included) and every stylesheet-function call, so a deep recursion ends
    /// in that error however much stack each of its calls uses — and also
    /// where evaluation nests without such an entry: before every key index
    /// build (a key's pattern or key specifier may call `key()` for another
    /// key, or for the same key over another document), every evaluation of
    /// a global variable, every attribute set that another one uses, and
    /// every compilation of a computed `xsl:analyze-string` regular
    /// expression. What one body uses *between* two such entries is not
    /// checked; the room left above the limit is for that.
    ///
    /// `None` (the default) derives the limit from the thread the run is on:
    ///
    /// - on a thread [`run_on_large_stack`] started, its stack size, less the
    ///   stack already in use where the run starts, less a reserve of one
    ///   eighth of the stack (at least 1 MiB, at most half the stack) — on the
    ///   default [`DEFAULT_STACK_SIZE`] about 224 MiB;
    /// - on any other thread, whose stack size the run cannot know,
    ///   [`OTHER_THREAD_STACK_LIMIT`] (1 MiB), which leaves room on a thread
    ///   with Rust's default 2 MiB stack.
    ///
    /// A host that runs the transformation on a thread of its own with a known
    /// stack can set the limit explicitly: that stack size, less what is in use
    /// when the run starts, less a reserve.
    pub stack_limit: Option<usize>,
    /// Where `document('')` and relative URIs resolve against.
    pub base_uri: Option<String>,
    /// The base output URI (§2.3): "a URI to be used as the base URI when
    /// resolving a relative URI allocated to a final result tree" — the `href`
    /// of every `xsl:result-document` resolves against it, and it is the URI
    /// of the primary result (§2.4 treats the tree created implicitly as an
    /// `xsl:result-document href=""`). The run takes it without a fragment
    /// identifier and with its `.` / `..` segments removed
    /// ([`Outcome::base_output_uri`]).
    ///
    /// "The way in which a base output URI is established is
    /// implementation-defined": `None` (the default) takes the base URI of the
    /// principal stylesheet module ([`CompiledStylesheet::base_uri`]). That
    /// one is always known — a run with no source document, or over a tree
    /// built in memory, has no document URI to fall back on — and it makes the
    /// URIs of a run's results depend on the stylesheet and its `href`s, not on
    /// which document was transformed. This processor writes nothing anywhere
    /// ([`Outcome::secondary`]); a host that writes the results out under
    /// their URIs sets this option, or it would write the primary result over
    /// the stylesheet.
    ///
    /// **Absolute or not.** The result URIs are absolute only when the base
    /// output URI is (or an `href` is absolute itself). Nothing rejects a
    /// relative or empty one, and both occur by default: a stylesheet compiled
    /// with `Compiler::compile_str(text, "")` has the empty base URI, so the
    /// primary result's URI is `""`, an `href="a.xml"` is `a.xml` and an
    /// `href="../b.xml"` stays `../b.xml`; a relative base (`out/`) gives
    /// relative URIs (`out/a.xml`). They are still the trees' identities —
    /// XTDE1490 and [`Outcome::result_document`] work the same — but a host
    /// that writes files resolves them against something of its own first.
    ///
    /// **A directory needs a trailing `/`.** RFC 3986 §5.2.3 merges a relative
    /// `href` with "all but the last segment of the base URI's path", so
    /// against `file:///o/out` the `href` `a.xml` is `file:///o/a.xml`, and
    /// the primary result is `file:///o/out` itself; against `file:///o/out/`
    /// it is `file:///o/out/a.xml`.
    pub base_output_uri: Option<String>,
    /// How a source document is validated when it enters the run: the default for
    /// every source-tree loader — [`Transformation::transform_source`],
    /// [`transform_uri`](Transformation::transform_uri), `document()`, `doc()` and a registered
    /// source — which [`Transformation::transform_source_with`],
    /// [`load_document_with`](Transformation::load_document_with) and
    /// [`register_source_with`](Transformation::register_source_with) override per document.
    /// Default [`SourceValidation::Skip`]: nothing is validated unless the host asks.
    ///
    /// The default is for the documents the run transforms and reads; it does **not** apply to a
    /// stylesheet module of this stylesheet read with `document()` / `doc()` — `document('')`,
    /// which "refers to the document node of the containing stylesheet module" (§16.1) — which is
    /// read unvalidated unless the host sets a mode for its URI
    /// ([`Transformation::set_source_validation`]): a stylesheet module is not a source the host
    /// asked to validate. Any other document the
    /// stylesheet reads by URI is validated by the default, so with `Strict` a document that is
    /// not an instance of the schema set is an error (FODC0002); exempt one with
    /// `set_source_validation(uri, SourceValidation::Skip)`.
    pub source_validation: SourceValidation,
    /// The implicit timezone of the run (XPath 2.0 §C.2's dynamic-context component): what
    /// `implicit-timezone()` returns, the timezone of `current-dateTime()`, and the
    /// timezone a date or time without one is compared and subtracted with — F&O 1.0 §10.4: "If
    /// either operand to a comparison function on date or time values does not have an
    /// (explicit) timezone then, for the purpose of the operation, an implicit timezone,
    /// provided by the dynamic context …, is assumed to be present as part of the value". The
    /// processor's own comparisons use it too: sort keys (`lt`), key values and grouping keys
    /// (`eq`).
    ///
    /// §5.4.3.2: "The implicit timezone is implementation-defined"; `None` (the default) is the
    /// host's local offset, read once, when the run first needs the clock. Either way it is one
    /// value for the whole transformation — §5.4.3: "the following components defined in Section
    /// 2.1.2 Dynamic Context must be stable: function implementations, current dateTime, implicit
    /// timezone, …" —, in every expression, template, stylesheet function and
    /// `xsl:result-document`.
    ///
    /// A timezone is within -14:00 and +14:00 (XML Schema 1.1 Part 2 §3.3.7); a run given one
    /// outside that range fails with FODT0003. Not affected: the XPath expressions `xsd-schema`
    /// evaluates during schema validation — XML Schema 1.1 assertions and type alternatives on
    /// validated sources and constructed trees — which read the host's offset.
    pub implicit_timezone: Option<TimezoneOffset>,
}

/// Whether, and how, a source document is validated against the stylesheet's schema set when it
/// enters a run ([`TransformOptions::source_validation`]).
///
/// XSLT 2.0 leaves it to the processor: §2.8 "where the source document has undergone schema
/// validity assessment, the XSLT processor has access to the type information associated with
/// individual nodes", and F&O 1.0 §15.5.4 (`fn:doc`): "It is ·implementation-defined· whether DTD
/// validation and/or schema validation is applied to the source document". The schemas are those
/// of the stylesheet's `SchemaContext` — its `xsl:import-schema` declarations and the schemas the
/// host added (§3.13's implementation-defined mechanism); `xsi:schemaLocation` hints in the
/// document are not followed. A document that is not valid is an error.
///
/// New variants may be added in a minor release (`#[non_exhaustive]`): a
/// `match` on it outside this crate needs a wildcard arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceValidation {
    /// Not validated: the tree is untyped (every element `xs:untyped`, every attribute
    /// `xs:untypedAtomic`), as on a basic processor.
    #[default]
    Skip,
    /// Strict validation: the document element must have a top-level element declaration (or an
    /// `xsi:type`), and the document must be valid.
    Strict,
    /// Lax validation: as strict for a document element that has a declaration (or an
    /// `xsi:type`); one that has neither is assessed laxly — no error for it, its declared
    /// descendants validated, and it is unbound (`xs:untyped`) in the typed tree (XML Schema 1.1
    /// §5.2's lax wildcard validation).
    Lax,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            initial_template: None,
            initial_mode: ModeKey::Default,
            strict_recoverable: false,
            max_recursion_depth: DEFAULT_MAX_RECURSION_DEPTH,
            stack_limit: None,
            base_uri: None,
            base_output_uri: None,
            source_validation: SourceValidation::Skip,
            implicit_timezone: None,
        }
    }
}

/// The stack a transformation runs on by default.
///
/// A deeply recursive named template consumes host stack, and there is no
/// tail-call elimination. Running the whole job on a thread with a large stack
/// is the remedy; see [`run_on_large_stack`]. Measured on this stack in a
/// release build: a recursive named template that does little else uses about
/// 7.5 KB per level, so the stack guard ([`TransformOptions::stack_limit`],
/// about 224 MiB here) stops it with the depth error at about 31 000 levels;
/// without the guard it overflowed between 35 000 and 36 000.
pub const DEFAULT_STACK_SIZE: usize = 256 * 1024 * 1024;

/// The nesting depth a run allows by default
/// ([`TransformOptions::max_recursion_depth`]): 40 000 template invocations, a
/// stylesheet-function call counting as 8.
///
/// Set below the depth of light template bodies [`DEFAULT_STACK_SIZE`] can
/// carry, so that a runaway recursion through them ends in an error the caller
/// can catch rather than in a process abort the caller cannot. Heavier bodies
/// are stopped by [`TransformOptions::stack_limit`] first.
pub const DEFAULT_MAX_RECURSION_DEPTH: usize = 40_000;

/// The [`TransformOptions::stack_limit`] a run uses by default on a thread that
/// [`run_on_large_stack`] did not start: 1 MiB from the point where the run
/// starts.
///
/// The run cannot know the size of such a thread's stack. Rust gives a spawned
/// thread 2 MiB by default (and a test thread the same), so this limit leaves
/// half of that for what was in use before the run and for the stack one body
/// uses between two checks.
pub const OTHER_THREAD_STACK_LIMIT: usize = 1024 * 1024;

/// The smallest reserve the stack guard keeps on a [`run_on_large_stack`]
/// thread (but never more than half its stack); the reserve is one eighth of
/// the thread's stack when that is more.
const MIN_STACK_RESERVE: usize = 1024 * 1024;

thread_local! {
    /// For a thread [`run_on_large_stack`] started: the stack position where
    /// the thread began and its stack size, from which the stack guard derives
    /// [`TransformOptions::stack_limit`]. `None` on every other thread.
    static LARGE_STACK: std::cell::Cell<Option<(usize, usize)>> =
        const { std::cell::Cell::new(None) };
}

/// A position on the current thread's stack: the address of a local of the
/// caller's frame. Stacks grow downwards on every supported target, but the
/// guard only ever takes the distance between two positions, so the direction
/// does not matter.
#[inline(always)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::hint::black_box(std::ptr::addr_of!(marker)) as usize
}

/// Runs `job` on a thread with `stack_size` bytes of stack and returns its
/// result.
///
/// The compiled stylesheet borrows the name table and the arena, which are not
/// `Sync`, so it is not `Send`: the *whole* job — compile, load, transform,
/// serialize — has to happen inside the closure; only owned, `Send` values
/// cross the boundary.
///
/// The thread remembers its stack size, so a transformation run inside `job`
/// knows how much stack it may use before it stops with the depth error
/// rather than overflowing ([`TransformOptions::stack_limit`]).
pub fn run_on_large_stack<R, F>(stack_size: usize, job: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(stack_size)
        .spawn(move || {
            LARGE_STACK.with(|cell| cell.set(Some((stack_position(), stack_size))));
            job()
        })
        .expect("a worker thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// Where on the stack a run started, and how far below that it may go
/// ([`TransformOptions::stack_limit`]). `base` is 0 outside a run.
#[derive(Clone, Copy, Default)]
struct StackGuard {
    base: usize,
    limit: usize,
}

/// The extent of one public entry point that evaluates stylesheet code (a
/// run, a direct [`Transformation::matches`]): the outermost one sets the
/// stack guard's base on entry and clears it on exit, so the guard is on for
/// everything evaluated below it and a later call, from wherever on the stack
/// it is made, never measures from a stale base. A nested one leaves the
/// outer base alone.
pub(super) struct StackScope<'g> {
    cell: &'g std::cell::Cell<StackGuard>,
    owner: bool,
}

impl Drop for StackScope<'_> {
    fn drop(&mut self) {
        if self.owner {
            self.cell.set(StackGuard::default());
        }
    }
}

/// One `xsl:message` (§17).
///
/// New fields may be added in a minor release (`#[non_exhaustive]`): the run
/// makes messages, a program reads them.
#[derive(Debug)]
#[non_exhaustive]
pub struct Message {
    /// The message: the document the instruction's `select` attribute and
    /// content build, serialized as XML without an XML declaration.
    pub content: String,
    /// Whether the instruction said `terminate="yes"`: the run then stopped
    /// with the error XTMM9000 after this message.
    pub terminate: bool,
}

/// What a run produced: its final result trees (§19.1) and its messages.
///
/// The **primary result** is the final result tree whose URI is the base
/// output URI ([`base_output_uri`](Self::base_output_uri)): the tree the
/// initial template's result builds, or — when that result is empty and an
/// `xsl:result-document` created a tree at the base output URI — that tree
/// (§2.4). Every other tree an `xsl:result-document` created is in
/// [`secondary`](Self::secondary).
#[non_exhaustive]
pub struct Outcome<'t> {
    /// The primary result tree; `None` only when the initial template's result
    /// was empty, `xsl:result-document` was evaluated, and none of those
    /// instructions wrote to the base output URI: then "An implicit result tree
    /// is also created when the result sequence is empty, provided that no
    /// xsl:result-document instruction has been evaluated" does not apply.
    ///
    /// "Empty" is §2.4's "If the result sequence is non-empty" read
    /// literally: the result *sequence* counts, not the tree it builds. Any
    /// item makes it non-empty — a zero-length text node (`<xsl:text/>`,
    /// `<xsl:value-of select="''"/>`), a zero-length string
    /// (`<xsl:sequence select="''"/>`), an empty document node
    /// (`<xsl:document/>`) — even though §5.7.1 then leaves nothing of it in
    /// the tree (§11.4: "zero-length text nodes are ignored when they appear
    /// in a sequence that is used to form the content of a node"). So such an
    /// initial template next to an `xsl:result-document` without `href` is
    /// XTDE1490, two trees at the base output URI; `<xsl:sequence
    /// select="()"/>` is not. The same holds when the zero-length text node
    /// comes out of a template with an `as` attribute — the initial template
    /// or one it calls or applies — or out of `xsl:perform-sort`'s sequence
    /// constructor. (A variable, parameter or function result with `as` still
    /// loses such a node — no tree this crate builds can hold a text node with
    /// an empty value — so `<xsl:sequence select="$v"/>` over one does not
    /// count.)
    ///
    /// The tree created implicitly has the base output URI as its document
    /// node's base URI (none when the base output URI is empty), as a tree an
    /// `xsl:result-document href=""` creates does (§2.4, §19.1).
    pub primary: Option<&'t BufferDocument<'t>>,
    /// `xsl:message` output, in order.
    pub messages: Vec<Message>,
    /// The characters of the primary result tree's text nodes that were
    /// written with `disable-output-escaping="yes"` in final output state
    /// (§20.2) — the tree's "additional boolean property disable-escaping",
    /// which only the serializer reads. Empty when the stylesheet disabled no
    /// escaping.
    pub(crate) disabled_escaping: crate::serialize::EscapingMarks,
    /// The run's base output URI ([`TransformOptions::base_output_uri`], less
    /// any fragment identifier, with its dot segments removed), which is the
    /// URI of the primary result. Absolute only if the option (or the
    /// stylesheet's base URI it defaults to) is; it may be empty.
    pub base_output_uri: String,
    /// The other final result trees, in the order their `xsl:result-document`
    /// instructions were evaluated, each under its URI — `href` resolved
    /// against the base output URI, so absolute when that is (see
    /// [`TransformOptions::base_output_uri`] for a relative or empty one).
    /// §19.1: "If the implementation provides an API to access final result
    /// trees, then it must allow a final result tree to be identified by means
    /// of this base URI"; see [`result_document`](Self::result_document).
    /// Nothing is written anywhere: serializing a tree, and storing it, is the
    /// host's business (`CompiledStylesheet::serialize_secondary`).
    ///
    /// **Memory.** Every result tree — and every temporary tree the run builds
    /// (variables, `xsl:document`, messages) — costs about 118 KB of the run's
    /// arena however small it is (the document builder reserves a 4,096-node
    /// page and a 4,096-entry namespace page per tree), and the arena keeps it
    /// until it is dropped; every secondary result is also kept here until the
    /// run ends. Tens of thousands of result documents therefore need
    /// gigabytes (10,000 trivial ones ≈ 1.2 GB). This version has no streaming
    /// handler that would let the host serialize and drop each tree as it
    /// completes.
    pub secondary: Vec<SecondaryResult<'t>>,
    /// The effective output definition of a primary result an
    /// `xsl:result-document` created; `None` for the tree created implicitly,
    /// which the unnamed output definition serializes (§20).
    pub(crate) primary_output: Option<crate::output::OutputDefinition>,
}

impl std::fmt::Debug for Outcome<'_> {
    /// The result trees by their URIs and the number of messages; a tree is a placeholder.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secondary: Vec<&str> = self
            .secondary
            .iter()
            .map(|tree| tree.uri.as_str())
            .collect();
        f.debug_struct("Outcome")
            .field(
                "primary",
                &self.primary.map(|_| crate::Opaque("<document>")),
            )
            .field("base_output_uri", &self.base_output_uri)
            .field("secondary", &secondary)
            .field("messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

impl<'t> Outcome<'t> {
    /// The **secondary** result tree whose URI is `uri`: a URI as in
    /// [`SecondaryResult::uri`] (resolved, not relative to anything), compared
    /// as XTDE1490 compares two result URIs — after escaping the characters a
    /// URI may not contain (as `fn:iri-to-uri` does) and RFC 3986 §6.2.2
    /// syntax-based normalization (case of the scheme and host, percent-encoding,
    /// dot segments). So `file:///o/./a%2Exml` finds the tree at
    /// `file:///o/a.xml`, and `file:///o/a%20b.xml` the one at
    /// `file:///o/a b.xml`; `file:///o//a.xml` does not find `file:///o/a.xml`.
    ///
    /// The primary result is not among them, whichever instruction made it:
    /// its URI is [`base_output_uri`](Self::base_output_uri), its tree
    /// [`primary`](Self::primary), and `result_document(&outcome.base_output_uri)`
    /// is `None`. (A tree of its own type rather than a [`SecondaryResult`]:
    /// its output definition is `CompiledStylesheet::primary_output_definition`.)
    pub fn result_document(&self, uri: &str) -> Option<&SecondaryResult<'t>> {
        if let Some(tree) = self.secondary.iter().find(|tree| tree.uri == uri) {
            return Some(tree);
        }
        let key = crate::resolver::syntax_normalized(uri);
        self.secondary
            .iter()
            .find(|tree| crate::resolver::syntax_normalized(&tree.uri) == key)
    }
}
/// How a template was entered, which decides the code a missing required
/// parameter reports (§10.1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Invocation {
    /// The stylesheet's entry point.
    InitialTemplate,
    /// `xsl:call-template`.
    CallTemplate,
    /// `xsl:apply-templates`, `xsl:apply-imports`, `xsl:next-match`.
    ApplyTemplates,
}

/// A global variable's evaluation state (§9.5, XTDE0640).
enum GlobalSlot<'t> {
    Unevaluated,
    Evaluating,
    Value(XPathValue<Nav<'t>>),
}

/// One run of a compiled stylesheet.
///
/// It reads documents — `document()`, `doc()`, `unparsed-text()`,
/// [`transform_uri`](Self::transform_uri) — through the resolver its
/// stylesheet was compiled with
/// ([`Compiler::with_resolver`](crate::Compiler::with_resolver); by default
/// [`FileResolver`](crate::FileResolver), or for a schema-aware compiler the
/// resolver its context was built with) unless it is given another
/// ([`with_resolver`](Self::with_resolver)).
pub struct Transformation<'a, 't> {
    sheet: &'a CompiledStylesheet<'a>,
    store: RefCell<DocumentStore<'t>>,
    globals: RefCell<Vec<GlobalSlot<'t>>>,
    /// The key indexes (§16.3), one per key and document — and,
    /// for a key whose specifier may observe it, current group — built on
    /// first use.
    keys: RefCell<keys::KeyIndexes<'t>>,
    /// The serial number of the last current group or set of captured
    /// substrings created (`CurrentGroup::serial`,
    /// `CapturedSubstrings::serial`) — one counter for both, which
    /// `runtime/keys.rs` relies on; 0 before the first.
    scope_serials: std::cell::Cell<u64>,
    /// The regular expressions of `xsl:analyze-string` instructions whose
    /// `regex` or `flags` is computed, by effective value (`runtime/analyze.rs`).
    regexes: RefCell<analyze::RegexCache>,
    /// The compiled regular expressions of `fn:matches`, `fn:replace` and
    /// `fn:tokenize`, lent to every evaluation of the run (`eval_expr`). The
    /// engine's own cache lasts one evaluation, and an instruction is evaluated
    /// once per item, so without this a `matches()` in an `xsl:if` inside an
    /// `xsl:for-each` compiled its pattern for every item. One per
    /// transformation, because that is the object that already lives across
    /// evaluations and is used by one thread at a time; bounded by the engine
    /// (`RegexCache::DEFAULT_CAPACITY` entries, least recently used dropped).
    xpath_regexes: xsd_schema::xpath::RegexCache,
    /// What the call of each `id()` / `key()` pattern selects, per document
    /// (§5.5.2), computed on first use.
    id_key_selections: RefCell<patterns::IdKeySelections<'t>>,
    /// The sibling sequences positional pattern predicates were evaluated
    /// against, per predicate and parent (§5.5.3), remembered for the run.
    placements: RefCell<patterns::PlacementCache>,
    /// What each `xsl:number` instruction's last evaluation counted
    /// (`runtime/number.rs`), by `NumberInstruction::memo_slot`.
    number_memos: RefCell<HashMap<usize, number::NumberMemo<'t>>>,
    messages: RefCell<Vec<Message>>,
    params: RefCell<HashMap<QName, XPathValue<Nav<'t>>>>,
    resolver: Arc<dyn Resolver>,
    options: TransformOptions,
    /// The focus global variables are evaluated with (§9.1): the root of the
    /// tree containing the initial context node.
    initial_root: RefCell<Option<Nav<'t>>>,
    terminated: RefCell<bool>,
    /// How deep template invocation is nested right now, a function call
    /// counting `calls::FUNCTION_CALL_DEPTH`.
    depth: std::cell::Cell<usize>,
    /// The stack guard of the current run.
    stack: std::cell::Cell<StackGuard>,
    /// The current dateTime and implicit timezone of this transformation,
    /// fixed the first time an expression is evaluated (§5.4.3.2).
    clock: std::cell::OnceCell<Option<(DateTimeValue, TimezoneOffset)>>,
    #[allow(clippy::type_complexity)]
    message_sink: Option<Box<dyn Fn(&Message)>>,
    /// The final result trees `xsl:result-document` created during the
    /// current run, and the URIs taken (§19.1).
    results: RefCell<results::FinalResults<'t>>,
    /// Whether anything has been written to the primary result: the initial
    /// template's result sequence is non-empty (§2.4).
    primary_written: std::cell::Cell<bool>,
    /// The stylesheet's schema set (schema-aware mode), at the lifetime of the
    /// run's trees, which a typed source tree borrows.
    schema_set: Option<&'t xsd_schema::SchemaSet>,
    /// The validator source documents are validated with, made on first use
    /// (one per transformation).
    validator: std::cell::OnceCell<xsd_schema::validation::SchemaValidator<'a>>,
    /// The validator constructed trees are validated with, made on first use: the source
    /// validator's flags and `ValidationFlags::XPATH_TYPES` (`construction_validator`).
    construction_validator: std::cell::OnceCell<xsd_schema::validation::SchemaValidator<'a>>,
    /// The value constraints the validation of constructed trees reads, kept for the run.
    value_constraints: validate::ValueConstraints,
    /// The validation modes `register_source_with` recorded, by URI.
    source_modes: RefCell<HashMap<String, SourceValidation>>,
    /// See [`Transformation::take_warnings`].
    warnings: RefCell<Vec<String>>,
}

/// For `Debug`: the length of what a `RefCell` of the run holds, or `<borrowed>` while it is
/// borrowed mutably.
struct CellLen<'c, T>(&'c RefCell<T>, fn(&T) -> usize);

impl<T> std::fmt::Debug for CellLen<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.try_borrow() {
            Ok(value) => write!(f, "{}", (self.1)(&value)),
            Err(_) => f.write_str("<borrowed>"),
        }
    }
}

impl std::fmt::Debug for Transformation<'_, '_> {
    /// The stylesheet's base URI, the options, how many parameters and documents the run has; the
    /// resolver and the message sink are placeholders.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transformation")
            .field("stylesheet", &self.sheet.base_uri())
            .field("options", &self.options)
            .field("params", &CellLen(&self.params, HashMap::len))
            .field("documents", &CellLen(&self.store, DocumentStore::len))
            .field("resolver", &crate::Opaque("<dyn Resolver>"))
            .field(
                "message_sink",
                &self
                    .message_sink
                    .as_ref()
                    .map(|_| crate::Opaque("<closure>")),
            )
            .finish_non_exhaustive()
    }
}

impl<'a> CompiledStylesheet<'a> {
    /// A run of this stylesheet over `arena`, reading documents through the
    /// resolver the stylesheet was compiled with
    /// ([`Transformation::with_resolver`] gives it another).
    pub fn transformation<'t>(
        &'a self,
        arena: &'t Bump,
        options: TransformOptions,
    ) -> Transformation<'a, 't>
    where
        'a: 't,
    {
        let globals = (0..self.globals().len())
            .map(|_| GlobalSlot::Unevaluated)
            .collect();
        Transformation {
            sheet: self,
            store: RefCell::new(
                DocumentStore::new(arena, self.names()).with_schema_set(self.schema),
            ),
            globals: RefCell::new(globals),
            keys: RefCell::new(keys::KeyIndexes::default()),
            scope_serials: std::cell::Cell::new(0),
            regexes: RefCell::new(analyze::RegexCache::default()),
            xpath_regexes: xsd_schema::xpath::RegexCache::new(),
            id_key_selections: RefCell::new(HashMap::new()),
            placements: RefCell::new(patterns::PlacementCache::default()),
            number_memos: RefCell::new(HashMap::new()),
            messages: RefCell::new(Vec::new()),
            params: RefCell::new(HashMap::new()),
            resolver: Arc::clone(&self.resolver),
            options,
            initial_root: RefCell::new(None),
            terminated: RefCell::new(false),
            depth: std::cell::Cell::new(0),
            stack: std::cell::Cell::new(StackGuard::default()),
            clock: std::cell::OnceCell::new(),
            message_sink: None,
            results: RefCell::new(results::FinalResults::default()),
            primary_written: std::cell::Cell::new(false),
            schema_set: self.schema,
            validator: std::cell::OnceCell::new(),
            construction_validator: std::cell::OnceCell::new(),
            value_constraints: validate::ValueConstraints::default(),
            source_modes: RefCell::new(HashMap::new()),
            warnings: RefCell::new(Vec::new()),
        }
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// Supplies a stylesheet parameter (§9.5).
    pub fn with_param(self, name: QName, value: XPathValue<Nav<'t>>) -> Self {
        self.params.borrow_mut().insert(name, value);
        self
    }

    /// Where this run reads `document()`, `doc()` and `unparsed-text()`
    /// targets and [`transform_uri`](Self::transform_uri) sources from.
    ///
    /// The default is the resolver the stylesheet was compiled with
    /// ([`Compiler::with_resolver`](crate::Compiler::with_resolver);
    /// for a compiler given none, [`FileResolver`](crate::FileResolver), or
    /// the resolver of a schema-aware compiler's context).
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Starts from a named template instead of matching the context node.
    pub fn with_initial_template(mut self, name: QName) -> Self {
        self.options.initial_template = Some(name);
        self
    }

    /// The mode the initial `xsl:apply-templates` runs in (§6.5).
    pub fn with_initial_mode(mut self, mode: ModeKey) -> Self {
        self.options.initial_mode = mode;
        self
    }

    /// A callback run as each `xsl:message` is produced (§17).
    pub fn with_message_sink(mut self, sink: Box<dyn Fn(&Message)>) -> Self {
        self.message_sink = Some(sink);
        self
    }

    /// Parses `xml` and runs the transformation over it.
    pub fn transform_source(&self, uri: &str, xml: &str) -> Result<Outcome<'t>, XsltError> {
        let id = self.load_document(uri, xml)?;
        self.transform_document(id)
    }

    /// Parses `xml`, validated as `validation` says (rather than as
    /// [`TransformOptions::source_validation`]), and runs over it; see
    /// [`load_document_with`](Self::load_document_with).
    pub fn transform_source_with(
        &self,
        uri: &str,
        xml: &str,
        validation: SourceValidation,
    ) -> Result<Outcome<'t>, XsltError> {
        let id = self.load_document_with(uri, xml, validation)?;
        self.transform_document(id)
    }

    /// The warnings of the run so far: things the run did on its own that the host may want to
    /// know — a source tree typed against another schema set than the stylesheet's, whose
    /// annotations were stripped.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }

    /// Whether the run's schema context is an XML Schema 1.1 one
    /// (`SchemaContextBuilder::xsd_version`): the built-in datatypes are then XML Schema 1.1's.
    pub(crate) fn xsd11(&self) -> bool {
        self.schema_set.is_some_and(xsd_schema::SchemaSet::is_xsd11)
    }

    /// Loads the document the resolver serves at `uri` and runs over it.
    pub fn transform_uri(&self, uri: &str) -> Result<Outcome<'t>, XsltError> {
        let id = self.load_uri(uri)?;
        self.transform_document(id)
    }

    /// The compiled stylesheet this run belongs to.
    pub fn stylesheet(&self) -> &'a CompiledStylesheet<'a> {
        self.sheet
    }

    /// The document store of this run.
    pub fn store(&self) -> &RefCell<DocumentStore<'t>> {
        &self.store
    }

    /// A serial number for a new current group or a new set of current
    /// captured substrings (`CurrentGroup::serial`,
    /// `CapturedSubstrings::serial`): one counter for both, so that of two
    /// nested activations the inner one always has the larger number.
    pub(super) fn next_scope_serial(&self) -> u64 {
        let serial = self.scope_serials.get() + 1;
        self.scope_serials.set(serial);
        serial
    }

    /// The messages produced so far.
    pub fn take_messages(&self) -> Vec<Message> {
        std::mem::take(&mut self.messages.borrow_mut())
    }
    // ── Running ──────────────────────────────────────────────────────────

    /// Runs the transformation over a source document already in the store.
    ///
    /// A tree the host built and put into the store ([`DocumentStore::insert`]) with type
    /// annotations is admitted first. On a basic processor it is refused (XTDE1665, §21.1), unless a
    /// module says `input-type-annotations="strip"`; on a schema-aware one it is taken as it is when
    /// it was validated against the stylesheet's schema set, stripped under
    /// `input-type-annotations="strip"` (§4.3), and stripped with a warning
    /// ([`take_warnings`](Self::take_warnings)) when it was validated against another set. An
    /// untyped tree is taken as it is; none is whitespace-stripped (§4.4).
    pub fn transform_document(&self, source: DocId) -> Result<Outcome<'t>, XsltError> {
        self.admit_stored(source)?;
        let root = self.store.borrow().navigator(source);
        self.run(Some(root))
    }

    /// Runs the transformation with no initial context item, which only makes
    /// sense with an initial template.
    pub fn transform_without_source(&self) -> Result<Outcome<'t>, XsltError> {
        self.run(None)
    }

    /// Enters a [`StackScope`] whose base, if it is the outermost one, is
    /// `base` — the caller's [`stack_position`], taken in its own frame.
    pub(super) fn stack_scope(&self, base: usize) -> StackScope<'_> {
        if self.stack.get().base != 0 {
            return StackScope {
                cell: &self.stack,
                owner: false,
            };
        }
        self.stack.set(StackGuard {
            base,
            limit: self.stack_limit_from(base),
        });
        StackScope {
            cell: &self.stack,
            owner: true,
        }
    }

    fn run(&self, initial: Option<Nav<'t>>) -> Result<Outcome<'t>, XsltError> {
        // Everything the run evaluates happens below this frame, so the
        // stack it uses is measured from here.
        let _stack = self.stack_scope(stack_position());
        // The implicit timezone the host fixed is a timezone: XML Schema 1.1 Part 2 §3.3.7
        // (`timezoneFrag`) limits an offset to -14:00 … +14:00. F&O 1.0 gives a value outside that
        // range FODT0003, "Invalid timezone value" (`adjust-dateTime-to-timezone`).
        if let Some(timezone) = self.options.implicit_timezone {
            if timezone.0.abs() > 14 * 60 {
                return Err(XsltError::new(
                    "FODT0003",
                    format!(
                        "TransformOptions::implicit_timezone is {} minutes from UTC: a timezone is \
                         within -14:00 and +14:00",
                        timezone.0
                    ),
                ));
            }
        }
        // §21.1 XTDE1665, before anything can observe a typed node: "the input
        // to the processor" is the tree containing the initial context node and
        // every tree passed as a stylesheet parameter. The first was admitted by
        // `transform_document` (`admit_stored`); the parameters are checked
        // here, once per document.
        let param_nodes: Vec<Nav<'t>> = self
            .params
            .borrow()
            .values()
            .flat_map(|value| value.clone().into_vec())
            .filter_map(|item| match item {
                XmlItem::Node(nav) => Some(nav),
                XmlItem::Atomic(_) => None,
            })
            .collect();
        for node in &param_nodes {
            self.check_parameter_annotations(node)?;
        }

        *self.initial_root.borrow_mut() = initial.clone().map(|mut n| {
            n.move_to_root();
            n
        });

        // §2.3: "The way in which a base output URI is established is
        // implementation-defined" — see `TransformOptions::base_output_uri`.
        //
        // Taken as `href=""` resolves against it — without a fragment
        // identifier — so that it is exactly the URI of the primary result,
        // which §2.4 treats as an `xsl:result-document href=""`; no other
        // reference resolves differently against the two (RFC 3986 §5.2.2
        // never reads the base's fragment). And with its dot segments removed
        // (§5.2.4), as every non-empty `href` resolved against it has them
        // removed — §5.2.2 copies the base's path unchanged for an empty
        // reference — so that the primary result, XTDE1490 and
        // `Outcome::base_output_uri` see one spelling of it.
        let base_output_uri = crate::resolver::normalize_uri(&crate::resolver::resolve_uri(
            self.options
                .base_output_uri
                .as_deref()
                .unwrap_or_else(|| self.sheet.base_uri()),
            "",
        ));
        *self.results.borrow_mut() = results::FinalResults::new(base_output_uri.clone());
        self.primary_written.set(false);

        let builder = ResultBuilder::new(
            self.store.borrow().arena(),
            self.store.borrow().names(),
            self.schema_set,
        )?;
        // The primary result is written in final output state (§19.1), so
        // `disable-output-escaping` is honoured here (§20.2) — and in the
        // trees `xsl:result-document` creates, each in its own builder.
        let mut out = ComplexContentNormalizer::with_annotations(
            self.validating(builder),
            crate::receiver::Annotations::of_copies(self.schema_set),
        )
        .honouring_disabled_escaping();
        // §3.6: `default-validation` "also determines the validation applied
        // to the implicit final result tree created in the absence of an
        // xsl:result-document instruction" — the principal module's, `strip`
        // or `preserve`, neither of which validates (§19.2.2: "The values
        // validation="preserve" and validation="strip" do not request
        // validation"): the tree's nodes are untyped, or keep what they were
        // given.
        out.validation_boundary(match self.sheet.final_validation {
            crate::model::Validation::Preserve => crate::receiver::Boundary::Preserve,
            _ => crate::receiver::Boundary::Strip,
        })?;
        out.start_document()?;

        let mut cx = XsltContext::empty();
        if let Some(node) = initial {
            cx.item = Some(XmlItem::Node(node.clone()));
            cx.position = 1;
            cx.size = 1;
            cx.current = cx.item.clone();
        }
        cx.mode = self.options.initial_mode.clone();
        // §19.1: "The instructions in the initial template are evaluated in
        // final output state."
        cx.final_output = true;

        let result = self.run_entry(
            &mut cx,
            &mut results::InitialResult {
                out: &mut out,
                written: &self.primary_written,
            },
        );
        match result {
            Ok(()) => {}
            Err(e) if e.is_termination() => {
                // `terminate="yes"`: the message is kept, the result is not.
                // `is_termination` rather than a comparison against the code
                // string, so that a stylesheet cannot reach this arm by giving
                // its own `fn:error` QName the local name `XTMM9000`.
                return Err(e);
            }
            Err(e) => return Err(e),
        }

        out.end_document()?;
        let (mut doc, disabled_escaping) = out.into_inner().into_inner().finish_with_marks()?;
        // §2.4 makes the tree created implicitly an `xsl:result-document
        // href=""`, and §19.1: "The base URI of the document node at the root
        // of the final result tree is based on the effective value of the href
        // attribute. If the effective value is a relative URI, then it is
        // resolved relative to the base output URI."
        if !base_output_uri.is_empty() {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(&base_output_uri)));
        }
        let id = self.store.borrow_mut().insert(doc, None);
        let implicit = self.store.borrow().get(id).expect("just inserted").doc;
        let delivered = self.deliver((implicit, disabled_escaping))?;
        let (primary, disabled_escaping) = match delivered.primary {
            Some((doc, marks)) => (Some(doc), marks),
            None => (None, crate::serialize::EscapingMarks::default()),
        };
        Ok(Outcome {
            primary,
            messages: self.take_messages(),
            disabled_escaping,
            base_output_uri,
            secondary: delivered.secondary,
            primary_output: delivered.primary_output,
        })
    }

    fn run_entry(
        &self,
        cx: &mut XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        // > [ERR XTDE0047] It is a non-recoverable dynamic error if the
        // > invocation of the stylesheet specifies both an initial mode and an
        // > initial template.
        //
        // Checked before XTDE0040 and XTDE0045: the invocation is wrong however
        // well-formed the names it carries are.
        if self.options.initial_template.is_some() && self.options.initial_mode != ModeKey::Default
        {
            return Err(XsltError::new(
                codes::XTDE0047,
                "the invocation specifies both an initial mode and an initial template",
            ));
        }
        // > [ERR XTDE0050] It is a non-recoverable dynamic error if the
        // > stylesheet that is invoked declares a visible stylesheet parameter
        // > with required="yes" and no value for this parameter is supplied
        // > during the invocation of the stylesheet.
        //
        // Checked here, at the invocation, and not when the parameter is first
        // read: globals are evaluated lazily (§9.5), so one that is never
        // referenced would never be checked (`initial-mode-003`). The globals
        // table holds the visible declaration of each name only.
        {
            let params = self.params.borrow();
            if let Some(missing) = self
                .sheet
                .globals()
                .iter()
                .find(|g| g.is_param && g.required && !params.contains_key(&g.name))
            {
                return Err(XsltError::new(
                    codes::XTDE0050,
                    format!(
                        "no value was supplied for the required stylesheet parameter {}",
                        missing.name
                    ),
                )
                .or_location(&missing.loc));
            }
        }
        if let Some(name) = self.options.initial_template.clone() {
            let id = self.sheet.named_template(&name).ok_or_else(|| {
                XsltError::new(
                    codes::XTDE0040,
                    format!("there is no template named {name} to start from"),
                )
            })?;
            return self.invoke_as(id, Vec::new(), cx, out, Invocation::InitialTemplate);
        }
        if cx.item.is_none() {
            // A template named `xsl:initial-template` is the conventional
            // default entry point when no context item is supplied; several
            // test stylesheets rely on it.
            let default_entry = QName::new(XSLT_NS, "initial-template");
            if let Some(id) = self.sheet.named_template(&default_entry) {
                return self.invoke_as(id, Vec::new(), cx, out, Invocation::InitialTemplate);
            }
            return Err(XsltError::new(
                codes::XTDE0040,
                "the transformation has neither an initial context node nor an initial template",
            ));
        }
        // §6.5: an initial mode that no template declares is XTDE0045.
        if self.options.initial_mode != ModeKey::Default
            && !self.sheet.has_mode(&self.options.initial_mode)
        {
            return Err(XsltError::new(
                codes::XTDE0045,
                "the initial mode matches no template in the stylesheet",
            ));
        }
        let item = cx.item.clone().expect("checked");
        self.apply_to_items(vec![item], cx.mode.clone(), Vec::new(), cx, out)
    }
}
