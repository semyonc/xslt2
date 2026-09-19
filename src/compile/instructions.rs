//! Sequence constructors (§5.7) and the XSLT instructions of §7.4.

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::attributes::check_attributes;
use super::{Builder, ScopeEntry};
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::names::{parse_yes_no, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, is_stripped_stylesheet_text, ModuleId, StaticContextId};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── Sequence constructors (§5.7) ─────────────────────────────────────

    pub(super) fn sequence_constructor(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Body, XsltError> {
        self.sequence_constructor_skipping(parent, module_id, "")
    }

    /// Compiles the children of `parent`, skipping XSLT elements named `skip`
    /// (used for the `xsl:param` children of a template).
    pub(super) fn sequence_constructor_skipping(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
        skip: &str,
    ) -> Result<Body, XsltError> {
        let scope_mark = self.scope.len();
        let mut out = Vec::new();
        for child in self.children(parent, module_id) {
            match child.node_type() {
                DomNodeType::Element => {
                    let skipped = !skip.is_empty()
                        && child.namespace_uri() == XSLT_NS
                        && child.local_name() == skip;
                    if !skipped {
                        out.push(self.instruction(&child, module_id)?);
                    }
                }
                // §4.2: whitespace-only text nodes are stripped.
                kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => {
                    let module = self.modules.module(module_id);
                    let ctx = self.context_of(parent, module)?;
                    out.push(self.wrap(
                        InstructionKind::Text {
                            text: child.value(),
                            disable_output_escaping: false,
                        },
                        ctx,
                        module_id,
                        &child,
                    ));
                }
                // Comments and processing instructions in a stylesheet are
                // not part of the result (§4.1).
                _ => {}
            }
        }
        self.scope.truncate(scope_mark);
        Ok(Body(out))
    }

    pub(super) fn wrap(
        &self,
        kind: InstructionKind,
        ctx: StaticContextId,
        module_id: ModuleId,
        nav: &Nav<'t>,
    ) -> Instruction {
        let loc = self.modules.module(module_id).location(nav);
        Instruction {
            kind,
            ctx,
            module: module_id,
            line: loc.line,
            column: loc.column,
        }
    }

    pub(super) fn instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Instruction, XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        if nav.namespace_uri() != XSLT_NS {
            let sc = self.static_contexts.get(ctx);
            if sc.is_extension_namespace(nav.namespace_uri()) {
                // §18.2.2: an extension instruction this processor does not
                // know falls back to its xsl:fallback children.
                return self.extension_instruction(nav, module_id, ctx);
            }
            let kind = self.literal_element_kind(nav, module_id, ctx)?;
            return Ok(self.wrap(kind, ctx, module_id, nav));
        }
        let kind = self.xslt_instruction(nav, module_id, ctx)?;
        Ok(self.wrap(kind, ctx, module_id, nav))
    }

    fn extension_instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<Instruction, XsltError> {
        match self.fallback_bodies(nav, module_id)? {
            Some(body) => Ok(self.wrap(
                InstructionKind::Choose {
                    whens: Vec::new(),
                    otherwise: Some(body),
                },
                ctx,
                module_id,
                nav,
            )),
            None => Ok(self.wrap(
                InstructionKind::NotImplemented {
                    feature: format!("the extension instruction {}", nav.name()),
                    milestone: "M3",
                },
                ctx,
                module_id,
                nav,
            )),
        }
    }

    /// The contents of **every** `xsl:fallback` child, concatenated.
    ///
    /// > \[§18.2.2\] If the instruction is not available, then the
    /// > `xsl:fallback` children of the instruction are evaluated (there may
    /// > be more than one).
    fn fallback_bodies(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Option<Body>, XsltError> {
        let mut found = false;
        let mut out = Vec::new();
        for child in self.children(nav, module_id) {
            if child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "fallback"
            {
                found = true;
                out.extend(self.sequence_constructor(&child, module_id)?.0);
            }
        }
        Ok(found.then_some(Body(out)))
    }

    fn xslt_instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<InstructionKind, XsltError> {
        let local = nav.local_name().to_string();
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        if !self.static_contexts.get(ctx).compat.is_forwards() {
            check_attributes(nav, &local, &loc)?;
        }
        match local.as_str() {
            "apply-templates" => {
                self.reject_sort(nav, module_id)?;
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                let mode = self.mode_ref(nav, ctx)?;
                let params = self.with_params(nav, module_id)?;
                Ok(InstructionKind::ApplyTemplates {
                    select,
                    mode,
                    params,
                })
            }
            "apply-imports" => Ok(InstructionKind::ApplyImports {
                params: self.with_params(nav, module_id)?,
            }),
            "next-match" => Ok(InstructionKind::NextMatch {
                params: self.with_params(nav, module_id)?,
            }),
            "call-template" => {
                let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                    XsltError::new(codes::XTSE0010, "xsl:call-template has no name attribute")
                        .with_location(loc.clone())
                })?;
                let params = self.with_params(nav, module_id)?;
                Ok(InstructionKind::CallTemplate { name, params })
            }
            "for-each" => {
                self.reject_sort(nav, module_id)?;
                let select = self.required_expr(nav, "select", ctx, &loc)?;
                let body = self.sequence_constructor_skipping(nav, module_id, "sort")?;
                Ok(InstructionKind::ForEach { select, body })
            }
            "if" => {
                let test = self.required_expr(nav, "test", ctx, &loc)?;
                let then = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::If { test, then })
            }
            "choose" => self.choose(nav, module_id, &loc),
            "value-of" => {
                let separator = match attr_local(nav, "separator") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let has_select = attr_local(nav, "select").is_some();
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0870)?;
                let doe = attr_local(nav, "disable-output-escaping")
                    .map(|v| parse_yes_no(&v, "disable-output-escaping"))
                    .transpose()?
                    .unwrap_or(false);
                // §9.10 row 5: with backwards-compatible behaviour and no
                // separator, all items after the first are ignored.
                let first_item_only = separator.is_none()
                    && has_select
                    && self.static_contexts.get(ctx).compat.is_backwards();
                Ok(InstructionKind::ValueOf {
                    value,
                    separator,
                    disable_output_escaping: doe,
                    first_item_only,
                })
            }
            "text" => {
                let doe = attr_local(nav, "disable-output-escaping")
                    .map(|v| parse_yes_no(&v, "disable-output-escaping"))
                    .transpose()?
                    .unwrap_or(false);
                Ok(InstructionKind::Text {
                    text: self.text_content(nav, module_id),
                    disable_output_escaping: doe,
                })
            }
            "sequence" => {
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                Ok(InstructionKind::Sequence { select })
            }
            "element" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let namespace = match attr_local(nav, "namespace") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let attr_sets = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
                let inherit = attr_local(nav, "inherit-namespaces")
                    .map(|v| parse_yes_no(&v, "inherit-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Element {
                    name,
                    namespace,
                    attr_sets,
                    inherit_namespaces: inherit,
                    body,
                })
            }
            "attribute" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let namespace = match attr_local(nav, "namespace") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let separator = match attr_local(nav, "separator") {
                    Some(text) => Some(self.avt(&text, ctx)?),
                    None => None,
                };
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0840)?;
                Ok(InstructionKind::Attribute {
                    name,
                    namespace,
                    separator,
                    value,
                })
            }
            "namespace" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0910)?;
                Ok(InstructionKind::Namespace { name, value })
            }
            "comment" => {
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0940)?;
                Ok(InstructionKind::Comment { value })
            }
            "processing-instruction" => {
                let name = self.required_avt(nav, "name", ctx, &loc)?;
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0880)?;
                Ok(InstructionKind::ProcessingInstruction { name, value })
            }
            "document" => Ok(InstructionKind::Document {
                body: self.sequence_constructor(nav, module_id)?,
            }),
            "copy" => {
                let copy_ns = attr_local(nav, "copy-namespaces")
                    .map(|v| parse_yes_no(&v, "copy-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let inherit = attr_local(nav, "inherit-namespaces")
                    .map(|v| parse_yes_no(&v, "inherit-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                let attr_sets = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Copy {
                    copy_namespaces: copy_ns,
                    inherit_namespaces: inherit,
                    attr_sets,
                    body,
                })
            }
            "copy-of" => {
                let select = self.required_expr(nav, "select", ctx, &loc)?;
                let copy_ns = attr_local(nav, "copy-namespaces")
                    .map(|v| parse_yes_no(&v, "copy-namespaces"))
                    .transpose()?
                    .unwrap_or(true);
                Ok(InstructionKind::CopyOf {
                    select,
                    copy_namespaces: copy_ns,
                })
            }
            "variable" => {
                let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                    XsltError::new(codes::XTSE0010, "xsl:variable has no name attribute")
                        .with_location(loc.clone())
                })?;
                let as_type = match attr_local(nav, "as") {
                    Some(text) => Some(self.compile_type(&text, ctx)?),
                    None => None,
                };
                let value = self.var_value(nav, module_id, ctx, as_type.is_some())?;
                let slot = self.frame_size;
                self.frame_size += 1;
                self.scope.push(ScopeEntry {
                    name: name.clone(),
                    location: VarLocation::Local(slot),
                });
                Ok(InstructionKind::Variable {
                    slot,
                    value,
                    as_type,
                    name,
                })
            }
            "param" => Err(XsltError::new(
                codes::XTSE0010,
                "xsl:param is only allowed at the start of a template, function or stylesheet",
            )
            .with_location(loc)),
            "message" => {
                let terminate = match attr_local(nav, "terminate") {
                    Some(text) => self.avt(&text, ctx)?,
                    None => Avt::literal("no"),
                };
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Message {
                    select,
                    body,
                    terminate,
                })
            }
            "fallback" => Ok(InstructionKind::Fallback),
            "number" | "for-each-group" | "analyze-string" | "perform-sort" | "result-document"
            | "sort" | "function" | "key" => Ok(InstructionKind::NotImplemented {
                feature: format!("xsl:{local}"),
                milestone: match local.as_str() {
                    "number" | "result-document" => "M3",
                    _ => "M2",
                },
            }),
            other => {
                // §3.9: in a forwards-compatible region an unknown XSLT
                // instruction becomes its xsl:fallback children; with no
                // fallback children it is the same static error as below (see
                // `fallback_or_error`, which does not yet raise it).
                if self.static_contexts.get(ctx).compat.is_forwards() {
                    return self.fallback_or_error(nav, module_id, other);
                }
                Err(XsltError::new(
                    codes::XTSE0010,
                    format!("xsl:{other} is not an XSLT instruction"),
                )
                .with_location(loc))
            }
        }
    }

    /// The forwards-compatible reading of an unknown `xsl:` instruction (§3.9).
    ///
    /// > \[§3.9\] … if an element in the XSLT namespace appears as part of a
    /// > sequence constructor, and XSLT 2.0 does not allow such elements to
    /// > appear as part of a sequence constructor, then: If the element has one
    /// > or more `xsl:fallback` children, then no error is reported either
    /// > statically or dynamically, and the result of evaluating the instruction
    /// > is the concatenation of the sequences formed by evaluating the sequence
    /// > constructors within its `xsl:fallback` children, in document order. …
    /// > If the element has no `xsl:fallback` children, then a static error is
    /// > reported in the same way as if forwards-compatible behavior were not
    /// > enabled.
    ///
    /// "No error is reported either statically or dynamically" covers the
    /// unknown element, not the fallback bodies: those *are* evaluated, so a
    /// failure to compile one is a real error and is propagated (it used to be
    /// replaced by the `XSLT2-NOTIMPL` below, which hid genuine `XTSE` errors —
    /// M2a review, finding 1).
    ///
    /// The `None` arm is **not** conformant and is left as it was: §3.9 asks for
    /// the same static error as the non-forwards-compatible branch raises
    /// (`XTSE0010`), not for an error deferred to evaluation — that was the
    /// XSLT 1.0 rule. Deferring it is a separate defect, recorded in
    /// `XSLT2_M2A_REVIEW_FIXES.md` under "Noticed, not done".
    fn fallback_or_error(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        name: &str,
    ) -> Result<InstructionKind, XsltError> {
        Ok(match self.fallback_bodies(nav, module_id)? {
            Some(body) => InstructionKind::Choose {
                whens: Vec::new(),
                otherwise: Some(body),
            },
            None => InstructionKind::NotImplemented {
                feature: format!("the unknown XSLT instruction xsl:{name}"),
                milestone: "M3",
            },
        })
    }

    fn choose(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let mut whens = Vec::new();
        let mut otherwise = None;
        for child in self.children(nav, module_id) {
            if child.node_type() == DomNodeType::Element {
                let module = self.modules.module(module_id);
                let child_loc = module.location(&child);
                if child.namespace_uri() != XSLT_NS {
                    return Err(XsltError::new(
                        codes::XTSE0010,
                        "only xsl:when and xsl:otherwise may appear inside xsl:choose",
                    )
                    .with_location(child_loc));
                }
                match child.local_name() {
                    "when" => {
                        if otherwise.is_some() {
                            return Err(XsltError::new(
                                codes::XTSE0010,
                                "xsl:when must not follow xsl:otherwise",
                            )
                            .with_location(child_loc));
                        }
                        let child_ctx = self.context_of(&child, self.modules.module(module_id))?;
                        let test = self.required_expr(&child, "test", child_ctx, &child_loc)?;
                        let body = self.sequence_constructor(&child, module_id)?;
                        whens.push((test, body));
                    }
                    "otherwise" => {
                        if otherwise.is_some() {
                            return Err(XsltError::new(
                                codes::XTSE0010,
                                "xsl:choose may have only one xsl:otherwise child",
                            )
                            .with_location(child_loc));
                        }
                        otherwise = Some(self.sequence_constructor(&child, module_id)?);
                    }
                    other => {
                        return Err(XsltError::new(
                            codes::XTSE0010,
                            format!("xsl:{other} may not appear inside xsl:choose"),
                        )
                        .with_location(child_loc))
                    }
                }
            }
        }
        if whens.is_empty() {
            return Err(XsltError::new(
                codes::XTSE0010,
                "xsl:choose must have at least one xsl:when child",
            )
            .with_location(loc.clone()));
        }
        Ok(InstructionKind::Choose { whens, otherwise })
    }

    fn reject_sort(&self, nav: &Nav<'t>, module_id: ModuleId) -> Result<(), XsltError> {
        for child in self.children(nav, module_id) {
            if child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "sort"
            {
                return Err(XsltError::not_implemented("xsl:sort", "M2")
                    .with_location(self.modules.module(module_id).location(&child)));
            }
        }
        Ok(())
    }

    fn with_params(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Vec<WithParam>, XsltError> {
        let mut out = Vec::new();
        for child in self.children(nav, module_id) {
            if child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "with-param"
            {
                let module = self.modules.module(module_id);
                let ctx = self.context_of(&child, module)?;
                let name = self
                    .qname_attr(&child, "name", ctx, module)?
                    .ok_or_else(|| {
                        XsltError::new(codes::XTSE0010, "xsl:with-param has no name attribute")
                            .with_location(module.location(&child))
                    })?;
                let as_type = match attr_local(&child, "as") {
                    Some(text) => Some(self.compile_type(&text, ctx)?),
                    None => None,
                };
                let tunnel = attr_local(&child, "tunnel")
                    .map(|v| parse_yes_no(&v, "tunnel"))
                    .transpose()?
                    .unwrap_or(false);
                let value = self.var_value(&child, module_id, ctx, as_type.is_some())?;
                out.push(WithParam {
                    name,
                    value,
                    as_type,
                    tunnel,
                    loc: self.modules.module(module_id).location(&child),
                });
            }
        }
        Ok(out)
    }

    fn mode_ref(&mut self, nav: &Nav<'t>, ctx: StaticContextId) -> Result<ModeRef, XsltError> {
        let _ = &self.names;
        Ok(match attr_local(nav, "mode") {
            None => ModeRef::Default,
            Some(text) => match whitespace::trim(&text) {
                "#default" => ModeRef::Default,
                "#current" => ModeRef::Current,
                name => ModeRef::Named(self.resolve(name, ctx, codes::XTSE0550)?),
            },
        })
    }

    fn value_source(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        exclusivity_code: &'static str,
    ) -> Result<ValueSource, XsltError> {
        let select = attr_local(nav, "select");
        let has_content = self.has_content(nav, module_id);
        let module = self.modules.module(module_id);
        match (select, has_content) {
            (Some(_), true) => Err(XsltError::new(
                exclusivity_code,
                format!(
                    "xsl:{} has both a select attribute and content",
                    nav.local_name()
                ),
            )
            .with_location(module.location(nav))),
            (Some(source), false) => Ok(ValueSource::Select(self.compile_expr(&source, ctx)?)),
            (None, _) => Ok(ValueSource::Content(
                self.sequence_constructor(nav, module_id)?,
            )),
        }
    }

    /// The concatenated text of an element's children (`xsl:text`).
    ///
    /// Only text children contribute, so §3.12 cannot change the answer — but
    /// the walk goes through the filtered iterator anyway, so that this stays
    /// true if it ever grows a case for elements.
    fn text_content(&self, nav: &Nav<'t>, module_id: ModuleId) -> String {
        let mut out = String::new();
        for child in self.children(nav, module_id) {
            if child.node_type().is_text_like() {
                out.push_str(&child.value());
            }
        }
        out
    }
}
