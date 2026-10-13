//! `xsl:number` at run time (§12): the place marker — from `value` (§12.1) or
//! from the position of a node (§12.2) — and its conversion to a string by the
//! pure formatter in `crate::numbering` (§12.3).
//!
//! # Cost
//!
//! Every walk goes up (parents) or forward (first child, next sibling) — the
//! navigator finds a previous sibling by scanning from the first one — and
//! `NumberMemo` lets an evaluation continue where an earlier evaluation of the
//! same instruction stopped: in the same tree (`level="any"`, the last
//! `ANY_MARKS` trees) or under the same parent (`single`, `multiple`, the last
//! `SIBLING_MARKS` parents), for each implied count pattern separately.
//! Numbering the nodes of a tree in document order is therefore linear in the
//! tree — times the number of names interleaved under an implied count, the
//! first row below: 50,000 elements take 0.04–0.13 s at every level, also with
//! an implied count over 40 interleaved names, or with the nodes of two trees
//! numbered in turn.
//!
//! What does not scale linearly — measured after the M3c review (release build;
//! a flat document of N elements, each with a text child; the time includes
//! compiling, parsing and serializing):
//!
//! | Shape | Why | Measured |
//! |---|---|---|
//! | Implied count, many names interleaved | each evaluation walks back to the previous node of its own name: about names × nodes tests | N = 50,000 of 200 / 1,000 / 5,000 random names: `any` 0.47 / 1.9 / 8.5 s, `single` 0.15 / 0.46 / 2.4 s |
//! | Reverse document order | every mark is after the node, so each evaluation starts from the root (`any`) or the first sibling | N = 10,000: `any` 2.6 s, `single` 0.64 s, `multiple` 0.64 s |
//! | A random `xsl:sort` order, `level="single"` | the same, for most nodes | N = 10,000: 0.62 s; 50,000: 5.7 s |
//! | A `count` / `from` pattern that reads a local variable or calls a stylesheet function | no memo (`Transformation::numbering_is_stable`) | N = 10,000: `any` 12.2 s, `single` 10.3 s |
//!
//! `tests` guards the linear shapes by counting the nodes the walks test.

use std::borrow::Cow;
use std::collections::HashMap;

use xsd_schema::navigator::{DomNavigator, DomNodeType, XmlNodeOrder};
use xsd_schema::xpath::functions::{atomize_sequence, atomize_to_double, XPathValue};
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::patterns::EvaluationScope;
use super::{Frame, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::FN_NS;
use crate::numbering::{check_attribute, Conversion, Grouping, NumberFormat, Numeral};
use crate::pattern::{Anchor, Pattern};
use crate::receiver::{Nav, Receiver};
use crate::whitespace;

/// `matches-count($node)` (§12.2): the `count` pattern, or the pattern implied
/// by the selected node when there is none.
enum CountTest<'p> {
    Pattern(&'p Pattern),
    /// > If count attribute is not specified, then it defaults to the pattern
    /// > that matches any node with the same node kind as the selected node
    /// > and, if the selected node has an expanded-QName, with the same
    /// > expanded-QName as the selected node.
    Implied {
        kind: DomNodeType,
        /// (namespace URI, local name); `None` for a node without a name.
        name: Option<(String, String)>,
    },
}

/// The implied count pattern a memo was filled with: the node kind and name it
/// matches.
type ImpliedKey = (DomNodeType, Option<(String, String)>);

impl CountTest<'_> {
    /// The implied pattern's kind and name; `None` for a `count` attribute.
    fn implied_key(&self) -> Option<ImpliedKey> {
        match self {
            CountTest::Pattern(_) => None,
            CountTest::Implied { kind, name } => Some((*kind, name.clone())),
        }
    }

    fn implied_by(node: &Nav<'_>) -> CountTest<'static> {
        let kind = node_kind(node);
        let named = match kind {
            DomNodeType::Element | DomNodeType::Attribute | DomNodeType::ProcessingInstruction => {
                true
            }
            // XDM: a namespace node's name is its prefix, and the namespace
            // node of the default namespace has none.
            DomNodeType::Namespace => !node.local_name().is_empty(),
            _ => false,
        };
        CountTest::Implied {
            kind,
            name: named.then(|| {
                (
                    node.namespace_uri().to_string(),
                    node.local_name().to_string(),
                )
            }),
        }
    }
}

/// The node kind of `node`, with the three text-like kinds of the navigator
/// taken as the one XDM text kind.
fn node_kind(node: &Nav<'_>) -> DomNodeType {
    let kind = node.node_type();
    if kind.is_text_like() {
        DomNodeType::Text
    } else {
        kind
    }
}

/// The two functions of §12.2 for one evaluation of the instruction.
struct Numbering<'p, 'f, 't> {
    count: CountTest<'p>,
    from: Option<&'p Pattern>,
    scope: EvaluationScope<'f, 't>,
}

/// What the last evaluations of one `xsl:number` instruction counted, kept for
/// the next ones (`Transformation::number_memos`, by
/// `NumberInstruction::memo_slot`) so that numbering many nodes in document
/// order costs one pass over them instead of one per node.
///
/// Used only when the instruction's patterns are stable
/// ([`Transformation::numbering_is_stable`]): then a node matches them the
/// same way in every evaluation, and a count made by one evaluation is a count
/// the next would make. Without a `count` attribute the pattern is implied by
/// the selected node, so the marks are kept per implied pattern (the kind and
/// name of the node), every one of them for the whole run. The memo is taken
/// out of the run while an evaluation uses it, so an evaluation of the same
/// instruction reached from inside a pattern starts without one.
#[derive(Default)]
pub(super) struct NumberMemo<'t> {
    /// Whether the patterns are stable; decided at the first evaluation.
    stable: Option<bool>,
    /// The marks per implied count pattern (`None`: the `count` attribute).
    ///
    /// Not bounded, and nothing is ever dropped: an instruction without
    /// `count` that numbers the nodes of a document in document order goes
    /// from one name to another at every node, and a name whose marks were
    /// dropped counts every later node of that name again from the start of
    /// the tree (or of the siblings) — a bounded table of 16 names made 50,000
    /// elements of 17 names cost 51 s instead of 0.07 s. The number of keys
    /// is bounded by the distinct implied patterns the instruction meets in
    /// the run — the distinct node kinds and expanded names (a namespace
    /// node's: its prefix) of the nodes it numbers, a vocabulary, not a
    /// multiple of the document's size — and each holds at most [`ANY_MARKS`]
    /// `any` marks and [`SIBLING_MARKS`] sibling marks.
    by_pattern: HashMap<Option<ImpliedKey>, Marks<'t>>,
}

/// How many parents a [`Marks`] remembers a numbered child of.
const SIBLING_MARKS: usize = 32;

/// How many trees a [`Marks`] remembers the `level="any"` walk of.
const ANY_MARKS: usize = 32;

/// What one count pattern's last evaluations counted.
#[derive(Default)]
struct Marks<'t> {
    /// `level="single"` / `"multiple"`: the last $AF node numbered under each
    /// of the [`SIBLING_MARKS`] parents most recently seen, and its number,
    /// most recent last.
    siblings: Vec<SiblingMark<'t>>,
    /// `level="any"`: where the last walk in each of the [`ANY_MARKS`] trees
    /// most recently numbered in ended and the count it had there, most
    /// recent last — one instruction numbering the nodes of two trees in
    /// turn continues each tree's walk instead of starting it again at every
    /// switch.
    any: Vec<AnyMark<'t>>,
}

impl<'t> NumberMemo<'t> {
    /// The marks for `key` (created empty if absent).
    fn marks(&mut self, key: Option<ImpliedKey>) -> &mut Marks<'t> {
        self.by_pattern.entry(key).or_default()
    }
}

/// A node, its parent, and `1 + count(preceding-sibling::node()
/// [matches-count(.)])` for it.
struct SiblingMark<'t> {
    parent: Nav<'t>,
    node: Nav<'t>,
    number: u64,
}

/// The `level="any"` count at `node` (not an attribute or namespace node): the
/// number of nodes from the last `from` match up to and including `node` that
/// match `count`.
struct AnyMark<'t> {
    root: Nav<'t>,
    node: Nav<'t>,
    count: u64,
}

impl<'a, 't> Transformation<'a, 't> {
    /// Evaluates an `xsl:number` instruction (§12): "The result of the
    /// instruction is a newly constructed text node containing the formatted
    /// number as its string value."
    pub(super) fn number(
        &self,
        number: &NumberInstruction,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let place_marker = match &number.source {
            NumberSource::Value(value) => {
                self.supplied_place_marker(*value, number.backwards_compatible, frame, cx)?
            }
            NumberSource::Node {
                select,
                level,
                count,
                from,
            } => {
                let node = self.numbered_node(*select, frame, cx)?;
                let numbering = Numbering {
                    count: match count {
                        Some(id) => CountTest::Pattern(self.sheet.pattern(*id)),
                        None => CountTest::implied_by(&node),
                    },
                    from: from.map(|id| self.sheet.pattern(id)),
                    scope: EvaluationScope::new(frame),
                };
                let slot = number.memo_slot;
                let mut memo = self
                    .number_memos
                    .borrow_mut()
                    .remove(&slot)
                    .unwrap_or_default();
                let stable = *memo
                    .stable
                    .get_or_insert_with(|| self.numbering_is_stable(*count, *from));
                let marks = stable.then(|| memo.marks(numbering.count.implied_key()));
                let numbers = self.place_marker(&node, *level, &numbering, marks)?;
                self.number_memos.borrow_mut().insert(slot, memo);
                numbers.into_iter().map(Numeral::Int).collect()
            }
        };
        let format = match &number.format {
            None => Cow::Owned(NumberFormat::parse("1")),
            Some(NumberFormatSource::Fixed(format)) => Cow::Borrowed(format),
            Some(NumberFormatSource::Computed(avt)) => {
                Cow::Owned(NumberFormat::parse(&self.avt(avt, frame, cx)?))
            }
        };
        let conversion = self.conversion(number, frame, cx)?;
        out.text(&format.format(&place_marker, &conversion), false)
    }

    /// §12.1: the place marker of `value`.
    ///
    /// > The value of this expression is atomized using the procedure defined
    /// > in \[XPath 2.0\], and each value $V in the atomized sequence is then
    /// > converted to the integer value returned by the XPath expression
    /// > `xs:integer(round(number($V)))`.
    ///
    /// > \[ERR XTDE0980\] It is a non-recoverable dynamic error if any
    /// > undiscarded item in the atomized sequence supplied as the value of the
    /// > value attribute of xsl:number cannot be converted to an integer, or if
    /// > the resulting integer is less than 0 (zero).
    ///
    /// With backwards-compatible behaviour (DESIGN §9.10 row 7):
    ///
    /// > all items in the atomized sequence after the first are discarded; If
    /// > the atomized sequence is empty, it is replaced by a sequence containing
    /// > the xs:double value NaN as its only item; If any value in the sequence
    /// > cannot be converted to an integer (this includes the case where the
    /// > sequence contains a NaN value) then the string NaN is inserted into
    /// > the formatted result string in its proper position. The error
    /// > described in the following paragraph does not apply in this case.
    ///
    /// "This case" is the value that cannot be converted; a negative value can
    /// be, so XTDE0980 still applies to it.
    fn supplied_place_marker(
        &self,
        value: ExprId,
        backwards_compatible: bool,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<Numeral>, XsltError> {
        let value = self.eval_expr(value, frame, cx)?;
        let mut atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
        if backwards_compatible {
            atoms.truncate(1);
            if atoms.is_empty() {
                return Ok(vec![Numeral::NaN]);
            }
        }
        let mut numbers = Vec::with_capacity(atoms.len());
        for atom in atoms {
            let double = atomize_to_double(XPathValue::<Nav<'t>>::from_atomic(atom.clone()))
                .map_err(|e| self.xpath_error(e))?;
            let rounded = xpath_round(double);
            if !rounded.is_finite() {
                if backwards_compatible {
                    numbers.push(Numeral::NaN);
                    continue;
                }
                return Err(XsltError::new(
                    codes::XTDE0980,
                    format!(
                        "xsl:number cannot convert {:?} to an integer",
                        self.atom_string(&atom)
                    ),
                ));
            }
            if rounded < 0.0 {
                return Err(XsltError::new(
                    codes::XTDE0980,
                    format!(
                        "xsl:number cannot format {:?}: the integer is less than zero",
                        self.atom_string(&atom)
                    ),
                ));
            }
            numbers.push(integral(rounded));
        }
        Ok(numbers)
    }

    /// §12.2: the selected node.
    ///
    /// > If the select attribute is present, then the expression contained in
    /// > the select attribute is evaluated to determine the selected node. If
    /// > the select attribute is omitted, then the selected node is the context
    /// > node.
    ///
    /// > \[ERR XTTE0990\] It is a type error if the xsl:number instruction is
    /// > evaluated, with no value or select attribute, when the context item is
    /// > not a node.
    ///
    /// > \[ERR XTTE1000\] It is a type error if the result of evaluating the
    /// > select attribute of the xsl:number instruction is anything other than
    /// > a single node.
    ///
    /// An absent context item is XTTE0990 too (`number-0824`: an `xsl:number`
    /// in the body of a stylesheet function, where "the focus is initially
    /// undefined", §10.3).
    fn numbered_node(
        &self,
        select: Option<ExprId>,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Nav<'t>, XsltError> {
        match select {
            None => match &cx.item {
                Some(XmlItem::Node(node)) => Ok(node.clone()),
                Some(XmlItem::Atomic(atom)) => Err(XsltError::new(
                    codes::XTTE0990,
                    format!(
                        "xsl:number has no value or select attribute, and the context item is \
                         the atomic value {:?}, not a node",
                        self.atom_string(atom)
                    ),
                )),
                None => Err(XsltError::new(
                    codes::XTTE0990,
                    "xsl:number has no value or select attribute, and there is no context item",
                )),
            },
            Some(select) => {
                let mut items = self.eval_expr(select, frame, cx)?.into_vec();
                match (items.len(), items.pop()) {
                    (1, Some(XmlItem::Node(node))) => Ok(node),
                    (n, last) => Err(XsltError::new(
                        codes::XTTE1000,
                        match (n, last) {
                            (0, _) => "the select attribute of xsl:number selected nothing, \
                                       and must select a single node"
                                .to_string(),
                            (1, Some(XmlItem::Atomic(atom))) => format!(
                                "the select attribute of xsl:number selected the atomic value \
                                 {:?}, and must select a single node",
                                self.atom_string(&atom)
                            ),
                            (n, _) => format!(
                                "the select attribute of xsl:number selected {n} items, and \
                                 must select a single node"
                            ),
                        },
                    )),
                }
            }
        }
    }

    /// §12.2: the place marker of the selected node `node` ($S).
    ///
    /// > Let matches-count($node) be a function that returns true if and only
    /// > if the given node $node matches the pattern given in the count
    /// > attribute, or the implied pattern (according to the rules given above)
    /// > if the count attribute is omitted.
    ///
    /// > Let matches-from($node) be a function that returns true if and only if
    /// > the given node $node matches the pattern given in the from attribute,
    /// > or if $node is the root node of a tree. If the from attribute is
    /// > omitted, then the function returns true if and only if $node is the
    /// > root node of a tree.
    ///
    /// Every walk goes up (parents) or forward (first child, next sibling):
    /// the navigator finds a previous sibling by scanning from the first one,
    /// so walking backwards would cost the square of the number of siblings.
    /// A pattern has no side effects and an error while matching one is
    /// recoverable (§5.5.4), so the order in which nodes are matched is not
    /// observable (with `TransformOptions::strict_recoverable` it decides
    /// which error is reported first, as everywhere else). The single and
    /// multiple levels never match an ancestor above $F; `level="any"` matches
    /// every node before $S, as the spec's own `$A` does.
    fn place_marker(
        &self,
        node: &Nav<'t>,
        level: NumberLevel,
        numbering: &Numbering<'_, '_, 't>,
        marks: Option<&mut Marks<'t>>,
    ) -> Result<Vec<u64>, XsltError> {
        match level {
            // > When level="single": Let $A be the node sequence selected by
            // > the following expression:
            // > `$S/ancestor-or-self::node()[matches-count(.)][1]` … Let $F be
            // > … `$S/ancestor-or-self::node()[matches-from(.)][1]` … Let $AF
            // > be the value of: `$A[ancestor-or-self::node()[. is $F]]` … If
            // > $AF is empty, return the empty sequence, () Otherwise return
            // > the value of: `1 + count($AF/preceding-sibling::node()
            // > [matches-count(.)])`
            //
            // Walking up from $S, $A is in the subtree of $F exactly when $A is
            // reached no later than $F (both are ancestors-or-self of $S).
            NumberLevel::Single => {
                let mut current = node.clone();
                loop {
                    if self.matches_count(numbering, &current)? {
                        let numbers = self.sibling_numbers(numbering, vec![current], marks)?;
                        return Ok(numbers);
                    }
                    if self.matches_number_from(numbering, &current)? || !current.move_to_parent() {
                        return Ok(Vec::new());
                    }
                }
            }
            // > When level="multiple": Let $A be the node sequence selected by
            // > the expression `$S/ancestor-or-self::node()[matches-count(.)]`
            // > … Let $F be … `$S/ancestor-or-self::node()[matches-from(.)][1]`
            // > … Let $AF be the value of `$A[ancestor-or-self::node()[. is
            // > $F]]` … Return the result of the expression `for $af in $AF
            // > return 1+count($af/preceding-sibling::node()[matches-count(.)])`
            NumberLevel::Multiple => {
                let mut counted = Vec::new();
                let mut current = node.clone();
                loop {
                    if self.matches_count(numbering, &current)? {
                        counted.push(current.clone());
                    }
                    if self.matches_number_from(numbering, &current)? || !current.move_to_parent() {
                        break;
                    }
                }
                // $AF is in document order: outermost first.
                counted.reverse();
                self.sibling_numbers(numbering, counted, marks)
            }
            NumberLevel::Any => self.number_any(numbering, node, marks),
        }
    }

    /// `1 + count($af/preceding-sibling::node()[matches-count(.)])` for each
    /// `$af` of `counted`; an attribute or namespace node, and the root of a
    /// tree, have no siblings.
    ///
    /// The siblings are walked forward from the first one — or, with a memo,
    /// from the node of the same parent this instruction numbered last, if it
    /// comes before: numbering every child of a parent in document order then
    /// costs one pass over the children, not one per child. The memo then
    /// remembers `counted` and their numbers.
    fn sibling_numbers(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        counted: Vec<Nav<'t>>,
        marks: Option<&mut Marks<'t>>,
    ) -> Result<Vec<u64>, XsltError> {
        let previous: &[SiblingMark<'t>] = match &marks {
            Some(marks) => &marks.siblings,
            None => &[],
        };
        let mut numbers = Vec::with_capacity(counted.len());
        let mut made = Vec::with_capacity(counted.len());
        for node in counted {
            let mut parent = node.clone();
            if matches!(
                node.node_type(),
                DomNodeType::Attribute | DomNodeType::Namespace
            ) || !parent.move_to_parent()
            {
                numbers.push(1);
                continue;
            }
            let mark = previous.iter().find(|mark| {
                mark.parent.is_same_position(&parent)
                    && mark.node.compare_position(&node) != XmlNodeOrder::After
            });
            let number = match mark {
                Some(mark) if mark.node.is_same_position(&node) => mark.number,
                Some(mark) => {
                    // The marked node matched the count pattern (it was an $af
                    // node of the same pattern), so it counts for `node`.
                    let mut sibling = mark.node.clone();
                    self.count_siblings_until(numbering, &mut sibling, &node, mark.number + 1)?
                }
                None => {
                    let mut sibling = parent.clone();
                    if !sibling.move_to_first_child() {
                        return Err(XsltError::internal(
                            "xsl:number: a node's parent has no first child",
                        ));
                    }
                    if sibling.is_same_position(&node) {
                        1
                    } else {
                        self.count_from_sibling(numbering, &mut sibling, &node, 1)?
                    }
                }
            };
            numbers.push(number);
            made.push(SiblingMark {
                parent,
                node,
                number,
            });
        }
        if let Some(marks) = marks {
            for mark in made {
                marks
                    .siblings
                    .retain(|old| !old.parent.is_same_position(&mark.parent));
                marks.siblings.push(mark);
            }
            let excess = marks.siblings.len().saturating_sub(SIBLING_MARKS);
            marks.siblings.drain(..excess);
        }
        Ok(numbers)
    }

    /// From `sibling` (already counted), moves forward to `node`, adding one
    /// to `number` for every sibling in between that matches the count
    /// pattern; returns the number of `node`.
    fn count_siblings_until(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        sibling: &mut Nav<'t>,
        node: &Nav<'t>,
        number: u64,
    ) -> Result<u64, XsltError> {
        if !sibling.move_to_next_sibling() {
            return Err(XsltError::internal(
                "xsl:number: a sibling was not found after an earlier one",
            ));
        }
        if sibling.is_same_position(node) {
            return Ok(number);
        }
        self.count_from_sibling(numbering, sibling, node, number)
    }

    /// From `sibling` (not yet counted, before `node`), moves forward to
    /// `node`, adding one to `number` for every sibling before it that matches
    /// the count pattern.
    fn count_from_sibling(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        sibling: &mut Nav<'t>,
        node: &Nav<'t>,
        mut number: u64,
    ) -> Result<u64, XsltError> {
        loop {
            if self.matches_count(numbering, sibling)? {
                number += 1;
            }
            if !sibling.move_to_next_sibling() {
                return Err(XsltError::internal(
                    "xsl:number: a node was not found among its parent's children",
                ));
            }
            if sibling.is_same_position(node) {
                return Ok(number);
            }
        }
    }

    /// > When level="any": Let $A be the node sequence selected by the
    /// > expression `$S/(preceding::node()|ancestor-or-self::node())
    /// > [matches-count(.)]` … Let $F be … `$S/(preceding::node()|
    /// > ancestor-or-self::node())[matches-from(.)][last()]` … Let $AF be the
    /// > node sequence `$A[. is $F or . >> $F]`. … If $AF is empty, return the
    /// > empty sequence, () Otherwise return the value of the expression
    /// > `count($AF)`
    ///
    /// $S and the nodes before it in document order other than attributes and
    /// namespace nodes, walked forward from the root of the tree: the count
    /// restarts at every node that matches `from` (the root is the first), so
    /// at $S it is `count($AF)`. An attribute or namespace $S comes right
    /// after its element ("the attribute nodes of an element occur after the
    /// element and before its children", XDM §2.4).
    ///
    /// With a memo the walk starts from the node this instruction's last
    /// evaluation in the same tree reached, with the count it had there, when
    /// that node comes before: numbering every node of a tree in document
    /// order then costs one pass over the tree, also when the evaluations go
    /// from one tree to another in between.
    fn number_any(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        node: &Nav<'t>,
        marks: Option<&mut Marks<'t>>,
    ) -> Result<Vec<u64>, XsltError> {
        let on_element = matches!(
            node.node_type(),
            DomNodeType::Attribute | DomNodeType::Namespace
        );
        // The last node of the walk that is not an attribute: $S, or the
        // element of an attribute or namespace $S.
        let mut target = node.clone();
        if on_element && !target.move_to_parent() {
            // A parentless attribute is the whole of its tree.
            let total = self.visit_any(numbering, node, 0)?;
            return Ok(if total == 0 { Vec::new() } else { vec![total] });
        }
        let mut root = node.clone();
        root.move_to_root();
        let start = marks
            .as_ref()
            .and_then(|marks| {
                marks
                    .any
                    .iter()
                    .find(|mark| mark.root.is_same_position(&root))
            })
            .filter(|mark| mark.node.compare_position(&target) != XmlNodeOrder::After);
        let (mut cursor, mut total) = match start {
            Some(mark) => (mark.node.clone(), mark.count),
            None => {
                let total = self.visit_any(numbering, &root, 0)?;
                (root.clone(), total)
            }
        };
        while !cursor.is_same_position(&target) {
            if !next_in_document(&mut cursor) {
                return Err(XsltError::internal(
                    "xsl:number: the selected node was not found after the start of the walk",
                ));
            }
            total = self.visit_any(numbering, &cursor, total)?;
        }
        if let Some(marks) = marks {
            marks.any.retain(|old| !old.root.is_same_position(&root));
            marks.any.push(AnyMark {
                root,
                node: target,
                count: total,
            });
            let excess = marks.any.len().saturating_sub(ANY_MARKS);
            marks.any.drain(..excess);
        }
        if on_element {
            total = self.visit_any(numbering, node, total)?;
        }
        Ok(if total == 0 { Vec::new() } else { vec![total] })
    }

    /// One step of the `level="any"` walk: the count after `node`, which
    /// restarts if `node` matches `from` and counts `node` if it matches
    /// `count`.
    fn visit_any(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        node: &Nav<'t>,
        total: u64,
    ) -> Result<u64, XsltError> {
        let total = if self.matches_number_from(numbering, node)? {
            0
        } else {
            total
        };
        Ok(total + u64::from(self.matches_count(numbering, node)?))
    }

    /// `matches-count($node)`.
    fn matches_count(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        node: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        #[cfg(test)]
        COUNT_TESTS.with(|tests| tests.set(tests.get() + 1));
        match &numbering.count {
            CountTest::Pattern(pattern) => self.matches_in(pattern, node, &numbering.scope),
            CountTest::Implied { kind, name } => Ok(node_kind(node) == *kind
                && name.as_ref().is_none_or(|(uri, local)| {
                    node.local_name() == local && node.namespace_uri() == uri
                })),
        }
    }

    /// `matches-from($node)`.
    fn matches_number_from(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        node: &Nav<'t>,
    ) -> Result<bool, XsltError> {
        let mut parent = node.clone();
        if !parent.move_to_parent() {
            return Ok(true);
        }
        match numbering.from {
            Some(pattern) => self.matches_in(pattern, node, &numbering.scope),
            None => Ok(false),
        }
    }

    /// Whether every node matches the `count` and `from` patterns the same
    /// way in every evaluation of the instruction, so that what one
    /// evaluation counted can be reused by the next ([`NumberMemo`]).
    ///
    /// A pattern is matched with nothing of the caller's dynamic context but
    /// the frame (`Transformation::matches_in` builds the predicates' context
    /// from the candidate alone), so what can differ is what a predicate — or
    /// the argument of an `id()` / `key()` anchor — reads: a local variable,
    /// and the nodes a stylesheet function may construct anew at each call
    /// (the same reason `candidate_independent` refuses them in
    /// `runtime/patterns.rs`). A pattern that reads only global variables and
    /// calls only functions in the standard function namespace gives the same
    /// answer for a node throughout the run; `current()` is the node being
    /// matched (§16.6.1), so it is no exception. The implied count pattern
    /// has no predicates.
    fn numbering_is_stable(&self, count: Option<PatternId>, from: Option<PatternId>) -> bool {
        [count, from].into_iter().flatten().all(|id| {
            self.sheet
                .pattern(id)
                .alternatives
                .iter()
                .all(|alternative| {
                    let anchor = match alternative.anchor {
                        Anchor::IdKey(anchor) => Some(anchor.call),
                        _ => None,
                    };
                    alternative
                        .steps
                        .iter()
                        .flat_map(|step| step.predicates.iter().copied())
                        .chain(anchor)
                        .all(|expr| {
                            self.reads_only_globals(expr)
                                && self
                                    .sheet
                                    .expr(expr)
                                    .expr
                                    .function_calls()
                                    .iter()
                                    .all(|call| call.namespace == FN_NS)
                        })
                })
        })
    }

    /// The effective values of the §12.3 conversion attributes other than
    /// `format`, each checked (XTDE0030 for a computed value outside the
    /// permitted ones; a fixed value was checked when the stylesheet was
    /// compiled).
    fn conversion(
        &self,
        number: &NumberInstruction,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Conversion, XsltError> {
        let value = |name: &str, avt: &Option<Avt>| -> Result<Option<String>, XsltError> {
            let Some(avt) = avt else {
                return Ok(None);
            };
            let value = self.avt(avt, frame, cx)?;
            if avt.as_literal().is_none() {
                check_attribute(name, &value)
                    .map_err(|problem| XsltError::new(codes::XTDE0030, problem))?;
            }
            Ok(Some(value))
        };
        // `lang` and `letter-value` are checked and do not change the result
        // (`crate::numbering`: English for every language).
        value("lang", &number.lang)?;
        value("letter-value", &number.letter_value)?;
        let ordinal = value("ordinal", &number.ordinal)?;
        let separator = value("grouping-separator", &number.grouping_separator)?;
        let size = value("grouping-size", &number.grouping_size)?;
        Ok(Conversion {
            ordinal,
            // "If only one of the grouping-separator and grouping-size
            // attributes is specified, then it is ignored."
            grouping: match (separator, size) {
                (Some(separator), Some(size)) => Some(Grouping {
                    separator,
                    // Digits only (checked); one too large for `usize` is a
                    // size no number reaches.
                    size: whitespace::trim(&size).parse().unwrap_or(usize::MAX),
                }),
                _ => None,
            },
        })
    }
}

#[cfg(test)]
thread_local! {
    /// How many nodes `matches_count` has tested on this thread — every walk
    /// of the place marker tests each node it passes, so this is the work the
    /// walks did (the guard of the memo's cost,
    /// `tests::numbering_many_names_in_document_order_is_linear`).
    static COUNT_TESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Moves `node` to the next node in document order that is not an attribute
/// or a namespace node (a node of `node`'s subtree first, then what follows
/// it); `false` after the last node of the tree.
fn next_in_document(node: &mut Nav<'_>) -> bool {
    if node.move_to_first_child() {
        return true;
    }
    loop {
        if node.move_to_next_sibling() {
            return true;
        }
        if !node.move_to_parent() {
            return false;
        }
    }
}

/// XPath 2.0 `fn:round` on an `xs:double`: "the number with no fractional part
/// that is closest to the argument. If there are two such numbers, then the one
/// that is closest to positive infinity is returned."
fn xpath_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let floor = value.floor();
    // Exact: below 2^52 the fraction is representable, above it there is none.
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// `xs:integer` of a finite, non-negative `xs:double` with no fractional part.
fn integral(value: f64) -> Numeral {
    // 2^64: every smaller integral double fits in a u64.
    if value < 18_446_744_073_709_551_616.0 {
        Numeral::Int(value as u64)
    } else {
        // Exact: Rust prints the double's own value, every digit of it.
        Numeral::Big(format!("{value:.0}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bumpalo::Bump;
    use xsd_schema::namespace::NameTable;

    use crate::{Compiler, TransformOptions, XsltFunctions};

    /// Numbers `select` (in document order) with `<xsl:number {attributes}/>`
    /// over `<doc>` + `elements` children named `n0`, `n1`, … `n{names - 1}`
    /// in turn, each with a text child. Returns the result and how many nodes
    /// the walks tested.
    fn number_cycling_names(
        select: &str,
        attributes: &str,
        names: usize,
        elements: usize,
    ) -> (String, u64) {
        let sheet = format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:output method="text"/>
                 <xsl:template match="/"><xsl:for-each select="{select}"><xsl:number {attributes}/>,</xsl:for-each></xsl:template>
               </xsl:stylesheet>"#
        );
        let mut source = String::from("<doc>");
        for i in 0..elements {
            source.push_str(&format!("<n{0}>t</n{0}>", i % names));
        }
        source.push_str("</doc>");
        run_counting(&sheet, &source)
    }

    /// Runs `sheet` over `source`; returns the result and how many nodes the
    /// walks of the place marker tested.
    fn run_counting(sheet: &str, source: &str) -> (String, u64) {
        let functions = XsltFunctions::new();
        let table = NameTable::new();
        let sheet = Compiler::new(&table, &functions)
            .compile_str(sheet, "file:///number/main.xsl")
            .expect("the stylesheet compiles");
        let arena = Bump::new();
        let run = sheet.transformation(&arena, TransformOptions::default());
        let before = COUNT_TESTS.with(|tests| tests.get());
        let outcome = run
            .transform_source("file:///number/in.xml", source)
            .expect("the run succeeds");
        let tested = COUNT_TESTS.with(|tests| tests.get()) - before;
        let text = sheet.serialize_primary(&outcome).expect("serializes");
        (text, tested)
    }

    /// Review F1: an `xsl:number` without `count` keeps marks for every name
    /// it numbers, so numbering the elements of a document with more names
    /// than any small bound (40 here, cycled — the order that defeats a
    /// least-recently-used table of fewer entries) costs a number of tests
    /// proportional to the document, not to its square. With marks for only
    /// 16 names (the M3c version) the walks test about `elements²` nodes;
    /// here they test about `elements × 2 × names` (each element walks back
    /// to the previous one of its name), and twice the elements cost twice
    /// as much.
    #[test]
    fn numbering_many_names_in_document_order_is_linear() {
        const NAMES: usize = 40;
        const ELEMENTS: usize = 4_000;
        // `doc`, the elements, their text children (the document node is not
        // an element, and `//*` does not number it).
        let tree = 1 + 2 * ELEMENTS as u64;
        for level in ["any", "single", "multiple"] {
            let attributes = format!(r#"level="{level}""#);
            let (text, tested) = number_cycling_names("//*", &attributes, NAMES, ELEMENTS);
            // The element at position `i` is the `i / NAMES + 1`-th of its
            // name: the same for every level in a flat document.
            let expected: String = std::iter::once("1,".to_string())
                .chain((0..ELEMENTS).map(|i| format!("{},", i / NAMES + 1)))
                .collect();
            assert_eq!(text, expected, "level={level}");
            let bound = 2 * (NAMES as u64 + 1) * tree;
            assert!(
                tested <= bound,
                "level={level}: {tested} nodes tested for a tree of {tree} (at most {bound})"
            );
            let (_, doubled) = number_cycling_names("//*", &attributes, NAMES, 2 * ELEMENTS);
            assert!(
                doubled * 10 <= tested * 25,
                "level={level}: twice the elements tested {doubled} nodes, {tested} before"
            );
        }
    }

    /// Review F5: the `level="any"` marks are kept per tree, for several
    /// trees: one instruction numbering the elements of a document and of a
    /// copy of it in turn continues each tree's walk where it stopped, where
    /// one mark for all trees started the walk again from the root at every
    /// switch (about `elements²` tests: 4.7 s for two trees of 10,000).
    #[test]
    fn numbering_two_trees_in_turn_is_linear() {
        const ELEMENTS: usize = 2_000;
        let sheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:output method="text"/>
                 <xsl:template match="/">
                   <xsl:variable name="copy"><xsl:copy-of select="/"/></xsl:variable>
                   <xsl:for-each select="//e | $copy//e"><xsl:sort select="@i" data-type="number"/><xsl:number level="any" count="e"/>,</xsl:for-each>
                 </xsl:template>
               </xsl:stylesheet>"#;
        let mut source = String::from("<doc>");
        for i in 0..ELEMENTS {
            source.push_str(&format!(r#"<e i="{i}"/>"#));
        }
        source.push_str("</doc>");
        let (text, tested) = run_counting(sheet, &source);
        // The sort is stable: the `i`-th element of one tree, then of the
        // other, each the `i + 1`-th `e` of its tree.
        let expected: String = (1..=ELEMENTS).map(|n| format!("{n},{n},")).collect();
        assert_eq!(text, expected);
        // Two trees of a document node, `doc` and the elements.
        let trees = 2 * (2 + ELEMENTS as u64);
        assert!(
            tested <= 4 * trees,
            "{tested} nodes tested for two trees of {trees} nodes (at most {})",
            4 * trees
        );
    }

    /// The sibling marks are kept per parent, for several parents: numbering
    /// `//node()` with `count="node()"` alternates between the children of
    /// `doc` and the text child of each, and a memo of one parent would
    /// count `doc`'s children from the first one again after every text node
    /// (the review's surviving mutation M20: about `elements² / 2` tests).
    #[test]
    fn numbering_alternating_parents_in_document_order_is_linear() {
        const ELEMENTS: usize = 4_000;
        let (text, tested) = number_cycling_names("//node()", r#"count="node()""#, 1, ELEMENTS);
        // `doc` is 1; each element is its position among `doc`'s children,
        // and its text child is 1.
        let expected: String = std::iter::once("1,".to_string())
            .chain((0..ELEMENTS).map(|i| format!("{},1,", i + 1)))
            .collect();
        assert_eq!(text, expected);
        let tree = 1 + 2 * ELEMENTS as u64;
        assert!(
            tested <= 4 * tree,
            "{tested} nodes tested for a tree of {tree} (at most {})",
            4 * tree
        );
    }

    #[test]
    fn round_half_towards_positive_infinity() {
        assert_eq!(xpath_round(2.5), 3.0);
        assert_eq!(xpath_round(-2.5), -2.0);
        assert_eq!(xpath_round(-0.5), 0.0);
        assert_eq!(xpath_round(6.5000000000), 7.0);
        assert_eq!(xpath_round(2.499999999), 2.0);
        assert_eq!(xpath_round(0.49999999999999994), 0.0);
        assert_eq!(xpath_round(1e300), 1e300);
        assert!(xpath_round(f64::NAN).is_nan());
    }

    #[test]
    fn integral_values_beyond_u64_keep_every_digit() {
        assert_eq!(integral(7.0), Numeral::Int(7));
        // u64::MAX is about 1.84e19.
        assert_eq!(integral(1.8e19), Numeral::Int(18_000_000_000_000_000_000));
        assert_eq!(integral(2e19), Numeral::Big("20000000000000000000".into()));
        assert_eq!(
            integral(1e100),
            Numeral::Big(
                "10000000000000000159028911097599180468360808563945281389781327557747838772170381060813469985856815104"
                    .into()
            )
        );
    }
}
