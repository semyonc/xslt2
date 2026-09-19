//! Simple content (§5.7.2), attribute value templates (§5.6) and the
//! document an `xsl:message` builds (§17).

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
        self.pending_text
            .get_or_insert_with(String::new)
            .push_str(s);
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
