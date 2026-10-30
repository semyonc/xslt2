//! The push pipeline: raw events → [`ComplexContentNormalizer`] → one
//! destination.
//!
//! The instruction evaluator emits **raw** events in
//! evaluation order; all the rules of XSLT 2.0 §5.7.1 live in one component,
//! the normalizer, which wraps every tree-shaped destination. Because
//! [`ResultBuilder`] and the test-only `EventSerializer` consume the *same*
//! normalized stream, materialise-then-serialize and serialize-directly cannot
//! disagree — the pipeline's invariant the unit tests check.
//!
//! [`SequenceCollector`] is the other destination: the `as=` cases, where an
//! existing node keeps its identity instead of being copied. The
//! distinction is drawn by *which* trait method an instruction calls —
//! [`Receiver::append_item`] appends the item itself (`xsl:sequence`, §11.10)
//! and [`Receiver::append_copy`] appends a new node that is a deep copy of it
//! (`xsl:copy`, `xsl:copy-of`, §11.9) — never by the receiver guessing.

use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use xsd_schema::document::{
    serialize, BufferDocNavigator, BufferDocument, BufferDocumentBuilder, BufferDocumentError,
    BufferDocumentOptions, CopySource, DocumentKind, NamespaceFixup, NodeSchemaBinding,
};
use xsd_schema::ids::TypeKey;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope, TypedValue};
use xsd_schema::parser::frames::SimpleTypeVariety;
use xsd_schema::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use xsd_schema::types::XmlTypeCode;
use xsd_schema::validation::{validate_simple_type, ContentType, SchemaValidator};
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSet;

use crate::error::{codes, XsltError};
use crate::runtime::validate::Validating;
use crate::store::{DocId, DocumentStore};

/// A node of a transformation's documents: `xsd-schema`'s navigator over its
/// in-memory document, positioned on one node.
///
/// A host meets it where nodes cross the API: a node-valued stylesheet
/// parameter is an [`XPathValue`](crate::XPathValue) of `Nav`s
/// (`XPathValue::from_node`), and [`DocumentStore::navigator`] gives one on a
/// document node. `'t` is the lifetime of the transformation's arena.
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
/// deterministic. `p` becomes `p_0`, `p_1`, … , which is what the W3C XSLT test
/// suite's reference results were produced with (`namespace-2614`, `namespace-2001`,
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

/// What a copy carries of the PSVI of the nodes it copies — the private
/// counterpart of [`Annotations`] for [`ComplexContentNormalizer::copy_subtree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    /// No binding at all: a basic processor's copies, and a copy whose root a
    /// `strict` validation re-annotates (§19.2.1.1: the validation, not the
    /// source, decides).
    Nothing,
    /// No binding at all, but the is-id and is-idrefs properties of each
    /// node, passed on with [`Receiver::element_id`] (the id of an attribute
    /// that is an `xs:ID` in its source, and of an element that is one by its
    /// own typed value) and [`Receiver::attribute_idrefs`] /
    /// [`Receiver::element_idrefs`]: a `strip` copy loses its annotations, but
    /// "The values of the is-id and is-idrefs properties are unchanged"
    /// (§19.2.1.1). An element's nilled property is not read (it is set to
    /// false, and asking a typed element costs a typed-value computation).
    Ids,
    /// Every binding and the nilled property — and the source of each
    /// element and attribute ([`Receiver::element_copied_from`],
    /// [`Receiver::attribute_copied_from`]), so that a `strip` boundary
    /// downstream that takes the binding away
    /// (`runtime::validate::Validating`) keeps the is-id and is-idrefs the
    /// source has, reading them only then.
    All,
}

impl From<Annotations> for Carry {
    fn from(annotations: Annotations) -> Self {
        match annotations {
            Annotations::Strip => Carry::Nothing,
            Annotations::Preserve => Carry::All,
        }
    }
}

/// The `[xsl:]validation` of `xsl:copy-of` and `xsl:copy` as the pipeline
/// applies it to the copy of each node the instruction copies (§11.9.1,
/// §11.9.2, §19.2.1.1, §19.2.2): "These attributes are applied individually
/// to each element, attribute, and document node that is selected by the
/// expression in the select attribute … they do not apply to nodes that are
/// implicitly copied because they have selected nodes as an ancestor" — and
/// "These attributes are ignored when copying an item that is not an
/// element, attribute or document node."
///
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyValidation {
    /// `strip`: the copy and the nodes it contains are untyped, is-id kept —
    /// made so by the [`ComplexContentNormalizer`] itself, whichever receivers
    /// build the tree (the ids pass with [`Receiver::element_id`]); the copy
    /// is no boundary.
    #[default]
    Strip,
    /// `preserve`: "In the case of xsl:copy-of, all the nodes that are copied
    /// will retain their type annotations unchanged" — the copy is no
    /// boundary, so an enclosing `strip` or `strict` still decides.
    Preserve,
    /// `strict`: the copy of an element is validated as a constructed element
    /// ([`Boundary::Strict`] on it), the copy of a document node as a
    /// document (§19.2.2, [`Boundary::Strict`] on a document node). An
    /// attribute is validated by the instruction before it is passed on
    /// (`Receiver::attribute` with its new binding), never through
    /// [`Receiver::append_copy`]. The validation is the validating receiver's
    /// (the transformation's pipelines): a chain without it — a normalizer
    /// over a bare [`ResultBuilder`] — ignores the boundary, as it ignores
    /// every boundary, and builds the copy untyped.
    Strict,
    /// `lax`: as `strict`, with [`Boundary::Lax`] (§19.2.1.1: validated
    /// against the top-level declaration when there is one, laxly otherwise).
    Lax,
    /// `type`: the copy of an element is validated against the type
    /// ([`Boundary::Type`], §19.2.1.2), the copy of a document node as a
    /// document whose element child is (§19.2.2). An attribute is validated
    /// by the instruction, as for `strict`.
    Type(TypeKey),
    /// `type="xs:untyped"`: [`Boundary::Untyped`] — on an element the effect
    /// of `strip`, on a document node document-level validation whose element
    /// child is untyped.
    Untyped,
}

impl CopyValidation {
    /// Whether the copy of a node is validated (`strict`, `lax`, `type`): its
    /// source's bindings are then not read, the validation decides.
    pub fn validates(self) -> bool {
        matches!(
            self,
            CopyValidation::Strict | CopyValidation::Lax | CopyValidation::Type(_)
        )
    }

    /// The boundary the copy of an element or document node gets — `None`
    /// for `strip` and `preserve`, which copy with no boundary.
    pub fn boundary(self) -> Option<Boundary> {
        match self {
            CopyValidation::Strip | CopyValidation::Preserve => None,
            CopyValidation::Strict => Some(Boundary::Strict),
            CopyValidation::Lax => Some(Boundary::Lax),
            CopyValidation::Type(type_key) => Some(Boundary::Type(type_key)),
            CopyValidation::Untyped => Some(Boundary::Untyped),
        }
    }
}

impl Annotations {
    /// What the pipelines of a transformation read off the nodes they copy into
    /// a tree through [`Receiver::append_item`]: everything a typed node carries
    /// when the transformation is schema-aware, so that a node copied into a
    /// tree keeps its annotation unless a construction boundary around it says
    /// otherwise ([`Boundary`]; §19.2.1.1: under `preserve` "nodes that are
    /// copied will retain their type annotations"); nothing for a basic
    /// processor, whose nodes are never typed (§21.1).
    pub fn of_copies(schema_set: Option<&SchemaSet>) -> Self {
        if schema_set.is_some() {
            Annotations::Preserve
        } else {
            Annotations::Strip
        }
    }
}

/// The `[xsl:]validation` of an instruction that constructs a container — an
/// element or a document node — as far as the tree being built is concerned
/// (§19.2.1.1, §19.2.2): it decides the annotations of the container and of
/// every node it contains.
///
/// > The `[xsl:]validation` attribute defines the validation action to be
/// > taken. It determines not only the type annotation of the node that is
/// > constructed by the relevant instruction itself, but also the type
/// > annotations of all element and attribute nodes that have the constructed
/// > node as an ancestor. Conceptually, the validation requested for a child
/// > element or attribute node is applied before the validation requested for
/// > its parent element.
///
/// So the **outermost** enclosing boundary whose validation is not `preserve`
/// decides a node's annotation: under a `strip` boundary every
/// node is untyped; when every enclosing boundary is `preserve`, a new element
/// is `xs:anyType` and every other node keeps what it was given (a copied node
/// its source's annotation, an attribute what its own `xsl:attribute`
/// validated it to); under a `strict` one, the node has what that boundary's
/// validation gave it — every `strict` boundary still validates what it
/// contains, for its errors. The validating receiver of the transformation's
/// pipelines (`runtime::validate::Validating`) applies the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// `validation="strip"`: the container and every node it contains are
    /// untyped — elements `xs:untyped`, attributes `xs:untypedAtomic`, nilled
    /// false — while is-id and is-idrefs are unchanged.
    Strip,
    /// `validation="preserve"`: no validation; a new element is `xs:anyType`,
    /// the nodes it contains keep their annotations. A document node created
    /// for a variable is one (§9.4, §19.2.2).
    Preserve,
    /// `validation="strict"` on an element constructor (§19.2.1.1): the
    /// element is assessed against the top-level declaration of its name, and
    /// the assessment annotates it and everything it contains. On a document
    /// node (`xsl:document`, `xsl:result-document`, a copy of a document
    /// node — §19.2.2) it requests document-level validation: exactly one
    /// element child and no text children (XTTE1550), that element validated
    /// as a validation root with the ID/IDREF rule applied (XTTE1555).
    Strict,
    /// `validation="lax"` (§19.2.1.1): as `strict` when the element has a
    /// top-level declaration; otherwise it "is not validated, but its
    /// attributes and children are validated, again with lax validation" —
    /// XML Schema's lax wildcard validation (§5.2). On a document node,
    /// document-level validation whose element child is validated so.
    Lax,
    /// `[xsl:]type` (§19.2.1.2): the element is assessed with this type as
    /// the "processor-stipulated type definition", whatever its name; on a
    /// document node, document-level validation whose element child is. Any
    /// in-scope type (§3.13), the XPath 2.0 types `xs:untypedAtomic`,
    /// `xs:anyAtomicType`, `xs:dayTimeDuration` and `xs:yearMonthDuration`
    /// included: upstream has them as components of every schema set
    /// (`SchemaSet::xpath_type`).
    Type(TypeKey),
    /// `[xsl:]type="xs:untyped"`: XDM 1.0 §2.6.2, "the dynamic type of an
    /// element node that has not been validated, or has been validated in skip
    /// mode", with untyped content (§6.2.1) — on an element the effect of
    /// `strip`; on a document node, document-level validation (§19.2.2 "when a
    /// type attribute is present": XTTE1550) whose element child is untyped.
    Untyped,
}

impl Boundary {
    /// Whether the boundary requests validation (§19.2.2: "when validation is
    /// set to strict or lax, or when a type attribute is present") — on a
    /// document node, document-level validation.
    pub fn requests_validation(self) -> bool {
        !matches!(self, Boundary::Strip | Boundary::Preserve)
    }
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
    /// element ends up **parentless** (§11.9.1, `xsl:copy`; §11.1 and §11.2,
    /// literal result elements and `xsl:element`: the instruction's base URI).
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

    /// The container the **next** [`start_element`](Receiver::start_element)
    /// or [`start_document`](Receiver::start_document) opens is constructed by
    /// an instruction whose `[xsl:]validation` is `boundary` (§19.2.1.1,
    /// §19.2.2). A container opened without one — an element a deep copy
    /// produces, the holder of a parentless node — is no boundary: its nodes
    /// keep what they arrive with, unless an enclosing boundary strips them.
    ///
    /// The default implementation ignores it, which is right for every
    /// receiver that builds no typed tree: only the validating receiver
    /// (`runtime::validate::Validating`) applies the rule; the receivers in
    /// front of it pass the announcement on, in order.
    ///
    fn validation_boundary(&mut self, _boundary: Boundary) -> Result<(), XsltError> {
        Ok(())
    }

    /// The element whose start tag is open answers to `id` in the id index
    /// (`fn:id`) although no binding of it or of its attributes says so: an
    /// `xs:ID` attribute a copy with `validation="strip"` made untyped keeps
    /// its is-id property (§19.2.1.1: "The values of the is-id and is-idrefs
    /// properties are unchanged"), and so does an element that is an ID by
    /// its own typed value (XDM 1.0 §6.2.4). Sent by a
    /// [`ComplexContentNormalizer`] just after that attribute, or after the
    /// element's attributes for its own id. A [`ResultBuilder`] files the
    /// element under the id; the default implementation ignores it, which is
    /// right for every receiver that builds no tree.
    fn element_id(&mut self, _id: &str) -> Result<(), XsltError> {
        Ok(())
    }

    /// The attribute passed last ([`attribute`](Receiver::attribute)) has the
    /// is-idrefs property although no binding says so: an `xs:IDREF` /
    /// `xs:IDREFS` attribute a `strip` copy made untyped keeps it (§19.2.1.1;
    /// §11.9.1: the copy of an attribute "will also have the same values for
    /// the is-id and is-idrefs properties"), so `fn:idref` still finds it.
    /// Sent by a [`ComplexContentNormalizer`] just after the attribute. A
    /// [`ResultBuilder`] marks the attribute; the default implementation
    /// ignores it, which is right for every receiver that builds no tree.
    fn attribute_idrefs(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    /// The element whose start tag is open has the is-idrefs property
    /// although no binding says so — the element counterpart of
    /// [`attribute_idrefs`](Receiver::attribute_idrefs), for an element whose
    /// typed value holds an `xs:IDREF`. Sent by a [`ComplexContentNormalizer`]
    /// after the element's attributes. A [`ResultBuilder`] marks the element,
    /// which is filed by the tokens of its string value when it ends.
    fn element_idrefs(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    /// The attribute passed last is a copy of `source` that keeps its binding
    /// (`validation="preserve"`, or a node sequenced into element content): a
    /// receiver that takes the binding away — under a `strip` boundary,
    /// §19.2.1.1: "The values of the is-id and is-idrefs properties are
    /// unchanged" — keeps the is-id and is-idrefs properties `source` has.
    /// Sent by a [`ComplexContentNormalizer`] just after the attribute. The
    /// default implementation ignores it, which is right for every receiver
    /// that keeps bindings as they arrive (the binding gives the properties).
    fn attribute_copied_from(&mut self, _source: &Nav<'t>) -> Result<(), XsltError> {
        Ok(())
    }

    /// The element whose start tag is open is a copy of `source` that keeps
    /// its binding — the element counterpart of
    /// [`attribute_copied_from`](Receiver::attribute_copied_from), sent after
    /// the element's attributes.
    fn element_copied_from(&mut self, _source: &Nav<'t>) -> Result<(), XsltError> {
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
    /// `validation` is the copying instruction's ([`CopyValidation`]), which
    /// decides what the copy of a selected element, attribute or document node
    /// carries (§11.9.2: "These attributes are applied individually to each
    /// element, attribute, and document node that is selected").
    ///
    /// The default implementation forwards to `append_item`, which is right
    /// for a receiver that decomposes every item into events regardless and
    /// builds no typed tree.
    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        _namespaces: CopyNamespaces,
        _validation: CopyValidation,
    ) -> Result<(), XsltError> {
        self.append_item(item)
    }
}

/// XTTE1550 (§19.2.2) for a document node whose validation is requested and that has `what`
/// among its children: "A type error occurs unless the children of the document node comprise
/// exactly one element node, no text nodes, and zero or more comment and processing instruction
/// nodes, in any order."
pub(crate) fn xtte1550(what: &str) -> XsltError {
    XsltError::new(
        codes::XTTE1550,
        format!(
            "a document node whose validation is requested (validation=\"strict\" or \"lax\", or \
             a type) must have exactly one element child, no text child, and only comments and \
             processing instructions besides: it has {what}"
        ),
    )
}

/// What a copy of the attribute `nav` keeps without its binding (§19.2.1.1: "The values of the
/// is-id and is-idrefs properties are unchanged"; §11.9.1): the id it makes its element answer to,
/// and its is-idrefs property — both read off its source tree, which upstream classified by the
/// attribute's typed value with its element's in-scope namespaces, not
/// classified again here. is-idrefs is the node flag (`DomNavigator::is_idrefs`). is-id: an
/// attribute whose binding can make it an ID (`may_hold_id`) whose element answers to its value in
/// the id index (the navigator has no `is_id`; the index files the attribute's claim for its
/// element — the physical one, for a parentless attribute too: its hidden holder).
pub(crate) fn kept_of_attribute(nav: &Nav<'_>) -> Kept {
    let idrefs = nav.is_idrefs();
    let id = (|| {
        let binding = nav.schema_binding()?;
        let set = nav.document().schema_set()?;
        if !may_hold_id(set, binding.type_key, 0) {
            return None;
        }
        let mut owner = nav.document().create_navigator_at(nav.current_ref());
        if !owner.move_to_parent() {
            return None;
        }
        let owner = owner.current_ref();
        let value = nav.value();
        (nav.document().get_element_by_id_in_tree(owner, &value) == Some(owner)).then_some(value)
    })();
    Kept { id, idrefs }
}

/// Whether a value of `type_key` can be an `xs:ID` value: an atomic type derived from `xs:ID`, a
/// list whose item type, a union one of whose member types, can be (the walk of
/// `runtime::validate::namespace_sensitive`, for `xs:ID`). A complex type cannot: an attribute's type is simple.
fn may_hold_id(set: &SchemaSet, type_key: TypeKey, depth: u32) -> bool {
    reaches_code(set, type_key, &[XmlTypeCode::Id], depth)
}

/// Whether a value of `type_key` can hold an `xs:IDREF` value — the is-idrefs property: an atomic
/// type derived from `xs:IDREF`, `xs:IDREFS`, a list or a union reaching one (upstream's
/// `IdrefsClass`). A complex type cannot.
fn may_hold_idref(set: &SchemaSet, type_key: TypeKey) -> bool {
    reaches_code(set, type_key, &[XmlTypeCode::IdRef, XmlTypeCode::IdRefs], 0)
}

/// Whether a simple type reaches a built-in type of one of `codes` through its derivation, its list
/// item type or its union member types (the walk of [`may_hold_id`]).
fn reaches_code(set: &SchemaSet, type_key: TypeKey, codes: &[XmlTypeCode], depth: u32) -> bool {
    let TypeKey::Simple(key) = type_key else {
        return false;
    };
    if depth > 64 {
        return true;
    }
    if let Some(code) = set.get_type_code(key) {
        return codes.contains(&code);
    }
    let Some(def) = set.arenas.simple_types.get(key) else {
        return true;
    };
    match def.variety {
        SimpleTypeVariety::Atomic => def
            .resolved_base_type
            .is_none_or(|base| reaches_code(set, base, codes, depth + 1)),
        SimpleTypeVariety::List => def
            .resolved_item_type
            .is_none_or(|item| reaches_code(set, item, codes, depth + 1)),
        SimpleTypeVariety::Union => def
            .resolved_member_types
            .iter()
            .any(|&member| reaches_code(set, member, codes, depth + 1)),
    }
}

/// What a node keeps of its is-id and is-idrefs properties when a `strip` copy or boundary takes
/// its binding away (§4.3: "The values of the is-id and is-idrefs properties are not changed";
/// §19.2.1.1: "The values of the is-id and is-idrefs properties are unchanged"), passed on with
/// [`Receiver::element_id`], [`Receiver::attribute_idrefs`] and [`Receiver::element_idrefs`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kept {
    /// The id the node makes its element answer to in the id index: an attribute's `xs:ID` value,
    /// or an element's own `xs:ID` typed value.
    pub(crate) id: Option<String>,
    /// The node has the is-idrefs property.
    pub(crate) idrefs: bool,
}

/// The is-id and is-idrefs properties a node bound to `type_key` — a simple type, or a complex type
/// with simple content — has with the string value `value`, as the upstream builder decides them
/// for a bound node: is-id when the typed value is exactly one atomic value of type `xs:ID` or a
/// type derived from it (XDM 1.0 §6.2.4, §6.3.4), is-idrefs when "any of the atomic values in the
/// typed-value … is of type xs:IDREF or xs:IDREFS, or a type derived from one of those types". Only
/// a type that can hold either is validated again; an attribute type that cannot answers at once.
/// Validated **without** the node's namespaces (the public `validate_simple_type` takes none), so
/// it is exact only for a type whose values hold no `xs:QName` / `xs:NOTATION` — a union with a
/// QName member can be classified otherwise than validation with the node's in-scope namespaces
/// does: `Validating` asks it only for such types, or when the run's
/// own typed value is missing.
pub(crate) fn kept_by_type(value: &str, type_key: TypeKey, set: &SchemaSet) -> Kept {
    let candidate = match type_key {
        TypeKey::Simple(_) => may_hold_id(set, type_key, 0) || may_hold_idref(set, type_key),
        // Only complex types with simple content are asked (a run's text-only element).
        TypeKey::Complex(_) => true,
    };
    if !candidate {
        return Kept::default();
    }
    match validate_simple_type(value, type_key, set) {
        Ok(result) => Kept {
            id: single_id(&result.typed_value).map(str::to_string),
            idrefs: holds_idref(&result.typed_value),
        },
        Err(_) => Kept::default(),
    }
}

/// What a copy of the element `nav` keeps without its binding: the id it answers to by its own
/// typed value — read off its tree's id index, which upstream files such an element in by the
/// typed value it has with its in-scope namespaces; only an element whose binding can make it
/// an ID is looked up (`may_hold_id`, or simple content) — and its is-idrefs property (a node
/// flag: also kept by a tree that has it without a binding).
pub(crate) fn kept_of_element(nav: &Nav<'_>) -> Kept {
    let idrefs = nav.is_idrefs();
    let Some(binding) = nav.schema_binding() else {
        return Kept { id: None, idrefs };
    };
    let Some(set) = nav.document().schema_set() else {
        return Kept { id: None, idrefs };
    };
    let candidate = match binding.type_key {
        TypeKey::Simple(_) => may_hold_id(set, binding.type_key, 0),
        TypeKey::Complex(_) => binding.content_type == Some(ContentType::TextOnly),
    };
    let id = if candidate {
        let owner = nav.current_ref();
        let value = nav.value_ref();
        (nav.document().get_element_by_id_in_tree(owner, &value) == Some(owner))
            .then(|| value.into_owned())
    } else {
        None
    };
    Kept { id, idrefs }
}

/// The is-id and is-idrefs properties a node with the typed value `value` has (XDM 1.0 §6.2.4,
/// §6.3.4): is-id when it is exactly one `xs:ID` value, is-idrefs when it holds an `xs:IDREF`.
pub(crate) fn kept_of_value(value: &XmlValue) -> Kept {
    Kept {
        id: single_id(value).map(str::to_string),
        idrefs: holds_idref(value),
    }
}

/// Whether a typed value holds an atomic value of type `xs:IDREF` or a type derived from it (the
/// members of an `xs:IDREFS` value are such values) — upstream's `typed_is_idrefs`.
fn holds_idref(value: &XmlValue) -> bool {
    match &value.value {
        XmlValueKind::Atomic(_) => value.type_code == XmlTypeCode::IdRef,
        XmlValueKind::Union(member) => holds_idref(member),
        XmlValueKind::List { items, .. } => items.iter().any(holds_idref),
        XmlValueKind::UntypedAtomic(_) => false,
    }
}

/// The `xs:ID` value a typed value consists of, when it is exactly one atomic value of type
/// `xs:ID` or a type derived from it (every type derived from `xs:ID` validates to the `xs:ID`
/// type code); a union value counts by its member's value, a list value when it has exactly one
/// item.
fn single_id(value: &XmlValue) -> Option<&str> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::String(id)) if value.type_code == XmlTypeCode::Id => {
            Some(id)
        }
        XmlValueKind::Union(member) => single_id(member),
        XmlValueKind::List { items, .. } => match items.as_slice() {
            [member] => single_id(member),
            _ => None,
        },
        _ => None,
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

    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        self.out.validation_boundary(boundary)
    }

    fn element_id(&mut self, id: &str) -> Result<(), XsltError> {
        self.out.element_id(id)
    }

    fn attribute_idrefs(&mut self) -> Result<(), XsltError> {
        self.out.attribute_idrefs()
    }

    fn element_idrefs(&mut self) -> Result<(), XsltError> {
        self.out.element_idrefs()
    }

    fn attribute_copied_from(&mut self, source: &Nav<'t>) -> Result<(), XsltError> {
        self.out.attribute_copied_from(source)
    }

    fn element_copied_from(&mut self, source: &Nav<'t>) -> Result<(), XsltError> {
        self.out.element_copied_from(source)
    }

    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: CopyNamespaces,
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        self.out.append_copy(item, namespaces, validation)
    }
}

// ── ComplexContentNormalizer ─────────────────────────────────────────────

/// One open container of the normalizer.
#[derive(Default)]
struct Open<'t> {
    /// The element's name; `None` for the document level.
    name: Option<Name>,
    binding: Option<NodeSchemaBinding>,
    nilled: bool,
    /// The validation boundary the element's instruction announced
    /// ([`Receiver::validation_boundary`]), passed on just before the element
    /// when its start tag is flushed.
    boundary: Option<Boundary>,
    /// Namespace nodes seen for this element, not yet flushed.
    namespaces: Vec<(String, String)>,
    /// Attributes seen for this element, not yet flushed, each with what it
    /// keeps of is-id and is-idrefs without a binding (an attribute a `strip`
    /// copy made untyped; [`Receiver::element_id`],
    /// [`Receiver::attribute_idrefs`]).
    /// A `preserve` copy's attribute also carries its source, passed on with
    /// [`Receiver::attribute_copied_from`].
    attributes: Vec<Attribute<'t>>,
    /// What the element keeps of its own is-id and is-idrefs without a
    /// binding (an element a `strip` copy made untyped; [`Receiver::element_id`],
    /// [`Receiver::element_idrefs`]), passed on after its attributes.
    kept: Kept,
    /// The source of an element a `preserve` copy made (it keeps its binding),
    /// passed on after its attributes ([`Receiver::element_copied_from`]).
    source: Option<Nav<'t>>,
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
/// proportional to the bindings in scope, not to its depth.
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

/// One buffered attribute of an open start tag: its name, value and binding, what it keeps of is-id
/// and is-idrefs without a binding (a `strip` copy's), and the node a `preserve` copy copied.
type Attribute<'t> = (
    Name,
    String,
    Option<NodeSchemaBinding>,
    Kept,
    Option<Nav<'t>>,
);

impl Open<'_> {
    /// A container that inherits, which is the default everywhere.
    fn inheriting() -> Self {
        Self {
            inherit: true,
            ..Self::default()
        }
    }
}

/// A document node the normalizer has opened (see
/// [`ComplexContentNormalizer::document_marks`]).
struct DocumentMark {
    /// The depth of the normalizer's stack when the document node was opened.
    depth: usize,
    /// The validation boundary its instruction announced.
    boundary: Option<Boundary>,
    /// Whether `start_document` (and the boundary) has been passed on.
    ///
    /// A document node at the document level is passed on at once. One opened
    /// in element content contributes only its children to the element
    /// (§5.7.1), and the element's start tag may still be pending — an
    /// attribute may follow an empty document node — so it is passed on just
    /// before its first child is, after that start tag; one that turns out
    /// empty is never passed on. Either way the destination sees it nested
    /// inside the element, which is what a boundary needs.
    forwarded: bool,
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
/// the XQuery constructor; this crate has no fixup code of its own.
pub struct ComplexContentNormalizer<'t, R: Receiver<'t>> {
    out: R,
    stack: Vec<Open<'t>>,
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
    document_marks: Vec<DocumentMark>,
    /// The boundary announced for the next container
    /// ([`Receiver::validation_boundary`]).
    pending_boundary: Option<Boundary>,
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
            pending_boundary: None,
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
        !self.in_element()
            || self.document_marks.last().map(|mark| mark.depth) == Some(self.stack.len())
    }

    /// Passes on the document nodes opened in element content that enclose the
    /// node about to be passed on — those opened at a depth of at most `limit`
    /// — and have not been passed on yet ([`DocumentMark::forwarded`]). The
    /// ones not passed on are always the innermost.
    fn forward_documents(&mut self, limit: usize) -> Result<(), XsltError> {
        let Some(first) = self.document_marks.iter().position(|mark| !mark.forwarded) else {
            return Ok(());
        };
        for index in first..self.document_marks.len() {
            let mark = &mut self.document_marks[index];
            if mark.depth > limit {
                break;
            }
            mark.forwarded = true;
            if let Some(boundary) = mark.boundary {
                self.out.validation_boundary(boundary)?;
            }
            self.out.start_document()?;
        }
        Ok(())
    }

    /// The destination, once the stream is finished.
    pub fn into_inner(self) -> R {
        self.out
    }

    fn top(&mut self) -> &mut Open<'t> {
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
        self.forward_documents(self.stack.len())?;
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
        let boundary = open.boundary;
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
        let attrs: Vec<Attribute<'t>> = std::mem::take(&mut open.attributes);
        let element_kept = std::mem::take(&mut open.kept);
        let element_source = open.source.take();

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
            .map(|(n, _, _, _, _)| (n.prefix.as_str(), n.uri.as_str()))
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
            // namespace axis walks once per ancestor.
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

        self.forward_documents(self.stack.len() - 1)?;
        if let Some(boundary) = boundary {
            self.out.validation_boundary(boundary)?;
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
        for ((name, value, binding, kept, source), (prefix, _)) in attrs.iter().zip(fixed.iter()) {
            let fixed_name = Name {
                local: name.local.clone(),
                uri: name.uri.clone(),
                prefix: prefix.clone(),
            };
            self.out.attribute(&fixed_name, value, *binding)?;
            if let Some(id) = &kept.id {
                self.out.element_id(id)?;
            }
            if kept.idrefs {
                self.out.attribute_idrefs()?;
            }
            if let Some(source) = source {
                self.out.attribute_copied_from(source)?;
            }
        }
        if let Some(id) = &element_kept.id {
            self.out.element_id(id)?;
        }
        if element_kept.idrefs {
            self.out.element_idrefs()?;
        }
        if let Some(source) = &element_source {
            self.out.element_copied_from(source)?;
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
    /// those the parent itself inherited. The default namespace is
    /// `no_default`'s business, set when the element was opened. The set is
    /// the parent's [`PrefixedScope`], computed once per element and carried
    /// down from its own parent's, so finding it costs time
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
    ///   result tree has such a namespace node";
    /// - with `copy-namespaces="yes"` ("all the namespace nodes of the
    ///   original element are copied to the new element"), the parent's copied
    ///   nodes the original element lacks — a child of an element built with
    ///   `inherit-namespaces="no"`, which a copy must not hand those bindings
    ///   back to. The only way an element lacks a prefixed binding
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
    /// declarations costs O(1) per element (re-reading every open ancestor's
    /// `declared` for each parent would make it quadratic in the depth),
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

    /// Records one attribute, last-wins on a duplicate expanded name; `id` is the
    /// id it keeps without a binding (see `Open::attributes`).
    fn push_attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
        kept: Kept,
        source: Option<Nav<'t>>,
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
            .find(|(n, _, _, _, _)| n.local == name.local && n.uri == name.uri)
        {
            slot.1 = value.to_string();
            slot.2 = binding;
            slot.3 = kept;
            slot.4 = source;
        } else {
            open.attributes
                .push((name.clone(), value.to_string(), binding, kept, source));
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
        self.copy_subtree(nav, namespaces, self.annotations.into())
    }

    /// The copy `xsl:copy-of` (or `xsl:copy` of a node it does not construct
    /// anew) makes of one node it selected, under its `validation` (§11.9.2:
    /// "These attributes are applied individually to each element, attribute,
    /// and document node that is selected by the expression in the select
    /// attribute"; §19.2.1.1, §19.2.2):
    ///
    /// - `strip`: "the new node and each of the contained nodes will have the
    ///   type annotation xs:untyped … The values of the is-id and is-idrefs
    ///   properties are unchanged" — the copy is made untyped **here**, so
    ///   that it is untyped whichever receivers build it (the public pipeline
    ///   of a normalizer over a [`ResultBuilder`] too), and every copied
    ///   attribute that is an `xs:ID` in its source passes its id on with
    ///   [`Receiver::element_id`] (`Carry::Ids`); no boundary;
    /// - `preserve`: "all the nodes that are copied will retain their type
    ///   annotations unchanged" — every binding and the nilled property, and
    ///   no boundary (an enclosing `strip` or `strict` still decides);
    /// - `strict`: an element is a [`Boundary::Strict`] (validated against the
    ///   top-level declaration of its name, its descendants re-annotated by
    ///   that validation: "these attributes do indirectly affect the type
    ///   annotation carried by such implicitly copied nodes, as a consequence
    ///   of the validation process"), a document node a document-level
    ///   validation (a document node opened with [`Boundary::Strict`], in
    ///   element content too: §5.7.1 replaces it by its children afterwards);
    ///   the source's bindings are not read;
    /// - `lax` and `type`: as `strict`, with [`Boundary::Lax`] /
    ///   [`Boundary::Type`]; `type="xs:untyped"` is [`Boundary::Untyped`] over
    ///   a copy made as `strip` makes it.
    ///
    /// "These attributes are ignored when copying an item that is not an
    /// element, attribute or document node." An attribute with `strict`,
    /// `lax` or `type` is validated by the instruction, not here.
    pub(crate) fn copy_selected(
        &mut self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        match (nav.node_type(), validation) {
            (
                DomNodeType::Root | DomNodeType::Element | DomNodeType::Attribute,
                CopyValidation::Strip,
            ) => self.copy_subtree(nav, namespaces, Carry::Ids),
            (DomNodeType::Attribute, validation) if validation != CopyValidation::Preserve => {
                Err(XsltError::internal(
                    "a validated copy of an attribute is validated before it is passed on",
                ))
            }
            // `type="xs:untyped"` on an element: the effect of `strip`, is-id kept.
            (DomNodeType::Element, CopyValidation::Untyped) => {
                self.validation_boundary(Boundary::Untyped)?;
                self.copy_subtree(nav, namespaces, Carry::Ids)
            }
            (DomNodeType::Root, CopyValidation::Untyped) => {
                self.validation_boundary(Boundary::Untyped)?;
                self.start_document()?;
                self.copy_subtree(nav, namespaces, Carry::Ids)?;
                self.end_document()
            }
            (DomNodeType::Root, validation) if validation.validates() => {
                if let Some(boundary) = validation.boundary() {
                    self.validation_boundary(boundary)?;
                }
                self.start_document()?;
                self.copy_subtree(nav, namespaces, Carry::Nothing)?;
                self.end_document()
            }
            (DomNodeType::Element, validation) if validation.validates() => {
                if let Some(boundary) = validation.boundary() {
                    self.validation_boundary(boundary)?;
                }
                self.copy_subtree(nav, namespaces, Carry::Nothing)
            }
            (DomNodeType::Root | DomNodeType::Element | DomNodeType::Attribute, _) => {
                self.copy_subtree(nav, namespaces, Carry::All)
            }
            _ => self.copy_subtree(nav, namespaces, Carry::Nothing),
        }
    }

    /// The walk of [`copy_node`](Self::copy_node): the node and, for an
    /// element or a document node, everything below it, in document order.
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
    ///
    /// The walk is a loop with a cursor, not a recursion, so a copy of a
    /// document of any depth needs no more host stack than a shallow one.
    fn copy_subtree(
        &mut self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
        carry: Carry,
    ) -> Result<(), XsltError> {
        let top_is_element = match nav.node_type() {
            DomNodeType::Root => false,
            DomNodeType::Element => {
                self.open_copied_element(nav, namespaces, carry)?;
                true
            }
            _ => return self.copy_leaf(nav, carry),
        };
        // `depth` is how far below `nav` the cursor is; every element between
        // the two is open.
        let mut cursor = nav.clone();
        let mut depth = 0usize;
        let mut descend = true;
        loop {
            if descend && cursor.move_to_first_child() {
                depth += 1;
            } else {
                // The next node: a following sibling of the cursor or of the
                // nearest open element that has one, closing the elements left.
                loop {
                    if depth == 0 {
                        return if top_is_element {
                            self.end_element()
                        } else {
                            Ok(())
                        };
                    }
                    if cursor.move_to_next_sibling() {
                        break;
                    }
                    cursor.move_to_parent();
                    depth -= 1;
                    if depth > 0 {
                        self.end_element()?;
                    }
                }
            }
            descend = cursor.node_type() == DomNodeType::Element;
            if descend {
                self.open_copied_element(&cursor, namespaces, carry)?;
                if !cursor.clone().move_to_first_child() {
                    self.end_element()?;
                    descend = false;
                }
            } else {
                self.copy_leaf(&cursor, carry)?;
            }
        }
    }

    /// The start of an element's copy: its name and binding, its namespace
    /// nodes and its attributes ([`copy_subtree`](Self::copy_subtree)).
    fn open_copied_element(
        &mut self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
        carry: Carry,
    ) -> Result<(), XsltError> {
        let binding = match carry {
            Carry::All => CopySource::schema_binding(nav),
            Carry::Ids | Carry::Nothing => None,
        };
        // Only a typed node can be nilled; asking an untyped one would
        // still cost a typed-value computation.
        let nilled = binding.is_some() && matches!(nav.typed_value(), TypedValue::Nilled);
        self.start_element(&Name::of(nav), binding, nilled)?;
        self.top().copied = true;
        match carry {
            Carry::Ids => self.top().kept = kept_of_element(nav),
            // A `strip` boundary downstream may take the binding away (`Validating`):
            // the source's properties then stay, read from the source only then. A node
            // without a binding has nothing to lose but the is-idrefs a tree may give it by
            // registration (a strip copy's), a flag: kept at once.
            Carry::All if binding.is_some() => self.top().source = Some(nav.clone()),
            Carry::All => self.top().kept = kept_of_element(nav),
            Carry::Nothing => {}
        }
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
                self.copy_attribute(&attr, carry)?;
                if !attr.move_to_next_attribute() {
                    break;
                }
            }
        }
        self.end_start_tag()
    }

    /// The copy of a node that has no children to copy.
    fn copy_leaf(&mut self, nav: &Nav<'t>, carry: Carry) -> Result<(), XsltError> {
        match nav.node_type() {
            DomNodeType::Attribute => self.copy_attribute(nav, carry),
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

    fn copy_attribute(&mut self, nav: &Nav<'t>, carry: Carry) -> Result<(), XsltError> {
        let value = nav.value();
        // A `strip` copy keeps the is-id and is-idrefs its source has (`kept_of_attribute`); a
        // `preserve` copy keeps its binding and names its source, should a `strip` boundary
        // downstream take the binding away.
        let (binding, kept, source) = match carry {
            Carry::Nothing => (None, Kept::default(), None),
            Carry::Ids => (None, kept_of_attribute(nav), None),
            Carry::All => match CopySource::schema_binding(nav) {
                Some(binding) => (Some(binding), Kept::default(), Some(nav.clone())),
                None => (None, kept_of_attribute(nav), None),
            },
        };
        self.flush_text()?;
        self.push_attribute(&Name::of(nav), &value, binding, kept, source)
    }
}

impl<'t, R: Receiver<'t>> Receiver<'t> for ComplexContentNormalizer<'t, R> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        // The text before the document node is a sibling of it, not its child:
        // it goes out now, in the content it belongs to, before the document
        // node is passed on (§5.7.1 merges it with the document's first text
        // child only once the document node has been replaced by its
        // children — after the document node was constructed, and validated,
        // §19.2.2). Text never goes out on its own when nothing is pending.
        self.flush_text()?;
        let boundary = self.pending_boundary.take();
        // See `DocumentMark::forwarded`.
        let forwarded = !self.in_element();
        self.document_marks.push(DocumentMark {
            depth: self.stack.len(),
            boundary,
            forwarded,
        });
        if forwarded {
            if let Some(boundary) = boundary {
                self.out.validation_boundary(boundary)?;
            }
            self.out.start_document()?;
        }
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.flush_text()?;
        match self.document_marks.pop() {
            // A document node in element content that stayed empty is never passed on (see
            // `DocumentMark::forwarded`), so the destination cannot see that it has no element
            // child — which is XTTE1550 when its validation was requested (§19.2.2).
            Some(mark) if !mark.forwarded => match mark.boundary {
                Some(boundary) if boundary.requests_validation() => Err(xtte1550("no child")),
                _ => Ok(()),
            },
            _ => self.out.end_document(),
        }
    }

    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        self.pending_boundary = Some(boundary);
        Ok(())
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
        let boundary = self.pending_boundary.take();
        self.stack.push(Open {
            name: Some(name.clone()),
            binding,
            nilled,
            boundary,
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
        self.push_attribute(name, value, binding, Kept::default(), None)
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
        self.forward_documents(self.stack.len())?;
        let top = self.top();
        top.children_started = true;
        top.last_atomic = false;
        self.out.comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.flush_text()?;
        self.flush_start_tag()?;
        self.forward_documents(self.stack.len())?;
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
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(_) => self.append_item(item),
            XmlItem::Node(nav) => {
                self.copy_selected(nav, namespaces, validation)?;
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
///
/// Every binding is built as it arrives: the annotation rule of §19.2.1.1
/// ([`Boundary`]) is applied in front of it, by the validating receiver of the
/// transformation's pipelines (`runtime::validate::Validating`).
pub struct ResultBuilder<'t> {
    builder: BufferDocumentBuilder<'t>,
    pending: Option<(Name, Option<NodeSchemaBinding>, bool)>,
    decls: Vec<(String, String)>,
    /// The pending element's attributes, each with whether it keeps the is-idrefs property without
    /// a binding ([`Receiver::attribute_idrefs`]).
    attrs: Vec<(Name, String, Option<NodeSchemaBinding>, bool)>,
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
    /// The id values the pending element answers to without an attribute
    /// bound as an `xs:ID` saying so ([`file_id`](Self::file_id)).
    filed_ids: Vec<String>,
    /// The pending element keeps the is-idrefs property without a binding
    /// ([`Receiver::element_idrefs`]).
    element_idrefs: bool,
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
            filed_ids: Vec::new(),
            element_idrefs: false,
        })
    }

    /// The element whose start tag is being built answers to `id` in the id
    /// index (`fn:id`), although no binding of it or of its attributes says
    /// so: an attribute whose `xs:ID` annotation a `strip` boundary or copy
    /// took away keeps its is-id property (§19.2.1.1: "The values of the is-id
    /// and is-idrefs properties are unchanged"), and so does an element that
    /// is an ID by its own typed value. Filed with `register_xml_id`
    /// when the start tag is flushed; a value another element of the tree
    /// already answers to stays with that element (no uniqueness check, §9.4,
    /// §19.2.1.3).
    pub(crate) fn file_id(&mut self, id: String) {
        self.filed_ids.push(id);
    }

    /// The attribute passed last keeps the is-id and is-idrefs properties `kept` says, although a
    /// `strip` boundary took its binding away (§19.2.1.1: "The values of the is-id and is-idrefs
    /// properties are unchanged"): its element is filed under its `xs:ID` value
    /// ([`file_id`](Self::file_id)), and it is marked is-idrefs (`register_idrefs`, when the start
    /// tag is flushed).
    pub(crate) fn keep_attribute(&mut self, kept: Kept) {
        if let Some(id) = kept.id {
            self.file_id(id);
        }
        if kept.idrefs {
            if let Some(last) = self.attrs.last_mut() {
                last.3 = true;
            }
        }
    }

    /// The innermost open element keeps the is-id and is-idrefs properties `kept` says, although a
    /// `strip` boundary took its binding away (§19.2.1.1) — decided when its content is known, so
    /// sent just before it ends: filed under its own `xs:ID` value (`register_xml_id`) and marked
    /// is-idrefs (`register_idrefs`, filed by its string value when it ends).
    pub(crate) fn keep_open_element(&mut self, kept: Kept) -> Result<(), XsltError> {
        self.flush()?;
        let Some(&elem) = self.open.last() else {
            return Ok(());
        };
        if let Some(id) = kept.id {
            register_id(&mut self.builder, &id, elem)?;
        }
        if kept.idrefs {
            self.builder.register_idrefs(elem);
        }
        Ok(())
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
        for (attr, value, attr_binding, idrefs) in std::mem::take(&mut self.attrs) {
            let attr_ref = self
                .builder
                .attribute(&attr.local, &attr.uri, &attr.prefix, &value)
                .map_err(build_error)?;
            if let Some(binding) = attr_binding {
                self.builder
                    .set_node_binding(attr_ref, binding)
                    .map_err(build_error)?;
            }
            if idrefs {
                self.builder.register_idrefs(attr_ref);
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
        for id in std::mem::take(&mut self.filed_ids) {
            register_id(&mut self.builder, &id, elem)?;
        }
        if std::mem::take(&mut self.element_idrefs) {
            // Open: marked now, filed by the tokens of its string value when it ends.
            self.builder.register_idrefs(elem);
        }
        self.open.push(elem);
        Ok(())
    }
}

fn build_error(e: xsd_schema::document::BufferDocumentError) -> XsltError {
    XsltError::internal(format!("the result-tree builder refused: {e}"))
}

/// Files `elem` under `id` with `register_xml_id`. No document-level validation, so no uniqueness
/// check (§9.4, §19.2.1.3): the first element of the tree keeps the value, as for any other id.
fn register_id(
    builder: &mut BufferDocumentBuilder<'_>,
    id: &str,
    elem: u32,
) -> Result<(), XsltError> {
    match builder.register_xml_id(id, elem) {
        Ok(()) | Err(BufferDocumentError::DuplicateId(_)) => Ok(()),
        Err(e) => Err(build_error(e)),
    }
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
        self.attrs
            .push((name.clone(), value.to_string(), binding, false));
        Ok(())
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.flush()
    }

    /// Files the pending element under `id` in the id index of the tree (`file_id`).
    fn element_id(&mut self, id: &str) -> Result<(), XsltError> {
        self.file_id(id.to_string());
        Ok(())
    }

    /// Marks the attribute passed last with the is-idrefs property (`register_idrefs`, when the
    /// start tag is flushed).
    fn attribute_idrefs(&mut self) -> Result<(), XsltError> {
        if let Some(last) = self.attrs.last_mut() {
            last.3 = true;
        }
        Ok(())
    }

    /// Marks the pending element with the is-idrefs property (`register_idrefs`, when the start tag
    /// is flushed; filed by its string value when it ends).
    fn element_idrefs(&mut self) -> Result<(), XsltError> {
        if self.pending.is_some() {
            self.element_idrefs = true;
        }
        Ok(())
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
#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn push_qname(out: &mut String, prefix: &str, local: &str) {
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push(':');
    }
    out.push_str(local);
}

/// Text escaping, matching `document::serialize::write_text`.
#[cfg(test)]
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
#[cfg(test)]
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

/// Collects the result of a sequence constructor evaluated with `as=`.
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
    nested: Option<ComplexContentNormalizer<'t, Validating<'s, 't>>>,
    /// The validator a `strict` boundary in a group validates with
    /// ([`with_validator`](Self::with_validator)).
    validator: Option<&'s SchemaValidator<'s>>,
    /// The transformation's value constraints, which the groups' validation reads
    /// ([`with_value_constraints`](Self::with_value_constraints)).
    constraints: Option<&'s crate::runtime::validate::ValueConstraints>,
    /// What the open group will produce.
    group: GroupKind,
    /// The base URI the open `GroupKind::Document` group takes (§11.5).
    base_uri: Option<String>,
    /// How many containers (elements and document nodes) are open.
    depth: usize,
    /// How many zero-length text nodes the sequence had that are not in
    /// [`items`](Self::items) — see [`zero_length_text_nodes`](Self::zero_length_text_nodes).
    zero_length_texts: usize,
    /// A boundary announced while no group is open: it is the next group's
    /// (`Receiver::validation_boundary`).
    pending_boundary: Option<Boundary>,
}

/// What a construction group in a sequence produces.
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
    /// shorter borrow enough.
    pub fn new(store: &'s RefCell<DocumentStore<'t>>) -> Self {
        Self {
            store,
            items: Vec::new(),
            nested: None,
            group: GroupKind::Element,
            base_uri: None,
            depth: 0,
            zero_length_texts: 0,
            pending_boundary: None,
            validator: None,
            constraints: None,
        }
    }

    /// The collector of a transformation that validates constructed elements
    /// (§19.2.1.1): a `strict` boundary in one of its groups is validated with
    /// `validator` — the transformation's one validator.
    pub(crate) fn with_validator(mut self, validator: Option<&'s SchemaValidator<'s>>) -> Self {
        self.validator = validator;
        self
    }

    /// The collector of a transformation, whose value constraints the
    /// validation of its groups reads and keeps.
    pub(crate) fn with_value_constraints(
        mut self,
        constraints: Option<&'s crate::runtime::validate::ValueConstraints>,
    ) -> Self {
        self.constraints = constraints;
        self
    }

    /// A validating receiver in front of `builder`, as this collector's
    /// groups validate.
    fn validating(
        &self,
        builder: ResultBuilder<'t>,
        schema_set: Option<&'t SchemaSet>,
    ) -> Validating<'s, 't> {
        Validating::new(builder, schema_set, self.validator)
            .with_value_constraints(self.constraints)
    }

    /// How many items of the sequence were zero-length text nodes, which
    /// [`items`](Self::items) does not hold.
    ///
    /// §11.4: "It is possible to construct text nodes whose string value is
    /// zero-length. A zero-length text node, when atomized, produces a
    /// zero-length string. However, zero-length text nodes are ignored when
    /// they appear in a sequence that is used to form the content of a node" —
    /// so in a *sequence* (§11.4.2 `<xsl:text/>`, §11.4.3 `xsl:value-of`) such
    /// a node is an item like any other. It cannot be kept as one: the
    /// upstream `BufferDocumentBuilder` builds no text node with an empty
    /// value (`text` only buffers, and `flush_text` adds a node only when the
    /// buffer is not empty), so there is no `BufferDocument` node to point at.
    /// Instead the collector counts them, and a caller that passes the
    /// sequence on to another receiver unchanged — a template with `as`, the
    /// sequence constructor of `xsl:perform-sort` — passes them on as
    /// zero-length [`text`](Receiver::text) events after the items
    /// ([`replay_zero_length_text_nodes`](Self::replay_zero_length_text_nodes)).
    /// There §5.7.1 and §5.7.2 drop them, as the spec says, and the initial
    /// template's result notes that it is not empty (§2.4).
    ///
    /// What this does not cover — a sequence that becomes an XPath value (a
    /// variable, parameter or function result with `as`, a sort or key
    /// value) — still loses them: `count()` of such a value, its type check
    /// (`as="text()"`) and its atomization (`as="xs:string"`) see one item
    /// fewer per zero-length text node.
    pub(crate) fn zero_length_text_nodes(&self) -> usize {
        self.zero_length_texts
    }

    /// Passes the zero-length text nodes of the sequence on to `out`, as
    /// zero-length [`text`](Receiver::text) events — see
    /// [`zero_length_text_nodes`](Self::zero_length_text_nodes).
    pub(crate) fn replay_zero_length_text_nodes(
        count: usize,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        for _ in 0..count {
            out.text("", false)?;
        }
        Ok(())
    }

    /// The collected sequence.
    pub fn into_items(self) -> Vec<XmlItem<Nav<'t>>> {
        self.items
    }

    fn begin_group(&mut self, group: GroupKind) -> Result<(), XsltError> {
        let builder = self.new_builder(group)?;
        let schema_set = self.store.borrow().schema_set();
        let annotations = Annotations::of_copies(schema_set);
        let mut nested = ComplexContentNormalizer::with_annotations(
            self.validating(builder, schema_set),
            annotations,
        );
        if let Some(boundary) = self.pending_boundary.take() {
            nested.validation_boundary(boundary)?;
        }
        self.nested = Some(nested);
        self.group = group;
        self.base_uri = None;
        Ok(())
    }

    fn new_builder(&self, group: GroupKind) -> Result<ResultBuilder<'t>, XsltError> {
        let (arena, names, schema_set) = {
            let store = self.store.borrow();
            (store.arena(), store.names(), store.schema_set())
        };
        let options = match group {
            // A parentless node is built in a hidden document whose root the
            // navigator then hides (`BufferDocNavigator::new_orphan`).
            GroupKind::Element => BufferDocumentOptions::fragment(),
            // A document node is a real document node, with its own identity.
            GroupKind::Document => BufferDocumentOptions::default(),
        };
        ResultBuilder::with_options(arena, names, schema_set, options)
    }

    fn end_group(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.take() else {
            return Ok(());
        };
        let group = self.group;
        let mut doc = nested.into_inner().into_inner().finish()?;
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
    /// A newly constructed node that lands in a *sequence* rather than
    /// in element content is parentless. An attribute, comment, processing
    /// instruction or text node has no way to exist in a `BufferDocument`
    /// without a parent, so one is built around it and the navigator hides it.
    ///
    /// The holder is no validation boundary, so the node keeps the binding it
    /// is built with — and a copy built inside it is validated as its copying
    /// instruction asks ([`CopyValidation`]; the validating receiver is in the
    /// pipeline).
    fn push_parentless(
        &mut self,
        build: impl FnOnce(
            &mut ComplexContentNormalizer<'t, Validating<'s, 't>>,
        ) -> Result<(), XsltError>,
        locate: Locate,
    ) -> Result<(), XsltError> {
        self.push_parentless_with_base(build, locate, None)
    }

    /// [`push_parentless`](Self::push_parentless), the hidden document taking
    /// `base` as its base URI — the base URI of a parentless element built
    /// in it that has no `xml:base` of its own (the navigator reports it
    /// there; `stylesheet::node_base_uri` reads it).
    fn push_parentless_with_base(
        &mut self,
        build: impl FnOnce(
            &mut ComplexContentNormalizer<'t, Validating<'s, 't>>,
        ) -> Result<(), XsltError>,
        locate: Locate,
        base: Option<String>,
    ) -> Result<(), XsltError> {
        let builder = self.new_builder(GroupKind::Element)?;
        let schema_set = self.store.borrow().schema_set();
        let mut pipeline = ComplexContentNormalizer::new(self.validating(builder, schema_set));
        build(&mut pipeline)?;
        let mut doc = pipeline.into_inner().into_inner().finish()?;
        if let Some(base) = base.filter(|base| !base.is_empty()) {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(&base)));
        }
        self.push_holder(doc, locate)
    }

    /// A parentless namespace node (§11.7).
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

    /// Appends a **new** node that is a deep copy of `nav` (§11.9.1, §11.9.2),
    /// made as `validation` says ([`CopyValidation`]).
    ///
    /// The copy is built in its own hidden document, so it is parentless and
    /// is not the node it was copied from — which is the whole difference
    /// between `xsl:copy-of` and `xsl:sequence` when the result lands in a
    /// sequence rather than in element content.
    ///
    /// §11.9.2: "The base URI of a node is copied, except in the case of an
    /// element node having an xml:base attribute, in which case the base URI
    /// of the new node is taken as the value of the xml:base attribute,
    /// resolved if it is relative against the base URI of the xsl:copy-of
    /// instruction." The copy of a document node, and of an element without
    /// an `xml:base` of its own, takes the original's base URI
    /// (`stylesheet::node_base_uri`: its `xml:base` chain resolved) as the base
    /// URI of its hidden document. An element with one carries it: its base
    /// URI is that `xml:base` resolved — upstream's `fn:base-uri` resolves it
    /// against the static base URI of the expression that asks, not of the
    /// instruction (noticed; a proposed upstream row).
    fn push_copy(
        &mut self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        match nav.node_type() {
            // A document node copies to a new document node, with its own
            // identity: "the instruction adds a new document node to the
            // result sequence". A `strict` copy is a document node validated
            // as a document (§19.2.2), opened inside the new one — the same
            // tree, through the validating receiver.
            DomNodeType::Root => {
                let builder = self.new_builder(GroupKind::Document)?;
                let schema_set = self.store.borrow().schema_set();
                let mut pipeline =
                    ComplexContentNormalizer::new(self.validating(builder, schema_set));
                pipeline.start_document()?;
                pipeline.copy_selected(nav, namespaces, validation)?;
                pipeline.end_document()?;
                let mut doc = pipeline.into_inner().into_inner().finish()?;
                let base = crate::stylesheet::node_base_uri(nav, "");
                if !base.is_empty() {
                    doc.set_document_base_uri(Some(self.store.borrow().intern_uri(&base)));
                }
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
            // A parentless attribute: untyped with `strip`, its own annotation
            // with `preserve` — copied by the normalizer as any selected
            // attribute is (`copy_selected`), so that it keeps its is-id and
            // is-idrefs properties for a later copy into an element (`fn:id` and
            // `fn:idref` need a document node at the root of the tree, so they
            // are not observable on the parentless node itself).
            DomNodeType::Attribute => {
                if !matches!(validation, CopyValidation::Strip | CopyValidation::Preserve) {
                    return Err(XsltError::internal(
                        "a validated copy of an attribute is validated before it is passed on",
                    ));
                }
                let source = nav.clone();
                self.push_parentless(
                    move |out| {
                        out.start_element(&Name::local(HOLDER), None, false)?;
                        out.copy_selected(&source, namespaces, validation)?;
                        out.end_start_tag()?;
                        out.end_element()
                    },
                    Locate::Attribute,
                )
            }
            _ => {
                let source = nav.clone();
                let base = (nav.node_type() == DomNodeType::Element
                    && crate::stylesheet::attr_ns(nav, crate::names::XML_NS, "base").is_none())
                .then(|| crate::stylesheet::node_base_uri(nav, ""));
                self.push_parentless_with_base(
                    move |out| {
                        out.start_element(&Name::local(HOLDER), None, false)?;
                        out.copy_selected(&source, namespaces, validation)?;
                        out.end_start_tag()?;
                        out.end_element()
                    },
                    Locate::Child,
                    base,
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

    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            Some(nested) => nested.validation_boundary(boundary),
            None => {
                self.pending_boundary = Some(boundary);
                Ok(())
            }
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
            // An item of the sequence that no `BufferDocument` can hold: it is
            // counted instead (`zero_length_text_nodes`). Inside a
            // construction group, above, it is content, where §5.7.1 drops it.
            self.zero_length_texts += 1;
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
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        match (self.nested.as_mut(), item) {
            // Inside a construction group the copy is content, so it is
            // decomposed into events like anything else.
            (Some(nested), _) => nested.append_copy(item, namespaces, validation),
            // "If the item is an atomic value, the value is appended to the
            // result sequence, as with xsl:sequence."
            (None, XmlItem::Atomic(_)) => {
                self.items.push(item.clone());
                Ok(())
            }
            (None, XmlItem::Node(nav)) => self.push_copy(nav, namespaces, validation),
        }
    }
}

#[cfg(test)]
mod tests;
