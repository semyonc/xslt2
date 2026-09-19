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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::document::{BufferDocument, BufferDocumentOptions};
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};
use xsd_schema::types::{XmlAtomicValue, XmlTypeCode, XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::{
    atomize_sequence, effective_boolean_value, BuiltinEvaluator, FunctionEvaluator,
    FunctionHandle, XPathValue,
};
use xsd_schema::xpath::{DynamicContext, XPathError, XmlItem};

use crate::error::{codes, Location, XsltError};
use crate::functions::XsltFn;
use crate::model::*;
use crate::names::{is_ncname, split_qname, QName, XML_NS, XSLT_NS};
use crate::output::OutputMethod;
use crate::pattern::{Anchor, Connector, NodeTest, PathPattern, Pattern, PatternAxis, PatternStep};
use crate::receiver::{
    ComplexContentNormalizer, CopyNamespaces, Name, Nav, Receiver, ResultBuilder,
    SequenceCollector,
};
use crate::resolver::{self, FileResolver, Resolver};
use crate::whitespace;
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

/// A variable frame: slot-indexed, as the compiler numbered them (§7.5).
#[derive(Default)]
pub struct Frame<'t> {
    slots: Vec<Option<XPathValue<Nav<'t>>>>,
}

impl<'t> Frame<'t> {
    pub fn with_slots(n: usize) -> Self {
        Self { slots: (0..n).map(|_| None).collect() }
    }

    pub fn set(&mut self, slot: usize, value: XPathValue<Nav<'t>>) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }
        self.slots[slot] = Some(value);
    }

    pub fn get(&self, slot: usize) -> Option<&XPathValue<Nav<'t>>> {
        self.slots.get(slot).and_then(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Tunnel parameters (§10.1.2): a persistent map that travels through every
/// call until something overrides it.
type TunnelMap<'t> = HashMap<QName, XPathValue<Nav<'t>>>;

/// The XSLT half of the dynamic context (§5.4, §9.1).
#[derive(Clone)]
pub struct XsltContext<'t> {
    /// The focus: context item, position and size.
    pub item: Option<XmlItem<Nav<'t>>>,
    pub position: usize,
    pub size: usize,
    /// What `current()` returns — the focus of the innermost instruction, not
    /// of the expression being evaluated (§16.6.2).
    pub current: Option<XmlItem<Nav<'t>>>,
    pub mode: ModeKey,
    /// The current template rule (§5.4) — one *alternative* of a declaration's
    /// pattern, because that is what §6.4 calls a template rule.
    pub current_rule: Option<RuleId>,
    pub tunnel: Rc<TunnelMap<'t>>,
}

impl<'t> XsltContext<'t> {
    fn empty() -> Self {
        Self {
            item: None,
            position: 0,
            size: 0,
            current: None,
            mode: ModeKey::Default,
            current_rule: None,
            tunnel: Rc::new(TunnelMap::default()),
        }
    }

    /// The transitions of §9.1 that replace the focus: `xsl:for-each`,
    /// `xsl:apply-templates` and the built-in rules all set `current()` to the
    /// new focus item.
    fn with_focus(&self, item: XmlItem<Nav<'t>>, position: usize, size: usize) -> Self {
        Self {
            item: Some(item.clone()),
            position,
            size,
            current: Some(item),
            ..self.clone()
        }
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
        let globals = (0..self.globals().len()).map(|_| GlobalSlot::Unevaluated).collect();
        Transformation {
            sheet: self,
            store: RefCell::new(DocumentStore::new(arena, self.names())),
            globals: RefCell::new(globals),
            messages: RefCell::new(Vec::new()),
            parked_error: RefCell::new(None),
            params: RefCell::new(HashMap::new()),
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

    /// Parses `xml` as the document named `uri` and registers it, applying
    /// `xsl:strip-space` (§4.4).
    pub fn load_document(&self, uri: &str, xml: &str) -> Result<DocId, XsltError> {
        self.admit(Some(uri), xml)
    }

    /// The **one** way a source document enters the store.
    ///
    /// §4.4: "For the purposes of this section, the term source tree means the
    /// document containing the initial context node, and any document returned
    /// by the functions `document`, `doc`, or `collection`." Every one of those
    /// arrives here — `transform_source`, `transform_uri`, `document()`,
    /// `doc()`, a registered source — so whitespace stripping and the
    /// document's own base URI are applied exactly once, in one place, and no
    /// future loader can forget them.
    fn admit(&self, uri: Option<&str>, xml: &str) -> Result<DocId, XsltError> {
        if let Some(uri) = uri {
            if let Some(id) = self.store.borrow().id_of_uri(uri) {
                return Ok(id);
            }
        }
        // §9.1: a re-entrant load of the same URI is XTDE1170, not a hang.
        if let Some(uri) = uri {
            self.store.borrow_mut().enter_load(uri)?;
        }
        let parsed = self.store.borrow().parse(xml, uri);
        if let Some(uri) = uri {
            self.store.borrow_mut().leave_load(uri);
        }
        let parsed = parsed?;
        let doc = self.strip_source(parsed, uri)?;
        let stripped = !std::ptr::eq(doc, parsed);
        if let Some(uri) = uri {
            self.store.borrow_mut().register_source(uri, xml);
        }
        let id = self.store.borrow_mut().insert_ref(doc, uri);
        if stripped {
            self.store.borrow_mut().mark_stripped(id);
        }
        Ok(id)
    }

    /// §4.4 Stripping Whitespace from a Source Tree.
    ///
    /// > The set of whitespace-preserving element names is determined by the
    /// > `xsl:strip-space` and `xsl:preserve-space` declarations … A whitespace
    /// > text node is preserved if either of the following apply: The element
    /// > name of the parent of the text node is in the set of
    /// > whitespace-preserving element names. An ancestor element of the text
    /// > node has an `xml:space` attribute with a value of `preserve`, and no
    /// > closer ancestor element has `xml:space` with a value of `default`.
    ///
    /// The tree is rebuilt without the stripped text nodes, because a
    /// `BufferDocument` is immutable once built and a navigator cannot hide
    /// nodes. The original is never registered, so nothing can reach it.
    fn strip_source(
        &self,
        source: &'t BufferDocument<'t>,
        uri: Option<&str>,
    ) -> Result<&'t BufferDocument<'t>, XsltError> {
        if self.sheet.space_rules().is_empty() {
            return Ok(source);
        }
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::new(arena, names, None)?;
        let mut out = ComplexContentNormalizer::new(builder);
        out.start_document()?;
        let mut child = source.create_navigator();
        if child.move_to_first_child() {
            loop {
                self.copy_stripped(&child, false, &mut out)?;
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        out.end_document()?;
        let mut doc = out.into_inner().finish()?;
        // The copy stands for the same document, so it keeps its base URI.
        if let Some(uri) = uri {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(uri)));
        }
        Ok(arena.alloc(doc))
    }

    fn copy_stripped(
        &self,
        nav: &Nav<'t>,
        preserve_space: bool,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match nav.node_type() {
            DomNodeType::Element => {
                let preserve = match attribute_value(nav, XML_NS, "space") {
                    Some(value) => whitespace::trim(&value) == "preserve",
                    None => preserve_space,
                };
                out.start_element(&Name::of(nav), None, false)?;
                let mut ns = nav.clone();
                if ns.move_to_first_namespace(NamespaceAxisScope::ExcludeXml) {
                    loop {
                        let (prefix, uri) = (ns.local_name().to_string(), ns.value());
                        out.namespace(&prefix, &uri)?;
                        if !ns.move_to_next_namespace(NamespaceAxisScope::ExcludeXml) {
                            break;
                        }
                    }
                }
                let mut attr = nav.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        let value = attr.value();
                        out.attribute(&Name::of(&attr), &value, None)?;
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
                out.end_start_tag()?;
                let strip_children = !preserve && self.strips(nav);
                let mut child = nav.clone();
                if child.move_to_first_child() {
                    loop {
                        // §4: a whitespace text node is one whose content is
                        // *XML* whitespace — #x9, #xA, #xD, #x20 — and nothing
                        // else. `char::is_whitespace` would also strip a
                        // no-break space, which is ordinary character data.
                        let drop = strip_children
                            && child.node_type().is_text_like()
                            && whitespace::is_all_xml_whitespace(&child.value());
                        if !drop {
                            self.copy_stripped(&child, preserve, out)?;
                        }
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
                out.end_element()
            }
            DomNodeType::Comment => out.comment(&nav.value()),
            DomNodeType::ProcessingInstruction => {
                let (target, data) = (nav.local_name().to_string(), nav.value());
                out.pi(&target, &data)
            }
            _ => out.text(&nav.value(), false),
        }
    }

    /// Whether whitespace-only text children of this element are stripped.
    ///
    /// §4.4 resolves conflicts between `xsl:strip-space` and
    /// `xsl:preserve-space` "using the rules for conflict resolution of
    /// template rules": import precedence first, then priority.
    fn strips(&self, nav: &Nav<'t>) -> bool {
        let (uri, local) = (nav.namespace_uri(), nav.local_name());
        let mut best: Option<&SpaceRule> = None;
        for rule in self.sheet.space_rules() {
            if !rule.test.matches(uri, local) {
                continue;
            }
            let better = match best {
                None => true,
                Some(current) => {
                    (rule.precedence, rule.priority, rule.decl_order)
                        > (current.precedence, current.priority, current.decl_order)
                }
            };
            if better {
                best = Some(rule);
            }
        }
        best.is_some_and(|rule| rule.strip)
    }

    /// Registers text a later `document()` call should find at `uri`.
    pub fn register_source(&self, uri: &str, xml: &str) {
        self.store.borrow_mut().register_source(uri, xml);
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
        Ok(Outcome { primary, messages: self.take_messages() })
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

    // ── Instruction evaluation ───────────────────────────────────────────

    fn eval_body(
        &self,
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        for instruction in &body.0 {
            self.eval(instruction, frame, cx, out)?;
        }
        Ok(())
    }

    fn eval(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        self.eval_inner(instruction, frame, cx, out)
            .map_err(|e| e.or_location(&instruction.location(self.sheet)))
    }

    fn eval_inner(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match &instruction.kind {
            InstructionKind::Text { text, .. } => out.text(text, false),

            InstructionKind::ValueOf { value, separator, first_item_only, .. } => {
                let separator = match separator {
                    Some(avt) => Some(self.avt(avt, frame, cx)?),
                    None => None,
                };
                let text = self.simple_content(
                    value,
                    separator,
                    frame,
                    cx,
                    *first_item_only,
                    SimpleDefault::of(value),
                )?;
                out.text(&text, false)
            }

            InstructionKind::Sequence { select } => {
                let value = match select {
                    Some(id) => self.eval_expr(*id, frame, cx)?,
                    None => XPathValue::empty(),
                };
                for item in value.into_vec() {
                    out.append_item(&item)?;
                }
                Ok(())
            }

            InstructionKind::If { test, then } => {
                if self.ebv(*test, frame, cx)? {
                    self.eval_body(then, frame, cx, out)?;
                }
                Ok(())
            }

            InstructionKind::Choose { whens, otherwise } => {
                for (test, body) in whens {
                    if self.ebv(*test, frame, cx)? {
                        return self.eval_body(body, frame, cx, out);
                    }
                }
                match otherwise {
                    Some(body) => self.eval_body(body, frame, cx, out),
                    None => Ok(()),
                }
            }

            InstructionKind::ForEach { select, body } => {
                let items = self.eval_expr(*select, frame, cx)?.into_vec();
                let size = items.len();
                for (index, item) in items.into_iter().enumerate() {
                    // §5.4: the focus is replaced and the frame is the same,
                    // but "when an xsl:for-each … instruction is evaluated …
                    // the current template rule becomes null for the
                    // evaluation of that instruction", so an xsl:next-match
                    // inside one is XTDE0560.
                    let mut inner = cx.with_focus(item, index + 1, size);
                    inner.current_rule = None;
                    self.eval_body(body, frame, &inner, out)?;
                }
                Ok(())
            }

            InstructionKind::Variable { slot, value, as_type, .. } => {
                let value = self.var_value(value, as_type.as_ref(), frame, cx, codes::XTTE0570)?;
                frame.set(*slot, value);
                Ok(())
            }

            InstructionKind::ApplyTemplates { select, mode, params } => {
                let items = match select {
                    Some(id) => self.eval_expr(*id, frame, cx)?.into_vec(),
                    None => self.children_of_focus(cx)?,
                };
                let mode = match mode {
                    ModeRef::Default => ModeKey::Default,
                    ModeRef::Current => cx.mode.clone(),
                    ModeRef::Named(name) => ModeKey::Named(name.clone()),
                };
                let params = self.eval_with_params(params, frame, cx)?;
                self.apply_to_items(items, mode, params, cx, out)
            }

            InstructionKind::CallTemplate { name, params } => {
                let id = self.sheet.named_template(name).ok_or_else(|| {
                    XsltError::new(
                        codes::XTSE0650,
                        format!("there is no template named {name}"),
                    )
                })?;
                // §10.1.1 / §9.10 row 9: a parameter the callee does not
                // declare is a static error, unless backwards-compatible
                // behaviour is enabled for the xsl:call-template instruction.
                if !self.sheet.is_backwards(instruction.ctx) {
                    let rule = self.sheet.template(id);
                    for param in params {
                        if !param.tunnel
                            && !rule.params.iter().any(|p| p.name == param.name)
                        {
                            return Err(XsltError::new(
                                codes::XTSE0680,
                                format!(
                                    "the template {name} has no parameter named {}",
                                    param.name
                                ),
                            ));
                        }
                    }
                }
                let params = self.eval_with_params(params, frame, cx)?;
                self.call_template(id, params, cx, out)
            }

            InstructionKind::LiteralElement { name, namespaces, attrs, attr_sets, body } => {
                out.start_element(name, None, false)?;
                for (prefix, uri) in namespaces {
                    out.namespace(prefix, uri)?;
                }
                self.attribute_sets(attr_sets, frame, cx, out)?;
                for (attr_name, avt) in attrs {
                    let value = self.avt(avt, frame, cx)?;
                    out.attribute(attr_name, &value, None)?;
                }
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }

            InstructionKind::Element { name, namespace, attr_sets, body, .. } => {
                let lexical = self.avt(name, frame, cx)?;
                let (prefix, local) = split_qname(whitespace::trim(&lexical));
                if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0820,
                        format!("{lexical:?} is not a lexical QName"),
                    ));
                }
                let uri = match namespace {
                    Some(avt) => self.avt(avt, frame, cx)?,
                    None => self
                        .static_prefix(instruction, prefix)
                        .ok_or_else(|| {
                            XsltError::new(
                                codes::XTDE0830,
                                format!("the prefix {prefix:?} of {lexical:?} is not in scope"),
                            )
                        })?,
                };
                let prefix = if uri.is_empty() { "" } else { prefix };
                out.start_element(&Name::qualified(prefix, local, &uri), None, false)?;
                self.attribute_sets(attr_sets, frame, cx, out)?;
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }

            InstructionKind::Attribute { name, namespace, separator, value } => {
                let lexical = self.avt(name, frame, cx)?;
                let (prefix, local) = split_qname(whitespace::trim(&lexical));
                if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0850,
                        format!("{lexical:?} is not a lexical QName"),
                    ));
                }
                if namespace.is_none() && whitespace::trim(&lexical) == "xmlns" {
                    return Err(XsltError::new(
                        codes::XTDE0855,
                        "an attribute may not be named xmlns",
                    ));
                }
                let uri = match namespace {
                    Some(avt) => self.avt(avt, frame, cx)?,
                    None if prefix.is_empty() => String::new(),
                    None => self.static_prefix(instruction, prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0860,
                            format!("the prefix {prefix:?} of {lexical:?} is not in scope"),
                        )
                    })?,
                };
                let separator = match separator {
                    Some(avt) => Some(self.avt(avt, frame, cx)?),
                    None => None,
                };
                let text = self.simple_content(
                    value,
                    separator,
                    frame,
                    cx,
                    false,
                    SimpleDefault::of(value),
                )?;
                let prefix = if uri.is_empty() { "" } else { prefix };
                out.attribute(&Name::qualified(prefix, local, &uri), &text, None)
            }

            InstructionKind::Namespace { name, value } => {
                let prefix = self.avt(name, frame, cx)?;
                let prefix = whitespace::trim(&prefix);
                if prefix == "xmlns" || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0920,
                        format!("{prefix:?} is not a namespace prefix"),
                    ));
                }
                let uri =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                if uri.is_empty() {
                    return Err(XsltError::new(
                        codes::XTDE0930,
                        "xsl:namespace must not produce a zero-length namespace URI",
                    ));
                }
                if (prefix == "xml") != (uri == XML_NS) {
                    return Err(XsltError::new(
                        codes::XTDE0925,
                        "the prefix xml may only be bound to the XML namespace, and vice versa",
                    ));
                }
                out.namespace(prefix, &uri)
            }

            InstructionKind::Comment { value } => {
                let text =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                out.comment(&text)
            }

            InstructionKind::ProcessingInstruction { name, value } => {
                let target = self.avt(name, frame, cx)?;
                let target = whitespace::trim(&target);
                if !is_ncname(target) || target.eq_ignore_ascii_case("xml") {
                    return Err(XsltError::new(
                        codes::XTDE0890,
                        format!("{target:?} is not a processing-instruction target"),
                    ));
                }
                let text =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                out.pi(target, whitespace::trim_start(&text))
            }

            InstructionKind::Document { body } => {
                // §11.5: a new document node whose content is the sequence
                // constructor; in element content it contributes its children.
                out.start_document()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_document()
            }

            InstructionKind::Copy { copy_namespaces, attr_sets, body, .. } => {
                self.copy_shallow(*copy_namespaces, attr_sets, body, frame, cx, out)
            }

            InstructionKind::CopyOf { select, copy_namespaces } => {
                let value = self.eval_expr(*select, frame, cx)?;
                let scope = copy_scope(*copy_namespaces);
                for item in value.into_vec() {
                    // §11.9.2: every node of the selected sequence is
                    // *constructed anew*, so this is `append_copy`, never
                    // `append_item` — the difference only shows in a sequence
                    // collector, where `append_item` would keep the original.
                    out.append_copy(&item, scope)?;
                }
                Ok(())
            }

            InstructionKind::Message { select, body, terminate } => {
                let terminate = self.avt(terminate, frame, cx)?;
                let terminate = crate::names::parse_yes_no(&terminate, "terminate")?;
                let content = self.message_content(*select, body, frame, cx)?;
                let message = Message { content, terminate };
                if let Some(sink) = &self.message_sink {
                    sink(&message);
                }
                self.messages.borrow_mut().push(message);
                if terminate {
                    *self.terminated.borrow_mut() = true;
                    return Err(XsltError::new(
                        codes::XTMM9000,
                        "the transformation was terminated by xsl:message terminate=\"yes\"",
                    ));
                }
                Ok(())
            }

            // §18.2.3: an xsl:fallback that is reached normally does nothing.
            InstructionKind::Fallback => Ok(()),

            InstructionKind::ApplyImports { params } => {
                let params = self.eval_with_params(params, frame, cx)?;
                self.override_rule(true, params, cx, out)
            }

            InstructionKind::NextMatch { params } => {
                let params = self.eval_with_params(params, frame, cx)?;
                self.override_rule(false, params, cx, out)
            }

            InstructionKind::NotImplemented { feature, milestone } => {
                Err(XsltError::not_implemented(feature, milestone))
            }
        }
    }

    /// §11.9.1 `xsl:copy`.
    fn copy_shallow(
        &self,
        copy_namespaces: bool,
        attr_sets: &[QName],
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let Some(item) = cx.item.clone() else {
            return Err(XsltError::new(
                "XPDY0002",
                "xsl:copy has no context item to copy",
            ));
        };
        let nav = match item {
            // "When the context item is an atomic value, the xsl:copy
            // instruction returns this value."
            XmlItem::Atomic(value) => return out.append_item(&XmlItem::Atomic(value)),
            XmlItem::Node(nav) => nav,
        };
        match nav.node_type() {
            DomNodeType::Root => {
                out.start_document()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_document()
            }
            DomNodeType::Element => {
                out.start_element(&Name::of(&nav), None, false)?;
                if copy_namespaces {
                    let mut ns = nav.clone();
                    if ns.move_to_first_namespace(NamespaceAxisScope::ExcludeXml) {
                        loop {
                            let (prefix, uri) = (ns.local_name().to_string(), ns.value());
                            out.namespace(&prefix, &uri)?;
                            if !ns.move_to_next_namespace(NamespaceAxisScope::ExcludeXml) {
                                break;
                            }
                        }
                    }
                }
                self.attribute_sets(attr_sets, frame, cx, out)?;
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }
            // "the xsl:copy instruction returns a new node that is a copy of
            // the context node … The sequence constructor, if present, is not
            // evaluated." A *new* node: `append_copy`, so that a copy landing
            // in a sequence is parentless rather than the original node.
            _ => out.append_copy(&XmlItem::Node(nav), copy_scope(copy_namespaces)),
        }
    }

    fn attribute_sets(
        &self,
        names: &[QName],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let mut seen: Vec<QName> = Vec::new();
        self.attribute_sets_inner(names, &mut seen, frame, cx, out)
    }

    fn attribute_sets_inner(
        &self,
        names: &[QName],
        seen: &mut Vec<QName>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        for name in names {
            if seen.contains(name) {
                return Err(XsltError::new(
                    codes::XTSE0720,
                    format!("the attribute set {name} uses itself"),
                ));
            }
            let Some(set) = self.sheet.attribute_set(name) else {
                return Err(XsltError::new(
                    codes::XTSE0710,
                    format!("there is no attribute set named {name}"),
                ));
            };
            seen.push(name.clone());
            self.attribute_sets_inner(&set.use_attribute_sets, seen, frame, cx, out)?;
            for attribute in &set.attributes {
                self.eval(attribute, frame, cx, out)?;
            }
            seen.pop();
        }
        Ok(())
    }

    // ── Template dispatch (§6) ───────────────────────────────────────────

    fn apply_to_items(
        &self,
        items: Vec<XmlItem<Nav<'t>>>,
        mode: ModeKey,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        // > [ERR XTTE0520] It is a type error if the sequence returned by the
        // > select expression contains an item that is not a node.
        if let Some(XmlItem::Atomic(value)) = items.iter().find(|i| matches!(i, XmlItem::Atomic(_))) {
            return Err(XsltError::new(
                codes::XTTE0520,
                format!(
                    "xsl:apply-templates selected an atomic value ({}) where only nodes are allowed",
                    self.atom_string(value)
                ),
            ));
        }
        let size = items.len();
        for (index, item) in items.into_iter().enumerate() {
            let mut inner = cx.with_focus(item.clone(), index + 1, size);
            inner.mode = mode.clone();
            match self.select_rule(&item, &mode)? {
                Some(id) => {
                    inner.current_rule = Some(id);
                    self.invoke(id.template, params.clone(), &inner, out)?;
                }
                None => self.builtin_rule(&item, &mode, &params, &inner, out)?,
            }
        }
        Ok(())
    }

    /// §6.6 Built-in Template Rules.
    ///
    /// > The built-in template rule for element and document nodes is
    /// > equivalent to `<xsl:template match="*|/"><xsl:apply-templates/>`,
    /// > with the additional property that parameters are passed through.
    /// > … The built-in template rule for text and attribute nodes is
    /// > equivalent to `<xsl:template match="text()|@*"><xsl:value-of
    /// > select="."/>`. … The built-in template rule for comment and
    /// > processing instruction nodes is to do nothing.
    fn builtin_rule(
        &self,
        item: &XmlItem<Nav<'t>>,
        mode: &ModeKey,
        params: &[(QName, XPathValue<Nav<'t>>, bool)],
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(value) => out.text(&self.atom_string(value), false),
            XmlItem::Node(nav) => match nav.node_type() {
                DomNodeType::Root | DomNodeType::Element => {
                    let children = children_of(nav);
                    self.apply_to_items(children, mode.clone(), params.to_vec(), cx, out)
                }
                DomNodeType::Attribute
                | DomNodeType::Text
                | DomNodeType::Whitespace
                | DomNodeType::SignificantWhitespace => out.text(&nav.value(), false),
                _ => Ok(()),
            },
        }
    }

    /// §6.4 conflict resolution: the candidate list is pre-sorted by
    /// (precedence, priority, declaration order), all descending, so the first
    /// match wins.
    ///
    /// A candidate is one *alternative* of a declaration's pattern, because
    /// "if the pattern contains multiple alternatives separated by `|`, then
    /// the template rule is treated equivalently to a set of template rules,
    /// one for each alternative".
    fn select_rule(
        &self,
        item: &XmlItem<Nav<'t>>,
        mode: &ModeKey,
    ) -> Result<Option<RuleId>, XsltError> {
        let XmlItem::Node(nav) = item else { return Ok(None) };
        let candidates = self.candidates(nav, mode);
        let mut matched: Option<RuleId> = None;
        for id in candidates {
            if self.rule_matches(id, nav)? {
                match matched {
                    None => {
                        matched = Some(id);
                        if !self.options.strict_recoverable {
                            break;
                        }
                    }
                    Some(first) => {
                        // §6.4: "However, it is not an error if a node matches
                        // more than one of the alternatives" — two rules of
                        // the *same* declaration never conflict.
                        if first.template == id.template {
                            continue;
                        }
                        let (pa, ra, _) = self.sheet.rule_rank(first);
                        let (pb, rb, _) = self.sheet.rule_rank(id);
                        if pa == pb && ra == rb {
                            // > [ERR XTRE0540] It is a recoverable dynamic
                            // > error if the conflict resolution algorithm for
                            // > template rules leaves more than one matching
                            // > template rule.
                            return Err(XsltError::new(
                                codes::XTRE0540,
                                format!(
                                    "two template rules of the same import precedence and \
                                     priority match {}",
                                    nav.name()
                                ),
                            ));
                        }
                        break;
                    }
                }
            }
        }
        Ok(matched)
    }

    /// Whether one rule — one alternative of one declaration's pattern —
    /// matches `nav`.
    fn rule_matches(&self, id: RuleId, nav: &Nav<'t>) -> Result<bool, XsltError> {
        let Some(pattern_id) = self.sheet.rule(id).pattern else { return Ok(false) };
        let pattern = self.sheet.pattern(pattern_id);
        let Some(alternative) = pattern.alternatives.get(id.alternative as usize) else {
            return Ok(false);
        };
        self.guard_recoverable(self.matches_alternative(alternative, nav, nav))
    }

    /// The rules of `mode` that could match `nav`, in conflict-resolution
    /// order (§8.5).
    ///
    /// The mode's own index and the `mode="#all"` index are both consulted:
    /// §6.5 makes an `#all` rule applicable to "every mode that is named in an
    /// `xsl:apply-templates` instruction or `xsl:template` declaration
    /// anywhere in the stylesheet", and a mode named only by an
    /// `xsl:apply-templates` has no index of its own.
    fn candidates(&self, nav: &Nav<'t>, mode: &ModeKey) -> Vec<RuleId> {
        let mut candidates: Vec<RuleId> = Vec::new();
        let mut collect = |index: &ModeIndex| {
            let name = QName::new(nav.namespace_uri(), nav.local_name());
            let buckets: [Option<&Vec<RuleId>>; 3] = match nav.node_type() {
                DomNodeType::Attribute => [
                    index.by_attribute.get(&name),
                    index.by_namespace.get(nav.namespace_uri()),
                    index.by_local.get(nav.local_name()),
                ],
                DomNodeType::Element => [
                    index.by_element.get(&name),
                    index.by_namespace.get(nav.namespace_uri()),
                    index.by_local.get(nav.local_name()),
                ],
                _ => [None, None, None],
            };
            for bucket in buckets.into_iter().flatten() {
                candidates.extend(bucket.iter().copied());
            }
            candidates.extend(index.other.iter().copied());
        };
        if let Some(index) = self.sheet.modes().index(mode) {
            collect(index);
        }
        collect(self.sheet.modes().all_modes());
        candidates.sort_by(|a, b| {
            let (pa, ra, da) = self.sheet.rule_rank(*a);
            let (pb, rb, db) = self.sheet.rule_rank(*b);
            pb.cmp(&pa)
                .then(rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal))
                .then(db.cmp(&da))
        });
        candidates.dedup();
        candidates
    }

    /// §6.7 `xsl:apply-imports` and `xsl:next-match`.
    ///
    /// > `xsl:apply-imports` … searches only the template rules that were
    /// > defined in stylesheet modules imported (directly or indirectly) by
    /// > the stylesheet module containing the current template rule …
    /// > `xsl:next-match` … considers all the template rules that were
    /// > eliminated on the grounds that they had lower import precedence or
    /// > lower priority.
    fn override_rule(
        &self,
        only_imports: bool,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let Some(current) = cx.current_rule else {
            // > [ERR XTDE0560] It is a non-recoverable dynamic error if
            // > xsl:apply-imports or xsl:next-match is evaluated when the
            // > current template rule is null.
            return Err(XsltError::new(
                codes::XTDE0560,
                "there is no current template rule to override",
            ));
        };
        let Some(XmlItem::Node(nav)) = cx.item.clone() else {
            return Err(XsltError::new(
                "XPDY0002",
                "there is no context node to re-match",
            ));
        };
        let rule = self.sheet.rule(current);
        // §6.7: "the xsl:next-match instruction considers all other template
        // rules of lower import precedence and/or priority". The comparison is
        // on the conflict-resolution key of the *current rule*, not on its
        // position in this node's candidate list — the context item may be a
        // different node from the one the rule matched, and a rank test always
        // terminates. Since §6.4 makes each `|` alternative a rule of its own,
        // a lower-priority alternative of the very same declaration is a
        // legitimate next match.
        let current_rank = self.sheet.rule_rank(current);
        for id in self.candidates(&nav, &cx.mode) {
            let candidate = self.sheet.rule(id);
            let candidate_rank = self.sheet.rule_rank(id);
            let lower = candidate_rank.0 < current_rank.0
                || (candidate_rank.0 == current_rank.0
                    && (candidate_rank.1 < current_rank.1
                        || (candidate_rank.1 == current_rank.1
                            && candidate_rank.2 < current_rank.2)));
            if !lower {
                continue;
            }
            if only_imports
                && !(candidate.precedence >= rule.subtree_min
                    && candidate.precedence < rule.precedence)
            {
                continue;
            }
            if self.rule_matches(id, &nav)? {
                let mut inner = cx.clone();
                inner.current_rule = Some(id);
                return self.invoke(id.template, params, &inner, out);
            }
        }
        // §6.7: "If there is no matching template rule, the built-in template
        // rule for the relevant node kind is used."
        self.builtin_rule(&XmlItem::Node(nav), &cx.mode, &params, cx, out)
    }

    fn invoke(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        self.invoke_as(id, params, cx, out, Invocation::ApplyTemplates)
    }

    fn invoke_as(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        let depth = self.depth.get() + 1;
        if depth > self.options.max_recursion_depth {
            return Err(XsltError::internal(format!(
                "template invocation nested more than {} deep; the stylesheet is probably \
                 recursing without end (TransformOptions::max_recursion_depth)",
                self.options.max_recursion_depth
            )));
        }
        self.depth.set(depth);
        let result = self.invoke_body(id, params, cx, out, how);
        self.depth.set(depth - 1);
        result
    }

    fn invoke_body(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        let rule = self.sheet.template(id);
        let mut frame = Frame::with_slots(rule.frame_size);
        let mut inner = cx.clone();
        // Tunnel parameters travel through every call (§10.1.2).
        let tunnelled: Vec<_> = params.iter().filter(|(_, _, t)| *t).collect();
        if !tunnelled.is_empty() {
            let mut map = (*cx.tunnel).clone();
            for (name, value, _) in tunnelled {
                map.insert(name.clone(), value.clone());
            }
            inner.tunnel = Rc::new(map);
        }
        self.bind_params(&rule.params, &params, &mut frame, &inner, how)?;
        if rule.as_type.is_some() {
            let mut collector = SequenceCollector::new(&self.store);
            self.eval_body(&rule.body, &mut frame, &inner, &mut collector)?;
            let value = XPathValue::from_sequence(collector.into_items());
            let value = self.convert(value, rule.as_type.as_ref(), codes::XTTE0505)?;
            for item in value.into_vec() {
                out.append_item(&item)?;
            }
            return Ok(());
        }
        self.eval_body(&rule.body, &mut frame, &inner, out)
    }

    fn call_template(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        // §9.1: xsl:call-template preserves the focus and the current rule.
        self.invoke_as(id, params, cx, out, Invocation::CallTemplate)
    }

    fn bind_params(
        &self,
        declared: &[ParamDecl],
        supplied: &[(QName, XPathValue<Nav<'t>>, bool)],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        for param in declared {
            let value = if param.tunnel {
                cx.tunnel.get(&param.name).cloned()
            } else {
                supplied
                    .iter()
                    .find(|(name, _, tunnel)| !*tunnel && *name == param.name)
                    .map(|(_, value, _)| value.clone())
            };
            let value = match value {
                Some(value) => {
                    self.convert(value, param.as_type.as_ref(), codes::XTTE0590)?
                }
                None if param.required => {
                    // §10.1.1 distinguishes the three ways a template can be
                    // entered: XTSE0690 for xsl:call-template (a static
                    // error), XTDE0060 for the initial template, XTDE0700 for
                    // everything else.
                    let code = match how {
                        Invocation::CallTemplate if !param.tunnel => codes::XTSE0690,
                        Invocation::InitialTemplate => codes::XTDE0060,
                        _ => codes::XTDE0700,
                    };
                    return Err(XsltError::new(
                        code,
                        format!("no value was supplied for the required parameter {}", param.name),
                    )
                    .or_location(&param.loc));
                }
                None => {
                    self.var_value(&param.value, param.as_type.as_ref(), frame, cx, codes::XTTE0590)?
                }
            };
            frame.set(param.slot, value);
        }
        Ok(())
    }

    fn eval_with_params(
        &self,
        params: &[WithParam],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<(QName, XPathValue<Nav<'t>>, bool)>, XsltError> {
        let mut out = Vec::with_capacity(params.len());
        for param in params {
            let value =
                self.var_value(&param.value, param.as_type.as_ref(), frame, cx, codes::XTTE0590)?;
            out.push((param.name.clone(), value, param.tunnel));
        }
        Ok(out)
    }

    // ── Values ───────────────────────────────────────────────────────────

    fn var_value(
        &self,
        value: &VarValue,
        as_type: Option<&TypeCheck>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        code: &'static str,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let raw = match value {
            VarValue::Select(id) => self.eval_expr(*id, frame, cx)?,
            VarValue::Sequence(body) => {
                let mut collector = SequenceCollector::new(&self.store);
                self.eval_body(body, frame, cx, &mut collector)?;
                XPathValue::from_sequence(collector.into_items())
            }
            VarValue::Tree { body, ctx: sc } => {
                let node = self.temporary_tree(body, *sc, frame, cx)?;
                XPathValue::from_node(node)
            }
            // §9.2: no select and no content means the empty sequence with
            // `as`, and a zero-length string without.
            VarValue::Empty => match as_type {
                Some(_) => XPathValue::empty(),
                None => XPathValue::string(String::new()),
            },
        };
        self.convert(raw, as_type, code)
    }

    /// §9.4: a sequence constructor with no `as` builds a temporary tree.
    ///
    /// > The base URI of the document node is taken from the base URI of the
    /// > variable binding element in the stylesheet. … The `document-uri` and
    /// > `unparsed-entities` properties of the new document node are set to
    /// > empty.
    ///
    /// The document is therefore registered with **no** URI — which is what
    /// makes `document-uri()` empty — while its base URI is recorded on the
    /// document itself.
    fn temporary_tree(
        &self,
        body: &Body,
        sc: crate::stylesheet::StaticContextId,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Nav<'t>, XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::new(arena, names, None)?;
        let mut out = ComplexContentNormalizer::new(builder);
        out.start_document()?;
        self.eval_body(body, frame, cx, &mut out)?;
        out.end_document()?;
        let mut doc = out.into_inner().finish()?;
        let base = &self.sheet.static_contexts.get(sc).base_uri;
        if !base.is_empty() {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(base)));
        }
        let id = self.store.borrow_mut().insert(doc, None);
        Ok(self.store.borrow().navigator(id))
    }

    /// The function conversion rules of XPath §3.1.5, as XSLT applies them to
    /// an `as` attribute (§9.3).
    fn convert(
        &self,
        value: XPathValue<Nav<'t>>,
        as_type: Option<&TypeCheck>,
        code: &'static str,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let Some(check) = as_type else { return Ok(value) };
        if self.instance_of(&value, check)? {
            return Ok(value);
        }
        if check.atomic {
            // XPath 2.0 §3.1.5, the function conversion rules XSLT §9.3 cites:
            // the value is atomized; an `xs:untypedAtomic` item is cast to the
            // expected type; a numeric item is promoted to `xs:double` or
            // `xs:float`, and `xs:anyURI` to `xs:string`. Nothing else is
            // converted, so a genuine type mismatch stays one.
            let atomized = atomize_sequence(value.clone())
                .map_err(|e| self.xpath_error(e))?;
            let mut items: Vec<XmlItem<Nav<'t>>> = Vec::with_capacity(atomized.len());
            for atom in atomized {
                let needs_cast = atom.type_code == XmlTypeCode::UntypedAtomic
                    || (atom.type_code.is_numeric()
                        && matches!(check.item_type.as_str(), "xs:double" | "xs:float"))
                    || (atom.type_code == XmlTypeCode::AnyUri
                        && check.item_type == "xs:string");
                let item = XPathValue::from_atomic(atom);
                let converted = match (&check.cast, needs_cast) {
                    (Some(cast), true) => match self.apply_probe(*cast, item.clone()) {
                        Ok(value) => value,
                        // A failed cast is the type error of the construct,
                        // not the raw FORG0001 of the cast.
                        Err(_) => {
                            return Err(XsltError::new(
                                code,
                                format!(
                                    "the value cannot be converted to the required type {}",
                                    check.text
                                ),
                            ))
                        }
                    },
                    _ => item,
                };
                items.extend(converted.into_vec());
            }
            let converted = XPathValue::from_sequence(items);
            if self.instance_of(&converted, check)? {
                return Ok(converted);
            }
        }
        Err(XsltError::new(
            code,
            format!("the value does not match the required type {}", check.text),
        ))
    }

    fn instance_of(
        &self,
        value: &XPathValue<Nav<'t>>,
        check: &TypeCheck,
    ) -> Result<bool, XsltError> {
        let result = self.apply_probe(check.test, value.clone())?;
        Ok(result.as_bool().unwrap_or(false))
    }

    /// Evaluates one of the helper expressions whose only variable is
    /// `$value`.
    fn apply_probe(
        &self,
        id: ExprId,
        value: XPathValue<Nav<'t>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let compiled = self.sheet.expr(id);
        let ctx = self.sheet.context(compiled.ctx);
        self.parked_error.borrow_mut().take();
        compiled
            .expr
            .evaluator(ctx)
            .run_with_node_and_setup::<Nav<'t>, _>(None, |ev| {
                ev.set_variable(0, value);
            })
            .map_err(|e| self.xpath_error(e))
    }

    // ── Expressions ──────────────────────────────────────────────────────

    fn ebv(
        &self,
        id: ExprId,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<bool, XsltError> {
        let value = self.eval_expr(id, frame, cx)?;
        effective_boolean_value(&value).map_err(|e| self.xpath_error(e))
    }

    fn eval_expr(
        &self,
        id: ExprId,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let compiled = self.sheet.expr(id);
        if let Some(why) = &compiled.unavailable {
            // §18.1.2: "When backwards compatible behavior is enabled, it is a
            // non-recoverable dynamic error to evaluate an extension function
            // call if no implementation of the extension function is
            // available."
            return Err(XsltError::new(codes::XTDE1425, why.clone()));
        }
        let ctx = self.sheet.context(compiled.ctx);
        // A parked error belongs to the evaluation that parked it; one that
        // was swallowed (a pattern predicate treated as a non-match, §5.5.4)
        // must not surface from the next unrelated XPath error.
        self.parked_error.borrow_mut().take();

        // Bind only the slots the expression actually references (§7.5).
        let mut bindings: Vec<(u32, XPathValue<Nav<'t>>)> =
            Vec::with_capacity(compiled.vars.len());
        for (slot, location) in compiled.vars.iter().enumerate() {
            if !compiled.expr.references_external_var(slot as u32) {
                continue;
            }
            let value = match location {
                VarLocation::Local(index) => frame.get(*index).cloned().unwrap_or_else(XPathValue::empty),
                VarLocation::Global(index) => self.global(*index)?,
            };
            bindings.push((slot as u32, value));
        }

        let evaluator = XsltEvaluator {
            run: self,
            cx,
            sc: self.sheet.static_contexts.get(compiled.ctx),
        };
        let item = cx.item.clone();
        let (position, size) = (cx.position, cx.size);
        let result = compiled
            .expr
            .evaluator(ctx)
            .run_with_node_and_setup::<Nav<'t>, _>(None, |ev| {
                for (slot, value) in bindings {
                    ev.set_variable(slot, value);
                }
                let dynamic = ev.context();
                dynamic.context_item = item;
                dynamic.context_position = position;
                dynamic.context_size = size;
                dynamic.set_function_evaluator(Some(&evaluator));
            });
        result.map_err(|e| self.xpath_error(e))
    }

    /// Turns an `XPathError` back into an `XsltError`, recovering an XSLT code
    /// that the evaluator parked on the way out.
    fn xpath_error(&self, e: XPathError) -> XsltError {
        if let Some(parked) = self.parked_error.borrow_mut().take() {
            return parked;
        }
        match e.error_code() {
            Some(code) => XsltError::new(code, e.to_string()),
            None => XsltError::internal(e.to_string()),
        }
    }

    /// A global variable, evaluated on first use (§9.5).
    fn global(&self, index: usize) -> Result<XPathValue<Nav<'t>>, XsltError> {
        match &self.globals.borrow()[index] {
            GlobalSlot::Value(value) => return Ok(value.clone()),
            GlobalSlot::Evaluating => {
                let name = &self.sheet.globals()[index].name;
                return Err(XsltError::new(
                    codes::XTDE0640,
                    format!("the global variable {name} depends on itself"),
                ));
            }
            GlobalSlot::Unevaluated => {}
        }
        self.globals.borrow_mut()[index] = GlobalSlot::Evaluating;
        let global = &self.sheet.globals()[index];
        let result = self.eval_global(global);
        match result {
            Ok(value) => {
                self.globals.borrow_mut()[index] = GlobalSlot::Value(value.clone());
                Ok(value)
            }
            Err(e) => {
                self.globals.borrow_mut()[index] = GlobalSlot::Unevaluated;
                Err(e.or_location(&global.loc))
            }
        }
    }

    fn eval_global(&self, global: &GlobalVar) -> Result<XPathValue<Nav<'t>>, XsltError> {
        // §9.1: a global is evaluated with a singleton focus on the root of
        // the tree containing the initial context node.
        let mut cx = XsltContext::empty();
        if let Some(root) = self.initial_root.borrow().clone() {
            cx.item = Some(XmlItem::Node(root.clone()));
            cx.position = 1;
            cx.size = 1;
            cx.current = cx.item.clone();
        }
        if global.is_param {
            if let Some(value) = self.params.borrow().get(&global.name).cloned() {
                return self.convert(value, global.as_type.as_ref(), codes::XTTE0590);
            }
            if global.required {
                return Err(XsltError::new(
                    codes::XTDE0050,
                    format!("no value was supplied for the required parameter {}", global.name),
                ));
            }
        }
        let mut frame = Frame::with_slots(global.frame_size);
        self.var_value(&global.value, global.as_type.as_ref(), &mut frame, &cx, codes::XTTE0570)
    }

    /// The string an atomized item contributes to simple content (§5.7.2
    /// step 4, "every value in the atomized sequence is cast to a string").
    ///
    /// `xs:QName` and `xs:NOTATION` need the name table to render, because a
    /// `QualifiedName` holds interned ids: upstream's `Display` prints a debug
    /// form, which would reach the result tree as an element name. Casting a
    /// QName to a string yields its lexical form, prefix and all.
    fn atom_string(&self, value: &XmlValue) -> String {
        atom_to_string(self.sheet.names(), value)
    }

    /// §5.6: the effective value of an attribute value template.
    fn avt(
        &self,
        avt: &Avt,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<String, XsltError> {
        if let Some(text) = avt.as_literal() {
            return Ok(text.to_string());
        }
        let mut out = String::new();
        for part in &avt.parts {
            match part {
                AvtPart::Literal(text) => out.push_str(text),
                AvtPart::Expr(id) => {
                    let value = self.eval_expr(*id, frame, cx)?;
                    // §5.7.2: the same simple-content rules, with a single
                    // space as the separator.
                    let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                    let strings: Vec<String> =
                        atoms.iter().map(|a| self.atom_string(a)).collect();
                    out.push_str(&strings.join(" "));
                }
            }
        }
        Ok(out)
    }

    /// §5.7.2 Constructing Simple Content.
    fn simple_content(
        &self,
        value: &ValueSource,
        separator: Option<String>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        first_item_only: bool,
        default: SimpleDefault,
    ) -> Result<String, XsltError> {
        let separator = separator.unwrap_or_else(|| default.text().to_string());
        match value {
            ValueSource::Select(id) => {
                let value = self.eval_expr(*id, frame, cx)?;
                let mut atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                if first_item_only {
                    atoms.truncate(1);
                }
                let strings: Vec<String> = atoms.iter().map(|a| self.atom_string(a)).collect();
                Ok(strings.join(&separator))
            }
            ValueSource::Content(body) => {
                let mut collector = SimpleContentCollector::new(self.sheet.names());
                self.eval_body(body, frame, cx, &mut collector)?;
                collector.finish(&separator)
            }
        }
    }

    /// The document an `xsl:message` builds, serialized (§17).
    ///
    /// The `select` attribute behaves as an `xsl:copy-of` prepended to the
    /// sequence constructor, so both together make one document.
    fn message_content(
        &self,
        select: Option<ExprId>,
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<String, XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::new(arena, names, None)?;
        let mut out = ComplexContentNormalizer::new(builder);
        out.start_document()?;
        if let Some(id) = select {
            let value = self.eval_expr(id, frame, cx)?;
            for item in value.into_vec() {
                out.append_item(&item)?;
            }
        }
        self.eval_body(body, frame, cx, &mut out)?;
        out.end_document()?;
        let doc = out.into_inner().finish()?;
        let doc = self.store.borrow().arena().alloc(doc);
        crate::receiver::serialize_document(doc)
    }

    // ── Patterns (§5.5.3) ────────────────────────────────────────────────

    /// Whether `node` matches `pattern`.
    ///
    /// > \[Definition: A node matches a pattern if the pattern, evaluated as
    /// > an expression with the node as the context node, selects a sequence
    /// > that includes the node.\]
    ///
    /// §5.5.4 makes a dynamic error raised while matching recoverable: "the
    /// optional recovery action is to treat the pattern as not matching".
    /// `TransformOptions::strict_recoverable` signals it instead.
    pub fn matches(&self, pattern: &Pattern, node: &Nav<'t>) -> Result<bool, XsltError> {
        for alternative in &pattern.alternatives {
            // The node handed in is both the candidate and, per §16.6.1, the
            // node `current()` reports throughout the match.
            match self.guard_recoverable(self.matches_alternative(alternative, node, node)) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    /// §5.5.4: an error raised while matching is recoverable, and the recovery
    /// action is to treat the pattern as not matching.
    fn guard_recoverable(&self, result: Result<bool, XsltError>) -> Result<bool, XsltError> {
        match result {
            Ok(matched) => Ok(matched),
            Err(e) if self.options.strict_recoverable => Err(e),
            Err(_) => Ok(false),
        }
    }

    /// Whether `node` matches one alternative.
    ///
    /// `current` is the node the *whole* pattern is being matched against.
    /// §16.6.1: "If the current function is used within a pattern, its value
    /// is the node that is being matched against the pattern" — so it stays
    /// fixed while the match walks left through ancestors, and is what every
    /// predicate of every step sees, whatever node that step is testing.
    fn matches_alternative(
        &self,
        alternative: &PathPattern,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        if alternative.steps.is_empty() {
            // The pattern `/`.
            return Ok(node.node_type() == DomNodeType::Root);
        }
        self.matches_from(alternative, 0, node, current)
    }

    fn matches_from(
        &self,
        alternative: &PathPattern,
        index: usize,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        let step = &alternative.steps[index];
        if !self.step_matches(step, node, current)? {
            return Ok(false);
        }
        if index + 1 == alternative.steps.len() {
            return Ok(match alternative.anchor {
                Anchor::None => true,
                Anchor::Root => {
                    let mut parent = node.clone();
                    parent.move_to_parent() && parent.node_type() == DomNodeType::Root
                }
                Anchor::RootDescendant => {
                    let mut root = node.clone();
                    root.move_to_root();
                    root.node_type() == DomNodeType::Root
                }
            });
        }
        match step.connector {
            Connector::Parent => {
                let mut parent = node.clone();
                if !parent.move_to_parent() {
                    return Ok(false);
                }
                self.matches_from(alternative, index + 1, &parent, current)
            }
            Connector::Ancestor => {
                let mut ancestor = node.clone();
                while ancestor.move_to_parent() {
                    if self.matches_from(alternative, index + 1, &ancestor, current)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    fn step_matches(
        &self,
        step: &PatternStep,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        if !node_test_matches(&step.test, step.axis, node) {
            return Ok(false);
        }
        if step.predicates.is_empty() {
            return Ok(true);
        }
        // §5.5.3: the predicates apply successively to the sequence the node
        // test selects among the candidate's siblings, with the focus
        // recomputed after each one. A candidate with no parent is treated as
        // a one-item sequence (`child-or-top` selects the context node).
        let mut siblings = self.node_test_siblings(step, node);
        for predicate in &step.predicates {
            let size = siblings.len();
            let mut kept = Vec::with_capacity(size);
            for (index, candidate) in siblings.iter().enumerate() {
                let mut cx = XsltContext::empty();
                cx.item = Some(XmlItem::Node(candidate.clone()));
                cx.position = index + 1;
                cx.size = size;
                // Not the candidate: `current()` inside a pattern is the node
                // the pattern as a whole is being matched against (§16.6.1).
                cx.current = Some(XmlItem::Node(current.clone()));
                let value = self.eval_expr(*predicate, &Frame::default(), &cx)?;
                if predicate_holds(&value, index + 1).map_err(|e| self.xpath_error(e))? {
                    kept.push(candidate.clone());
                }
            }
            siblings = kept;
        }
        Ok(siblings.iter().any(|n| n.is_same_position(node)))
    }

    fn node_test_siblings(&self, step: &PatternStep, node: &Nav<'t>) -> Vec<Nav<'t>> {
        let mut parent = node.clone();
        if !parent.move_to_parent() {
            return vec![node.clone()];
        }
        let mut out = Vec::new();
        match step.axis {
            PatternAxis::Attribute => {
                let mut attr = parent.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        if node_test_matches(&step.test, step.axis, &attr) {
                            out.push(attr.clone());
                        }
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
            }
            PatternAxis::Child => {
                let mut child = parent.clone();
                if child.move_to_first_child() {
                    loop {
                        if node_test_matches(&step.test, step.axis, &child) {
                            out.push(child.clone());
                        }
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(node.clone());
        }
        out
    }

    // ── Helpers the function evaluator needs ─────────────────────────────

    fn children_of_focus(
        &self,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<XmlItem<Nav<'t>>>, XsltError> {
        match &cx.item {
            Some(XmlItem::Node(nav)) => Ok(children_of(nav)),
            Some(XmlItem::Atomic(_)) => Err(XsltError::new(
                "XPTY0019",
                "xsl:apply-templates with no select needs a node as the context item",
            )),
            None => Err(XsltError::new(
                "XPDY0002",
                "xsl:apply-templates has no context item",
            )),
        }
    }

    /// The namespace a prefix is bound to in an instruction's static context.
    fn static_prefix(&self, instruction: &Instruction, prefix: &str) -> Option<String> {
        let sc = self.sheet.static_contexts.get(instruction.ctx);
        if prefix.is_empty() {
            return Some(sc.resolve_prefix("").unwrap_or_default());
        }
        sc.resolve_prefix(prefix)
    }

    /// The base URI the *caller's* relative references resolve against — the
    /// initial source document of [`transform_uri`](Self::transform_uri).
    ///
    /// An expression inside the stylesheet does **not** use this: §16.1
    /// resolves against the base URI from the static context of the expression,
    /// which is the base URI of the stylesheet element that contains it, in
    /// whichever module that is. See [`Self::load_relative_to`].
    fn base_uri(&self) -> String {
        self.options
            .base_uri
            .clone()
            .unwrap_or_else(|| self.sheet.base_uri().to_string())
    }

    fn load_uri(&self, uri: &str) -> Result<DocId, XsltError> {
        self.load_relative_to(&self.base_uri(), uri)
    }

    /// Loads the document `uri` names, resolved against `base` (§16.1).
    ///
    /// > If it is a relative URI reference, then it is resolved against the
    /// > base URI of `$base-node` if supplied, or against the base URI from the
    /// > static context otherwise (this will usually be the base URI of the
    /// > stylesheet module).
    fn load_relative_to(&self, base: &str, uri: &str) -> Result<DocId, XsltError> {
        let absolute = resolver::resolve_uri(base, uri);
        if let Some(id) = self.store.borrow().id_of_uri(&absolute) {
            return Ok(id);
        }
        // A pre-registered source wins over the resolver, so that a caller who
        // handed the text in gets the same document back (§16.1 identity).
        let registered = self
            .store
            .borrow()
            .source_text(&absolute)
            .map(str::to_string);
        let text = match registered {
            Some(text) => text,
            None => self.resolver.resolve_text(&absolute).map_err(|e| {
                XsltError::new(codes::XTDE1170, format!("cannot read {absolute:?}: {e}"))
            })?,
        };
        self.admit(Some(&absolute), &text)
    }
}

/// The default separator §5.7.2 gives an instruction.
#[derive(Clone, Copy)]
pub enum SimpleDefault {
    /// A single space — what a `select` attribute and an AVT use.
    Space,
    /// A zero-length string — what a sequence constructor uses.
    Empty,
}

impl SimpleDefault {
    fn of(value: &ValueSource) -> Self {
        match value {
            ValueSource::Select(_) => SimpleDefault::Space,
            ValueSource::Content(_) => SimpleDefault::Empty,
        }
    }

    fn text(self) -> &'static str {
        match self {
            SimpleDefault::Space => " ",
            SimpleDefault::Empty => "",
        }
    }
}

/// Collects the result of a sequence constructor that feeds simple content
/// (§5.7.2).
///
/// The distinction the rules turn on is text node versus atomic value:
/// adjacent text nodes merge into one string and are never separated, while
/// adjacent atomic values are. Constructed elements contribute their string
/// value, which for an untyped element is the concatenation of its descendant
/// text.
struct SimpleContentCollector<'n> {
    names: &'n xsd_schema::namespace::NameTable,
    parts: Vec<SimplePart>,
    pending_text: Option<String>,
    depth: usize,
    element_text: String,
}

enum SimplePart {
    Text(String),
    Atomic(String),
}

impl<'n> SimpleContentCollector<'n> {
    fn new(names: &'n xsd_schema::namespace::NameTable) -> Self {
        Self {
            names,
            parts: Vec::new(),
            pending_text: None,
            depth: 0,
            element_text: String::new(),
        }
    }

    fn flush_text(&mut self) {
        if let Some(text) = self.pending_text.take() {
            if !text.is_empty() {
                self.parts.push(SimplePart::Text(text));
            }
        }
    }

    fn push_text(&mut self, s: &str) {
        self.pending_text.get_or_insert_with(String::new).push_str(s);
    }

    fn push_atomic(&mut self, s: String) {
        self.flush_text();
        self.parts.push(SimplePart::Atomic(s));
    }

    fn finish(mut self, separator: &str) -> Result<String, XsltError> {
        self.flush_text();
        let mut out = String::new();
        let mut first = true;
        for part in &self.parts {
            let text = match part {
                SimplePart::Text(text) => text,
                SimplePart::Atomic(text) => text,
            };
            if !first {
                out.push_str(separator);
            }
            out.push_str(text);
            first = false;
        }
        Ok(out)
    }
}

impl<'t> Receiver<'t> for SimpleContentCollector<'_> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }
    fn end_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }
    fn start_element(
        &mut self,
        _name: &Name,
        _binding: Option<xsd_schema::document::NodeSchemaBinding>,
        _nilled: bool,
    ) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.flush_text();
            self.element_text.clear();
        }
        self.depth += 1;
        Ok(())
    }
    fn namespace(&mut self, _prefix: &str, _uri: &str) -> Result<(), XsltError> {
        Ok(())
    }
    fn attribute(
        &mut self,
        _name: &Name,
        _value: &str,
        _binding: Option<xsd_schema::document::NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        // An attribute node in a sequence used as simple content is not part
        // of the string value of the element it belongs to; inside one, it
        // contributes nothing.
        Ok(())
    }
    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        Ok(())
    }
    fn text(&mut self, s: &str, _disable_escaping: bool) -> Result<(), XsltError> {
        if self.depth > 0 {
            self.element_text.push_str(s);
        } else {
            self.push_text(s);
        }
        Ok(())
    }
    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.push_atomic(s.to_string());
        }
        Ok(())
    }
    fn pi(&mut self, _target: &str, data: &str) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.push_atomic(data.to_string());
        }
        Ok(())
    }
    fn end_element(&mut self) -> Result<(), XsltError> {
        self.depth -= 1;
        if self.depth == 0 {
            let text = std::mem::take(&mut self.element_text);
            self.push_atomic(text);
        }
        Ok(())
    }
    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(value) => {
                let text = atom_to_string(self.names, value);
                if self.depth > 0 {
                    self.element_text.push_str(&text);
                } else {
                    self.push_atomic(text);
                }
            }
            XmlItem::Node(nav) => {
                let text = nav.value();
                if self.depth > 0 {
                    self.element_text.push_str(&text);
                } else if nav.node_type().is_text_like() {
                    // "Adjacent text nodes in the sequence are merged."
                    self.push_text(&text);
                } else {
                    self.push_atomic(text);
                }
            }
        }
        Ok(())
    }
}

// ── The XSLT function evaluator (§16, §9.6) ──────────────────────────────

/// Runs the XSLT function library over the running transformation.
pub struct XsltEvaluator<'r, 'a, 't> {
    run: &'r Transformation<'a, 't>,
    cx: &'r XsltContext<'t>,
    /// The lexical static context of the expression being evaluated (§5.4.1):
    /// `system-property()`, `element-available()` and `function-available()`
    /// resolve the prefix of their argument here.
    sc: &'r crate::stylesheet::StaticContext,
}

impl<'t> FunctionEvaluator<Nav<'t>> for XsltEvaluator<'_, '_, 't> {
    fn eval(
        &self,
        handle: FunctionHandle,
        ctx: &mut DynamicContext<'_, Nav<'t>>,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XPathError> {
        let Some(which) = self.run.sheet.functions().resolve(handle) else {
            return BuiltinEvaluator.eval(handle, ctx, args);
        };
        match self.call(which, ctx, args) {
            Ok(value) => Ok(value),
            // `XPathError` has no host-error variant, so the real error is
            // parked on the transformation and picked up by `xpath_error`.
            Err(e) => {
                let message = e.to_string();
                *self.run.parked_error.borrow_mut() = Some(e);
                Err(XPathError::internal(message))
            }
        }
    }
}

impl<'t> XsltEvaluator<'_, '_, 't> {
    /// The base URI relative references in *this* expression resolve against.
    ///
    /// §5.4.1 gives the static context "Base URI: The base URI of the
    /// containing element in the stylesheet", after `xml:base`, and the
    /// containing element may be in an imported or included module — which is
    /// exactly why this is the expression's own static context and not the
    /// principal module's.
    fn static_base_uri(&self) -> &str {
        &self.sc.base_uri
    }

    /// The XPath context item — what a function with a context-dependent
    /// default argument sees.
    ///
    /// This is **not** `self.cx.item`: the XSLT-level focus is the focus of the
    /// innermost *instruction*, which inside a predicate or on the right of `/`
    /// is a different node. Only `current()` (§16.6.1) wants that one.
    fn context_item(
        &self,
        ctx: &DynamicContext<'_, Nav<'t>>,
    ) -> Option<XmlItem<Nav<'t>>> {
        ctx.context_item.clone()
    }

    fn call(
        &self,
        which: XsltFn,
        ctx: &mut DynamicContext<'_, Nav<'t>>,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        match which {
            // §16.6.1: "returns the item that was the context item at the
            // point where the expression was invoked from the XSLT
            // stylesheet" — the one function that takes the XSLT-level focus.
            XsltFn::Current => match &self.cx.current {
                Some(item) => Ok(XPathValue::from_item(item.clone())),
                None => Err(XsltError::new(
                    codes::XTDE1360,
                    "current() was called where there is no context item",
                )),
            },
            XsltFn::GenerateId => {
                // §16.6.4: "If the argument is omitted, it defaults to the
                // context node", i.e. the XPath context item.
                let node = match args.first() {
                    None => self.context_item(ctx),
                    Some(value) => value.first().cloned(),
                };
                Ok(match node {
                    Some(XmlItem::Node(nav)) => {
                        XPathValue::string(self.run.store.borrow().generate_id(&nav))
                    }
                    Some(XmlItem::Atomic(_)) => {
                        return Err(XsltError::new(
                            "XPTY0004",
                            "generate-id() takes a node, not an atomic value",
                        ))
                    }
                    None => XPathValue::string(String::new()),
                })
            }
            XsltFn::Document | XsltFn::Doc => {
                // §16.1: "The second argument, if present, is a node whose base
                // URI is used to resolve any relative URI references contained
                // in the first argument." Otherwise a string argument resolves
                // against the base URI from the static context, and a *node*
                // argument against the base URI of the node that contained it.
                let explicit_base = args.get(1).and_then(|value| match value.first() {
                    Some(XmlItem::Node(nav)) => Some(nav.base_uri().to_string()),
                    _ => None,
                });
                let items = match args.first() {
                    Some(value) => value.clone().into_vec(),
                    None => Vec::new(),
                };
                let mut nodes = Vec::new();
                for item in items {
                    let (uri, item_base) = match &item {
                        XmlItem::Atomic(value) => (value.to_string_value(), None),
                        XmlItem::Node(nav) => (nav.value(), Some(nav.base_uri().to_string())),
                    };
                    if uri.contains('#') {
                        return Err(XsltError::not_implemented(
                            "a fragment identifier in document()",
                            "M2",
                        ));
                    }
                    let base = explicit_base
                        .clone()
                        .or(item_base)
                        .unwrap_or_else(|| self.static_base_uri().to_string());
                    // §16.1: an empty URI means the stylesheet module itself,
                    // which falls out of resolving "" against the base URI.
                    let id = self.run.load_relative_to(&base, &uri)?;
                    nodes.push(XmlItem::Node(self.run.store.borrow().navigator(id)));
                }
                Ok(XPathValue::from_sequence(nodes))
            }
            XsltFn::DocAvailable => {
                let uris = string_args(args.first())?;
                let available = match uris.first() {
                    Some(uri) => self
                        .run
                        .load_relative_to(self.static_base_uri(), uri)
                        .is_ok(),
                    None => false,
                };
                Ok(XPathValue::boolean(available))
            }
            XsltFn::DocumentUri => {
                // F&O `fn:document-uri`: the `document-uri` property of the
                // argument, which only a document node has, and which §9.4
                // sets to empty for a temporary tree. The store is the only
                // place that knows which document came from which URI.
                let node = match args.first() {
                    None => self.context_item(ctx),
                    Some(value) => value.first().cloned(),
                };
                let Some(XmlItem::Node(nav)) = node else { return Ok(XPathValue::empty()) };
                if nav.node_type() != DomNodeType::Root {
                    return Ok(XPathValue::empty());
                }
                Ok(match self.run.store.borrow().uri_of(&nav) {
                    Some(uri) => XPathValue::from_atomic(any_uri(uri)),
                    None => XPathValue::empty(),
                })
            }
            XsltFn::SystemProperty => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::string(self.system_property_of(&name)?))
            }
            XsltFn::ElementAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::boolean(self.element_available(&name)))
            }
            XsltFn::FunctionAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::boolean(self.function_available(&name)))
            }
            XsltFn::TypeAvailable => Ok(XPathValue::boolean(false)),
            // §16.2: "If the URI is a relative URI, then it is resolved
            // relative to the base URI from the static context."
            XsltFn::UnparsedText => {
                let uri = single_string(args.first()).unwrap_or_default();
                let absolute = resolver::resolve_uri(self.static_base_uri(), &uri);
                let text = self.run.resolver.resolve_text(&absolute).map_err(|e| {
                    XsltError::new(codes::XTDE1170, format!("cannot read {absolute:?}: {e}"))
                })?;
                Ok(XPathValue::string(text))
            }
            XsltFn::UnparsedTextAvailable => {
                let uri = single_string(args.first()).unwrap_or_default();
                let absolute = resolver::resolve_uri(self.static_base_uri(), &uri);
                Ok(XPathValue::boolean(self.run.resolver.available(&absolute)))
            }
            // §16.6.2/§16.6.3: the entity is looked up "in the document
            // containing the context node". No source document carries DTD
            // information here, so there is never such an entity and the
            // result is the empty value — but the context-node conditions are
            // errors in their own right and are checked.
            XsltFn::UnparsedEntityUri => {
                self.require_document_context(ctx, codes::XTDE1370)?;
                Ok(XPathValue::from_atomic(any_uri("")))
            }
            XsltFn::UnparsedEntityPublicId => {
                self.require_document_context(ctx, codes::XTDE1380)?;
                Ok(XPathValue::string(String::new()))
            }
            XsltFn::Key => Err(XsltError::not_implemented("key()", "M2")),
            XsltFn::CurrentGroup | XsltFn::CurrentGroupingKey => {
                Err(XsltError::not_implemented(which.name(), "M2"))
            }
            XsltFn::RegexGroup => Err(XsltError::not_implemented("regex-group()", "M2")),
        }
    }

    /// The precondition `unparsed-entity-uri` and `unparsed-entity-public-id`
    /// share (§16.6.2, §16.6.3).
    ///
    /// > It is a non-recoverable dynamic error if the function is called when
    /// > there is no context node, or when the root of the tree containing the
    /// > context node is not a document node.
    fn require_document_context(
        &self,
        ctx: &DynamicContext<'_, Nav<'t>>,
        code: &'static str,
    ) -> Result<(), XsltError> {
        let Some(XmlItem::Node(nav)) = self.context_item(ctx) else {
            return Err(XsltError::new(code, "there is no context node"));
        };
        let mut root = nav.clone();
        root.move_to_root();
        if root.node_type() != DomNodeType::Root {
            return Err(XsltError::new(
                code,
                "the root of the tree containing the context node is not a document node",
            ));
        }
        Ok(())
    }

    /// §16.6.5 system-property.
    fn system_property_of(&self, name: &str) -> Result<String, XsltError> {
        let (prefix, local) = split_qname(whitespace::trim(name));
        if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
            return Err(XsltError::new(
                codes::XTDE1390,
                format!("{name:?} is not a QName"),
            ));
        }
        let uri = self.resolve_static_prefix(prefix).ok_or_else(|| {
            XsltError::new(
                codes::XTDE1390,
                format!("the prefix {prefix:?} of {name:?} is not bound"),
            )
        })?;
        if uri != XSLT_NS {
            return Ok(String::new());
        }
        Ok(crate::functions::system_property(local).to_string())
    }

    /// §18.1.1 element-available.
    fn element_available(&self, name: &str) -> bool {
        let (prefix, local) = split_qname(whitespace::trim(name));
        match self.resolve_static_prefix(prefix) {
            Some(uri) => crate::functions::element_available(&uri, local),
            None => false,
        }
    }

    /// §18.1.2 function-available.
    fn function_available(&self, name: &str) -> bool {
        let (prefix, local) = split_qname(whitespace::trim(name));
        let uri = if prefix.is_empty() {
            crate::names::FN_NS.to_string()
        } else {
            match self.resolve_static_prefix(prefix) {
                Some(uri) => uri,
                None => return false,
            }
        };
        crate::functions::function_available(self.run.sheet.functions(), &uri, local)
    }

    fn resolve_static_prefix(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            return Some(String::new());
        }
        if prefix == "xml" {
            return Some(XML_NS.to_string());
        }
        // §5.4.1: the in-scope namespaces of a static context are "the
        // namespaces in scope for the containing element in the stylesheet",
        // and nothing else. A prefix bound somewhere else in the stylesheet is
        // not bound here, and the caller turns that into XTDE1390/XTDE1400/
        // XTDE1440 as its own definition requires.
        self.sc.resolve_prefix(prefix)
    }
}

// ── Free helpers ─────────────────────────────────────────────────────────

/// An `xs:anyURI` atomic value.
fn any_uri(uri: &str) -> XmlValue {
    XmlValue::new(
        XmlTypeCode::AnyUri,
        XmlValueKind::Atomic(XmlAtomicValue::AnyUri(uri.to_string())),
    )
}

/// The `copy-namespaces` setting of a copying instruction (§11.9.1, §11.9.2).
fn copy_scope(copy_namespaces: bool) -> CopyNamespaces {
    if copy_namespaces {
        CopyNamespaces::All
    } else {
        CopyNamespaces::FixupOnly
    }
}

/// The value of an attribute of `nav`, by expanded name.
fn attribute_value(nav: &Nav<'_>, uri: &str, local: &str) -> Option<String> {
    let mut attr = nav.clone();
    if !attr.move_to_first_attribute() {
        return None;
    }
    loop {
        if attr.local_name() == local && attr.namespace_uri() == uri {
            return Some(attr.value());
        }
        if !attr.move_to_next_attribute() {
            return None;
        }
    }
}

/// The string an atomized item contributes to simple content (§5.7.2 step 4).
///
/// `xs:QName` and `xs:NOTATION` need the name table to render, because a
/// `QualifiedName` holds interned ids: upstream's `Display` prints a debug
/// form, which would reach the result tree as an element name. Casting a QName
/// to a string yields its lexical form, prefix and all.
fn atom_to_string(names: &xsd_schema::namespace::NameTable, value: &XmlValue) -> String {
    match value.as_qname() {
        Some(qname) => {
            let local = names.resolve(qname.local_name);
            match qname.prefix.and_then(|p| names.try_resolve(p)) {
                Some(prefix) if !prefix.is_empty() => format!("{prefix}:{local}"),
                _ => local,
            }
        }
        None => value.to_string_value(),
    }
}

fn children_of<'t>(nav: &Nav<'t>) -> Vec<XmlItem<Nav<'t>>> {
    let mut out = Vec::new();
    let mut child = nav.clone();
    if child.move_to_first_child() {
        loop {
            out.push(XmlItem::Node(child.clone()));
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    out
}

/// Whether a node satisfies a pattern step's axis and node test (§5.5.3).
///
/// The axes a pattern uses are the adjusted ones §5.5.3 defines:
///
/// > If PS uses the child axis … the axis in step PS is replaced by
/// > `child-or-top` … If the context node is a parentless element, comment,
/// > processing-instruction, or text node then the `child-or-top` axis selects
/// > the context node; otherwise it selects the children of the context node.
/// > It is a forwards axis whose principal node kind is element.
///
/// Two consequences are easy to get wrong, and the spec calls both out:
/// `node()` does **not** match a document node ("for backwards compatibility
/// reasons the `child-or-top` axis does not match a document node"), and a
/// step whose node test is `document-node()` uses the `self` axis instead, so
/// that it does.
fn node_test_matches(test: &NodeTest, axis: PatternAxis, nav: &Nav<'_>) -> bool {
    let kind = nav.node_type();
    // `document-node()` is the one test that looks at the node itself.
    if let NodeTest::DocumentNode(inner) = test {
        if kind != DomNodeType::Root {
            return false;
        }
        return match inner {
            None => true,
            Some(inner) => {
                let mut child = nav.clone();
                if !child.move_to_first_child() {
                    return false;
                }
                loop {
                    if child.node_type() == DomNodeType::Element {
                        return node_test_matches(inner, PatternAxis::Child, &child);
                    }
                    if !child.move_to_next_sibling() {
                        return false;
                    }
                }
            }
        };
    }
    // Everything else selects along `child-or-top` or `attribute-or-top`.
    let on_axis = match axis {
        PatternAxis::Attribute => kind == DomNodeType::Attribute,
        PatternAxis::Child => matches!(
            kind,
            DomNodeType::Element
                | DomNodeType::Text
                | DomNodeType::Whitespace
                | DomNodeType::SignificantWhitespace
                | DomNodeType::Comment
                | DomNodeType::ProcessingInstruction
        ),
    };
    if !on_axis {
        return false;
    }
    let principal = principal_kind(axis);
    match test {
        NodeTest::Name(q) => {
            kind == principal && nav.local_name() == q.local && nav.namespace_uri() == q.uri
        }
        NodeTest::NsWildcard(uri) => kind == principal && nav.namespace_uri() == uri,
        NodeTest::LocalWildcard(local) => kind == principal && nav.local_name() == local,
        NodeTest::AnyName => kind == principal,
        NodeTest::AnyNode => true,
        NodeTest::Text => kind.is_text_like(),
        NodeTest::Comment => kind == DomNodeType::Comment,
        NodeTest::Pi(None) => kind == DomNodeType::ProcessingInstruction,
        NodeTest::Pi(Some(target)) => {
            kind == DomNodeType::ProcessingInstruction && nav.local_name() == target
        }
        NodeTest::Element(name) => {
            kind == DomNodeType::Element
                && name
                    .as_ref()
                    .is_none_or(|q| nav.local_name() == q.local && nav.namespace_uri() == q.uri)
        }
        NodeTest::Attribute(name) => {
            kind == DomNodeType::Attribute
                && name
                    .as_ref()
                    .is_none_or(|q| nav.local_name() == q.local && nav.namespace_uri() == q.uri)
        }
        NodeTest::DocumentNode(_) => unreachable!("handled above"),
    }
}

/// The principal node kind of an axis: elements for `child`, attributes for
/// `attribute`.
fn principal_kind(axis: PatternAxis) -> DomNodeType {
    match axis {
        PatternAxis::Child => DomNodeType::Element,
        PatternAxis::Attribute => DomNodeType::Attribute,
    }
}

/// A predicate whose value is a singleton number is positional; anything else
/// is taken by its effective boolean value.
fn predicate_holds(value: &XPathValue<Nav<'_>>, position: usize) -> Result<bool, XPathError> {
    let single = match value {
        XPathValue::Item(item) => Some(item),
        XPathValue::Sequence(items) if items.len() == 1 => Some(&items[0]),
        _ => None,
    };
    if let Some(XmlItem::Atomic(atomic)) = single {
        if atomic.type_code.is_numeric() {
            if let Some(number) = atomic.as_double() {
                return Ok(number == position as f64);
            }
        }
    }
    effective_boolean_value(value)
}

fn single_string<N: DomNavigator>(value: Option<&XPathValue<N>>) -> Option<String> {
    match value?.first()? {
        XmlItem::Atomic(v) => Some(v.to_string_value()),
        XmlItem::Node(n) => Some(n.value()),
    }
}

/// The string values of every item of an argument.
///
/// `XPathValue::as_slice` returns an empty slice for the single-item variant
/// (`functions/mod.rs`, documented there as a limitation), so the value is
/// materialized instead.
fn string_args<N: DomNavigator>(value: Option<&XPathValue<N>>) -> Result<Vec<String>, XsltError> {
    let Some(value) = value else { return Ok(Vec::new()) };
    Ok(value
        .clone()
        .into_vec()
        .into_iter()
        .map(|item| match item {
            XmlItem::Atomic(v) => v.to_string_value(),
            XmlItem::Node(n) => n.value(),
        })
        .collect())
}

/// The options a run uses to serialize its primary result.
pub fn serialize_options(
    sheet: &CompiledStylesheet<'_>,
) -> Result<xsd_schema::document::serialize::SerializeOptions, XsltError> {
    sheet.output().serialize_options()
}

/// Serializes a result tree per the stylesheet's merged `xsl:output` (§20).
pub fn serialize_result(
    sheet: &CompiledStylesheet<'_>,
    doc: &BufferDocument<'_>,
) -> Result<String, XsltError> {
    let limit = sheet.output().character_escaping()?;
    let serialized = match sheet.output().effective_method() {
        OutputMethod::Text => text_value(doc),
        _ => {
            let options = sheet.output().serialize_options()?;
            xsd_schema::document::serialize::to_string(&doc.create_navigator(), &options)
                .map_err(|e| XsltError::internal(format!("could not serialize: {e}")))?
        }
    };
    Ok(match limit {
        None => serialized,
        Some(limit) => escape_above(&serialized, limit),
    })
}

/// Writes every character above `limit` as a numeric character reference.
///
/// §20: a character the output encoding cannot represent is serialized as a
/// character reference. M1 always writes UTF-8 bytes, so this is the only part
/// of the `encoding` parameter it can honour — and the part the result of a
/// transformation actually depends on, because the reference is in the result
/// text rather than in the byte encoding.
///
/// A character reference is only legal in content and in an attribute value,
/// never in a name, so the scan tracks whether it is inside a tag and, within
/// a tag, whether it is inside the quotes of an attribute value.
fn escape_above(text: &str, limit: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;
    for ch in text.chars() {
        let escapable = !in_tag || quote.is_some();
        match ch {
            '<' if !in_tag => {
                in_tag = true;
                out.push(ch);
            }
            '>' if in_tag && quote.is_none() => {
                in_tag = false;
                out.push(ch);
            }
            '"' | '\'' if in_tag => {
                quote = match quote {
                    Some(q) if q == ch => None,
                    Some(q) => Some(q),
                    None => Some(ch),
                };
                out.push(ch);
            }
            c if c > limit && escapable => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The `text` output method: the string values of the text nodes, in order.
fn text_value(doc: &BufferDocument<'_>) -> String {
    let mut out = String::new();
    let mut nav = doc.create_navigator();
    let root = nav.clone();
    if !nav.move_to_first_child() {
        return out;
    }
    let mut stack = vec![nav];
    while let Some(mut cursor) = stack.pop() {
        loop {
            if cursor.node_type().is_text_like() {
                out.push_str(&cursor.value());
            }
            let mut child = cursor.clone();
            if child.move_to_first_child() {
                let mut rest = cursor.clone();
                if rest.move_to_next_sibling() {
                    stack.push(rest);
                }
                cursor = child;
                continue;
            }
            if !cursor.move_to_next_sibling() {
                break;
            }
        }
    }
    let _ = root;
    out
}

/// A run's default `BufferDocumentOptions` for source documents.
pub fn source_options() -> BufferDocumentOptions {
    BufferDocumentOptions::default()
}

/// Where an error without its own location is reported.
pub fn unknown_location() -> Location {
    Location { module_uri: None, line: 0, column: 0 }
}
