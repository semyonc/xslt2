//! The M0b stand-ins for the pieces the invariants need before the compiler
//! and the instruction evaluator exist.
//!
//! Nothing here is meant to survive M1 unchanged: a [`Frame`] is a bare slot
//! vector, a "stylesheet function" is a Rust `fn`, and a
//! [`CompiledStylesheet`] holds a handful of pre-compiled expressions. They
//! exist to make the contracts of §2.6 testable — ownership (Q1, Q35),
//! per-run isolation, and the pattern-predicate rule — at a point where
//! changing them is still cheap.

use std::cell::RefCell;

use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::{XPathContext, XPathExpr, XmlItem};

use crate::error::XsltError;
use crate::receiver::Nav;
use crate::store::{DocumentStore, ProtoFunctions, PROTO_NS};

/// A variable frame: §9.1's `Frame`, without tunnel parameters.
#[derive(Default)]
pub struct Frame<'t> {
    /// Slot-indexed values, as `NameBinder` numbers them upstream.
    pub slots: Vec<XPathValue<Nav<'t>>>,
}

impl<'t> Frame<'t> {
    /// A frame with `n` empty slots.
    pub fn with_slots(n: usize) -> Self {
        Self {
            slots: (0..n).map(|_| XPathValue::Empty).collect(),
        }
    }

    /// Binds one slot.
    pub fn set(&mut self, slot: usize, value: XPathValue<Nav<'t>>) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || XPathValue::Empty);
        }
        self.slots[slot] = value;
    }

    /// Reads one slot.
    pub fn get(&self, slot: usize) -> Option<&XPathValue<Nav<'t>>> {
        self.slots.get(slot)
    }
}

/// The immutable, shareable half of a transformation (§9.1, invariant 3).
///
/// It owns nothing a run mutates: the name table is borrowed and shared by
/// every document of every run (Q35 option (a): documented single-threaded
/// reuse), and the compiled expressions carry no lifetimes at all
/// (`XPathExpr` owns its arena).
///
/// **Ownership note (Q35 / Q13).** `XPathContext<'a>` keeps the function
/// catalog as a `&'a dyn FunctionCatalog` (`xsd-schema/src/xpath/context.rs:174`),
/// so a `CompiledStylesheet` cannot own both its [`ProtoFunctions`] and the
/// static context that points at them — that would be a self-referential
/// struct. The function set is therefore a separate value the caller keeps
/// alive for `'t`, exactly as the name table is. M1 either keeps this shape or
/// leaks the set once per compiled stylesheet.
pub struct CompiledStylesheet<'t> {
    names: &'t NameTable,
    context: XPathContext<'t>,
    exprs: Vec<XPathExpr>,
}

impl<'t> CompiledStylesheet<'t> {
    /// Compiles the expressions of the stand-in stylesheet.
    ///
    /// The `proto:` prefix is bound so that `proto:doc('…')` resolves at
    /// compile time through [`ProtoFunctions::catalog`].
    pub fn compile<'f>(
        names: &'t NameTable,
        functions: &'t ProtoFunctions<'f>,
        sources: &[&str],
    ) -> Result<Self, XsltError>
    where
        'f: 't,
    {
        let mut namespaces = NamespaceContextSnapshot::default();
        namespaces
            .bindings
            .push((names.add("proto"), names.add(PROTO_NS)));
        let context = XPathContext::new(names)
            .with_namespaces(namespaces)
            .with_function_catalog(functions.catalog());
        let mut exprs = Vec::with_capacity(sources.len());
        for source in sources {
            exprs.push(XPathExpr::compile(source, &context)?);
        }
        Ok(Self {
            names,
            context,
            exprs,
        })
    }

    /// The shared name table (§6.6).
    pub fn names(&self) -> &'t NameTable {
        self.names
    }

    /// The static context every expression was compiled in.
    pub fn context(&self) -> &XPathContext<'t> {
        &self.context
    }

    /// One compiled expression, by the index it was given at compile time.
    pub fn expr(&self, index: usize) -> &XPathExpr {
        &self.exprs[index]
    }
}

/// The mutable, per-run half (§9.1, invariant 3).
///
/// Every run gets a fresh one: a fresh arena, therefore a fresh
/// [`DocumentStore`], fresh frames and a fresh message log. Nothing a run
/// produces can reach the next one, and because the store numbers documents by
/// its own ordinal the second run's `generate-id()` values repeat the first
/// run's.
pub struct Transformation<'s, 't> {
    store: &'s RefCell<DocumentStore<'t>>,
    frames: Vec<Frame<'t>>,
    messages: Vec<String>,
}

impl<'s, 't> Transformation<'s, 't> {
    /// A run over a freshly created store.
    pub fn new(store: &'s RefCell<DocumentStore<'t>>) -> Self {
        Self {
            store,
            frames: vec![Frame::default()],
            messages: Vec::new(),
        }
    }

    /// The document store of this run.
    pub fn store(&self) -> &'s RefCell<DocumentStore<'t>> {
        self.store
    }

    /// `xsl:message` output of this run.
    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    /// Records an `xsl:message`.
    pub fn message(&mut self, text: impl Into<String>) {
        self.messages.push(text.into());
    }

    /// Pushes a variable frame (a template or function call).
    pub fn push_frame(&mut self, frame: Frame<'t>) {
        self.frames.push(frame);
    }

    /// Pops the innermost frame.
    pub fn pop_frame(&mut self) -> Option<Frame<'t>> {
        if self.frames.len() > 1 {
            self.frames.pop()
        } else {
            None
        }
    }

    /// The innermost frame.
    pub fn frame(&self) -> &Frame<'t> {
        self.frames.last().expect("the globals frame is never popped")
    }

    /// The innermost frame, mutably.
    pub fn frame_mut(&mut self) -> &mut Frame<'t> {
        self.frames
            .last_mut()
            .expect("the globals frame is never popped")
    }

    /// How deep the call stack is.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

// ── The stand-in stylesheet function (invariant 6, Q1) ───────────────────

/// A "stylesheet function" that recurses, loads a second document mid-recursion
/// and returns nodes from both.
///
/// This is the shape §6.1 asks M0b to borrow-check: a navigator obtained before
/// a load is still usable after it, because [`DocumentStore::navigator`]
/// borrows the arena and not the store.
///
/// `depth` counts down; at each level the function appends the node its frame
/// carries, and at the bottom it loads `uri` and appends that document's
/// document element too.
pub fn recursive_collect<'t>(
    run: &mut Transformation<'_, 't>,
    frame: Frame<'t>,
    depth: usize,
    uri: &str,
) -> Result<Vec<XmlItem<Nav<'t>>>, XsltError> {
    run.push_frame(frame);
    // The frame is popped on the error path too: a failed call must not leave
    // its frame on the caller's stack (the transition table of §9.1).
    let result = collect_level(run, depth, uri);
    run.pop_frame();
    result
}

fn collect_level<'t>(
    run: &mut Transformation<'_, 't>,
    depth: usize,
    uri: &str,
) -> Result<Vec<XmlItem<Nav<'t>>>, XsltError> {
    let mut out: Vec<XmlItem<Nav<'t>>> = Vec::new();
    // The node this level was handed: kept across the load below.
    if let Some(XPathValue::Item(item)) = run.frame().get(0) {
        out.push(item.clone());
    }
    if depth == 0 {
        let id = run.store().borrow_mut().load(uri)?;
        let mut nav = run.store().borrow().navigator(id);
        if nav.move_to_first_child() {
            out.push(XmlItem::Node(nav));
        }
    } else {
        let inner = Frame {
            slots: out.iter().cloned().map(XPathValue::from_item).collect(),
        };
        out.extend(recursive_collect(run, inner, depth - 1, uri)?);
    }
    Ok(out)
}

// ── Pattern predicates (invariant 4) ─────────────────────────────────────

/// Does `candidate` match a pattern step `name[p1][p2]…`?
///
/// XSLT 2.0 §5.5.3 *The Meaning of a Pattern* defines a pattern by rewriting it
/// as a path and asking whether the candidate is in the result. The consequence
/// this function pins down is that the predicates apply **successively**: `p2`
/// is evaluated over the sequence `p1` left behind, with the focus (item,
/// position, size) recomputed for that filtered sequence. So `item[@enabled][1]`
/// selects the first *enabled* item, not "the first item, if it is enabled".
///
/// The node test is a local-name test on the sibling axis, which is all M0b
/// needs; `predicates` are already compiled. A predicate whose value is a
/// singleton number is positional, every other value is taken by its effective
/// boolean value.
pub fn pattern_step_matches<'t>(
    candidate: &Nav<'t>,
    local_name: &str,
    predicates: &[XPathExpr],
    ctx: &XPathContext<'_>,
) -> Result<bool, XsltError> {
    if candidate.node_type() != DomNodeType::Element || candidate.local_name() != local_name {
        return Ok(false);
    }

    // The sequence the node test selects: the candidate's siblings with that
    // name, in document order.
    let mut siblings: Vec<Nav<'t>> = Vec::new();
    let mut cursor = candidate.clone();
    if cursor.move_to_parent() && cursor.move_to_first_child() {
        loop {
            if cursor.node_type() == DomNodeType::Element && cursor.local_name() == local_name {
                siblings.push(cursor.clone());
            }
            if !cursor.move_to_next_sibling() {
                break;
            }
        }
    } else {
        siblings.push(candidate.clone());
    }

    for predicate in predicates {
        let size = siblings.len();
        let mut kept: Vec<Nav<'t>> = Vec::with_capacity(size);
        for (index, node) in siblings.iter().enumerate() {
            let position = index + 1;
            let value = predicate
                .evaluator(ctx)
                .run_with_node_and_setup::<Nav<'t>, _>(Some(node.clone()), |eval| {
                    let dyn_ctx = eval.context();
                    dyn_ctx.context_position = position;
                    dyn_ctx.context_size = size;
                })?;
            if predicate_holds(&value, position)? {
                kept.push(node.clone());
            }
        }
        siblings = kept;
    }

    Ok(siblings.iter().any(|n| n.is_same_position(candidate)))
}

/// A singleton numeric predicate is positional; anything else is an effective
/// boolean value.
fn predicate_holds(value: &XPathValue<Nav<'_>>, position: usize) -> Result<bool, XsltError> {
    let single = match value {
        XPathValue::Item(item) => Some(item),
        XPathValue::Sequence(items) if items.len() == 1 => Some(&items[0]),
        _ => None,
    };
    if let Some(XmlItem::Atomic(atomic)) = single {
        if let Some(number) = atomic.as_double() {
            return Ok(number == position as f64);
        }
    }
    Ok(xsd_schema::xpath::functions::effective_boolean_value(value)?)
}
