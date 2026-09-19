//! Expanded names, the XSLT namespace, and the attribute spellings §3.5 allows.

use std::fmt;

use crate::error::{codes, XsltError};
use crate::whitespace;

/// The XSLT namespace.
pub const XSLT_NS: &str = "http://www.w3.org/1999/XSL/Transform";
/// The standard function namespace: where the XSLT functions of §16 live.
pub const FN_NS: &str = "http://www.w3.org/2005/xpath-functions";
/// The `xml` prefix, which is bound everywhere and never declared.
pub const XML_PREFIX: &str = "xml";
/// The XML namespace (`xml:base`, `xml:space`, `xml:lang`).
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// The XML Schema namespace.
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
/// The codepoint collation, the only one this implementation knows (§9.5).
pub const CODEPOINT_COLLATION: &str = "http://www.w3.org/2005/xpath-functions/collation/codepoint";

/// An expanded name: namespace URI (empty for none) plus local part.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QName {
    pub uri: String,
    pub local: String,
}

impl QName {
    /// A name in no namespace.
    pub fn local(local: impl Into<String>) -> Self {
        Self { uri: String::new(), local: local.into() }
    }

    /// A name in a namespace.
    pub fn new(uri: impl Into<String>, local: impl Into<String>) -> Self {
        Self { uri: uri.into(), local: local.into() }
    }

    /// Whether this is a name in the XSLT namespace.
    pub fn is_xslt(&self) -> bool {
        self.uri == XSLT_NS
    }
}

impl fmt::Display for QName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.uri.is_empty() {
            write!(f, "{}", self.local)
        } else {
            write!(f, "{{{}}}{}", self.uri, self.local)
        }
    }
}

/// Splits a lexical QName into `(prefix, local)`; the prefix is empty when
/// there is none.
pub fn split_qname(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", name),
    }
}

/// Whether `name` is a syntactically valid NCName.
///
/// The check is the practical one: a name character set wide enough for every
/// XML name the parser will have accepted anyway, since the stylesheet has
/// already been parsed as XML by the time this runs.
pub fn is_ncname(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '\u{B7}'))
}

/// Resolves a lexical QName written in an attribute value, with `resolve`
/// mapping a prefix to a namespace URI.
///
/// An unprefixed name is in **no namespace**: §5.1 says the default namespace
/// declaration does not apply to unprefixed names used as QName-valued
/// attributes (`name`, `mode`, variable names, …).
pub fn resolve_qname(
    name: &str,
    resolve: impl Fn(&str) -> Option<String>,
    code: &str,
) -> Result<QName, XsltError> {
    // XML whitespace only: a no-break space is part of the name, and a name
    // that contains one is not an NCName (§4).
    let name = whitespace::trim(name);
    let (prefix, local) = split_qname(name);
    if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
        return Err(XsltError::new(code, format!("{name:?} is not a valid QName")));
    }
    if prefix.is_empty() {
        return Ok(QName::local(local));
    }
    match resolve(prefix) {
        Some(uri) => Ok(QName::new(uri, local)),
        None => Err(XsltError::new(
            codes::XTSE0280,
            format!("the prefix {prefix:?} in {name:?} is not bound to a namespace"),
        )),
    }
}

/// `yes` / `no`, the boolean spelling §3.5 uses everywhere.
pub fn parse_yes_no(value: &str, what: &str) -> Result<bool, XsltError> {
    match whitespace::trim(value) {
        "yes" => Ok(true),
        "no" => Ok(false),
        other => Err(XsltError::new(
            codes::XTSE0020,
            format!("{what} must be \"yes\" or \"no\", not {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unprefixed_qname_attribute_is_in_no_namespace() {
        let q = resolve_qname("x", |_| Some("urn:d".into()), codes::XTSE0020).unwrap();
        assert_eq!(q, QName::local("x"));
        let q = resolve_qname("p:x", |p| (p == "p").then(|| "urn:p".to_string()), codes::XTSE0020).unwrap();
        assert_eq!(q, QName::new("urn:p", "x"));
        assert_eq!(q.to_string(), "{urn:p}x");
    }

    #[test]
    fn an_unbound_prefix_is_xtse0280() {
        let e = resolve_qname("q:x", |_| None, codes::XTSE0020).unwrap_err();
        assert_eq!(e.code, codes::XTSE0280);
    }

    #[test]
    fn yes_and_no_are_the_only_booleans() {
        assert!(parse_yes_no("yes", "x").unwrap());
        assert!(!parse_yes_no(" no ", "x").unwrap());
        assert_eq!(parse_yes_no("true", "x").unwrap_err().code, codes::XTSE0020);
    }
}
