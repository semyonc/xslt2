//! Traversal of the stylesheet tree, and the §3.12 `use-when` exclusion
//! every traversal has to respect.

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::Builder;
use crate::receiver::Nav;
use crate::stylesheet::{is_stripped_stylesheet_text, ModuleId};

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Whether an element has any content that is not stripped whitespace
    /// (§4.2) and is not excluded by §3.12.
    pub(super) fn has_content(&self, nav: &Nav<'t>, module_id: ModuleId) -> bool {
        let mut child = nav.clone();
        if !child.move_to_first_child() {
            return false;
        }
        loop {
            match child.node_type() {
                DomNodeType::Element if self.excluded(&child, module_id) => {}
                DomNodeType::Element => return true,
                kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => return true,
                _ => {}
            }
            if !child.move_to_next_sibling() {
                return false;
            }
        }
    }

    /// Whether §3.12 effectively excludes this element, and with it its whole
    /// subtree.
    ///
    /// The decision was made once, by the [`Loader`], before the module graph
    /// was even resolved; every traversal in the compiler consults it, so no
    /// instruction compiler has to remember `use-when`.
    pub(super) fn excluded(&self, nav: &Nav<'t>, module_id: ModuleId) -> bool {
        self.modules.module(module_id).is_excluded(nav)
    }
}
