//! Sequence constructors (§5.7) and the XSLT instructions of §7.4.

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::attributes::check_attributes;
use super::checks;
use super::{Builder, ScopeEntry};
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::names::{parse_yes_no, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, starts_text_run, text_run_value, ModuleId, StaticContextId};
use crate::whitespace;

/// Whether `child` ends the leading run of `xsl:{names}` children of
/// `parent`: it is an element other than those, or significant text (§4.2).
/// Comments, processing instructions and whitespace-only text do not end it.
///
/// The run is what a template's or function's parameter list (§9.2: "The
/// xsl:param elements must come before any other children") and a sort key
/// specification (§13: "xsl:sort elements must occur before any other
/// children") consist of; everything from its end on is the sequence
/// constructor.
pub(super) fn leading_run_ends(parent: &Nav<'_>, child: &Nav<'_>, names: &[&str]) -> bool {
    match child.node_type() {
        DomNodeType::Element => {
            !(child.namespace_uri() == XSLT_NS && names.contains(&child.local_name()))
        }
        kind if kind.is_text_like() => checks::is_significant_text(parent, child),
        _ => false,
    }
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── Sequence constructors (§5.7) ─────────────────────────────────────

    pub(super) fn sequence_constructor(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Body, XsltError> {
        self.sequence_constructor_skipping(parent, module_id, "")
    }

    /// Compiles the children of `parent`, skipping the **leading** XSLT
    /// elements named `skip` — the `xsl:param` declarations of a template or
    /// function, the `xsl:sort` key specification of `xsl:for-each`,
    /// `xsl:for-each-group` or `xsl:perform-sort`, which their owners compile.
    ///
    /// Only the leading run is theirs: one after the first other element or
    /// significant text (`leading_run_ends`) is part of the sequence
    /// constructor, where XSLT 2.0 does not allow it, so it goes through
    /// [`Self::instruction`] — §3.9's fallback in a forwards-compatible region,
    /// the static error anywhere else.
    pub(super) fn sequence_constructor_skipping(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
        skip: &str,
    ) -> Result<Body, XsltError> {
        let scope_mark = self.scope.len();
        let mut out = Vec::new();
        let mut leading = !skip.is_empty();
        for child in self.children(parent, module_id) {
            if leading && leading_run_ends(parent, &child, &[skip]) {
                leading = false;
            }
            match child.node_type() {
                DomNodeType::Element => {
                    if !leading {
                        out.push(self.instruction(&child, module_id)?);
                    }
                }
                // §4.2: comments and PIs are removed, the text nodes that
                // become adjacent are merged — one instruction per run, made
                // by its first node — and whitespace-only runs are stripped,
                // including (whatever `xml:space` says) one "whose
                // following-sibling node is an xsl:param or xsl:sort element"
                // (`whitespace-015`: `xsl:for-each xml:space="preserve"`).
                kind if kind.is_text_like()
                    && starts_text_run(&child)
                    && checks::is_significant_text(parent, &child) =>
                {
                    let module = self.modules.module(module_id);
                    let ctx = self.context_of(parent, module)?;
                    out.push(self.wrap(
                        InstructionKind::Text {
                            text: text_run_value(&child),
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

    /// An extension instruction (§18.2): no implementation is available for
    /// any, so it compiles to its fallback — or to nothing, and XTDE1450 when
    /// it is evaluated. Its attributes and its other children are the
    /// extension's business and are not read (§18.2.2: "An implementation must
    /// not signal an error merely because the stylesheet contains an extension
    /// instruction for which no implementation is available").
    fn extension_instruction(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<Instruction, XsltError> {
        let fallback = self.fallback_bodies(nav, module_id)?;
        Ok(self.wrap(
            InstructionKind::ExtensionInstruction {
                name: crate::names::QName::new(nav.namespace_uri(), nav.local_name()),
                lexical: nav.name().to_string(),
                fallback,
            },
            ctx,
            module_id,
            nav,
        ))
    }

    /// Whether an XSLT element that XSLT 2.0 does not allow where it stands
    /// performs §3.9's fallback instead of being a static error: it is in a
    /// region with forwards-compatible behaviour (its own `[xsl:]version`
    /// included) and has an `xsl:fallback` child.
    pub(super) fn falls_back(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<bool, XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        if !self.static_contexts.get(ctx).compat.is_forwards() {
            return Ok(false);
        }
        Ok(self.children(nav, module_id).any(|child| {
            child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "fallback"
        }))
    }

    /// The contents of **every** `xsl:fallback` child, concatenated.
    ///
    /// > \[§18.2.3\] … if the instruction element has one or more
    /// > `xsl:fallback` children, then the content of each of the
    /// > `xsl:fallback` children must be evaluated
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
        } else if !checks::is_instruction(&local) {
            // §3.9: an element in the XSLT namespace "that XSLT 2.0 does not
            // allow … to appear as part of a sequence constructor" — an
            // unknown name, or an XSLT 2.0 element that is not an instruction
            // (`xsl:param` after the first instruction, `xsl:sort`, `xsl:key`,
            // `xsl:when` …) — falls back to its `xsl:fallback` children, or is
            // the static error it would be anyway.
            return self.fallback_or_error(nav, module_id, &local);
        }
        match local.as_str() {
            "apply-templates" => {
                let select = match attr_local(nav, "select") {
                    Some(source) => Some(self.compile_expr(&source, ctx)?),
                    None => None,
                };
                let mode = self.mode_ref(nav, ctx)?;
                if let ModeRef::Named(name) = &mode {
                    self.reject_reserved(name, nav, module)?;
                }
                let sort = self.sort_keys(nav, module_id)?;
                let params = self.with_params(nav, module_id)?;
                Ok(InstructionKind::ApplyTemplates {
                    select,
                    mode,
                    params,
                    sort,
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
                let select = self.required_expr(nav, "select", ctx, &loc)?;
                let sort = self.sort_keys(nav, module_id)?;
                let body = self.sequence_constructor_skipping(nav, module_id, "sort")?;
                Ok(InstructionKind::ForEach { select, sort, body })
            }
            "perform-sort" => self.perform_sort(nav, module_id, ctx, &loc),
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
                let validation = self.validation_of(nav, module, ctx, false)?;
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Element {
                    name,
                    namespace,
                    attr_sets,
                    inherit_namespaces: inherit,
                    validation,
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
                let validation = self.validation_of(nav, module, ctx, false)?;
                let value = self.value_source(nav, module_id, ctx, codes::XTSE0840)?;
                Ok(InstructionKind::Attribute {
                    name,
                    namespace,
                    separator,
                    validation,
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
                validation: self.validation_of(nav, module, ctx, false)?,
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
                let validation = self.validation_of(nav, module, ctx, false)?;
                let body = self.sequence_constructor(nav, module_id)?;
                Ok(InstructionKind::Copy {
                    copy_namespaces: copy_ns,
                    inherit_namespaces: inherit,
                    attr_sets,
                    validation,
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
                    validation: self.validation_of(nav, module, ctx, false)?,
                })
            }
            "variable" => {
                let name = self
                    .declared_name(nav, "name", ctx, module)?
                    .ok_or_else(|| {
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
                // > [ERR XTSE0020] It is a static error if an attribute (other
                // > than an attribute written using curly brackets in a
                // > position where an attribute value template is permitted)
                // > contains a value that is not one of the permitted values.
                // A value with no expression in it is checked here; only an
                // effective value computed at run time is XTDE0030.
                if let Some(fixed) = terminate.as_literal() {
                    parse_yes_no(fixed, "terminate").map_err(|e| e.with_location(loc.clone()))?;
                }
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
            // §13: "A sort key specification may occur immediately within an
            // xsl:apply-templates, xsl:for-each, xsl:perform-sort, or
            // xsl:for-each-group element" — each of which compiles its own
            // xsl:sort children. The element table rejects one anywhere else
            // before compilation starts, so this arm is a safety net.
            "sort" => Err(XsltError::new(
                codes::XTSE0010,
                "xsl:sort is only allowed at the start of xsl:apply-templates, xsl:for-each, \
                 xsl:perform-sort or xsl:for-each-group",
            )
            .with_location(loc)),
            // §16.3.1: xsl:key is a declaration ("Category: declaration"); the
            // element table rejects one anywhere else before compilation
            // starts, so this arm is a safety net.
            "key" => Err(XsltError::new(
                codes::XTSE0010,
                "xsl:key is a declaration and is only allowed as a top-level element",
            )
            .with_location(loc)),
            "for-each-group" => self.for_each_group(nav, module_id, ctx, &loc),
            "number" => self.number(nav, ctx, &loc),
            "analyze-string" => self.analyze_string(nav, module_id, ctx, &loc),
            "result-document" => self.result_document(nav, module_id, ctx, &loc),
            // In a forwards-compatible region an unknown name has already
            // gone to `fallback_or_error` above.
            other => Err(XsltError::new(
                codes::XTSE0010,
                format!("xsl:{other} is not an XSLT instruction"),
            )
            .with_location(loc)),
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
    /// The `None` arm raises the same **static** error as the
    /// non-forwards-compatible branch, which is what §3.9 asks for: deferring it
    /// to evaluation was the XSLT 1.0 rule, and until M2c the compiler kept it
    /// (`XSLT2_M2A_REVIEW_FIXES.md`, "Noticed, not done"). The element table
    /// normally reports this before the instruction is ever compiled
    /// ([`checks`]); the arm is kept so that the rule holds here too rather than
    /// depending on the order the two passes run in.
    fn fallback_or_error(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        name: &str,
    ) -> Result<InstructionKind, XsltError> {
        match self.fallback_bodies(nav, module_id)? {
            Some(body) => Ok(InstructionKind::Choose {
                whens: Vec::new(),
                otherwise: Some(body),
            }),
            None => Err(XsltError::new(
                codes::XTSE0010,
                format!("xsl:{name} is not an XSLT instruction and has no xsl:fallback child"),
            )
            .with_location(self.modules.module(module_id).location(nav))),
        }
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

    /// `xsl:perform-sort` (§13.2).
    ///
    /// > The initial sequence is obtained either by evaluating the `select`
    /// > attribute or by evaluating the contained sequence constructor (but not
    /// > both). If there is no `select` attribute and no sequence constructor
    /// > then the initial sequence (and therefore, the sorted sequence) is an
    /// > empty sequence.
    ///
    /// > \[ERR XTSE1040\] It is a static error if an `xsl:perform-sort`
    /// > instruction with a `select` attribute has any content other than
    /// > `xsl:sort` and `xsl:fallback` instructions.
    fn perform_sort(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let sort = self.sort_keys(nav, module_id)?;
        let select = match attr_local(nav, "select") {
            Some(source) => Some(self.compile_expr(&source, ctx)?),
            None => None,
        };
        if select.is_some() {
            for child in self.children(nav, module_id) {
                let allowed = match child.node_type() {
                    DomNodeType::Element => {
                        child.namespace_uri() == XSLT_NS
                            && matches!(child.local_name(), "sort" | "fallback")
                    }
                    // §4.2, all three bullets — including "Any whitespace text
                    // node whose following-sibling node is an xsl:param or
                    // xsl:sort element is removed from the tree, regardless of
                    // any xml:space attributes": whitespace before or between
                    // the xsl:sort elements is not content even when
                    // `xml:space="preserve"` is in scope.
                    kind if kind.is_text_like() => !checks::is_significant_text(nav, &child),
                    _ => true,
                };
                if !allowed {
                    return Err(XsltError::new(
                        codes::XTSE1040,
                        "xsl:perform-sort with a select attribute may contain only xsl:sort \
                         and xsl:fallback",
                    )
                    .with_location(loc.clone()));
                }
            }
            // Its xsl:fallback children are ignored (§18.2.3), so there is no
            // body to evaluate.
            return Ok(InstructionKind::PerformSort {
                select,
                sort,
                body: Body::default(),
            });
        }
        let body = self.sequence_constructor_skipping(nav, module_id, "sort")?;
        Ok(InstructionKind::PerformSort { select, sort, body })
    }

    /// The sort key specification among the children of `parent` (§13.1): its
    /// `xsl:sort` elements, in order.
    ///
    /// > \[ERR XTSE1015\] It is a static error if an `xsl:sort` element with a
    /// > `select` attribute has non-empty content.
    ///
    /// > \[ERR XTSE1017\] It is a static error if an `xsl:sort` element other
    /// > than the first in a sequence of sibling `xsl:sort` elements has a
    /// > `stable` attribute.
    ///
    /// §13: "xsl:sort elements must occur before any other children", so the
    /// specification is the leading run of `xsl:sort` children
    /// (`leading_run_ends`; `xsl:apply-templates` interleaves them with its
    /// `xsl:with-param` children). An `xsl:sort` after it is not a sort key: the
    /// element table has rejected it, or, with forwards-compatible behaviour
    /// and an `xsl:fallback` child, the sequence constructor performs §3.9's
    /// fallback for it.
    pub(super) fn sort_keys(
        &mut self,
        parent: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<Vec<SortKey>, XsltError> {
        let mut keys = Vec::new();
        for child in self.children(parent, module_id) {
            if leading_run_ends(parent, &child, &["sort", "with-param"]) {
                break;
            }
            if child.node_type() != DomNodeType::Element || child.local_name() != "sort" {
                continue;
            }
            let module = self.modules.module(module_id);
            let ctx = self.context_of(&child, module)?;
            let loc = self.modules.module(module_id).location(&child);
            if !self.static_contexts.get(ctx).compat.is_forwards() {
                check_attributes(&child, "sort", &loc)?;
            }
            if !keys.is_empty() && attr_local(&child, "stable").is_some() {
                return Err(XsltError::new(
                    codes::XTSE1017,
                    "only the first xsl:sort of a sort key specification may have a stable \
                     attribute",
                )
                .with_location(loc));
            }
            let select = attr_local(&child, "select");
            let has_content = self.has_content(&child, module_id);
            let value = match (select, has_content) {
                (Some(_), true) => {
                    return Err(XsltError::new(
                        codes::XTSE1015,
                        "xsl:sort has both a select attribute and content",
                    )
                    .with_location(loc));
                }
                (Some(source), false) => ValueSource::Select(self.compile_expr(&source, ctx)?),
                (None, true) => ValueSource::Content(self.sequence_constructor(&child, module_id)?),
                (None, false) => ValueSource::Select(self.compile_expr(".", ctx)?),
            };
            let order = self.sort_avt(&child, "order", ctx, &loc, |v| {
                SortOrder::parse(v).is_some()
            })?;
            let data_type = self.sort_avt(&child, "data-type", ctx, &loc, |v| {
                SortDataType::parse(v).is_some()
            })?;
            // §5.1 XTSE0280 for a fixed QName value whose prefix the xsl:sort
            // element does not bind (a computed one is XTDE0290, at run time).
            if let Some(prefix) = data_type
                .as_ref()
                .and_then(|avt| avt.as_literal())
                .and_then(SortDataType::qname_prefix)
            {
                if self
                    .static_contexts
                    .get(ctx)
                    .resolve_prefix(prefix)
                    .is_none()
                {
                    return Err(XsltError::new(
                        codes::XTSE0280,
                        format!(
                            "the data-type of xsl:sort uses the prefix {prefix:?}, which is not \
                             declared"
                        ),
                    )
                    .with_location(loc));
                }
            }
            let case_order = self.sort_avt(&child, "case-order", ctx, &loc, |v| {
                parse_case_order(v).is_some()
            })?;
            let lang = self.sort_avt(&child, "lang", ctx, &loc, is_valid_lang)?;
            let collation = self.sort_avt(&child, "collation", ctx, &loc, |_| true)?;
            let stable = self.sort_avt(&child, "stable", ctx, &loc, |v| {
                matches!(whitespace::trim(v), "yes" | "no")
            })?;
            keys.push(SortKey {
                value,
                order,
                data_type,
                case_order,
                lang,
                collation,
                stable,
                ctx,
                loc,
            });
        }
        Ok(keys)
    }

    /// One attribute value template of `xsl:sort`.
    ///
    /// > \[ERR XTSE0020\] It is a static error if an attribute (other than an
    /// > attribute written using curly brackets in a position where an
    /// > attribute value template is permitted) contains a value that is not
    /// > one of the permitted values for that attribute.
    ///
    /// So a value with no expression in it is checked here, statically; a
    /// computed one is checked when it is evaluated (XTDE0030).
    fn sort_avt(
        &mut self,
        nav: &Nav<'t>,
        attr: &str,
        ctx: StaticContextId,
        loc: &Location,
        valid: impl Fn(&str) -> bool,
    ) -> Result<Option<Avt>, XsltError> {
        let Some(text) = attr_local(nav, attr) else {
            return Ok(None);
        };
        let avt = self.avt(&text, ctx).map_err(|e| e.or_location(loc))?;
        if let Some(fixed) = avt.as_literal() {
            if !valid(fixed) {
                return Err(XsltError::new(
                    codes::XTSE0020,
                    format!(
                        "{fixed:?} is not a permitted value of the {attr} attribute of xsl:sort"
                    ),
                )
                .with_location(loc.clone()));
            }
        }
        Ok(Some(avt))
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

    /// The `mode` attribute of `xsl:apply-templates` (§6.3).
    ///
    /// §6.5 scopes XTSE0550 to the *list* of modes an `xsl:template` declares;
    /// `xsl:apply-templates/@mode` names one mode and is a `token`, not an
    /// attribute value template, so a value that is neither an EQName nor
    /// `#default` nor `#current` is §3.4's XTSE0020 — which is what
    /// [`checks::bad_value_code`] reads out of the element table.
    fn mode_ref(&mut self, nav: &Nav<'t>, ctx: StaticContextId) -> Result<ModeRef, XsltError> {
        let _ = &self.names;
        let code = checks::bad_value_code("apply-templates", "mode");
        Ok(match attr_local(nav, "mode") {
            None => ModeRef::Default,
            Some(text) => match whitespace::trim(&text) {
                "#default" => ModeRef::Default,
                "#current" => ModeRef::Current,
                name => ModeRef::Named(self.resolve(name, ctx, code)?),
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
