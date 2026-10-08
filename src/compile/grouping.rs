//! `xsl:for-each-group` (§14.3): the population, the one grouping attribute,
//! the collation, the sort key specification and the body.

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::*;
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, ModuleId, StaticContextId};

/// The four grouping attributes, of which exactly one must be present.
const GROUPING_ATTRIBUTES: [&str; 4] = [
    "group-by",
    "group-adjacent",
    "group-starting-with",
    "group-ending-with",
];

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:for-each-group` instruction.
    ///
    /// The element table ([`checks`](super::checks)) has already checked its
    /// attributes and that its `xsl:sort` children come first. What it cannot
    /// express are the two co-occurrence rules of §14.3:
    ///
    /// > \[ERR XTSE1080\] These four attributes are mutually exclusive: it is a
    /// > static error if none of these four attributes is present, or if more
    /// > than one of them is present.
    ///
    /// > \[ERR XTSE1090\] It is an error to specify the collation attribute if
    /// > neither the group-by attribute nor group-adjacent attribute is
    /// > specified.
    ///
    /// The `group-starting-with` / `group-ending-with` pattern is compiled in
    /// the instruction's own scope, so its predicates see the local variables
    /// in scope there; the run time matches it in the instruction's frame.
    pub(super) fn for_each_group(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let present: Vec<(&str, String)> = GROUPING_ATTRIBUTES
            .iter()
            .filter_map(|&name| attr_local(nav, name).map(|value| (name, value)))
            .collect();
        let (attribute, text) = match present.as_slice() {
            [(attribute, text)] => (*attribute, text.clone()),
            [] => {
                return Err(XsltError::new(
                    codes::XTSE1080,
                    "xsl:for-each-group needs one of group-by, group-adjacent, \
                     group-starting-with and group-ending-with",
                )
                .with_location(loc.clone()))
            }
            several => {
                let names: Vec<&str> = several.iter().map(|(name, _)| *name).collect();
                return Err(XsltError::new(
                    codes::XTSE1080,
                    format!(
                        "xsl:for-each-group may have only one grouping attribute, not {}",
                        names.join(" and ")
                    ),
                )
                .with_location(loc.clone()));
            }
        };
        let by_value = matches!(attribute, "group-by" | "group-adjacent");
        let collation = match attr_local(nav, "collation") {
            Some(_) if !by_value => {
                return Err(XsltError::new(
                    codes::XTSE1090,
                    format!(
                        "xsl:for-each-group with {attribute} may not have a collation attribute"
                    ),
                )
                .with_location(loc.clone()))
            }
            Some(value) => Some(self.avt(&value, ctx).map_err(|e| e.or_location(loc))?),
            None => None,
        };
        let select = self.required_expr(nav, "select", ctx, loc)?;
        let grouping = match attribute {
            "group-by" => Grouping::By(
                self.compile_expr(&text, ctx)
                    .map_err(|e| e.or_location(loc))?,
            ),
            "group-adjacent" => Grouping::Adjacent(
                self.compile_expr(&text, ctx)
                    .map_err(|e| e.or_location(loc))?,
            ),
            "group-starting-with" => Grouping::StartingWith(
                self.compile_pattern(&text, ctx)
                    .map_err(|e| e.or_location(loc))?,
            ),
            _ => Grouping::EndingWith(
                self.compile_pattern(&text, ctx)
                    .map_err(|e| e.or_location(loc))?,
            ),
        };
        // §13: "A sort key specification may occur immediately within … an
        // xsl:for-each-group element"; the sort keys and the body are compiled
        // as for xsl:for-each.
        let sort = self.sort_keys(nav, module_id)?;
        let body = self.sequence_constructor_skipping(nav, module_id, "sort")?;
        Ok(InstructionKind::ForEachGroup {
            select,
            grouping,
            collation,
            sort,
            body,
        })
    }
}
