//! Error type for the XSLT 2.0 processor.
//!
//! Every error carries a specification error code — Appendix E of `xslt20.html`
//! (`XTSE…`, `XTDE…`, `XTTE…`, `XTRE…`, `XTMM9000`), or an XPath / F&O / serialization
//! code that passes through unchanged — mirroring `ValidationError.constraint` in
//! `xsd-schema`: the code is the join key between the spec, the code base and the
//! conformance suite.

pub mod codes;

use std::fmt;

/// Codes that are **not** in Appendix E: this implementation's own.
///
/// The specification has no code for "the processor does not implement this
/// yet", and M0b used the non-existent `"XTDE0000"` as a placeholder for
/// builder and serializer failures. Both are replaced by the two constants
/// below, which are documented, greppable and classified as
/// [`ErrorKind::Other`], so the conformance harness can separate a missing
/// feature from a wrong answer.
pub mod codes_internal {
    /// A construct the processor recognizes but has not implemented yet.
    ///
    /// The message always names the feature and the milestone that owns it,
    /// e.g. `xsl:sort is not implemented in M1 (M2)`. The conformance harness
    /// reports a case ending in this code as `not implemented` rather than as a
    /// plain failure.
    pub const NOT_IMPLEMENTED: &str = "XSLT2-NOTIMPL";

    /// An internal failure: a broken invariant, or an error from a lower layer
    /// (the tree builder, the serializer, the document store) that has no
    /// specification code of its own.
    pub const INTERNAL: &str = "XSLT2-INTERNAL";
}

/// Error family, derived from the code prefix (§2.9 Error Handling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `XTSE`: static error, detected while compiling the stylesheet.
    Static,
    /// `XTDE`: non-recoverable dynamic error.
    Dynamic,
    /// `XTTE`: type error.
    Type,
    /// `XTRE`: recoverable dynamic error (recovered unless strict mode is on).
    Recoverable,
    /// `XTMM9000`: termination by `xsl:message terminate="yes"`.
    Termination,
    /// `SE…`: serialization error.
    Serialization,
    /// `XP…` / `FO…`: raised by the XPath engine or its function library.
    XPath,
    /// Anything else (host, I/O, unknown code).
    Other,
}

impl ErrorKind {
    /// Classify a code by its prefix.
    pub fn of_code(code: &str) -> Self {
        match code.get(..4) {
            Some("XTSE") => Self::Static,
            Some("XTDE") => Self::Dynamic,
            Some("XTTE") => Self::Type,
            Some("XTRE") => Self::Recoverable,
            Some("XTMM") => Self::Termination,
            Some("XPST") | Some("XPDY") | Some("XPTY") | Some("XQST") | Some("XQDY")
            | Some("XQTY") => Self::XPath,
            _ if code.starts_with("SE") => Self::Serialization,
            _ if code.starts_with("FO") => Self::XPath,
            _ => Self::Other,
        }
    }
}

/// Where an error was raised in the stylesheet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Location {
    pub module_uri: Option<String>,
    pub line: u32,
    pub column: u32,
}

impl Location {
    /// A location in a stylesheet module.
    pub fn new(module_uri: impl Into<String>, line: u32, column: u32) -> Self {
        Self {
            module_uri: Some(module_uri.into()),
            line,
            column,
        }
    }
}

/// An XSLT error: spec code + message + optional stylesheet location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsltError {
    /// Spec error code, e.g. `"XTSE0010"`; XPath codes pass through (`"XPTY0004"`).
    pub code: String,
    pub message: String,
    pub location: Option<Location>,
}

impl XsltError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            location: None,
        }
    }

    /// A feature this milestone does not implement (`XSLT2-NOTIMPL`).
    ///
    /// `feature` names the construct, `milestone` the roadmap entry that owns
    /// it, so a harness failure reads as a plan item rather than as a bug.
    pub fn not_implemented(feature: impl fmt::Display, milestone: &str) -> Self {
        Self::new(
            codes_internal::NOT_IMPLEMENTED,
            format!("{feature} is not implemented in M1 ({milestone})"),
        )
    }

    /// A broken invariant or a lower-layer failure with no spec code
    /// (`XSLT2-INTERNAL`).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes_internal::INTERNAL, message)
    }

    /// Whether this is the "not implemented" internal code.
    pub fn is_not_implemented(&self) -> bool {
        self.code == codes_internal::NOT_IMPLEMENTED
    }

    pub fn with_location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }

    /// Attaches `location` unless the error already carries one (the innermost
    /// instruction that knew where it was wins).
    pub fn or_location(mut self, location: &Location) -> Self {
        if self.location.is_none() {
            self.location = Some(location.clone());
        }
        self
    }

    pub fn kind(&self) -> ErrorKind {
        ErrorKind::of_code(&self.code)
    }

    /// The spec's own description of this code, when it is an XSLT 2.0 code.
    pub fn describe(&self) -> Option<&'static str> {
        codes::describe(&self.code)
    }
}

impl fmt::Display for XsltError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(loc) = &self.location {
            write!(
                f,
                " (at {}:{}:{})",
                loc.module_uri.as_deref().unwrap_or("?"),
                loc.line,
                loc.column
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for XsltError {}

impl From<xsd_schema::xpath::XPathError> for XsltError {
    fn from(e: xsd_schema::xpath::XPathError) -> Self {
        // FOER0000 is F&O's "unidentified error"; used only when the engine gives no code.
        Self::new(e.error_code().unwrap_or("FOER0000"), e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_table_is_sorted_and_complete() {
        assert!(
            codes::CODES.windows(2).all(|w| w[0].0 < w[1].0),
            "CODES must be sorted for binary search"
        );
        assert_eq!(codes::CODES.len(), 163, "Appendix E lists 163 codes");
        assert!(codes::describe("XTSE0010")
            .unwrap()
            .contains("static error"));
        assert!(codes::describe("XTDE0410").unwrap().contains("attribute"));
        assert!(codes::describe("XPTY0004").is_none());
    }

    #[test]
    fn kind_from_prefix() {
        assert_eq!(ErrorKind::of_code(codes::XTSE0010), ErrorKind::Static);
        assert_eq!(ErrorKind::of_code(codes::XTDE0410), ErrorKind::Dynamic);
        assert_eq!(ErrorKind::of_code(codes::XTTE0570), ErrorKind::Type);
        assert_eq!(ErrorKind::of_code(codes::XTRE0540), ErrorKind::Recoverable);
        assert_eq!(ErrorKind::of_code(codes::XTMM9000), ErrorKind::Termination);
        assert_eq!(ErrorKind::of_code("SEPM0004"), ErrorKind::Serialization);
        assert_eq!(ErrorKind::of_code("XPTY0004"), ErrorKind::XPath);
        assert_eq!(ErrorKind::of_code("FORG0001"), ErrorKind::XPath);
        assert_eq!(ErrorKind::of_code("nonsense"), ErrorKind::Other);
    }

    #[test]
    fn internal_codes_are_not_spec_codes() {
        let e = XsltError::not_implemented("xsl:sort", "M2");
        assert_eq!(e.code, codes_internal::NOT_IMPLEMENTED);
        assert_eq!(e.kind(), ErrorKind::Other);
        assert!(e.is_not_implemented());
        assert!(e.message.contains("xsl:sort") && e.message.contains("M2"));
        assert!(codes::describe(codes_internal::NOT_IMPLEMENTED).is_none());
        assert!(codes::describe(codes_internal::INTERNAL).is_none());
        assert!(!XsltError::internal("boom").is_not_implemented());
    }

    #[test]
    fn or_location_keeps_the_innermost() {
        let inner = Location::new("a.xsl", 1, 2);
        let outer = Location::new("a.xsl", 9, 9);
        let e = XsltError::internal("x")
            .with_location(inner.clone())
            .or_location(&outer);
        assert_eq!(e.location, Some(inner));
    }

    #[test]
    fn display_includes_location() {
        let e = XsltError::new(codes::XTSE0010, "bad").with_location(Location {
            module_uri: Some("a.xsl".into()),
            line: 3,
            column: 7,
        });
        assert_eq!(e.to_string(), "XTSE0010: bad (at a.xsl:3:7)");
        assert_eq!(e.kind(), ErrorKind::Static);
    }
}
