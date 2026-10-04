//! Matching a node against a pattern (§5.5.3), walking the pattern right to
//! left from the candidate node.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::xpath::functions::{effective_boolean_value, XPathValue};
use xsd_schema::xpath::{XPathError, XmlItem};

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::error::XsltError;
use crate::model::{ExprId, VarLocation};
use crate::names::QName;
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
    pub fn matches(&self, pattern: &Pattern, node: &Nav<'t>) -> Result<bool, XsltError> {
        // A pattern's predicates may call stylesheet functions, so a direct
        // call measures the stack it uses from here, as a run does (Q33).
        let _stack = self.stack_scope(super::stack_position());
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
            Err(_) => {
                // The error has just been dropped; drop the side channel with
                // it. `XsltEvaluator::eval` parks the real error and hands the
                // engine a placeholder, and `xpath_error` normally collects it
                // on the way out — but if the engine were ever to swallow the
                // placeholder itself, the park would outlive this match and
                // surface from an unrelated evaluation later on.
                self.parked_error.borrow_mut().take();
                Ok(false)
            }
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
                return Ok(self.id_key_selection(anchor, node, current)?.contains(node));
            }
        } else if alternative.steps.is_empty() {
            // The pattern `/`.
            return Ok(node.node_type() == DomNodeType::Root);
        }
        self.matches_from(alternative, 0, node, current)
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
    /// variable is evaluated per candidate, as before; no pattern site
    /// compiles one today (template and key patterns see only globals), but a
    /// pattern of a later instruction might. An error is not kept: it is
    /// raised for this candidate (and a key whose index build failed
    /// remembers that itself, `runtime/keys.rs`).
    fn id_key_selection(
        &self,
        anchor: IdKeyAnchor,
        node: &Nav<'t>,
        current: &Nav<'t>,
    ) -> Result<Rc<SelectedNodes<'t>>, XsltError> {
        let fixed = self.reads_only_globals(anchor.call);
        let slot = (anchor.call, node.document().serial());
        if fixed {
            if let Some(selected) = self.id_key_selections.borrow().get(&slot) {
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
        let value = self.eval_expr(anchor.call, &Frame::default(), &cx)?;
        let selected = Rc::new(SelectedNodes::new(value.into_vec().into_iter().filter_map(
            |item| match item {
                XmlItem::Node(nav) => Some(nav),
                XmlItem::Atomic(_) => None,
            },
        )));
        if fixed {
            self.id_key_selections
                .borrow_mut()
                .insert(slot, Rc::clone(&selected));
        }
        Ok(selected)
    }

    /// Whether every variable the expression `id` reads is a global variable
    /// or parameter.
    fn reads_only_globals(&self, id: ExprId) -> bool {
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
                // `id(…)/…`: the parent of the leftmost step is selected by the
                // call; `id(…)//…`: some ancestor is.
                Anchor::IdKey(anchor) => {
                    let mut ancestor = node.clone();
                    if !ancestor.move_to_parent() {
                        return Ok(false);
                    }
                    let selected = self.id_key_selection(anchor, node, current)?;
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
            // Only a parentless document node is ever matched along `self`,
            // and it returned above.
            PatternAxis::SelfAxis => {}
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
/// that it does — but only when "no explicit axis is specified" and the step
/// is the first of a RelativePathPattern ([`PatternAxis::SelfAxis`]). On any
/// other axis a document node is never selected: it is never a child and
/// never an attribute.
fn node_test_matches(test: &NodeTest, axis: PatternAxis, nav: &Nav<'_>) -> bool {
    let kind = nav.node_type();
    // `document-node()` is the one test that looks at the node itself.
    if let NodeTest::DocumentNode(inner) = test {
        if kind != DomNodeType::Root || axis != PatternAxis::SelfAxis {
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
        // The two-argument forms. XPath 2.0 §2.5.4.3: an `ElementTest` with a
        // TypeName matches an element node whose "type annotation is the same
        // as, or derives from, the TypeName". `xs:anyType` and the two
        // attribute supertypes are satisfied by every annotation there is, so
        // for them the test is the name test; `xs:untyped` and
        // `xs:untypedAtomic` are satisfied only by a node that was never
        // validated, which is a node with no type annotation at all.
        //
        // §4.3 makes the distinction invisible for a tree this processor
        // parsed — it reads the source "as if no schema processing had been
        // performed" — but not for a tree that was handed to it already typed.
        NodeTest::TypedElement { name, untyped_only } => {
            kind == DomNodeType::Element
                && name_matches(name, nav)
                && (!untyped_only || nav.type_annotation().is_none())
        }
        NodeTest::TypedAttribute { name, untyped_only } => {
            kind == DomNodeType::Attribute
                && name_matches(name, nav)
                && (!untyped_only || nav.type_annotation().is_none())
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

#[cfg(test)]
mod tests {
    use bumpalo::Bump;
    use xsd_schema::document::{build_typed_document, BufferDocumentOptions};
    use xsd_schema::pipeline::load_and_process_schema;
    use xsd_schema::SchemaSet;

    use super::*;
    use crate::compile::Compiler;
    use crate::functions::XsltFunctions;
    use crate::runtime::TransformOptions;

    // ── §5.5.4: what may and may not be recovered from ───────────────────

    /// `guard_recoverable` seen directly, including the one case no stylesheet
    /// can reach through the public API in this milestone: an
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
                XsltError::not_implemented("format-number()", "M3d"),
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

    /// Swallowing an error must also clear the parked-error side channel, so a
    /// host error the engine dropped cannot surface from a later, unrelated
    /// evaluation.
    #[test]
    fn recovering_from_an_error_clears_the_parked_error() {
        let names = xsd_schema::namespace::NameTable::new();
        let functions = XsltFunctions::new();
        let sheet = Compiler::new(&names, &functions)
            .compile_str(
                "<xsl:stylesheet version=\"2.0\" \
                 xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\
                 <xsl:template match=\"/\"/></xsl:stylesheet>",
                "file:///parked.xsl",
            )
            .expect("the stylesheet compiles");
        let arena = Bump::new();
        let run = sheet.transformation(&arena, TransformOptions::default());
        *run.parked_error.borrow_mut() = Some(XsltError::not_implemented("format-number()", "M3d"));
        assert_eq!(
            run.guard_recoverable(Err(XsltError::new("FORG0001", "x"))),
            Ok(false)
        );
        assert!(
            run.parked_error.borrow().is_none(),
            "the park must not outlive the match that dropped its error"
        );
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

    fn element_test(local: &str, untyped_only: bool) -> NodeTest {
        NodeTest::TypedElement {
            name: Some(QName::new("", local)),
            untyped_only,
        }
    }

    fn attribute_test(local: &str, untyped_only: bool) -> NodeTest {
        NodeTest::TypedAttribute {
            name: Some(QName::new("", local)),
            untyped_only,
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

        assert!(
            !node_test_matches(&element_test("item", true), PatternAxis::Child, &nav),
            "element(item, xs:untyped) must not match a schema-typed element"
        );
        assert!(
            node_test_matches(&element_test("item", false), PatternAxis::Child, &nav),
            "element(item, xs:anyType) matches every element"
        );
        // The one-argument form has never looked at the type.
        assert!(node_test_matches(
            &NodeTest::Element(Some(QName::new("", "item"))),
            PatternAxis::Child,
            &nav
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

        assert!(
            !node_test_matches(&attribute_test("n", true), PatternAxis::Attribute, &nav),
            "attribute(n, xs:untypedAtomic) must not match a schema-typed attribute"
        );
        assert!(
            node_test_matches(&attribute_test("n", false), PatternAxis::Attribute, &nav),
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

        let mut element = root.clone();
        assert!(element.move_to_first_child());
        assert!(element.move_to_first_child());
        assert_eq!(element.local_name(), "item");
        assert_eq!(element.type_annotation(), None);
        for untyped_only in [true, false] {
            assert!(
                node_test_matches(
                    &element_test("item", untyped_only),
                    PatternAxis::Child,
                    &element
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
                    &attribute_test("n", untyped_only),
                    PatternAxis::Attribute,
                    &attribute
                ),
                "untyped_only={untyped_only}"
            );
        }
    }
}
