//! Template dispatch (§6): conflict resolution, the built-in rules, and the
//! parameter binding every invocation goes through.

use std::rc::Rc;

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::patterns::MatchEnv;
use super::{Frame, Invocation, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::QName;
use crate::receiver::{Nav, Receiver, SequenceCollector};

impl<'a, 't> Transformation<'a, 't> {
    // ── Template dispatch (§6) ───────────────────────────────────────────

    pub(super) fn apply_to_items(
        &self,
        items: Vec<XmlItem<Nav<'t>>>,
        mode: ModeKey,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        // > [ERR XTTE0520] It is a type error if the sequence returned by the
        // > select expression contains an item that is not a node.
        if let Some(XmlItem::Atomic(value)) = items.iter().find(|i| matches!(i, XmlItem::Atomic(_)))
        {
            return Err(XsltError::new(
                codes::XTTE0520,
                format!(
                    "xsl:apply-templates selected an atomic value ({}) where only nodes are allowed",
                    value.to_string_value()
                ),
            ));
        }
        let size = items.len();
        for (index, item) in items.into_iter().enumerate() {
            let mut inner = cx.with_focus(item.clone(), index + 1, size);
            inner.mode = mode.clone();
            match self.select_rule(&item, &mode)? {
                Some(id) => {
                    inner.current_rule = Some(id);
                    self.invoke(id.template, params.clone(), &inner, out)?;
                }
                None => self.builtin_rule(&item, &mode, &params, &inner, out)?,
            }
        }
        Ok(())
    }

    /// §6.6 Built-in Template Rules.
    ///
    /// > The built-in template rule for element and document nodes is
    /// > equivalent to `<xsl:template match="*|/"><xsl:apply-templates/>`,
    /// > with the additional property that parameters are passed through.
    /// > … The built-in template rule for text and attribute nodes is
    /// > equivalent to `<xsl:template match="text()|@*"><xsl:value-of
    /// > select="."/>`. … The built-in template rule for comment and
    /// > processing instruction nodes is to do nothing.
    fn builtin_rule(
        &self,
        item: &XmlItem<Nav<'t>>,
        mode: &ModeKey,
        params: &[(QName, XPathValue<Nav<'t>>, bool)],
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match item {
            XmlItem::Atomic(value) => out.text(&value.to_string_value(), false),
            XmlItem::Node(nav) => match nav.node_type() {
                DomNodeType::Root | DomNodeType::Element => {
                    // The built-in rule recurses into the children without a
                    // template invocation to count, so over a deep enough
                    // document only the stack guard stops it (Q33).
                    self.check_stack()?;
                    let children = children_of(nav);
                    self.apply_to_items(children, mode.clone(), params.to_vec(), cx, out)
                }
                DomNodeType::Attribute
                | DomNodeType::Text
                | DomNodeType::Whitespace
                | DomNodeType::SignificantWhitespace => out.text(&nav.value(), false),
                _ => Ok(()),
            },
        }
    }

    /// §6.4 conflict resolution: the candidate list is pre-sorted by
    /// (precedence, priority, declaration order), all descending, so the first
    /// match wins.
    ///
    /// A candidate is one *alternative* of a declaration's pattern, because
    /// "if the pattern contains multiple alternatives separated by `|`, then
    /// the template rule is treated equivalently to a set of template rules,
    /// one for each alternative".
    fn select_rule(
        &self,
        item: &XmlItem<Nav<'t>>,
        mode: &ModeKey,
    ) -> Result<Option<RuleId>, XsltError> {
        let XmlItem::Node(nav) = item else {
            return Ok(None);
        };
        let candidates = self.candidates(nav, mode);
        let mut matched: Option<RuleId> = None;
        for id in candidates {
            if self.rule_matches(id, nav)? {
                match matched {
                    None => {
                        matched = Some(id);
                        if !self.options.strict_recoverable {
                            break;
                        }
                    }
                    Some(first) => {
                        // §6.4: "However, it is not an error if a node matches
                        // more than one of the alternatives" — two rules of
                        // the *same* declaration never conflict.
                        if first.template == id.template {
                            continue;
                        }
                        let (pa, ra, _) = self.sheet.rule_rank(first);
                        let (pb, rb, _) = self.sheet.rule_rank(id);
                        if pa == pb && ra == rb {
                            // > [ERR XTRE0540] It is a recoverable dynamic
                            // > error if the conflict resolution algorithm for
                            // > template rules leaves more than one matching
                            // > template rule.
                            return Err(XsltError::new(
                                codes::XTRE0540,
                                format!(
                                    "two template rules of the same import precedence and \
                                     priority match {}",
                                    nav.name()
                                ),
                            ));
                        }
                        break;
                    }
                }
            }
        }
        Ok(matched)
    }

    /// Whether one rule — one alternative of one declaration's pattern —
    /// matches `nav`.
    fn rule_matches(&self, id: RuleId, nav: &Nav<'t>) -> Result<bool, XsltError> {
        let Some(pattern_id) = self.sheet.rule(id).pattern else {
            return Ok(false);
        };
        let pattern = self.sheet.pattern(pattern_id);
        let Some(alternative) = pattern.alternatives.get(id.alternative as usize) else {
            return Ok(false);
        };
        let empty = Frame::default();
        self.guard_recoverable(self.matches_alternative(
            alternative,
            nav,
            nav,
            MatchEnv::top_level(&empty),
        ))
    }

    /// The rules of `mode` that could match `nav`, in conflict-resolution
    /// order (§8.5).
    ///
    /// The mode's own index and the `mode="#all"` index are both consulted:
    /// §6.5 makes an `#all` rule applicable to "every mode that is named in an
    /// `xsl:apply-templates` instruction or `xsl:template` declaration
    /// anywhere in the stylesheet", and a mode named only by an
    /// `xsl:apply-templates` has no index of its own.
    fn candidates(&self, nav: &Nav<'t>, mode: &ModeKey) -> Vec<RuleId> {
        let mut candidates: Vec<RuleId> = Vec::new();
        let mut collect = |index: &ModeIndex| {
            let name = QName::new(nav.namespace_uri(), nav.local_name());
            let buckets: [Option<&Vec<RuleId>>; 3] = match nav.node_type() {
                DomNodeType::Attribute => [
                    index.by_attribute.get(&name),
                    index.by_namespace.get(nav.namespace_uri()),
                    index.by_local.get(nav.local_name()),
                ],
                DomNodeType::Element => [
                    index.by_element.get(&name),
                    index.by_namespace.get(nav.namespace_uri()),
                    index.by_local.get(nav.local_name()),
                ],
                _ => [None, None, None],
            };
            for bucket in buckets.into_iter().flatten() {
                candidates.extend(bucket.iter().copied());
            }
            candidates.extend(index.other.iter().copied());
        };
        if let Some(index) = self.sheet.modes().index(mode) {
            collect(index);
        }
        collect(self.sheet.modes().all_modes());
        candidates.sort_by(|a, b| {
            let (pa, ra, da) = self.sheet.rule_rank(*a);
            let (pb, rb, db) = self.sheet.rule_rank(*b);
            pb.cmp(&pa)
                .then(rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal))
                .then(db.cmp(&da))
        });
        candidates.dedup();
        candidates
    }

    /// §6.7 `xsl:apply-imports` and `xsl:next-match`.
    ///
    /// > `xsl:apply-imports` … searches only the template rules that were
    /// > defined in stylesheet modules imported (directly or indirectly) by
    /// > the stylesheet module containing the current template rule …
    /// > `xsl:next-match` … considers all the template rules that were
    /// > eliminated on the grounds that they had lower import precedence or
    /// > lower priority.
    pub(super) fn override_rule(
        &self,
        only_imports: bool,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let Some(current) = cx.current_rule else {
            // > [ERR XTDE0560] It is a non-recoverable dynamic error if
            // > xsl:apply-imports or xsl:next-match is evaluated when the
            // > current template rule is null.
            return Err(XsltError::new(
                codes::XTDE0560,
                "there is no current template rule to override",
            ));
        };
        let Some(XmlItem::Node(nav)) = cx.item.clone() else {
            return Err(XsltError::new(
                "XPDY0002",
                "there is no context node to re-match",
            ));
        };
        let rule = self.sheet.rule(current);
        // §6.7: "the xsl:next-match instruction considers all other template
        // rules of lower import precedence and/or priority". The comparison is
        // on the conflict-resolution key of the *current rule*, not on its
        // position in this node's candidate list — the context item may be a
        // different node from the one the rule matched, and a rank test always
        // terminates. Since §6.4 makes each `|` alternative a rule of its own,
        // a lower-priority alternative of the very same declaration is a
        // legitimate next match.
        let current_rank = self.sheet.rule_rank(current);
        for id in self.candidates(&nav, &cx.mode) {
            let candidate = self.sheet.rule(id);
            let candidate_rank = self.sheet.rule_rank(id);
            let lower = candidate_rank.0 < current_rank.0
                || (candidate_rank.0 == current_rank.0
                    && (candidate_rank.1 < current_rank.1
                        || (candidate_rank.1 == current_rank.1
                            && candidate_rank.2 < current_rank.2)));
            if !lower {
                continue;
            }
            if only_imports
                && !(candidate.precedence >= rule.subtree_min
                    && candidate.precedence < rule.precedence)
            {
                continue;
            }
            if self.rule_matches(id, &nav)? {
                let mut inner = cx.clone();
                inner.current_rule = Some(id);
                return self.invoke(id.template, params, &inner, out);
            }
        }
        // §6.7: "If there is no matching template rule, the built-in template
        // rule for the relevant node kind is used."
        self.builtin_rule(&XmlItem::Node(nav), &cx.mode, &params, cx, out)
    }

    fn invoke(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        self.invoke_as(id, params, cx, out, Invocation::ApplyTemplates)
    }

    pub(super) fn invoke_as(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        let depth = self.enter(1)?;
        let result = self.invoke_body(id, params, cx, out, how);
        self.depth.set(depth - 1);
        result
    }

    /// Counts one more level of template invocation or function call against
    /// `TransformOptions::max_recursion_depth` (Q33), checks the stack guard
    /// (`TransformOptions::stack_limit`), and returns the new depth, which the
    /// caller restores to `depth - weight` when the call returns.
    ///
    /// `weight` is 1 for a template and `calls::FUNCTION_CALL_DEPTH` for a
    /// stylesheet function, whose calls take more host stack each.
    pub(super) fn enter(&self, weight: usize) -> Result<usize, XsltError> {
        let depth = self.depth.get() + weight;
        if depth > self.options.max_recursion_depth {
            return Err(XsltError::internal(format!(
                "template invocation nested more than {} deep (a stylesheet-function call \
                 counts as {}); the stylesheet is probably recursing without end \
                 (TransformOptions::max_recursion_depth)",
                self.options.max_recursion_depth,
                super::calls::FUNCTION_CALL_DEPTH
            )));
        }
        self.check_stack()?;
        self.depth.set(depth);
        Ok(depth)
    }

    /// The stack guard (Q33): fails with the depth error once the run has used
    /// more stack, measured from where it started, than its limit allows.
    ///
    /// Checked by [`Self::enter`] and wherever else the run nests on the host
    /// stack with no template invocation or function call in between: the
    /// built-in template rules, a key index build (`runtime/keys.rs`), the
    /// evaluation of a global variable, the expansion of an attribute set
    /// that uses another, the compilation of a computed regular expression
    /// (`runtime/analyze.rs`).
    ///
    /// A count of nested calls cannot stand in for this, because the stack one
    /// call uses depends on its body — an `xsl:variable` holding nested literal
    /// result elements, nested `xsl:if` / `xsl:for-each`, nested XPath `if` and
    /// `for` all deepen it — so a heavy enough body would overflow the stack
    /// before any fixed count is reached.
    pub(super) fn check_stack(&self) -> Result<(), XsltError> {
        let guard = self.stack.get();
        if guard.base == 0 {
            return Ok(());
        }
        let used = guard.base.abs_diff(super::stack_position());
        if used > guard.limit {
            return Err(XsltError::internal(format!(
                "evaluation nested so deep that the run used {used} bytes of stack, \
                 more than the {} it may use; the stylesheet is probably recursing without \
                 end (TransformOptions::stack_limit, TransformOptions::max_recursion_depth)",
                guard.limit
            )));
        }
        Ok(())
    }

    /// `TransformOptions::stack_limit` for a run whose stack is measured from
    /// `base`: the explicit option, else what the thread allows (see the
    /// option's documentation).
    pub(super) fn stack_limit_from(&self, base: usize) -> usize {
        if let Some(limit) = self.options.stack_limit {
            return limit;
        }
        match super::LARGE_STACK.with(std::cell::Cell::get) {
            Some((top, size)) => {
                let in_use = top.abs_diff(base);
                // One eighth, at least 1 MiB — but never more than half, so
                // that a small worker stack still runs shallow stylesheets.
                let reserve = (size / 8).max(super::MIN_STACK_RESERVE.min(size / 2));
                size.saturating_sub(in_use).saturating_sub(reserve)
            }
            None => super::OTHER_THREAD_STACK_LIMIT,
        }
    }

    fn invoke_body(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        let rule = self.sheet.template(id);
        let mut frame = Frame::with_slots(rule.frame_size);
        let mut inner = cx.clone();
        // Tunnel parameters travel through every call (§10.1.2).
        let tunnelled: Vec<_> = params.iter().filter(|(_, _, t)| *t).collect();
        if !tunnelled.is_empty() {
            let mut map = (*cx.tunnel).clone();
            for (name, value, _) in tunnelled {
                map.insert(name.clone(), value.clone());
            }
            inner.tunnel = Rc::new(map);
        }
        self.bind_params(&rule.params, &params, &mut frame, &inner, how)?;
        if rule.as_type.is_some() {
            let mut collector = self.collector();
            self.eval_body(&rule.body, &mut frame, &inner, &mut collector)?;
            let zero_length_texts = collector.zero_length_text_nodes();
            let value = XPathValue::from_sequence(collector.into_items());
            let value = self.convert(value, rule.as_type.as_ref(), codes::XTTE0505)?;
            for item in value.into_vec() {
                out.append_item(&item)?;
            }
            // The zero-length text nodes of the result, which the collector
            // could not keep as items: in content they vanish (§5.7.1), but
            // they still make the initial template's result non-empty (§2.4).
            return SequenceCollector::replay_zero_length_text_nodes(zero_length_texts, out);
        }
        self.eval_body(&rule.body, &mut frame, &inner, out)
    }

    pub(super) fn call_template(
        &self,
        id: TemplateId,
        params: Vec<(QName, XPathValue<Nav<'t>>, bool)>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        // §9.1: xsl:call-template preserves the focus and the current rule.
        self.invoke_as(id, params, cx, out, Invocation::CallTemplate)
    }

    fn bind_params(
        &self,
        declared: &[ParamDecl],
        supplied: &[(QName, XPathValue<Nav<'t>>, bool)],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        how: Invocation,
    ) -> Result<(), XsltError> {
        for param in declared {
            let value = if param.tunnel {
                cx.tunnel.get(&param.name).cloned()
            } else {
                supplied
                    .iter()
                    .find(|(name, _, tunnel)| !*tunnel && *name == param.name)
                    .map(|(_, value, _)| value.clone())
            };
            let value = match value {
                Some(value) => self.convert(value, param.as_type.as_ref(), codes::XTTE0590)?,
                None if param.required => {
                    // §10.1.1 distinguishes the three ways a template can be
                    // entered: XTSE0690 for xsl:call-template (a static
                    // error), XTDE0060 for the initial template, XTDE0700 for
                    // everything else.
                    let code = match how {
                        Invocation::CallTemplate if !param.tunnel => codes::XTSE0690,
                        Invocation::InitialTemplate => codes::XTDE0060,
                        _ => codes::XTDE0700,
                    };
                    return Err(XsltError::new(
                        code,
                        format!(
                            "no value was supplied for the required parameter {}",
                            param.name
                        ),
                    )
                    .or_location(&param.loc));
                }
                None => self
                    .param_default(&param.name, &param.value, param.as_type.as_ref(), frame, cx)
                    .map_err(|e| e.or_location(&param.loc))?,
            };
            frame.set(param.slot, value);
        }
        Ok(())
    }

    pub(super) fn eval_with_params(
        &self,
        params: &[WithParam],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<(QName, XPathValue<Nav<'t>>, bool)>, XsltError> {
        let mut out = Vec::with_capacity(params.len());
        for param in params {
            let value = self.var_value(
                &param.value,
                param.as_type.as_ref(),
                frame,
                cx,
                codes::XTTE0590,
            )?;
            out.push((param.name.clone(), value, param.tunnel));
        }
        Ok(out)
    }
}

pub(super) fn children_of<'t>(nav: &Nav<'t>) -> Vec<XmlItem<Nav<'t>>> {
    let mut out = Vec::new();
    let mut child = nav.clone();
    if child.move_to_first_child() {
        loop {
            out.push(XmlItem::Node(child.clone()));
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    out
}
