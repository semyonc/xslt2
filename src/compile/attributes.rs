//! The attribute-validation tables of §3.5 and §3.6: which attributes each
//! XSLT element allows, and the XTSE0090 check that enforces them.

use crate::error::{codes, Location, XsltError};
use crate::names::XSLT_NS;
use crate::receiver::Nav;
use crate::stylesheet::attributes;

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
