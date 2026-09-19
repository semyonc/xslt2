//! Literal result elements (§11.1) and the `xsl:namespace-alias` rewrite
//! they go through (§11.1.4).

use xsd_schema::navigator::DomNavigator;

use super::attributes::STANDARD_ATTRS;
use super::Builder;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::XSLT_NS;
use crate::receiver::{Name, Nav};
use crate::stylesheet::{attributes, in_scope_namespaces, Module, ModuleId, StaticContextId};

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// The literal result element of §11.1.
    pub(super) fn literal_element(
        &mut self,
        nav: &Nav<'t>,
        module: &Module<'t>,
        ctx: StaticContextId,
    ) -> Result<Instruction, XsltError> {
        let module_id = module.id;
        let kind = self.literal_element_kind(nav, module_id, ctx)?;
        Ok(self.wrap(kind, ctx, module_id, nav))
    }

    pub(super) fn literal_element_kind(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<InstructionKind, XsltError> {
        let module = self.modules.module(module_id);
        let sc = self.static_contexts.get(ctx).clone();

        let (uri, prefix) = self.alias_of(nav.namespace_uri(), nav.prefix());
        let name = Name::qualified(&prefix, nav.local_name(), &uri);

        // §11.1.2: every in-scope namespace of the literal result element
        // becomes a namespace node of the result element, except the XSLT
        // namespace, the excluded namespaces and the extension namespaces.
        let mut namespaces = Vec::new();
        for (prefix, uri) in in_scope_namespaces(nav) {
            if uri == XSLT_NS || sc.is_excluded(&uri) || sc.is_extension_namespace(&uri) {
                continue;
            }
            let (aliased_uri, aliased_prefix) = self.alias_of(&uri, &prefix);
            namespaces.push((aliased_prefix, aliased_uri));
        }

        let mut attrs = Vec::new();
        let mut attr_sets = Vec::new();
        for (attr_uri, local, attr_prefix, value) in attributes(nav) {
            if attr_uri == XSLT_NS {
                match local.as_str() {
                    "use-attribute-sets" => {
                        attr_sets = self.qname_list_value(&value, ctx)?;
                    }
                    other if STANDARD_ATTRS.contains(&other) => {}
                    kind @ ("type" | "validation") => {
                        return Err(XsltError::not_implemented(
                            format!("xsl:{kind} on a literal result element"),
                            "M4",
                        )
                        .with_location(module.location(nav)))
                    }
                    other => {
                        return Err(XsltError::new(
                            codes::XTSE0805,
                            format!("xsl:{other} is not allowed on a literal result element"),
                        )
                        .with_location(module.location(nav)))
                    }
                }
                continue;
            }
            let (aliased_uri, aliased_prefix) = if attr_uri.is_empty() {
                (String::new(), String::new())
            } else {
                self.alias_of(&attr_uri, &attr_prefix)
            };
            attrs.push((
                Name::qualified(&aliased_prefix, &local, &aliased_uri),
                self.avt(&value, ctx)?,
            ));
        }

        let body = self.sequence_constructor(nav, module_id)?;
        Ok(InstructionKind::LiteralElement {
            name,
            namespaces,
            attrs,
            attr_sets,
            body,
        })
    }

    /// §11.1.4: `xsl:namespace-alias` rewrites a literal result element's
    /// namespace on the way out.
    fn alias_of(&self, uri: &str, prefix: &str) -> (String, String) {
        match self.aliases.get(uri) {
            Some((to_uri, to_prefix)) => (to_uri.clone(), to_prefix.clone()),
            None => (uri.to_string(), prefix.to_string()),
        }
    }
}
