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

mod content;
mod context;
mod dispatch;
mod documents;
mod eval;
mod expressions;
mod functions;
mod patterns;
mod serialize;
mod variables;

pub use content::SimpleDefault;
pub use context::{Frame, XsltContext};
pub use functions::XsltEvaluator;
pub use serialize::{serialize_options, serialize_result, source_options, unknown_location};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::BufferDocument;
use xsd_schema::navigator::DomNavigator;
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
    /// Stack size of the thread [`run_on_large_stack`] uses (Q33).
    pub stack_size: usize,
    /// How deep template invocation may nest before the run is abandoned.
    ///
    /// M1 has no tail-call elimination, so a recursive named template consumes
    /// host stack — about 5 KiB per call — and a runaway recursion would
    /// otherwise abort the process, which a conformance harness cannot catch.
    /// The default is chosen to stay inside [`DEFAULT_STACK_SIZE`].
    pub max_recursion_depth: usize,
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
            base_uri: None,
        }
    }
}

/// The stack a transformation runs on by default (Q33).
///
/// A deeply recursive named template consumes host stack, and M1 has no
/// tail-call elimination. Running the whole job on a thread with a large stack
/// is the stopgap; see [`run_on_large_stack`]. Measured on this stack: a
/// recursive named template reaches **50 000** levels and overflows before
/// 55 000, i.e. about 5 KiB of stack per call; 1 GiB reaches 100 000.
pub const DEFAULT_STACK_SIZE: usize = 256 * 1024 * 1024;

/// The template-invocation depth a run allows by default.
///
/// Set below the depth [`DEFAULT_STACK_SIZE`] can carry, so that a runaway
/// recursion ends in an error the caller can catch rather than in a process
/// abort the caller cannot.
pub const DEFAULT_MAX_RECURSION_DEPTH: usize = 40_000;

/// Runs `job` on a thread with `stack_size` bytes of stack and returns its
/// result (Q33).
///
/// The compiled stylesheet, the name table and the arena are all `!Send`, so
/// the *whole* job — compile, load, transform, serialize — has to happen
/// inside the closure; only owned, `Send` values cross the boundary.
pub fn run_on_large_stack<R, F>(stack_size: usize, job: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(stack_size)
        .spawn(job)
        .expect("a worker thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
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
    messages: RefCell<Vec<Message>>,
    /// Where an XSLT error raised inside the XPath engine is parked: an
    /// `XPathError` has no "host error with a code" variant, so the evaluator
    /// stores the real error here and returns a placeholder (M0b finding 4).
    parked_error: RefCell<Option<XsltError>>,
    params: RefCell<HashMap<QName, XPathValue<Nav<'t>>>>,
    /// `BufferDocument::serial()` of every tree already known to carry no type
    /// annotation — because this run parsed it, or because the §21.1 walk has
    /// been done. The XTDE1665 check is one walk per externally supplied
    /// document, and this is what keeps it to one.
    annotation_free: RefCell<HashSet<u64>>,
    resolver: Arc<dyn Resolver>,
    options: TransformOptions,
    /// The focus global variables are evaluated with (§9.1): the root of the
    /// tree containing the initial context node.
    initial_root: RefCell<Option<Nav<'t>>>,
    terminated: RefCell<bool>,
    /// How deep template invocation is nested right now.
    depth: std::cell::Cell<usize>,
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
            messages: RefCell::new(Vec::new()),
            parked_error: RefCell::new(None),
            params: RefCell::new(HashMap::new()),
            annotation_free: RefCell::new(HashSet::new()),
            resolver: Arc::new(FileResolver),
            options,
            initial_root: RefCell::new(None),
            terminated: RefCell::new(false),
            depth: std::cell::Cell::new(0),
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

    fn run(&self, initial: Option<Nav<'t>>) -> Result<Outcome<'t>, XsltError> {
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
            Err(e) if e.code == codes::XTMM9000 => {
                // `terminate="yes"`: the message is kept, the result is not.
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
