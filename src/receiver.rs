//! The push pipeline: raw events → [`ComplexContentNormalizer`] → one
//! destination.
//!
//! Design sketch §9.2–§9.3. The instruction evaluator emits **raw** events in
//! evaluation order; all the rules of XSLT 2.0 §5.7.1 live in one component,
//! the normalizer, which wraps every tree-shaped destination. Because
//! [`ResultBuilder`] and [`EventSerializer`] consume the *same* normalized
//! stream, materialise-then-serialize and serialize-directly cannot disagree —
//! architecture invariant 2.
//!
//! [`SequenceCollector`] is the other destination: the `as=` cases, where an
//! existing node keeps its identity instead of being copied (§6.3). The
//! distinction is drawn by *which* trait method an instruction calls —
//! [`Receiver::append_item`] appends the item itself (`xsl:sequence`, §11.10)
//! and [`Receiver::append_copy`] appends a new node that is a deep copy of it
//! (`xsl:copy`, `xsl:copy-of`, §11.9) — never by the receiver guessing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xsd_schema::document::{
    serialize, BufferDocNavigator, BufferDocument, BufferDocumentBuilder, BufferDocumentOptions,
    CopySource, DocumentKind, NamespaceFixup, NodeSchemaBinding,
};
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope, TypedValue};
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSet;

use crate::error::{codes, XsltError};
use crate::store::{DocId, DocumentStore};

/// The navigator every receiver in this crate carries nodes of (Q22).
pub type Nav<'t> = BufferDocNavigator<'t>;

/// An element or attribute name, in the shape [`BufferDocumentBuilder`] wants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name {
    /// Local part.
    pub local: String,
    /// Namespace URI (empty for no namespace).
    pub uri: String,
    /// Preferred prefix; namespace fixup may replace it.
    pub prefix: String,
}

impl Name {
    /// A name in no namespace.
    pub fn local(local: &str) -> Self {
        Self {
            local: local.to_string(),
            uri: String::new(),
            prefix: String::new(),
        }
    }

    /// A prefixed name in a namespace.
    pub fn qualified(prefix: &str, local: &str, uri: &str) -> Self {
        Self {
            local: local.to_string(),
            uri: uri.to_string(),
            prefix: prefix.to_string(),
        }
    }

    /// The name of the node at a navigator's cursor.
    pub fn of<N: DomNavigator>(nav: &N) -> Self {
        Self {
            local: nav.local_name().to_string(),
            uri: nav.namespace_uri().to_string(),
            prefix: nav.prefix().to_string(),
        }
    }
}

/// The namespace **nodes** of an existing element, prefix → URI.
///
/// §11.9.1 and §11.9.2 both say "all the namespace nodes of the original
/// element are copied", so this is the list a copying instruction pushes. The
/// axis walked with [`NamespaceAxisScope::ExcludeXml`] is upstream's view of
/// the *declarations* — it keeps `xmlns=""` so that serialization and shallow
/// copy can see it — and the walk dedups by prefix innermost-first, so
/// dropping the zero-length URIs afterwards leaves exactly the bindings that
/// are in scope: the element's namespace nodes. An undeclaration is the
/// absence of a node, not a node (§5.7.1 speaks only of "a namespace node with
/// no name").
pub(crate) fn namespace_nodes_of(
    nav: &Nav<'_>,
    scope: NamespaceAxisScope,
) -> Vec<(String, String)> {
    namespace_nodes_and_undeclarations_of(nav, scope).0
}

/// [`namespace_nodes_of`], plus the prefixes the same walk found
/// **undeclared** (`xmlns:p=""`) — the prefixed bindings that are absent from
/// the element although an ancestor declares them. The default namespace is
/// not among them: whether an element has a namespace node with no name is
/// tracked separately (`Open::no_default`).
///
/// A deep copy needs both halves: the nodes to copy, and — because the walk
/// dedups innermost-first — whether a prefix its original *parent* binds is
/// missing from the original itself, which is the only way an element can
/// lack a prefixed namespace node its parent has (§5.7.3: "XSLT 2.0 supports
/// the creation of result trees that do not satisfy this constraint").
fn namespace_nodes_and_undeclarations_of(
    nav: &Nav<'_>,
    scope: NamespaceAxisScope,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut nodes: Vec<(String, String)> = Vec::new();
    let mut undeclared: Vec<String> = Vec::new();
    let mut ns = nav.clone();
    if ns.move_to_first_namespace(scope) {
        loop {
            // `local_name()` of a namespace node is its prefix.
            let (prefix, uri) = (ns.local_name().to_string(), ns.value());
            if !uri.is_empty() {
                nodes.push((prefix, uri));
            } else if !prefix.is_empty() {
                undeclared.push(prefix);
            }
            if !ns.move_to_next_namespace(scope) {
                break;
            }
        }
    }
    (nodes, undeclared)
}

/// A prefix for an element whose own one is taken by a namespace node it
/// carries (§11.7), derived from the prefix that was asked for.
///
/// §5.7.3 leaves the choice open — "the actual namespace nodes that are added
/// to the tree by the namespace fixup process are implementation-dependent" —
/// so the only requirements are that the prefix be free and that the choice be
/// deterministic. `p` becomes `p_0`, `p_1`, … , which is what the suite's
/// reference results were produced with (`namespace-2614`, `namespace-2001`,
/// `namespace-alias-1903`); an unprefixed name, which has no prefix to derive
/// from, becomes `ns_0`.
fn invented_prefix(requested: &str, keep: &[(String, String)], fixup: &NamespaceFixup) -> String {
    let stem = if requested.is_empty() {
        "ns"
    } else {
        requested
    };
    for n in 0u32.. {
        let candidate = format!("{stem}_{n}");
        let taken = keep.iter().any(|(prefix, _)| *prefix == candidate)
            || fixup.in_scope(&candidate).is_some();
        if !taken {
            return candidate;
        }
    }
    unreachable!("u32 is not exhausted by the prefixes of one element")
}

/// Which namespace nodes a deep copy carries (§11.9.2 `copy-namespaces`).
///
/// > If this attribute is omitted, or takes the value `yes`, then all the
/// > namespace nodes of the original element are copied to the new element. If
/// > it takes the value `no`, then **none** of the namespace nodes are copied:
/// > however, namespace nodes will still be created in the result tree as
/// > required by the namespace fixup process … This attribute affects all
/// > elements copied by this instruction: both elements selected directly by
/// > the `select` expression, and elements that are descendants of nodes
/// > selected by the `select` expression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyNamespaces {
    /// `copy-namespaces="yes"` (the default): every in-scope namespace node.
    #[default]
    All,
    /// `copy-namespaces="no"`: **no** namespace node is copied — not even one
    /// the source element declares itself. What the result ends up declaring
    /// is exactly what namespace fixup (§5.7.3) adds for the element's own
    /// name and its attributes' names.
    FixupOnly,
}

impl CopyNamespaces {
    /// The namespace axis a copy walks, or `None` when it copies none.
    fn scope(self) -> Option<NamespaceAxisScope> {
        match self {
            CopyNamespaces::All => Some(NamespaceAxisScope::ExcludeXml),
            CopyNamespaces::FixupOnly => None,
        }
    }
}

/// Whether a copy carries the source's PSVI state (§6.4, `xsl:copy-of
/// validation`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Annotations {
    /// `validation="strip"`: the copy is untyped.
    #[default]
    Strip,
    /// `validation="preserve"`: type annotation, declaration binding and the
    /// nilled state travel with the copy.
    Preserve,
}

/// A destination for the event stream (§9.2).
pub trait Receiver<'t> {
    /// Opens a document node.
    fn start_document(&mut self) -> Result<(), XsltError>;
    /// Closes the document node.
    fn end_document(&mut self) -> Result<(), XsltError>;
    /// Opens an element.
    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError>;
    /// A namespace node on the open element.
    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError>;
    /// An attribute of the open element.
    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError>;
    /// No more attributes or namespaces on the open element.
    fn end_start_tag(&mut self) -> Result<(), XsltError>;
    /// Character data.
    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError>;
    /// A comment node.
    fn comment(&mut self, s: &str) -> Result<(), XsltError>;
    /// A processing instruction.
    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError>;
    /// Closes the open element.
    fn end_element(&mut self) -> Result<(), XsltError>;
    /// An item of the result sequence that is **not** decomposed into events by
    /// the caller: an atomic value, or an existing node passed by reference.
    ///
    /// A sequence collector keeps the reference; a content receiver copies it.
    /// That decision belongs to the normalizer, never to an instruction.
    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError>;

    /// The base URI of a document node just opened with
    /// [`start_document`](Receiver::start_document) (§11.5).
    ///
    /// §9.4 fixes the base URI of a temporary tree to that of the variable
    /// binding element, and §11.5's `xsl:document` is the same shape: the node
    /// the instruction creates takes the instruction's own base URI. Only a
    /// receiver that produces a document node with an identity of its own — a
    /// [`SequenceCollector`] building a `GroupKind::Document` — has anywhere to
    /// put it, hence the no-op default.
    fn document_base_uri(&mut self, _uri: &str) -> Result<(), XsltError> {
        Ok(())
    }

    /// The base URI of an element just opened with
    /// [`start_element`](Receiver::start_element), for the case where the
    /// element ends up **parentless** (§11.9.1, `xsl:copy`).
    ///
    /// > The base URI of a node is copied … If the copied node is subsequently
    /// > attached as a child to a new element or document node, the final copy
    /// > of the node inherits its base URI from its parent node.
    ///
    /// Only a [`SequenceCollector`] building a parentless element has anywhere
    /// to put it — on the hidden document the element lives in; in element or
    /// document content the parent decides, hence the no-op default.
    fn element_base_uri(&mut self, _uri: &str) -> Result<(), XsltError> {
        Ok(())
    }

    /// `inherit-namespaces="no"` on the instruction that created the element
    /// just opened (§11.1's `[xsl:]inherit-namespaces`, §11.2, §11.9.1).
    ///
    /// > When constructing the content of an element, the `inherit-namespaces`
    /// > attribute of the `xsl:element` or `xsl:copy` instruction, or the
    /// > `xsl:inherit-namespaces` property of the literal result element,
    /// > determines whether namespace nodes are to be inherited.
    ///
    /// Called between [`start_element`](Receiver::start_element) and the first
    /// child. The default implementation is a no-op, which is right for every
    /// receiver that sits *behind* a [`ComplexContentNormalizer`]: inheritance
    /// is the normalizer's business.
    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    /// Appends a **copy** of an item: what `xsl:copy-of` and `xsl:copy` do
    /// (§11.9.1, §11.9.2), as opposed to [`append_item`](Receiver::append_item),
    /// which appends the item itself (`xsl:sequence`, §11.10).
    ///
    /// The difference is only observable where a receiver would otherwise keep
    /// a node by reference — a [`SequenceCollector`]. In element or document
    /// content every node is copied anyway, so the two coincide there; what
    /// the `namespaces` argument adds is the `copy-namespaces` setting of the
    /// copying instruction.
    ///
    /// > If the item is an element node, a **new** element is constructed and
    /// > appended to the result sequence.
    ///
    /// The default implementation forwards to `append_item`, which is right
    /// for a receiver that decomposes every item into events regardless.
    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        _namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        self.append_item(item)
    }
}

// ── TemporaryOutputState ─────────────────────────────────────────────────

/// The content of an `xsl:document` instruction, passed on to `out` with the
/// disable-escaping property of its text dropped (§20.2).
///
/// §11.5: "The temporary tree rooted at this document node forms the result
/// tree" — and §19.1: temporary output state "applies when instructions are
/// writing to a temporary tree or any other non-final destination", whose
/// consequence §20.2 spells out: "If output escaping is disabled for an
/// xsl:value-of or xsl:text instruction evaluated when temporary output state
/// is in effect, the request to disable output escaping is ignored." The
/// document node's children may still end up in a final result tree (§5.7.1
/// replaces a document node in element or document content by its children),
/// so the receiver behind this one may be honouring the property; this
/// wrapper is what keeps it from seeing it.
///
/// Every [`Receiver`] method is forwarded, the defaulted ones included.
pub(crate) struct TemporaryOutputState<'r, 't> {
    out: &'r mut dyn Receiver<'t>,
}

impl<'r, 't> TemporaryOutputState<'r, 't> {
    pub(crate) fn new(out: &'r mut dyn Receiver<'t>) -> Self {
        Self { out }
    }
}

impl<'t> Receiver<'t> for TemporaryOutputState<'_, 't> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        self.out.start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.out.end_document()
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        self.out.start_element(name, binding, nilled)
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.out.namespace(prefix, uri)
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.out.attribute(name, value, binding)
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.out.end_start_tag()
    }

    fn text(&mut self, s: &str, _disable_escaping: bool) -> Result<(), XsltError> {
        self.out.text(s, false)
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.out.comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.out.pi(target, data)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.out.end_element()
    }

    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        self.out.append_item(item)
    }

    fn document_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        self.out.document_base_uri(uri)
    }

    fn element_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        self.out.element_base_uri(uri)
    }

    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        self.out.no_inherit_namespaces()
    }

    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        self.out.append_copy(item, namespaces)
    }
}

// ── ComplexContentNormalizer ─────────────────────────────────────────────

/// One open container of the normalizer.
#[derive(Default)]
struct Open {
    /// The element's name; `None` for the document level.
    name: Option<Name>,
    binding: Option<NodeSchemaBinding>,
    nilled: bool,
    /// Namespace nodes seen for this element, not yet flushed.
    namespaces: Vec<(String, String)>,
    /// Attributes seen for this element, not yet flushed.
    attributes: Vec<(Name, String, Option<NodeSchemaBinding>)>,
    /// The start tag has been written downstream.
    flushed: bool,
    /// A child (anything that is not an attribute or namespace) has been added.
    children_started: bool,
    /// The item appended last was an atomic value — §5.7.1's separator rule.
    last_atomic: bool,
    /// The element is known to have **no** namespace node with no name, even
    /// though a default namespace is in scope in the result tree — so §5.7.1's
    /// inheritance rule does not reach it and fixup has to write `xmlns=""`.
    ///
    /// A producer states this by sending a namespace event with an empty
    /// prefix *and* an empty URI: the undeclaration is not a namespace node
    /// (see [`push_namespace`](ComplexContentNormalizer::push_namespace)), it
    /// is the absence of one, and this is where that absence is recorded.
    no_default: bool,
    /// Whether this element's namespace nodes are copied to its descendants —
    /// §5.7.1's last rule, switched off by `inherit-namespaces="no"`
    /// ([`Receiver::no_inherit_namespaces`]). `true` at the document level, so
    /// a top-level element is unaffected.
    inherit: bool,
    /// The parent's instruction said `inherit-namespaces="no"`, so this element
    /// gets none of the parent's namespace nodes: every prefixed binding in
    /// scope at the parent is withheld from it at flush time
    /// ([`withheld_bindings`](ComplexContentNormalizer::withheld_bindings)).
    parent_withholds: bool,
    /// The declarations written for this element — what it adds to (or, for
    /// a withheld binding, removes from) the scope of its parent. Recorded at
    /// flush time; read only to enumerate the bindings a child of a
    /// non-inheriting element must not inherit.
    declared: Vec<(String, String)>,
    /// The prefixed bindings in scope at this element, its own declarations
    /// included — what each child of a non-inheriting element withholds.
    /// `None` until something needs it
    /// ([`prefixed_scope_at`](ComplexContentNormalizer::prefixed_scope_at));
    /// fixed once computed, because an element's `declared` is final before
    /// its first child is flushed.
    prefixed_scope: Option<PrefixedScope>,
    /// The element is a deep copy of an existing element (§11.9.2), made by
    /// [`copy_subtree`](ComplexContentNormalizer::copy_subtree) — the root of
    /// the copy or an element below it. A copied element is not constructed
    /// by a sequence constructor, so §5.7.1's inheritance rule does not start
    /// at it: what its copied children get from it is decided by
    /// [`fixup_name`](Self::fixup_name), [`fixup_nodes`](Self::fixup_nodes),
    /// [`owns_default`](Self::owns_default) and the child's own
    /// [`undeclared`](Self::undeclared).
    copied: bool,
    /// For a copied element (`copy-namespaces="yes"`): the prefixes the
    /// original lacks although an ancestor of the original declares them.
    /// Those its original parent binds are the ones the copy must not inherit
    /// from the copy of that parent.
    undeclared: Vec<String>,
    /// For a copied element, recorded at flush time: the prefixed namespace
    /// node its name needs is one of its own that namespace fixup produced —
    /// not copied from the original. The binding is the name's (`name`), so
    /// the commonest case costs no allocation.
    fixup_name: bool,
    /// For a copied element, recorded at flush time: the other namespace
    /// nodes it has only because namespace fixup produced them, for an
    /// attribute's name (or for a name §11.7 re-prefixed), prefix → URI.
    ///
    /// Its copied children inherit neither (§5.7.3: "the namespace fixup
    /// process does not add a namespace node to an element merely because its
    /// parent node in the result tree has such a namespace node").
    fixup_nodes: Vec<(String, String)>,
    /// For a copied element, recorded at flush time: it has a namespace node
    /// with no name of its own — copied, or produced by fixup for an
    /// unprefixed name in a namespace. A copied child without one of its own
    /// then has none (`xmlns=""`).
    owns_default: bool,
}

/// The prefixed namespace bindings in scope at an element, prefix → URI:
/// innermost declaration first, one entry per prefix, `xml` and undeclared
/// prefixes left out.
///
/// Shared, not copied, between an element and the descendants that declare
/// no prefix of their own, so that carrying it down a chain of elements
/// without declarations costs a reference count per element: what a
/// non-inheriting element withholds from its children is found in time
/// proportional to the bindings in scope, not to its depth (review R1).
type PrefixedScope = Arc<[(String, String)]>;

/// The [`PrefixedScope`] of an element that writes `declared`, given that of
/// its parent, `outer`.
///
/// The element's own prefixed declarations come first — the first one per
/// prefix, an undeclaration (`xmlns:p=""`) shadowing without an entry of its
/// own — then the entries of `outer` whose prefix the element does not
/// declare. That is the order a walk over the open elements' `declared`,
/// innermost first, would give.
fn with_declarations(outer: &PrefixedScope, declared: &[(String, String)]) -> PrefixedScope {
    let mut own = declared
        .iter()
        .filter(|(prefix, _)| !prefix.is_empty() && prefix != "xml")
        .peekable();
    if own.peek().is_none() {
        return Arc::clone(outer);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut scope: Vec<(String, String)> = Vec::new();
    for (prefix, uri) in own {
        if seen.insert(prefix.as_str()) && !uri.is_empty() {
            scope.push((prefix.clone(), uri.clone()));
        }
    }
    scope.extend(
        outer
            .iter()
            .filter(|(prefix, _)| !seen.contains(prefix.as_str()))
            .cloned(),
    );
    scope.into()
}

impl Open {
    /// A container that inherits, which is the default everywhere.
    fn inheriting() -> Self {
        Self {
            inherit: true,
            ..Self::default()
        }
    }
}

/// XSLT 2.0 §5.7.1 *Constructing Complex Content*, applied once for every
/// destination.
///
/// The spec lists the rules and says they are applied in order:
///
/// > Any atomic value in the sequence is cast to a string.
/// >
/// > Any consecutive sequence of strings within the result sequence is
/// > converted to a single text node, whose string value contains the content
/// > of each of the strings in turn, with a single space (#x20) used as a
/// > separator between successive strings.
/// >
/// > Any document node within the result sequence is replaced by a sequence
/// > containing each of its children, in document order.
/// >
/// > Zero-length text nodes within the result sequence are removed.
/// >
/// > Adjacent text nodes within the result sequence are merged into a single
/// > text node.
///
/// and, for the nodes that may not appear where they do:
///
/// > \[ERR XTDE0410\] It is a non-recoverable dynamic error if the result
/// > sequence used to construct the content of an element node contains a
/// > namespace node or attribute node that is preceded in the sequence by a
/// > node that is neither a namespace node nor an attribute node.
/// >
/// > \[ERR XTDE0420\] It is a non-recoverable dynamic error if the result
/// > sequence used to construct the content of a document node contains a
/// > namespace node or attribute node.
/// >
/// > \[ERR XTDE0430\] It is a non-recoverable dynamic error if the result
/// > sequence contains two or more namespace nodes having the same name but
/// > different string values (that is, namespace nodes that map the same prefix
/// > to different namespace URIs).
/// >
/// > \[ERR XTDE0440\] It is a non-recoverable dynamic error if the result
/// > sequence contains a namespace node with no name and the element node being
/// > constructed has a null namespace URI (that is, it is an error to define a
/// > default namespace when the element is in no namespace).
///
/// and, for duplicates:
///
/// > If an attribute A in the result sequence has the same name as another
/// > attribute B that appears later in the result sequence, then attribute A is
/// > discarded from the result sequence.
///
/// Namespace fixup (§5.7.3) is delegated to upstream
/// [`NamespaceFixup::declarations_for`], which implements the same rules for
/// the XQuery constructor; M0b uses it unchanged, so the prototype has no
/// fixup code of its own. `exclude-result-prefixes`, `inherit-namespaces` and
/// `xsl:namespace-alias` are M1 work.
pub struct ComplexContentNormalizer<'t, R: Receiver<'t>> {
    out: R,
    stack: Vec<Open>,
    /// Buffered text, merged and flushed at the next structural event.
    text: String,
    text_pending: bool,
    fixup: NamespaceFixup,
    annotations: Annotations,
    copy_namespaces: CopyNamespaces,
    /// The depth of `stack` at which each open document node was started.
    ///
    /// A document node opened inside element content (`xsl:document`, or
    /// `xsl:copy` of a document node, in a sequence constructor that builds an
    /// element) contributes only its children to that element, so it pushes
    /// no [`Open`] of its own — but its content is still "the content of a
    /// document node", which XTDE0420 forbids an attribute or namespace node
    /// in. The innermost container is such a document node exactly when the
    /// last mark equals the current stack depth.
    document_marks: Vec<usize>,
    /// Whether `disable-output-escaping` reaches `out` (§20.2): only for the
    /// pipeline of a final result tree. Every other pipeline is a temporary
    /// destination, written in temporary output state — "If output escaping is
    /// disabled for an xsl:value-of or xsl:text instruction evaluated when
    /// temporary output state is in effect, the request to disable output
    /// escaping is ignored."
    honour_disabled_escaping: bool,
    /// The byte ranges of `text` that were written with escaping disabled.
    disabled: Vec<(usize, usize)>,
    _marker: std::marker::PhantomData<&'t ()>,
}

impl<'t, R: Receiver<'t>> ComplexContentNormalizer<'t, R> {
    /// A normalizer writing into `out`, stripping annotations.
    pub fn new(out: R) -> Self {
        Self::with_annotations(out, Annotations::Strip)
    }

    /// A normalizer that copies existing nodes with (or without) their PSVI
    /// state (§6.4).
    pub fn with_annotations(out: R, annotations: Annotations) -> Self {
        Self {
            out,
            stack: vec![Open::inheriting()],
            text: String::new(),
            text_pending: false,
            fixup: NamespaceFixup::new(),
            annotations,
            copy_namespaces: CopyNamespaces::All,
            document_marks: Vec::new(),
            honour_disabled_escaping: false,
            disabled: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Passes `disable-output-escaping` on to the destination (§20.2): for the
    /// pipeline that builds a final result tree, in final output state. Text
    /// that arrives with escaping disabled is still merged with its neighbours
    /// into one text node (§5.7.1); the destination receives it as consecutive
    /// [`Receiver::text`] calls, one per run of equally-flagged characters.
    pub fn honouring_disabled_escaping(mut self) -> Self {
        self.honour_disabled_escaping = true;
        self
    }

    /// Whether the innermost container being built is a document node, so
    /// that an attribute or namespace node here is XTDE0420.
    fn in_document_content(&self) -> bool {
        !self.in_element() || self.document_marks.last() == Some(&self.stack.len())
    }

    /// The default `copy-namespaces` setting of this pipeline.
    pub fn with_copy_namespaces(mut self, namespaces: CopyNamespaces) -> Self {
        self.copy_namespaces = namespaces;
        self
    }

    /// The destination, once the stream is finished.
    pub fn into_inner(self) -> R {
        self.out
    }

    fn top(&mut self) -> &mut Open {
        self.stack
            .last_mut()
            .expect("the document level is never popped")
    }

    fn in_element(&self) -> bool {
        self.stack.len() > 1
    }

    /// Merges and emits the buffered text, dropping a zero-length one.
    fn flush_text(&mut self) -> Result<(), XsltError> {
        if !self.text_pending {
            return Ok(());
        }
        self.text_pending = false;
        let text = std::mem::take(&mut self.text);
        let disabled = std::mem::take(&mut self.disabled);
        if text.is_empty() {
            return Ok(());
        }
        self.flush_start_tag()?;
        self.top().children_started = true;
        if disabled.is_empty() {
            return self.out.text(&text, false);
        }
        let mut at = 0;
        for (start, end) in disabled {
            if start > at {
                self.out.text(&text[at..start], false)?;
            }
            self.out.text(&text[start..end], true)?;
            at = end;
        }
        if at < text.len() {
            self.out.text(&text[at..], false)?;
        }
        Ok(())
    }

    /// Writes the pending start tag: namespace fixup, then the declarations and
    /// attributes the element ends up with.
    fn flush_start_tag(&mut self) -> Result<(), XsltError> {
        if !self.in_element() || self.stack.last().expect("checked").flushed {
            return Ok(());
        }
        // The bindings this element must not inherit from its parent
        // (`withheld_bindings`), taken out of scope *before* fixup runs, so that
        // fixup (§5.7.3) re-declares on this element exactly the ones its own
        // name, its attributes and its own namespace nodes need — "namespace
        // nodes will still be added … as required by the namespace fixup
        // process" — and the rest end up undeclared.
        let (withheld, default_withheld) = self.withheld_bindings();
        if !withheld.is_empty() {
            let undeclarations: Vec<(String, String)> = withheld
                .iter()
                .map(|(prefix, _)| (prefix.clone(), String::new()))
                .collect();
            self.fixup.push_scope(&undeclarations);
        }
        let open = self.stack.last_mut().expect("checked");
        open.flushed = true;
        let mut name = open.name.clone().unwrap_or_default();
        let binding = open.binding;
        let nilled = open.nilled;
        let mut keep: Vec<(String, String)> = open.namespaces.clone();
        // §5.7.1: the element has no namespace node with no name, so a default
        // namespace already in scope in the result tree must be undeclared.
        // Upstream fixup reads a `("", "")` entry as exactly that request and
        // drops it again when no default is in scope. The guard is for the one
        // shape that would contradict it — an unprefixed name *in* a namespace
        // has a default-namespace node by definition.
        let name_has_a_default = name.prefix.is_empty() && !name.uri.is_empty();
        if (open.no_default || default_withheld) && !name_has_a_default {
            keep.push((String::new(), String::new()));
        }
        let attrs: Vec<(Name, String, Option<NodeSchemaBinding>)> =
            std::mem::take(&mut open.attributes);

        // §11.7: "If a namespace prefix is mapped to a particular namespace URI
        // using the xsl:namespace instruction, or by using xsl:copy or
        // xsl:copy-of to copy a namespace node, this prevents the namespace
        // fixup process (and hence the serializer) from using the same prefix
        // for a different namespace URI on the same element." The explicit
        // namespace node wins and the element's own prefix is re-derived; the
        // engine used to drop the explicit node instead. §11.7's own example is
        // `namespace-2615`'s stylesheet.
        if !name.uri.is_empty() {
            if let Some((_, bound)) = keep.iter().find(|(prefix, _)| *prefix == name.prefix) {
                if *bound != name.uri {
                    name.prefix = invented_prefix(&name.prefix, &keep, &self.fixup);
                }
            }
        }

        let attr_names: Vec<(&str, &str)> = attrs
            .iter()
            .map(|(n, _, _)| (n.prefix.as_str(), n.uri.as_str()))
            .collect();
        let keep_refs: Vec<(&str, &str)> =
            keep.iter().map(|(p, u)| (p.as_str(), u.as_str())).collect();
        // Pushes the scope it computes; `end_element` pops it.
        let (mut decls, fixed) = self.fixup.declarations_for(
            (name.prefix.as_str(), name.uri.as_str()),
            &attr_names,
            &keep_refs,
        );
        if !withheld.is_empty() {
            // One scope per element, as `end_element` expects. A withheld
            // binding that fixup did not re-declare becomes an undeclaration
            // (`xmlns:p=""`, the tree's way of saying "no namespace node `p`" —
            // the serializer never writes one). One it re-declared with the
            // very URI the parent binds the prefix to is dropped instead,
            // together with its undeclaration: inheriting the parent's binding
            // gives the element the same namespace node, and re-declaring it
            // on every element of a deep non-inheriting chain made the tree
            // carry one declaration per prefix per level, which upstream's
            // namespace axis walks once per ancestor (review N2).
            self.fixup.pop_scope();
            self.fixup.pop_scope();
            let mut merged: Vec<(String, String)> = withheld
                .iter()
                .filter(|(prefix, _)| !decls.iter().any(|(declared, _)| declared == prefix))
                .map(|(prefix, _)| (prefix.clone(), String::new()))
                .collect();
            decls.retain(|declaration| !withheld.contains(declaration));
            merged.append(&mut decls);
            self.fixup.push_scope(&merged);
            decls = merged;
        }

        self.out.start_element(&name, binding, nilled)?;
        for (prefix, uri) in &decls {
            self.out.namespace(prefix, uri)?;
        }
        let open = self.stack.last_mut().expect("checked");
        open.declared = decls;
        if open.copied {
            // What a copied element has of its own besides the namespace nodes
            // it copied: the nodes its (final) name and attribute names need,
            // which fixup produced — or, for the default namespace, the one an
            // unprefixed name in a namespace has by definition.
            let copied_prefix = |namespaces: &[(String, String)], prefix: &str| -> bool {
                namespaces.iter().any(|(copied, _)| copied == prefix)
            };
            open.owns_default = name_has_a_default || copied_prefix(&open.namespaces, "");
            let name_is_own = !name.prefix.is_empty()
                && !name.uri.is_empty()
                && name.prefix != "xml"
                && !copied_prefix(&open.namespaces, &name.prefix);
            let name_kept = open
                .name
                .as_ref()
                .is_some_and(|requested| requested.prefix == name.prefix);
            open.fixup_name = name_is_own && name_kept;
            let mut fixup_nodes: Vec<(String, String)> = Vec::new();
            if name_is_own && !name_kept {
                fixup_nodes.push((name.prefix.clone(), name.uri.clone()));
            }
            for (prefix, uri) in &fixed {
                if prefix.is_empty()
                    || uri.is_empty()
                    || prefix == "xml"
                    || (name_is_own && *prefix == name.prefix)
                    || copied_prefix(&open.namespaces, prefix)
                    || fixup_nodes.iter().any(|(seen, _)| seen == prefix)
                {
                    continue;
                }
                fixup_nodes.push((prefix.clone(), uri.clone()));
            }
            open.fixup_nodes = fixup_nodes;
        }
        for ((name, value, binding), (prefix, _)) in attrs.iter().zip(fixed.iter()) {
            let fixed_name = Name {
                local: name.local.clone(),
                uri: name.uri.clone(),
                prefix: prefix.clone(),
            };
            self.out.attribute(&fixed_name, value, *binding)?;
        }
        self.out.end_start_tag()
    }

    /// What the element being flushed must **not** inherit from its parent:
    /// the prefixed bindings, each with the URI the parent binds it to, and
    /// whether the parent's default namespace is withheld too.
    ///
    /// Two shapes withhold anything; everywhere else the element inherits
    /// what is in scope at its parent, as the tree does by construction.
    ///
    /// **The parent's instruction said `inherit-namespaces="no"`** (§11.1.3,
    /// §11.2, §11.9.1):
    ///
    /// > If the value is set to `no`, then these namespace nodes are not
    /// > automatically copied to the children.
    ///
    /// Every prefixed binding in scope at the parent is withheld — including
    /// those the parent itself inherited (review N5). The default namespace is
    /// `no_default`'s business, set when the element was opened. The set is
    /// the parent's [`PrefixedScope`], computed once per element and carried
    /// down from its own parent's (review N3, R1), so finding it costs time
    /// proportional to the bindings in scope, whatever the depth.
    ///
    /// **The element is a deep copy below the root of the copy** (§11.9.2).
    /// Its parent is a copy too, and is not "the newly constructed element"
    /// of §5.7.1's last rule — that is the element the copy is attached to,
    /// whose namespace nodes reach the copied elements through the tree. What
    /// the element must not get from its parent's copy is what the parent has
    /// of its own and the original element did not have:
    ///
    /// - the parent's fixup-produced nodes (`fixup_name`, `fixup_nodes`,
    ///   `owns_default`):
    ///   with `copy-namespaces="no"` "none of the namespace nodes are copied:
    ///   however, namespace nodes will still be created in the result tree as
    ///   required by the namespace fixup process" — for each element's own
    ///   names, and §5.7.3 adds that "the namespace fixup process does not add
    ///   a namespace node to an element merely because its parent node in the
    ///   result tree has such a namespace node" (review O1);
    /// - with `copy-namespaces="yes"` ("all the namespace nodes of the
    ///   original element are copied to the new element"), the parent's copied
    ///   nodes the original element lacks — a child of an element built with
    ///   `inherit-namespaces="no"`, which a copy must not hand those bindings
    ///   back to (review N1). The only way an element lacks a prefixed binding
    ///   its parent has is an undeclaration of its own (`undeclared`).
    ///
    /// An ordinary tree copied with `copy-namespaces="yes"` has neither, so
    /// nothing is withheld and the copy is what it always was.
    fn withheld_bindings(&mut self) -> (Vec<(String, String)>, bool) {
        let depth = self.stack.len();
        let parent_scope = if self.stack[depth - 1].parent_withholds {
            Some(self.prefixed_scope_at(depth - 2))
        } else {
            None
        };
        let (element, parent) = (&self.stack[depth - 1], &self.stack[depth - 2]);
        // A binding the element itself has with the same URI — one of its own
        // namespace nodes, or the one its name needs — would only be
        // re-declared by fixup and dropped again by the flush: it is left in
        // scope instead, which is the same thing without the round trip.
        // (Unless §11.7 is about to re-prefix the name, which happens only
        // when one of its own nodes rebinds the name's prefix; then the name
        // does not keep that prefix.)
        let name = element.name.as_ref();
        let renamed = name.is_some_and(|name| {
            element
                .namespaces
                .iter()
                .any(|(prefix, uri)| *prefix == name.prefix && *uri != name.uri)
        });
        let rebinds_identically = |prefix: &str, uri: &str| {
            element
                .namespaces
                .iter()
                .any(|(own, own_uri)| own == prefix && own_uri == uri)
                || (!renamed && name.is_some_and(|name| name.prefix == prefix && name.uri == uri))
        };
        if let Some(parent_scope) = parent_scope {
            let withheld = parent_scope
                .iter()
                .filter(|(prefix, uri)| !rebinds_identically(prefix, uri))
                .cloned()
                .collect();
            return (withheld, false);
        }
        if !(element.copied && parent.copied) {
            return (Vec::new(), false);
        }
        let mut withheld: Vec<(String, String)> = Vec::new();
        if parent.fixup_name {
            if let Some(own) = parent.name.as_ref() {
                if !rebinds_identically(&own.prefix, &own.uri) {
                    withheld.push((own.prefix.clone(), own.uri.clone()));
                }
            }
        }
        for (prefix, uri) in &parent.fixup_nodes {
            if !rebinds_identically(prefix, uri) {
                withheld.push((prefix.clone(), uri.clone()));
            }
        }
        for prefix in &element.undeclared {
            let parent_copied_it = parent.namespaces.iter().any(|(copied, _)| copied == prefix);
            if !parent_copied_it || withheld.iter().any(|(seen, _)| seen == prefix) {
                continue;
            }
            if let Some(uri) = self.fixup.in_scope(prefix).filter(|uri| !uri.is_empty()) {
                withheld.push((prefix.clone(), uri.to_string()));
            }
        }
        let default_withheld = parent.owns_default
            && !element
                .namespaces
                .iter()
                .any(|(prefix, _)| prefix.is_empty());
        (withheld, default_withheld)
    }

    /// The [`PrefixedScope`] of the flushed container `self.stack[index]`.
    ///
    /// Computed on demand and kept on the container: the walk goes up only to
    /// the nearest container that already knows its scope, and every one
    /// passed on the way is given its own on the way down — the parent's,
    /// shared, when it declares no prefix. Each container is computed at most
    /// once while it is open, so a chain of non-inheriting elements without
    /// declarations costs O(1) per element (review R1: re-reading every open
    /// ancestor's `declared` for each parent made it quadratic in the depth),
    /// and nothing is computed where no element says
    /// `inherit-namespaces="no"`.
    fn prefixed_scope_at(&mut self, index: usize) -> PrefixedScope {
        let mut known = index;
        while known > 0 && self.stack[known].prefixed_scope.is_none() {
            known -= 1;
        }
        let (mut scope, first) = match &self.stack[known].prefixed_scope {
            Some(scope) => (Arc::clone(scope), known + 1),
            // The document level, which declares nothing.
            None => (PrefixedScope::from(Vec::new()), known),
        };
        for open in &mut self.stack[first..=index] {
            scope = with_declarations(&scope, &open.declared);
            open.prefixed_scope = Some(Arc::clone(&scope));
        }
        scope
    }

    /// Records one attribute, last-wins on a duplicate expanded name.
    fn push_attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        if self.in_document_content() {
            return Err(XsltError::new(
                codes::XTDE0420,
                format!(
                    "attribute {:?} cannot be a child of a document node",
                    name.local
                ),
            ));
        }
        if self.stack.last().expect("checked").children_started {
            return Err(XsltError::new(
                codes::XTDE0410,
                format!(
                    "attribute {:?} follows a node that is neither an attribute nor a namespace",
                    name.local
                ),
            ));
        }
        let open = self.top();
        if let Some(slot) = open
            .attributes
            .iter_mut()
            .find(|(n, _, _)| n.local == name.local && n.uri == name.uri)
        {
            slot.1 = value.to_string();
            slot.2 = binding;
        } else {
            open.attributes
                .push((name.clone(), value.to_string(), binding));
        }
        open.last_atomic = false;
        Ok(())
    }

    /// Records one namespace node; duplicates collapse, conflicts are XTDE0430.
    ///
    /// A zero-length URI is a namespace **undeclaration**, which is *not* a
    /// namespace node: §5.7.1 knows only "a namespace node with no name",
    /// which maps the empty prefix to a namespace, and XTDE0430 speaks of
    /// nodes that "map the same prefix to different namespace URIs". So no
    /// such node is ever recorded. What an empty-prefix undeclaration *does*
    /// say is that the element has no namespace node with no name, which
    /// §5.7.1's inheritance rule needs to know — that is
    /// [`Open::no_default`], and namespace fixup (§5.7.3) turns it back into a
    /// written `xmlns=""` only where one is needed. A **prefixed**
    /// undeclaration (`xmlns:p=""`, XML 1.1 only) says nothing that XML 1.0
    /// output can express and is dropped.
    ///
    /// This is the one gate every producer passes through: literal result
    /// elements, `xsl:copy` / `xsl:copy-of` copying a source element's
    /// namespace nodes, the whitespace-stripping source copy, and
    /// `xsl:namespace` (whose own zero-length URI is XTDE0930, raised before
    /// it gets here).
    fn push_namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        if self.in_document_content() {
            return Err(XsltError::new(
                codes::XTDE0420,
                format!("namespace node {prefix:?} cannot be a child of a document node"),
            ));
        }
        if self.stack.last().expect("checked").children_started {
            return Err(XsltError::new(
                codes::XTDE0410,
                format!(
                    "namespace node {prefix:?} follows a node that is neither an attribute \
                     nor a namespace"
                ),
            ));
        }
        if uri.is_empty() {
            let open = self.top();
            if prefix.is_empty() && !open.namespaces.iter().any(|(p, _)| p.is_empty()) {
                open.no_default = true;
            }
            open.last_atomic = false;
            return Ok(());
        }
        if prefix.is_empty()
            && self
                .stack
                .last()
                .and_then(|open| open.name.as_ref())
                .is_some_and(|name| name.uri.is_empty())
        {
            return Err(XsltError::new(
                codes::XTDE0440,
                format!(
                    "a default namespace {uri:?} cannot be declared on an element that is in \
                     no namespace"
                ),
            ));
        }
        let open = self.top();
        if let Some((_, existing)) = open.namespaces.iter().find(|(p, _)| p == prefix) {
            if existing == uri {
                return Ok(());
            }
            return Err(XsltError::new(
                codes::XTDE0430,
                format!(
                    "two namespace nodes map the prefix {prefix:?} to {existing:?} and {uri:?}"
                ),
            ));
        }
        open.namespaces.push((prefix.to_string(), uri.to_string()));
        if prefix.is_empty() {
            open.no_default = false;
        }
        open.last_atomic = false;
        Ok(())
    }

    /// Deep-copies an existing node into the event stream (§5.7.1's "each node
    /// … is attached … Conceptually this involves making a deep copy").
    ///
    /// Upstream `document::copy` implements the same rules, but it writes
    /// straight into a [`BufferDocumentBuilder`]: it cannot feed a serializer,
    /// and invariant 2 needs both destinations to see the *same* stream. The
    /// walk below is therefore the normalizer's own; its namespace handling is
    /// still upstream's ([`NamespaceFixup`]), applied at flush time.
    pub(crate) fn copy_node(
        &mut self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        self.copy_subtree(nav, namespaces)
    }

    /// The recursive half of [`copy_node`](Self::copy_node).
    ///
    /// Every element it opens is marked `copied`, so that the flush gives it
    /// exactly the namespace nodes §11.9.2 does (`withheld_bindings` has the
    /// rules): with `copy-namespaces="yes"` "all the namespace nodes of the
    /// original element are copied to the new element" — and no binding the
    /// original lacks comes back from the copy of its parent — and with `no`
    /// "none of the namespace nodes are copied", only what fixup produces for
    /// each element's own names. On top of that, §5.7.1's last rule hands the
    /// copy the namespace nodes of the element it is attached to:
    ///
    /// > If the newly constructed node is an element node, and if namespaces
    /// > are inherited, then each namespace node of the newly constructed
    /// > element (including any produced as a result of the namespace fixup
    /// > process) is copied to each descendant element of the newly
    /// > constructed element, unless that element **or an intermediate
    /// > element** already has a namespace node with the same name (or absence
    /// > of a name) …
    ///
    /// The copy's own elements are not newly constructed, so nothing
    /// propagates *from* them; what propagates is the namespace nodes of the
    /// element (or nothing, for a document node) the copy is being attached
    /// to — through the tree, which already stops at an element that has a
    /// node of its own with the same name.
    ///
    /// For the default namespace this is the whole difference between
    /// `namespace-3001` (a deep copy attached to a document node: the inner
    /// `xmlns=""` survives, because its copied parent has a default namespace
    /// of its own) and `namespace-3004` (the same deep copy attached to a
    /// literal result element with a default namespace: the copy root has none
    /// of its own and inherits that one, and the undeclaration disappears).
    fn copy_subtree(&mut self, nav: &Nav<'t>, namespaces: CopyNamespaces) -> Result<(), XsltError> {
        match nav.node_type() {
            DomNodeType::Root => {
                let mut child = nav.clone();
                if child.move_to_first_child() {
                    loop {
                        self.copy_subtree(&child, namespaces)?;
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
                Ok(())
            }
            DomNodeType::Element => {
                let binding = self.binding_of(nav);
                let nilled = self.annotations == Annotations::Preserve
                    && matches!(nav.typed_value(), TypedValue::Nilled);
                self.start_element(&Name::of(nav), binding, nilled)?;
                self.top().copied = true;
                // §11.9.2: with `copy-namespaces="no"` none of the namespace
                // nodes are copied and fixup supplies what the names need.
                if let Some(scope) = namespaces.scope() {
                    let (nodes, undeclared) = namespace_nodes_and_undeclarations_of(nav, scope);
                    for (prefix, uri) in &nodes {
                        self.namespace(prefix, uri)?;
                    }
                    self.top().undeclared = undeclared;
                }
                let mut attr = nav.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        self.copy_attribute(&attr)?;
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
                self.end_start_tag()?;
                let mut child = nav.clone();
                if child.move_to_first_child() {
                    loop {
                        self.copy_subtree(&child, namespaces)?;
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
                self.end_element()
            }
            DomNodeType::Attribute => self.copy_attribute(nav),
            DomNodeType::Namespace => {
                let (prefix, uri) = (nav.local_name().to_string(), nav.value());
                self.namespace(&prefix, &uri)
            }
            DomNodeType::Comment => self.comment(&nav.value()),
            DomNodeType::ProcessingInstruction => {
                let (target, data) = (nav.local_name().to_string(), nav.value());
                self.pi(&target, &data)
            }
            _ => self.text(&nav.value(), false),
        }
    }

    fn copy_attribute(&mut self, nav: &Nav<'t>) -> Result<(), XsltError> {
        let binding = self.binding_of(nav);
        let value = nav.value();
        self.attribute(&Name::of(nav), &value, binding)
    }

    fn binding_of(&self, nav: &Nav<'t>) -> Option<NodeSchemaBinding> {
        match self.annotations {
            Annotations::Strip => None,
            Annotations::Preserve => CopySource::schema_binding(nav),
        }
    }
}

impl<'t, R: Receiver<'t>> Receiver<'t> for ComplexContentNormalizer<'t, R> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        self.document_marks.push(self.stack.len());
        self.out.start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.flush_text()?;
        self.document_marks.pop();
        self.out.end_document()
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        self.flush_text()?;
        self.flush_start_tag()?;
        let parent = self.top();
        parent.children_started = true;
        parent.last_atomic = false;
        // §5.7.1: with `inherit-namespaces="no"` on the parent's instruction
        // this element gets none of the parent's namespace nodes, so it starts
        // out with no namespace node with no name — a default namespace the
        // parent declared has to be undeclared here, which a default namespace
        // of this element's own clears again — and with none of the parent's
        // prefixed ones either (`parent_withholds`, applied at flush time).
        // Only the default-namespace half shows in XML 1.0 output (§5.7.3's
        // note); the prefixed half shows on the `namespace` axis of the tree.
        let parent_withholds = !parent.inherit;
        self.stack.push(Open {
            name: Some(name.clone()),
            binding,
            nilled,
            no_default: parent_withholds,
            parent_withholds,
            ..Open::inheriting()
        });
        Ok(())
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.flush_text()?;
        self.push_namespace(prefix, uri)
    }

    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        self.top().inherit = false;
        Ok(())
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.flush_text()?;
        self.push_attribute(name, value, binding)
    }

    /// A no-op: the normalizer decides where the start tag ends, because an
    /// attribute may still arrive through [`append_item`](Receiver::append_item)
    /// after the instruction that would have closed it.
    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        if s.is_empty() {
            return Ok(());
        }
        if disable_escaping && self.honour_disabled_escaping {
            let start = self.text.len();
            match self.disabled.last_mut() {
                Some(last) if last.1 == start => last.1 = start + s.len(),
                _ => self.disabled.push((start, start + s.len())),
            }
        }
        self.text.push_str(s);
        self.text_pending = true;
        self.top().last_atomic = false;
        Ok(())
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.flush_text()?;
        self.flush_start_tag()?;
        let top = self.top();
        top.children_started = true;
        top.last_atomic = false;
        self.out.comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.flush_text()?;
        self.flush_start_tag()?;
        let top = self.top();
        top.children_started = true;
        top.last_atomic = false;
        self.out.pi(target, data)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.flush_text()?;
        self.flush_start_tag()?;
        self.stack.pop();
        self.fixup.pop_scope();
        let top = self.top();
        top.children_started = true;
        top.last_atomic = false;
        self.out.end_element()
    }

    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(value) => {
                let text = value.to_string_value();
                // "a single space (#x20) used as a separator between successive
                // strings" — but only between two *atomic* items.
                if self.top().last_atomic {
                    self.text.push(' ');
                    self.text_pending = true;
                }
                if !text.is_empty() {
                    self.text.push_str(&text);
                    self.text_pending = true;
                }
                self.top().last_atomic = true;
                Ok(())
            }
            XmlItem::Node(nav) => {
                self.copy_node(nav, self.copy_namespaces)?;
                self.top().last_atomic = false;
                Ok(())
            }
        }
    }

    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(_) => self.append_item(item),
            XmlItem::Node(nav) => {
                self.copy_node(nav, namespaces)?;
                self.top().last_atomic = false;
                Ok(())
            }
        }
    }
}

// ── ResultBuilder ────────────────────────────────────────────────────────

/// Adapter from normalized events to [`BufferDocumentBuilder`] (§9.3).
///
/// Everything hard happened in the normalizer, so this only buffers the start
/// tag — the builder wants the namespace declarations as an argument of
/// `start_element` — and replays `set_node_binding`/`set_nil`.
pub struct ResultBuilder<'t> {
    builder: BufferDocumentBuilder<'t>,
    pending: Option<(Name, Option<NodeSchemaBinding>, bool)>,
    decls: Vec<(String, String)>,
    attrs: Vec<(Name, String, Option<NodeSchemaBinding>)>,
    open: Vec<u32>,
    /// The `disable-output-escaping` property of the tree (§20.2), by text
    /// node ordinal; see [`crate::serialize::EscapingMarks`].
    marks: crate::serialize::EscapingMarks,
    /// Text nodes started so far: the builder merges consecutive `text` calls
    /// into one node, so a node starts at a `text` call that follows any other
    /// event.
    text_nodes: u32,
    /// The byte length of the text node being built, while the last event was
    /// `text`.
    text_len: Option<usize>,
}

impl<'t> ResultBuilder<'t> {
    /// A builder for a `DocumentKind::Full` result tree.
    pub fn new(
        arena: &'t bumpalo::Bump,
        names: &'t xsd_schema::namespace::NameTable,
        schema_set: Option<&'t SchemaSet>,
    ) -> Result<Self, XsltError> {
        Self::with_options(arena, names, schema_set, BufferDocumentOptions::default())
    }

    /// A builder with explicit [`BufferDocumentOptions`] — `Fragment` for the
    /// hidden documents [`SequenceCollector`] builds.
    pub fn with_options(
        arena: &'t bumpalo::Bump,
        names: &'t xsd_schema::namespace::NameTable,
        schema_set: Option<&'t SchemaSet>,
        options: BufferDocumentOptions,
    ) -> Result<Self, XsltError> {
        let builder = BufferDocumentBuilder::new(arena, names, schema_set, options)
            .map_err(|e| XsltError::internal(format!("could not start a result tree: {e}")))?;
        Ok(Self {
            builder,
            pending: None,
            decls: Vec::new(),
            attrs: Vec::new(),
            open: Vec::new(),
            marks: crate::serialize::EscapingMarks::default(),
            text_nodes: 0,
            text_len: None,
        })
    }

    /// The finished document.
    pub fn finish(self) -> Result<BufferDocument<'t>, XsltError> {
        self.finish_with_marks().map(|(doc, _)| doc)
    }

    /// The finished document and its `disable-output-escaping` marks (§20.2).
    pub fn finish_with_marks(
        mut self,
    ) -> Result<(BufferDocument<'t>, crate::serialize::EscapingMarks), XsltError> {
        self.flush()?;
        let doc = self
            .builder
            .finalize()
            .map_err(|e| XsltError::internal(format!("could not finish a result tree: {e}")))?;
        Ok((doc, self.marks))
    }

    /// The node index of the element opened last, for the caller that needs to
    /// point at it afterwards.
    pub fn last_opened(&self) -> Option<u32> {
        self.open.last().copied()
    }

    fn flush(&mut self) -> Result<(), XsltError> {
        let Some((name, binding, nilled)) = self.pending.take() else {
            return Ok(());
        };
        self.text_len = None;
        let decls: Vec<(&str, &str)> = self
            .decls
            .iter()
            .map(|(p, u)| (p.as_str(), u.as_str()))
            .collect();
        let elem = self
            .builder
            .start_element(&name.local, &name.uri, &name.prefix, &decls)
            .map_err(build_error)?;
        self.decls.clear();
        for (attr, value, attr_binding) in std::mem::take(&mut self.attrs) {
            let attr_ref = self
                .builder
                .attribute(&attr.local, &attr.uri, &attr.prefix, &value)
                .map_err(build_error)?;
            if let Some(binding) = attr_binding {
                self.builder
                    .set_node_binding(attr_ref, binding)
                    .map_err(build_error)?;
            }
        }
        self.builder.end_of_attributes();
        if let Some(binding) = binding {
            self.builder
                .set_node_binding(elem, binding)
                .map_err(build_error)?;
        }
        if nilled {
            self.builder.set_nil(elem);
        }
        self.open.push(elem);
        Ok(())
    }
}

fn build_error(e: xsd_schema::document::BufferDocumentError) -> XsltError {
    XsltError::internal(format!("the result-tree builder refused: {e}"))
}

impl<'t> Receiver<'t> for ResultBuilder<'t> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.text_len = None;
        self.flush()
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        self.flush()?;
        self.text_len = None;
        self.pending = Some((name.clone(), binding, nilled));
        Ok(())
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.decls.push((prefix.to_string(), uri.to_string()));
        Ok(())
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.attrs.push((name.clone(), value.to_string(), binding));
        Ok(())
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.flush()
    }

    /// Character data; with `disable_escaping`, the characters carry the
    /// disable-escaping property (§20.2) — only a pipeline built with
    /// [`ComplexContentNormalizer::honouring_disabled_escaping`] sends it.
    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        if s.is_empty() {
            return Ok(());
        }
        self.flush()?;
        let start = match self.text_len {
            Some(len) => len,
            None => {
                self.text_nodes += 1;
                0
            }
        };
        if disable_escaping {
            self.marks.push(self.text_nodes - 1, start, start + s.len());
        }
        self.text_len = Some(start + s.len());
        self.builder.text(s);
        Ok(())
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.flush()?;
        self.text_len = None;
        self.builder.comment(s).map_err(build_error)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.flush()?;
        self.text_len = None;
        self.builder
            .processing_instruction(target, data)
            .map_err(build_error)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.flush()?;
        self.text_len = None;
        self.open.pop();
        self.builder.end_element().map_err(build_error)
    }

    /// Unreachable below a [`ComplexContentNormalizer`], which decomposes every
    /// item into events before it reaches a tree builder.
    fn append_item(&mut self, _item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        Err(XsltError::internal(
            "ResultBuilder must sit behind a ComplexContentNormalizer",
        ))
    }
}

// ── EventSerializer ──────────────────────────────────────────────────────

/// Writes the normalized event stream straight out as XML.
///
/// The output is byte-identical to upstream
/// [`serialize::to_string`] in compact mode over the equivalent tree, which is
/// what architecture invariant 2 asserts. Two details of upstream's format are
/// reproduced deliberately: declarations are written **before** the attributes
/// and **sorted by prefix** (`serialize.rs:847`, which sorts because "the
/// `namespace::` axis has no defined order"), and `>` is always escaped in text
/// (`serialize.rs:961`). A declaration is written only where it changes the
/// scope written so far, and a prefixed undeclaration (`xmlns:p=""`, which a
/// child of an `inherit-namespaces="no"` element carries in the tree) never is
/// — upstream's `write_namespace_declarations` follows the same two rules.
#[derive(Default)]
pub struct EventSerializer {
    out: String,
    pending: Option<Name>,
    decls: Vec<(String, String)>,
    attrs: Vec<(Name, String)>,
    open: Vec<Name>,
    tag_open: bool,
    /// The prefixes of the declarations written so far, innermost last — what
    /// `end_element` takes back off `bound`.
    written: Vec<String>,
    /// For each prefix, the URIs its written declarations bind it to,
    /// innermost last, so that asking what a prefix is bound to costs one
    /// lookup however many declarations are in scope.
    bound: HashMap<String, Vec<String>>,
    /// Where each open element's declarations start in `written`.
    marks: Vec<usize>,
}

impl EventSerializer {
    /// An empty serializer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The XML written so far.
    pub fn into_string(self) -> String {
        self.out
    }

    /// Whether `prefix` is bound to `uri` by what has been written so far; the
    /// empty prefix is bound to no namespace until something else is written.
    fn is_written(&self, prefix: &str, uri: &str) -> bool {
        match self.bound.get(prefix).and_then(|uris| uris.last()) {
            Some(bound) => bound == uri,
            None => prefix.is_empty() && uri.is_empty(),
        }
    }

    fn close_tag(&mut self, self_closing: bool) {
        if self.tag_open {
            self.out.push_str(if self_closing { "/>" } else { ">" });
            self.tag_open = false;
        }
    }

    fn flush(&mut self) {
        let Some(name) = self.pending.take() else {
            return;
        };
        self.close_tag(false);
        self.out.push('<');
        push_qname(&mut self.out, &name.prefix, &name.local);
        self.decls.sort_by(|(a, _), (b, _)| a.cmp(b));
        self.marks.push(self.written.len());
        for (prefix, uri) in std::mem::take(&mut self.decls) {
            if (!prefix.is_empty() && uri.is_empty()) || self.is_written(&prefix, &uri) {
                continue;
            }
            if prefix.is_empty() {
                self.out.push_str(" xmlns=\"");
            } else {
                self.out.push_str(" xmlns:");
                self.out.push_str(&prefix);
                self.out.push_str("=\"");
            }
            push_attribute_value(&mut self.out, &uri);
            self.out.push('"');
            self.bound.entry(prefix.clone()).or_default().push(uri);
            self.written.push(prefix);
        }
        for (attr, value) in std::mem::take(&mut self.attrs) {
            self.out.push(' ');
            push_qname(&mut self.out, &attr.prefix, &attr.local);
            self.out.push_str("=\"");
            push_attribute_value(&mut self.out, &value);
            self.out.push('"');
        }
        self.open.push(name);
        self.tag_open = true;
    }
}

impl<'t> Receiver<'t> for EventSerializer {
    fn start_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.flush();
        self.close_tag(true);
        Ok(())
    }

    fn start_element(
        &mut self,
        name: &Name,
        _binding: Option<NodeSchemaBinding>,
        _nilled: bool,
    ) -> Result<(), XsltError> {
        self.flush();
        self.pending = Some(name.clone());
        Ok(())
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.decls.push((prefix.to_string(), uri.to_string()));
        Ok(())
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        _binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.attrs.push((name.clone(), value.to_string()));
        Ok(())
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.flush();
        Ok(())
    }

    fn text(&mut self, s: &str, _disable_escaping: bool) -> Result<(), XsltError> {
        self.flush();
        self.close_tag(false);
        push_text(&mut self.out, s);
        Ok(())
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.flush();
        self.close_tag(false);
        self.out.push_str("<!--");
        self.out.push_str(s);
        self.out.push_str("-->");
        Ok(())
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.flush();
        self.close_tag(false);
        self.out.push_str("<?");
        self.out.push_str(target);
        if !data.is_empty() {
            self.out.push(' ');
            self.out.push_str(data);
        }
        self.out.push_str("?>");
        Ok(())
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.flush();
        if let Some(mark) = self.marks.pop() {
            for prefix in self.written.drain(mark..) {
                if let Some(uris) = self.bound.get_mut(&prefix) {
                    uris.pop();
                }
            }
        }
        let name = self.open.pop().unwrap_or_default();
        if self.tag_open {
            // Upstream always collapses an empty element to `<a/>`.
            self.close_tag(true);
        } else {
            self.out.push_str("</");
            push_qname(&mut self.out, &name.prefix, &name.local);
            self.out.push('>');
        }
        Ok(())
    }

    fn append_item(&mut self, _item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        Err(XsltError::internal(
            "EventSerializer must sit behind a ComplexContentNormalizer",
        ))
    }
}

fn push_qname(out: &mut String, prefix: &str, local: &str) {
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push(':');
    }
    out.push_str(local);
}

/// Text escaping, matching `document::serialize::write_text`.
fn push_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

/// Attribute-value escaping, matching `document::serialize::write_attribute_value`.
fn push_attribute_value(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

/// [`serialize::to_string`] of a whole document, as the invariant-2 comparison
/// uses it.
pub fn serialize_document(doc: &BufferDocument<'_>) -> Result<String, XsltError> {
    serialize::to_string(&doc.create_navigator(), &Default::default())
        .map_err(|e| XsltError::internal(format!("could not serialize: {e}")))
}

// ── SequenceCollector ────────────────────────────────────────────────────

/// Collects the result of a sequence constructor evaluated with `as=` (§6.3).
///
/// [`append_item`](Receiver::append_item) keeps an existing node **by
/// reference**, so its identity, parent, siblings and type annotation survive.
/// A raw `start_element` … `end_element` group is diverted into a hidden
/// document registered in the [`DocumentStore`], and the element it produced is
/// appended as a parentless node.
pub struct SequenceCollector<'s, 't> {
    store: &'s RefCell<DocumentStore<'t>>,
    items: Vec<XmlItem<Nav<'t>>>,
    /// The nested pipeline building the current construction group.
    nested: Option<ComplexContentNormalizer<'t, ResultBuilder<'t>>>,
    /// What the open group will produce.
    group: GroupKind,
    /// The base URI the open `GroupKind::Document` group takes (§11.5).
    base_uri: Option<String>,
    /// How many containers (elements and document nodes) are open.
    depth: usize,
}

/// What a construction group in a sequence produces (§6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
    /// A parentless element.
    Element,
    /// A document node, from `xsl:document` or a temporary tree.
    Document,
}

impl<'s, 't> SequenceCollector<'s, 't> {
    /// A collector whose hidden documents live in `store`.
    ///
    /// The store is borrowed for `'s` while the nodes it yields live for `'t`,
    /// and the two cannot be the same lifetime: `&'t RefCell<DocumentStore<'t>>`
    /// does not borrow-check, because the borrow would have to outlive the value
    /// that carries `'t` in its own type. The navigators the store hands out are
    /// `'t` however long the store itself is borrowed, which is what makes the
    /// shorter borrow enough. (The M0b prototype's `ProtoFunctions::evaluator`
    /// carried this note; it was replaced in M1.)
    pub fn new(store: &'s RefCell<DocumentStore<'t>>) -> Self {
        Self {
            store,
            items: Vec::new(),
            nested: None,
            group: GroupKind::Element,
            base_uri: None,
            depth: 0,
        }
    }

    /// The items collected so far.
    pub fn items(&self) -> &[XmlItem<Nav<'t>>] {
        &self.items
    }

    /// The collected sequence.
    pub fn into_items(self) -> Vec<XmlItem<Nav<'t>>> {
        self.items
    }

    fn begin_group(&mut self, group: GroupKind) -> Result<(), XsltError> {
        let builder = self.new_builder(group)?;
        self.nested = Some(ComplexContentNormalizer::new(builder));
        self.group = group;
        self.base_uri = None;
        Ok(())
    }

    fn new_builder(&self, group: GroupKind) -> Result<ResultBuilder<'t>, XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let options = match group {
            // A parentless node is built in a hidden document whose root the
            // navigator then hides (`BufferDocNavigator::new_orphan`).
            GroupKind::Element => BufferDocumentOptions::fragment(),
            // A document node is a real document node, with its own identity.
            GroupKind::Document => BufferDocumentOptions::default(),
        };
        ResultBuilder::with_options(arena, names, None, options)
    }

    fn end_group(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.take() else {
            return Ok(());
        };
        let group = self.group;
        let mut doc = nested.into_inner().finish()?;
        // §11.5: the document node `xsl:document` builds takes the base URI of
        // the instruction, exactly as §9.4's temporary tree takes that of the
        // variable binding element.
        if let Some(base) = self.base_uri.take() {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(&base)));
        }
        let id = self.store.borrow_mut().insert(doc, None);
        let root = self.store.borrow().navigator(id);
        let item = match group {
            // A document node keeps its own root and its own identity.
            GroupKind::Document => root,
            GroupKind::Element => {
                let mut elem = root.clone();
                if elem.move_to_first_child() {
                    orphan(self.store, id, elem.current_ref())
                } else {
                    root
                }
            }
        };
        self.items.push(XmlItem::Node(item));
        Ok(())
    }

    /// Builds a hidden document holding one parentless node and pushes it.
    ///
    /// §6.3: a newly constructed node that lands in a *sequence* rather than
    /// in element content is parentless. An attribute, comment, processing
    /// instruction or text node has no way to exist in a `BufferDocument`
    /// without a parent, so one is built around it and the navigator hides it.
    fn push_parentless(
        &mut self,
        build: impl FnOnce(
            &mut ComplexContentNormalizer<'t, ResultBuilder<'t>>,
        ) -> Result<(), XsltError>,
        locate: Locate,
    ) -> Result<(), XsltError> {
        let builder = self.new_builder(GroupKind::Element)?;
        let mut pipeline = ComplexContentNormalizer::new(builder);
        build(&mut pipeline)?;
        self.push_holder(pipeline.into_inner().finish()?, locate)
    }

    /// A parentless namespace node (§11.7, §6.3).
    ///
    /// Unlike every other node kind this one does **not** go through the
    /// normalizer, and deliberately so: the holder element is an implementation
    /// artefact, not a constructed element, so §5.7.1's rules about the content
    /// of an element — in particular XTDE0440, "it is an error to define a
    /// default namespace when the element is in no namespace" — have nothing to
    /// say about it. Routing `<xsl:namespace name="">` through the normalizer
    /// raised XTDE0440 against the holder (`namespace-3005`, whose point is that
    /// `deep-equal` compares two unnamed namespace nodes).
    fn push_namespace_node(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        let mut builder = self.new_builder(GroupKind::Element)?;
        builder.start_element(&Name::local(HOLDER), None, false)?;
        builder.namespace(prefix, uri)?;
        builder.end_start_tag()?;
        builder.end_element()?;
        self.push_holder(builder.finish()?, Locate::Namespace)
    }

    /// Finds the node a holder document was built around and appends it.
    fn push_holder(&mut self, doc: BufferDocument<'t>, locate: Locate) -> Result<(), XsltError> {
        let id = self.store.borrow_mut().insert(doc, None);
        let root = self.store.borrow().navigator(id);
        let mut cursor = root.clone();
        // The holder element the node was built inside.
        if !cursor.move_to_first_child() {
            return Err(XsltError::internal("a parentless node was not built"));
        }
        let holder = cursor.current_ref();
        let item = match locate {
            // A namespace node has no node index of its own — the cursor sits
            // on the element that owns it — so the *holder* is the orphan and
            // the copy keeps it as its parent.
            Locate::Namespace => {
                let mut nav = orphan(self.store, id, holder);
                if !nav.move_to_first_namespace(NamespaceAxisScope::Local) {
                    return Err(XsltError::internal("a parentless node was not built"));
                }
                nav
            }
            // Everything else has an index, and *it* becomes the orphan root,
            // so the holder is invisible: the node has no parent at all.
            Locate::Attribute | Locate::Child => {
                let found = match locate {
                    Locate::Attribute => cursor.move_to_first_attribute(),
                    _ => cursor.move_to_first_child(),
                };
                if !found {
                    return Err(XsltError::internal("a parentless node was not built"));
                }
                orphan(self.store, id, cursor.current_ref())
            }
        };
        self.items.push(XmlItem::Node(item));
        Ok(())
    }

    /// Appends a **new** node that is a deep copy of `nav` (§11.9.1, §11.9.2).
    ///
    /// The copy is built in its own hidden document, so it is parentless and
    /// is not the node it was copied from — which is the whole difference
    /// between `xsl:copy-of` and `xsl:sequence` when the result lands in a
    /// sequence rather than in element content.
    fn push_copy(&mut self, nav: &Nav<'t>, namespaces: CopyNamespaces) -> Result<(), XsltError> {
        match nav.node_type() {
            // A document node copies to a new document node, with its own
            // identity: "the instruction adds a new document node to the
            // result sequence".
            DomNodeType::Root => {
                let builder = self.new_builder(GroupKind::Document)?;
                let mut pipeline = ComplexContentNormalizer::new(builder);
                pipeline.start_document()?;
                pipeline.copy_node(nav, namespaces)?;
                pipeline.end_document()?;
                let doc = pipeline.into_inner().finish()?;
                let id = self.store.borrow_mut().insert(doc, None);
                let root = self.store.borrow().navigator(id);
                self.items.push(XmlItem::Node(root));
                Ok(())
            }
            // A namespace node's copy is a new node with the same name and
            // value, which is what its identity turns on, and the holder is
            // built without the normalizer for the reason given there.
            DomNodeType::Namespace => {
                let (prefix, uri) = (nav.local_name().to_string(), nav.value());
                self.push_namespace_node(&prefix, &uri)
            }
            kind => {
                let locate = match kind {
                    DomNodeType::Attribute => Locate::Attribute,
                    _ => Locate::Child,
                };
                let source = nav.clone();
                self.push_parentless(
                    move |out| {
                        out.start_element(&Name::local(HOLDER), None, false)?;
                        out.copy_node(&source, namespaces)?;
                        out.end_start_tag()?;
                        out.end_element()
                    },
                    locate,
                )
            }
        }
    }
}

/// Where in the hidden document the parentless node sits.
///
/// Everything is built inside a holder element, because a
/// `BufferDocumentBuilder` only accepts text, comments and processing
/// instructions inside one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Locate {
    /// The holder's first child.
    Child,
    /// The holder's first attribute.
    Attribute,
    /// The holder's first namespace node.
    ///
    /// Unlike the others this one is **not** parentless: the XDM lets a
    /// namespace node stand alone, a `BufferDocument` does not, and the
    /// navigator has no cursor for a namespace node that is not attached to an
    /// element. The copy is a new node with the right name and value — which
    /// is what identity turns on — and its parent is the holder.
    Namespace,
}

/// The element every parentless node is built inside.
const HOLDER: &str = "parentless-node-holder";

/// A navigator on a node of a hidden document that must appear parentless.
fn orphan<'t>(store: &RefCell<DocumentStore<'t>>, id: DocId, node: u32) -> Nav<'t> {
    let doc = store.borrow().get(id).expect("just inserted").doc;
    debug_assert_eq!(doc.kind(), DocumentKind::Fragment);
    BufferDocNavigator::new_orphan(doc, node)
}

impl<'t> Receiver<'t> for SequenceCollector<'_, 't> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.begin_group(GroupKind::Document)?;
        }
        self.depth += 1;
        match self.nested.as_mut() {
            Some(nested) => nested.start_document(),
            None => Ok(()),
        }
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        if self.depth == 0 {
            return Ok(());
        }
        if let Some(nested) = self.nested.as_mut() {
            nested.end_document()?;
        }
        self.depth -= 1;
        if self.depth == 0 {
            self.end_group()?;
        }
        Ok(())
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.begin_group(GroupKind::Element)?;
        }
        self.depth += 1;
        self.nested
            .as_mut()
            .expect("group open")
            .start_element(name, binding, nilled)
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            Some(nested) => nested.namespace(prefix, uri),
            // §6.3 and §11.7: a namespace node may stand on its own in a
            // sequence. `BufferDocument` cannot hold one without a parent, so
            // it gets the synthetic holder — see `Locate::Namespace`.
            None => self.push_namespace_node(prefix, uri),
        }
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        if let Some(nested) = self.nested.as_mut() {
            return nested.attribute(name, value, binding);
        }
        let (name, value) = (name.clone(), value.to_string());
        self.push_parentless(
            move |out| {
                out.start_element(&Name::local(HOLDER), None, false)?;
                out.attribute(&name, &value, binding)?;
                out.end_start_tag()?;
                out.end_element()
            },
            Locate::Attribute,
        )
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            Some(nested) => nested.end_start_tag(),
            None => Ok(()),
        }
    }

    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            Some(nested) => nested.no_inherit_namespaces(),
            None => Ok(()),
        }
    }

    fn document_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        // Only the outermost group is a document node of its own; an inner
        // `xsl:document` contributes its children to the element around it.
        if self.depth == 1 && self.group == GroupKind::Document && !uri.is_empty() {
            self.base_uri = Some(uri.to_string());
        }
        Ok(())
    }

    fn element_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        // Only the outermost element of a group is parentless; `end_group`
        // records the URI on the hidden document it is built in, which is
        // where the navigator looks once no `xml:base` is found.
        if self.depth == 1 && self.group == GroupKind::Element && !uri.is_empty() {
            self.base_uri = Some(uri.to_string());
        }
        Ok(())
    }

    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        if let Some(nested) = self.nested.as_mut() {
            return nested.text(s, disable_escaping);
        }
        if s.is_empty() {
            // §5.7.1: a zero-length text node is dropped.
            return Ok(());
        }
        let s = s.to_string();
        self.push_parentless(
            move |out| {
                out.start_element(&Name::local(HOLDER), None, false)?;
                out.text(&s, disable_escaping)?;
                out.end_element()
            },
            Locate::Child,
        )
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        if let Some(nested) = self.nested.as_mut() {
            return nested.comment(s);
        }
        let s = s.to_string();
        self.push_parentless(
            move |out| {
                out.start_element(&Name::local(HOLDER), None, false)?;
                out.comment(&s)?;
                out.end_element()
            },
            Locate::Child,
        )
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        if let Some(nested) = self.nested.as_mut() {
            return nested.pi(target, data);
        }
        let (target, data) = (target.to_string(), data.to_string());
        self.push_parentless(
            move |out| {
                out.start_element(&Name::local(HOLDER), None, false)?;
                out.pi(&target, &data)?;
                out.end_element()
            },
            Locate::Child,
        )
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.as_mut() else {
            return Err(XsltError::internal("end_element with no element open"));
        };
        nested.end_element()?;
        self.depth -= 1;
        if self.depth == 0 {
            self.end_group()?;
        }
        Ok(())
    }

    /// Keeps the item **unchanged**: a node stays the very node it was, which
    /// is what `xsl:sequence select="//item"` inside a typed variable means.
    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            // Inside a construction group the item is content, so the
            // normalizer's copy rules apply.
            Some(nested) => nested.append_item(item),
            None => {
                self.items.push(item.clone());
                Ok(())
            }
        }
    }

    /// §11.9.2: a copy landing directly in the sequence is a **new**,
    /// parentless node, not the node it was copied from.
    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        match (self.nested.as_mut(), item) {
            // Inside a construction group the copy is content, so it is
            // decomposed into events like anything else.
            (Some(nested), _) => nested.append_copy(item, namespaces),
            // "If the item is an atomic value, the value is appended to the
            // result sequence, as with xsl:sequence."
            (None, XmlItem::Atomic(_)) => {
                self.items.push(item.clone());
                Ok(())
            }
            (None, XmlItem::Node(nav)) => self.push_copy(nav, namespaces),
        }
    }
}
