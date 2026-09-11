//! Error type for the XSLT 2.0 processor.
//!
//! Every error carries a specification error code — Appendix E of `xslt20.html`
//! (`XTSE…`, `XTDE…`, `XTTE…`, `XTRE…`, `XTMM9000`), or an XPath / F&O / serialization
//! code that passes through unchanged — mirroring `ValidationError.constraint` in
//! `xsd-schema`: the code is the join key between the spec, the code base and the
//! conformance suite.

pub mod codes;

use std::fmt;

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
            Some("XPST") | Some("XPDY") | Some("XPTY") | Some("XQST") | Some("XQDY") | Some("XQTY") => {
                Self::XPath
            }
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
        Self { code: code.into(), message: message.into(), location: None }
    }

    pub fn with_location(mut self, location: Location) -> Self {
        self.location = Some(location);
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
            write!(f, " (at {}:{}:{})", loc.module_uri.as_deref().unwrap_or("?"), loc.line, loc.column)?;
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
        assert!(codes::CODES.windows(2).all(|w| w[0].0 < w[1].0), "CODES must be sorted for binary search");
        assert_eq!(codes::CODES.len(), 163, "Appendix E lists 163 codes");
        assert!(codes::describe("XTSE0010").unwrap().contains("static error"));
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
    fn display_includes_location() {
        let e = XsltError::new(codes::XTSE0010, "bad")
            .with_location(Location { module_uri: Some("a.xsl".into()), line: 3, column: 7 });
        assert_eq!(e.to_string(), "XTSE0010: bad (at a.xsl:3:7)");
        assert_eq!(e.kind(), ErrorKind::Static);
    }
}
