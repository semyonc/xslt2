//! Simple content (§5.7.2), attribute value templates (§5.6) and the
//! document an `xsl:message` builds (§17).

use xsd_schema::document::BufferDocumentOptions;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::types::value::XmlValue;
use xsd_schema::validation::validate_simple_type;
use xsd_schema::xpath::functions::{atomize_sequence, XPathValue};
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::validate::Validating;
use super::{Frame, Transformation};
use crate::error::XsltError;
use crate::model::*;
use crate::receiver::{
    Boundary, ComplexContentNormalizer, CopyValidation, Name, Nav, Receiver, ResultBuilder,
};

impl<'a, 't> Transformation<'a, 't> {
    /// §5.6: the effective value of an attribute value template.
    pub(super) fn avt(
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
                    if avt.first_item_only {
                        // §9.10 row 6, §5.6: "After atomizing the result of the
                        // expression, all items other than the first item in
                        // the resulting sequence are discarded … If the
                        // atomized sequence is empty, the result is a
                        // zero-length string."
                        let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                        if let Some(first) = atoms.first() {
                            out.push_str(&first.to_string_value());
                        }
                    } else {
                        // §5.6: "This conversion is done using the rules given
                        // in 5.7.2 Constructing Simple Content" — "the separator
                        // is a single space character".
                        out.push_str(&self.simple_strings(value)?.join(" "));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Steps 1–4 of §5.7.2 over the value of an expression: the strings that
    /// the separator then joins.
    ///
    /// > 1. Zero-length text nodes in the sequence are discarded.
    /// > 2. Adjacent text nodes in the sequence are merged into a single text
    /// >    node.
    /// > 3. The sequence is atomized.
    /// > 4. Every value in the atomized sequence is cast to a string.
    ///
    /// So a run of adjacent text nodes is one string, never separated
    /// (`avt-1205`: a function returning the text nodes `[`, `0`, `]` makes
    /// `[0]`, not `[ 0 ]`), and every other item is atomized on its own. A
    /// sequence with no text node in it is atomized in one call, as before
    /// the merge existed.
    pub(super) fn simple_strings(
        &self,
        value: XPathValue<Nav<'t>>,
    ) -> Result<Vec<String>, XsltError> {
        let is_text = |item: &XmlItem<Nav<'t>>| matches!(item, XmlItem::Node(nav) if nav.node_type().is_text_like());
        if !value.as_slice().iter().any(is_text) {
            let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
            return Ok(atoms.iter().map(|a| a.to_string_value()).collect());
        }
        let mut strings = Vec::new();
        let mut pending: Option<String> = None;
        for item in value.into_vec() {
            if let XmlItem::Node(nav) = &item {
                if nav.node_type().is_text_like() {
                    pending
                        .get_or_insert_with(String::new)
                        .push_str(&nav.value());
                    continue;
                }
            }
            // Step 1 applies to the merged run: a run of zero-length text
            // nodes is discarded as a whole.
            if let Some(text) = pending.take().filter(|t| !t.is_empty()) {
                strings.push(text);
            }
            let atoms =
                atomize_sequence(XPathValue::from_item(item)).map_err(|e| self.xpath_error(e))?;
            strings.extend(atoms.iter().map(|a| a.to_string_value()));
        }
        if let Some(text) = pending.filter(|t| !t.is_empty()) {
            strings.push(text);
        }
        Ok(strings)
    }

    /// §5.7.2 Constructing Simple Content.
    pub(super) fn simple_content(
        &self,
        value: &ValueSource,
        separator: Option<String>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        first_item_only: bool,
        default: SimpleDefault,
    ) -> Result<String, XsltError> {
        let separator = separator.unwrap_or_else(|| default.text().to_string());
        // §19.1: "xsl:attribute, xsl:comment, xsl:processing-instruction,
        // xsl:namespace, xsl:value-of … always evaluate the instructions in
        // their contained sequence constructor in temporary output state."
        let cx = &*cx.temporary_output();
        match value {
            ValueSource::Select(id) => {
                let value = self.eval_expr(*id, frame, cx)?;
                if first_item_only {
                    // §11.4.3, §9.10 row 5: "all items in the atomized result
                    // sequence other than the first are ignored" — the first
                    // item atomized, as an XSLT 1.0 processor took the string
                    // value of the first node (so two text nodes of the
                    // sequence are not merged first).
                    let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                    return Ok(atoms
                        .first()
                        .map(|a| a.to_string_value())
                        .unwrap_or_default());
                }
                Ok(self.simple_strings(value)?.join(&separator))
            }
            ValueSource::Content(body) => {
                let mut collector = SimpleContentCollector::new(self);
                self.eval_body(body, frame, cx, &mut collector)?;
                collector.finish(&separator)
            }
        }
    }

    /// The document an `xsl:message` builds, serialized (§17).
    ///
    /// The `select` attribute behaves as an `xsl:copy-of` prepended to the
    /// sequence constructor, so both together make one document.
    pub(super) fn message_content(
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
        // §19.1: "… and xsl:message always evaluate the instructions in their
        // contained sequence constructor in temporary output state."
        let cx = &*cx.temporary_output();
        // The elements of the message are constructed like any others: a `strict` one is
        // validated (§19.2.1.1).
        let builder = ResultBuilder::new(arena, names, self.schema_set)?;
        let mut out = ComplexContentNormalizer::new(self.validating(builder));
        out.start_document()?;
        if let Some(id) = select {
            let value = self.eval_expr(id, frame, cx)?;
            for item in value.into_vec() {
                out.append_item(&item)?;
            }
        }
        self.eval_body(body, frame, cx, &mut out)?;
        out.end_document()?;
        let doc = out.into_inner().into_inner().finish()?;
        let doc = self.store.borrow().arena().alloc(doc);
        crate::receiver::serialize_document(doc)
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
    pub(super) fn of(value: &ValueSource) -> Self {
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
/// > 1. Zero-length text nodes in the sequence are discarded.
/// > 2. Adjacent text nodes in the sequence are merged into a single text node.
/// > 3. The sequence is atomized.
/// > 4. Every value in the atomized sequence is cast to a string.
/// > 5. The strings within the resulting sequence are concatenated, with a
/// >    (possibly zero-length) separator inserted between successive strings.
///
/// So the distinction the rules turn on is text node versus everything else:
/// adjacent text nodes merge into one string and are never separated, while
/// every other item is one string of its own. An element or document node the
/// sequence constructor *builds* is one item too, and step 3 atomizes it: such a
/// node is therefore built for real, through the same pipeline every other tree
/// of the transformation goes through — the [`ComplexContentNormalizer`] (§5.7.1:
/// adjacent atomic values joined by spaces) and the validating receiver, so a
/// `strict` element is validated (§19.2.1.1; decision D-d11 of
/// `XSLT2_M4D_REPORT.md`, reversed in its fix round) — and what it contributes is
/// its typed value, each atomic value cast to a string (`xsl-document-0601`): an
/// untyped node's string value, a validated element's value of its type — one
/// string per item of a list type, an element default it took included (a node
/// whose typed value is undefined, an element of element-only content, is
/// FOTY0012). A node that already exists, sequenced into the content, is
/// atomized the same way — its typed value, as through `select`
/// (`Transformation::simple_strings`) —, and a copy (`xsl:copy-of`, untyped:
/// `validation="strip"`) contributes its string value. A parentless attribute
/// atomizes to its typed value — the value of the type `xsl:attribute`
/// validated it to, one string per item of a list type — or, unvalidated, to
/// its string value; a namespace node to its URI (`namespace-2602`). Every
/// item that is not a text node ends the text before it, even when it atomizes
/// to nothing (`push_atoms`).
struct SimpleContentCollector<'r, 'a, 't> {
    run: &'r Transformation<'a, 't>,
    parts: Vec<String>,
    pending_text: Option<String>,
    /// The element or document node being built at the top level, if any.
    nested: Option<ComplexContentNormalizer<'t, Validating<'r, 't>>>,
    /// Whether that node is a document node.
    document: bool,
    /// How many containers of the nested node are open.
    depth: usize,
    /// The boundary announced for the next top-level node, before it is opened.
    pending_boundary: Option<Boundary>,
}

impl<'r, 'a, 't> SimpleContentCollector<'r, 'a, 't> {
    fn new(run: &'r Transformation<'a, 't>) -> Self {
        Self {
            run,
            parts: Vec::new(),
            pending_text: None,
            nested: None,
            document: false,
            depth: 0,
            pending_boundary: None,
        }
    }

    fn flush_text(&mut self) {
        if let Some(text) = self.pending_text.take() {
            // Step 1: a zero-length text node is discarded — after step 2's
            // merge it is the run as a whole that has to be empty.
            if !text.is_empty() {
                self.parts.push(text);
            }
        }
    }

    fn push_text(&mut self, s: &str) {
        self.pending_text
            .get_or_insert_with(String::new)
            .push_str(s);
    }

    fn push_atomic(&mut self, s: String) {
        self.flush_text();
        self.parts.push(s);
    }

    /// Contributes what a non-text item atomized to (§5.7.2 steps 3 and 4). The item ends the
    /// text before it even when it atomizes to nothing (a nilled element, an empty list, empty
    /// content): "Adjacent text nodes in the sequence are merged" — and text nodes with an item
    /// between them are not adjacent.
    fn push_atoms(&mut self, atoms: Vec<XmlValue>) {
        self.flush_text();
        self.parts
            .extend(atoms.iter().map(|atom| atom.to_string_value()));
    }

    fn finish(mut self, separator: &str) -> Result<String, XsltError> {
        self.flush_text();
        Ok(self.parts.join(separator))
    }

    /// Opens a node built at the top level: an element (`document = false`)
    /// or a document node.
    fn open(&mut self, document: bool) -> Result<(), XsltError> {
        let options = if document {
            BufferDocumentOptions::default()
        } else {
            BufferDocumentOptions::fragment()
        };
        let (arena, names) = {
            let store = self.run.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::with_options(arena, names, self.run.schema_set, options)?;
        let mut nested = ComplexContentNormalizer::new(self.run.validating(builder));
        if let Some(boundary) = self.pending_boundary.take() {
            nested.validation_boundary(boundary)?;
        }
        self.nested = Some(nested);
        self.document = document;
        Ok(())
    }

    /// Closes the top-level node and contributes its atomized value (§5.7.2 steps 3 and 4).
    fn close(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.take() else {
            return Ok(());
        };
        let doc = nested.into_inner().into_inner().finish()?;
        let mut node = doc.create_navigator();
        // An element is built in a fragment, under its root.
        if !self.document && !node.move_to_first_child() {
            return Err(XsltError::internal(
                "simple content: the element was not built",
            ));
        }
        let atoms = atomize_sequence(XPathValue::from_item(XmlItem::Node(node)))
            .map_err(|e| self.run.xpath_error(e))?;
        self.push_atoms(atoms);
        Ok(())
    }

    fn nested(
        &mut self,
    ) -> Result<&mut ComplexContentNormalizer<'t, Validating<'r, 't>>, XsltError> {
        self.nested
            .as_mut()
            .ok_or_else(|| XsltError::internal("simple content: no node is open"))
    }
}

impl<'t> Receiver<'t> for SimpleContentCollector<'_, '_, 't> {
    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.validation_boundary(boundary);
        }
        self.pending_boundary = Some(boundary);
        Ok(())
    }
    fn start_document(&mut self) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.open(true)?;
        }
        self.depth += 1;
        self.nested()?.start_document()
    }
    fn end_document(&mut self) -> Result<(), XsltError> {
        if self.depth == 0 {
            return Ok(());
        }
        self.nested()?.end_document()?;
        self.depth -= 1;
        if self.depth == 0 {
            self.close()?;
        }
        Ok(())
    }
    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<xsd_schema::document::NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        if self.depth == 0 {
            self.open(false)?;
        }
        self.depth += 1;
        self.nested()?.start_element(name, binding, nilled)
    }
    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.namespace(prefix, uri);
        }
        // A parentless namespace node atomizes to its string value, the URI.
        self.push_atomic(uri.to_string());
        Ok(())
    }
    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<xsd_schema::document::NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.attribute(name, value, binding);
        }
        // A parentless attribute node atomizes to its typed value (§5.7.2 step 3): an attribute
        // `xsl:attribute` validated (`type`, `strict`, `lax`) to the value of its type — one atomic
        // value per item of a list type —, an unvalidated one to its string value.
        let typed = match (binding, self.run.schema_set) {
            (Some(binding), Some(set)) => validate_simple_type(value, binding.type_key, set)
                .ok()
                .map(|result| result.typed_value),
            _ => None,
        };
        match typed {
            Some(typed) => {
                let atoms =
                    atomize_sequence(XPathValue::<Nav<'t>>::from_item(XmlItem::Atomic(typed)))
                        .map_err(|e| self.run.xpath_error(e))?;
                self.push_atoms(atoms);
            }
            None => self.push_atomic(value.to_string()),
        }
        Ok(())
    }
    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.end_start_tag();
        }
        Ok(())
    }
    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.no_inherit_namespaces();
        }
        Ok(())
    }
    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.text(s, disable_escaping);
        }
        self.push_text(s);
        Ok(())
    }
    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.comment(s);
        }
        self.push_atomic(s.to_string());
        Ok(())
    }
    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.pi(target, data);
        }
        self.push_atomic(data.to_string());
        Ok(())
    }
    fn end_element(&mut self) -> Result<(), XsltError> {
        if self.depth == 0 {
            return Err(XsltError::internal("end_element with no element open"));
        }
        self.nested()?.end_element()?;
        self.depth -= 1;
        if self.depth == 0 {
            self.close()?;
        }
        Ok(())
    }
    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.append_item(item);
        }
        match item {
            XmlItem::Atomic(value) => {
                // §5.7.2 step 4: "every value in the atomized sequence is
                // cast to a string" — for an xs:QName, its lexical form.
                self.push_atomic(value.to_string_value());
            }
            XmlItem::Node(nav) if nav.node_type().is_text_like() => {
                // "Adjacent text nodes in the sequence are merged."
                self.push_text(&nav.value());
            }
            XmlItem::Node(_) => {
                // Step 3: atomized — a typed node contributes its typed value, one string per
                // item, as through `select` (`simple_strings`).
                let atoms = atomize_sequence(XPathValue::from_item(item.clone()))
                    .map_err(|e| self.run.xpath_error(e))?;
                self.push_atoms(atoms);
            }
        }
        Ok(())
    }
    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: crate::receiver::CopyNamespaces,
        validation: CopyValidation,
    ) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.append_copy(item, namespaces, validation);
        }
        let XmlItem::Node(nav) = item else {
            return self.append_item(item);
        };
        if nav.node_type().is_text_like() {
            return self.append_item(item);
        }
        match (validation, nav.node_type()) {
            // `preserve`: the copy keeps the original's annotation, so it atomizes to the
            // original's typed value — as the original does (`append_item`).
            (CopyValidation::Preserve, _) => self.append_item(item),
            // `strict`: the copy of an element or document node is validated (§19.2.1.1,
            // §19.2.2), so it is built — through the transformation's pipeline, like a node the
            // content constructs — and contributes its typed value. (An attribute is validated
            // by the instruction and arrives through `attribute`.)
            (CopyValidation::Strict, kind @ (DomNodeType::Element | DomNodeType::Root)) => {
                self.open(kind == DomNodeType::Root)?;
                self.nested()?.copy_selected(nav, namespaces, validation)?;
                self.close()
            }
            // `strip` (and what validation does not apply to): untyped, so the typed value is the
            // string value of the original.
            _ => {
                self.push_atomic(nav.value());
                Ok(())
            }
        }
    }
}
