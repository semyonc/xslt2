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
use crate::names::{is_ncname, split_qname, QName, XML_NS};
use crate::receiver::{CopyNamespaces, Name, Nav, Receiver};
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
                if !self.sheet.is_backwards(instruction.ctx) {
                    let rule = self.sheet.template(id);
                    for param in params {
                        if !param.tunnel && !rule.params.iter().any(|p| p.name == param.name) {
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
                body,
            } => {
                out.start_element(name, None, false)?;
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
                body,
                ..
            } => {
                let lexical = self.avt(name, frame, cx)?;
                let (prefix, local) = split_qname(whitespace::trim(&lexical));
                if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0820,
                        format!("{lexical:?} is not a lexical QName"),
                    ));
                }
                let uri = match namespace {
                    Some(avt) => self.avt(avt, frame, cx)?,
                    None => self.static_prefix(instruction, prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTDE0830,
                            format!("the prefix {prefix:?} of {lexical:?} is not in scope"),
                        )
                    })?,
                };
                let prefix = if uri.is_empty() { "" } else { prefix };
                out.start_element(&Name::qualified(prefix, local, &uri), None, false)?;
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
                let (prefix, local) = split_qname(whitespace::trim(&lexical));
                if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
                    return Err(XsltError::new(
                        codes::XTDE0850,
                        format!("{lexical:?} is not a lexical QName"),
                    ));
                }
                if namespace.is_none() && whitespace::trim(&lexical) == "xmlns" {
                    return Err(XsltError::new(
                        codes::XTDE0855,
                        "an attribute may not be named xmlns",
                    ));
                }
                let uri = match namespace {
                    Some(avt) => self.avt(avt, frame, cx)?,
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
                self.eval_body(body, frame, cx, out)?;
                out.end_document()
            }

            InstructionKind::Copy {
                copy_namespaces,
                attr_sets,
                body,
                ..
            } => self.copy_shallow(*copy_namespaces, attr_sets, body, frame, cx, out),

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
        copy_namespaces: bool,
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
                if copy_namespaces {
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
            _ => out.append_copy(&XmlItem::Node(nav), copy_scope(copy_namespaces)),
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

/// The `copy-namespaces` setting of a copying instruction (§11.9.1, §11.9.2).
fn copy_scope(copy_namespaces: bool) -> CopyNamespaces {
    if copy_namespaces {
        CopyNamespaces::All
    } else {
        CopyNamespaces::FixupOnly
    }
}
