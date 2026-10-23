//! The attribute-validation tables of §3.5 and §3.6: which attributes each
//! XSLT element allows, and the XTSE0090 check that enforces them.

use xsd_schema::navigator::DomNavigator;

use crate::error::{codes, Location, XsltError};
use crate::model::Validation;
use crate::names::{lexical_qname, QName, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, attributes, Module, StaticContext};
use crate::whitespace;

/// The standard attributes §3.5 allows on every XSLT element.
pub(super) const STANDARD_ATTRS: &[&str] = &[
    "version",
    "exclude-result-prefixes",
    "extension-element-prefixes",
    "xpath-default-namespace",
    "default-collation",
    "use-when",
];
/// The attributes in no namespace each XSLT element allows, beyond the
/// standard attributes of §3.5.
///
/// > \[ERR XTSE0090\] It is a static error if an element from the XSLT
/// > namespace has an attribute whose name is in no namespace and that is not
/// > one of the attributes defined for that element in this specification.
fn allowed_attributes(local: &str) -> Option<&'static [&'static str]> {
    Some(match local {
        "stylesheet" | "transform" => &["id", "default-validation", "input-type-annotations"],
        "include" | "import" => &["href"],
        "import-schema" => &["namespace", "schema-location"],
        "template" => &["match", "name", "priority", "mode", "as"],
        "param" => &["name", "select", "as", "required", "tunnel", "static"],
        "variable" => &["name", "select", "as", "static"],
        "with-param" => &["name", "select", "as", "tunnel"],
        "apply-templates" => &["select", "mode"],
        "apply-imports" | "next-match" | "choose" | "otherwise" | "fallback" => &[],
        "call-template" => &["name"],
        "for-each" => &["select"],
        "if" | "when" => &["test"],
        "value-of" => &["select", "separator", "disable-output-escaping"],
        "text" => &["disable-output-escaping"],
        "sequence" => &["select"],
        "element" => &[
            "name",
            "namespace",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        "attribute" => &[
            "name",
            "namespace",
            "select",
            "separator",
            "type",
            "validation",
        ],
        "namespace" => &["name", "select"],
        "comment" => &["select"],
        "processing-instruction" => &["name", "select"],
        "document" => &["validation", "type"],
        "copy" => &[
            "copy-namespaces",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        "copy-of" => &["select", "copy-namespaces", "type", "validation"],
        "message" => &["select", "terminate"],
        "strip-space" | "preserve-space" => &["elements"],
        "attribute-set" => &["name", "use-attribute-sets"],
        "namespace-alias" => &["stylesheet-prefix", "result-prefix"],
        "sort" => &[
            "select",
            "lang",
            "order",
            "collation",
            "stable",
            "case-order",
            "data-type",
        ],
        "key" => &["name", "match", "use", "collation"],
        "function" => &["name", "as", "override"],
        // Everything else (xsl:output, xsl:number, xsl:for-each-group, …) is
        // checked where it is compiled, or not at all in M1.
        _ => return None,
    })
}

/// Raises XTSE0090 for an attribute the element does not define.
pub(super) fn check_attributes(
    nav: &Nav<'_>,
    local: &str,
    loc: &Location,
) -> Result<(), XsltError> {
    for (uri, name, _, _) in attributes(nav) {
        // §3.5: the standard attributes are spelled without a prefix on an
        // XSLT element, so an attribute that *is* in the XSLT namespace is
        // never one of them.
        if uri == XSLT_NS {
            return Err(XsltError::new(
                codes::XTSE0090,
                format!("xsl:{local} has an attribute xsl:{name} in the XSLT namespace"),
            )
            .with_location(loc.clone()));
        }
    }
    let Some(allowed) = allowed_attributes(local) else {
        return Ok(());
    };
    for (uri, name, _, _) in attributes(nav) {
        if !uri.is_empty() {
            continue;
        }
        if allowed.contains(&name.as_str()) || STANDARD_ATTRS.contains(&name.as_str()) {
            continue;
        }
        return Err(XsltError::new(
            codes::XTSE0090,
            format!("xsl:{local} has no attribute named {name:?}"),
        )
        .with_location(loc.clone()));
    }
    Ok(())
}

// ── [xsl:]validation, [xsl:]type, default-validation (§19.2, §3.6) ────────

/// A module's `default-validation` (§3.6): "If the attribute is omitted, the
/// default is strip. The permitted values are preserve and strip." Read off the
/// module's `xsl:stylesheet` / `xsl:transform` element; a simplified stylesheet
/// module (§3.7) has none. The value was checked by `checks.rs` (XTSE1660 for
/// a basic processor, XTSE0020 otherwise), so anything but `preserve` is
/// `strip` here.
pub(super) fn module_default_validation(module: &Module<'_>) -> Validation {
    let root = match module.root {
        Some(node) => module.doc.create_navigator_at(node),
        None => {
            let mut nav = module.doc.create_navigator();
            if !crate::stylesheet::move_to_document_element(&mut nav) {
                return Validation::Strip;
            }
            nav
        }
    };
    let is_stylesheet =
        root.namespace_uri() == XSLT_NS && matches!(root.local_name(), "stylesheet" | "transform");
    match attr_local(&root, "default-validation") {
        Some(value) if is_stylesheet && whitespace::trim(&value) == "preserve" => {
            Validation::Preserve
        }
        _ => Validation::Strip,
    }
}

/// The expanded name an `[xsl:]type` attribute names (§19.2.1.2):
///
/// > The `[xsl:]type` attribute takes as its value a QName. … If the QName has
/// > no prefix, it is expanded using the default namespace established using
/// > the effective `[xsl:]xpath-default-namespace` attribute if there is one;
/// > otherwise, it is taken as being a name in no namespace.
///
/// > \[ERR XTSE1520\] It is a static error if the value of the type attribute
/// > … is not a valid QName, or if it uses a prefix that is not defined in an
/// > in-scope namespace declaration, or if the QName is not the name of a type
/// > definition included in the in-scope schema components for the stylesheet.
///
/// This checks the first two clauses; the third needs the schema set.
pub(crate) fn type_attribute_name(value: &str, sc: &StaticContext) -> Result<QName, XsltError> {
    let value = whitespace::trim(value);
    let (prefix, local) = lexical_qname(value).ok_or_else(|| {
        XsltError::new(
            codes::XTSE1520,
            format!("the type attribute {value:?} is not a valid QName"),
        )
    })?;
    let uri = if prefix.is_empty() {
        sc.xpath_default_namespace.clone()
    } else {
        sc.resolve_prefix(prefix).ok_or_else(|| {
            XsltError::new(
                codes::XTSE1520,
                format!("the prefix {prefix:?} of the type attribute {value:?} is not bound to a namespace"),
            )
        })?
    };
    Ok(QName::new(uri, local))
}

/// The `[xsl:]validation` values §19.2.1.1 permits.
pub(crate) fn validation_value(value: &str) -> Option<Validation> {
    Some(match whitespace::trim(value) {
        "strict" => Validation::Strict,
        "lax" => Validation::Lax,
        "preserve" => Validation::Preserve,
        "strip" => Validation::Strip,
        _ => return None,
    })
}

impl<'a, 'm, 't> super::Builder<'a, 'm, 't> {
    /// The validation of a node constructor (§19.2): its `[xsl:]type` or
    /// `[xsl:]validation` — the `xsl:`-prefixed spelling on a literal result
    /// element (`lre`) — or else its module's `default-validation`.
    ///
    /// The static rules were applied by `checks.rs` before compilation; what
    /// is left is reading the value.
    pub(super) fn validation_of(
        &self,
        nav: &Nav<'t>,
        module: &Module<'t>,
        ctx: crate::stylesheet::StaticContextId,
        lre: bool,
    ) -> Result<Validation, XsltError> {
        let read = |name: &str| {
            if lre {
                crate::stylesheet::attr_ns(nav, XSLT_NS, name)
            } else {
                attr_local(nav, name)
            }
        };
        if let Some(value) = read("type") {
            let sc = self.static_contexts.get(ctx);
            let name = type_attribute_name(&value, sc)
                .map_err(|e| e.with_location(module.location(nav)))?;
            self.validates.set(true);
            return Ok(Validation::Type(name));
        }
        if let Some(value) = read("validation") {
            let validation = validation_value(&value);
            if matches!(validation, Some(Validation::Strict | Validation::Lax)) {
                self.validates.set(true);
            }
            return validation.ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0020,
                    format!(
                        "validation must be \"strict\", \"lax\", \"preserve\" or \"strip\", not {:?}",
                        whitespace::trim(&value)
                    ),
                )
                .with_location(module.location(nav))
            });
        }
        Ok(self.module_validation[module.id.0 as usize].clone())
    }
}
