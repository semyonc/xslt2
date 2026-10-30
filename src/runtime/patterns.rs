//! Matching a node against a pattern (§5.5.3), walking the pattern right to
//! left from the candidate node.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{XPathContext, XPathError, XmlItem};

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::error::XsltError;
use crate::model::{ExprId, VarLocation};
use crate::names::{QName, FN_NS};
use crate::pattern::{
    Anchor, Connector, IdKeyAnchor, NodeTest, PathPattern, Pattern, PatternAxis, PatternStep,
};
use crate::receiver::Nav;

/// The nodes the `id(…)` / `key(…)` call of an IdKeyPattern selects, by the
/// call and the serial number of the document it was evaluated in — filled
/// the first time a candidate of that document is matched against the
/// pattern, and kept for the run (see [`Transformation::id_key_selection`]).
pub(super) type IdKeySelections<'t> = HashMap<(ExprId, u64), Rc<SelectedNodes<'t>>>;

/// The nodes an IdKeyPattern's call selected in one document, as a set.
pub(super) struct SelectedNodes<'t> {
    /// Every selected node that is not a namespace node, by its index in the
    /// document's node array (`current_ref`), which identifies such a node
    /// within its document — it is what `generate-id()` is built from.
    indexed: HashSet<u32>,
    /// Selected namespace nodes, which share the index of their element and
    /// are compared by identity instead. (`id()` selects elements and a key
    /// never indexes a namespace node, so this stays empty in practice.)
    namespaces: Vec<Nav<'t>>,
}

impl<'t> SelectedNodes<'t> {
    fn new(nodes: impl IntoIterator<Item = Nav<'t>>) -> Self {
        let mut set = SelectedNodes {
            indexed: HashSet::new(),
            namespaces: Vec::new(),
        };
        for node in nodes {
            if node.node_type() == DomNodeType::Namespace {
                set.namespaces.push(node);
            } else {
                set.indexed.insert(node.current_ref());
            }
        }
        set
    }

    /// Whether `node`, a node of the document the set was made for, is in it.
    fn contains(&self, node: &Nav<'t>) -> bool {
        if node.node_type() == DomNodeType::Namespace {
            self.namespaces.iter().any(|n| n.is_same_position(node))
        } else {
            self.indexed.contains(&node.current_ref())
        }
    }
}

impl<'a, 't> Transformation<'a, 't> {
    // ── Patterns (§5.5.3) ────────────────────────────────────────────────

    /// Whether `node` matches `pattern`.
    ///
    /// > \[Definition: A node matches a pattern if the pattern, evaluated as
    /// > an expression with the node as the context node, selects a sequence
    /// > that includes the node.\]
    ///
    /// §5.5.4 makes a dynamic error raised while matching recoverable: "the
    /// optional recovery action is to treat the pattern as not matching".
    /// `TransformOptions::strict_recoverable` signals it instead.
    pub(crate) fn matches(&self, pattern: &Pattern, node: &Nav<'t>) -> Result<bool, XsltError> {
        let empty = Frame::default();
        self.matches_with(pattern, node, MatchEnv::top_level(&empty))
    }

    /// [`matches`](Self::matches) for a pattern of an instruction, within one
    /// evaluation of that instruction: the local variables of the scope's
    /// frame are in scope for the pattern's predicates, and the matcher may
    /// remember for the rest of the evaluation what depends on them
    /// ([`EvaluationScope`]).
    ///
    /// A `match` pattern of `xsl:template` or `xsl:key` is a top-level
    /// attribute and sees the global variables only, so its
    /// frame is empty; a `group-starting-with` / `group-ending-with` pattern
    /// is an attribute of an instruction and sees every variable in scope
    /// there (§9.7: "the scope of a local variable … is all following
    /// siblings of the variable-binding element, and their descendants") —
    /// which is the frame the instruction runs in (`for-each-group-071`
    /// refers to a function parameter). The `count` / `from` patterns of
    /// `xsl:number` are the same kind of pattern and come through here too,
    /// with a scope per evaluation of the instruction.
    pub(super) fn matches_in(
        &self,
        pattern: &Pattern,
        node: &Nav<'t>,
        scope: &EvaluationScope<'_, 't>,
    ) -> Result<bool, XsltError> {
        self.matches_with(pattern, node, scope.env())
    }

    fn matches_with(
        &self,
        pattern: &Pattern,
        node: &Nav<'t>,
        env: MatchEnv<'_, 't>,
    ) -> Result<bool, XsltError> {
        // A pattern's predicates may call stylesheet functions, so a direct
        // call measures the stack it uses from here, as a run does.
        let _stack = self.stack_scope(super::stack_position());
        for alternative in &pattern.alternatives {
            // The node handed in is both the candidate and, per §16.6.1, the
            // node `current()` reports throughout the match.
            match self.guard_recoverable(self.matches_alternative(alternative, node, node, env)) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    /// §5.5.4: an error raised while matching is recoverable, and the recovery
    /// action is to treat the pattern as not matching.
    ///
    /// > Any dynamic error or type error that occurs during the evaluation of a
    /// > pattern against a particular node is treated as a recoverable error,
    /// > even if the error would normally be non-recoverable. The optional
    /// > recovery action is to treat the pattern as not matching.
    ///
    /// The licence covers "any dynamic error or type error" **of the
    /// stylesheet** and nothing else, which is what
    /// [`XsltError::is_recoverable_in_pattern`] decides. A missing feature
    /// (`XSLT2-NOTIMPL`) and a broken invariant (`XSLT2-INTERNAL`) are not
    /// errors the specification knows about: recovering from one of them would
    /// not report that this pattern does not match — it would report that this
    /// processor cannot tell whether it matches, and then answer "no" anyway,
    /// selecting a different template rule and producing a wrong result with no
    /// diagnostic at all. They therefore propagate, in both modes.
    ///
    /// `TransformOptions::strict_recoverable` additionally signals the errors
    /// that *are* recoverable instead of taking the recovery action.
    pub(super) fn guard_recoverable(
        &self,
        result: Result<bool, XsltError>,
    ) -> Result<bool, XsltError> {
        match result {
            Ok(matched) => Ok(matched),
            Err(e) if self.options.strict_recoverable => Err(e),
            Err(e) if !e.is_recoverable_in_pattern() => Err(e),
            Err(_) => Ok(false),
        }
    }

    /// Whether `node` matches one alternative.
    ///
    /// `current` is the node the *whole* pattern is being matched against.
    /// §16.6.1: "If the current function is used within a pattern, its value
    /// is the node that is being matched against the pattern" — so it stays
    /// fixed while the match walks left through ancestors, and is what every
    /// predicate of every step sees, whatever node that step is testing.
    pub(super) fn matches_alternative(
        &self,
        alternative: &PathPattern,
        node: &Nav<'t>,
        current: &Nav<'t>,
        env: MatchEnv<'_, 't>,
    ) -> Result<bool, XsltError> {
        if let Anchor::IdKey(anchor) = alternative.anchor {
            // §5.5.2: "These patterns will never match a node in a tree whose
            // root is not a document node." §5.5.4 makes that "a non-match,
            // rather than a dynamic error" — so the call, which would raise
            // XTDE1270 (`key`) or FODC0001 (`id`) there, is not even made.
            let mut root = node.clone();
            root.move_to_root();
            if root.node_type() != DomNodeType::Root {
                return Ok(false);
            }
            if alternative.steps.is_empty() {
                // The call alone: the node must be one it selects.
                return Ok(self
                    .id_key_selection(anchor, node, current, env)?
                    .contains(node));
            }
        } else if alternative.steps.is_empty() {
            // The pattern `/`.
            return Ok(node.node_type() == DomNodeType::Root);
        }
        self.matches_from(alternative, 0, node, current, env)
    }

    /// The nodes the `id(…)` / `key(…)` call of an IdKeyPattern selects in the
    /// tree of `node` (§5.5.2).
    ///
    /// §5.5.3 evaluates `root(.)//(EE)` with a singleton focus on the
    /// candidate: the call runs with a context node in the candidate's tree,
    /// and the candidate is such a node. The argument is a literal or a
    /// variable reference, so nothing else of the focus can be observed;
    /// `current()` is still the node being matched (§16.6.1).
    ///
    /// The result is therefore the same for every candidate of one document:
    /// §5.5.2 allows only "a literal or a reference to a variable or
    /// parameter" as the value and "a string literal" as the key name, and
    /// `id()` and two-argument `key()` search "the document containing the
    /// context node". So it is computed once per (call, document) and kept for
    /// the run as a set — as long as every variable the call reads is global
    /// (a global's value is fixed for the run). A call that reads a local
    /// variable — possible only in a pattern of an instruction, since
    /// template and key patterns see only globals — is kept for one
    /// evaluation of the instruction instead ([`EvaluationScope`]), and
    /// evaluated per candidate when the pattern is matched outside one. An
    /// error is not kept: it is raised for this candidate (and a key whose
    /// index build failed remembers that itself, `runtime/keys.rs`).
    fn id_key_selection(
        &self,
        anchor: IdKeyAnchor,
        node: &Nav<'t>,
        current: &Nav<'t>,
        env: MatchEnv<'_, 't>,
    ) -> Result<Rc<SelectedNodes<'t>>, XsltError> {
        let slot = (anchor.call, node.document().serial());
        // The grammar (§5.5.2) leaves the call nothing to read but a variable,
        // so `standard_functions_only` holds for every compiled one; it is
        // checked so that the scope admits a call by the rule it applies to
        // predicates.
        let table = if self.reads_only_globals(anchor.call) {
            Some(&self.id_key_selections)
        } else {
            env.scope
                .filter(|_| self.standard_functions_only(anchor.call))
                .map(|scope| &scope.selections)
        };
        if let Some(table) = table {
            if let Some(selected) = table.borrow().get(&slot) {
                return Ok(Rc::clone(selected));
            }
        }
        let mut cx = XsltContext::empty();
        cx.item = Some(XmlItem::Node(node.clone()));
        cx.position = 1;
        cx.size = 1;
        cx.current = Some(XmlItem::Node(current.clone()));
        // The table is not borrowed here: the call may build a key index,
        // whose patterns may match other IdKeyPatterns.
        let value = self.eval_expr(anchor.call, env.frame, &cx)?;
        let selected = Rc::new(SelectedNodes::new(value.into_vec().into_iter().filter_map(
            |item| match item {
                XmlItem::Node(nav) => Some(nav),
                XmlItem::Atomic(_) => None,
            },
        )));
        if let Some(table) = table {
            table.borrow_mut().insert(slot, Rc::clone(&selected));
        }
        Ok(selected)
    }

    /// Whether every variable the expression `id` reads is a global variable
    /// or parameter.
    pub(super) fn reads_only_globals(&self, id: ExprId) -> bool {
        let compiled = self.sheet.expr(id);
        compiled.vars.iter().enumerate().all(|(slot, location)| {
            matches!(location, VarLocation::Global(_))
                || !compiled.expr.references_external_var(slot as u32)
        })
    }

    fn matches_from(
        &self,
        alternative: &PathPattern,
        index: usize,
        node: &Nav<'t>,
        current: &Nav<'t>,
        env: MatchEnv<'_, 't>,
    ) -> Result<bool, XsltError> {
        let step = &alternative.steps[index];
        if !self.step_matches(step, node, current, env)? {
            return Ok(false);
        }
        if index + 1 == alternative.steps.len() {
            return Ok(match alternative.anchor {
                Anchor::None => true,
                Anchor::Root => {
                    let mut parent = node.clone();
                    parent.move_to_parent() && parent.node_type() == DomNodeType::Root
                }
                Anchor::RootDescendant => {
                    let mut root = node.clone();
                    root.move_to_root();
                    root.node_type() == DomNodeType::Root
                }
                // `id(…)/…`: the parent of the leftmost step is selected by the
                // call; `id(…)//…`: some ancestor is.
                Anchor::IdKey(anchor) => {
                    let mut ancestor = node.clone();
                    if !ancestor.move_to_parent() {
                        return Ok(false);
                    }
                    let selected = self.id_key_selection(anchor, node, current, env)?;
                    let selects = |n: &Nav<'t>| selected.contains(n);
                    match anchor.connector {
                        Connector::Parent => selects(&ancestor),
                        Connector::Ancestor => loop {
                            if selects(&ancestor) {
                                break true;
                            }
                            if !ancestor.move_to_parent() {
                                break false;
                            }
                        },
                    }
                }
            });
        }
        match step.connector {
            Connector::Parent => {
                let mut parent = node.clone();
                if !parent.move_to_parent() {
                    return Ok(false);
                }
                self.matches_from(alternative, index + 1, &parent, current, env)
            }
            Connector::Ancestor => {
                let mut ancestor = node.clone();
                while ancestor.move_to_parent() {
                    if self.matches_from(alternative, index + 1, &ancestor, current, env)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    /// Whether `node` satisfies one step: its axis, its node test, and every
    /// predicate.
    ///
    /// §5.5.3 defines the step as part of `root(.)//(EE)`, so its predicates
    /// apply, left to right, to the sequence the node test selects among the
    /// node's siblings — XPath 2.0 §3.2.2: "In the case of multiple adjacent
    /// predicates, the predicates are applied from left to right, and the
    /// result of applying each predicate serves as the input sequence for the
    /// following predicate." Call that input sequence of predicate *i* S*i*
    /// (S0 is what the node test selects). The node matches the step if it is
    /// still in the sequence the last predicate leaves.
    ///
    /// Each predicate is decided for **this node alone**, which gives
    /// the same answer as filtering the whole of S*i* — the node is in
    /// S*i+1* exactly when the predicate holds for it with its own focus in
    /// S*i* — and needs S*i* only for that focus:
    ///
    /// - A predicate that reads neither `position()` nor `last()` in its own
    ///   focus (`XPathExpr::uses_position` / `uses_last`, which do not count a
    ///   `position()` inside a nested predicate or a later path step) has the
    ///   same value whatever the position and size, so it is evaluated with
    ///   the node as a singleton focus. If that value is not a singleton
    ///   number, its effective boolean value decides and S*i* is never built.
    ///   If it is one, §3.2.2's numeric-predicate rule compares it with the
    ///   context position, and the node's position in S*i* is looked up
    ///   ([`Transformation::placement`]).
    /// - A predicate that does read `position()` or `last()` is evaluated once,
    ///   for this node, with its position in S*i* and the size of S*i*.
    ///
    /// The difference from filtering S*i* whole is which errors are seen: an
    /// error the predicate raises for *another* sibling is no longer raised
    /// for this node. XPath 2.0 §2.3.4 permits it — "the processor is free to
    /// stop examining further items in a sequence as soon as it can establish
    /// that further items would not affect the result except possibly by
    /// causing an error" — and §5.5.4 confines a pattern's errors to "the
    /// evaluation of a pattern against a particular node" anyway. An error in
    /// a predicate *before* a positional one still reaches every sibling whose
    /// position depends on it, because building S*i* evaluates it for all of
    /// them.
    fn step_matches(
        &self,
        step: &PatternStep,
        node: &Nav<'t>,
        current: &Nav<'t>,
        env: MatchEnv<'_, 't>,
    ) -> Result<bool, XsltError> {
        let frame = env.frame;
        if !node_test_matches(&step.test, step.axis, node, &self.sheet.match_context) {
            return Ok(false);
        }
        // The sibling sequence built for this candidate, if one had to be.
        let mut built = None;
        for (level, &predicate) in step.predicates.iter().enumerate() {
            let expr = &self.sheet.expr(predicate).expr;
            let holds = if expr.uses_position() || expr.uses_last() {
                match self.placement(step, level, node, current, env, &mut built)? {
                    Some((position, size)) => {
                        let value =
                            self.eval_predicate(predicate, node, position, size, current, frame)?;
                        predicate_holds(&value, position).map_err(|e| self.xpath_error(e))?
                    }
                    None => false,
                }
            } else {
                // The position and size handed in cannot be observed: the
                // flags miss only what a custom function reads, and none of
                // this crate's does — the §16 functions read the context item
                // and `current()`, never the position or size, and a
                // stylesheet function's body has no focus (§10.3).
                let value = self.eval_predicate(predicate, node, 1, 1, current, frame)?;
                if numeric_predicate(&value).is_some() {
                    match self.placement(step, level, node, current, env, &mut built)? {
                        Some((position, _)) => {
                            predicate_holds(&value, position).map_err(|e| self.xpath_error(e))?
                        }
                        None => false,
                    }
                } else {
                    effective_boolean_value(&value).map_err(|e| self.xpath_error(e))?
                }
            };
            if !holds {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Evaluates one pattern predicate with `item` as the context item at
    /// `position` of `size`.
    ///
    /// `current` is not `item`: `current()` inside a pattern is the node the
    /// pattern as a whole is being matched against (§16.6.1).
    fn eval_predicate(
        &self,
        predicate: ExprId,
        item: &Nav<'t>,
        position: usize,
        size: usize,
        current: &Nav<'t>,
        frame: &Frame<'t>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let mut cx = XsltContext::empty();
        cx.item = Some(XmlItem::Node(item.clone()));
        cx.position = position;
        cx.size = size;
        cx.current = Some(XmlItem::Node(current.clone()));
        self.eval_expr(predicate, frame, &cx)
    }

    /// The position of `node` in S`level` — the sequence predicate `level` of
    /// `step` is applied to — and the size of that sequence, or `None` if the
    /// node is not in it.
    ///
    /// S`level` is S0 filtered by the predicates before `level`, each applied
    /// to the whole sequence exactly as §3.2.2 describes, every member with its
    /// own focus. A node with no parent is its own S0 (`child-or-top` /
    /// `attribute-or-top` select the context node itself, §5.5.3).
    ///
    /// For a node with a parent the result is remembered for the run, per
    /// (predicate, parent), when nothing the earlier predicates read can differ
    /// between two siblings' matches — see `candidate_independent` and
    /// [`PlacementCache`]. Then every sibling after the first finds its
    /// position in constant time, and `item[1]` or `item[@a][2]` costs one pass
    /// over the siblings per parent instead of one per sibling.
    ///
    /// When it cannot be remembered for the run but the pattern is matched
    /// within one evaluation of an instruction, it is remembered for that
    /// evaluation instead if nothing the earlier predicates read can differ
    /// between two candidates of the evaluation — see `evaluation_independent`
    /// and [`EvaluationScope`]: a predicate that reads a local variable of the
    /// instruction (`r[@k = $v][1]` in `group-starting-with`) is linear again.
    ///
    /// Otherwise the sequence is built for this candidate and kept in `built`
    /// (the level it is for, and its members) for the step's later predicates,
    /// which continue from it instead of starting again from S0 — so a step
    /// with several positional predicates still filters the siblings once per
    /// candidate.
    fn placement(
        &self,
        step: &PatternStep,
        level: usize,
        node: &Nav<'t>,
        current: &Nav<'t>,
        env: MatchEnv<'_, 't>,
        built: &mut Option<(usize, Vec<Nav<'t>>)>,
    ) -> Result<Option<(usize, usize)>, XsltError> {
        let frame = env.frame;
        let mut parent = node.clone();
        if !parent.move_to_parent() {
            let members = self.apply_predicates(
                &step.predicates[..level],
                vec![node.clone()],
                current,
                frame,
            )?;
            return Ok(locate(&members, node));
        }
        let predicate = step.predicates[level];
        let slot = (parent.document().serial(), parent.current_ref());
        // No borrow of the cache is held while predicates are evaluated: a
        // predicate may call `key()`, whose index build matches patterns of
        // its own.
        match self.cached_placement(step, level, slot) {
            Lookup::Hit(placement) => Ok(placement.locate(node)),
            Lookup::Miss => {
                let members = self.apply_predicates(
                    &step.predicates[..level],
                    self.node_test_siblings(step, &parent, node),
                    current,
                    frame,
                )?;
                let placement = Rc::new(Placement::new(&members));
                let found = placement.locate(node);
                self.remember_placement(predicate, slot, placement);
                Ok(found)
            }
            Lookup::Uncacheable => {
                if let Some(scope) = env
                    .scope
                    .filter(|scope| self.scope_admits(scope, step, level))
                {
                    return self.scoped_placement(scope, step, level, &parent, node, current);
                }
                // The levels only increase within one step, so a sequence
                // built for an earlier predicate is a prefix of this work.
                let (from, members) = match built.take() {
                    Some((from, members)) if from <= level => (from, members),
                    _ => (0, self.node_test_siblings(step, &parent, node)),
                };
                let members =
                    self.apply_predicates(&step.predicates[from..level], members, current, frame)?;
                let found = locate(&members, node);
                *built = Some((level, members));
                Ok(found)
            }
        }
    }

    /// Whether `scope` may remember S`level` of `step` for the rest of its
    /// evaluation (`evaluation_independent`) — decided once per predicate and
    /// evaluation.
    fn scope_admits(
        &self,
        scope: &EvaluationScope<'_, 't>,
        step: &PatternStep,
        level: usize,
    ) -> bool {
        scope
            .placements
            .borrow_mut()
            .entry(step.predicates[level])
            .or_insert_with(|| {
                self.evaluation_independent(&step.predicates[..level])
                    .then(HashMap::new)
            })
            .is_some()
    }

    /// The position of `node` in S`level` of `step` among the children of
    /// `parent`, and the size of that sequence, through `scope`: built on the
    /// first candidate of `parent` in this evaluation, looked up for the
    /// others. As for the run-wide cache, no borrow is held while predicates
    /// are evaluated, and an error is not remembered.
    fn scoped_placement(
        &self,
        scope: &EvaluationScope<'_, 't>,
        step: &PatternStep,
        level: usize,
        parent: &Nav<'t>,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<Option<(usize, usize)>, XsltError> {
        let predicate = step.predicates[level];
        let slot = (parent.document().serial(), parent.current_ref());
        if let Some(Some(by_parent)) = scope.placements.borrow().get(&predicate) {
            if let Some(placement) = by_parent.get(&slot) {
                return Ok(placement.locate(node));
            }
        }
        let members = self.apply_predicates(
            &step.predicates[..level],
            self.node_test_siblings(step, parent, node),
            current,
            scope.frame,
        )?;
        let placement = Placement::new(&members);
        let found = placement.locate(node);
        if let Some(Some(by_parent)) = scope.placements.borrow_mut().get_mut(&predicate) {
            by_parent.insert(slot, placement);
        }
        Ok(found)
    }

    /// Filters `members` by `predicates`, left to right, each over the whole
    /// sequence the previous one left (§3.2.2).
    fn apply_predicates(
        &self,
        predicates: &[ExprId],
        mut members: Vec<Nav<'t>>,
        current: &Nav<'t>,
        frame: &Frame<'t>,
    ) -> Result<Vec<Nav<'t>>, XsltError> {
        for &predicate in predicates {
            let size = members.len();
            let mut kept = Vec::with_capacity(size);
            for (index, member) in members.iter().enumerate() {
                let value =
                    self.eval_predicate(predicate, member, index + 1, size, current, frame)?;
                if predicate_holds(&value, index + 1).map_err(|e| self.xpath_error(e))? {
                    kept.push(member.clone());
                }
            }
            members = kept;
        }
        Ok(members)
    }

    /// Looks S`level` of `step` up for the parent `slot`.
    fn cached_placement(&self, step: &PatternStep, level: usize, slot: ParentSlot) -> Lookup {
        let predicate = step.predicates[level];
        let mut cache = self.placements.borrow_mut();
        let PlacementCache { steps, clock, .. } = &mut *cache;
        let entry = steps.entry(predicate).or_insert_with(|| StepPlacements {
            axis: step.axis,
            test: step.test.clone(),
            before: step.predicates[..level].to_vec(),
            by_parent: self
                .candidate_independent(&step.predicates[..level])
                .then(HashMap::new),
        });
        // A compiled stylesheet hands every predicate to exactly one step, so
        // this holds; it is checked because `matches` takes any `Pattern`.
        if entry.axis != step.axis
            || entry.test != step.test
            || entry.before[..] != step.predicates[..level]
        {
            return Lookup::Uncacheable;
        }
        match &mut entry.by_parent {
            None => Lookup::Uncacheable,
            Some(by_parent) => match by_parent.get_mut(&slot) {
                Some(remembered) => {
                    *clock += 1;
                    remembered.used = *clock;
                    Lookup::Hit(Rc::clone(&remembered.placement))
                }
                None => Lookup::Miss,
            },
        }
    }

    /// Remembers S`level` of the step `predicate` belongs to for the parent
    /// `slot`; at [`PLACEMENT_CACHE_LIMIT`] sequences, first forgets the less
    /// recently used half of them.
    fn remember_placement(&self, predicate: ExprId, slot: ParentSlot, placement: Rc<Placement>) {
        let mut cache = self.placements.borrow_mut();
        let PlacementCache {
            steps,
            entries,
            clock,
        } = &mut *cache;
        if *entries >= PLACEMENT_CACHE_LIMIT {
            let mut used: Vec<u64> = steps
                .values()
                .filter_map(|step| step.by_parent.as_ref())
                .flat_map(|by_parent| by_parent.values().map(|remembered| remembered.used))
                .collect();
            if !used.is_empty() {
                // The stamps are distinct, so this keeps exactly half.
                let middle = used.len() / 2;
                let (_, &mut cutoff, _) = used.select_nth_unstable(middle);
                *entries = 0;
                for by_parent in steps
                    .values_mut()
                    .filter_map(|step| step.by_parent.as_mut())
                {
                    by_parent.retain(|_, remembered| remembered.used >= cutoff);
                    *entries += by_parent.len();
                }
            }
        }
        if let Some(by_parent) = steps
            .get_mut(&predicate)
            .and_then(|step| step.by_parent.as_mut())
        {
            *clock += 1;
            let remembered = Remembered {
                placement,
                used: *clock,
            };
            if by_parent.insert(slot, remembered).is_none() {
                *entries += 1;
            }
        }
    }

    /// Whether the sequence the predicates `before` leave among a parent's
    /// children is the same whichever of those children is being matched.
    ///
    /// Everything a pattern predicate sees is fixed by the matcher except two
    /// inputs: the member's own focus, which is the same in every computation
    /// of the sequence, and the node the whole pattern is matched against,
    /// which `current()` returns (§16.6.1) and which differs from match to
    /// match. Local variables are a third: a `group-starting-with` /
    /// `group-ending-with` pattern sees the frame of its instruction
    /// ([`matches_in`](Self::matches_in)), whose values differ from one
    /// evaluation of the instruction to the next.
    /// So the sequence is candidate-independent when every predicate
    ///
    /// - reads only global variables and parameters, whose values are fixed
    ///   for the run (§9.5); and
    /// - calls only functions in the standard function namespace other than
    ///   `current()`. The others there — the F&O functions and §16's — depend
    ///   on their arguments, the focus and state that is stable for the run.
    ///   A stylesheet function (never in that namespace, §10.3 XTSE0740) is
    ///   refused, conservatively: its body cannot see the focus or `current()`
    ///   (§10.3 "Within the body of a stylesheet function, the focus is
    ///   initially undefined"), but each call may construct new nodes.
    fn candidate_independent(&self, before: &[ExprId]) -> bool {
        before
            .iter()
            .all(|&id| self.reads_only_globals(id) && self.standard_functions_only(id))
    }

    /// Whether the sequence the predicates `before` leave among a parent's
    /// children is the same for every candidate matched within **one
    /// evaluation** of an instruction ([`EvaluationScope`]):
    /// `candidate_independent` without its condition on variables.
    ///
    /// - A local variable is fixed for the evaluation: the scope holds a
    ///   shared borrow of the instruction's frame for as long as it exists,
    ///   so no slot of it can be assigned in the meantime, and the scope ends
    ///   with the evaluation. That is all the scope adds.
    /// - `current()` is still refused. In a pattern "its value is the node
    ///   that is being matched against the pattern" (§16.6.1), and one
    ///   evaluation matches many nodes, each as its own `current()`
    ///   (`group_by_pattern` hands every population node in as both): one
    ///   evaluation fixes the frame, not the candidate. `r[. is current()][1]`
    ///   matches every `r`.
    /// - A stylesheet function is still refused, for the run-wide cache's
    ///   reason. One evaluation adds nothing its body could read: it sees no
    ///   focus and no `current()` (§10.3: "Within the body of a stylesheet
    ///   function, the focus is initially undefined"), not the caller's
    ///   locals ("It is not possible within the body of the stylesheet
    ///   function to access the values of local variables that were in scope
    ///   in the place where the function call was written"), and no current
    ///   group (§14.3: "During the evaluation of a stylesheet function, the
    ///   current group and current grouping key are set to the empty
    ///   sequence"). But every call may construct new nodes (§11), whose
    ///   identity a predicate can observe (`is`, `generate-id()`, the order of
    ///   nodes in different trees), so two filterings of the same siblings
    ///   need not agree, and nothing in the specification lets a processor
    ///   share one call's nodes with another.
    fn evaluation_independent(&self, before: &[ExprId]) -> bool {
        before.iter().all(|&id| self.standard_functions_only(id))
    }

    /// Whether the expression `id` calls only functions in the standard
    /// function namespace other than `current()` (see `candidate_independent`).
    fn standard_functions_only(&self, id: ExprId) -> bool {
        self.sheet
            .expr(id)
            .expr
            .function_calls()
            .iter()
            .all(|call| call.namespace == FN_NS && call.local_name != "current")
    }

    /// S0 of `node`, whose parent is `parent`: the nodes `step`'s axis and
    /// node test select from `parent`, in document order.
    fn node_test_siblings(
        &self,
        step: &PatternStep,
        parent: &Nav<'t>,
        node: &Nav<'t>,
    ) -> Vec<Nav<'t>> {
        let mut out = Vec::new();
        match step.axis {
            PatternAxis::Attribute => {
                let mut attr = parent.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        if node_test_matches(
                            &step.test,
                            step.axis,
                            &attr,
                            &self.sheet.match_context,
                        ) {
                            out.push(attr.clone());
                        }
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
            }
            // Only a parentless document node is ever matched along `self`,
            // and it returned above.
            PatternAxis::SelfAxis => {}
            PatternAxis::Child => {
                let mut child = parent.clone();
                if child.move_to_first_child() {
                    loop {
                        if node_test_matches(
                            &step.test,
                            step.axis,
                            &child,
                            &self.sheet.match_context,
                        ) {
                            out.push(child.clone());
                        }
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(node.clone());
        }
        out
    }
}

/// Whether a node satisfies a pattern step's axis and node test (§5.5.3).
///
/// The axes a pattern uses are the adjusted ones §5.5.3 defines:
///
/// > If PS uses the child axis … the axis in step PS is replaced by
/// > `child-or-top` … If the context node is a parentless element, comment,
/// > processing-instruction, or text node then the `child-or-top` axis selects
/// > the context node; otherwise it selects the children of the context node.
/// > It is a forwards axis whose principal node kind is element.
///
/// Two consequences are easy to get wrong, and the spec calls both out:
/// `node()` does **not** match a document node ("for backwards compatibility
/// reasons the `child-or-top` axis does not match a document node"), and a
/// step whose node test is `document-node()` uses the `self` axis instead, so
/// that it does — but only when "no explicit axis is specified" and the step
/// is the first of a RelativePathPattern ([`PatternAxis::SelfAxis`]). On any
/// other axis a document node is never selected: it is never a child and
/// never an attribute.
///
/// A typed test — `element(*, T)`, `element(N, T?)`, `attribute(N, T)`,
/// `schema-element(N)`, `schema-attribute(N)` — is decided by `xsd-schema`'s
/// matcher of XPath 2.0 §2.5.4.3 – §2.5.4.6 (`ItemType::matches_item`), the
/// one `instance of` and a path step run too, in `cx` (the stylesheet's name
/// table and schema set): derives-from over the node's type annotation — none
/// is `xs:untyped` / `xs:untypedAtomic` —, the nilled property, a
/// substitution group. This crate decides only the axis and the node kind.
fn node_test_matches(
    test: &NodeTest,
    axis: PatternAxis,
    nav: &Nav<'_>,
    cx: &XPathContext<'_>,
) -> bool {
    let kind = nav.node_type();
    // `document-node()` is the one test that looks at the node itself.
    if let NodeTest::DocumentNode(inner) = test {
        if kind != DomNodeType::Root || axis != PatternAxis::SelfAxis {
            return false;
        }
        return match inner {
            None => true,
            // XPath 2.0 §2.5.4.2: "document-node(E) matches any document node that contains
            // exactly one element node, optionally accompanied by one or more comment and
            // processing instruction nodes, if E is an ElementTest or SchemaElementTest that
            // matches the element node" — so a second element child, or any text child, and the
            // test fails (`import-schema-055`).
            Some(inner) => {
                let mut child = nav.clone();
                if !child.move_to_first_child() {
                    return false;
                }
                let mut element = None;
                loop {
                    match child.node_type() {
                        DomNodeType::Element if element.is_none() => element = Some(child.clone()),
                        DomNodeType::Comment | DomNodeType::ProcessingInstruction => {}
                        _ => return false,
                    }
                    if !child.move_to_next_sibling() {
                        break;
                    }
                }
                element.is_some_and(|element| {
                    node_test_matches(inner, PatternAxis::Child, &element, cx)
                })
            }
        };
    }
    // Everything else selects along `child-or-top` or `attribute-or-top`.
    let on_axis = match axis {
        PatternAxis::Attribute => kind == DomNodeType::Attribute,
        // Only ever paired with a `document-node()` test, handled above.
        PatternAxis::SelfAxis => false,
        PatternAxis::Child => matches!(
            kind,
            DomNodeType::Element
                | DomNodeType::Text
                | DomNodeType::Whitespace
                | DomNodeType::SignificantWhitespace
                | DomNodeType::Comment
                | DomNodeType::ProcessingInstruction
        ),
    };
    if !on_axis {
        return false;
    }
    let principal = principal_kind(axis);
    match test {
        NodeTest::Name(q) => {
            kind == principal && nav.local_name() == q.local && nav.namespace_uri() == q.uri
        }
        NodeTest::NsWildcard(uri) => kind == principal && nav.namespace_uri() == uri,
        NodeTest::LocalWildcard(local) => kind == principal && nav.local_name() == local,
        NodeTest::AnyName => kind == principal,
        NodeTest::AnyNode => true,
        NodeTest::Text => kind.is_text_like(),
        NodeTest::Comment => kind == DomNodeType::Comment,
        NodeTest::Pi(None) => kind == DomNodeType::ProcessingInstruction,
        NodeTest::Pi(Some(target)) => {
            kind == DomNodeType::ProcessingInstruction && nav.local_name() == target
        }
        NodeTest::Element(name) => kind == DomNodeType::Element && name_matches(name, nav),
        NodeTest::Attribute(name) => kind == DomNodeType::Attribute && name_matches(name, nav),
        // The typed forms (XPath 2.0 §2.5.4.3 – §2.5.4.6): `xsd-schema` decides.
        NodeTest::TypedElement { test, .. } | NodeTest::SchemaElement { test, .. } => {
            kind == DomNodeType::Element && test.matches_item(&XmlItem::Node(nav.clone()), cx)
        }
        NodeTest::TypedAttribute { test, .. } | NodeTest::SchemaAttribute { test, .. } => {
            kind == DomNodeType::Attribute && test.matches_item(&XmlItem::Node(nav.clone()), cx)
        }
        NodeTest::DocumentNode(_) => unreachable!("handled above"),
    }
}

/// The name half of an `ElementTest` / `AttributeTest`: absent, or `*`, means
/// every name.
fn name_matches(name: &Option<QName>, nav: &Nav<'_>) -> bool {
    name.as_ref()
        .is_none_or(|q| nav.local_name() == q.local && nav.namespace_uri() == q.uri)
}

/// The principal node kind of an axis: elements for `child`, attributes for
/// `attribute`.
fn principal_kind(axis: PatternAxis) -> DomNodeType {
    match axis {
        PatternAxis::Child | PatternAxis::SelfAxis => DomNodeType::Element,
        PatternAxis::Attribute => DomNodeType::Attribute,
    }
}

/// The predicate truth value of `value` at context position `position`.
///
/// XPath 2.0 §3.2.2: "If the value of the predicate expression is a singleton
/// atomic value of a numeric type or derived from a numeric type, the
/// predicate truth value is true if the value of the predicate expression is
/// equal (by the eq operator) to the context position, and is false otherwise.
/// … Otherwise, the predicate truth value is the effective boolean value of
/// the predicate expression."
fn predicate_holds(value: &XPathValue<Nav<'_>>, position: usize) -> Result<bool, XPathError> {
    match numeric_predicate(value) {
        Some(number) => Ok(number == position as f64),
        None => effective_boolean_value(value),
    }
}

/// The number a numeric predicate compares with the context position, if
/// `value` is one — a singleton atomic value of a numeric type (§3.2.2) — and
/// `None` if its effective boolean value decides instead.
///
/// A sequence of several numbers is not a numeric predicate: `item[(1, 2)]`
/// takes the effective boolean value, which is FORG0006 for a sequence of
/// more than one atomic value.
fn numeric_predicate(value: &XPathValue<Nav<'_>>) -> Option<f64> {
    let single = match value {
        XPathValue::Item(item) => Some(item),
        XPathValue::Sequence(items) if items.len() == 1 => Some(&items[0]),
        _ => None,
    };
    match single {
        Some(XmlItem::Atomic(atomic)) if atomic.type_code.is_numeric() => atomic.as_double(),
        _ => None,
    }
}

/// The position of `node` in `members` and the size of `members`.
fn locate(members: &[Nav<'_>], node: &Nav<'_>) -> Option<(usize, usize)> {
    members
        .iter()
        .position(|member| member.is_same_position(node))
        .map(|index| (index + 1, members.len()))
}

// ── The placement cache ──────────────────────────────────────────────────

/// A parent node: the serial of its document and its index there.
type ParentSlot = (u64, u32);

/// How many (predicate, parent) sequences [`PlacementCache`] holds before it
/// forgets the less recently used half. Siblings are matched close together —
/// template rules for one `xsl:apply-templates` selection, a key index built
/// in document order — so a small cache serves them; the limit only stops a
/// long run from keeping every parent it ever visited. Forgetting by recency
/// rather than all at once keeps the parents still being worked through: in a
/// table of a million rows matched against `row[1]` and `cell[1]`, the table's
/// sequence is used for every row and must not be rebuilt every 4,096 rows.
const PLACEMENT_CACHE_LIMIT: usize = 4096;

/// The sequences positional predicates were applied to, remembered for the
/// run (`Transformation::placement`).
///
/// An entry is S*i* — the input sequence of predicate *i* of one step — among
/// the children (or attributes) of one parent, as the position of each member
/// and the size. It is only made when S*i* is the same for every candidate
/// (`candidate_independent`), and then it stays right for the whole run: a
/// tree never changes once built, documents are told apart by their
/// process-wide serial, global variables keep their first value, and nothing
/// else enters the computation. That last point is load-bearing: if globals are
/// ever re-evaluated within one `Transformation` (e.g. reset per run), this cache
/// and `id_key_selections` must be cleared with them. Errors are not remembered: a candidate whose
/// sequence cannot be built gets the error again (its cost is then a pass per
/// candidate).
#[derive(Default)]
pub(super) struct PlacementCache {
    /// By the predicate whose input sequence the entries are.
    steps: HashMap<ExprId, StepPlacements>,
    /// How many sequences are held across all steps.
    entries: usize,
    /// Ticks on every use; stamps [`Remembered::used`].
    clock: u64,
}

/// The remembered sequences of one predicate.
struct StepPlacements {
    /// The step the predicate belongs to, checked on every lookup.
    axis: PatternAxis,
    test: NodeTest,
    /// The predicates before this one in the step.
    before: Vec<ExprId>,
    /// By parent; `None` if the sequence depends on the candidate and is never
    /// remembered.
    by_parent: Option<HashMap<ParentSlot, Remembered>>,
}

/// One remembered sequence and when it was last used.
struct Remembered {
    placement: Rc<Placement>,
    used: u64,
}

/// One remembered sequence.
struct Placement {
    /// 1-based position by node index (`current_ref`) — unique within the
    /// document for every node a step selects: the members are children or
    /// attributes, never namespace nodes.
    positions: HashMap<u32, u32>,
    size: usize,
}

impl Placement {
    fn new(members: &[Nav<'_>]) -> Self {
        Placement {
            positions: members
                .iter()
                .enumerate()
                .map(|(index, member)| (member.current_ref(), index as u32 + 1))
                .collect(),
            size: members.len(),
        }
    }

    fn locate(&self, node: &Nav<'_>) -> Option<(usize, usize)> {
        self.positions
            .get(&node.current_ref())
            .map(|&position| (position as usize, self.size))
    }
}

/// What a pattern's predicates are evaluated in besides the candidate's focus
/// and `current()`: the variable frame and, for a pattern of an instruction,
/// the memory of the evaluation it is matched in.
#[derive(Clone, Copy)]
pub(super) struct MatchEnv<'e, 't> {
    frame: &'e Frame<'t>,
    scope: Option<&'e EvaluationScope<'e, 't>>,
}

impl<'e, 't> MatchEnv<'e, 't> {
    /// A `match` pattern of `xsl:template` or `xsl:key`: the global variables
    /// only — `empty` is an empty frame — and only the run-wide caches.
    pub(super) fn top_level(empty: &'e Frame<'t>) -> Self {
        MatchEnv {
            frame: empty,
            scope: None,
        }
    }
}

/// What the pattern matcher remembers for **one evaluation** of an instruction
/// whose patterns see the instruction's local variables:
/// `group-starting-with` / `group-ending-with` of `xsl:for-each-group`, and
/// the `count` / `from` patterns of `xsl:number`, which match through
/// [`Transformation::matches_in`] with a scope of their own per evaluation.
///
/// The run-wide caches ([`PlacementCache`], `id_key_selections`) refuse
/// whatever reads a local variable, because a local differs between
/// evaluations of the instruction: a variable inside `xsl:for-each` is bound
/// again for every item, in the same frame slot, and a template's parameters
/// for every call. Within one evaluation it cannot differ, and the scope makes
/// sure of that by construction rather than by convention:
///
/// - it holds a shared borrow of the frame for its whole life, so the frame
///   cannot be changed while it exists, and every predicate matched through it
///   is evaluated in exactly that frame ([`env`](Self::env));
/// - it is a local of the evaluating function (`group_by_pattern`), dropped
///   when the evaluation ends, never stored in the `Transformation`; the next
///   evaluation of the same instruction starts with an empty one;
/// - it reaches the matcher only as an argument: an evaluation of the same
///   instruction reached from inside a predicate (through a stylesheet
///   function, say) runs in a frame of its own with a scope of its own.
///
/// What it holds:
///
/// - S*i* of a positional predicate whose earlier predicates are
///   `evaluation_independent` but not `candidate_independent`, per (predicate,
///   parent) — what the run-wide [`PlacementCache`] holds for the others;
/// - the selection of an `id(…)` / `key(…)` anchor whose argument is a local
///   variable, per (call, document) — what `id_key_selections` holds when it
///   is a global or a literal.
///
/// It has no size limit: it lives for one evaluation, and holds at most one
/// sequence per (predicate, parent) and one selection per (call, document)
/// that evaluation's matches reach, so it is bounded by the trees matched. The
/// run-wide cache is bounded only because it lives for the whole run.
pub(super) struct EvaluationScope<'f, 't> {
    /// The frame of the instruction being evaluated.
    frame: &'f Frame<'t>,
    /// By predicate: the sequences by parent, or `None` if the step's sequence
    /// depends on the candidate even within one evaluation.
    placements: RefCell<HashMap<ExprId, Option<HashMap<ParentSlot, Placement>>>>,
    /// What each `id()` / `key()` anchor reading a local selected, per
    /// document.
    selections: RefCell<IdKeySelections<'t>>,
}

impl<'f, 't> EvaluationScope<'f, 't> {
    /// An empty scope for one evaluation of an instruction running in `frame`.
    pub(super) fn new(frame: &'f Frame<'t>) -> Self {
        EvaluationScope {
            frame,
            placements: RefCell::new(HashMap::new()),
            selections: RefCell::new(HashMap::new()),
        }
    }

    /// The environment to match in: this scope, and its frame.
    fn env(&self) -> MatchEnv<'_, 't> {
        MatchEnv {
            frame: self.frame,
            scope: Some(self),
        }
    }
}

/// What [`Transformation::cached_placement`] found.
enum Lookup {
    Hit(Rc<Placement>),
    /// Not there yet, but may be remembered.
    Miss,
    /// Never remembered for this step.
    Uncacheable,
}

#[cfg(test)]
mod tests {
    use bumpalo::Bump;
    use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
    use xsd_schema::namespace::{NameTable, QualifiedName};
    use xsd_schema::pipeline::load_and_process_schema;
    use xsd_schema::types::{ItemType, NameTest, TypeTest};
    use xsd_schema::{SchemaSet, XmlTypeCode};

    use super::*;
    use crate::compile::Compiler;
    use crate::functions::XsltFunctions;
    use crate::runtime::TransformOptions;

    // ── §5.5.4: what may and may not be recovered from ───────────────────

    /// `guard_recoverable` seen directly, including the one case no stylesheet
    /// can reach through the public API: an
    /// `XSLT2-INTERNAL` error raised while matching.
    ///
    /// A broken invariant is not "a dynamic error or type error that occurs
    /// during the evaluation of a pattern"; recovering from it would answer
    /// "this pattern does not match" on no evidence at all.
    #[test]
    fn guard_recoverable_lets_a_processor_failure_through_in_both_modes() {
        let names = xsd_schema::namespace::NameTable::new();
        let functions = XsltFunctions::new();
        let sheet = Compiler::new(&names, &functions)
            .compile_str(
                "<xsl:stylesheet version=\"2.0\" \
                 xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\
                 <xsl:template match=\"/\"/></xsl:stylesheet>",
                "file:///guard.xsl",
            )
            .expect("the stylesheet compiles");
        let arena = Bump::new();
        for strict in [false, true] {
            let options = TransformOptions {
                strict_recoverable: strict,
                ..TransformOptions::default()
            };
            let run = sheet.transformation(&arena, options);

            assert_eq!(run.guard_recoverable(Ok(true)), Ok(true));
            assert_eq!(run.guard_recoverable(Ok(false)), Ok(false));

            for e in [
                XsltError::internal("a broken invariant inside a pattern"),
                XsltError::not_implemented("a construct", "some-feature"),
            ] {
                assert_eq!(
                    run.guard_recoverable(Err(e.clone())),
                    Err(e.clone()),
                    "strict={strict}: {} must propagate",
                    e.code
                );
            }

            // A genuine dynamic error keeps §5.5.4's behaviour.
            let dynamic = XsltError::new("FORG0001", "invalid value for cast");
            assert_eq!(
                run.guard_recoverable(Err(dynamic.clone())),
                if strict { Err(dynamic) } else { Ok(false) },
                "strict={strict}"
            );
        }
    }

    /// An error the processor raises inside an XPath function call travels as
    /// the engine error's host payload — there is no side channel that could
    /// outlive an error §5.5.4 recovers from — and one that is not recovered
    /// from comes back with its code and origin, so a processor failure is
    /// still refused by the recovery guard.
    #[test]
    fn an_error_raised_inside_a_function_call_comes_back_unchanged() {
        let names = xsd_schema::namespace::NameTable::new();
        let functions = XsltFunctions::new();
        let sheet = Compiler::new(&names, &functions)
            .compile_str(
                "<xsl:stylesheet version=\"2.0\" \
                 xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\
                 <xsl:template match=\"/\"/></xsl:stylesheet>",
                "file:///payload.xsl",
            )
            .expect("the stylesheet compiles");
        let arena = Bump::new();
        let run = sheet.transformation(&arena, TransformOptions::default());
        let failure = XsltError::not_implemented("a construct", "some-feature");
        let back = run.xpath_error(failure.clone().into_xpath_error());
        assert_eq!(back, failure);
        assert!(back.is_processor_failure());
        assert_eq!(run.guard_recoverable(Err(back.clone())), Err(back));
        // A dynamic error raised the same way is recovered from as before.
        let dynamic = XsltError::new("FORG0001", "x");
        let back = run.xpath_error(dynamic.clone().into_xpath_error());
        assert_eq!(back, dynamic);
        assert_eq!(run.guard_recoverable(Err(back)), Ok(false));
    }

    // ── §4.3 / XPath 2.0 §2.5.4.3: the type tests read the annotation ────

    const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="item" type="xs:integer"/>
      <xs:element name="r">
        <xs:complexType>
          <xs:sequence><xs:element ref="item"/></xs:sequence>
          <xs:attribute name="n" type="xs:integer"/>
        </xs:complexType>
      </xs:element>
    </xs:schema>"#;

    fn typed<'a>(arena: &'a Bump, schema_set: &'a SchemaSet) -> Nav<'a> {
        let doc = build_typed_document(
            r#"<r n="7"><item>42</item></r>"#.as_bytes(),
            arena,
            schema_set,
            BufferDocumentOptions::default(),
        )
        .expect("a schema-validated document");
        arena.alloc(doc).create_navigator()
    }

    fn schema() -> SchemaSet {
        let mut schema_set = SchemaSet::xsd11();
        load_and_process_schema(SCHEMA.as_bytes(), "file:///s.xsd", &mut schema_set, None)
            .expect("the test schema compiles");
        schema_set
    }

    /// `element(local, xs:untyped)` (`untyped_only`) or `element(local,
    /// xs:anyType)`, resolved as the compiler resolves them (names interned in
    /// `names`).
    fn element_test(names: &NameTable, local: &str, untyped_only: bool) -> NodeTest {
        let type_test = if untyped_only {
            TypeTest::Untyped
        } else {
            TypeTest::BuiltIn(XmlTypeCode::AnyType)
        };
        NodeTest::TypedElement {
            name: Some(QName::new("", local)),
            test: ItemType::element(
                Some(NameTest::QName(QualifiedName::new(
                    None,
                    names.add(local),
                    None,
                ))),
                Some(type_test),
                false,
            ),
        }
    }

    /// `attribute(local, xs:untypedAtomic)` (`untyped_only`) or
    /// `attribute(local, xs:anySimpleType)`.
    fn attribute_test(names: &NameTable, local: &str, untyped_only: bool) -> NodeTest {
        let code = if untyped_only {
            XmlTypeCode::UntypedAtomic
        } else {
            XmlTypeCode::AnySimpleType
        };
        NodeTest::TypedAttribute {
            name: Some(QName::new("", local)),
            test: ItemType::attribute(
                Some(NameTest::QName(QualifiedName::new(
                    None,
                    names.add(local),
                    None,
                ))),
                Some(TypeTest::BuiltIn(code)),
            ),
        }
    }

    /// `element(item, xs:untyped)` matches only an element whose type
    /// annotation *is* `xs:untyped` — i.e. one that was never validated.
    /// `element(item, xs:anyType)` matches whatever the annotation is, because
    /// every element type derives from `xs:anyType`.
    #[test]
    fn a_typed_element_fails_the_untyped_test_and_passes_the_anytype_test() {
        let schema_set = schema();
        let arena = Bump::new();
        let mut nav = typed(&arena, &schema_set);
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_child());
        assert_eq!(nav.local_name(), "item");
        assert!(
            nav.type_annotation().is_some(),
            "the fixture must really be typed"
        );

        let names = &schema_set.name_table;
        let cx = XPathContext::new(names).with_schema_set(&schema_set);
        assert!(
            !node_test_matches(
                &element_test(names, "item", true),
                PatternAxis::Child,
                &nav,
                &cx
            ),
            "element(item, xs:untyped) must not match a schema-typed element"
        );
        assert!(
            node_test_matches(
                &element_test(names, "item", false),
                PatternAxis::Child,
                &nav,
                &cx
            ),
            "element(item, xs:anyType) matches every element"
        );
        // The one-argument form has never looked at the type.
        assert!(node_test_matches(
            &NodeTest::Element(Some(QName::new("", "item"))),
            PatternAxis::Child,
            &nav,
            &cx
        ));
    }

    /// The attribute twins: `xs:untypedAtomic` requires an unvalidated
    /// attribute, `xs:anySimpleType` / `xs:anyAtomicType` accept any.
    #[test]
    fn a_typed_attribute_fails_the_untyped_atomic_test() {
        let schema_set = schema();
        let arena = Bump::new();
        let mut nav = typed(&arena, &schema_set);
        assert!(nav.move_to_first_child());
        assert!(nav.move_to_first_attribute());
        assert_eq!(nav.local_name(), "n");
        assert!(
            nav.type_annotation().is_some(),
            "the fixture must really be typed"
        );

        let names = &schema_set.name_table;
        let cx = XPathContext::new(names).with_schema_set(&schema_set);
        assert!(
            !node_test_matches(
                &attribute_test(names, "n", true),
                PatternAxis::Attribute,
                &nav,
                &cx
            ),
            "attribute(n, xs:untypedAtomic) must not match a schema-typed attribute"
        );
        assert!(
            node_test_matches(
                &attribute_test(names, "n", false),
                PatternAxis::Attribute,
                &nav,
                &cx
            ),
            "attribute(n, xs:anySimpleType) matches every attribute"
        );
    }

    /// The control: on a tree this processor parsed, §4.3 makes both spellings
    /// match, exactly as they did before the distinction existed.
    #[test]
    fn an_untyped_element_and_attribute_pass_both_spellings() {
        let names = xsd_schema::namespace::NameTable::new();
        let arena = Bump::new();
        let doc = xsd_schema::document::BufferDocument::from_reader_default(
            r#"<r n="7"><item>42</item></r>"#.as_bytes(),
            &arena,
            &names,
        )
        .expect("a plain parse");
        let root: Nav<'_> = arena.alloc(doc).create_navigator();
        // A basic processor's context: no schema set.
        let cx = XPathContext::new(&names);

        let mut element = root.clone();
        assert!(element.move_to_first_child());
        assert!(element.move_to_first_child());
        assert_eq!(element.local_name(), "item");
        assert_eq!(element.type_annotation(), None);
        for untyped_only in [true, false] {
            assert!(
                node_test_matches(
                    &element_test(&names, "item", untyped_only),
                    PatternAxis::Child,
                    &element,
                    &cx
                ),
                "untyped_only={untyped_only}"
            );
        }

        let mut attribute = root.clone();
        assert!(attribute.move_to_first_child());
        assert!(attribute.move_to_first_attribute());
        assert_eq!(attribute.type_annotation(), None);
        for untyped_only in [true, false] {
            assert!(
                node_test_matches(
                    &attribute_test(&names, "n", untyped_only),
                    PatternAxis::Attribute,
                    &attribute,
                    &cx
                ),
                "untyped_only={untyped_only}"
            );
        }
    }

    // ── The stack guard on a direct match ───────────────────────────────

    /// Moved from the integration tests when `Transformation::matches`
    /// became crate-internal.
    mod stack_guard {
        use bumpalo::Bump;
        use xsd_schema::namespace::NameTable;
        use xsd_schema::navigator::DomNavigator;

        use crate::{Compiler, TransformOptions, XsltError, XsltFunctions};

        const NS: &str = "http://www.w3.org/1999/XSL/Transform";

        /// Wraps top-level content in a `version="2.0"` stylesheet with the
        /// `xs` and `f` prefixes bound.
        fn sheet(body: &str) -> String {
            format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{body}</xsl:stylesheet>"#
            )
        }

        /// A direct call of `Transformation::matches`
        /// evaluates a pattern's predicates — and the stylesheet functions they call —
        /// outside a run, so the stack guard must be on there too. On an ordinary
        /// 2 MiB thread, `r[f:g(1000)]` used to recurse past the end of the stack and
        /// abort the process; it now ends in the depth error, as the same stylesheet
        /// does through `transform_document`.
        #[test]
        fn a_direct_pattern_match_is_under_the_stack_guard() {
            let main = sheet(
                r#"<xsl:function name="f:g" as="xs:boolean">
                     <xsl:param name="n" as="xs:integer"/>
                     <xsl:sequence select="if ($n = 0) then true() else f:g($n - 1)"/>
                   </xsl:function>
                   <xsl:template match="r[f:g(1000)]">deep</xsl:template>"#,
            );
            let result = std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn(move || -> Result<bool, XsltError> {
                    let functions = XsltFunctions::new();
                    let names = NameTable::new();
                    let compiled = Compiler::new(&names, &functions).compile_str(&main, "urn:t")?;
                    let pattern = compiled
                        .templates()
                        .iter()
                        .find_map(|rule| rule.pattern)
                        .map(|id| compiled.pattern(id))
                        .expect("the r[f:g(1000)] rule");
                    let arena = Bump::new();
                    let run = compiled.transformation(&arena, TransformOptions::default());
                    let doc = run.load_document("urn:in", "<r/>")?;
                    let mut node = run
                        .store()
                        .borrow()
                        .get(doc)
                        .expect("loaded")
                        .doc
                        .create_navigator();
                    assert!(node.move_to_first_child(), "the r element");
                    // A second call, after the first has left its scope, is guarded
                    // just the same.
                    let first = run.matches(pattern, &node);
                    assert!(first.is_err(), "{first:?}");
                    run.matches(pattern, &node)
                })
                .unwrap()
                .join()
                .unwrap();
            let error = result.unwrap_err();
            assert_eq!(error.code, "XSLT2-INTERNAL", "{error}");
            assert!(error.message.contains("recursing without end"), "{error}");
        }
    }
}
