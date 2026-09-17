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
//! existing node keeps its identity instead of being copied (§6.3).

use std::cell::RefCell;

use xsd_schema::document::{
    serialize, BufferDocNavigator, BufferDocument, BufferDocumentBuilder, BufferDocumentOptions,
    CopySource, DocumentKind, NamespaceFixup, NodeSchemaBinding,
};
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope, TypedValue};
use xsd_schema::types::XmlValue;
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
            stack: vec![Open::default()],
            text: String::new(),
            text_pending: false,
            fixup: NamespaceFixup::new(),
            annotations,
            _marker: std::marker::PhantomData,
        }
    }

    /// The destination, once the stream is finished.
    pub fn into_inner(self) -> R {
        self.out
    }

    fn top(&mut self) -> &mut Open {
        self.stack.last_mut().expect("the document level is never popped")
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
        if text.is_empty() {
            return Ok(());
        }
        self.flush_start_tag()?;
        self.top().children_started = true;
        self.out.text(&text, false)
    }

    /// Writes the pending start tag: namespace fixup, then the declarations and
    /// attributes the element ends up with.
    fn flush_start_tag(&mut self) -> Result<(), XsltError> {
        if !self.in_element() || self.stack.last().expect("checked").flushed {
            return Ok(());
        }
        let open = self.stack.last_mut().expect("checked");
        open.flushed = true;
        let name = open.name.clone().unwrap_or_default();
        let binding = open.binding;
        let nilled = open.nilled;
        let keep: Vec<(String, String)> = open.namespaces.clone();
        let attrs: Vec<(Name, String, Option<NodeSchemaBinding>)> =
            std::mem::take(&mut open.attributes);

        let attr_names: Vec<(&str, &str)> = attrs
            .iter()
            .map(|(n, _, _)| (n.prefix.as_str(), n.uri.as_str()))
            .collect();
        let keep_refs: Vec<(&str, &str)> = keep
            .iter()
            .map(|(p, u)| (p.as_str(), u.as_str()))
            .collect();
        // Pushes the scope it computes; `end_element` pops it.
        let (decls, fixed) = self.fixup.declarations_for(
            (name.prefix.as_str(), name.uri.as_str()),
            &attr_names,
            &keep_refs,
        );

        self.out.start_element(&name, binding, nilled)?;
        for (prefix, uri) in &decls {
            self.out.namespace(prefix, uri)?;
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

    /// Records one attribute, last-wins on a duplicate expanded name.
    fn push_attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        if !self.in_element() {
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
    fn push_namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        if !self.in_element() {
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
    fn copy_node(&mut self, nav: &Nav<'t>) -> Result<(), XsltError> {
        match nav.node_type() {
            DomNodeType::Root => {
                let mut child = nav.clone();
                if child.move_to_first_child() {
                    loop {
                        self.copy_node(&child)?;
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
                let mut ns = nav.clone();
                if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
                    loop {
                        // `local_name()` of a namespace node is its prefix.
                        let (prefix, uri) = (ns.local_name().to_string(), ns.value());
                        self.namespace(&prefix, &uri)?;
                        if !ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                            break;
                        }
                    }
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
                        self.copy_node(&child)?;
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
        self.out.start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.flush_text()?;
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
        self.stack.push(Open {
            name: Some(name.clone()),
            binding,
            nilled,
            ..Open::default()
        });
        Ok(())
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.flush_text()?;
        self.push_namespace(prefix, uri)
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

    fn text(&mut self, s: &str, _disable_escaping: bool) -> Result<(), XsltError> {
        if s.is_empty() {
            return Ok(());
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
                self.copy_node(nav)?;
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
            .map_err(|e| XsltError::new("XTDE0000", format!("could not start a result tree: {e}")))?;
        Ok(Self {
            builder,
            pending: None,
            decls: Vec::new(),
            attrs: Vec::new(),
            open: Vec::new(),
        })
    }

    /// The finished document.
    pub fn finish(mut self) -> Result<BufferDocument<'t>, XsltError> {
        self.flush()?;
        self.builder
            .finalize()
            .map_err(|e| XsltError::new("XTDE0000", format!("could not finish a result tree: {e}")))
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
    XsltError::new("XTDE0000", format!("the result-tree builder refused: {e}"))
}

impl<'t> Receiver<'t> for ResultBuilder<'t> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.flush()
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        self.flush()?;
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

    fn text(&mut self, s: &str, _disable_escaping: bool) -> Result<(), XsltError> {
        self.flush()?;
        self.builder.text(s);
        Ok(())
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.flush()?;
        self.builder.comment(s).map_err(build_error)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.flush()?;
        self.builder
            .processing_instruction(target, data)
            .map_err(build_error)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.flush()?;
        self.open.pop();
        self.builder.end_element().map_err(build_error)
    }

    /// Unreachable below a [`ComplexContentNormalizer`], which decomposes every
    /// item into events before it reaches a tree builder.
    fn append_item(&mut self, _item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        Err(XsltError::new(
            "XTDE0000",
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
/// (`serialize.rs:961`).
#[derive(Default)]
pub struct EventSerializer {
    out: String,
    pending: Option<Name>,
    decls: Vec<(String, String)>,
    attrs: Vec<(Name, String)>,
    open: Vec<Name>,
    tag_open: bool,
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
        for (prefix, uri) in std::mem::take(&mut self.decls) {
            if prefix.is_empty() {
                self.out.push_str(" xmlns=\"");
            } else {
                self.out.push_str(" xmlns:");
                self.out.push_str(&prefix);
                self.out.push_str("=\"");
            }
            push_attribute_value(&mut self.out, &uri);
            self.out.push('"');
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
        Err(XsltError::new(
            "XTDE0000",
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
        .map_err(|e| XsltError::new("XTDE0000", format!("could not serialize: {e}")))
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
    depth: usize,
}

impl<'s, 't> SequenceCollector<'s, 't> {
    /// A collector whose hidden documents live in `store`.
    ///
    /// The store is borrowed for `'s` while the nodes it yields live for `'t`;
    /// see [`ProtoFunctions::evaluator`](crate::store::ProtoFunctions::evaluator)
    /// for why the two cannot be the same lifetime.
    pub fn new(store: &'s RefCell<DocumentStore<'t>>) -> Self {
        Self {
            store,
            items: Vec::new(),
            nested: None,
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

    fn begin_group(&mut self) -> Result<(), XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::with_options(
            arena,
            names,
            None,
            // Q6: the hidden document's synthetic root should be invisible.
            BufferDocumentOptions::fragment(),
        )?;
        self.nested = Some(ComplexContentNormalizer::new(builder));
        Ok(())
    }

    fn end_group(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.take() else {
            return Ok(());
        };
        let builder = nested.into_inner();
        let doc = builder.finish()?;
        let id = self.store.borrow_mut().insert(doc, None);
        let root = self.store.borrow().navigator(id);
        // The one element child of the hidden document.
        let mut elem = root.clone();
        let nav = if elem.move_to_first_child() {
            fragment_navigator(self.store, id, elem.current_ref())
        } else {
            root
        };
        self.items.push(XmlItem::Node(nav));
        Ok(())
    }
}

/// A navigator on the element of a hidden fragment document.
///
/// [`BufferDocNavigator::new_assertion`] is upstream's "fragment scope": it
/// hides the synthetic root's children from `move_to_first_child` and makes
/// `move_to_visible_root` land on this element. It does **not** make
/// `move_to_parent` fail — see `q6` in `tests/m0b_invariants.rs` and the M0b
/// report.
fn fragment_navigator<'t>(
    store: &RefCell<DocumentStore<'t>>,
    id: DocId,
    node: u32,
) -> Nav<'t> {
    let doc = store.borrow().get(id).expect("just inserted").doc;
    debug_assert_eq!(doc.kind(), DocumentKind::Fragment);
    BufferDocNavigator::new_assertion(doc, node)
}

impl<'t> Receiver<'t> for SequenceCollector<'_, 't> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        Ok(())
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.begin_group()?;
        }
        self.depth += 1;
        self.nested
            .as_mut()
            .expect("group open")
            .start_element(name, binding, nilled)
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.require_group()?.namespace(prefix, uri)
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.require_group()?.attribute(name, value, binding)
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.require_group()?.end_start_tag()
    }

    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        match self.nested.as_mut() {
            Some(nested) => nested.text(s, disable_escaping),
            // A bare text node in a sequence is an atomic-valued item here;
            // M0b does not construct parentless text nodes.
            None => {
                self.items
                    .push(XmlItem::Atomic(XmlValue::string(s.to_string())));
                Ok(())
            }
        }
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.require_group()?.comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.require_group()?.pi(target, data)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.require_group()?.end_element()?;
        self.depth -= 1;
        if self.depth == 0 {
            self.end_group()?;
        }
        Ok(())
    }

    /// Keeps the item **unchanged**: a node stays the very node it was, which
    /// is what `xsl:sequence select="//item"` inside a typed variable means.
    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        self.items.push(item.clone());
        Ok(())
    }
}

impl<'t> SequenceCollector<'_, 't> {
    /// The nested construction pipeline, or the M0b limitation.
    ///
    /// XDM allows a parentless attribute, comment or processing instruction in
    /// a sequence; M0b only builds the case §6.3 names — a construction group
    /// that starts with an element — so everything else is reported rather
    /// than silently dropped. M1 gives each kind its own hidden document.
    fn require_group(
        &mut self,
    ) -> Result<&mut ComplexContentNormalizer<'t, ResultBuilder<'t>>, XsltError> {
        self.nested.as_mut().ok_or_else(|| {
            XsltError::new(
                "XTDE0000",
                "M0b builds parentless nodes only for a construction group that starts \
                 with an element",
            )
        })
    }
}
