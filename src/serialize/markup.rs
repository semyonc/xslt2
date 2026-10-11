//! The `xml`, `xhtml` and `html` output methods (Serialization §5, §6, §7):
//! one writer, with the method's rules as switches.
//!
//! The walk is iterative — one frame per open element — so a deep result tree
//! cannot overflow the Rust stack.
//!
//! **Escaping** (§4 "character expansion", rule e; implementation-dependent
//! details chosen to match what the suite's expected files show):
//!
//! | Context | Written as |
//! |---|---|
//! | text | `&` `<` `>` as `&amp;` `&lt;` `&gt;`; CR as `&#xD;` |
//! | attribute value (`"`-quoted) | `&` `<` `>` `"` as `&amp;` `&lt;` `&gt;` `&#34;`; TAB LF CR as `&#x9;` `&#xA;` `&#xD;` |
//! | attribute value (`'`-quoted: a character map wrote `"` into it, see below) | as above, but `'` as `&#39;` and `"` as it is |
//! | HTML attribute value | as above, but `<` and an `&` before `{` are written as they are (§7.2) |
//! | text and attributes, XML and XHTML | #x7F–#x9F and U+2028 as `&#N;` (§5: "the non-whitespace control characters #x1 through #x1F and #x7F through #x9F in text nodes and attribute nodes MUST be output as character references") |
//! | text and attributes, any method | a character the encoding cannot hold as `&#N;` (§5.1.2, §7.4.2) |
//! | names, comments, processing instructions, HTML `script`/`style` content, a character-map string | a character the encoding cannot hold is SERE0008 |
//! | anything from the tree, HTML method | #x7F–#x9F is SERE0014 (§7.3) — text, attributes, comments, processing instructions, `disable-output-escaping` text, CDATA in XML islands, URI attributes before URI escaping |
//!
//! Character maps (§9) are applied before normalization and escaping, and what
//! they substitute is written as it is. CDATA sections (§5.1.4) and
//! `disable-output-escaping` (§20.2) bypass both, but not Unicode
//! normalization; a CDATA section is closed around a character that text
//! writes as a reference (the rows above), and that character is written as
//! the reference.
//!
//! **Attribute delimiters.** Serialization §3 leaves the choice open ("Whether
//! a serializer uses apostrophes or quotation marks to delimit attribute values
//! in the XML output method is an example of such a detail"); values are
//! written between `"` unless a character map writes a `"` and no `'` into the
//! value — XSLT 2.0 §20.1, after the JSP example: "This works on the assumption
//! that when an apostrophe or quotation mark is generated as part of an
//! attribute value by the use of character maps, the serializer will (where
//! possible) use the other choice of delimiter around the attribute value."
//! When the mapped strings hold both, no delimiter is possible and `"` stays:
//! Serialization §9 "The string that is substituted is output "as is," and the
//! serializer performs no checks that the resulting document is well-formed"
//! (its note names "attributes that contain … unescaped quotation marks").

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};

use super::codes::{SERE0006, SERE0014, SERE0015};
use super::html;
use super::params::XHTML_NS;
use super::{unrepresentable, EscapingMarks, ResolvedMethod, SerializationParams};
use crate::error::XsltError;
use crate::names::QName;
use crate::whitespace;

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Spaces per indentation level.
const INDENT: usize = 3;

/// Writes the document at `root` with the `xml`, `xhtml` or `html` method.
pub(crate) fn write_document<N: DomNavigator>(
    root: &N,
    params: &SerializationParams,
    marks: Option<&EscapingMarks>,
    out: &mut String,
) -> Result<(), XsltError> {
    Writer {
        p: params,
        marks,
        out,
        ns: Scope::default(),
        text_ordinal: 0,
        doctype_pending: params.doctype_system.is_some()
            || (params.method == ResolvedMethod::Html && params.doctype_public.is_some()),
        html: params.method == ResolvedMethod::Html,
        xhtml: params.method == ResolvedMethod::Xhtml,
    }
    .document(root)
}

/// What a container may receive in the way of added whitespace (§5.1.3,
/// §6.1.3, §7.4.3), and what its children inherit.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    /// Breaks may be added at this container's child boundaries.
    enabled: bool,
    /// Mixed content (a text child with a non-whitespace character) here or
    /// in an ancestor, an inline or a formatted element: no layout anywhere
    /// below.
    suppressed: bool,
    /// The effective `xml:space="preserve"` (§5.1.3: "Whitespace characters
    /// MUST NOT be inserted in a part of the result document that is controlled
    /// by an xml:space attribute with value preserve").
    preserve: bool,
}

/// What the previous child of a container was, for the layout rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Child {
    /// An element after which (and before which) whitespace may be added.
    Block,
    /// An inline element: "Whitespace MUST NOT be added or removed adjacent to
    /// an inline element".
    Inline,
    /// A whitespace-only text node of a container with layout that is written
    /// as it is (`Writer::replaceable_whitespace` says when): nothing is added
    /// next to it.
    Space,
    /// A text node, comment or processing instruction.
    Other,
}

/// One open element, or the document node at the bottom of the stack.
struct Frame<N> {
    /// The element; `None` for the document node.
    node: Option<N>,
    /// Where this element's namespace declarations start on the scope stack.
    ns_mark: usize,
    /// The element's default namespace in the tree — its own `xmlns`
    /// declaration, else its parent's; empty for none (or `xmlns=""`).
    tree_default: String,
    /// The end tag's name.
    name: String,
    layout: Layout,
    prev: Option<Child>,
    /// HTML `script` / `style`: "The HTML output method MUST NOT perform
    /// escaping for the content of the script and style elements" (§7.1).
    raw: bool,
    /// Text children are written as CDATA sections (§5.1.4).
    cdata: bool,
    /// A `head` that received the content-type `meta`: existing
    /// `meta http-equiv="Content-Type"` children are dropped (§6.1.13, §7.4.13).
    drop_content_type_meta: bool,
}

/// How the method sees one element.
#[derive(Clone, Copy, Debug, Default)]
struct Kind {
    /// Inline for the layout rules (HTML/XHTML only).
    inline: bool,
    /// pre, script, style, textarea (HTML/XHTML only).
    formatted: bool,
    /// An element with an EMPTY content model in the method's vocabulary.
    empty_model: bool,
    /// An HTML element of the `html` method (no namespace) or an XHTML element
    /// of the `xhtml` method (XHTML namespace): the one the method's own rules
    /// (URI attributes, boolean attributes, `head`) apply to.
    vocabulary: bool,
    /// `script` or `style` with the `html` method.
    raw: bool,
}

/// The namespace bindings written so far, per prefix innermost last, with an
/// undo log, so that a lookup and an element's going out of scope cost no
/// more than the declarations involved, however deep the tree.
#[derive(Default)]
struct Scope {
    bindings: HashMap<String, Vec<String>>,
    /// The prefixes in the order they were bound.
    log: Vec<String>,
}

impl Scope {
    fn mark(&self) -> usize {
        self.log.len()
    }

    fn push(&mut self, prefix: String, uri: String) {
        self.bindings.entry(prefix.clone()).or_default().push(uri);
        self.log.push(prefix);
    }

    /// Unbinds everything bound since `mark`.
    fn truncate(&mut self, mark: usize) {
        while self.log.len() > mark {
            let prefix = self.log.pop().expect("checked");
            if let Some(uris) = self.bindings.get_mut(&prefix) {
                uris.pop();
            }
        }
    }

    fn get(&self, prefix: &str) -> Option<&str> {
        self.bindings
            .get(prefix)
            .and_then(|uris| uris.last())
            .map(String::as_str)
    }
}

struct Writer<'a, 'o> {
    p: &'a SerializationParams,
    marks: Option<&'a EscapingMarks>,
    out: &'o mut String,
    /// The `(prefix, uri)` bindings written so far; an empty prefix with an
    /// empty URI is an undeclared default namespace.
    ns: Scope,
    /// The ordinal of the next text node in document order (the key of
    /// [`EscapingMarks`]).
    text_ordinal: u32,
    /// A document type declaration still has to be written before the first
    /// element.
    doctype_pending: bool,
    html: bool,
    xhtml: bool,
}

impl<'a, 'o> Writer<'a, 'o> {
    fn document<N: DomNavigator>(mut self, root: &N) -> Result<(), XsltError> {
        let declaration = !self.html && !self.p.omit_xml_declaration;
        if declaration {
            // §5.1.5: "The XML declaration MUST include both version information
            // and an encoding declaration. If the standalone parameter has the
            // value yes or the value no, the XML declaration MUST include a
            // standalone document declaration".
            let _ = write!(
                self.out,
                "<?xml version=\"{}\" encoding=\"{}\"",
                self.p.version, self.p.encoding_name
            );
            match self.p.standalone {
                Some(true) => self.out.push_str(" standalone=\"yes\""),
                Some(false) => self.out.push_str(" standalone=\"no\""),
                None => {}
            }
            self.out.push_str("?>");
        }
        let layout = self.content_layout(root, Layout::default(), None, Kind::default(), false);
        let mut stack: Vec<Frame<N>> = vec![Frame {
            node: None,
            ns_mark: 0,
            tree_default: String::new(),
            name: String::new(),
            layout,
            // The declaration stands in for the document's opening boundary.
            prev: declaration.then_some(Child::Other),
            raw: false,
            cdata: false,
            drop_content_type_meta: false,
        }];
        let mut cursor = first_child(root);
        loop {
            let Some(node) = cursor else {
                // The container's children are done.
                if stack.len() == 1 {
                    return Ok(());
                }
                let frame = stack.pop().expect("checked");
                if frame.layout.enabled && frame.prev == Some(Child::Block) {
                    self.newline(stack.len() - 1);
                }
                self.out.push_str("</");
                self.out.push_str(&frame.name);
                self.out.push('>');
                self.ns.truncate(frame.ns_mark);
                cursor = next_sibling(frame.node.as_ref().expect("an element frame"));
                continue;
            };
            let depth = stack.len() - 1;
            match node.node_type() {
                DomNodeType::Element => {
                    let parent = stack.last().expect("the document frame");
                    if parent.drop_content_type_meta && self.is_content_type_meta(&node) {
                        self.text_ordinal += count_text_nodes(&node);
                        cursor = next_sibling(&node);
                        continue;
                    }
                    let kind = self.kind(&node);
                    let child = if kind.inline {
                        Child::Inline
                    } else {
                        Child::Block
                    };
                    self.boundary(stack.last_mut().expect("frame"), child, depth);
                    if self.doctype_pending {
                        self.doctype(&node)?;
                    }
                    let parent = stack.last().expect("frame");
                    let parent_layout = parent.layout;
                    let ns_mark = self.ns.mark();
                    let name = qualified(node.prefix(), node.local_name());
                    self.check_name(&name)?;
                    self.out.push('<');
                    self.out.push_str(&name);
                    let tree_default = self.namespace_declarations(&node, &parent.tree_default)?;
                    let space = self.attributes(&node, kind)?;
                    let meta = self.content_type_meta(&node, kind);
                    let first = first_child(&node);
                    if first.is_none() && meta.is_none() {
                        self.empty_element(&name, &node, kind);
                        self.ns.truncate(ns_mark);
                        cursor = next_sibling(&node);
                        continue;
                    }
                    self.out.push('>');
                    let cdata = self.is_cdata_element(&node);
                    let layout = self.content_layout(&node, parent_layout, space, kind, cdata);
                    let mut frame = Frame {
                        node: Some(node.clone()),
                        ns_mark,
                        tree_default,
                        name,
                        layout,
                        prev: None,
                        raw: kind.raw,
                        cdata,
                        drop_content_type_meta: meta.is_some(),
                    };
                    if let Some((meta, content)) = meta {
                        self.boundary(&mut frame, Child::Block, depth + 1);
                        // The element is added to the tree before character
                        // expansion (§4, phase 1), so its attribute values are
                        // character-mapped and escaped like any other.
                        let _ = write!(self.out, "<{meta} http-equiv=");
                        self.attribute_value("Content-Type")?;
                        self.out.push_str(" content=");
                        self.attribute_value(&content)?;
                        self.out.push_str(if self.xhtml { " />" } else { ">" });
                    }
                    stack.push(frame);
                    cursor = first;
                    continue;
                }
                kind if kind.is_text_like() => {
                    let ordinal = self.text_ordinal;
                    self.text_ordinal += 1;
                    let value = node.value();
                    // A container with layout has no text but whitespace-only
                    // text without a character-mapped character
                    // (`content_layout`).
                    let frame = stack.last().expect("frame");
                    let child = if frame.layout.enabled {
                        let next = next_sibling(&node).map(|n| self.child_kind(&n));
                        if self.replaceable_whitespace(frame, next) {
                            cursor = next_sibling(&node);
                            continue;
                        }
                        Child::Space
                    } else {
                        Child::Other
                    };
                    let frame = stack.last_mut().expect("frame");
                    frame.prev = Some(child);
                    let (raw, cdata) = (frame.raw, frame.cdata);
                    self.text(&value, ordinal, raw, cdata)?;
                }
                DomNodeType::Comment => {
                    self.boundary(stack.last_mut().expect("frame"), Child::Other, depth);
                    let value = node.value();
                    self.check_unescapable(&value, "a comment")?;
                    self.check_html_controls(&value, "a comment")?;
                    self.out.push_str("<!--");
                    self.out.push_str(&value);
                    self.out.push_str("-->");
                }
                DomNodeType::ProcessingInstruction => {
                    self.boundary(stack.last_mut().expect("frame"), Child::Other, depth);
                    self.processing_instruction(node.local_name(), &node.value())?;
                }
                _ => {}
            }
            cursor = next_sibling(&node);
        }
    }

    // ── Layout ───────────────────────────────────────────────────────

    /// The layout an element's (or the document's) content gets.
    ///
    /// The rules, per method:
    ///
    /// * XML (§5.1.3): "Whitespace characters MUST NOT be added adjacent to a
    ///   text node that contains non-whitespace characters." "Whitespace
    ///   characters MUST NOT be added other than adjacent to an element node,
    ///   that is, immediately before a start tag or immediately after an end
    ///   tag." "Whitespace characters MUST NOT be inserted in a part of the
    ///   result document that is controlled by an xml:space attribute with
    ///   value preserve." "In any location where the above rules allow the
    ///   addition of whitespace characters, existing whitespace characters MAY
    ///   also be removed or replaced."
    /// * XHTML (§6.1.3) and HTML (§7.4.3): the serializer "MAY add or remove
    ///   whitespace … Whitespace MUST NOT be added other than before or after
    ///   an element, or adjacent to an existing whitespace character.
    ///   Whitespace MUST NOT be added or removed adjacent to an inline element
    ///   … Whitespace MUST NOT be added or removed inside a formatted element".
    ///
    /// Conservative on purpose: mixed content — a text child with a
    /// non-whitespace character — turns layout off for the whole subtree, so
    /// nothing is added or removed anywhere in it, and so do an inline or
    /// formatted HTML/XHTML element. A whitespace-only text child is not mixed
    /// content: the container keeps its layout, and the whitespace is replaced
    /// by the layout's own where the rules allow that and written as it is
    /// otherwise (`replaceable_whitespace`) — unless the character map
    /// replaces one of its characters (`has_mixed_text_child`).
    ///
    /// `cdata`: `node` is a `cdata-section-elements` element.
    fn content_layout<N: DomNavigator>(
        &self,
        node: &N,
        parent: Layout,
        space: Option<bool>,
        kind: Kind,
        cdata: bool,
    ) -> Layout {
        let indent = self.p.indent;
        let preserve = space.unwrap_or(parent.preserve);
        let suppressed = parent.suppressed
            || kind.inline
            || kind.formatted
            || (indent && self.has_mixed_text_child(node, cdata));
        Layout {
            enabled: indent && !suppressed && !preserve,
            suppressed,
            preserve,
        }
    }

    /// Whether `node` (an element, or the document node) has mixed content: a
    /// text child that is not whitespace once character expansion is done.
    ///
    /// Indentation comes after character expansion (Ser. §4: rule c, "Apply
    /// character mapping as determined by the use-character-maps parameter",
    /// then "Indentation, as controlled by the indent parameter, MAY add or
    /// remove whitespace"), so a whitespace character the character map
    /// replaces is not whitespace to the layout: the text node is kept, and
    /// nothing is added next to it (§5.1.3: "Whitespace characters MUST NOT be
    /// added adjacent to a text node that contains non-whitespace
    /// characters"). Characters that are never mapped do not count: those of a
    /// `cdata-section-elements` element (`cdata`) and those written with
    /// escaping disabled — Ser. §9: "Character mapping is not applied to
    /// characters in text nodes whose parent elements are listed in the
    /// cdata-section-elements parameter, nor to characters for which output
    /// escaping has been disabled". The whitespace the layout itself writes is
    /// never mapped.
    ///
    /// The `disable-output-escaping` marks are keyed by text-node ordinal, and
    /// `self.text_ordinal` is that of the first text node of `node`'s subtree;
    /// the ordinals of later text children are counted (through the element
    /// children before them) only when a mapped whitespace character and marks
    /// make them matter.
    fn has_mixed_text_child<N: DomNavigator>(&self, node: &N, cdata: bool) -> bool {
        let maps = !cdata && !self.p.character_map.is_empty();
        let mapped = |run: &str| run.chars().any(|c| self.p.character_map.get(c).is_some());
        let marks = self.marks.filter(|marks| !marks.is_empty());
        // The ordinal of the next text child, once the text nodes of the
        // element children in `uncounted` are added.
        let mut ordinal = self.text_ordinal;
        let mut uncounted: Vec<N> = Vec::new();
        let mut child = first_child(node);
        while let Some(c) = child {
            let node_type = c.node_type();
            if node_type.is_text_like() {
                let value = c.value_ref();
                if !whitespace::is_all_xml_whitespace(&value) {
                    return true;
                }
                if maps && mapped(&value) {
                    let Some(marks) = marks else {
                        return true;
                    };
                    for element in uncounted.drain(..) {
                        ordinal += count_text_nodes(&element);
                    }
                    let segments = marks.segments(ordinal, &value);
                    if segments
                        .iter()
                        .any(|&(run, disabled)| !disabled && mapped(run))
                    {
                        return true;
                    }
                }
                ordinal += 1;
            } else if maps && marks.is_some() && node_type == DomNodeType::Element {
                uncounted.push(c.clone());
            }
            child = next_sibling(&c);
        }
        false
    }

    /// The boundary before a child of `kind` inside `frame`: a line break where
    /// the layout rules allow one (§5.1.3: "immediately before a start tag or
    /// immediately after an end tag"; §6.1.3, §7.4.3: "before or after an
    /// element", never next to an inline element), and never next to
    /// whitespace that was kept as it is.
    fn boundary<N>(&mut self, frame: &mut Frame<N>, kind: Child, depth: usize) {
        let prev = frame.prev.replace(kind);
        if !frame.layout.enabled {
            return;
        }
        let allowed = match prev {
            None => kind == Child::Block && frame.node.is_some(),
            Some(prev) => {
                (kind == Child::Block && !matches!(prev, Child::Inline | Child::Space))
                    || (prev == Child::Block && kind != Child::Inline)
            }
        };
        if allowed {
            self.newline(depth);
        }
    }

    /// How the layout rules see a child node.
    fn child_kind<N: DomNavigator>(&self, node: &N) -> Child {
        match node.node_type() {
            DomNodeType::Element if self.kind(node).inline => Child::Inline,
            DomNodeType::Element => Child::Block,
            _ => Child::Other,
        }
    }

    /// Whether a whitespace-only text child of `frame` (a container with
    /// layout), between the child before it (`frame.prev`) and `next`, is
    /// left out so that the layout's own line breaks stand in its place.
    ///
    /// Only where whitespace may be added — next to a block-level element
    /// (§5.1.3 "immediately before a start tag or immediately after an end
    /// tag"; §6.1.3, §7.4.3 "before or after an element") — and may be
    /// removed: never next to an inline element ("MUST NOT be added or removed
    /// adjacent to an inline element"). A text child of a
    /// `cdata-section-elements` element keeps its CDATA section. Whitespace at
    /// the top level, outside the document element, is kept: the layout puts
    /// no break before the first element, so there is nothing to replace it
    /// with.
    fn replaceable_whitespace<N>(&self, frame: &Frame<N>, next: Option<Child>) -> bool {
        let prev = frame.prev;
        let next_to_element = prev == Some(Child::Block) || next == Some(Child::Block);
        let next_to_inline = prev == Some(Child::Inline) || next == Some(Child::Inline);
        frame.node.is_some() && !frame.cdata && next_to_element && !next_to_inline
    }

    fn newline(&mut self, depth: usize) {
        self.out.push('\n');
        for _ in 0..depth * INDENT {
            self.out.push(' ');
        }
    }

    // ── Elements ─────────────────────────────────────────────────────

    /// How the method sees the element at `node`.
    fn kind<N: DomNavigator>(&self, node: &N) -> Kind {
        let (uri, local) = (node.namespace_uri(), node.local_name());
        if self.html && uri.is_empty() {
            let lower = local.to_ascii_lowercase();
            let known = html::is_html_element(&lower);
            return Kind {
                // §7.1: an unknown element in no namespace is written "in the
                // same way as a non-empty, inline element such as span".
                inline: !known
                    || html::is_inline_element(&lower)
                    || (matches!(lower.as_str(), "ins" | "del") && !has_element_child(node)),
                formatted: html::is_formatted_element(&lower),
                empty_model: html::is_empty_element(&lower),
                vocabulary: true,
                raw: matches!(lower.as_str(), "script" | "style"),
            };
        }
        if self.xhtml && uri == XHTML_NS {
            return Kind {
                inline: html::is_inline_element(local)
                    || (matches!(local, "ins" | "del") && !has_element_child(node)),
                formatted: html::is_formatted_element(local),
                empty_model: html::is_empty_element(local),
                vocabulary: true,
                raw: false,
            };
        }
        // XML, and an element in a namespace under the html method — an XML
        // island, written "in the same way as … a div element" (§7.1).
        Kind::default()
    }

    /// The tag of an element with no children.
    ///
    /// * XML, and an XML island in HTML: `<a/>`.
    /// * XHTML (§6): "If an element that has no children is an XHTML element
    ///   with an EMPTY content model, the serializer MUST use the minimized tag
    ///   syntax, for example `<br />` … MUST include a space before the
    ///   trailing `/>`"; any other element is written `<p></p>`, which is what
    ///   §6 requires of the XHTML elements ("MUST NOT use the minimized form")
    ///   and what the suite's expected files show for the others.
    /// * HTML (§7.1): "The HTML output method MUST NOT output an end-tag for an
    ///   empty element if the element type has an empty content model"; any
    ///   other HTML element gets its end tag.
    fn empty_element<N: DomNavigator>(&mut self, name: &str, node: &N, kind: Kind) {
        if self.xhtml {
            if kind.vocabulary && kind.empty_model {
                self.out.push_str(" />");
            } else {
                let _ = write!(self.out, "></{name}>");
            }
        } else if self.html && node.namespace_uri().is_empty() {
            self.out.push('>');
            if !kind.empty_model {
                let _ = write!(self.out, "</{name}>");
            }
        } else {
            self.out.push_str("/>");
        }
    }

    /// Whether text children of `node` are written as CDATA sections.
    ///
    /// §5.1.4: "If the expanded QName of the parent of a text node is a member
    /// of the list, then the text node MUST be output as a CDATA section". For
    /// the HTML method the parameter "is not applicable … except in the case of
    /// XML Islands" (§7.4.4).
    fn is_cdata_element<N: DomNavigator>(&self, node: &N) -> bool {
        if self.p.cdata_section_elements.is_empty()
            || (self.html && node.namespace_uri().is_empty())
        {
            return false;
        }
        let (uri, local) = (node.namespace_uri(), node.local_name());
        self.p
            .cdata_section_elements
            .iter()
            .any(|q: &QName| q.uri == uri && q.local == local)
    }

    /// Writes the declarations this element introduces: its in-scope bindings
    /// minus what is already bound identically, plus `xmlns=""` when it leaves
    /// an inherited default namespace; sorted by prefix, the default first.
    /// A prefixed binding to the empty URI is an XML 1.1 undeclaration, which
    /// XML 1.0 cannot write (§5.1.7), so it is never written. Returns the
    /// element's default namespace in the tree (empty for none), given its
    /// parent's.
    ///
    /// Only the element's **own** declarations are read
    /// (`NamespaceAxisScope::Local`): every binding it inherits is in scope at
    /// its parent, and the parent's start tag left each of those bound
    /// identically in the writer's scope, so none of them can need a
    /// declaration here. That keeps the cost per element independent of the
    /// tree's depth — the in-scope set (`ExcludeXml`) is gathered from every
    /// ancestor — and gives the same declarations.
    fn namespace_declarations<N: DomNavigator>(
        &mut self,
        node: &N,
        parent_default: &str,
    ) -> Result<String, XsltError> {
        let mut own: Vec<(String, String)> = Vec::new();
        let mut ns = node.clone();
        if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
            loop {
                let prefix = ns.local_name();
                // The first declaration of a prefix is the one in scope, as on
                // the in-scope axis.
                if prefix != "xml" && !own.iter().any(|(p, _)| p == prefix) {
                    own.push((prefix.to_string(), ns.value()));
                }
                if !ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                    break;
                }
            }
        }
        let tree_default = match own.iter().find(|(p, _)| p.is_empty()) {
            Some((_, uri)) => uri.clone(),
            None => parent_default.to_string(),
        };
        let mut declared: Vec<(String, String)> = own
            .into_iter()
            .filter(|(prefix, uri)| !uri.is_empty() && !self.is_bound(prefix, uri))
            .collect();
        if tree_default.is_empty() && matches!(self.in_scope(""), Some(uri) if !uri.is_empty()) {
            declared.push((String::new(), String::new()));
        }
        declared.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (prefix, uri) in declared {
            self.check_name(&prefix)?;
            if prefix.is_empty() {
                self.out.push_str(" xmlns=\"");
            } else {
                let _ = write!(self.out, " xmlns:{prefix}=\"");
            }
            self.escape_attribute(&uri, '"')?;
            self.out.push('"');
            self.ns.push(prefix, uri);
        }
        Ok(tree_default)
    }

    fn in_scope(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(XML_NS);
        }
        self.ns.get(prefix)
    }

    fn is_bound(&self, prefix: &str, uri: &str) -> bool {
        match self.in_scope(prefix) {
            Some(bound) => bound == uri,
            None => prefix.is_empty() && uri.is_empty(),
        }
    }

    /// Writes the attributes of `node`; returns its `xml:space`, if any.
    fn attributes<N: DomNavigator>(
        &mut self,
        node: &N,
        kind: Kind,
    ) -> Result<Option<bool>, XsltError> {
        let mut space = None;
        let mut attr = node.clone();
        if !attr.move_to_first_attribute() {
            return Ok(None);
        }
        let element_name = self.vocabulary_name(node.local_name());
        loop {
            let (uri, local) = (
                attr.namespace_uri().to_string(),
                attr.local_name().to_string(),
            );
            let value = attr.value();
            if uri == XML_NS && local == "space" {
                space = match value.as_str() {
                    "preserve" => Some(true),
                    "default" => Some(false),
                    _ => None,
                };
            }
            let name = qualified(attr.prefix(), &local);
            self.check_name(&name)?;
            self.out.push(' ');
            self.out.push_str(&name);
            let own = kind.vocabulary && uri.is_empty();
            let attribute_name = self.vocabulary_name(&local);
            if self.html
                && own
                && html::is_boolean_attribute(&attribute_name)
                && value.eq_ignore_ascii_case(&local)
            {
                // §7.2: "The HTML output method MUST output boolean attributes
                // (that is attributes with only a single allowed value that is
                // equal to the name of the attribute) in minimized form."
            } else {
                self.out.push('=');
                if own
                    && self.p.escape_uri_attributes
                    && html::is_uri_attribute(&element_name, &attribute_name)
                {
                    // §6.1.12, §7.4.12: URI escaping, then HTML/XML escaping;
                    // §9: no character mapping for such a value. URI escaping
                    // turns a control character into `%HH`, but it is in the
                    // tree all the same (§7.3).
                    self.check_html_controls(&value, "an attribute value")?;
                    let escaped = html::escape_html_uri(&value);
                    self.out.push('"');
                    self.escape_attribute(&escaped, '"')?;
                    self.out.push('"');
                } else {
                    self.attribute_value(&value)?;
                }
            }
            if !attr.move_to_next_attribute() {
                break;
            }
        }
        Ok(space)
    }

    /// The content-type `meta` element to add as the first child of `node`,
    /// if `node` is the `head` it goes into (§6.1.13, §7.4.13): "If there is a
    /// head element, and the include-content-type parameter has the value yes,
    /// the HTML output method MUST add a meta element as the first child
    /// element of the head element specifying the character encoding actually
    /// used." The content is the media type: "The content type MUST be set to
    /// the value given for the media-type parameter."
    ///
    /// Returns the `meta` element's name and its `content` value.
    fn content_type_meta<N: DomNavigator>(&self, node: &N, kind: Kind) -> Option<(String, String)> {
        if !self.p.include_content_type || !kind.vocabulary {
            return None;
        }
        if self.vocabulary_name(node.local_name()) != "head" {
            return None;
        }
        let name = qualified(node.prefix(), "meta");
        let content = format!("{}; charset={}", self.p.media_type, self.p.encoding_name);
        Some((name, content))
    }

    /// An existing `meta http-equiv="Content-Type"` child of a `head` that
    /// received the added one: "any existing meta element child of the head
    /// element having an http-equiv attribute with the value "Content-Type",
    /// making the comparison without consideration of case and leading or
    /// trailing spaces, MUST be discarded." The comparison without case is the
    /// attribute value's; the element and attribute names are compared as the
    /// method compares its vocabulary (`vocabulary_name`), so with XHTML a
    /// `META` element or an `HTTP-EQUIV` attribute is kept.
    fn is_content_type_meta<N: DomNavigator>(&self, node: &N) -> bool {
        let kind = self.kind(node);
        if !kind.vocabulary || self.vocabulary_name(node.local_name()) != "meta" {
            return false;
        }
        let mut attr = node.clone();
        if attr.move_to_first_attribute() {
            loop {
                if attr.namespace_uri().is_empty()
                    && self.vocabulary_name(attr.local_name()) == "http-equiv"
                    && attr
                        .value()
                        .trim_matches(' ')
                        .eq_ignore_ascii_case("Content-Type")
                {
                    return true;
                }
                if !attr.move_to_next_attribute() {
                    break;
                }
            }
        }
        false
    }

    /// A name of the method's vocabulary — an HTML or XHTML element or
    /// attribute name — as the HTML/XHTML tables (`html.rs`) and the name
    /// tests here compare it. The HTML method "MUST recognize the names of HTML
    /// elements regardless of case" (§7.1), so the name is folded to lower
    /// case; XHTML is XML (§6: "The serialization of the instance of the data
    /// model follows the same rules as for the XML output method"), whose names
    /// are case-sensitive, so the name is taken as it is and only the
    /// lower-case names are XHTML's: `META` in the XHTML namespace is not the
    /// `meta` element, nor `HREF` the `href` attribute.
    fn vocabulary_name<'n>(&self, name: &'n str) -> Cow<'n, str> {
        if self.html {
            Cow::Owned(name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(name)
        }
    }

    /// The document type declaration, immediately before the first element.
    ///
    /// XML and XHTML (§5.1.6): "The name following <!DOCTYPE MUST be the name
    /// of the first element … If the doctype-public parameter is also
    /// specified, then the XML output method MUST output PUBLIC followed by the
    /// public identifier and then the system identifier; otherwise, it MUST
    /// output SYSTEM followed by the system identifier. The internal subset MUST
    /// be empty. The doctype-public parameter MUST be ignored unless the
    /// doctype-system parameter is specified."
    ///
    /// HTML (§7.4.6): "The name following <!DOCTYPE MUST be HTML or html. If the
    /// doctype-public parameter is specified, then the output method MUST output
    /// PUBLIC followed by the specified public identifier; if the doctype-system
    /// parameter is also specified, it MUST also output the specified system
    /// identifier following the public identifier. If the doctype-system
    /// parameter is specified but the doctype-public parameter is not
    /// specified, then the output method MUST output SYSTEM followed by the
    /// specified system identifier."
    fn doctype<N: DomNavigator>(&mut self, first: &N) -> Result<(), XsltError> {
        self.doctype_pending = false;
        let name = if self.html {
            "html".to_string()
        } else {
            qualified(first.prefix(), first.local_name())
        };
        let mut decl = format!("<!DOCTYPE {name}");
        match (&self.p.doctype_public, &self.p.doctype_system) {
            (Some(public), Some(system)) => {
                let _ = write!(decl, " PUBLIC \"{public}\" {}", quoted(system));
            }
            (Some(public), None) if self.html => {
                let _ = write!(decl, " PUBLIC \"{public}\"");
            }
            (_, Some(system)) => {
                let _ = write!(decl, " SYSTEM {}", quoted(system));
            }
            _ => return Ok(()),
        }
        decl.push_str(">\n");
        self.check_unescapable(&decl, "the document type declaration")?;
        self.out.push_str(&decl);
        Ok(())
    }

    fn processing_instruction(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.check_name(target)?;
        self.check_unescapable(data, "a processing instruction")?;
        self.check_html_controls(data, "a processing instruction")?;
        self.out.push_str("<?");
        self.out.push_str(target);
        if !data.is_empty() {
            self.out.push(' ');
            self.out.push_str(data);
        }
        if self.html {
            // §7.3: "The HTML output method MUST terminate processing
            // instructions with > rather than ?>. It is a serialization error
            // [err:SERE0015] to use the HTML output method when > appears
            // within a processing instruction".
            if data.contains('>') {
                return Err(XsltError::new(
                    SERE0015,
                    format!(
                        "the processing instruction {target:?} contains '>', which ends a \
                         processing instruction in HTML"
                    ),
                ));
            }
            self.out.push('>');
        } else {
            self.out.push_str("?>");
        }
        Ok(())
    }

    // ── Character data ───────────────────────────────────────────────

    /// A text node: its `disable-output-escaping` segments as they are (but
    /// normalized), the rest as raw HTML content, a CDATA section or escaped
    /// text.
    fn text(&mut self, value: &str, ordinal: u32, raw: bool, cdata: bool) -> Result<(), XsltError> {
        let segments = match self.marks {
            Some(marks) => marks.segments(ordinal, value),
            None => vec![(value, false)],
        };
        for (segment, disabled) in segments {
            if disabled {
                // §20.2: escaping suppressed — but "If output escaping is
                // disabled for a character that is not representable in the
                // encoding that the processor is using for output, the request to
                // disable output escaping is ignored in respect of that
                // character" — and no CDATA section ("CDATA is treated as an
                // alternative escaping mechanism, which is disabled by the
                // disable-output-escaping option"), no character mapping (§9).
                // Unicode normalization still applies: §20.2 suppresses
                // escaping only ("the normal action of the serializer to escape
                // special characters such as & and < is suppressed"), and Ser.
                // §4 rule d, "Apply Unicode Normalization if requested by the
                // normalization-form parameter", is not escaping. Each run is
                // normalized on its own.
                self.check_html_controls(segment, "text")?;
                let segment = self.p.normalization.apply(segment);
                for c in segment.chars() {
                    if self.p.encoding.can_represent(c) {
                        self.out.push(c);
                    } else {
                        let _ = write!(self.out, "&#{};", c as u32);
                    }
                }
            } else if cdata {
                self.cdata_section(segment)?;
            } else {
                self.mapped(segment, |w, run| {
                    if raw {
                        w.raw_content(run)
                    } else {
                        w.escape_text(run)
                    }
                })?;
            }
        }
        Ok(())
    }

    /// Applies the character map to `text`: mapped characters are replaced by
    /// their strings as they are (SERE0008 if a string holds a character the
    /// encoding cannot), and each run of unmapped characters is normalized and
    /// passed to `escape`.
    ///
    /// Serialization §9: "Character mapping is applied to the characters that
    /// actually appear in a text or attribute node … before any other
    /// serialization operations such as escaping or Unicode Normalization are
    /// applied. If a character is mapped, then it is not subjected to XML or
    /// HTML escaping, nor to Unicode Normalization. The string that is
    /// substituted … is not subjected to XML or HTML escaping, it is not
    /// subjected to Unicode Normalization, and it is not subjected to further
    /// character mapping."
    fn mapped(
        &mut self,
        text: &str,
        mut escape: impl FnMut(&mut Self, &str) -> Result<(), XsltError>,
    ) -> Result<(), XsltError> {
        if self.p.character_map.is_empty() {
            let normalized = self.p.normalization.apply(text);
            return escape(self, &normalized);
        }
        let mut run_start = 0;
        for (at, c) in text.char_indices() {
            if let Some(string) = self.p.character_map.get(c) {
                if run_start < at {
                    let normalized = self.p.normalization.apply(&text[run_start..at]);
                    escape(self, &normalized)?;
                }
                // §9: "A serialization error [err:SERE0008] occurs if character
                // mapping causes the output of a string containing a character
                // that cannot be represented in the encoding".
                super::check_representable(self.p.encoding, string, "a character-map string")?;
                self.out.push_str(string);
                run_start = at + c.len_utf8();
            }
        }
        if run_start < text.len() {
            let normalized = self.p.normalization.apply(&text[run_start..]);
            escape(self, &normalized)?;
        }
        Ok(())
    }

    /// An attribute value with its delimiters: character map, normalization,
    /// escaping.
    fn attribute_value(&mut self, value: &str) -> Result<(), XsltError> {
        let quote = self.delimiter(value);
        self.out.push(quote);
        self.mapped(value, |w, run| w.escape_attribute(run, quote))?;
        self.out.push(quote);
        Ok(())
    }

    /// The delimiter of an attribute value: `'` when the character map writes
    /// a `"` and no `'` into it, `"` otherwise (the module docs: XSLT 2.0
    /// §20.1 "the serializer will (where possible) use the other choice of
    /// delimiter around the attribute value").
    fn delimiter(&self, value: &str) -> char {
        if self.p.character_map.is_empty() {
            return '"';
        }
        let (mut quot, mut apos) = (false, false);
        for c in value.chars() {
            if let Some(string) = self.p.character_map.get(c) {
                quot |= string.contains('"');
                apos |= string.contains('\'');
            }
        }
        if quot && !apos {
            '\''
        } else {
            '"'
        }
    }

    /// Escapes `text` as text-node content (the table in the module docs).
    fn escape_text(&mut self, text: &str) -> Result<(), XsltError> {
        for c in text.chars() {
            match c {
                '&' => self.out.push_str("&amp;"),
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                c => self.character(c)?,
            }
        }
        Ok(())
    }

    /// Escapes `value` as the content of an attribute value delimited by
    /// `quote` (the table in the module docs). §7.2 for HTML: "The HTML output
    /// method MUST NOT escape "<" characters occurring in attribute values" and
    /// "MUST NOT escape a & character occurring in an attribute value
    /// immediately followed by a { character". The delimiter is escaped as a
    /// numeric reference — never `&apos;`, which XHTML must not use (§6).
    fn escape_attribute(&mut self, value: &str, quote: char) -> Result<(), XsltError> {
        let mut chars = value.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '&' if self.html && chars.peek() == Some(&'{') => self.out.push('&'),
                '&' => self.out.push_str("&amp;"),
                '<' if self.html => self.out.push('<'),
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                '"' if quote == '"' => self.out.push_str("&#34;"),
                '\'' if quote == '\'' => self.out.push_str("&#39;"),
                '"' | '\'' => self.out.push(c),
                '\t' => self.out.push_str("&#x9;"),
                '\n' => self.out.push_str("&#xA;"),
                '\r' => self.out.push_str("&#xD;"),
                c => self.character(c)?,
            }
        }
        Ok(())
    }

    /// A character of text or of an attribute value that is not markup: as it
    /// stands, or as a character reference when the round-trip rule of §5 or
    /// the encoding requires one (`is_referenced`).
    fn character(&mut self, c: char) -> Result<(), XsltError> {
        if self.is_referenced(c) {
            self.reference(c)
        } else {
            self.out.push(c);
            Ok(())
        }
    }

    /// Whether `character` — the writer of text-node content — writes `c` as
    /// a character reference (or rejects it, `reference`) instead of as it
    /// stands. §5: "CR, NEL and LINE SEPARATOR characters in text nodes MUST be
    /// output respectively as "`&#xD;`", "`&#x85;`", and "`&#x2028;`", or their
    /// equivalents … In addition, the non-whitespace control characters #x1
    /// through #x1F and #x7F through #x9F in text nodes and attribute nodes
    /// MUST be output as character references"; §5.1.2, §7.4.2: a character the
    /// encoding cannot hold. Tab and LF stand as they are in text; an attribute
    /// value escapes them itself (`escape_attribute`). A CDATA section is
    /// closed around exactly these characters (`cdata_section`).
    fn is_referenced(&self, c: char) -> bool {
        match c {
            '\t' | '\n' => false,
            '\r' | '\u{2028}' => true,
            c => (c as u32) < 0x20 || is_c1_control(c) || !self.p.encoding.can_represent(c),
        }
    }

    /// `c`, one of the characters `is_referenced` selects, as a character
    /// reference — or the error the method raises for it.
    fn reference(&mut self, c: char) -> Result<(), XsltError> {
        let code = c as u32;
        if c == '\r' {
            self.out.push_str("&#xD;");
            return Ok(());
        }
        if code < 0x20 {
            // §5.1.1: "if the version parameter has the value 1.0, and the
            // instance of the data model contains a non-whitespace control
            // character in the range #x1 to #x1F, a serialization error
            // [err:SERE0006] results." (Tab and LF never get here; 1.0 is the
            // only XML version supported, SESU0013.)
            return Err(XsltError::new(
                SERE0006,
                format!("U+{code:04X} is not an XML 1.0 character"),
            ));
        }
        if is_c1_control(c) && self.html {
            return Err(html_control(c, "text or an attribute value"));
        }
        let _ = write!(self.out, "&#{code};");
        Ok(())
    }

    /// Content of an HTML `script` or `style` element: no escaping (§7.1), so
    /// a character the encoding cannot hold has no reference to fall back on —
    /// §7.4.2: "otherwise (for example, in a script or style element or in a
    /// comment), the serializer MUST signal a serialization error
    /// [err:SERE0008]".
    fn raw_content(&mut self, text: &str) -> Result<(), XsltError> {
        for c in text.chars() {
            if is_c1_control(c) {
                return Err(html_control(c, "the content of a script or style element"));
            }
            if !self.p.encoding.can_represent(c) {
                return Err(unrepresentable(
                    c,
                    "the content of a script or style element",
                ));
            }
            self.out.push(c);
        }
        Ok(())
    }

    /// A text node as a CDATA section (§5.1.4), after Unicode normalization
    /// (§4 rule b). "If the text node contains the sequence of characters ]]>,
    /// then the currently open CDATA section MUST be closed following the ]]
    /// and a new CDATA section opened before the >." "If the text node
    /// contains characters that are not representable in the character
    /// encoding …, then the currently open CDATA section MUST be closed before
    /// such characters, the characters MUST be output using character
    /// references or entity references, and a new CDATA section MUST be opened
    /// for any further characters in the text node."
    ///
    /// The same mechanism keeps the characters §5's round-trip rule asks to
    /// be written as references — "CR, NEL and LINE SEPARATOR characters in
    /// text nodes MUST be output respectively as "`&#xD;`", "`&#x85;`", and
    /// "`&#x2028;`" … the non-whitespace control characters #x1 through #x1F
    /// and #x7F through #x9F in text nodes and attribute nodes MUST be output
    /// as character references" — out of the section: a literal CR in a CDATA
    /// section comes back from a parser as LF. The section is closed around
    /// exactly the characters escaped text writes as references
    /// (`is_referenced`), and they are written the same way (`reference`).
    fn cdata_section(&mut self, text: &str) -> Result<(), XsltError> {
        // With the HTML method only an XML island has CDATA sections (§7.4.4).
        self.check_html_controls(text, "a CDATA section")?;
        let text = self.p.normalization.apply(text);
        let mut open = false;
        let mut rest = text.as_str();
        while let Some(c) = rest.chars().next() {
            if self.is_referenced(c) {
                if open {
                    self.out.push_str("]]>");
                    open = false;
                }
                self.reference(c)?;
            } else {
                if !open {
                    self.out.push_str("<![CDATA[");
                    open = true;
                }
                if rest.starts_with("]]>") {
                    self.out.push_str("]]]]><![CDATA[>");
                    rest = &rest[3..];
                    continue;
                }
                self.out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
        if open {
            self.out.push_str("]]>");
        }
        Ok(())
    }

    /// SERE0008 for an unrepresentable character in a name (§5.1.2: "for
    /// example, if the character occurs in the name of an element").
    fn check_name(&self, name: &str) -> Result<(), XsltError> {
        self.check_unescapable(name, "a name")
    }

    /// SERE0008 for an unrepresentable character where references are not
    /// recognized: names, comments, processing instructions (§5.1.2: "If a
    /// comment node contains the same character, a serialization error
    /// [err:SERE0008] results").
    fn check_unescapable(&self, text: &str, context: &str) -> Result<(), XsltError> {
        super::check_representable(self.p.encoding, text, context)
    }

    /// SERE0014 for a control character #x7F–#x9F of the tree with the HTML
    /// method, wherever it occurs — §7.3: "Certain characters, specifically
    /// the control characters #x7F-#x9F, are legal in XML but not in HTML. It
    /// is a serialization error [err:SERE0014] to use the HTML output method
    /// when such characters appear in the instance of the data model. The
    /// serializer MUST signal the error." Text and attribute values check
    /// character by character as they are escaped (`character`, `raw_content`);
    /// this is for the contexts that are written without escaping. A string a
    /// character map substitutes is not from the tree (§9: it "is not
    /// validated or processed in any way by the serializer").
    fn check_html_controls(&self, text: &str, context: &str) -> Result<(), XsltError> {
        if !self.html {
            return Ok(());
        }
        match text.chars().find(|&c| is_c1_control(c)) {
            Some(c) => Err(html_control(c, context)),
            None => Ok(()),
        }
    }
}

/// A control character #x7F–#x9F (Serialization §5, §7.3).
fn is_c1_control(c: char) -> bool {
    ('\u{7F}'..='\u{9F}').contains(&c)
}

/// SERE0014: a control character #x7F–#x9F with the HTML method (§7.3).
fn html_control(c: char, context: &str) -> XsltError {
    XsltError::new(
        SERE0014,
        format!(
            "U+{:04X} in {context} is a control character HTML does not allow",
            c as u32
        ),
    )
}

/// `prefix:local`, or `local` when the prefix is empty.
fn qualified(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

/// A system literal in the quote that does not occur in it (Serialization §3:
/// `doctype-system` is "A string of Unicode characters that does not include
/// both an apostrophe (#x27) and a quotation mark (#x22) character").
fn quoted(literal: &str) -> String {
    if literal.contains('"') {
        format!("'{literal}'")
    } else {
        format!("\"{literal}\"")
    }
}

fn first_child<N: DomNavigator>(node: &N) -> Option<N> {
    let mut child = node.clone();
    child.move_to_first_child().then_some(child)
}

fn next_sibling<N: DomNavigator>(node: &N) -> Option<N> {
    let mut sibling = node.clone();
    sibling.move_to_next_sibling().then_some(sibling)
}

fn has_element_child<N: DomNavigator>(node: &N) -> bool {
    let mut child = first_child(node);
    while let Some(c) = child {
        if c.node_type() == DomNodeType::Element {
            return true;
        }
        child = next_sibling(&c);
    }
    false
}

/// The number of text nodes in the subtree at `node` (a subtree the writer
/// drops still advances the text-node ordinal of [`EscapingMarks`]).
fn count_text_nodes<N: DomNavigator>(node: &N) -> u32 {
    let mut count = 0;
    let mut stack = vec![node.clone()];
    while let Some(n) = stack.pop() {
        if n.node_type().is_text_like() {
            count += 1;
        }
        let mut child = first_child(&n);
        while let Some(c) = child {
            child = next_sibling(&c);
            stack.push(c);
        }
    }
    count
}
