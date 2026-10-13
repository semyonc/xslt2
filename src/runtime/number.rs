//! `xsl:number` at run time (§12): the place marker — from `value` (§12.1) or
//! from the position of a node (§12.2) — and its conversion to a string by the
//! pure formatter in `crate::numbering` (§12.3).
//!
//! # Cost
//!
//! Every walk goes up (parents) or forward (first child, next sibling) — the
//! navigator finds a previous sibling by scanning from the first one. The
//! nodes before $S are counted by a `Walk`: the nodes of its tree in document
//! order (`level="any"`) or the children of its parent (`single`,
//! `multiple`), with the count at the walk's frontier. Without a `count`
//! attribute a walk counts every implied pattern at once (`Census`, by node
//! kind and expanded name), so one walk serves every name and namespace; a
//! node the walk has passed is looked up in an index of the numbers it gave,
//! made the first time a node before the frontier is asked for (`Index`);
//! and `NumberMemo` keeps the walks of an instruction from one evaluation to
//! the next — by owner, for every tree and every parent (`Walks`: always a
//! walk past `SHORT_WALK` = 32 nodes, a shorter one until more than
//! `KEPT_WALKS` = 1,024 walks are kept) — while the local variables its
//! patterns read keep their values. So numbering is linear in the nodes
//! walked and the evaluations, in any order of evaluation, over any number
//! of names, namespaces, trees and parents, and with patterns that read
//! local variables: a kept walk visits each node at most twice, and an
//! evaluation that needs a dropped short walk again walks at most 32 nodes
//! of it again. Memory: 16 bytes per node a walk indexed, a census entry per
//! kind and name a walk met, and the walks themselves (168 bytes each, and
//! their hash table) — at most 1,024, or twice the walks past 32 nodes.
//! Measured (release build; the time includes compiling, parsing and
//! serializing; a flat document of N elements, each with a text child, or
//! nested chapters / sections / paragraphs; "baseline" is `name()` instead
//! of `xsl:number`, 0.028–0.031 s at N = 50,000):
//!
//! | Shape | Time |
//! |---|---|
//! | Document order, N = 50,000, every level, `count="*"` or implied, flat or nested | 0.033–0.056 s |
//! | Implied count over 200 / 1,000 / 5,000 random names, N = 50,000 (before the follow-up: `any` 0.47 / 1.8 / 8.3 s, `single` 0.15 / 0.48 / 2.4 s) | 0.037–0.042 s |
//! | Reverse document order, N = 50,000, every level (before, at N = 10,000: `any` 2.5 s, `single` / `multiple` 0.64 s) | 0.036–0.060 s |
//! | A pseudo-random `xsl:sort` order, N = 50,000, `any` / `single` (before: `single` 5.4 s) | 0.070 s |
//! | `count="*[$yes]"`, a local variable, N = 50,000, every level (before, at N = 10,000: `any` 12.1 s, `single` 10.1 s) | 0.046–0.11 s |
//! | Item k of each of S sections in turn (sorted by position), `single`, S × 1,500 / S × 4,000 items at S = 33 / 50 (before the review fixes, from 33 sections on: 0.77 / 8.0 s; baseline 0.059 / 0.24 s) | 0.064 / 0.26 s |
//! | Element i of each of T copies of a document in turn, `any`, T × 2,000 at T = 33 / 40 (before the review fixes, from 33 trees on: 3.7 / 4.4 s; baseline 0.107 / 0.130 s) | 0.108 / 0.132 s |
//! | 50,000 elements of one local name in 50,000 namespaces, implied, every level (before the review fixes: 1.8 s; baseline 0.047 s) | 0.056–0.061 s |
//!
//! What stays quadratic — each evaluation counts afresh from the start of
//! the tree or of the siblings, for a measured reason:
//!
//! | Shape | Why | N = 10,000, `any` |
//! |---|---|---|
//! | A `count` / `from` pattern that calls a stylesheet function | each call may construct new nodes, so a node can match differently in the next evaluation (`Stability::Unstable`) | 19.0 s |
//! | A local variable the pattern reads that changes value at every evaluation (`$p` bound to `position()`) | the walks are reset when the values change | 20.8 s |
//! | A local variable holding an atomic value of a type compared only by a lossy equality (`xs:decimal`, dates, QNames …) | not keyed (`comparable`) | as above |
//!
//! `tests` guards the linear shapes by counting the nodes the walks test.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use xsd_schema::navigator::{DomNavigator, DomNodeType, XmlNodeOrder};
use xsd_schema::types::{XmlAtomicValue, XmlValueKind};
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

impl CountTest<'_> {
    fn is_implied(&self) -> bool {
        matches!(self, CountTest::Implied { .. })
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
/// `NumberInstruction::memo_slot`) so that numbering many nodes costs about
/// one pass over them instead of one per node.
///
/// Used only when the instruction's patterns are stable ([`Stability`]): then
/// a node matches them the same way in every evaluation whose local variables
/// (those the patterns read) have the same values, and a count made by one
/// such evaluation is a count the next would make. When those values change,
/// the walks are dropped and counting starts again. The memo is taken out of
/// the run while an evaluation uses it, so an evaluation of the same
/// instruction reached from inside a pattern starts without one.
#[derive(Default)]
pub(super) struct NumberMemo<'t> {
    /// Whether, and with which local variables, the walks can be reused;
    /// decided at the first evaluation.
    stability: Option<Stability>,
    /// The values of those local variables the walks were made with.
    locals: Vec<XPathValue<Nav<'t>>>,
    walks: Walks<'t>,
}

/// Whether the walks of one evaluation of an instruction can serve the next
/// ([`Transformation::numbering_stability`]).
enum Stability {
    /// A pattern calls a function outside the standard function namespace —
    /// a stylesheet function, which may construct new nodes at each call:
    /// every evaluation counts afresh.
    Unstable,
    /// The patterns read these local variable slots (none: only global
    /// variables, fixed for the run) and call only functions in the standard
    /// function namespace: the walks serve every evaluation in which the
    /// slots hold the same values.
    Stable(Vec<usize>),
}

/// A walk that has visited at most this many nodes is short: [`Walks`] may
/// drop it, and an evaluation that needs it again walks at most this many
/// nodes again.
const SHORT_WALK: u32 = 32;

/// The fewest walks [`Walks`] holds before it drops the short ones.
const KEPT_WALKS: usize = 1_024;

/// The walks of one instruction (with a memo) or of one evaluation (without),
/// by owner: `level="any"` walks the nodes of a tree in document order, one
/// walk per tree; `single` and `multiple` walk the children of a parent, one
/// walk per parent. An instruction numbering the nodes of many trees, or the
/// children of many parents, in turn continues each walk where it stopped,
/// and one numbering `//node()` continues both the walk of `doc`'s children
/// and that of each child's own.
///
/// Every walk that has visited more than [`SHORT_WALK`] nodes (a node visited
/// again counted again) is kept, however many trees or parents are
/// interleaved. The short ones are kept too until the walks number `limit` —
/// [`KEPT_WALKS`], or twice the walks left by the last drop if more — and are
/// then all dropped: numbering `//node()` in a document of half a million
/// elements, each the parent of a text node, would otherwise keep half a
/// million walks of one node (and double the memory of the run). A dropped
/// walk needed again is made again and walks from its start: at most
/// [`SHORT_WALK`] nodes, or more only once, since it is then long and kept.
/// So an evaluation walks at most [`SHORT_WALK`] nodes again per walk it
/// needs (one, or one per level of `multiple`), each node is visited at most
/// twice by a kept walk ([`Index`]), and the walks are at most `limit`, of a
/// fixed size (`Walk`'s) — at most one long walk per [`SHORT_WALK`] + 1
/// visits — besides what each counted: an index entry per node it visited
/// and a census entry per kind and name it met ([`Census`]). Time and memory
/// are in proportion to the nodes walked and the evaluations.
struct Walks<'t> {
    by_owner: HashMap<Owner, Walk<'t>, BuildHasherDefault<OwnerHasher>>,
    /// When `by_owner` holds this many walks, the short ones are dropped
    /// before another is made.
    limit: usize,
}

impl Default for Walks<'_> {
    fn default() -> Self {
        Walks {
            by_owner: HashMap::default(),
            limit: KEPT_WALKS,
        }
    }
}

/// The key of a [`Walk`] in [`Walks`]: its route, and its owner — a tree's
/// root or a parent — as the serial of its document (unique in the process)
/// and its position in the document's node array: a function of the owner's
/// identity (`is_same_position`), which [`Walks::of`] still checks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Owner {
    route: Route,
    document: u64,
    node: u32,
}

/// The hasher of [`Owner`] keys: the multiply-rotate of `rustc`'s
/// `FxHasher`, which costs a fraction of SipHash on three integers — one
/// lookup per evaluation pays it. No document chooses the keys freely: they
/// are a route, a serial and a node index, and nodes whose indexes agree in
/// the bits a table of `2^k` buckets reads are at least `2^k` nodes apart.
#[derive(Default)]
struct OwnerHasher(u64);

impl OwnerHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for OwnerHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.add(u64::from(byte));
        }
    }

    fn write_u32(&mut self, word: u32) {
        self.add(u64::from(word));
    }

    fn write_u64(&mut self, word: u64) {
        self.add(word);
    }

    fn write_usize(&mut self, word: usize) {
        self.add(word as u64);
    }

    fn write_isize(&mut self, word: isize) {
        self.add(word as u64);
    }
}

impl<'t> Walks<'t> {
    /// The walk of `owner` (a tree's root, or a parent) on `route`; a new one
    /// if there is none.
    fn of(&mut self, route: Route, owner: &Nav<'t>, implied: bool) -> &mut Walk<'t> {
        let key = Owner {
            route,
            document: owner.document().serial(),
            node: owner.current_ref(),
        };
        if self.by_owner.len() >= self.limit && !self.by_owner.contains_key(&key) {
            // Linear in the walks, and at least `limit / 2` new walks apart:
            // constant time per walk made.
            self.by_owner.retain(|_, walk| walk.visited > SHORT_WALK);
            self.limit = KEPT_WALKS.max(2 * self.by_owner.len());
        }
        let walk = self
            .by_owner
            .entry(key)
            .or_insert_with(|| Walk::new(owner.clone(), implied));
        // An owner is a root or a parent — never an attribute or namespace
        // node, the only positions that share a node index with another — so
        // the key is its identity, and this never replaces a walk; it keeps
        // the identity check of the walk's owner the same in every case.
        if !walk.owner.is_same_position(owner) {
            *walk = Walk::new(owner.clone(), implied);
        }
        walk
    }
}

/// Which nodes a [`Walk`] visits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Route {
    /// `level="any"`: the nodes of a tree other than attributes and namespace
    /// nodes, in document order from its root; the count restarts at every
    /// node that matches `from`.
    Tree,
    /// `single` / `multiple`: the children of a parent, in order.
    Children,
}

/// A walk forward over the nodes of a [`Route`], with what it has counted.
///
/// The number of a node is the count at that node, the node included: for
/// `Route::Tree` the nodes that match the count pattern from the last match
/// of `from` (the root is the first) up to it — `count($AF)` of `level="any"`
/// when the node is $S — and for `Route::Children` the siblings up to it that
/// match — `1 + count($af/preceding-sibling::node()[matches-count(.)])` when
/// the node is an $af, which matches.
struct Walk<'t> {
    /// The tree's root, or the parent whose children are walked.
    owner: Nav<'t>,
    /// The last node visited and its number; `None` before the first.
    frontier: Option<(Nav<'t>, u64)>,
    tally: Tally,
    index: Index,
    /// How many nodes the walk has visited, again or not (saturating): above
    /// [`SHORT_WALK`] it is never dropped.
    visited: u32,
}

/// What a [`Walk`] counts.
enum Tally {
    /// The nodes that matched the `count` attribute's pattern.
    Pattern(u64),
    /// Without `count` the pattern is implied by the selected node — its kind
    /// and name (§12.2) — and differs from one evaluation to the next, so a
    /// walk counts every implied pattern at once: each node it visits under
    /// its own kind and name. A node visited by a walk matches the pattern
    /// implied by another exactly when their kinds and names are equal (the
    /// walks never visit attributes and namespace nodes, the only kinds whose
    /// implied pattern can match a node of another name), so one walk serves
    /// every name, however many are interleaved.
    Implied(Box<Census>),
}

impl Tally {
    /// "the count restarts": a node that matches `from`.
    fn restart(&mut self) {
        match self {
            Tally::Pattern(count) => *count = 0,
            Tally::Implied(census) => census.generation += 1,
        }
    }
}

/// How many implied patterns a [`Census`] scans before it looks them up by
/// hash.
const CENSUS_LIST: usize = 8;

/// The nodes counted per implied pattern since the last restart, by node
/// kind and expanded name ([`Census::key`]): a list scanned while it holds
/// at most [`CENSUS_LIST`] patterns — the children of most parents have a few
/// names — and a hash map beyond, so a lookup costs the same however many
/// names, or namespaces under one local name, have been met; found without
/// allocating; restarted in constant time by a generation number (a count of
/// an older generation is a count of 0).
#[derive(Default)]
struct Census {
    few: Vec<(Box<str>, Counted)>,
    /// Every pattern, once there are more than [`CENSUS_LIST`] (`few` is
    /// then empty).
    many: HashMap<Box<str>, Counted>,
    /// The key of the node being counted, written anew for each node.
    key: String,
    generation: u64,
}

/// The count of one implied pattern, and the [`Census`] generation it
/// belongs to.
#[derive(Clone, Copy)]
struct Counted {
    generation: u64,
    count: u64,
}

impl Census {
    /// Counts `node` under the pattern it implies; returns how many nodes of
    /// that pattern have been counted since the last restart, `node`
    /// included.
    fn add(&mut self, node: &Nav<'_>) -> u64 {
        Census::key(node, &mut self.key);
        let generation = self.generation;
        let fresh = Counted {
            generation,
            count: 0,
        };
        let counted = if self.many.is_empty() {
            match self.few.iter().position(|(key, _)| {
                #[cfg(test)]
                CENSUS_PROBES.with(|probes| probes.set(probes.get() + 1));
                **key == *self.key
            }) {
                Some(at) => &mut self.few[at].1,
                None if self.few.len() < CENSUS_LIST => {
                    self.few.push((Box::from(self.key.as_str()), fresh));
                    let last = self.few.len() - 1;
                    &mut self.few[last].1
                }
                None => {
                    self.many.extend(self.few.drain(..));
                    self.many
                        .entry(Box::from(self.key.as_str()))
                        .or_insert(fresh)
                }
            }
        } else {
            #[cfg(test)]
            CENSUS_PROBES.with(|probes| probes.set(probes.get() + 1));
            match self.many.get_mut(self.key.as_str()) {
                Some(counted) => counted,
                None => self
                    .many
                    .entry(Box::from(self.key.as_str()))
                    .or_insert(fresh),
            }
        };
        if counted.generation != generation {
            *counted = fresh;
        }
        counted.count += 1;
        counted.count
    }

    /// Writes into `key` the kind and expanded name of `node` — the pattern
    /// it implies: a letter per kind, then for an element or a processing
    /// instruction (the only kinds a walk visits that have names) the
    /// namespace URI, U+0000 (not an XML character, so it ends the URI
    /// unambiguously) and the local name.
    fn key(node: &Nav<'_>, key: &mut String) {
        key.clear();
        match node_kind(node) {
            kind @ (DomNodeType::Element | DomNodeType::ProcessingInstruction) => {
                key.push(if kind == DomNodeType::Element {
                    'e'
                } else {
                    'p'
                });
                key.push_str(node.namespace_uri());
                key.push('\0');
                key.push_str(node.local_name());
            }
            DomNodeType::Text => key.push('t'),
            DomNodeType::Comment => key.push('c'),
            DomNodeType::Root => key.push('d'),
            // Never visited by a walk (attributes and namespace nodes are
            // numbered without one), but kept apart all the same.
            DomNodeType::Attribute => key.push('a'),
            DomNodeType::Namespace => key.push('n'),
            _ => key.push('x'),
        }
    }
}

/// The numbers a [`Walk`] has given the nodes before its frontier.
enum Index {
    /// Not kept: every node asked for so far came at or after the frontier.
    Off,
    /// The number of every node visited since the walk (re)started, by the
    /// node's position in its document's node array (`current_ref`) — in
    /// document order within one tree, so increasing as a walk goes forward,
    /// and looked up by binary search.
    On(Vec<(u32, u64)>),
    /// The positions did not increase: no index, and a node before the
    /// frontier is counted by walking again from the start.
    Unusable,
}

impl<'t> Walk<'t> {
    fn new(owner: Nav<'t>, implied: bool) -> Self {
        Walk {
            owner,
            frontier: None,
            tally: if implied {
                Tally::Implied(Box::default())
            } else {
                Tally::Pattern(0)
            },
            index: Index::Off,
            visited: 0,
        }
    }

    /// Back to before the first node, keeping only whether to index.
    fn restart(&mut self) {
        self.frontier = None;
        self.tally.restart();
        if let Index::On(entries) = &mut self.index {
            entries.clear();
        }
    }

    /// Notes the number of `node`, just visited, in the index if there is one.
    fn record(&mut self, node: &Nav<'t>, number: u64) {
        if let Index::On(entries) = &mut self.index {
            let at = node.current_ref();
            if entries.last().is_some_and(|&(last, _)| last >= at) {
                self.index = Index::Unusable;
            } else {
                entries.push((at, number));
            }
        }
    }

    /// The number the index holds for `node`.
    fn indexed(&self, node: &Nav<'t>) -> Option<u64> {
        let Index::On(entries) = &self.index else {
            return None;
        };
        let at = node.current_ref();
        entries
            .binary_search_by_key(&at, |&(position, _)| position)
            .ok()
            .map(|found| entries[found].1)
    }
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
                if memo.stability.is_none() {
                    memo.stability = Some(self.numbering_stability(*count, *from));
                }
                let numbers = if self.memo_applies(&mut memo, frame) {
                    self.place_marker(&node, *level, &numbering, &mut memo.walks)?
                } else {
                    self.place_marker(&node, *level, &numbering, &mut Walks::default())?
                };
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
        walks: &mut Walks<'t>,
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
                        return self.sibling_numbers(numbering, vec![current], walks);
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
                self.sibling_numbers(numbering, counted, walks)
            }
            NumberLevel::Any => self.number_any(numbering, node, walks),
        }
    }

    /// `1 + count($af/preceding-sibling::node()[matches-count(.)])` for each
    /// `$af` of `counted` — its number in the walk of its parent's children;
    /// an attribute or namespace node, and the root of a tree, have no
    /// siblings.
    fn sibling_numbers(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        counted: Vec<Nav<'t>>,
        walks: &mut Walks<'t>,
    ) -> Result<Vec<u64>, XsltError> {
        let implied = numbering.count.is_implied();
        let mut numbers = Vec::with_capacity(counted.len());
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
            let walk = walks.of(Route::Children, &parent, implied);
            numbers.push(self.walk_to(numbering, walk, &node, Route::Children)?);
        }
        Ok(numbers)
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
    /// namespace nodes are the walk of its tree up to $S ([`Route::Tree`]);
    /// an attribute or namespace $S comes right after its element ("the
    /// attribute nodes of an element occur after the element and before its
    /// children", XDM §2.4), so its number is the count at its element, then
    /// $S itself counted.
    fn number_any(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        node: &Nav<'t>,
        walks: &mut Walks<'t>,
    ) -> Result<Vec<u64>, XsltError> {
        let implied = numbering.count.is_implied();
        let mut root = node.clone();
        root.move_to_root();
        let total = if matches!(
            node.node_type(),
            DomNodeType::Attribute | DomNodeType::Namespace
        ) {
            let mut element = node.clone();
            // Before $S: nothing for a parentless attribute (the whole of its
            // tree), nor for the pattern an attribute or namespace node
            // implies, which matches only nodes of its own kind and so none
            // before it; otherwise the count at its element.
            let before = if !element.move_to_parent() || implied {
                0
            } else {
                let walk = walks.of(Route::Tree, &root, implied);
                self.walk_to(numbering, walk, &element, Route::Tree)?
            };
            let before = if self.matches_number_from(numbering, node)? {
                0
            } else {
                before
            };
            before + u64::from(self.matches_count(numbering, node)?)
        } else {
            let walk = walks.of(Route::Tree, &root, implied);
            self.walk_to(numbering, walk, node, Route::Tree)?
        };
        Ok(if total == 0 { Vec::new() } else { vec![total] })
    }

    /// The number of `target` in `walk` ([`Walk`]): the walk goes on from its
    /// frontier to `target`. A target it has already passed is looked up in
    /// its index — made the first time that happens, by walking again from
    /// the start and noting every node's number — so that numbering nodes in
    /// any order (reverse document order, an `xsl:sort` order) visits each
    /// node of a walk at most twice. With an unusable index, the walk starts
    /// again.
    fn walk_to(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        walk: &mut Walk<'t>,
        target: &Nav<'t>,
        route: Route,
    ) -> Result<u64, XsltError> {
        if let Some((frontier, number)) = &walk.frontier {
            match frontier.compare_position(target) {
                XmlNodeOrder::Same => return Ok(*number),
                XmlNodeOrder::Before => {}
                XmlNodeOrder::After | XmlNodeOrder::Unknown => {
                    if matches!(walk.index, Index::Off) {
                        let end = frontier.clone();
                        walk.index = Index::On(Vec::new());
                        walk.restart();
                        self.advance(numbering, walk, &end, route)?;
                    }
                    if let Some(number) = walk.indexed(target) {
                        return Ok(number);
                    }
                    if matches!(walk.index, Index::On(_)) {
                        return Err(XsltError::internal(
                            "xsl:number: a node before the frontier of its walk was not visited",
                        ));
                    }
                    walk.restart();
                }
            }
        }
        self.advance(numbering, walk, target, route)
    }

    /// Visits the nodes of `walk`'s route after its frontier (from the start
    /// if it has none) up to and including `target`; returns the number of
    /// `target`.
    fn advance(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        walk: &mut Walk<'t>,
        target: &Nav<'t>,
        route: Route,
    ) -> Result<u64, XsltError> {
        // The cursor is the frontier moving on (an error ends the
        // evaluation, and the memo with it).
        let step = |node: &mut Nav<'t>| match route {
            Route::Tree => next_in_document(node),
            Route::Children => node.move_to_next_sibling(),
        };
        let mut cursor = match walk.frontier.take() {
            None if route == Route::Tree => Some(walk.owner.clone()),
            None => {
                let mut child = walk.owner.clone();
                child.move_to_first_child().then_some(child)
            }
            Some((mut node, _)) => step(&mut node).then_some(node),
        };
        loop {
            let Some(node) = cursor.as_mut() else {
                return Err(XsltError::internal(
                    "xsl:number: a node was not found on the walk that should reach it",
                ));
            };
            let number = self.visit(numbering, walk, node, route)?;
            if node.is_same_position(target) {
                walk.frontier = cursor.map(|node| (node, number));
                return Ok(number);
            }
            if !step(node) {
                cursor = None;
            }
        }
    }

    /// One node of a walk: on `Route::Tree` the count restarts if `node`
    /// matches `from`; `node` is counted if it matches the count pattern (or,
    /// without one, under its own implied pattern). Returns its number.
    fn visit(
        &self,
        numbering: &Numbering<'_, '_, 't>,
        walk: &mut Walk<'t>,
        node: &Nav<'t>,
        route: Route,
    ) -> Result<u64, XsltError> {
        if route == Route::Tree && self.matches_number_from(numbering, node)? {
            walk.tally.restart();
        }
        let number = match &mut walk.tally {
            Tally::Pattern(count) => {
                *count += u64::from(self.matches_count(numbering, node)?);
                *count
            }
            Tally::Implied(census) => {
                #[cfg(test)]
                COUNT_TESTS.with(|tests| tests.set(tests.get() + 1));
                census.add(node)
            }
        };
        walk.visited = walk.visited.saturating_add(1);
        walk.record(node, number);
        Ok(number)
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

    /// Whether, and with which local variables, the counts of one evaluation
    /// of the instruction can be reused by the next ([`NumberMemo`]).
    ///
    /// A pattern is matched with nothing of the caller's dynamic context but
    /// the frame (`Transformation::matches_in` builds the predicates' context
    /// from the candidate alone), so what can differ from one evaluation to
    /// the next is what a predicate — or the argument of an `id()` / `key()`
    /// anchor — reads: a local variable, and the nodes a stylesheet function
    /// may construct anew at each call (the same reason `candidate_independent`
    /// refuses them in `runtime/patterns.rs`). Global variables are fixed for
    /// the run, the functions in the standard function namespace depend only
    /// on their arguments and on state fixed for the run, and `current()` is
    /// the node being matched (§16.6.1), so a pattern that calls no stylesheet
    /// function matches a node the same way in every evaluation whose local
    /// variables hold the same values: those slots are the memo's key. The
    /// implied count pattern has no predicates.
    fn numbering_stability(&self, count: Option<PatternId>, from: Option<PatternId>) -> Stability {
        let mut locals = Vec::new();
        for id in [count, from].into_iter().flatten() {
            for alternative in &self.sheet.pattern(id).alternatives {
                let anchor = match alternative.anchor {
                    Anchor::IdKey(anchor) => Some(anchor.call),
                    _ => None,
                };
                let exprs = alternative
                    .steps
                    .iter()
                    .flat_map(|step| step.predicates.iter().copied())
                    .chain(anchor);
                for expr in exprs {
                    let compiled = self.sheet.expr(expr);
                    if !compiled
                        .expr
                        .function_calls()
                        .iter()
                        .all(|call| call.namespace == FN_NS)
                    {
                        return Stability::Unstable;
                    }
                    for (slot, location) in compiled.vars.iter().enumerate() {
                        if let VarLocation::Local(local) = location {
                            if compiled.expr.references_external_var(slot as u32) {
                                locals.push(*local);
                            }
                        }
                    }
                }
            }
        }
        locals.sort_unstable();
        locals.dedup();
        Stability::Stable(locals)
    }

    /// Whether this evaluation may use the walks of `memo`: its patterns are
    /// stable, and the local variables they read hold values the memo can
    /// compare exactly ([`comparable`]) — when those differ from the values
    /// the walks were made with ([`same_value`]), the walks are dropped and
    /// the new values kept. An absent slot reads as the empty sequence, as
    /// `Transformation::eval_expr` binds it. The comparison costs one look at
    /// each item of the variables, which every match of the pattern pays
    /// anyway (`eval_expr` binds a copy of each value it reads).
    fn memo_applies(&self, memo: &mut NumberMemo<'t>, frame: &Frame<'t>) -> bool {
        let NumberMemo {
            stability,
            locals: kept,
            walks,
        } = memo;
        let Some(Stability::Stable(slots)) = stability else {
            return false;
        };
        let empty = XPathValue::empty();
        let values: Vec<&XPathValue<Nav<'t>>> = slots
            .iter()
            .map(|&slot| frame.get(slot).unwrap_or(&empty))
            .collect();
        if !values
            .iter()
            .all(|value| items(value).iter().all(comparable))
        {
            *walks = Walks::default();
            kept.clear();
            return false;
        }
        let same = kept.len() == values.len()
            && kept
                .iter()
                .zip(&values)
                .all(|(kept, value)| same_value(kept, value));
        if !same {
            *walks = Walks::default();
            *kept = values.into_iter().cloned().collect();
        }
        true
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
    /// How many nodes have been tested on this thread — by `matches_count`,
    /// and by a walk's census without `count` — so the work the walks of the
    /// place marker did (the guard of their cost,
    /// `tests::numbering_many_names_in_document_order_is_linear` and its
    /// neighbours).
    static COUNT_TESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// How many patterns a census has compared with the one of a node it
    /// counts — one per entry of its list scanned, one per lookup in its
    /// hash map (`tests::the_census_is_linear_over_many_namespaces`).
    static CENSUS_PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

/// The items of `value`.
fn items<'v, 't>(value: &'v XPathValue<Nav<'t>>) -> &'v [XmlItem<Nav<'t>>] {
    match value {
        XPathValue::Empty => &[],
        XPathValue::Item(item) => std::slice::from_ref(item),
        XPathValue::Sequence(items) => items,
    }
}

/// Whether [`same_value`] can tell `item` from every item that a pattern
/// could tell it from: a node (by identity), or an atomic value of a type
/// compared here exactly — a string, `xs:anyURI`, untyped atomic, boolean,
/// integer, double or float.
fn comparable(item: &XmlItem<Nav<'_>>) -> bool {
    match item {
        XmlItem::Node(_) => true,
        XmlItem::Atomic(atom) => match &atom.value {
            XmlValueKind::UntypedAtomic(_) => true,
            XmlValueKind::Atomic(value) => matches!(
                value,
                XmlAtomicValue::String(_)
                    | XmlAtomicValue::AnyUri(_)
                    | XmlAtomicValue::Boolean(_)
                    | XmlAtomicValue::Integer(_)
                    | XmlAtomicValue::Double(_)
                    | XmlAtomicValue::Float(_)
            ),
            _ => false,
        },
    }
}

/// Whether two values of a local variable are the same for any pattern: the
/// same nodes (identity) and the same atomic values — the same type
/// annotation and the same value, a double or float by its bits (so `-0` is
/// not `0`). Only [`comparable`] items are ever equal.
fn same_value(a: &XPathValue<Nav<'_>>, b: &XPathValue<Nav<'_>>) -> bool {
    let (a, b) = (items(a), items(b));
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (XmlItem::Node(a), XmlItem::Node(b)) => a.is_same_position(b),
            (XmlItem::Atomic(a), XmlItem::Atomic(b)) => {
                a.type_code == b.type_code
                    && a.schema_type == b.schema_type
                    && match (&a.value, &b.value) {
                        (XmlValueKind::UntypedAtomic(a), XmlValueKind::UntypedAtomic(b)) => a == b,
                        (XmlValueKind::Atomic(a), XmlValueKind::Atomic(b)) => match (a, b) {
                            (XmlAtomicValue::String(a), XmlAtomicValue::String(b))
                            | (XmlAtomicValue::AnyUri(a), XmlAtomicValue::AnyUri(b)) => a == b,
                            (XmlAtomicValue::Boolean(a), XmlAtomicValue::Boolean(b)) => a == b,
                            (XmlAtomicValue::Integer(a), XmlAtomicValue::Integer(b)) => a == b,
                            (XmlAtomicValue::Double(a), XmlAtomicValue::Double(b)) => {
                                a.to_bits() == b.to_bits()
                            }
                            (XmlAtomicValue::Float(a), XmlAtomicValue::Float(b)) => {
                                a.to_bits() == b.to_bits()
                            }
                            _ => false,
                        },
                        _ => false,
                    }
            }
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use bumpalo::Bump;
    use xsd_schema::namespace::NameTable;

    use crate::{Compiler, TransformOptions, XsltFunctions};

    /// Numbers `select` with `<xsl:number {attributes}/>` over `<doc>` +
    /// `elements` children named `n0`, `n1`, … `n{names - 1}` in turn, each
    /// with a text child. Returns the result and how many nodes the walks
    /// tested.
    fn number_cycling_names(
        select: &str,
        attributes: &str,
        names: usize,
        elements: usize,
    ) -> (String, u64) {
        number_cycling_names_in(select, "", attributes, names, elements)
    }

    /// [`number_cycling_names`] in the order of the `xsl:sort` elements
    /// `sort` (none: in the order of `select`).
    fn number_cycling_names_in(
        select: &str,
        sort: &str,
        attributes: &str,
        names: usize,
        elements: usize,
    ) -> (String, u64) {
        let sheet = format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:output method="text"/>
                 <xsl:template match="/"><xsl:for-each select="{select}">{sort}<xsl:number {attributes}/>,</xsl:for-each></xsl:template>
               </xsl:stylesheet>"#
        );
        let mut source = String::from("<doc>");
        for i in 0..elements {
            source.push_str(&format!("<n{0}>t</n{0}>", i % names));
        }
        source.push_str("</doc>");
        run_counting(&sheet, &source)
    }

    /// What `<xsl:number level="{level}"/>` gives the element at 1-based
    /// position `p` of `//*` in [`number_cycling_names`]' document (`doc` is
    /// the first), with `count="*"` if `star`. Without `count` every level
    /// gives an element its rank among the elements of its name (the
    /// document is flat, and `doc`'s name is its own); with `count="*"`,
    /// `single` gives its position among its siblings, `multiple` that after
    /// `doc`'s, `any` its position among all elements.
    fn cycling_number(p: usize, names: usize, level: &str, star: bool) -> String {
        match (p, level, star) {
            (1, _, _) => "1".to_string(),
            (_, "any", true) => p.to_string(),
            (_, "multiple", true) => format!("1.{}", p - 1),
            (_, _, true) => (p - 1).to_string(),
            _ => ((p - 2) / names + 1).to_string(),
        }
    }

    /// Runs `sheet` over `source`; returns the result and how many nodes the
    /// walks of the place marker tested.
    fn run_counting(sheet: &str, source: &str) -> (String, u64) {
        run_measuring(sheet, source, &COUNT_TESTS)
    }

    /// Runs `sheet` over `source`; returns the result and how much `counter`
    /// went up.
    fn run_measuring(
        sheet: &str,
        source: &str,
        counter: &'static std::thread::LocalKey<std::cell::Cell<u64>>,
    ) -> (String, u64) {
        let functions = XsltFunctions::new();
        let table = NameTable::new();
        let sheet = Compiler::new(&table, &functions)
            .compile_str(sheet, "file:///number/main.xsl")
            .expect("the stylesheet compiles");
        let arena = Bump::new();
        let run = sheet.transformation(&arena, TransformOptions::default());
        let before = counter.with(|count| count.get());
        let outcome = run
            .transform_source("file:///number/in.xml", source)
            .expect("the run succeeds");
        let measured = counter.with(|count| count.get()) - before;
        let text = sheet.serialize_primary(&outcome).expect("serializes");
        (text, measured)
    }

    /// Review F1 and the follow-up: without `count` one walk counts every
    /// implied pattern at once (`Tally::Implied`), so numbering the elements
    /// of a document in document order tests the same number of nodes
    /// however many names are interleaved — 1, 40 or 2,000, cycled (the order
    /// that defeats any table of fewer names). Before the follow-up each
    /// evaluation walked back to the previous element of its own name (about
    /// `elements × names` tests: 8 million here for 2,000 names); with its
    /// marks kept for only 16 names (the M3c version), about `elements²`.
    #[test]
    fn numbering_many_names_in_document_order_is_linear() {
        const ELEMENTS: usize = 4_000;
        // `doc`, the elements, their text children (the document node is not
        // an element, and `//*` does not number it).
        let tree = 1 + 2 * ELEMENTS as u64;
        for level in ["any", "single", "multiple"] {
            let attributes = format!(r#"level="{level}""#);
            let mut costs = Vec::new();
            for names in [1, 40, 2_000] {
                let (text, tested) = number_cycling_names("//*", &attributes, names, ELEMENTS);
                let expected: String = (1..=ELEMENTS + 1)
                    .map(|p| format!("{},", cycling_number(p, names, level, false)))
                    .collect();
                assert_eq!(text, expected, "level={level} names={names}");
                assert!(
                    tested <= 3 * tree,
                    "level={level} names={names}: {tested} nodes tested for a tree of {tree} \
                     (at most {})",
                    3 * tree
                );
                costs.push(tested);
            }
            let (least, most) = (costs.iter().min(), costs.iter().max());
            assert_eq!(
                least, most,
                "level={level}: {costs:?} for 1, 40, 2000 names"
            );
        }
    }

    /// Follow-up items 2 and 3: nodes numbered out of document order — in
    /// reverse, or in the order of an `xsl:sort` — are looked up in their
    /// walk's index (`Index`), made when the first node before the walk's
    /// frontier is asked for, so each node of a walk is visited at most
    /// twice; every evaluation used to walk again from the start (about
    /// `elements² / 2` tests: 8 million here).
    #[test]
    fn numbering_out_of_document_order_is_linear() {
        const ELEMENTS: usize = 4_000;
        const NAMES: usize = 7;
        let tree = 1 + 2 * ELEMENTS as u64;
        let positions: Vec<usize> = (1..=ELEMENTS + 1).collect();
        let reversed: Vec<usize> = positions.iter().rev().copied().collect();
        // `xsl:sort` is stable, and these keys are distinct.
        let mut sorted = positions.clone();
        sorted.sort_by_key(|p| (p * 7919) % 10007);
        let sort = r#"<xsl:sort select="(position() * 7919) mod 10007" data-type="number"/>"#;
        for (select, sort, order) in [("reverse(//*)", "", &reversed), ("//*", sort, &sorted)] {
            for level in ["any", "single", "multiple"] {
                for count in ["", r#"count="*""#] {
                    let attributes = format!(r#"level="{level}" {count}"#);
                    let (text, tested) =
                        number_cycling_names_in(select, sort, &attributes, NAMES, ELEMENTS);
                    let star = !count.is_empty();
                    let expected: String = order
                        .iter()
                        .map(|&p| format!("{},", cycling_number(p, NAMES, level, star)))
                        .collect();
                    assert_eq!(text, expected, "{select} {sort} {attributes}");
                    assert!(
                        tested <= 4 * tree,
                        "{select} {sort} {attributes}: {tested} nodes tested for a tree of \
                         {tree} (at most {})",
                        4 * tree
                    );
                }
            }
        }
    }

    /// Follow-up item 4: a `count` / `from` pattern that reads a local
    /// variable keeps the memo while the variable keeps its value
    /// (`NumberMemo::locals`); every evaluation used to count afresh (about
    /// `elements² / 2` tests). A variable whose value changes from one
    /// evaluation to the next, and a stylesheet function, still count afresh
    /// — and give the numbers of a fresh evaluation.
    #[test]
    fn a_pattern_reading_a_local_variable_keeps_the_memo() {
        let numbered = |elements: usize, variable: &str, count: &str, level: &str| {
            let sheet = format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:f="urn:f">
                     <xsl:output method="text"/>
                     <xsl:function name="f:yes"><xsl:sequence select="true()"/></xsl:function>
                     <xsl:template match="/"><xsl:variable name="yes" select="true()"/><xsl:for-each select="//*">{variable}<xsl:number level="{level}" count="{count}"/>,</xsl:for-each></xsl:template>
                   </xsl:stylesheet>"#
            );
            let mut source = String::from("<doc>");
            for i in 0..elements {
                source.push_str(&format!(r#"<e k="{}">t</e>"#, i % 3));
            }
            source.push_str("</doc>");
            run_counting(&sheet, &source)
        };
        const ELEMENTS: usize = 4_000;
        let tree = 1 + 2 * ELEMENTS as u64;
        for level in ["any", "single", "multiple"] {
            let (text, tested) = numbered(ELEMENTS, "", "*[$yes]", level);
            let expected: String = (1..=ELEMENTS + 1)
                .map(|p| format!("{},", cycling_number(p, 1, level, true)))
                .collect();
            assert_eq!(text, expected, "level={level}");
            assert!(
                tested <= 4 * tree,
                "level={level}: {tested} nodes tested for a tree of {tree} (at most {})",
                4 * tree
            );
        }
        // `$k` changes at every element: the walks start again each time.
        let (text, _) = numbered(
            30,
            r#"<xsl:variable name="k" select="string(@k)"/>"#,
            "e[@k = $k]",
            "any",
        );
        let expected: String = std::iter::once(String::new())
            .chain((0..30).map(|i| (i / 3 + 1).to_string()))
            .map(|n| n + ",")
            .collect();
        assert_eq!(text, expected);
        // A stylesheet function: no memo, the same numbers.
        let (text, tested) = numbered(30, "", "*[f:yes()]", "single");
        let expected: String = (1..=31)
            .map(|p| format!("{},", cycling_number(p, 1, "single", true)))
            .collect();
        assert_eq!(text, expected);
        assert!(tested > 30 * 30 / 2, "{tested}: the memo was used");
    }

    /// The values a memo is keyed by are compared exactly: a double by its
    /// bits (`-0` is not `0`, a pattern can tell them apart with `string()`),
    /// the type annotation too (`1` is not `1.0e0`, nor a string its untyped
    /// twin); an atomic type compared only by a lossy equality (decimal,
    /// dates, QNames …) is not comparable at all.
    #[test]
    fn the_memo_key_compares_values_exactly() {
        use xsd_schema::types::{XmlTypeCode, XmlValue};
        let atom = |code: XmlTypeCode, kind: XmlValueKind| {
            XPathValue::<Nav<'static>>::from_atomic(XmlValue::new(code, kind))
        };
        let double = |d: f64| {
            atom(
                XmlTypeCode::Double,
                XmlValueKind::Atomic(XmlAtomicValue::Double(d)),
            )
        };
        let string = |s: &str| {
            atom(
                XmlTypeCode::String,
                XmlValueKind::Atomic(XmlAtomicValue::String(s.into())),
            )
        };
        let untyped = |s: &str| {
            atom(
                XmlTypeCode::UntypedAtomic,
                XmlValueKind::UntypedAtomic(s.into()),
            )
        };
        let integer = |i: i64| {
            atom(
                XmlTypeCode::Integer,
                XmlValueKind::Atomic(XmlAtomicValue::Integer(i.into())),
            )
        };
        assert!(same_value(&double(0.0), &double(0.0)));
        assert!(!same_value(&double(0.0), &double(-0.0)));
        assert!(same_value(&double(f64::NAN), &double(f64::NAN)));
        assert!(!same_value(&double(1.0), &integer(1)));
        assert!(same_value(&integer(7), &integer(7)));
        assert!(!same_value(&integer(7), &integer(8)));
        assert!(same_value(&string("a"), &string("a")));
        assert!(!same_value(&string("a"), &untyped("a")));
        assert!(same_value(&untyped("a"), &untyped("a")));
        assert!(same_value(&XPathValue::empty(), &XPathValue::empty()));
        assert!(!same_value(&XPathValue::empty(), &string("")));
        let two = XPathValue::from_sequence(vec![
            XmlItem::Atomic(XmlValue::new(
                XmlTypeCode::String,
                XmlValueKind::Atomic(XmlAtomicValue::String("a".into())),
            )),
            XmlItem::Atomic(XmlValue::new(
                XmlTypeCode::Boolean,
                XmlValueKind::Atomic(XmlAtomicValue::Boolean(true)),
            )),
        ]);
        assert!(same_value(&two, &two.clone()));
        assert!(!same_value(&two, &string("a")));
        for value in [
            &double(-0.0),
            &string("x"),
            &untyped("x"),
            &integer(1),
            &two,
        ] {
            assert!(items(value).iter().all(comparable));
        }
        let decimal = XmlItem::Atomic(XmlValue::new(
            XmlTypeCode::Decimal,
            XmlValueKind::Atomic(XmlAtomicValue::Decimal(Default::default())),
        ));
        assert!(!comparable(&decimal));
        assert!(!same_value(
            &XPathValue::from_item(decimal.clone()),
            &XPathValue::from_item(decimal)
        ));
    }

    /// Review F5: the `level="any"` walks are kept per tree, for several
    /// trees: one instruction numbering the elements of a document and of a
    /// copy of it in turn continues each tree's walk where it stopped, where
    /// one walk for all trees started again from the root at every switch
    /// (about `elements²` tests: 4.7 s for two trees of 10,000).
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

    /// The sibling walks are kept per parent, for several parents: numbering
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

    /// `<doc>` with `sections` `sec` children of `items` `item` children
    /// each, `<item p="{position}">t</item>`, numbered with `<xsl:number
    /// {attributes}/>` in the order of `@p` — item 1 of every section, then
    /// item 2 … (an index sorted by key, each entry numbered within its
    /// section); returns the result, how many nodes the walks tested, and
    /// the size of the tree (`doc`, the sections, the items, their text).
    fn number_sections_in_turn(
        sections: usize,
        items: usize,
        attributes: &str,
    ) -> (String, u64, u64) {
        let sheet = format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:output method="text"/>
                 <xsl:template match="/"><xsl:for-each select="//item"><xsl:sort select="@p" data-type="number"/><xsl:number {attributes}/>,</xsl:for-each></xsl:template>
               </xsl:stylesheet>"#
        );
        let mut source = String::from("<doc>");
        for _ in 0..sections {
            source.push_str("<sec>");
            for p in 0..items {
                source.push_str(&format!(r#"<item p="{p}">t</item>"#));
            }
            source.push_str("</sec>");
        }
        source.push_str("</doc>");
        let (text, tested) = run_counting(&sheet, &source);
        (text, tested, (1 + sections + 2 * sections * items) as u64)
    }

    /// The numbers [`number_sections_in_turn`] should give: `p` for item `p`
    /// of every section, or `s.p` with `level="multiple" count="sec|item"`.
    fn sections_in_turn(sections: usize, items: usize, multiple: bool) -> String {
        (1..=items)
            .flat_map(|p| {
                (1..=sections).map(move |s| {
                    if multiple {
                        format!("{s}.{p},")
                    } else {
                        format!("{p},")
                    }
                })
            })
            .collect()
    }

    /// Fixes review F1 (M3c follow-up review): the walks are found by their
    /// owner (`Walks`) and every walk past `SHORT_WALK` nodes is kept,
    /// however many parents are interleaved. With the 32 parents kept before,
    /// 33 sections numbered in turn walked each section from its first child
    /// at every evaluation (`sections × items² / 2` tests: 165,000 here at
    /// 33 sections, 500,000 at 100).
    #[test]
    fn numbering_many_parents_in_turn_is_linear() {
        const ITEMS: usize = 100;
        for sections in [33, 100] {
            for (attributes, multiple) in [
                ("", false),
                (r#"count="item""#, false),
                (r#"level="multiple" count="sec|item""#, true),
            ] {
                let (text, tested, tree) = number_sections_in_turn(sections, ITEMS, attributes);
                assert_eq!(
                    text,
                    sections_in_turn(sections, ITEMS, multiple),
                    "{sections} sections, {attributes}"
                );
                assert!(
                    tested <= 4 * tree,
                    "{sections} sections, {attributes}: {tested} nodes tested for a tree of \
                     {tree} (at most {})",
                    4 * tree
                );
            }
        }
    }

    /// Fixes review F1: the `level="any"` walks are kept for every tree — 33
    /// and 50 documents numbered in turn (the `i`-th `e` of each, then the
    /// `i + 1`-th …). With the 32 trees kept before, 33 walked each tree from
    /// its root at every evaluation (`trees × elements² / 2` tests: 165,000
    /// here at 33 trees).
    #[test]
    fn numbering_many_trees_in_turn_is_linear() {
        const ELEMENTS: usize = 100;
        let mut source = String::from("<doc>");
        for i in 0..ELEMENTS {
            source.push_str(&format!(r#"<e i="{i}"/>"#));
        }
        source.push_str("</doc>");
        for trees in [33, 50] {
            for count in [r#"count="e""#, ""] {
                let sheet = format!(
                    r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:f="urn:f">
                         <xsl:output method="text"/>
                         <xsl:function name="f:copy"><xsl:param name="d"/><xsl:document><xsl:copy-of select="$d"/></xsl:document></xsl:function>
                         <xsl:template match="/">
                           <xsl:variable name="trees" select="for $t in 1 to {trees} return f:copy(/)"/>
                           <xsl:for-each select="for $t in $trees return $t//e"><xsl:sort select="@i" data-type="number"/><xsl:number level="any" {count}/>,</xsl:for-each>
                         </xsl:template>
                       </xsl:stylesheet>"#
                );
                let (text, tested) = run_counting(&sheet, &source);
                // The sort is stable: the `i`-th element of every tree in
                // turn, each the `i + 1`-th `e` of its tree.
                let expected: String = (1..=ELEMENTS)
                    .flat_map(|n| std::iter::repeat_n(format!("{n},"), trees))
                    .collect();
                assert_eq!(text, expected, "{trees} trees, {count}");
                // A document node, `doc` and the elements per tree.
                let nodes = (trees * (2 + ELEMENTS)) as u64;
                assert!(
                    tested <= 4 * nodes,
                    "{trees} trees, {count}: {tested} nodes tested for trees of {nodes} nodes \
                     (at most {})",
                    4 * nodes
                );
            }
        }
    }

    /// More parents than `KEPT_WALKS` numbered in turn: the walks of at most
    /// `SHORT_WALK` nodes are dropped together each time the walks reach
    /// their limit, and walked again when needed (at most `SHORT_WALK` nodes
    /// each time); a walk past `SHORT_WALK` nodes is kept from then on. So the
    /// numbers stay right, the first `SHORT_WALK` rounds cost at most
    /// `sections × SHORT_WALK² / 2` tests and the rest is linear — where
    /// dropping every walk (or keeping 32 parents, as before the fix) costs
    /// `sections × items² / 2` (2.3 million here).
    #[test]
    fn numbering_more_parents_than_kept_walks_in_turn() {
        let sections = KEPT_WALKS + 76;
        let items = 2 * SHORT_WALK as usize;
        let (text, tested, tree) = number_sections_in_turn(sections, items, "");
        assert_eq!(text, sections_in_turn(sections, items, false));
        let short = SHORT_WALK as u64;
        let bound = 4 * tree + sections as u64 * short * (short + 1) / 2;
        assert!(
            tested <= bound,
            "{tested} nodes tested for a tree of {tree} (at most {bound})"
        );
    }

    /// Fixes review F3: a census finds the pattern of a node by its kind and
    /// expanded name in one lookup, however many namespaces share one local
    /// name — 2,000 `x` elements over 1,000 namespaces, each twice. It used to
    /// scan every namespace met under the local name (about `elements ×
    /// namespaces / 2` comparisons: a million here).
    #[test]
    fn the_census_is_linear_over_many_namespaces() {
        const NAMESPACES: usize = 1_000;
        let mut source = String::from("<doc>");
        for i in 0..2 * NAMESPACES {
            source.push_str(&format!(r#"<x xmlns="urn:{}">t</x>"#, i % NAMESPACES));
        }
        source.push_str("</doc>");
        // `doc`, the elements and their text.
        let tree = 1 + 4 * NAMESPACES as u64;
        for level in ["any", "single", "multiple"] {
            let sheet = format!(
                r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                     <xsl:output method="text"/>
                     <xsl:template match="/"><xsl:for-each select="//*:x"><xsl:number level="{level}"/>,</xsl:for-each></xsl:template>
                   </xsl:stylesheet>"#
            );
            let (text, probes) = run_measuring(&sheet, &source, &CENSUS_PROBES);
            let expected: String = (0..2 * NAMESPACES)
                .map(|i| if i < NAMESPACES { "1," } else { "2," })
                .collect();
            assert_eq!(text, expected, "level={level}");
            assert!(
                probes <= 2 * tree,
                "level={level}: {probes} census probes for a tree of {tree} (at most {})",
                2 * tree
            );
        }
    }

    /// The census keys tell apart what the implied pattern does (§12.2: "the
    /// same node kind … and … the same expanded-QName"): `{urn:x}bc` and
    /// `{urn:xb}c` (one string once joined), an element and a processing
    /// instruction of one name, an element in no namespace.
    #[test]
    fn the_census_tells_kinds_and_expanded_names_apart() {
        let sheet = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
                 <xsl:output method="text"/>
                 <xsl:template match="/"><xsl:for-each select="/*/node()"><xsl:number level="any"/>,<xsl:number/>,</xsl:for-each></xsl:template>
               </xsl:stylesheet>"#;
        let source = r#"<doc><a:bc xmlns:a="urn:x"/><b:c xmlns:b="urn:xb"/><a:bc xmlns:a="urn:x"/><?bc?><bc/><b:c xmlns:b="urn:xb"/><?bc?></doc>"#;
        let (text, _) = run_counting(sheet, source);
        assert_eq!(text, "1,1,1,1,2,2,1,1,1,1,2,2,2,2,");
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
