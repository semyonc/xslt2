//! Traversal of the stylesheet tree, and the §3.12 `use-when` exclusion
//! every traversal has to respect.

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::Builder;
use crate::receiver::Nav;
use crate::stylesheet::{is_stripped_stylesheet_text, Children, ModuleId};

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// The children of a stylesheet element that §3.12 leaves in the module.
    ///
    /// The **only** way the compiler walks a stylesheet element's children.
    /// [`Module::children`](crate::stylesheet::Module::children) drops every
    /// element the [`Loader`](crate::stylesheet::Loader) excluded by `use-when`
    /// — together with its whole subtree, which is never descended into — and
    /// passes text, comments and processing instructions through unchanged. A
    /// traversal written with this helper therefore cannot forget `use-when`,
    /// which nine hand-written sibling walks each had to remember before.
    ///
    /// The iterator borrows the [`ModuleSet`](crate::stylesheet::ModuleSet), not
    /// the builder, so the loop body is free to take `&mut self`.
    pub(super) fn children(&self, nav: &Nav<'t>, module_id: ModuleId) -> Children<'m, 't> {
        self.modules.module(module_id).children(nav)
    }

    /// Whether an element has any content that is not stripped whitespace
    /// (§4.2) and is not excluded by §3.12.
    pub(super) fn has_content(&self, nav: &Nav<'t>, module_id: ModuleId) -> bool {
        for child in self.children(nav, module_id) {
            match child.node_type() {
                DomNodeType::Element => return true,
                kind if kind.is_text_like() && !is_stripped_stylesheet_text(&child) => return true,
                _ => {}
            }
        }
        false
    }
}
