//! `xsl:key` (§16.3.1): one declaration at a time, merged by name into the
//! key table.

use super::Builder;
use crate::collation::{UCA_COLLATION_PREFIX, UCA_MILESTONE};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, ModuleId, StaticContextId};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:key` declaration and adds it to the key of its name.
    ///
    /// > \[Definition: A key is defined as a set of `xsl:key` declarations in the
    /// > stylesheet that share the same name.\]
    ///
    /// Import precedence plays no part: "all the `xsl:key` declarations in the
    /// stylesheet are effective regardless of their import precedence".
    pub(super) fn key_declaration(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        // `name` and `match` are required by the element table (XTSE0010), so
        // their absence never reaches this point; the errors are a safety net.
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:key has no name attribute")
                    .with_location(loc.clone())
            })?;
        let match_text = attr_local(nav, "match").ok_or_else(|| {
            XsltError::new(codes::XTSE0010, "xsl:key has no match attribute")
                .with_location(loc.clone())
        })?;

        // > [ERR XTSE1205] It is a static error if an xsl:key declaration has a
        // > use attribute and has non-empty content, or if it has empty content
        // > and no use attribute.
        let use_text = attr_local(nav, "use");
        let has_content = self.has_content(nav, module_id);
        if use_text.is_some() == has_content {
            return Err(XsltError::new(
                codes::XTSE1205,
                if has_content {
                    format!("xsl:key {name} has both a use attribute and content")
                } else {
                    format!("xsl:key {name} has neither a use attribute nor content")
                },
            )
            .with_location(loc));
        }

        let collation = self.key_collation(nav, ctx, &loc)?;

        // DESIGN §9.1: the pattern and the key specifier see the global
        // variables only — a top-level declaration has no other variables in
        // scope, and the sequence constructor's own locals live in a frame of
        // their own.
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let pattern = self.compile_pattern(&match_text, ctx)?;
        let specifier = match use_text {
            Some(text) => ValueSource::Select(self.compile_expr(&text, ctx)?),
            None => ValueSource::Content(self.sequence_constructor(nav, module_id)?),
        };
        self.scope.truncate(scope_mark);
        let definition = KeyDefinition {
            pattern,
            specifier,
            frame_size: self.frame_size,
            loc: loc.clone(),
        };
        let compat = self.static_contexts.get(ctx).compat.is_backwards();

        match self.key_index.get(&name) {
            Some(id) => {
                let key = &mut self.keys[id.0 as usize];
                // > [ERR XTSE1220] It is a static error if there are several
                // > xsl:key declarations in the stylesheet with the same key
                // > name and different effective collations. Two collations are
                // > the same if their URIs are equal under the rules for
                // > comparing xs:anyURI values …
                if key.collation != collation {
                    return Err(XsltError::new(
                        codes::XTSE1220,
                        format!(
                            "the declarations of the key {name} have different effective \
                             collations: {:?} and {collation:?}",
                            key.collation
                        ),
                    )
                    .with_location(loc));
                }
                key.compat |= compat;
                key.definitions.push(definition);
            }
            None => {
                let id = KeyId(self.keys.len() as u32);
                self.key_index.insert(name.clone(), id);
                self.keys.push(Key {
                    name,
                    definitions: vec![definition],
                    collation,
                    compat,
                });
            }
        }
        Ok(())
    }

    /// The effective collation of one `xsl:key` declaration (§16.3.1).
    ///
    /// > The effective collation for an `xsl:key` declaration is the collation
    /// > specified in its `collation` attribute if present, resolved against
    /// > the base URI of the `xsl:key` element, or the default collation that
    /// > is in scope for the `xsl:key` declaration otherwise
    ///
    /// > \[ERR XTSE1210\] It is a static error if the `xsl:key` declaration has
    /// > a `collation` attribute whose value (after resolving against the base
    /// > URI) is not a URI recognized by the implementation as referring to a
    /// > collation.
    fn key_collation(
        &self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        loc: &crate::error::Location,
    ) -> Result<String, XsltError> {
        let sc = self.static_contexts.get(ctx);
        let Some(value) = attr_local(nav, "collation") else {
            return Ok(sc.default_collation.clone());
        };
        let uri = crate::resolver::resolve_uri(&sc.base_uri, whitespace::trim(&value));
        if self.functions.collations().recognizes(&uri) {
            return Ok(uri);
        }
        if uri.starts_with(UCA_COLLATION_PREFIX) {
            return Err(
                XsltError::not_implemented(format!("the collation {uri}"), UCA_MILESTONE)
                    .with_location(loc.clone()),
            );
        }
        Err(XsltError::new(
            codes::XTSE1210,
            format!("xsl:key names the collation {uri:?}, which this processor does not recognize"),
        )
        .with_location(loc.clone()))
    }
}
