//! How a source document enters a run: admission, whitespace stripping
//! (§4.4) and URI resolution (§16.1).

use xsd_schema::document::BufferDocument;
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};

use super::Transformation;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::XML_NS;
use crate::receiver::{ComplexContentNormalizer, Name, Nav, Receiver, ResultBuilder};
use crate::resolver;
use crate::store::DocId;
use crate::whitespace;

impl<'a, 't> Transformation<'a, 't> {
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
    pub(super) fn admit(&self, uri: Option<&str>, xml: &str) -> Result<DocId, XsltError> {
        self.admit_with(uri, xml, codes::XTDE1170)
    }

    /// [`admit`](Self::admit), reporting a document that cannot be parsed
    /// with `parse_code`. (A re-entrant load keeps XTDE1170, §9.1.)
    fn admit_with(
        &self,
        uri: Option<&str>,
        xml: &str,
        parse_code: &str,
    ) -> Result<DocId, XsltError> {
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
        let parsed = parsed.map_err(|mut e| {
            e.code = parse_code.to_string();
            e
        })?;
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

    // ── §21.1 XTDE1665: the input may not be schema-typed ─────────────────

    /// Rejects an externally supplied tree that carries type annotations.
    ///
    /// > \[ERR XTDE1665\] A basic XSLT processor must raise a non-recoverable
    /// > dynamic error if the input to the processor includes a node with a
    /// > type annotation other than `xs:untyped` or `xs:untypedAtomic`, or an
    /// > atomic value of a type other than those which a basic XSLT processor
    /// > supports.
    ///
    /// > \[Note\] The phrase *input to the processor* is deliberately wide: it
    /// > includes the tree containing the initial context node, trees passed as
    /// > stylesheet parameters, trees accessed using the `document`, `doc` and
    /// > `collection` functions, and trees returned by extension functions.
    ///
    /// Of that list, only two can reach this processor already typed. Every
    /// tree it reads itself — [`load_document`](Self::load_document),
    /// [`register_source`](Self::register_source), `document()`, `doc()` — is
    /// parsed without a schema, and [`admit`](Self::admit) marks it as such;
    /// there are no extension functions and no `collection()`. What is left is
    /// a document a caller built elsewhere and handed to
    /// [`DocumentStore::insert`](crate::store::DocumentStore::insert) or
    /// [`insert_ref`](crate::store::DocumentStore::insert_ref), and a node-valued
    /// stylesheet parameter. Both are checked here, on the way into
    /// [`Transformation::run`](Self::run) — the point where they first become
    /// *input*.
    ///
    /// The question is asked of the document, in constant time:
    /// [`BufferDocument::has_type_annotations`] is exact — `true` iff some
    /// element or attribute of the document would report a type annotation —
    /// and is maintained where bindings are attached, so an untyped tree, which
    /// is every tree this processor parses itself and almost every tree a caller
    /// hands it, is admitted without being visited. Only a document that does
    /// carry annotations is walked, to name the offending node in the message;
    /// the walk can still come back empty when the annotations belong to another
    /// tree of the same document, and then this tree is admitted.
    ///
    /// `input-type-annotations="strip"` would make the error "not arise", but
    /// this crate does not implement that attribute at all (it is accepted and
    /// ignored), so there is nothing to honour here yet.
    pub(super) fn check_input_annotations(&self, node: &Nav<'t>) -> Result<(), XsltError> {
        let mut root = node.clone();
        root.move_to_root();
        if !root.document().has_type_annotations() {
            return Ok(());
        }
        // Nothing is remembered between calls: a rejected tree must be rejected
        // again if the same `Transformation` is run a second time, and an
        // admitted one costs nothing to admit again.
        match first_annotated(&root) {
            None => Ok(()),
            Some(found) => Err(XsltError::new(
                codes::XTDE1665,
                format!(
                    "the input to the processor contains the {} {}, which carries a schema type \
                     annotation; this is a basic XSLT processor, so the input must be untyped \
                     (§21.1)",
                    match found.node_type() {
                        DomNodeType::Attribute => "attribute",
                        _ => "element",
                    },
                    found.name()
                ),
            )),
        }
    }

    /// Registers text a later `document()` call should find at `uri`.
    pub fn register_source(&self, uri: &str, xml: &str) {
        self.store.borrow_mut().register_source(uri, xml);
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

    pub(super) fn load_uri(&self, uri: &str) -> Result<DocId, XsltError> {
        self.load_relative_to(&self.base_uri(), uri)
    }

    /// Loads the document `uri` names, resolved against `base` (§16.1).
    ///
    /// > If it is a relative URI reference, then it is resolved against the
    /// > base URI of `$base-node` if supplied, or against the base URI from the
    /// > static context otherwise (this will usually be the base URI of the
    /// > stylesheet module).
    pub(super) fn load_relative_to(&self, base: &str, uri: &str) -> Result<DocId, XsltError> {
        self.load_with_code(base, uri, codes::XTDE1170)
    }

    /// The load behind `document()` and `fn:doc` (§16.1), which report the
    /// failure with `fn:doc`'s own code.
    ///
    /// §16.1: the URI "is passed to the doc function … If an error occurs
    /// during evaluation of the doc function, the processor may either signal
    /// this error in the normal way, or may recover". F&O §15.5.4 (`fn:doc`):
    /// a resource that cannot be retrieved, or whose content cannot be parsed,
    /// raises FODC0002. XTDE1170 is `unparsed-text()`'s code (§16.2).
    pub(super) fn load_for_doc(&self, base: &str, uri: &str) -> Result<DocId, XsltError> {
        self.load_with_code(base, uri, "FODC0002")
    }

    /// Loads `uri` resolved against `base`, reporting a resource that cannot
    /// be read or parsed with `code`.
    fn load_with_code(&self, base: &str, uri: &str, code: &str) -> Result<DocId, XsltError> {
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
            None => self
                .resolver
                .resolve_text(&absolute)
                .map_err(|e| XsltError::new(code, format!("cannot read {absolute:?}: {e}")))?,
        };
        self.admit_with(Some(&absolute), &text, code)
    }
}

/// The first element or attribute at or below `node`, in document order, that
/// carries a type annotation.
///
/// `DomNavigator::type_annotation()` is the XDM *type-name* property: `None`
/// means the node was never schema-validated, which for an element is
/// `xs:untyped` and for an attribute `xs:untypedAtomic` — exactly the two
/// XTDE1665 permits. Every other node kind reports `None` unconditionally, so
/// only elements and their attributes are worth visiting.
fn first_annotated<'t>(node: &Nav<'t>) -> Option<Nav<'t>> {
    if matches!(
        node.node_type(),
        DomNodeType::Element | DomNodeType::Attribute
    ) && node.type_annotation().is_some()
    {
        return Some(node.clone());
    }
    if node.node_type() == DomNodeType::Element {
        let mut attr = node.clone();
        if attr.move_to_first_attribute() {
            loop {
                if attr.type_annotation().is_some() {
                    return Some(attr);
                }
                if !attr.move_to_next_attribute() {
                    break;
                }
            }
        }
    }
    let mut child = node.clone();
    if child.move_to_first_child() {
        loop {
            if child.node_type() == DomNodeType::Element {
                if let Some(found) = first_annotated(&child) {
                    return Some(found);
                }
            }
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    None
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
