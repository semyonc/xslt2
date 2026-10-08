//! `xsl:for-each-group` at run time (§14.3): allocating the population to
//! groups, putting the groups in processing order, and evaluating the body once
//! per group with that group as the current group (§14.1, §14.2; DESIGN §9.1).
//!
//! `group-by` hashes its grouping keys with the `eq`-consistent placement of
//! `runtime/keys.rs` — the same slots an `xsl:key` index uses — so allocating a
//! population of *n* items is linear in *n* for every key type that placement
//! can hash, not *n* × (number of groups). A slot only narrows the candidates:
//! every candidate group is confirmed by the real comparison, `eq` with strings
//! under the grouping collation (`key_values_equal`), with one change §14.3
//! makes for grouping: "the value NaN is considered equal to itself".

use std::collections::HashMap;
use std::rc::Rc;

use xsd_schema::types::{XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::atomize_sequence;
use xsd_schema::xpath::XmlItem;

use super::context::{CurrentGroup, XsltContext};
use super::keys::{key_values_equal, placement_of, KeyCollation, Placement, Slot};
use super::patterns::EvaluationScope;
use super::sort::SortRows;
use super::{Frame, Transformation};
use crate::collation::{
    CollationResolver, CODEPOINT_COLLATION_URI, UCA_COLLATION_PREFIX, UCA_MILESTONE,
};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::{Nav, Receiver};
use crate::whitespace;

/// One group while the population is being allocated.
struct Group<'t> {
    /// The members, in population order; the first is the group's *initial
    /// item* (§14.3: "the item within the group that is first in population
    /// order").
    items: Vec<XmlItem<Nav<'t>>>,
    /// The grouping key the group was created for — a grouping key of its
    /// initial item, and what later keys are compared with (§14.5: "those
    /// existing groups G such that the grouping key of the initial item of G
    /// is equal to K"); `None` for the pattern forms.
    key: Option<XmlValue>,
    /// The population position of the last member added: §14.3 "An item will
    /// never be assigned more than once to the same group".
    last: usize,
}

impl<'t> Group<'t> {
    fn new(item: &XmlItem<Nav<'t>>, key: Option<XmlValue>, position: usize) -> Self {
        Group {
            items: vec![item.clone()],
            key,
            last: position,
        }
    }

    /// Adds the item at `position`, unless it is already a member.
    fn add(&mut self, item: &XmlItem<Nav<'t>>, position: usize) {
        if self.last != position {
            self.items.push(item.clone());
            self.last = position;
        }
    }
}

/// The `group-by` groups found so far, by the hash slot of their key.
///
/// The obligation is the one `runtime/keys.rs` states for an index: a key
/// equal to a group's key can be found from it — in its home slot, in its
/// `also` slot (a date/time value on the other side of the timezone line), or
/// among the groups whose key has no hash (`unhashed`, compared with every
/// key). A key that itself has no hash is compared with every group.
#[derive(Default)]
struct GroupIndex {
    by_slot: HashMap<Slot, Vec<usize>>,
    unhashed: Vec<usize>,
    /// The group of NaN, which for grouping "is considered equal to itself".
    nan: Option<usize>,
}

impl GroupIndex {
    /// The earliest-created group whose key equals `key`, if any.
    ///
    /// §14.5: "If there is more than one group G (which can only happen in
    /// exceptional circumstances involving non-transitivity), then one of these
    /// groups is selected in an implementation-dependent way" — the one
    /// created first.
    fn find(
        &self,
        groups: &[Group<'_>],
        key: &XmlValue,
        placement: &Placement,
        collation: &KeyCollation,
    ) -> Option<usize> {
        let equal = |&g: &usize| {
            groups[g]
                .key
                .as_ref()
                .is_some_and(|k| key_values_equal(k, key, collation))
        };
        if placement.home == Slot::Unhashed {
            // No hash for the sought key: every group is a candidate.
            return (0..groups.len()).find(equal);
        }
        let slots = [Some(&placement.home), placement.also.as_ref()];
        slots
            .into_iter()
            .flatten()
            .filter_map(|slot| self.by_slot.get(slot))
            .chain(std::iter::once(&self.unhashed))
            .filter_map(|candidates| candidates.iter().copied().find(|g| equal(g)))
            .min()
    }

    fn insert(&mut self, group: usize, placement: Placement) {
        match placement.home {
            Slot::Unhashed => self.unhashed.push(group),
            home => self.by_slot.entry(home).or_default().push(group),
        }
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// Evaluates an `xsl:for-each-group` instruction (§14.3).
    pub(super) fn for_each_group(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let InstructionKind::ForEachGroup {
            select,
            grouping,
            collation,
            sort,
            body,
        } = &instruction.kind
        else {
            return Err(XsltError::internal(
                "for_each_group called on another instruction",
            ));
        };
        // §14.3: "The sequence of items to be grouped, which is referred to as
        // the population, is determined by evaluating the XPath expression
        // contained in the select attribute."
        let groups = match grouping {
            Grouping::By(key) | Grouping::Adjacent(key) => {
                // The collation is resolved first, even for an empty
                // population: XTDE1110 is a property of the attribute, as
                // XTDE1035 is of xsl:sort's (`sort-027`).
                let collation =
                    self.grouping_collation(instruction, collation.as_ref(), frame, cx)?;
                let population = self.eval_expr(*select, frame, cx)?.into_vec();
                if matches!(grouping, Grouping::By(_)) {
                    self.group_by(*key, &population, &collation, frame, cx)?
                } else {
                    self.group_adjacent(*key, &population, &collation, frame, cx)?
                }
            }
            Grouping::StartingWith(pattern) | Grouping::EndingWith(pattern) => {
                let population = self.eval_expr(*select, frame, cx)?.into_vec();
                let starting = matches!(grouping, Grouping::StartingWith(_));
                self.group_by_pattern(*pattern, &population, starting, frame)?
            }
        };
        let groups: Vec<Rc<CurrentGroup<'t>>> = groups
            .into_iter()
            .map(|group| {
                Rc::new(CurrentGroup {
                    items: group.items,
                    key: group.key,
                    serial: self.next_group_serial(),
                })
            })
            .collect();
        let outcome = self.evaluate_groups(&groups, sort, body, frame, cx, out);
        // No group of this instruction is current any more (§14.3: "On
        // completion of the evaluation of the xsl:for-each-group instruction,
        // the current group and current grouping key revert to their previous
        // value"), so the key indexes built for them can never be asked for
        // again. Each body drops its group's as it ends; this also drops those
        // of groups whose body never ran because an error ended the
        // instruction early.
        let mut keys = self.keys.borrow_mut();
        for group in &groups {
            keys.forget_group(group.serial);
        }
        outcome
    }

    /// Puts the groups of an `xsl:for-each-group` instruction in processing
    /// order and evaluates its body once per group (§14.3).
    fn evaluate_groups(
        &self,
        groups: &[Rc<CurrentGroup<'t>>],
        sort: &[SortKey],
        body: &Body,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let count = groups.len();

        // §14.3: "If there are no xsl:sort elements immediately within the
        // xsl:for-each-group element, the processing order of the groups is
        // the order of first appearance." The groups were created in that
        // order. Otherwise each sort key "is evaluated once for each group.
        // During this evaluation, the context item is the initial item of the
        // group, the context position is the position of this item within the
        // set of initial items (that is, one item for each group in the
        // population) in population order, the context size is the number of
        // groups, the current group is the group whose sort key value is being
        // determined, and the current grouping key is the grouping key for that
        // group."
        let order: Vec<usize> = if sort.is_empty() {
            (0..count).collect()
        } else {
            self.sort_order(
                sort,
                SortRows::Groups,
                count,
                |index| group_context(cx, &groups[index], index + 1, count),
                frame,
                cx,
            )?
        };

        // §14.3: "The sequence constructor contained in the xsl:for-each-group
        // element is evaluated once for each of the groups, in processing
        // order. … Within the sequence constructor, the context item is the
        // initial item of the relevant group, the context position is the
        // position of this item among the sequence of initial items (one item
        // for each group) arranged in processing order of the groups, the
        // context size is the number of groups, the current group is the group
        // being processed, and the current grouping key is the grouping key
        // for that group."
        //
        // §6.7: "When an xsl:for-each, xsl:for-each-group, or
        // xsl:analyze-string instruction is evaluated … the current template
        // rule becomes null for the evaluation of that instruction"; §6.5: "No
        // other instruction changes the current mode." The frame is the
        // instruction's own. When the last group is done the caller's `cx` is
        // in force again — §14.3: "On completion of the evaluation of the
        // xsl:for-each-group instruction, the current group and current
        // grouping key revert to their previous value."
        for (position, &index) in order.iter().enumerate() {
            let mut inner = group_context(cx, &groups[index], position + 1, count);
            inner.current_rule = None;
            self.eval_body(body, frame, &inner, out)?;
            // The group is never current again: its key indexes go now, so
            // that at most the groups being evaluated hold any.
            self.keys.borrow_mut().forget_group(groups[index].serial);
        }
        Ok(())
    }

    /// A serial number for a new current group ([`CurrentGroup::serial`]).
    fn next_group_serial(&self) -> u64 {
        let serial = self.group_serials.get() + 1;
        self.group_serials.set(serial);
        serial
    }

    /// The collation string grouping keys are compared under (§14.3).
    ///
    /// > If the values are strings, or untyped atomic values, then if there is
    /// > a collation attribute the values are compared using the collation
    /// > specified as the effective value of the collation attribute, resolved
    /// > if relative against the base URI of the xsl:for-each-group element.
    /// > If there is no collation attribute then the default collation is
    /// > used.
    ///
    /// > \[ERR XTDE1110\] It is a non-recoverable dynamic error if the
    /// > collation URI specified to xsl:for-each-group (after resolving against
    /// > the base URI) is a collation that is not recognized by the
    /// > implementation.
    ///
    /// The default collation of the instruction's static context was checked
    /// when the stylesheet was compiled (§3.6.1, XTSE0125).
    fn grouping_collation(
        &self,
        instruction: &Instruction,
        collation: Option<&Avt>,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<KeyCollation, XsltError> {
        let sc = self.sheet.static_contexts.get(instruction.ctx);
        let uri = match collation {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                crate::resolver::resolve_uri(&sc.base_uri, whitespace::trim(&value))
            }
            None => sc.default_collation.clone(),
        };
        if uri == CODEPOINT_COLLATION_URI {
            return Ok(KeyCollation::Codepoint);
        }
        if let Some(collation) = self.sheet.functions().collations().resolve(&uri) {
            return Ok(KeyCollation::Other(collation));
        }
        if uri.starts_with(UCA_COLLATION_PREFIX) {
            return Err(XsltError::not_implemented(
                format!("the collation {uri}"),
                UCA_MILESTONE,
            ));
        }
        Err(XsltError::new(
            codes::XTDE1110,
            format!(
                "xsl:for-each-group names the collation {uri:?}, which this processor does not \
                 recognize"
            ),
        ))
    }

    /// The grouping keys of the item at `position` (1-based) of a population
    /// of `size` items.
    ///
    /// > \[§14.3\] When calculating grouping keys for an item in the
    /// > population, the expression contained in the group-by or group-adjacent
    /// > attribute is evaluated with that item as the context item, with its
    /// > position in population order as the context position, and with the
    /// > size of the population as the context size. The resulting sequence is
    /// > atomized, and each atomic value in the atomized sequence acts as a
    /// > grouping key for that item in the population.
    ///
    /// and, from the definition of grouping keys, "then casting an
    /// xs:untypedAtomic value to xs:string". `current()` is the item (§16.6.1,
    /// an outermost expression); the current group is still the enclosing
    /// instruction's, if any.
    fn grouping_keys(
        &self,
        expr: ExprId,
        item: &XmlItem<Nav<'t>>,
        position: usize,
        size: usize,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<XmlValue>, XsltError> {
        let mut inner = cx.with_focus(item.clone(), position, size);
        inner.current_rule = None;
        let value = self.eval_expr(expr, frame, &inner)?;
        let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
        Ok(atoms
            .into_iter()
            .map(|atom| {
                if atom.is_untyped() {
                    XmlValue::string(atom.to_string_value())
                } else {
                    atom
                }
            })
            .collect())
    }

    /// `group-by` (§14.3).
    ///
    /// > If the group-by attribute is present, the items in the population are
    /// > examined, in population order. For each item J, the expression in the
    /// > group-by attribute is evaluated to produce a sequence of zero or more
    /// > grouping key values. For each one of these grouping keys, if there is
    /// > already a group created to hold items having that grouping key value,
    /// > J is added to that group; otherwise a new group is created for items
    /// > with that grouping key value, and J becomes its first member.
    ///
    /// Groups are created in their order of first appearance: by the
    /// population position of their initial item and, for one initial item, in
    /// the order of its keys.
    fn group_by(
        &self,
        expr: ExprId,
        population: &[XmlItem<Nav<'t>>],
        collation: &KeyCollation,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<Group<'t>>, XsltError> {
        let size = population.len();
        let mut groups: Vec<Group<'t>> = Vec::new();
        let mut index = GroupIndex::default();
        for (position, item) in population.iter().enumerate() {
            for key in self.grouping_keys(expr, item, position + 1, size, frame, cx)? {
                // `placement_of` has no slot for exactly one value, NaN, which
                // `eq` makes equal to nothing and grouping to itself.
                let Some(placement) = placement_of(&key, collation) else {
                    match index.nan {
                        Some(g) => groups[g].add(item, position),
                        None => {
                            index.nan = Some(groups.len());
                            groups.push(Group::new(item, Some(key), position));
                        }
                    }
                    continue;
                };
                match index.find(&groups, &key, &placement, collation) {
                    Some(g) => groups[g].add(item, position),
                    None => {
                        index.insert(groups.len(), placement);
                        groups.push(Group::new(item, Some(key), position));
                    }
                }
            }
        }
        Ok(groups)
    }

    /// `group-adjacent` (§14.3).
    ///
    /// > If the group-adjacent attribute is present, the items in the
    /// > population are examined, in population order. If an item has the same
    /// > value for the grouping key as its preceding item within the population
    /// > (in population order), then it is assigned to the same group as its
    /// > preceding item; otherwise a new group is created and the item becomes
    /// > its first member.
    ///
    /// > \[ERR XTTE1100\] It is a type error if the grouping key evaluated using
    /// > the group-adjacent attribute is an empty sequence, or a sequence
    /// > containing more than one item.
    fn group_adjacent(
        &self,
        expr: ExprId,
        population: &[XmlItem<Nav<'t>>],
        collation: &KeyCollation,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<Group<'t>>, XsltError> {
        let size = population.len();
        let mut groups: Vec<Group<'t>> = Vec::new();
        let mut previous: Option<XmlValue> = None;
        for (position, item) in population.iter().enumerate() {
            let mut keys = self.grouping_keys(expr, item, position + 1, size, frame, cx)?;
            if keys.len() != 1 {
                return Err(XsltError::new(
                    codes::XTTE1100,
                    format!(
                        "the group-adjacent key of item {} is a sequence of {} items, not one",
                        position + 1,
                        keys.len()
                    ),
                ));
            }
            let key = keys.pop().expect("exactly one key");
            match (previous.as_ref(), groups.last_mut()) {
                (Some(before), Some(group)) if grouping_keys_equal(before, &key, collation) => {
                    group.add(item, position);
                }
                _ => groups.push(Group::new(item, Some(key.clone()), position)),
            }
            previous = Some(key);
        }
        Ok(groups)
    }

    /// `group-starting-with` (`starting`) or `group-ending-with` (§14.3).
    ///
    /// > \[ERR XTTE1120\] When the group-starting-with or group-ending-with
    /// > attribute is used, it is a type error if the result of evaluating the
    /// > select expression contains an item that is not a node.
    ///
    /// > \[group-starting-with\] If a node matches the pattern, or is the first
    /// > node in the population, then a new group is created and the node
    /// > becomes its first member. Otherwise, the node is assigned to the same
    /// > group as its preceding node within the population.
    ///
    /// > \[group-ending-with\] If a node is the first node in the population, or
    /// > if the previous node in the population matches the pattern, then a new
    /// > group is created and the node becomes its first member. Otherwise, the
    /// > node is assigned to the same group as its preceding node within the
    /// > population.
    ///
    /// So the first node of `group-starting-with` and the last node of
    /// `group-ending-with` are never matched: the answer cannot change the
    /// groups. The pattern is matched in the instruction's frame, where its
    /// predicates find the local variables in scope; an error raised while
    /// matching is recoverable as for every pattern (§5.5.4).
    ///
    /// The matches share one [`EvaluationScope`], made here and dropped on
    /// return: the frame cannot change during this evaluation, so a
    /// positional predicate after one that reads a local variable
    /// (`r[@k = $v][position() mod 100 = 1]`) filters each parent's children
    /// once per evaluation instead of once per population node.
    fn group_by_pattern(
        &self,
        pattern: PatternId,
        population: &[XmlItem<Nav<'t>>],
        starting: bool,
        frame: &Frame<'t>,
    ) -> Result<Vec<Group<'t>>, XsltError> {
        let nodes: Vec<&Nav<'t>> = population
            .iter()
            .enumerate()
            .map(|(position, item)| match item {
                XmlItem::Node(node) => Ok(node),
                XmlItem::Atomic(value) => Err(XsltError::new(
                    codes::XTTE1120,
                    format!(
                        "item {} of the population is the atomic value {:?}, and {} needs \
                         nodes",
                        position + 1,
                        self.atom_string(value),
                        if starting {
                            "group-starting-with"
                        } else {
                            "group-ending-with"
                        }
                    ),
                )),
            })
            .collect::<Result<_, _>>()?;
        let pattern = self.sheet.pattern(pattern);
        let scope = EvaluationScope::new(frame);
        let last = nodes.len().saturating_sub(1);
        let mut groups: Vec<Group<'t>> = Vec::new();
        let mut previous_matched = false;
        for (position, (item, node)) in population.iter().zip(&nodes).enumerate() {
            let starts = position == 0
                || if starting {
                    self.matches_in(pattern, node, &scope)?
                } else {
                    previous_matched
                };
            if starts {
                groups.push(Group::new(item, None, position));
            } else if let Some(group) = groups.last_mut() {
                group.add(item, position);
            }
            if !starting && position < last {
                previous_matched = self.matches_in(pattern, node, &scope)?;
            }
        }
        Ok(groups)
    }
}

/// The dynamic context for one group: the focus on its initial item at
/// `position` of `size`, `current()` the same item (§16.6.1), and the group as
/// the current group and grouping key.
fn group_context<'t>(
    cx: &XsltContext<'t>,
    group: &Rc<CurrentGroup<'t>>,
    position: usize,
    size: usize,
) -> XsltContext<'t> {
    let mut inner = cx.with_focus(group.items[0].clone(), position, size);
    inner.group = Some(Rc::clone(group));
    inner
}

/// Whether two grouping keys are the same (§14.3): `eq`, strings under the
/// grouping collation, incomparable values unequal — and NaN equal to NaN: "For
/// the purposes of grouping, the value NaN is considered equal to itself."
fn grouping_keys_equal(a: &XmlValue, b: &XmlValue, collation: &KeyCollation) -> bool {
    (is_nan(a) && is_nan(b)) || key_values_equal(a, b, collation)
}

/// Whether `value` is a NaN of `xs:float` or `xs:double` (the two are one
/// grouping key: §14.3 says NaN, not the NaN of one type).
fn is_nan(value: &XmlValue) -> bool {
    match &value.value {
        XmlValueKind::Union(inner) => is_nan(inner),
        _ => value.type_code.is_numeric() && value.as_double().is_some_and(f64::is_nan),
    }
}
