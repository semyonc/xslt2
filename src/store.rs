//! `DocumentStore` — every document a transformation can see, with addresses
//! that stay put while it keeps loading more.
//!
//! A transformation holds navigators for a long time
//! (global variables, key indexes, tunnel parameters, sequences in frames)
//! while it keeps loading documents, so the `BufferDocument` *structs*
//! themselves need stable addresses — a growable container that moves its
//! values cannot hand out `&'t` borrows. The store therefore moves every
//! finished document into the transformation arena
//! ([`bumpalo::Bump::alloc`]) and keeps only the resulting `&'t` reference in
//! its slot map. [`DocumentStore::navigator`] borrows `'t`, not `&self`, which
//! is what lets a caller keep a navigator across later loads.
//!
//! `fn:doc()` and `document()` do not exist upstream; they are implemented over
//! this store by `XsltEvaluator` (`runtime/functions.rs`), which holds it as
//! `&RefCell<DocumentStore>`, since the XPath engine's `DynamicContext`
//! extension slot is `&dyn Any` and cannot carry borrowed state.

use std::collections::{HashMap, HashSet};

use crate::error::{codes, XsltError};
use bumpalo::Bump;
use slotmap::{new_key_type, SlotMap};
use xsd_schema::document::{BufferDocNavigator, BufferDocument};
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::SchemaSet;

new_key_type! {
    /// The store's own slot key; [`DocId`] wraps it so that the key type of
    /// the slot map is not part of the public API.
    struct Slot;
}

/// Identifies one document inside a [`DocumentStore`]: what
/// [`Transformation::load_document`](crate::Transformation::load_document) and
/// [`DocumentStore::insert`] return, and what
/// [`Transformation::transform_document`](crate::Transformation::transform_document)
/// and [`DocumentStore::navigator`] take. An id is meaningful only for the
/// store that issued it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocId(Slot);

/// One document known to a transformation.
pub(crate) struct StoredDocument<'t> {
    /// Allocated with `arena.alloc(..)`: the address is fixed for `'t`.
    pub(crate) doc: &'t BufferDocument<'t>,
    /// `document-uri()`; `None` for a temporary tree or a hidden fragment.
    pub(crate) uri: Option<Box<str>>,
    /// Stable position in the store's own document order. Unlike
    /// [`BufferDocument::serial`], which is a process-wide counter, this is
    /// reproducible from run to run — see [`DocumentStore::generate_id`].
    pub(crate) ordinal: u32,
    /// Whether §4.4 whitespace stripping was applied to this tree, which the
    /// transformation's admission path records. Only that: whether the tree
    /// has been *admitted* (§4.3) is a separate, private mark,
    /// which a host cannot set.
    pub(crate) stripped: bool,
    /// Whether the transformation has admitted this tree: loaded it itself, or
    /// taken a tree the host inserted through `Transformation::admit_stored`.
    pub(crate) admitted: bool,
}

/// The documents of one transformation ([`Transformation::store`](crate::Transformation::store)):
/// the sources it loaded, the documents `doc()` and `document()` read, the
/// trees it built, and the trees the host put in.
///
/// A host gives a run a tree it built itself — with `xsd-schema`'s builders, in
/// the run's [`arena`](Self::arena) and with its [`names`](Self::names) — by
/// [`insert`](Self::insert)ing it, and then runs over it with
/// [`Transformation::transform_document`](crate::Transformation::transform_document)
/// or passes one of its nodes ([`navigator`](Self::navigator)) as a
/// parameter. A tree inserted under a URI is also what `doc()` and
/// `document()` return for that URI.
///
/// The arena and the name table are the transformation's, borrowed: one
/// `NameTable` serves every document of the transformation, and one `Bump`
/// holds the nodes *and* the document structs, so a document keeps its
/// address while more are loaded.
pub struct DocumentStore<'t> {
    arena: &'t Bump,
    names: &'t NameTable,
    /// The schema set of a schema-aware transformation, which
    /// every tree built for it is built with ([`schema_set`](Self::schema_set)).
    schema_set: Option<&'t SchemaSet>,
    docs: SlotMap<Slot, StoredDocument<'t>>,
    /// `document()`/`doc()` identity: the same URI is the same document (§16.1).
    by_uri: HashMap<Box<str>, DocId>,
    /// `BufferDocument::serial()` → `DocId`, so a navigator can be mapped back
    /// to its store ordinal. The navigator exposes its document but not its
    /// `DocId`; the serial is the only stable key the document offers.
    by_serial: HashMap<u64, DocId>,
    /// Re-entrancy guard: URIs whose load is in progress.
    loading: HashSet<Box<str>>,
    /// The text registered for a URI (`register_source`): URI → document text.
    sources: HashMap<Box<str>, Box<str>>,
    next_ordinal: u32,
}

impl<'t> DocumentStore<'t> {
    /// A store over the transformation's arena and name table.
    pub(crate) fn new(arena: &'t Bump, names: &'t NameTable) -> Self {
        Self {
            arena,
            names,
            schema_set: None,
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

    /// The store of a schema-aware transformation: the trees built for it
    /// (temporary trees, result trees, the hidden documents of parentless
    /// nodes) are built with `schema_set`, so the type annotations their nodes
    /// are given resolve (§19.2; `names` is then the set's own table).
    pub(crate) fn with_schema_set(mut self, schema_set: Option<&'t SchemaSet>) -> Self {
        self.schema_set = schema_set;
        self
    }

    /// The schema set the trees of this transformation are built with — `None`
    /// for a basic processor, whose trees are never typed (§21.1).
    pub fn schema_set(&self) -> Option<&'t SchemaSet> {
        self.schema_set
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
    /// `Bump` does not run destructors, so the `Vec`/`HashMap` members of
    /// `BufferDocument` are never freed. That is acceptable for an arena that
    /// lives as long as a transformation, and is the cost of the `&'t` shape.
    pub fn insert(&mut self, doc: BufferDocument<'t>, uri: Option<&str>) -> DocId {
        let doc: &'t BufferDocument<'t> = self.arena.alloc(doc);
        self.insert_ref(doc, uri)
    }

    /// Registers a document that is **already** allocated in the arena.
    ///
    /// The admission path of a source document parses, then rebuilds the tree
    /// without its stripped whitespace (§4.4), and only the survivor is
    /// registered; both trees live in the arena, so the caller holds `&'t`
    /// references rather than owned documents.
    pub fn insert_ref(&mut self, doc: &'t BufferDocument<'t>, uri: Option<&str>) -> DocId {
        let serial = doc.serial();
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let id = DocId(self.docs.insert(StoredDocument {
            doc,
            uri: uri.map(Box::from),
            ordinal,
            stripped: false,
            admitted: false,
        }));
        self.by_serial.insert(serial, id);
        if let Some(uri) = uri {
            self.by_uri.insert(Box::from(uri), id);
        }
        id
    }

    /// Replaces the document registered as `id` by `doc` (already in the arena) — the admission
    /// copy of a typed tree the host inserted (`Transformation::admit_stored`), which stands for it
    /// under the same `DocId` and URI — and marks it admitted.
    pub(crate) fn replace_ref(&mut self, id: DocId, doc: &'t BufferDocument<'t>) {
        let Some(stored) = self.docs.get_mut(id.0) else {
            return;
        };
        self.by_serial.remove(&stored.doc.serial());
        stored.doc = doc;
        stored.admitted = true;
        self.by_serial.insert(doc.serial(), id);
    }

    /// Marks the document `id` as admitted by the transformation (§4.3, §4.4).
    pub(crate) fn mark_admitted(&mut self, id: DocId) {
        if let Some(stored) = self.docs.get_mut(id.0) {
            stored.admitted = true;
        }
    }

    /// Parses `xml` into the arena **without** registering it, recording
    /// `base_uri` as the document's own base URI.
    ///
    /// Splitting the parse from the registration is what lets one admission
    /// path apply §4.4 whitespace stripping: the raw tree is navigable but
    /// never visible to `document()`, and only the stripped copy is inserted.
    ///
    /// `base_uri` becomes [`BufferDocument::set_document_base_uri`], so
    /// `fn:base-uri` on a node of this document reports the document's URI
    /// rather than falling back to the static base URI of whichever expression
    /// happens to ask.
    pub(crate) fn parse(
        &self,
        xml: &str,
        base_uri: Option<&str>,
    ) -> Result<&'t BufferDocument<'t>, XsltError> {
        let mut doc = BufferDocument::from_reader_default(xml.as_bytes(), self.arena, self.names)
            .map_err(|e| {
            XsltError::new(
                codes::XTDE1170,
                match base_uri {
                    Some(uri) => format!("could not read the document {uri:?}: {e}"),
                    None => format!("could not read a document: {e}"),
                },
            )
        })?;
        if let Some(uri) = base_uri {
            doc.set_document_base_uri(Some(self.arena.alloc_str(uri)));
        }
        Ok(self.arena.alloc(doc))
    }

    /// Interns `uri` in the arena, for a caller that needs a `&'t str`.
    pub(crate) fn intern_uri(&self, uri: &str) -> &'t str {
        self.arena.alloc_str(uri)
    }

    /// The text [`register_source`](Self::register_source) recorded for `uri`.
    pub(crate) fn source_text(&self, uri: &str) -> Option<&str> {
        self.sources.get(uri).map(|text| &**text)
    }

    /// Records that §4.4 stripping produced this tree.
    pub(crate) fn mark_stripped(&mut self, id: DocId) {
        if let Some(stored) = self.docs.get_mut(id.0) {
            stored.stripped = true;
        }
    }

    /// The `document-uri()` of the document a navigator points into.
    ///
    /// `None` for a temporary tree, a result tree or a hidden fragment, which
    /// §9.4 says have an empty `document-uri` property.
    pub fn uri_of(&self, nav: &BufferDocNavigator<'t>) -> Option<&str> {
        let id = self.by_serial.get(&nav.document().serial())?;
        self.docs.get(id.0)?.uri.as_deref()
    }

    /// Registers the text a later load of `uri` should parse.
    pub(crate) fn register_source(&mut self, uri: &str, xml: &str) {
        self.sources.insert(Box::from(uri), Box::from(xml));
    }

    /// Parses `xml` as the document named `uri`.
    ///
    /// Loading the same URI twice returns the same [`DocId`] and does not
    /// re-parse, so node identities are stable across calls (§16.1). A URI
    /// whose load is already in progress is
    /// [`XTDE1170`](crate::error::codes::XTDE1170) — an error, never a panic or
    /// a double borrow.
    ///
    /// This is the **raw** store operation: it does not apply the §4.4
    /// whitespace stripping a *source tree* is subject to.
    /// [`Transformation::load_document`](crate::runtime::Transformation::load_document)
    /// is the admission path that does.
    #[cfg(test)]
    pub(crate) fn load_str(&mut self, uri: &str, xml: &str) -> Result<DocId, XsltError> {
        if let Some(&id) = self.by_uri.get(uri) {
            return Ok(id);
        }
        self.enter_load(uri)?;
        let parsed = self.parse(xml, Some(uri));
        self.leave_load(uri);
        let doc = parsed?;
        self.register_source(uri, xml);
        Ok(self.insert_ref(doc, Some(uri)))
    }

    /// Marks `uri` as being loaded, or reports the re-entrant load.
    ///
    /// The guard, not the parse, is the interesting state: a parse is atomic,
    /// so nothing re-enters it on its own, but a resolver can call back into
    /// the store, which is what this guards.
    pub(crate) fn enter_load(&mut self, uri: &str) -> Result<(), XsltError> {
        if !self.loading.insert(Box::from(uri)) {
            return Err(XsltError::new(
                codes::XTDE1170,
                format!("the document {uri:?} is already being loaded (re-entrant document())"),
            ));
        }
        Ok(())
    }

    /// Releases the guard [`enter_load`](Self::enter_load) took.
    pub(crate) fn leave_load(&mut self, uri: &str) {
        self.loading.remove(uri);
    }

    /// The registration record of a document.
    pub(crate) fn get(&self, id: DocId) -> Option<&StoredDocument<'t>> {
        self.docs.get(id.0)
    }

    /// The store ordinal of a document.
    pub(crate) fn ordinal(&self, id: DocId) -> Option<u32> {
        self.docs.get(id.0).map(|d| d.ordinal)
    }

    /// The `DocId` a URI resolved to, if it has been loaded.
    pub fn id_of_uri(&self, uri: &str) -> Option<DocId> {
        self.by_uri.get(uri).copied()
    }

    /// A navigator on the document node of `id`.
    ///
    /// The navigator borrows `'t` — the arena — and **not** `&self`, so a
    /// caller may hold it across later [`insert`](Self::insert)s and loads
    /// (the test `inv6_global_variable_spans_two_documents_through_a_recursive_function`
    /// checks it).
    ///
    /// # Panics
    ///
    /// If `id` does not belong to this store.
    pub fn navigator(&self, id: DocId) -> BufferDocNavigator<'t> {
        let stored = self
            .docs
            .get(id.0)
            .expect("DocId does not belong to this DocumentStore");
        stored.doc.create_navigator()
    }

    /// The store ordinal of the document a navigator points into.
    pub(crate) fn ordinal_of(&self, nav: &BufferDocNavigator<'t>) -> Option<u32> {
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
    pub(crate) fn generate_id(&self, nav: &BufferDocNavigator<'t>) -> String {
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

#[cfg(test)]
mod tests;
