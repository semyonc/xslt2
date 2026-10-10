//! The transformation: the dynamic context of §5.4/§9.1, template dispatch
//! (§6), and the evaluation of the instruction tree.
//!
//! Everything a run needs that outlives a single call — the document store,
//! the lazily evaluated globals, the message log, the error side channel —
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
mod functions;
mod grouping;
mod keys;
mod patterns;
mod serialize;
mod sort;
mod variables;

pub use content::SimpleDefault;
pub use context::{Frame, XsltContext};
pub use functions::XsltEvaluator;
pub use serialize::{serialize_options, serialize_result, source_options, unknown_location};

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
use crate::resolver::{FileResolver, Resolver};
use crate::store::{DocId, DocumentStore};

/// What a caller can change about one run.
pub struct TransformOptions {
    /// `xsl:template name="…"` to start from, instead of matching the initial
    /// context node.
    pub initial_template: Option<QName>,
    /// The mode the initial `xsl:apply-templates` uses.
    pub initial_mode: ModeKey,
    /// Signal recoverable errors instead of taking the spec's recovery action.
    pub strict_recoverable: bool,
    /// A stack size for the host to pass to [`run_on_large_stack`] (Q33).
    ///
    /// The transformation does not read it: [`run_on_large_stack`] takes its
    /// size as an argument, and the stack guard learns the size of the thread
    /// from there (see [`Self::stack_limit`]).
    pub stack_size: usize,
    /// How deep template invocations and stylesheet-function calls may nest
    /// before the run is abandoned with an `XSLT2-INTERNAL` error (Q33).
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
    /// [`Self::max_recursion_depth`] (Q33).
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
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            initial_template: None,
            initial_mode: ModeKey::Default,
            strict_recoverable: false,
            stack_size: DEFAULT_STACK_SIZE,
            max_recursion_depth: DEFAULT_MAX_RECURSION_DEPTH,
            stack_limit: None,
            base_uri: None,
        }
    }
}

/// The stack a transformation runs on by default (Q33).
///
/// A deeply recursive named template consumes host stack, and there is no
/// tail-call elimination. Running the whole job on a thread with a large stack
/// is the stopgap; see [`run_on_large_stack`]. Measured on this stack in a
/// release build (M2e): the recursive named template of `tests/q33_depth.rs`
/// uses about 7.5 KB per level, so the stack guard
/// ([`TransformOptions::stack_limit`], about 224 MiB here) stops it with the
/// depth error at about 31 000 levels; without the guard it overflowed between
/// 35 000 and 36 000. (M1 measured 50 000 levels, about 5 KiB per call; the
/// frames have grown since.)
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
    /// [`TransformOptions::stack_limit`] (Q33). `None` on every other thread.
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
/// result (Q33).
///
/// The compiled stylesheet, the name table and the arena are all `!Send`, so
/// the *whole* job — compile, load, transform, serialize — has to happen
/// inside the closure; only owned, `Send` values cross the boundary.
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
pub struct Message {
    pub content: String,
    pub terminate: bool,
}

/// What a run produced.
pub struct Outcome<'t> {
    /// The primary result tree.
    pub primary: Option<&'t BufferDocument<'t>>,
    /// `xsl:message` output, in order.
    pub messages: Vec<Message>,
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
pub struct Transformation<'a, 't> {
    sheet: &'a CompiledStylesheet<'a>,
    store: RefCell<DocumentStore<'t>>,
    globals: RefCell<Vec<GlobalSlot<'t>>>,
    /// The key indexes (§16.3, DESIGN §9.7), one per key and document — and,
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
    /// What the call of each `id()` / `key()` pattern selects, per document
    /// (§5.5.2), computed on first use.
    id_key_selections: RefCell<patterns::IdKeySelections<'t>>,
    /// The sibling sequences positional pattern predicates were evaluated
    /// against, per predicate and parent (§5.5.3), remembered for the run.
    placements: RefCell<patterns::PlacementCache>,
    messages: RefCell<Vec<Message>>,
    /// Where an XSLT error raised inside the XPath engine is parked: an
    /// `XPathError` has no "host error with a code" variant, so the evaluator
    /// stores the real error here and returns a placeholder (M0b finding 4).
    parked_error: RefCell<Option<XsltError>>,
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
    /// The stack guard of the current run (Q33).
    stack: std::cell::Cell<StackGuard>,
    /// The current dateTime and implicit timezone of this transformation,
    /// fixed the first time an expression is evaluated (§5.4.3.2).
    clock: std::cell::OnceCell<Option<(DateTimeValue, TimezoneOffset)>>,
    #[allow(clippy::type_complexity)]
    message_sink: Option<Box<dyn Fn(&Message)>>,
}

impl<'a> CompiledStylesheet<'a> {
    /// A run of this stylesheet over `arena`.
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
            store: RefCell::new(DocumentStore::new(arena, self.names())),
            globals: RefCell::new(globals),
            keys: RefCell::new(keys::KeyIndexes::default()),
            scope_serials: std::cell::Cell::new(0),
            regexes: RefCell::new(analyze::RegexCache::default()),
            id_key_selections: RefCell::new(HashMap::new()),
            placements: RefCell::new(patterns::PlacementCache::default()),
            messages: RefCell::new(Vec::new()),
            parked_error: RefCell::new(None),
            params: RefCell::new(HashMap::new()),
            resolver: Arc::new(FileResolver),
            options,
            initial_root: RefCell::new(None),
            terminated: RefCell::new(false),
            depth: std::cell::Cell::new(0),
            stack: std::cell::Cell::new(StackGuard::default()),
            clock: std::cell::OnceCell::new(),
            message_sink: None,
        }
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// Supplies a stylesheet parameter (§9.5).
    pub fn with_param(self, name: QName, value: XPathValue<Nav<'t>>) -> Self {
        self.params.borrow_mut().insert(name, value);
        self
    }

    /// Where `document()` and `doc()` read from.
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
    pub fn transform_document(&self, source: DocId) -> Result<Outcome<'t>, XsltError> {
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
        // Q33: everything the run evaluates happens below this frame, so the
        // stack it uses is measured from here.
        let _stack = self.stack_scope(stack_position());
        // §21.1 XTDE1665, before anything can observe a typed node: "the input
        // to the processor" is the tree containing the initial context node and
        // every tree passed as a stylesheet parameter. Both are checked here,
        // once per document.
        if let Some(node) = &initial {
            self.check_input_annotations(node)?;
        }
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
            self.check_input_annotations(node)?;
        }

        *self.initial_root.borrow_mut() = initial.clone().map(|mut n| {
            n.move_to_root();
            n
        });

        let builder = ResultBuilder::new(
            self.store.borrow().arena(),
            self.store.borrow().names(),
            None,
        )?;
        let mut out = ComplexContentNormalizer::new(builder);
        out.start_document()?;

        let mut cx = XsltContext::empty();
        if let Some(node) = initial {
            cx.item = Some(XmlItem::Node(node.clone()));
            cx.position = 1;
            cx.size = 1;
            cx.current = cx.item.clone();
        }
        cx.mode = self.options.initial_mode.clone();

        let result = self.run_entry(&mut cx, &mut out);
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
        let doc = out.into_inner().finish()?;
        let id = self.store.borrow_mut().insert(doc, None);
        let primary = self.store.borrow().get(id).map(|d| d.doc);
        Ok(Outcome {
            primary,
            messages: self.take_messages(),
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
