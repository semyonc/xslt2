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
        // `namespace-alias-1902` and `-4201`, where the XSLT namespace —
        // otherwise always excluded — is the target of an alias.
        //
        // What the precedence sentence is *about* is spelled out by §11.1.4's
        // own note, which is the key to reading it:
        //
        // > These rules achieve the effect that the element generated from the
        // > literal result element will have an in-scope namespace node that
        // > binds **the `result-prefix`** to the target namespace URI, provided
        // > that the namespace declaration associating **this prefix with this
        // > URI** is in scope for both the `xsl:namespace-alias` instruction
        // > and for the literal result element.
        //
        // So the node the precedence protects is the one binding an alias's
        // `result-prefix` to that alias's target URI: that node has to reach the
        // result even when its URI is *also* a literal namespace URI (the chain
        // `a → b`, `b → c`, where `b`'s URI is both) and even when it is an
        // excluded namespace. A node that merely happens to carry a target URI
        // under some other prefix is not what the note guarantees, so for it
        // rule 1 still applies — which is why `namespace-alias-1006`, a circular
        // chain of five aliases whose prefix bindings sit on the
        // `xsl:namespace-alias` elements and are therefore **not** in scope on
        // any literal result element, copies none of them: the only namespace
        // node in scope on `<v xmlns="http://a.com/"/>` binds the *default*
        // prefix, and no alias has `result-prefix="#default"`, so rule 1 drops
        // it and the `b:v` binding comes from namespace fixup (§5.7.3) instead.
        //
        // Hence the three tests below, in the order the rules are resolved.
        let mut namespaces = Vec::new();
        for (prefix, uri) in in_scope_namespaces(nav) {
            // Rule 2, taking precedence over rule 1: an alias's own
            // `result-prefix` → target-URI binding, copied whatever else the URI
            // is used for and whether or not it is excluded.
            if self
                .aliases
                .values()
                .any(|alias| alias.uri == uri && alias.prefix == prefix)
            {
                namespaces.push((prefix, uri));
                continue;
            }
            // Rule 1: a literal namespace URI is a stylesheet-internal fiction.
            if self.aliases.contains_key(&uri) {
                continue;
            }
            // Rule 2 for every other binding of a target namespace URI: copied
            // "whether or not the URI identifies an excluded namespace".
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
                    // §3.9: "if an element has an attribute that XSLT 2.0 does
                    // not allow the element to have, then the attribute must
                    // be ignored" — a literal result element included.
                    _ if sc.compat.is_forwards() => {}
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
