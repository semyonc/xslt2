//! Literal result elements (§11.1) and the `xsl:namespace-alias` rewrite
//! they go through (§11.1.4).

use xsd_schema::navigator::DomNavigator;

use super::attributes::STANDARD_ATTRS;
use super::Builder;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::{parse_yes_no, XSLT_NS};
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
        //
        // §11.1.4 overrides that for the two namespaces aliasing is about:
        //
        // > A namespace node whose string value is a **literal namespace URI**
        // > is not copied to the result tree.
        // >
        // > A namespace node whose string value is a **target namespace URI**
        // > is copied to the result tree, **whether or not the URI identifies
        // > an excluded namespace**.
        // >
        // > In the event that the same URI is used as a literal namespace URI
        // > and a target namespace URI, the second of these rules takes
        // > precedence.
        //
        // The target rule is what makes `xmlns:xsl` survive in
        // `namespace-alias-1902`, where the XSLT namespace — otherwise always
        // excluded — is the target of an alias. It is also the mechanism behind
        // §11.1.4's own note: the result element gets its `result-prefix` →
        // target binding from the stylesheet's own declaration of that prefix,
        // not from rewriting the literal one.
        //
        // The order below puts the **literal** rule first, which is the one
        // deviation from the wording above, and it is the suite that forces it:
        // `namespace-alias-1006` is a circular chain of five aliases in which
        // every URI is both a literal and a target URI, and its expected result
        // copies **none** of them — each result element carries only the
        // declaration its own aliased name needs. Reading "the second of these
        // rules takes precedence" literally would copy all five. Both readings
        // agree wherever a URI plays only one of the two roles, which is every
        // other case in the suite.
        let mut namespaces = Vec::new();
        for (prefix, uri) in in_scope_namespaces(nav) {
            if self.aliases.contains_key(&uri) {
                continue;
            }
            if self.aliases.values().any(|alias| alias.uri == uri) {
                namespaces.push((prefix, uri));
                continue;
            }
            if uri == XSLT_NS || sc.is_excluded(&uri) || sc.is_extension_namespace(&uri) {
                continue;
            }
            namespaces.push((prefix, uri));
        }

        let mut attrs = Vec::new();
        let mut attr_sets = Vec::new();
        // §11.1: "A literal result element may have an optional
        // `xsl:inherit-namespaces` attribute, with the value `yes` or `no`. The
        // default value is `yes`."
        let mut inherit_namespaces = true;
        for (attr_uri, local, attr_prefix, value) in attributes(nav) {
            if attr_uri == XSLT_NS {
                match local.as_str() {
                    "use-attribute-sets" => {
                        attr_sets = self.qname_list_value(&value, ctx)?;
                    }
                    "inherit-namespaces" => {
                        inherit_namespaces = parse_yes_no(&value, "xsl:inherit-namespaces")
                            .map_err(|e| e.with_location(module.location(nav)))?;
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
            inherit_namespaces,
            body,
        })
    }

    /// §11.1.4: `xsl:namespace-alias` rewrites a literal result element's
    /// namespace on the way out.
    fn alias_of(&self, uri: &str, prefix: &str) -> (String, String) {
        match self.aliases.get(uri) {
            Some(alias) => (alias.uri.clone(), alias.prefix.clone()),
            None => (uri.to_string(), prefix.to_string()),
        }
    }
}
