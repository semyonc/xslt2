//! The XSLT half of the dynamic context (§5.4, §9.1) and the variable frame
//! the compiler numbered slots in (§7.5).

use std::collections::HashMap;
use std::rc::Rc;

use xsd_schema::types::XmlValue;
use xsd_schema::xpath::functions::XPathValue;
use xsd_schema::xpath::XmlItem;

use crate::model::{ModeKey, RuleId};
use crate::names::QName;
use crate::receiver::Nav;

/// A variable frame: slot-indexed, as the compiler numbered them (§7.5).
#[derive(Default)]
pub struct Frame<'t> {
    slots: Vec<Option<XPathValue<Nav<'t>>>>,
}

impl<'t> Frame<'t> {
    pub fn with_slots(n: usize) -> Self {
        Self {
            slots: (0..n).map(|_| None).collect(),
        }
    }

    pub fn set(&mut self, slot: usize, value: XPathValue<Nav<'t>>) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }
        self.slots[slot] = Some(value);
    }

    pub fn get(&self, slot: usize) -> Option<&XPathValue<Nav<'t>>> {
        self.slots.get(slot).and_then(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Tunnel parameters (§10.1.2): a persistent map that travels through every
/// call until something overrides it.
type TunnelMap<'t> = HashMap<QName, XPathValue<Nav<'t>>>;

/// The current group and the current grouping key while an
/// `xsl:for-each-group` body or sort key is evaluated (§14.1, §14.2).
pub(crate) struct CurrentGroup<'t> {
    /// "the collection of related items that are processed collectively in one
    /// iteration of the xsl:for-each-group element", in population order.
    pub(crate) items: Vec<XmlItem<Nav<'t>>>,
    /// "the grouping key of the initial item in the group, after atomization
    /// and casting of xs:untypedAtomic to xs:string"; `None` for
    /// `group-starting-with` / `group-ending-with`, where "the current grouping
    /// key is the empty sequence".
    pub(crate) key: Option<XmlValue>,
    /// A number no other group of the run has (the first is 1): the key
    /// indexes built for this group are kept under it (`runtime/keys.rs`), and
    /// dropped when the group's evaluation ends. Never an address, which a
    /// later group could reuse.
    pub(crate) serial: u64,
}

/// The XSLT half of the dynamic context (§5.4, §9.1).
#[derive(Clone)]
pub struct XsltContext<'t> {
    /// The focus: context item, position and size.
    pub item: Option<XmlItem<Nav<'t>>>,
    pub position: usize,
    pub size: usize,
    /// What `current()` returns — the focus of the innermost instruction, not
    /// of the expression being evaluated (§16.6.2).
    pub current: Option<XmlItem<Nav<'t>>>,
    pub mode: ModeKey,
    /// The current template rule (§5.4) — one *alternative* of a declaration's
    /// pattern, because that is what §6.4 calls a template rule.
    pub current_rule: Option<RuleId>,
    pub tunnel: Rc<TunnelMap<'t>>,
    /// The current group and grouping key (§14.1, §14.2); `None` when both are
    /// the empty sequence. §5.4.4: set by `xsl:for-each-group`, cleared by
    /// "calls on stylesheet functions" — and by nothing else, so a template
    /// invoked from a group's body still sees it, and so does a key
    /// specifier evaluated for a `key()` call made there
    /// (`Transformation::key_lookup`). A context built from
    /// [`empty`](Self::empty) has none: a stylesheet function's body, a global
    /// variable, a pattern (a key's `match` included).
    pub(crate) group: Option<Rc<CurrentGroup<'t>>>,
}

impl<'t> XsltContext<'t> {
    pub(super) fn empty() -> Self {
        Self {
            item: None,
            position: 0,
            size: 0,
            current: None,
            mode: ModeKey::Default,
            current_rule: None,
            tunnel: Rc::new(TunnelMap::default()),
            group: None,
        }
    }

    /// The transitions of §9.1 that replace the focus: `xsl:for-each`,
    /// `xsl:apply-templates` and the built-in rules all set `current()` to the
    /// new focus item.
    pub(super) fn with_focus(&self, item: XmlItem<Nav<'t>>, position: usize, size: usize) -> Self {
        Self {
            item: Some(item.clone()),
            position,
            size,
            current: Some(item),
            ..self.clone()
        }
    }
}
