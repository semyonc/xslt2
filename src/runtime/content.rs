//! Simple content (§5.7.2), attribute value templates (§5.6) and the
//! document an `xsl:message` builds (§17).

use xsd_schema::document::BufferDocumentOptions;
use xsd_schema::navigator::DomNavigator;
use xsd_schema::types::XmlValue;
use xsd_schema::xpath::functions::atomize_sequence;
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::error::XsltError;
use crate::model::*;
use crate::receiver::{ComplexContentNormalizer, Name, Nav, Receiver, ResultBuilder};

impl<'a, 't> Transformation<'a, 't> {
    /// The string an atomized item contributes to simple content (§5.7.2
    /// step 4, "every value in the atomized sequence is cast to a string").
    ///
    /// `xs:QName` and `xs:NOTATION` need the name table to render, because a
    /// `QualifiedName` holds interned ids: upstream's `Display` prints a debug
    /// form, which would reach the result tree as an element name. Casting a
    /// QName to a string yields its lexical form, prefix and all.
    pub(super) fn atom_string(&self, value: &XmlValue) -> String {
        atom_to_string(self.sheet.names(), value)
    }

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
                    // §5.7.2: the same simple-content rules, with a single
                    // space as the separator.
                    let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                    let strings: Vec<String> = atoms.iter().map(|a| self.atom_string(a)).collect();
                    out.push_str(&strings.join(" "));
                }
            }
        }
        Ok(out)
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
                let mut atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
                if first_item_only {
                    atoms.truncate(1);
                }
                let strings: Vec<String> = atoms.iter().map(|a| self.atom_string(a)).collect();
                Ok(strings.join(&separator))
            }
            ValueSource::Content(body) => {
                let (arena, names) = {
                    let store = self.store.borrow();
                    (store.arena(), store.names())
                };
                let mut collector = SimpleContentCollector::new(arena, names);
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
/// sequence constructor *builds* is one item too, and atomizing it gives its
/// string value — which is the string value of the node §5.7.1 builds, where
/// adjacent atomic values are joined by spaces and comments and processing
/// instructions contribute nothing. Such a node is therefore built for real,
/// through the same [`ComplexContentNormalizer`] every other result tree goes
/// through, and only its string value is kept (`xsl-document-0601`).
/// Attribute and namespace nodes atomize to their values (`namespace-2602`).
struct SimpleContentCollector<'t> {
    arena: &'t bumpalo::Bump,
    names: &'t xsd_schema::namespace::NameTable,
    parts: Vec<String>,
    pending_text: Option<String>,
    /// The element or document node being built at the top level, if any.
    nested: Option<ComplexContentNormalizer<'t, ResultBuilder<'t>>>,
    /// How many containers of the nested node are open.
    depth: usize,
}

impl<'t> SimpleContentCollector<'t> {
    fn new(arena: &'t bumpalo::Bump, names: &'t xsd_schema::namespace::NameTable) -> Self {
        Self {
            arena,
            names,
            parts: Vec::new(),
            pending_text: None,
            nested: None,
            depth: 0,
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
        let builder = ResultBuilder::with_options(self.arena, self.names, None, options)?;
        self.nested = Some(ComplexContentNormalizer::new(builder));
        Ok(())
    }

    /// Closes the top-level node and contributes its string value.
    fn close(&mut self) -> Result<(), XsltError> {
        let Some(nested) = self.nested.take() else {
            return Ok(());
        };
        let doc = nested.into_inner().finish()?;
        let value = doc.create_navigator().value();
        self.push_atomic(value);
        Ok(())
    }

    fn nested(
        &mut self,
    ) -> Result<&mut ComplexContentNormalizer<'t, ResultBuilder<'t>>, XsltError> {
        self.nested
            .as_mut()
            .ok_or_else(|| XsltError::internal("simple content: no node is open"))
    }
}

impl<'t> Receiver<'t> for SimpleContentCollector<'t> {
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
        // A parentless attribute node atomizes to its value (§5.7.2 step 3).
        self.push_atomic(value.to_string());
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
                let text = atom_to_string(self.names, value);
                self.push_atomic(text);
            }
            XmlItem::Node(nav) => {
                let text = nav.value();
                if nav.node_type().is_text_like() {
                    // "Adjacent text nodes in the sequence are merged."
                    self.push_text(&text);
                } else {
                    self.push_atomic(text);
                }
            }
        }
        Ok(())
    }
    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: crate::receiver::CopyNamespaces,
    ) -> Result<(), XsltError> {
        if self.depth > 0 {
            return self.nested()?.append_copy(item, namespaces);
        }
        // A copy has the same string value as its original.
        self.append_item(item)
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
