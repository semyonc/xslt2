//! Keys at run time (§16.3, DESIGN §9.7): the per-document index, built the
//! first time `key()` asks for it, and the lookup `key()` performs.
//!
//! An index belongs to one key **and one document**. §16.3.1: "An `xsl:key`
//! declaration is not bound to a specific source document. The source
//! document to which it applies is determined only when the `key` function is
//! used to locate nodes using the key. Keys can be used to locate nodes within
//! any source document (including temporary trees), but each use of the key
//! function searches one document only." So the table is keyed by the
//! [`KeyId`] and the document's serial number — the identity
//! `DocumentStore` maps to its `DocId`, which every tree has whether or not the
//! store registered it.
//!
//! An index of a key whose specifier may observe the current group
//! (`Key::observes_group`) also belongs to **one current group**: the
//! specifier is evaluated with the current group and grouping key of the
//! `key()` call (§16.3.1 changes only the focus: "The expression will be
//! evaluated with the node that matches the pattern as the context node";
//! §5.4.4 clears the current group for "calls on stylesheet functions" and
//! nothing else), so the same key over the same document has different values
//! in two groups. Such indexes are kept per group, under the group's serial
//! number ([`KeyIndexes`]), and dropped when the group's evaluation ends. Every
//! other key is evaluated with no current group — it cannot tell — and keeps
//! one index per document for the run.
//!
//! Building an index evaluates the key's patterns and key specifiers, which
//! may call `key()` again. A call for a *different* (key, document) pair — or,
//! for a key that observes the group, (key, document, group) — just builds
//! that index first; a call for the one being built is a circularity
//! (§9.8: "A circularity is said to exist if a construct such as a global
//! variable, an attribute set, or a key is defined in terms of itself"), which
//! the [`KeyState::Building`] marker detects. The table is never borrowed
//! across an evaluation. Every build is entered through the stack guard
//! (`TransformOptions::stack_limit`), because a chain of builds nests on the
//! host stack with nothing else in between to check it.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use xsd_schema::ids::NameId;
use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::types::{
    DateTimeValue, DateValue, TimeValue, TimezoneOffset, XmlAtomicValue, XmlTypeCode, XmlValue,
    XmlValueKind,
};
use xsd_schema::xpath::ast::BinaryOpKind;
use xsd_schema::xpath::functions::{atomize_sequence, XPathValue};
use xsd_schema::xpath::operators::{eval_binary, value_eq};
use xsd_schema::xpath::XmlItem;

use super::context::{CurrentGroup, XsltContext};
use super::{Frame, Transformation};
use crate::collation::{Collation, CollationResolver, CODEPOINT_COLLATION_URI};
use crate::error::{codes, XsltError};
use crate::model::{Key, KeyDefinition, KeyId, ValueSource};
use crate::receiver::{Nav, SequenceCollector};

/// The index of one key over one document, the marker that it is being built,
/// or the error its build failed with.
pub(super) enum KeyState<'t> {
    Building,
    Built(Rc<KeyIndex<'t>>),
    /// The build failed with this error, which every later use of the index
    /// raises again, for as long as the index is kept, instead of building it
    /// again (see [`Transformation::key_index`]).
    Failed(XsltError),
}

/// Where one index is kept: the key, the serial number of the document, and
/// the serial number of the current group it was built for
/// (`CurrentGroup::serial`) — 0 for a call made outside any group, and for
/// every key that cannot observe the group.
#[derive(Clone, Copy)]
struct IndexSlot {
    key: KeyId,
    document: u64,
    group: u64,
}

/// Every index of a run.
///
/// The indexes built with no current group live for the run. Those built for
/// one group are kept in a table of that group's own, which
/// [`forget_group`](Self::forget_group) drops when the group's evaluation is
/// over: the group can never be current again, and without that a
/// group-dependent key used in each of many groups would keep an index per
/// group to the end of the run. At any moment only the groups being evaluated
/// — the nested `xsl:for-each-group` bodies in progress, and the groups whose
/// sort keys have been computed but whose body has not run yet — hold any.
#[derive(Default)]
pub(super) struct KeyIndexes<'t> {
    run: HashMap<(KeyId, u64), KeyState<'t>>,
    groups: HashMap<u64, HashMap<(KeyId, u64), KeyState<'t>>>,
}

impl<'t> KeyIndexes<'t> {
    fn get(&self, slot: IndexSlot) -> Option<&KeyState<'t>> {
        let table = if slot.group == 0 {
            Some(&self.run)
        } else {
            self.groups.get(&slot.group)
        };
        table.and_then(|table| table.get(&(slot.key, slot.document)))
    }

    fn insert(&mut self, slot: IndexSlot, state: KeyState<'t>) {
        let table = if slot.group == 0 {
            &mut self.run
        } else {
            self.groups.entry(slot.group).or_default()
        };
        table.insert((slot.key, slot.document), state);
    }

    fn remove(&mut self, slot: IndexSlot) {
        if slot.group == 0 {
            self.run.remove(&(slot.key, slot.document));
        } else if let Some(table) = self.groups.get_mut(&slot.group) {
            table.remove(&(slot.key, slot.document));
        }
    }

    /// Drops every index built for the group with this serial number.
    pub(super) fn forget_group(&mut self, serial: u64) {
        // The usual case — no key of the stylesheet observes the group — costs
        // no hashing per group.
        if !self.groups.is_empty() {
            self.groups.remove(&serial);
        }
    }
}

/// One key's index over one document.
///
/// `nodes` holds every node that some declaration matched and gave at least
/// one value, in document order (the order the document is walked in), so a
/// set of positions sorted ascending is a node sequence in document order with
/// no duplicates — §16.3.2's "The sequence returned by the key function will be
/// in document order, with duplicates (that is, nodes having the same identity)
/// removed."
///
/// `buckets` maps a hash [`Slot`] to the values in it and the position of the
/// node each came from. A slot only narrows the candidates: every candidate is
/// confirmed by the real comparison ([`key_values_equal`]), so the one
/// obligation on [`placement_of`] is that two values that are equal can be
/// found from each other (see [`Placement`]). The values whose slot is
/// [`Slot::Unhashed`] are kept apart in `unhashed`, so that the lookup of every
/// other value pays nothing for them when there are none.
#[derive(Default)]
pub(super) struct KeyIndex<'t> {
    nodes: Vec<Nav<'t>>,
    buckets: HashMap<Slot, Vec<(XmlValue, u32)>>,
    unhashed: Vec<(XmlValue, u32)>,
}

/// The hash class of a key value.
///
/// The one obligation on a slot is that two values that are equal under the
/// key's comparison (§16.3.2: `eq`, strings under the key's collation) can be
/// found from each other: either they share their home slot, or one of them is
/// in the other's [`Placement::also`] slot, or one of them is
/// [`Slot::Unhashed`] (see [`Placement`]). A slot never decides equality — every
/// candidate is confirmed by [`key_values_equal`] — so a collision only costs a
/// comparison.
#[derive(Hash, PartialEq, Eq)]
pub(super) enum Slot {
    /// A string-like value (`xs:string` and its derivatives, `xs:anyURI`,
    /// `xs:untypedAtomic`): its codepoints, or the collation's sort key, whose
    /// "equality and order agree with `compare`".
    String(Vec<u8>),
    /// A string-like value under a collation that cannot produce sort keys:
    /// one bucket, compared pairwise — so every lookup of a string in such a
    /// key is linear in the key's string values (documented for hosts on
    /// `XsltFunctions::register_collation`).
    AnyString,
    /// A numeric value, by the bits of its `xs:float` rounding. Equal numbers
    /// of any two numeric types round to the same `xs:float`.
    Number(u32),
    Boolean(bool),
    /// A date/time value **with** a timezone: a hash of the instant `eq`
    /// compares it at, as its distance ([`Seconds`]) from the epoch
    /// `1970-01-01T00:00:00Z` (or the date / time of that form) — see
    /// [`temporal_placement`].
    Zoned(Temporal, u64),
    /// A date/time value **without** a timezone: a hash of its local reading,
    /// as its distance from the timezone-less epoch, which does not depend on
    /// the implicit timezone — see [`temporal_placement`].
    Local(Temporal, u64),
    /// `xs:duration`, `xs:yearMonthDuration`, `xs:dayTimeDuration`: a hash of
    /// the (months, [`Seconds`]) pair `eq` compares any two durations by.
    Duration(u64),
    /// `xs:QName` or `xs:NOTATION` (with the type code: `eq` is not defined
    /// between the two): the namespace URI and local name, which is all `eq`
    /// compares — the prefix is not part of the value.
    Name(XmlTypeCode, Option<NameId>, NameId),
    /// `xs:hexBinary` or `xs:base64Binary` (with the type code: `eq` is not
    /// defined between the two): the octets.
    Binary(XmlTypeCode, Vec<u8>),
    /// A type `eq` compares as two values of the same type code with the same
    /// components (the five `xs:g*` types): the type code and a hash of the
    /// components. (Upstream `eq` compares these structurally, timezone
    /// included, without the implicit timezone; should it ever normalize
    /// them, this slot must hash the normalized form instead.)
    Components(XmlTypeCode, u64),
    /// A value for which none of the above could be computed (a list value, a
    /// failed conversion): compared with every sought value — see
    /// [`Placement`].
    Unhashed,
}

/// The three date/time families `eq` compares within (`xs:dateTime` and
/// `xs:dateTimeStamp` are one family).
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum Temporal {
    DateTime,
    Date,
    Time,
}

/// A signed number of seconds, split so that two equal amounts always give
/// equal values: the sign (never set for zero), the whole seconds, and a hash
/// of the fractional part (whose `Hash` normalizes trailing zeros). Slots keep
/// only a hash of it, which keeps a slot as small as a string slot.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Seconds {
    negative: bool,
    whole: i128,
    fraction: u64,
}

/// Where a key value is stored in an index, and where the values equal to it
/// may be.
///
/// A value is stored in its `home` slot. A sought value is looked for in its
/// `home` slot, in its `also` slot, and among the [`Slot::Unhashed`] values; a
/// sought value whose own home is `Unhashed` is compared with every value of
/// the index. That is what keeps a value no hash could be computed for
/// findable.
pub(super) struct Placement {
    pub(super) home: Slot,
    /// Only for a date/time value: the slot of the values on the other side of
    /// the timezone line (a value with a timezone can equal one without).
    pub(super) also: Option<Slot>,
}

impl Placement {
    fn home(home: Slot) -> Self {
        Placement { home, also: None }
    }
}

/// The collation a key — or a grouping instruction (`runtime/grouping.rs`) —
/// compares strings with.
pub(super) enum KeyCollation {
    Codepoint,
    Other(Rc<dyn Collation>),
}

impl KeyCollation {
    fn equals(&self, a: &str, b: &str) -> bool {
        match self {
            KeyCollation::Codepoint => a == b,
            KeyCollation::Other(collation) => collation.equals(a, b),
        }
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// The nodes `key()` returns (§16.3.2): those of the document whose root is
    /// `root` that are matched by a declaration of `key` and have a key value
    /// equal to one of `sought`, restricted to the subtree of `top` when the
    /// call had a third argument — in document order, without duplicates.
    ///
    /// `group` is the current group of the call (`XsltContext::group`), with
    /// which the key specifier is evaluated.
    pub(super) fn key_lookup(
        &self,
        key: KeyId,
        sought: Vec<XmlValue>,
        root: &Nav<'t>,
        top: Option<&Nav<'t>>,
        group: Option<&Rc<CurrentGroup<'t>>>,
    ) -> Result<Vec<Nav<'t>>, XsltError> {
        // "If the second argument is an empty sequence, the result of the
        // function will be an empty sequence."
        if sought.is_empty() {
            return Ok(Vec::new());
        }
        let index = self.key_index(key, root, group)?;
        let definition = self.sheet.key(key);
        let collation = self.key_collation(definition)?;
        let mut hits: Vec<u32> = Vec::new();
        for value in sought {
            let value = self.key_value(value, definition.compat);
            let Some(placement) = placement_of(&value, &collation) else {
                continue;
            };
            let mut confirm = |bucket: &Vec<(XmlValue, u32)>| {
                hits.extend(
                    bucket
                        .iter()
                        .filter(|(stored, _)| key_values_equal(stored, &value, &collation))
                        .map(|(_, node)| *node),
                );
            };
            // A value no hash was computed for may equal anything.
            confirm(&index.unhashed);
            if placement.home == Slot::Unhashed {
                // No hash for the sought value: every value is a candidate.
                index.buckets.values().for_each(confirm);
                continue;
            }
            for slot in [Some(&placement.home), placement.also.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Some(bucket) = index.buckets.get(slot) {
                    confirm(bucket);
                }
            }
        }
        hits.sort_unstable();
        hits.dedup();
        let nodes = hits.into_iter().map(|i| index.nodes[i as usize].clone());
        Ok(match top {
            // "If the argument is present, the selected subtree is the set of
            // nodes that have $top as an ancestor-or-self node."
            Some(top) => nodes.filter(|n| has_ancestor_or_self(n, top)).collect(),
            None => nodes.collect(),
        })
    }

    /// The index of `key` over the document whose root is `root` — for a key
    /// that may observe the current group, the one for `group` — built on
    /// first use.
    ///
    /// A build that fails is not attempted again while its index could be
    /// asked for: its error is remembered and raised by every later use. The
    /// build sees only the nodes of the document, the global variables, the
    /// key's own declarations (DESIGN §9.1's row: singleton focus, default
    /// mode, no tunnel parameters, no current template rule) and, for a key
    /// that may observe it, the current group — all fixed for as long as the
    /// index is kept, so it would fail the same way each time — while a
    /// pattern that calls `key()` would otherwise repeat it for every
    /// candidate, §5.5.4 turning each failure into a non-match. The one
    /// exception is an `XSLT2-INTERNAL` error: the stack and recursion guards
    /// report through it, and whether they trip depends on where the build
    /// was started, so such a failure is forgotten and the next use builds
    /// again.
    fn key_index(
        &self,
        key: KeyId,
        root: &Nav<'t>,
        group: Option<&Rc<CurrentGroup<'t>>>,
    ) -> Result<Rc<KeyIndex<'t>>, XsltError> {
        // A key that cannot observe the group is built with none, so that one
        // index answers every call over the document, in a group or not.
        let group = group.filter(|_| self.sheet.key(key).observes_group);
        let slot = IndexSlot {
            key,
            document: root.document().serial(),
            group: group.map_or(0, |group| group.serial),
        };
        match self.keys.borrow().get(slot) {
            Some(KeyState::Built(index)) => return Ok(Rc::clone(index)),
            Some(KeyState::Failed(error)) => return Err(error.clone()),
            // §9.8: "[ERR XTDE0640] In general, a circularity in a stylesheet
            // is a non-recoverable dynamic error."
            Some(KeyState::Building) => {
                return Err(XsltError::new(
                    codes::XTDE0640,
                    format!(
                        "the key {} is defined in terms of itself: its index over this \
                         document is needed while it is being built",
                        self.sheet.key(key).name
                    ),
                ))
            }
            None => {}
        }
        // Q33: a build can start another one — a pattern or key specifier
        // that calls `key()` for another key, or for this key over another
        // document — with no template invocation or function call in between
        // to check the stack, and a chain of them (dependent keys, or one key
        // over a new temporary tree at each level) would otherwise overflow
        // it. Checked after the circularity test above, so that a key defined
        // in terms of itself stays XTDE0640 however deep it is found; and
        // before the marker goes in, so that the depth error leaves none
        // behind.
        self.check_stack()?;
        self.keys.borrow_mut().insert(slot, KeyState::Building);
        let built = self.build_key_index(key, root, group);
        let mut keys = self.keys.borrow_mut();
        match built {
            Ok(index) => {
                let index = Rc::new(index);
                keys.insert(slot, KeyState::Built(Rc::clone(&index)));
                Ok(index)
            }
            Err(e) if e.is_internal() => {
                keys.remove(slot);
                Err(e)
            }
            Err(e) => {
                keys.insert(slot, KeyState::Failed(e.clone()));
                Err(e)
            }
        }
    }

    /// Walks the document once and records, for every node that a declaration
    /// of `key` matches, the values of that declaration's key specifier.
    ///
    /// §16.3.2: "When the key specifier of that `xsl:key` declaration is
    /// evaluated with a singleton focus based on $N, the atomized value of the
    /// resulting sequence includes a value that compares equal to at least one
    /// item in the atomized value of the sequence supplied as $key-value".
    fn build_key_index(
        &self,
        key: KeyId,
        root: &Nav<'t>,
        group: Option<&Rc<CurrentGroup<'t>>>,
    ) -> Result<KeyIndex<'t>, XsltError> {
        let definition = self.sheet.key(key);
        let collation = self.key_collation(definition)?;
        let mut index = KeyIndex::default();
        for_each_node(root, |node| {
            let mut position: Option<u32> = None;
            for declaration in &definition.definitions {
                // §5.5.4 applies to a key's pattern like any other: an error
                // raised while matching it is recoverable (`matches`).
                if !self.matches(self.sheet.pattern(declaration.pattern), node)? {
                    continue;
                }
                for value in self.key_specifier_values(declaration, node, group)? {
                    let value = self.key_value(value, definition.compat);
                    let Some(placement) = placement_of(&value, &collation) else {
                        continue;
                    };
                    let at = *position.get_or_insert_with(|| {
                        index.nodes.push(node.clone());
                        (index.nodes.len() - 1) as u32
                    });
                    match placement.home {
                        Slot::Unhashed => index.unhashed.push((value, at)),
                        home => index.buckets.entry(home).or_default().push((value, at)),
                    }
                }
            }
            Ok(())
        })?;
        Ok(index)
    }

    /// The atomized value of a declaration's key specifier for `node`.
    ///
    /// The dynamic context is DESIGN §9.1's row for `xsl:key`: a singleton
    /// focus on the node (§16.3.1: "The expression will be evaluated with the
    /// node that matches the pattern as the context node"); `current()` the
    /// same node (§16.6.1: "For an outermost expression … the current item is
    /// always the same as the context item"); the current template rule null
    /// (§6.7: "when evaluating a sequence constructor contained in an xsl:sort
    /// or xsl:key element … the current template rule becomes null"); the
    /// current mode the default mode (§6.5: "While evaluating … the sequence
    /// constructor contained in xsl:key or xsl:sort, the current mode is set to
    /// the default mode"); no tunnel parameters; and a frame of its own, in
    /// which only the globals and the constructor's own locals are visible.
    ///
    /// The current group and grouping key are `group`'s: those of the `key()`
    /// call for a key that may observe them, none for any other (see
    /// `key_index`). §16.3.1 changes the focus only, and §5.4.4's table
    /// clears the current group and grouping key for "calls on stylesheet
    /// functions" alone — unlike the current template rule and mode, which it
    /// clears for "the sequence constructors contained in xsl:key" — so §14.1
    /// applies: "While an xsl:for-each-group instruction is being evaluated,
    /// the current group will be non-empty."
    fn key_specifier_values(
        &self,
        declaration: &KeyDefinition,
        node: &Nav<'t>,
        group: Option<&Rc<CurrentGroup<'t>>>,
    ) -> Result<Vec<XmlValue>, XsltError> {
        let item = XmlItem::Node(node.clone());
        let mut cx = XsltContext::empty();
        cx.item = Some(item.clone());
        cx.position = 1;
        cx.size = 1;
        cx.current = Some(item);
        cx.group = group.cloned();
        let mut frame = Frame::with_slots(declaration.frame_size);
        let value = match &declaration.specifier {
            ValueSource::Select(id) => self.eval_expr(*id, &frame, &cx),
            ValueSource::Content(body) => {
                let mut collector = SequenceCollector::new(&self.store);
                self.eval_body(body, &mut frame, &cx, &mut collector)
                    .map(|()| XPathValue::from_sequence(collector.into_items()))
            }
        }
        .map_err(|e| e.or_location(&declaration.loc))?;
        // "The result of evaluating the expression is atomized."
        atomize_sequence(value).map_err(|e| self.xpath_error(e).or_location(&declaration.loc))
    }

    /// A key value as the comparison sees it: unchanged, or — for a key
    /// processed in backwards compatible mode — cast to `xs:string`.
    ///
    /// §16.3.2: "The result of evaluating the key specifier in any `xsl:key`
    /// declaration having this key name is converted after atomization to a
    /// sequence of strings, by applying a cast to each item in the sequence.
    /// When the first argument to the key function specifies this key name,
    /// then the value of the second argument is converted after atomization to
    /// a sequence of strings, by applying a cast to each item in the sequence.
    /// The values are then compared as strings."
    fn key_value(&self, value: XmlValue, compat: bool) -> XmlValue {
        if !compat {
            return value;
        }
        let text = self.atom_string(&value);
        XmlValue::new(
            XmlTypeCode::String,
            XmlValueKind::Atomic(XmlAtomicValue::String(text)),
        )
    }

    /// The collation a key's strings compare under (§16.3.1: "two values $a and
    /// $b are considered equal if the result of the function call `compare($a,
    /// $b, $collation)` is zero").
    ///
    /// The compiler has already checked that the registry recognizes the URI
    /// (XTSE1210), so a failure here is a broken invariant.
    fn key_collation(&self, key: &Key) -> Result<KeyCollation, XsltError> {
        if key.collation == CODEPOINT_COLLATION_URI {
            return Ok(KeyCollation::Codepoint);
        }
        self.sheet
            .functions()
            .collations()
            .resolve(&key.collation)
            .map(KeyCollation::Other)
            .ok_or_else(|| {
                XsltError::internal(format!(
                    "the collation {:?} of the key {} was recognized at compile time and is \
                     unknown now",
                    key.collation, key.name
                ))
            })
    }
}

/// Whether `value` is compared as a string (XPath 2.0 §3.5.1: "If the atomized
/// operand is of type `xs:untypedAtomic`, it is cast to `xs:string`"; B.1: "a
/// value of type `xs:anyURI` … can be promoted to the type `xs:string`").
fn is_string_like(value: &XmlValue) -> bool {
    value.type_code.is_string_derived()
        || matches!(
            value.type_code,
            XmlTypeCode::AnyUri | XmlTypeCode::UntypedAtomic
        )
}

/// The value inside any number of union wrappers.
fn unwrapped(value: &XmlValue) -> &XmlValue {
    match &value.value {
        XmlValueKind::Union(inner) => unwrapped(inner),
        _ => value,
    }
}

/// Where a key value goes ([`Placement`]); `None` for a value that equals
/// nothing (NaN: "`NaN` does not equal itself", so `eq` is false against every
/// value).
///
/// Each arm follows the dispatch of the upstream `eq` (`compare_eq` in
/// `xsd_schema::xpath::operators`, which [`key_values_equal`] calls): a value
/// is hashed on exactly the form `eq` compares it by, and anything this
/// function cannot hash on that form is [`Slot::Unhashed`] rather than
/// guessed.
pub(super) fn placement_of(value: &XmlValue, collation: &KeyCollation) -> Option<Placement> {
    let value = unwrapped(value);
    if is_string_like(value) {
        let text = value.to_string_value();
        return Some(Placement::home(match collation {
            KeyCollation::Codepoint => Slot::String(text.into_bytes()),
            KeyCollation::Other(collation) => match collation.sort_key(&text) {
                Some(key) => Slot::String(key),
                None => Slot::AnyString,
            },
        }));
    }
    let code = value.type_code;
    if code.is_numeric() {
        let Some(number) = value.as_double() else {
            return Some(Placement::home(Slot::Unhashed));
        };
        if number.is_nan() {
            return None;
        }
        let rounded = number as f32;
        // `-0` and `+0` are equal and must share a slot.
        let rounded = if rounded == 0.0 { 0.0f32 } else { rounded };
        return Some(Placement::home(Slot::Number(rounded.to_bits())));
    }
    let XmlValueKind::Atomic(atom) = &value.value else {
        // A list value: `eq` compares two lists of one type item by item.
        return Some(Placement::home(Slot::Unhashed));
    };
    let slot = match (code, atom) {
        (XmlTypeCode::Boolean, XmlAtomicValue::Boolean(b)) => Slot::Boolean(*b),
        (XmlTypeCode::DateTime | XmlTypeCode::DateTimeStamp, XmlAtomicValue::DateTime(v)) => {
            return Some(temporal_placement(
                value,
                Temporal::DateTime,
                v.timezone.is_some(),
            ));
        }
        (XmlTypeCode::Date, XmlAtomicValue::Date(v)) => {
            return Some(temporal_placement(
                value,
                Temporal::Date,
                v.timezone.is_some(),
            ));
        }
        (XmlTypeCode::Time, XmlAtomicValue::Time(v)) => {
            return Some(temporal_placement(
                value,
                Temporal::Time,
                v.timezone.is_some(),
            ));
        }
        (
            XmlTypeCode::Duration | XmlTypeCode::YearMonthDuration | XmlTypeCode::DayTimeDuration,
            _,
        ) => match duration_parts(atom) {
            Some(parts) => Slot::Duration(hash_of(&parts)),
            None => Slot::Unhashed,
        },
        // `eq` on two QNames compares "namespace URI + local name only"; on
        // two NOTATIONs it compares the values, whose equality is the same.
        (XmlTypeCode::QName, XmlAtomicValue::QName(name))
        | (XmlTypeCode::Notation, XmlAtomicValue::Notation(name)) => {
            Slot::Name(code, name.namespace_uri, name.local_name)
        }
        (XmlTypeCode::HexBinary, XmlAtomicValue::HexBinary(octets))
        | (XmlTypeCode::Base64Binary, XmlAtomicValue::Base64Binary(octets)) => {
            Slot::Binary(code, octets.clone())
        }
        // The `xs:g*` types: `eq` compares two values of the same type code by
        // their components, timezone included.
        (XmlTypeCode::GYearMonth, XmlAtomicValue::GYearMonth(v)) => {
            Slot::Components(code, hash_of(&(v.year, v.month, v.timezone.map(|t| t.0))))
        }
        (XmlTypeCode::GYear, XmlAtomicValue::GYear(v)) => {
            Slot::Components(code, hash_of(&(v.year, v.timezone.map(|t| t.0))))
        }
        (XmlTypeCode::GMonthDay, XmlAtomicValue::GMonthDay(v)) => {
            Slot::Components(code, hash_of(&(v.month, v.day, v.timezone.map(|t| t.0))))
        }
        (XmlTypeCode::GDay, XmlAtomicValue::GDay(v)) => {
            Slot::Components(code, hash_of(&(v.day, v.timezone.map(|t| t.0))))
        }
        (XmlTypeCode::GMonth, XmlAtomicValue::GMonth(v)) => {
            Slot::Components(code, hash_of(&(v.month, v.timezone.map(|t| t.0))))
        }
        _ => Slot::Unhashed,
    };
    Some(Placement::home(slot))
}

/// The placement of an `xs:dateTime`, `xs:date` or `xs:time` value.
///
/// `eq` compares two such values (of one family) by their instants (XPath
/// 2.0 §B.2 maps it to `op:dateTime-equal`, `op:date-equal`, `op:time-equal`),
/// a value without a timezone being normalized with the implicit timezone —
/// §2.1.2: "the timezone to be used when a date, time, or dateTime value that
/// does not have a timezone is used in a comparison or arithmetic operation";
/// upstream `eq` takes the host's current offset, read again on every
/// comparison. The index must not depend on that offset, so the key is a
/// **difference computed by the upstream subtraction itself**, which
/// normalizes both operands exactly as `eq` does:
///
/// * a value with a timezone is keyed by `value − E` with `E` the epoch
///   *with* timezone `Z`: the difference of two absolute instants, the same
///   whatever the implicit timezone;
/// * a value without a timezone is keyed by `value − E′` with `E′` the epoch
///   *without* a timezone: both operands are shifted by the implicit timezone,
///   so the difference is the distance of the local readings, again the same
///   whatever the implicit timezone.
///
/// A value with a timezone equals one without when its instant equals the
/// other's local reading shifted by the implicit timezone. So the *other*
/// slot of a sought value is computed with the other epoch, at lookup time:
/// `x − E′` for a value with a timezone is its instant measured against `E′`
/// normalized with the current implicit timezone — the key a timezone-less
/// value equal to `x` has — and `x − E` for a value without one is its
/// normalized instant, the key of an equal value with a timezone. Both
/// subtractions read the implicit timezone at the moment of the lookup, as the
/// confirming `eq` does.
fn temporal_placement(value: &XmlValue, family: Temporal, zoned: bool) -> Placement {
    let with_zone = temporal_distance(value, family, true);
    let without_zone = temporal_distance(value, family, false);
    match (with_zone, without_zone) {
        (Some(with_zone), Some(without_zone)) => {
            let (zoned_slot, local_slot) = (
                Slot::Zoned(family, hash_of(&with_zone)),
                Slot::Local(family, hash_of(&without_zone)),
            );
            if zoned {
                Placement {
                    home: zoned_slot,
                    also: Some(local_slot),
                }
            } else {
                Placement {
                    home: local_slot,
                    also: Some(zoned_slot),
                }
            }
        }
        // Without both keys the value can be neither stored where an equal
        // value would look for it nor looked for where an equal value is.
        _ => Placement::home(Slot::Unhashed),
    }
}

/// `value − epoch` by the upstream subtraction (`op:subtract-dateTimes`,
/// `op:subtract-dates`, `op:subtract-times`), as [`Seconds`]; `None` if the
/// subtraction fails (an instant out of range).
fn temporal_distance(value: &XmlValue, family: Temporal, zoned_epoch: bool) -> Option<Seconds> {
    let timezone = zoned_epoch.then_some(TimezoneOffset::UTC);
    let (code, epoch) = match family {
        Temporal::DateTime => (
            XmlTypeCode::DateTime,
            XmlAtomicValue::DateTime(DateTimeValue {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: Default::default(),
                timezone,
            }),
        ),
        Temporal::Date => (
            XmlTypeCode::Date,
            XmlAtomicValue::Date(DateValue {
                year: 1970,
                month: 1,
                day: 1,
                timezone,
            }),
        ),
        Temporal::Time => (
            XmlTypeCode::Time,
            XmlAtomicValue::Time(TimeValue {
                hour: 0,
                minute: 0,
                second: Default::default(),
                timezone,
            }),
        ),
    };
    let epoch = XmlValue::new(code, XmlValueKind::Atomic(epoch));
    let difference = eval_binary(BinaryOpKind::Sub, value, &epoch).ok()?;
    match &difference.value {
        XmlValueKind::Atomic(atom @ XmlAtomicValue::DayTimeDuration(_)) => {
            duration_parts(atom).map(|(_, seconds)| seconds)
        }
        _ => None,
    }
}

/// The (months, seconds) pair upstream `eq` compares a duration by (its
/// `duration_parts`: total months, and total seconds `days × 86400 + hours ×
/// 3600 + minutes × 60 + seconds`, both negated for a negative duration);
/// `None` for a value that is not a duration or whose seconds field is
/// negative (never produced by the parser; not hashed rather than risk two
/// spellings of one amount).
fn duration_parts(atom: &XmlAtomicValue) -> Option<(i64, Seconds)> {
    let (negative, months, days, hours, minutes, seconds) = match atom {
        XmlAtomicValue::Duration(d) => (
            d.negative,
            i64::from(d.years) * 12 + i64::from(d.months),
            d.days,
            d.hours,
            d.minutes,
            d.seconds,
        ),
        XmlAtomicValue::YearMonthDuration(d) => (
            d.negative,
            i64::from(d.years) * 12 + i64::from(d.months),
            0,
            0,
            0,
            Default::default(),
        ),
        XmlAtomicValue::DayTimeDuration(d) => {
            (d.negative, 0, d.days, d.hours, d.minutes, d.seconds)
        }
        _ => return None,
    };
    if seconds.is_sign_negative() && !seconds.is_zero() {
        return None;
    }
    let fraction = seconds.fract();
    let whole = i128::from(days) * 86_400
        + i128::from(hours) * 3_600
        + i128::from(minutes) * 60
        + i128::try_from(seconds.trunc()).ok()?;
    let zero = whole == 0 && fraction.is_zero();
    Some((
        if negative { -months } else { months },
        Seconds {
            negative: negative && !zero,
            whole,
            fraction: hash_of(&fraction),
        },
    ))
}

/// A 64-bit hash of `value` (a fixed-key hasher: the same in every run).
fn hash_of<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// §16.3.2: equal "under the rules appropriate to the XPath `eq` operator for
/// the two values in question, using the collation attributes of the `xsl:key`
/// declaration when comparing strings. No error is reported if two values are
/// encountered that are not comparable; they are regarded for the purposes of
/// this function as being not equal."
pub(super) fn key_values_equal(a: &XmlValue, b: &XmlValue, collation: &KeyCollation) -> bool {
    let (a, b) = (unwrapped(a), unwrapped(b));
    if is_string_like(a) && is_string_like(b) {
        return collation.equals(&a.to_string_value(), &b.to_string_value());
    }
    value_eq(a, b).unwrap_or(false)
}

/// Whether `top` is `node` or one of its ancestors.
fn has_ancestor_or_self(node: &Nav<'_>, top: &Nav<'_>) -> bool {
    let mut cursor = node.clone();
    loop {
        if cursor.is_same_position(top) {
            return true;
        }
        if !cursor.move_to_parent() {
            return false;
        }
    }
}

/// Calls `f` on every node of the tree rooted at `root` that a pattern can
/// match, in document order: the root, then each element followed by its
/// attributes and its children. Namespace nodes are skipped — no pattern
/// selects one (§5.5.3: patterns use only the `child` and `attribute` axes).
fn for_each_node<'t>(
    root: &Nav<'t>,
    mut f: impl FnMut(&Nav<'t>) -> Result<(), XsltError>,
) -> Result<(), XsltError> {
    f(root)?;
    let mut nav = root.clone();
    if !nav.move_to_first_child() {
        return Ok(());
    }
    loop {
        f(&nav)?;
        if nav.node_type() == DomNodeType::Element {
            let mut attribute = nav.clone();
            if attribute.move_to_first_attribute() {
                loop {
                    f(&attribute)?;
                    if !attribute.move_to_next_attribute() {
                        break;
                    }
                }
            }
        }
        if nav.move_to_first_child() {
            continue;
        }
        loop {
            if nav.move_to_next_sibling() {
                break;
            }
            if !nav.move_to_parent() || nav.is_same_position(root) {
                return Ok(());
            }
        }
    }
}
