//! Evaluating the instruction tree (§7.4, §11): one `match` over
//! [`InstructionKind`], pushing into a [`Receiver`].

use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope};
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::XmlItem;

use super::content::SimpleDefault;
use super::context::XsltContext;
use super::dispatch::children_of;
use super::{Frame, Message, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::{is_ncname, lexical_qname, QName, XML_NS};
use crate::receiver::{namespace_nodes_of, CopyNamespaces, Name, Nav, Receiver};
use crate::whitespace;

impl<'a, 't> Transformation<'a, 't> {
    // ── Instruction evaluation ───────────────────────────────────────────

    pub(super) fn eval_body(
        &self,
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        for instruction in &body.0 {
            self.eval(instruction, frame, cx, out)?;
        }
        Ok(())
    }

    fn eval(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        self.eval_inner(instruction, frame, cx, out)
            .map_err(|e| e.or_location(&instruction.location(self.sheet)))
    }

    fn eval_inner(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match &instruction.kind {
            InstructionKind::Text { text, .. } => out.text(text, false),

            InstructionKind::ValueOf {
                value,
                separator,
                first_item_only,
                ..
            } => {
                let separator = match separator {
                    Some(avt) => Some(self.avt(avt, frame, cx)?),
                    None => None,
                };
                let text = self.simple_content(
                    value,
                    separator,
                    frame,
                    cx,
                    *first_item_only,
                    SimpleDefault::of(value),
                )?;
                out.text(&text, false)
            }

            InstructionKind::Sequence { select } => {
                let value = match select {
                    Some(id) => self.eval_expr(*id, frame, cx)?,
                    None => XPathValue::empty(),
                };
                for item in value.into_vec() {
                    out.append_item(&item)?;
                }
                Ok(())
            }

            InstructionKind::If { test, then } => {
                if self.ebv(*test, frame, cx)? {
                    self.eval_body(then, frame, cx, out)?;
                }
                Ok(())
            }

            InstructionKind::Choose { whens, otherwise } => {
                for (test, body) in whens {
                    if self.ebv(*test, frame, cx)? {
                        return self.eval_body(body, frame, cx, out);
                    }
                }
                match otherwise {
                    Some(body) => self.eval_body(body, frame, cx, out),
                    None => Ok(()),
                }
            }

            InstructionKind::ForEach { select, body } => {
                let items = self.eval_expr(*select, frame, cx)?.into_vec();
                let size = items.len();
                for (index, item) in items.into_iter().enumerate() {
                    // §5.4: the focus is replaced and the frame is the same,
                    // but "when an xsl:for-each … instruction is evaluated …
                    // the current template rule becomes null for the
                    // evaluation of that instruction", so an xsl:next-match
                    // inside one is XTDE0560.
                    let mut inner = cx.with_focus(item, index + 1, size);
                    inner.current_rule = None;
                    self.eval_body(body, frame, &inner, out)?;
                }
                Ok(())
            }

            InstructionKind::Variable {
                slot,
                value,
                as_type,
                ..
            } => {
                let value = self.var_value(value, as_type.as_ref(), frame, cx, codes::XTTE0570)?;
                frame.set(*slot, value);
                Ok(())
            }

            InstructionKind::ApplyTemplates {
                select,
                mode,
                params,
            } => {
                let items = match select {
                    Some(id) => self.eval_expr(*id, frame, cx)?.into_vec(),
                    None => self.children_of_focus(cx)?,
                };
                let mode = match mode {
                    ModeRef::Default => ModeKey::Default,
                    ModeRef::Current => cx.mode.clone(),
                    ModeRef::Named(name) => ModeKey::Named(name.clone()),
                };
                let params = self.eval_with_params(params, frame, cx)?;
                self.apply_to_items(items, mode, params, cx, out)
            }

            InstructionKind::CallTemplate { name, params } => {
                let id = self.sheet.named_template(name).ok_or_else(|| {
                    XsltError::new(
                        codes::XTSE0650,
                        format!("there is no template named {name}"),
                    )
                })?;
                // §10.1.1 / §9.10 row 9: a parameter the callee does not
                // declare is a static error, unless backwards-compatible
                // behaviour is enabled for the xsl:call-template instruction.
                // The compiler makes the same test over the whole stylesheet
                // (`compile::checks::check_call_template_params`), so this is a
                // belt-and-braces check on the path an embedder could otherwise
                // reach with a hand-built call; a **tunnel** `xsl:param` of the
                // callee does not satisfy a non-tunnel `xsl:with-param` (§10.1.2
                // keeps the two sets of parameters apart).
                if !self.sheet.is_backwards(instruction.ctx) {
                    let rule = self.sheet.template(id);
                    for param in params {
                        if !param.tunnel
                            && !rule
                                .params
                                .iter()
                                .any(|p| !p.tunnel && p.name == param.name)
                        {
                            return Err(XsltError::new(
                                codes::XTSE0680,
                                format!(
                                    "the template {name} has no parameter named {}",
                                    param.name
                                ),
                            ));
                        }
                    }
                }
                let params = self.eval_with_params(params, frame, cx)?;
                self.call_template(id, params, cx, out)
            }

            InstructionKind::LiteralElement {
                name,
                namespaces,
                attrs,
                attr_sets,
                inherit_namespaces,
                body,
            } => {
                out.start_element(name, None, false)?;
                if !*inherit_namespaces {
                    out.no_inherit_namespaces()?;
                }
                for (prefix, uri) in namespaces {
                    out.namespace(prefix, uri)?;
                }
                self.attribute_sets(attr_sets, frame, cx, out)?;
                for (attr_name, avt) in attrs {
                    let value = self.avt(avt, frame, cx)?;
                    out.attribute(attr_name, &value, None)?;
                }
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }

            InstructionKind::Element {
                name,
                namespace,
                attr_sets,
                inherit_namespaces,
                body,
            } => {
                let lexical = self.avt(name, frame, cx)?;
                let (prefix, local) =
                    lexical_qname(whitespace::trim(&lexical)).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0820,
                            format!("{lexical:?} is not a lexical QName"),
                        )
                    })?;
                let uri = match namespace {
                    Some(avt) => {
                        let uri = self.avt(avt, frame, cx)?;
                        check_namespace_uri(&uri, codes::XTDE0835)?;
                        uri
                    }
                    None => self.static_prefix(instruction, prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0830,
                            format!("the prefix {prefix:?} of {lexical:?} is not in scope"),
                        )
                    })?,
                };
                let prefix = if uri.is_empty() { "" } else { prefix };
                out.start_element(&Name::qualified(prefix, local, &uri), None, false)?;
                if !*inherit_namespaces {
                    out.no_inherit_namespaces()?;
                }
                self.attribute_sets(attr_sets, frame, cx, out)?;
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }

            InstructionKind::Attribute {
                name,
                namespace,
                separator,
                value,
            } => {
                let lexical = self.avt(name, frame, cx)?;
                let (prefix, local) =
                    lexical_qname(whitespace::trim(&lexical)).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0850,
                            format!("{lexical:?} is not a lexical QName"),
                        )
                    })?;
                if namespace.is_none() && whitespace::trim(&lexical) == "xmlns" {
                    return Err(XsltError::new(
                        codes::XTDE0855,
                        "an attribute may not be named xmlns",
                    ));
                }
                let uri = match namespace {
                    Some(avt) => {
                        let uri = self.avt(avt, frame, cx)?;
                        check_namespace_uri(&uri, codes::XTDE0865)?;
                        uri
                    }
                    None if prefix.is_empty() => String::new(),
                    None => self.static_prefix(instruction, prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0860,
                            format!("the prefix {prefix:?} of {lexical:?} is not in scope"),
                        )
                    })?,
                };
                let separator = match separator {
                    Some(avt) => Some(self.avt(avt, frame, cx)?),
                    None => None,
                };
                let text = self.simple_content(
                    value,
                    separator,
                    frame,
                    cx,
                    false,
                    SimpleDefault::of(value),
                )?;
                let prefix = if uri.is_empty() { "" } else { prefix };
                out.attribute(&Name::qualified(prefix, local, &uri), &text, None)
            }

            InstructionKind::Namespace { name, value } => {
                let prefix = self.avt(name, frame, cx)?;
                let prefix = whitespace::trim(&prefix);
                if prefix == "xmlns" || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0920,
                        format!("{prefix:?} is not a namespace prefix"),
                    ));
                }
                let uri =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                if uri.is_empty() {
                    return Err(XsltError::new(
                        codes::XTDE0930,
                        "xsl:namespace must not produce a zero-length namespace URI",
                    ));
                }
                check_namespace_uri(&uri, codes::XTDE0905)?;
                if (prefix == "xml") != (uri == XML_NS) {
                    return Err(XsltError::new(
                        codes::XTDE0925,
                        "the prefix xml may only be bound to the XML namespace, and vice versa",
                    ));
                }
                out.namespace(prefix, &uri)
            }

            InstructionKind::Comment { value } => {
                let text =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                out.comment(&text)
            }

            InstructionKind::ProcessingInstruction { name, value } => {
                let target = self.avt(name, frame, cx)?;
                let target = whitespace::trim(&target);
                if !is_ncname(target) || target.eq_ignore_ascii_case("xml") {
                    return Err(XsltError::new(
                        codes::XTDE0890,
                        format!("{target:?} is not a processing-instruction target"),
                    ));
                }
                let text =
                    self.simple_content(value, None, frame, cx, false, SimpleDefault::Space)?;
                out.pi(target, whitespace::trim_start(&text))
            }

            InstructionKind::Document { body } => {
                // §11.5: a new document node whose content is the sequence
                // constructor; in element content it contributes its children.
                out.start_document()?;
                out.document_base_uri(&self.sheet.static_contexts.get(instruction.ctx).base_uri)?;
                self.eval_body(body, frame, cx, out)?;
                out.end_document()
            }

            InstructionKind::Copy {
                copy_namespaces,
                inherit_namespaces,
                attr_sets,
                body,
            } => self.copy_shallow(
                ShallowCopyNamespaces {
                    copy: copy_scope(*copy_namespaces),
                    inherit: *inherit_namespaces,
                },
                attr_sets,
                body,
                frame,
                cx,
                out,
            ),

            InstructionKind::CopyOf {
                select,
                copy_namespaces,
            } => {
                let value = self.eval_expr(*select, frame, cx)?;
                let scope = copy_scope(*copy_namespaces);
                for item in value.into_vec() {
                    // §11.9.2: every node of the selected sequence is
                    // *constructed anew*, so this is `append_copy`, never
                    // `append_item` — the difference only shows in a sequence
                    // collector, where `append_item` would keep the original.
                    out.append_copy(&item, scope)?;
                }
                Ok(())
            }

            InstructionKind::Message {
                select,
                body,
                terminate,
            } => {
                let terminate = self.avt(terminate, frame, cx)?;
                let terminate = crate::names::parse_yes_no(&terminate, "terminate")?;
                let content = self.message_content(*select, body, frame, cx)?;
                let message = Message { content, terminate };
                if let Some(sink) = &self.message_sink {
                    sink(&message);
                }
                self.messages.borrow_mut().push(message);
                if terminate {
                    *self.terminated.borrow_mut() = true;
                    return Err(XsltError::new(
                        codes::XTMM9000,
                        "the transformation was terminated by xsl:message terminate=\"yes\"",
                    ));
                }
                Ok(())
            }

            // §18.2.3: an xsl:fallback that is reached normally does nothing.
            InstructionKind::Fallback => Ok(()),

            InstructionKind::ApplyImports { params } => {
                let params = self.eval_with_params(params, frame, cx)?;
                self.override_rule(true, params, cx, out)
            }

            InstructionKind::NextMatch { params } => {
                let params = self.eval_with_params(params, frame, cx)?;
                self.override_rule(false, params, cx, out)
            }

            InstructionKind::NotImplemented { feature, milestone } => {
                Err(XsltError::not_implemented(feature, milestone))
            }
        }
    }

    /// §11.9.1 `xsl:copy`.
    fn copy_shallow(
        &self,
        namespaces: ShallowCopyNamespaces,
        attr_sets: &[QName],
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let Some(item) = cx.item.clone() else {
            return Err(XsltError::new(
                "XPDY0002",
                "xsl:copy has no context item to copy",
            ));
        };
        let nav = match item {
            // "When the context item is an atomic value, the xsl:copy
            // instruction returns this value."
            XmlItem::Atomic(value) => return out.append_item(&XmlItem::Atomic(value)),
            XmlItem::Node(nav) => nav,
        };
        match nav.node_type() {
            DomNodeType::Root => {
                out.start_document()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_document()
            }
            DomNodeType::Element => {
                out.start_element(&Name::of(&nav), None, false)?;
                if !namespaces.inherit {
                    out.no_inherit_namespaces()?;
                }
                if namespaces.copy == CopyNamespaces::All {
                    // §11.9.2: "all the namespace nodes of the original element
                    // are copied". A namespace *node* never has a zero-length
                    // URI, so an `xmlns=""` on the source element contributes
                    // none — and because the element `xsl:copy` produces is a
                    // **newly constructed** element, §5.7.1's last rule lets it
                    // inherit the default namespace of the element it is being
                    // constructed in (`namespace-3002`, spec bug 5857).
                    for (prefix, uri) in namespace_nodes_of(&nav, NamespaceAxisScope::ExcludeXml) {
                        out.namespace(&prefix, &uri)?;
                    }
                }
                self.attribute_sets(attr_sets, frame, cx, out)?;
                out.end_start_tag()?;
                self.eval_body(body, frame, cx, out)?;
                out.end_element()
            }
            // "the xsl:copy instruction returns a new node that is a copy of
            // the context node … The sequence constructor, if present, is not
            // evaluated." A *new* node: `append_copy`, so that a copy landing
            // in a sequence is parentless rather than the original node.
            _ => out.append_copy(&XmlItem::Node(nav), namespaces.copy),
        }
    }

    fn attribute_sets(
        &self,
        names: &[QName],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let mut seen: Vec<QName> = Vec::new();
        self.attribute_sets_inner(names, &mut seen, frame, cx, out)
    }

    fn attribute_sets_inner(
        &self,
        names: &[QName],
        seen: &mut Vec<QName>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        for name in names {
            if seen.contains(name) {
                return Err(XsltError::new(
                    codes::XTSE0720,
                    format!("the attribute set {name} uses itself"),
                ));
            }
            let Some(set) = self.sheet.attribute_set(name) else {
                return Err(XsltError::new(
                    codes::XTSE0710,
                    format!("there is no attribute set named {name}"),
                ));
            };
            seen.push(name.clone());
            self.attribute_sets_inner(&set.use_attribute_sets, seen, frame, cx, out)?;
            for attribute in &set.attributes {
                self.eval(attribute, frame, cx, out)?;
            }
            seen.pop();
        }
        Ok(())
    }

    // ── Helpers the instruction bodies above need ────────────────────────

    fn children_of_focus(&self, cx: &XsltContext<'t>) -> Result<Vec<XmlItem<Nav<'t>>>, XsltError> {
        match &cx.item {
            Some(XmlItem::Node(nav)) => Ok(children_of(nav)),
            Some(XmlItem::Atomic(_)) => Err(XsltError::new(
                "XPTY0019",
                "xsl:apply-templates with no select needs a node as the context item",
            )),
            None => Err(XsltError::new(
                "XPDY0002",
                "xsl:apply-templates has no context item",
            )),
        }
    }

    /// The namespace a prefix is bound to in an instruction's static context.
    fn static_prefix(&self, instruction: &Instruction, prefix: &str) -> Option<String> {
        let sc = self.sheet.static_contexts.get(instruction.ctx);
        if prefix.is_empty() {
            return Some(sc.resolve_prefix("").unwrap_or_default());
        }
        sc.resolve_prefix(prefix)
    }
}

/// The namespace URI a node-construction instruction was handed, checked
/// against §11.2 / §11.3 / §11.7.
///
/// > \[ERR XTDE0835\] It is a non-recoverable dynamic error if the effective
/// > value of the `namespace` attribute is not in the lexical space of the
/// > `xs:anyURI` data type or if it is the string
/// > `http://www.w3.org/2000/xmlns/`.
///
/// XTDE0865 (`xsl:attribute`) and XTDE0905 (`xsl:namespace`, on the node's
/// string value) are the same sentence, so the check is shared and the caller
/// passes its code.
///
/// The XMLNS namespace is named outright. The `xs:anyURI` half is two rules, and
/// nothing more, because nothing more is derivable:
///
/// 1. **Every character must match XML's `Char` production.** `datatypes.html`
///    §3.3.17.2: "The ·lexical space· of `anyURI` is the set of finite-length
///    sequences of zero or more characters (as defined in \[XML\]) that ·match·
///    the `Char` production from \[XML\]." That is the *whole* constraint the
///    datatype imposes; the same section says so outright: "Because it is
///    impractical for processors to check that a value is a context-appropriate
///    URI reference, neither the syntactic constraints defined by the definitions
///    of individual schemes nor the generic syntactic constraints defined by
///    \[RFC 3987\] and \[RFC 3986\] and their successors are part of this
///    datatype as defined here."
///
///    So tab, newline, carriage return and U+0085 — all of them XML `Char`s —
///    are **in** the lexical space, as are `"` (`namespace-3005` builds such a
///    URI and expects it to work) and every other printable character. Only the
///    C0 controls XML itself forbids, the surrogates and the two non-characters
///    `#xFFFE`/`#xFFFF` are out; a `char::is_control` test rejected far more
///    than that.
///
/// 2. **At most one `#`.** This is *not* derivable from the local
///    `datatypes.html`, which is XSD 1.1. It comes from XSD **1.0** Part 2
///    §3.2.17, where `anyURI`'s lexical space is a URI reference "as defined in
///    \[RFC 2396\], as amended by \[RFC 2732\]" — a grammar with at most one
///    fragment separator. (That wording is from memory: no XSD 1.0 datatypes
///    document is available locally.) The suite selects that reading explicitly:
///    `error-0905a` feeds `####` to `xsl:namespace` and requires XTDE0905, under
///    the dependency `<feature value="XSD_1.1" satisfied="false"/>`, while its
///    twin `error-0905b` runs the same stylesheet for an XSD 1.1 processor and
///    requires **no** error, citing W3C bug 30180. This processor reports itself
///    as not XSD-1.1-schema-aware, so it is `error-0905a` that applies to it.
///
/// Nothing else. `error-0835a` (`##not-a-uri`) and `error-0865a`
/// (`http://####/`) were relaxed in 2019 — "Processors have dispensation to
/// treat all strings as valid namespaces" — and now accept either the error or
/// success, so they constrain nothing either way.
fn check_namespace_uri(uri: &str, code: &str) -> Result<(), XsltError> {
    /// The namespace `xmlns` declarations themselves nominally live in; no
    /// node may be in it (§5.7.3's XDM constraints, erratum E6).
    const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
    if uri == XMLNS_NS {
        return Err(XsltError::new(
            code,
            format!("{XMLNS_NS} cannot be used as a namespace URI"),
        ));
    }
    if !uri.chars().all(is_xml_char) || uri.matches('#').count() > 1 {
        return Err(XsltError::new(
            code,
            format!("{uri:?} is not in the lexical space of xs:anyURI"),
        ));
    }
    Ok(())
}

/// XML 1.0 `Char`: `#x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] |
/// [#x10000-#x10FFFF]`.
///
/// Rust's `char` cannot hold a surrogate, so the two printable ranges reduce to
/// "at least `#x20`, and not `#xFFFE`/`#xFFFF`".
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= '\u{20}' && c != '\u{FFFE}' && c != '\u{FFFF}')
}

/// The two namespace settings of `xsl:copy` (§11.9.1): which of the source
/// element's namespace nodes the copy carries, and whether the copy's own
/// namespace nodes reach the content constructed inside it.
#[derive(Clone, Copy)]
struct ShallowCopyNamespaces {
    copy: CopyNamespaces,
    inherit: bool,
}

/// The `copy-namespaces` setting of a copying instruction (§11.9.1, §11.9.2).
fn copy_scope(copy_namespaces: bool) -> CopyNamespaces {
    if copy_namespaces {
        CopyNamespaces::All
    } else {
        CopyNamespaces::FixupOnly
    }
}
