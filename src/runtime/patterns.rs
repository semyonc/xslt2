//! Matching a node against a pattern (§5.5.3), walking the pattern right to
//! left from the candidate node.

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{XPathError, XmlItem};

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::error::XsltError;
use crate::pattern::{Anchor, Connector, NodeTest, PathPattern, Pattern, PatternAxis, PatternStep};
use crate::receiver::Nav;

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
    pub fn matches(&self, pattern: &Pattern, node: &Nav<'t>) -> Result<bool, XsltError> {
        for alternative in &pattern.alternatives {
            // The node handed in is both the candidate and, per §16.6.1, the
            // node `current()` reports throughout the match.
            match self.guard_recoverable(self.matches_alternative(alternative, node, node)) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    /// §5.5.4: an error raised while matching is recoverable, and the recovery
    /// action is to treat the pattern as not matching.
    pub(super) fn guard_recoverable(
        &self,
        result: Result<bool, XsltError>,
    ) -> Result<bool, XsltError> {
        match result {
            Ok(matched) => Ok(matched),
            Err(e) if self.options.strict_recoverable => Err(e),
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
    ) -> Result<bool, XsltError> {
        if alternative.steps.is_empty() {
            // The pattern `/`.
            return Ok(node.node_type() == DomNodeType::Root);
        }
        self.matches_from(alternative, 0, node, current)
    }

    fn matches_from(
        &self,
        alternative: &PathPattern,
        index: usize,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        let step = &alternative.steps[index];
        if !self.step_matches(step, node, current)? {
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
            });
        }
        match step.connector {
            Connector::Parent => {
                let mut parent = node.clone();
                if !parent.move_to_parent() {
                    return Ok(false);
                }
                self.matches_from(alternative, index + 1, &parent, current)
            }
            Connector::Ancestor => {
                let mut ancestor = node.clone();
                while ancestor.move_to_parent() {
                    if self.matches_from(alternative, index + 1, &ancestor, current)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    fn step_matches(
        &self,
        step: &PatternStep,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        if !node_test_matches(&step.test, step.axis, node) {
            return Ok(false);
        }
        if step.predicates.is_empty() {
            return Ok(true);
        }
        // §5.5.3: the predicates apply successively to the sequence the node
        // test selects among the candidate's siblings, with the focus
        // recomputed after each one. A candidate with no parent is treated as
        // a one-item sequence (`child-or-top` selects the context node).
        let mut siblings = self.node_test_siblings(step, node);
        for predicate in &step.predicates {
            let size = siblings.len();
            let mut kept = Vec::with_capacity(size);
            for (index, candidate) in siblings.iter().enumerate() {
                let mut cx = XsltContext::empty();
                cx.item = Some(XmlItem::Node(candidate.clone()));
                cx.position = index + 1;
                cx.size = size;
                // Not the candidate: `current()` inside a pattern is the node
                // the pattern as a whole is being matched against (§16.6.1).
                cx.current = Some(XmlItem::Node(current.clone()));
                let value = self.eval_expr(*predicate, &Frame::default(), &cx)?;
                if predicate_holds(&value, index + 1).map_err(|e| self.xpath_error(e))? {
                    kept.push(candidate.clone());
                }
            }
            siblings = kept;
        }
        Ok(siblings.iter().any(|n| n.is_same_position(node)))
    }

    fn node_test_siblings(&self, step: &PatternStep, node: &Nav<'t>) -> Vec<Nav<'t>> {
        let mut parent = node.clone();
        if !parent.move_to_parent() {
            return vec![node.clone()];
        }
        let mut out = Vec::new();
        match step.axis {
            PatternAxis::Attribute => {
                let mut attr = parent.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        if node_test_matches(&step.test, step.axis, &attr) {
                            out.push(attr.clone());
                        }
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
            }
            PatternAxis::Child => {
                let mut child = parent.clone();
                if child.move_to_first_child() {
                    loop {
                        if node_test_matches(&step.test, step.axis, &child) {
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
/// that it does.
fn node_test_matches(test: &NodeTest, axis: PatternAxis, nav: &Nav<'_>) -> bool {
    let kind = nav.node_type();
    // `document-node()` is the one test that looks at the node itself.
    if let NodeTest::DocumentNode(inner) = test {
        if kind != DomNodeType::Root {
            return false;
        }
        return match inner {
            None => true,
            Some(inner) => {
                let mut child = nav.clone();
                if !child.move_to_first_child() {
                    return false;
                }
                loop {
                    if child.node_type() == DomNodeType::Element {
                        return node_test_matches(inner, PatternAxis::Child, &child);
                    }
                    if !child.move_to_next_sibling() {
                        return false;
                    }
                }
            }
        };
    }
    // Everything else selects along `child-or-top` or `attribute-or-top`.
    let on_axis = match axis {
        PatternAxis::Attribute => kind == DomNodeType::Attribute,
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
        NodeTest::Element(name) => {
            kind == DomNodeType::Element
                && name
                    .as_ref()
                    .is_none_or(|q| nav.local_name() == q.local && nav.namespace_uri() == q.uri)
        }
        NodeTest::Attribute(name) => {
            kind == DomNodeType::Attribute
                && name
                    .as_ref()
                    .is_none_or(|q| nav.local_name() == q.local && nav.namespace_uri() == q.uri)
        }
        NodeTest::DocumentNode(_) => unreachable!("handled above"),
    }
}

/// The principal node kind of an axis: elements for `child`, attributes for
/// `attribute`.
fn principal_kind(axis: PatternAxis) -> DomNodeType {
    match axis {
        PatternAxis::Child => DomNodeType::Element,
        PatternAxis::Attribute => DomNodeType::Attribute,
    }
}

/// A predicate whose value is a singleton number is positional; anything else
/// is taken by its effective boolean value.
fn predicate_holds(value: &XPathValue<Nav<'_>>, position: usize) -> Result<bool, XPathError> {
    let single = match value {
        XPathValue::Item(item) => Some(item),
        XPathValue::Sequence(items) if items.len() == 1 => Some(&items[0]),
        _ => None,
    };
    if let Some(XmlItem::Atomic(atomic)) = single {
        if atomic.type_code.is_numeric() {
            if let Some(number) = atomic.as_double() {
                return Ok(number == position as f64);
            }
        }
    }
    effective_boolean_value(value)
}
