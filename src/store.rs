//! `DocumentStore` — every document a transformation can see, with addresses
//! that stay put while it keeps loading more.
//!
//! Design sketch §6.1. A transformation holds navigators for a long time
//! (global variables, key indexes, tunnel parameters, sequences in frames)
//! while it keeps loading documents, so the `BufferDocument` *structs*
//! themselves need stable addresses — a growable container that moves its
//! values cannot hand out `&'t` borrows. The store therefore moves every
//! finished document into the transformation arena
//! ([`bumpalo::Bump::alloc`]) and keeps only the resulting `&'t` reference in
//! its slot map. [`DocumentStore::navigator`] borrows `'t`, not `&self`, which
//! is what lets a caller keep a navigator across later loads.
//!
//! The module also carries the prototype's answer to §9.6 / Q8: `fn:doc()` does
//! not exist upstream, so a `proto:doc($uri)` function is registered through
//! [`FunctionSet`] and evaluated by [`ProtoEvaluator`], which holds the store.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use bumpalo::Bump;
use slotmap::{new_key_type, SlotMap};
use xsd_schema::document::{BufferDocNavigator, BufferDocument};
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::types::sequence::SequenceType;
use xsd_schema::xpath::functions::{
    BuiltinEvaluator, DynamicFunctionSignature, FunctionCatalog, FunctionEvaluator, FunctionHandle,
    FunctionSet, XPathValue,
};
use xsd_schema::xpath::{DynamicContext, XPathError};

use crate::error::{codes, XsltError};

new_key_type! {
    /// Identifies one document inside a [`DocumentStore`].
    pub struct DocId;
}

/// The private namespace the prototype's XSLT-level functions live in.
///
/// The real processor puts `document()` in no namespace and `doc()` in the F&O
/// namespace; M0b keeps them out of the way so nothing can collide with a
/// built-in while the shape is being proven.
pub const PROTO_NS: &str = "urn:xslt2:proto";

/// One document known to a transformation.
pub struct StoredDocument<'t> {
    /// Allocated with `arena.alloc(..)`: the address is fixed for `'t`.
    pub doc: &'t BufferDocument<'t>,
    /// `document-uri()`; `None` for a temporary tree or a hidden fragment.
    pub uri: Option<Box<str>>,
    /// Stable position in the store's own document order. Unlike
    /// [`BufferDocument::serial`], which is a process-wide counter, this is
    /// reproducible from run to run — see [`DocumentStore::generate_id`].
    pub ordinal: u32,
    /// Whitespace stripping applied (§4.4). Never set in M0b.
    pub stripped: bool,
}

/// The documents of one transformation.
///
/// `arena` and `names` are borrowed, not owned: the same `NameTable` serves
/// every document of the transformation (§6.6), and one `Bump` per
/// transformation holds the nodes *and* the document structs (§6.1, Q1).
pub struct DocumentStore<'t> {
    arena: &'t Bump,
    names: &'t NameTable,
    docs: SlotMap<DocId, StoredDocument<'t>>,
    /// `document()`/`doc()` identity: the same URI is the same document (§16.1).
    by_uri: HashMap<Box<str>, DocId>,
    /// `BufferDocument::serial()` → `DocId`, so a navigator can be mapped back
    /// to its store ordinal. The navigator exposes its document but not its
    /// `DocId`; the serial is the only stable key upstream offers (Q2).
    by_serial: HashMap<u64, DocId>,
    /// Re-entrancy guard: URIs whose load is in progress (§9.1).
    loading: HashSet<Box<str>>,
    /// Stand-in for a `ResourceResolver` (Q15): URI → document text.
    sources: HashMap<Box<str>, Box<str>>,
    next_ordinal: u32,
}

impl<'t> DocumentStore<'t> {
    /// A store over the transformation's arena and name table.
    pub fn new(arena: &'t Bump, names: &'t NameTable) -> Self {
        Self {
            arena,
            names,
            docs: SlotMap::with_key(),
            by_uri: HashMap::new(),
            by_serial: HashMap::new(),
            loading: HashSet::new(),
            sources: HashMap::new(),
            next_ordinal: 0,
        }
    }

    /// The arena every document of this transformation is built in.
    pub fn arena(&self) -> &'t Bump {
        self.arena
    }

    /// The one name table shared by every document of this transformation.
    pub fn names(&self) -> &'t NameTable {
        self.names
    }

    /// How many documents the store holds.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the store holds no documents.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Moves a finished document into the arena and registers it.
    ///
    /// The document struct is allocated in the arena so that its address is
    /// fixed for `'t`; the slot map only ever holds the reference, so growth
    /// never moves a document.
    ///
    /// Note (Q1): `Bump` does not run destructors, so the `Vec`/`HashMap`
    /// members of `BufferDocument` are never freed. That is acceptable for a
    /// transformation-scoped arena and is the cost of the `&'t` shape; see the
    /// M0b report.
    pub fn insert(&mut self, doc: BufferDocument<'t>, uri: Option<&str>) -> DocId {
        let serial = doc.serial();
        let doc: &'t BufferDocument<'t> = self.arena.alloc(doc);
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let id = self.docs.insert(StoredDocument {
            doc,
            uri: uri.map(Box::from),
            ordinal,
            stripped: false,
        });
        self.by_serial.insert(serial, id);
        if let Some(uri) = uri {
            self.by_uri.insert(Box::from(uri), id);
        }
        id
    }

    /// Registers the text a later [`load`](Self::load) of `uri` should parse.
    pub fn register_source(&mut self, uri: &str, xml: &str) {
        self.sources.insert(Box::from(uri), Box::from(xml));
    }

    /// Parses `xml` as the document named `uri`.
    ///
    /// Loading the same URI twice returns the same [`DocId`] and does not
    /// re-parse, so node identities are stable across calls (§16.1). A URI
    /// whose load is already in progress is
    /// [`XTDE1170`](crate::error::codes::XTDE1170) — an error, never a panic or
    /// a double borrow (§9.1).
    pub fn load_str(&mut self, uri: &str, xml: &str) -> Result<DocId, XsltError> {
        if let Some(&id) = self.by_uri.get(uri) {
            return Ok(id);
        }
        self.enter_load(uri)?;
        let parsed = BufferDocument::from_reader_default(xml.as_bytes(), self.arena, self.names);
        self.leave_load(uri);
        let doc = parsed.map_err(|e| {
            XsltError::new(
                codes::XTDE1170,
                format!("could not read the document {uri:?}: {e}"),
            )
        })?;
        self.register_source(uri, xml);
        Ok(self.insert(doc, Some(uri)))
    }

    /// Loads a URI whose text was given to [`register_source`](Self::register_source).
    pub fn load(&mut self, uri: &str) -> Result<DocId, XsltError> {
        if let Some(&id) = self.by_uri.get(uri) {
            return Ok(id);
        }
        let xml = self.sources.get(uri).cloned().ok_or_else(|| {
            XsltError::new(
                codes::XTDE1170,
                format!("there is no resource at the URI {uri:?}"),
            )
        })?;
        self.load_str(uri, &xml)
    }

    /// Marks `uri` as being loaded, or reports the re-entrant load.
    ///
    /// Public because the guard, not the parse, is the interesting state: in
    /// M0b a parse is atomic, so nothing can re-enter it on its own. A real
    /// resolver (Q15) can call back into the store, which is what this guards.
    pub fn enter_load(&mut self, uri: &str) -> Result<(), XsltError> {
        if !self.loading.insert(Box::from(uri)) {
            return Err(XsltError::new(
                codes::XTDE1170,
                format!("the document {uri:?} is already being loaded (re-entrant document())"),
            ));
        }
        Ok(())
    }

    /// Releases the guard [`enter_load`](Self::enter_load) took.
    pub fn leave_load(&mut self, uri: &str) {
        self.loading.remove(uri);
    }

    /// The registration record of a document.
    pub fn get(&self, id: DocId) -> Option<&StoredDocument<'t>> {
        self.docs.get(id)
    }

    /// The store ordinal of a document.
    pub fn ordinal(&self, id: DocId) -> Option<u32> {
        self.docs.get(id).map(|d| d.ordinal)
    }

    /// The `DocId` a URI resolved to, if it has been loaded.
    pub fn id_of_uri(&self, uri: &str) -> Option<DocId> {
        self.by_uri.get(uri).copied()
    }

    /// A navigator on the document node of `id`.
    ///
    /// The navigator borrows `'t` — the arena — and **not** `&self`, so a
    /// caller may hold it across later [`insert`](Self::insert) and
    /// [`load_str`](Self::load_str) calls. That is the property §6.1 asks M0b
    /// to prove; `tests/m0b_invariants.rs::inv6_…` is the proof.
    ///
    /// # Panics
    ///
    /// If `id` does not belong to this store.
    pub fn navigator(&self, id: DocId) -> BufferDocNavigator<'t> {
        let stored = self
            .docs
            .get(id)
            .expect("DocId does not belong to this DocumentStore");
        stored.doc.create_navigator()
    }

    /// The store ordinal of the document a navigator points into.
    pub fn ordinal_of(&self, nav: &BufferDocNavigator<'t>) -> Option<u32> {
        let id = self.by_serial.get(&nav.document().serial())?;
        self.ordinal(*id)
    }

    /// `generate-id()` for a node (§16.6.1).
    ///
    /// `d{ordinal}n{node}` — the store ordinal, not
    /// [`BufferDocument::serial`], so the ids of two runs of the same
    /// transformation are identical (invariant 3). The node part is the
    /// navigator's flat node index, which is distinct for every element, text,
    /// comment, PI **and attribute** node.
    ///
    /// Namespace nodes are the one exception: the cursor stays on the owning
    /// element and the namespace is held in a private `current_ns` field, so
    /// the index alone would collide with the element's. The prefix — the only
    /// distinguishing part the navigator exposes — is appended instead.
    ///
    /// A document the store does not know (never inserted) falls back to its
    /// process-wide serial, marked `u`; such an id is unique but **not**
    /// reproducible.
    pub fn generate_id(&self, nav: &BufferDocNavigator<'t>) -> String {
        let doc = match self.ordinal_of(nav) {
            Some(ordinal) => format!("d{ordinal}"),
            None => format!("u{}", nav.document().serial()),
        };
        let node = nav.current_ref();
        match nav.node_type() {
            // `local_name()` of a namespace node is its prefix.
            DomNodeType::Namespace => format!("{doc}n{node}ns{}", nav.local_name()),
            _ => format!("{doc}n{node}"),
        }
    }
}

// ── The `doc()` function (§9.6, Q8) ──────────────────────────────────────

/// Compile-time catalog for the prototype's XSLT functions.
///
/// [`FunctionHandle`]'s constructor is `pub(crate)` upstream, so a handle can
/// only be minted by [`FunctionSet::register`] — a hand-written
/// [`FunctionCatalog`] is not possible from outside `xsd-schema`. The set is
/// therefore used as the catalog, and the closure it stores is a placeholder:
/// the real implementation is [`ProtoEvaluator`], which can hold borrowed
/// engine state that a `'static + Send + Sync` closure cannot.
pub struct ProtoFunctions<'t> {
    set: FunctionSet<BufferDocNavigator<'t>>,
    doc_handle: FunctionHandle,
}

impl Default for ProtoFunctions<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'t> ProtoFunctions<'t> {
    /// The built-ins plus `proto:doc($uri as xs:string) as node()`.
    pub fn new() -> Self {
        let mut set: FunctionSet<BufferDocNavigator<'t>> = FunctionSet::with_builtins();
        let doc_handle = set.register(
            DynamicFunctionSignature::new(
                PROTO_NS,
                "doc",
                vec![SequenceType::string()],
                SequenceType::node(),
            ),
            |_ctx, _args| {
                Err(XPathError::internal(
                    "proto:doc must be evaluated through xslt2::store::ProtoEvaluator, \
                     which holds the DocumentStore",
                ))
            },
        );
        Self { set, doc_handle }
    }

    /// Pass this to [`XPathContext::with_function_catalog`].
    ///
    /// [`XPathContext::with_function_catalog`]: xsd_schema::xpath::XPathContext::with_function_catalog
    pub fn catalog(&self) -> &dyn FunctionCatalog {
        &self.set
    }

    /// The evaluator to install with `DynamicContext::set_function_evaluator`.
    ///
    /// The store is borrowed for `'s`, **not** for `'t`: `&'t RefCell<DocumentStore<'t>>`
    /// — the shape §6.1 sketches — does not borrow-check, because the borrow
    /// would have to outlive the value that carries `'t` in its own type. The
    /// navigators the store hands out are `'t` regardless of how long the store
    /// itself is borrowed, which is what makes the shorter borrow enough.
    pub fn evaluator<'s>(
        &'s self,
        store: &'s RefCell<DocumentStore<'t>>,
    ) -> ProtoEvaluator<'s, 't> {
        ProtoEvaluator {
            funcs: self,
            store,
        }
    }
}

/// Runs the prototype's XSLT functions over a borrowed [`DocumentStore`].
///
/// This is the route §9.6 needs and the answer M0b gives to Q8: the
/// `DynamicContext` extension slot is typed `&dyn Any`, and `Any` implies
/// `'static`, so it cannot carry a `DocumentStore<'t>`. A `FunctionEvaluator`
/// implementation can, because `&'a dyn FunctionEvaluator<N>` is only bounded
/// by `'a`.
pub struct ProtoEvaluator<'s, 't> {
    funcs: &'s ProtoFunctions<'t>,
    store: &'s RefCell<DocumentStore<'t>>,
}

impl<'t> FunctionEvaluator<BufferDocNavigator<'t>> for ProtoEvaluator<'_, 't> {
    fn eval(
        &self,
        handle: FunctionHandle,
        ctx: &mut DynamicContext<'_, BufferDocNavigator<'t>>,
        args: Vec<XPathValue<BufferDocNavigator<'t>>>,
    ) -> Result<XPathValue<BufferDocNavigator<'t>>, XPathError> {
        if handle != self.funcs.doc_handle {
            return BuiltinEvaluator.eval(handle, ctx, args);
        }
        let uri = single_string(args.first()).ok_or_else(|| {
            XPathError::invalid_argument_type("proto:doc", "a single xs:string")
        })?;
        // The borrow ends with the statement; the navigator that comes out
        // borrows `'t`, so it outlives every later load.
        let id = self
            .store
            .borrow_mut()
            .load(&uri)
            // `XPathError` has no "host error with an arbitrary code" variant,
            // so the XSLT code travels as text. See the M0b report.
            .map_err(|e| XPathError::internal(e.to_string()))?;
        let nav = self.store.borrow().navigator(id);
        Ok(XPathValue::from_node(nav))
    }
}

/// The string value of a singleton atomic argument.
fn single_string<N: DomNavigator>(value: Option<&XPathValue<N>>) -> Option<String> {
    match value? {
        XPathValue::Item(item) => item.as_atomic().map(|v| v.to_string_value()),
        XPathValue::Sequence(items) if items.len() == 1 => {
            items[0].as_atomic().map(|v| v.to_string_value())
        }
        _ => None,
    }
}
