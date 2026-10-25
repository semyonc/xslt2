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
    /// The message always names the feature and the one sub-milestone of the
    /// roadmap (`xslt_doc/XSLT2_ROADMAP.md` §4) that owns it, e.g.
    /// `xsl:sort is not implemented (M2d)`. The conformance harness reports a
    /// case ending in this code as `not implemented` rather than as a plain
    /// failure, and totals those cases per owning sub-milestone from the
    /// trailing `(…)`.
    pub const NOT_IMPLEMENTED: &str = "XSLT2-NOTIMPL";

    /// An internal failure: a broken invariant, or an error from a lower layer
    /// (the tree builder, the serializer, the document store) that has no
    /// specification code of its own.
    pub const INTERNAL: &str = "XSLT2-INTERNAL";
}

/// The namespace of the error QNames the specifications define, and the
/// namespace of `fn:error`'s default error QName.
///
/// An error QName in this namespace adds nothing to its local name — the code
/// alone identifies it, and that is how the rest of this crate prints and
/// compares such codes — so [`XsltError`]'s `Display` leaves it out.
pub const XQT_ERRORS_NAMESPACE: &str = xsd_schema::xpath::error::XQT_ERRORS_NAMESPACE;

/// Who raised an error, when its code alone does not say.
///
/// Every classification that used to be read off the *spelling* of
/// [`XsltError::code`] consults this first. The code of an error raised by the
/// stylesheet is a name the stylesheet chose, and a stylesheet may choose
/// `XSLT2-NOTIMPL`, `XSLT2-INTERNAL` or `XTMM9000` as easily as `boom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorOrigin {
    /// The processor raised it, so its code is one this implementation chose
    /// and [`ErrorKind::of_code`] classifies it.
    Processor,
    /// The stylesheet raised it, through `fn:error`, which identifies its error
    /// by an `xs:QName` (F&O §3 — quoted from memory; the function's signatures
    /// are `fn:error()`, `fn:error($error as xs:QName)`,
    /// `fn:error($error as xs:QName?, $description as xs:string)` and the
    /// three-argument form). Such an error is a **dynamic error** by
    /// definition, whatever its QName is spelled like.
    Raised,
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
    /// Spec error code, e.g. `"XTSE0010"`; XPath codes pass through
    /// (`"XPTY0004"`). For an error raised by `fn:error` this is the **local
    /// name** of the error QName; the namespace URI is
    /// [`error_namespace`](Self::error_namespace).
    pub code: String,
    pub message: String,
    pub location: Option<Location>,
    /// Set exactly when the stylesheet raised this error through `fn:error`,
    /// and then holds the namespace URI of the error QName — the empty string
    /// for a QName in no namespace. `None` means the processor raised it.
    ///
    /// Private, because it is the discriminant [`origin`](Self::origin) reports
    /// and every classification depends on: nothing outside this module may set
    /// it by hand, or the guarantee that a stylesheet's error is never taken
    /// for a processor failure would come apart.
    error_namespace: Option<String>,
}

impl XsltError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            location: None,
            error_namespace: None,
        }
    }

    /// An error the **stylesheet** raised through `fn:error`, identified by an
    /// error QName.
    ///
    /// `namespace_uri` is the empty string for a QName in no namespace;
    /// `local_name` becomes [`code`](Self::code), so every existing caller that
    /// reads the code, and the conformance harness that compares it, keep
    /// working. What changes is that the error now also carries its namespace
    /// and its [`ErrorOrigin`], and so is classified as a dynamic error
    /// ([`ErrorKind::Dynamic`]) whatever its local name happens to be.
    pub fn raised(
        namespace_uri: impl Into<String>,
        local_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: local_name.into(),
            message: message.into(),
            location: None,
            error_namespace: Some(namespace_uri.into()),
        }
    }

    /// Who raised this error — see [`ErrorOrigin`].
    pub fn origin(&self) -> ErrorOrigin {
        match self.error_namespace {
            Some(_) => ErrorOrigin::Raised,
            None => ErrorOrigin::Processor,
        }
    }

    /// Whether the stylesheet raised this error through `fn:error`.
    pub fn is_raised(&self) -> bool {
        self.origin() == ErrorOrigin::Raised
    }

    /// The namespace URI of the error QName, for an error the stylesheet raised
    /// through `fn:error`; `Some("")` when that QName is in no namespace, and
    /// `None` for an error the processor raised itself.
    pub fn error_namespace(&self) -> Option<&str> {
        self.error_namespace.as_deref()
    }

    /// The error QName of an error the stylesheet raised, as
    /// `(namespace_uri, local_name)`; `None` for a processor error.
    pub fn error_qname(&self) -> Option<(&str, &str)> {
        Some((self.error_namespace()?, self.code.as_str()))
    }

    /// A feature this processor does not implement yet (`XSLT2-NOTIMPL`).
    ///
    /// `feature` names the construct, `milestone` the roadmap sub-milestone that
    /// owns it — `M2d`, `M3a`, `M4`, …, never a coarse `M2` or `M3` — so a
    /// harness failure reads as a plan item rather than as a bug. The message is
    /// `{feature} is not implemented ({milestone})`.
    pub fn not_implemented(feature: impl fmt::Display, milestone: &str) -> Self {
        Self::new(
            codes_internal::NOT_IMPLEMENTED,
            format!("{feature} is not implemented ({milestone})"),
        )
    }

    /// A broken invariant or a lower-layer failure with no spec code
    /// (`XSLT2-INTERNAL`).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes_internal::INTERNAL, message)
    }

    /// Whether this is the "not implemented" internal code.
    ///
    /// Only the **processor** can report a missing feature. A stylesheet that
    /// writes `error(QName('urn:x', 'x:XSLT2-NOTIMPL'), '…')` has raised an
    /// ordinary dynamic error that happens to be spelled like this crate's
    /// internal code, and treating it as a missing feature would both escape
    /// §5.5.4's recovery and mislead the conformance harness — hence the
    /// [`ErrorOrigin`] guard.
    pub fn is_not_implemented(&self) -> bool {
        !self.is_raised() && self.code == codes_internal::NOT_IMPLEMENTED
    }

    /// Whether this is the internal-failure code (`XSLT2-INTERNAL`).
    ///
    /// Guarded by [`ErrorOrigin`] for the same reason as
    /// [`is_not_implemented`](Self::is_not_implemented).
    pub fn is_internal(&self) -> bool {
        !self.is_raised() && self.code == codes_internal::INTERNAL
    }

    /// Whether this is the termination "error" of `xsl:message terminate="yes"`
    /// (`XTMM9000`), raised by the processor.
    ///
    /// `xsl:message` does not go through `fn:error` — `Instruction::Message`
    /// builds this error directly — so a stylesheet cannot reach the
    /// termination path by choosing the local name `XTMM9000` for its own error
    /// QName.
    pub fn is_termination(&self) -> bool {
        !self.is_raised() && self.code == codes::XTMM9000
    }

    /// Whether this error is a failure of the **processor** rather than of the
    /// stylesheet: the two codes of [`codes_internal`].
    ///
    /// Neither is an error the specification describes, so neither may be
    /// recovered from, defaulted away or turned into a "no" answer: a missing
    /// feature and a broken invariant are both states in which the processor
    /// does not know the right answer, and any answer it invents is silently
    /// wrong. Every site that turns an `Err` into a non-error has to let these
    /// two through.
    pub fn is_processor_failure(&self) -> bool {
        self.is_not_implemented() || self.is_internal()
    }

    /// Whether §5.5.4's recovery action may be applied to this error when it is
    /// raised while a pattern is being matched.
    ///
    /// > \[§5.5.4\] Any dynamic error or type error that occurs during the
    /// > evaluation of a pattern against a particular node is treated as a
    /// > recoverable error, even if the error would normally be
    /// > non-recoverable. The optional recovery action is to treat the pattern
    /// > as not matching.
    ///
    /// The licence is precisely scoped: "any *dynamic* error or *type* error",
    /// which is [`ErrorKind::Dynamic`], [`ErrorKind::Type`],
    /// [`ErrorKind::Recoverable`] and the XPath and F&O codes the engine raises
    /// while evaluating the predicate ([`ErrorKind::XPath`]). It is not a
    /// licence to recover from anything else that can go wrong in the same
    /// call: [`is_processor_failure`](Self::is_processor_failure) is not a
    /// dynamic error of the stylesheet at all, and neither is a termination
    /// ([`ErrorKind::Termination`]) or a serialization error. Recovering from
    /// one of those would not "treat the pattern as not matching" in any
    /// meaningful sense — it would silently select a different template rule
    /// and produce a wrong result with no diagnostic.
    ///
    /// One dynamic error is signalled rather than recovered from: a
    /// circularity (XTDE0640) that the processor detects. §2.9 makes the choice
    /// the processor's — "When a recoverable error occurs, this specification
    /// allows the processor either to signal the error … or to take a defined
    /// recovery action and continue processing. … It is implementation-defined
    /// whether the error is signaled or the recovery action is taken" — and a
    /// circularity is not a property of the one node being matched but of the
    /// definitions involved (§9.8: "A circularity is said to exist if a
    /// construct such as a global variable, an attribute set, or a key is
    /// defined in terms of itself"). Recovering would hide the defect: a key
    /// whose `match` pattern calls the key itself would silently index nothing.
    /// An error the stylesheet raises itself through `fn:error` keeps the
    /// general rule, whatever its local name.
    pub fn is_recoverable_in_pattern(&self) -> bool {
        if self.origin() == ErrorOrigin::Processor && self.code == codes::XTDE0640 {
            return false;
        }
        matches!(
            self.kind(),
            ErrorKind::Dynamic | ErrorKind::Type | ErrorKind::Recoverable | ErrorKind::XPath
        )
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

    /// The error family.
    ///
    /// An error the stylesheet raised through `fn:error` is a **dynamic error**
    /// by definition (F&O §3 — from memory: the function "raises a dynamic
    /// error"), so it is classified by its [`ErrorOrigin`] and not by the
    /// spelling of the QName the stylesheet chose.
    pub fn kind(&self) -> ErrorKind {
        match self.origin() {
            ErrorOrigin::Raised => ErrorKind::Dynamic,
            ErrorOrigin::Processor => ErrorKind::of_code(&self.code),
        }
    }

    /// The spec's own description of this code, when it is an XSLT 2.0 code.
    pub fn describe(&self) -> Option<&'static str> {
        codes::describe(&self.code)
    }
}

impl fmt::Display for XsltError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // An error QName the stylesheet chose is shown whole, in the EQName
        // form the conformance catalogues use, so that two errors with the same
        // local name in different namespaces do not read alike. A QName in no
        // namespace, and one in the namespace whose codes this crate already
        // prints bare, add nothing: those print exactly as before.
        match self.error_namespace.as_deref() {
            Some(ns) if !ns.is_empty() && ns != XQT_ERRORS_NAMESPACE => {
                write!(f, "Q{{{ns}}}{}: {}", self.code, self.message)?;
            }
            _ => write!(f, "{}: {}", self.code, self.message)?,
        }
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

impl XsltError {
    /// This error as the XPath engine carries it out of a function the
    /// processor implements: an `XPathError::Raised` identified by the error's
    /// QName — the code in the error namespace for a processor error, the
    /// stylesheet's own QName for one raised through `fn:error` — with the
    /// whole `XsltError` as its host payload.
    ///
    /// The engine never looks inside the payload and propagates the error as
    /// it is, so [`raised_error_to_xslt`] (and `From<XPathError>`) give back
    /// exactly this error — code, message, location and [`ErrorOrigin`] — on
    /// the way out.
    pub fn into_xpath_error(self) -> xsd_schema::xpath::XPathError {
        let (namespace_uri, local_name) = match self.error_qname() {
            Some((namespace_uri, local_name)) => {
                (namespace_uri.to_string(), local_name.to_string())
            }
            None => (XQT_ERRORS_NAMESPACE.to_string(), self.code.clone()),
        };
        let description = self.message.clone();
        xsd_schema::xpath::XPathError::host(&namespace_uri, &local_name, Some(&description), self)
    }
}

impl From<xsd_schema::xpath::XPathError> for XsltError {
    fn from(e: xsd_schema::xpath::XPathError) -> Self {
        // An error the processor raised inside a function call comes back as it
        // was raised; one the engine identifies by an error QName keeps that
        // identity: its namespace and its `Raised` origin, not just the local
        // name that `error_code` would report for the three QNames it knows.
        if let Some(raised) = raised_error_to_xslt(&e) {
            return raised;
        }
        // FOER0000 is F&O's "unidentified error"; used only when the engine gives no code.
        Self::new(e.error_code().unwrap_or("FOER0000"), e.to_string())
    }
}

/// An error the XPath engine identifies by an **error QName** rather than by one
/// of its own fixed codes — what `fn:error` raises.
///
/// The whole QName is kept. Its local name becomes the XSLT error code, which is
/// what §16's `error` is for: `error(QName('urn:e', 'e:boom'), 'why')` is
/// reported as `boom`, and the default QName as `FOER0000`; its namespace URI
/// becomes [`XsltError::error_namespace`], so two errors with the same local
/// name in different namespaces stay distinguishable, and the error's
/// [`ErrorOrigin`] becomes [`ErrorOrigin::Raised`], which is what makes it a
/// dynamic error regardless of how the stylesheet spelled the QName. The
/// description becomes the message.
///
/// An error that carries an `XsltError` as its host payload — one the processor
/// raised inside an XPath function call ([`XsltError::into_xpath_error`]) — is
/// that error, unchanged.
///
/// Returns `None` for every other engine error, so the caller keeps its own
/// mapping.
pub fn raised_error_to_xslt(e: &xsd_schema::xpath::XPathError) -> Option<XsltError> {
    if let Some(own) = e.downcast_host_payload::<XsltError>() {
        return Some(own.clone());
    }
    let raised = e.raised_error()?;
    Some(XsltError::raised(
        raised.namespace_uri,
        raised.local_name,
        raised.description.unwrap_or(raised.local_name),
    ))
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
        let e = XsltError::not_implemented("xsl:sort", "M2d");
        assert_eq!(e.code, codes_internal::NOT_IMPLEMENTED);
        assert_eq!(e.kind(), ErrorKind::Other);
        assert!(e.is_not_implemented());
        assert_eq!(e.message, "xsl:sort is not implemented (M2d)");
        assert!(codes::describe(codes_internal::NOT_IMPLEMENTED).is_none());
        assert!(codes::describe(codes_internal::INTERNAL).is_none());
        assert!(!XsltError::internal("boom").is_not_implemented());
        assert!(XsltError::internal("boom").is_internal());
        assert!(!e.is_internal());
    }

    /// §5.5.4 licenses recovery for "any dynamic error or type error"; the two
    /// internal codes are neither, and neither is a termination.
    #[test]
    fn only_dynamic_and_type_errors_are_recoverable_in_a_pattern() {
        for code in [
            codes::XTDE1340,
            codes::XTTE0570,
            codes::XTRE0540,
            "XPTY0004",
            "XPDY0002",
            "FORG0001",
        ] {
            let e = XsltError::new(code, "x");
            assert!(
                e.is_recoverable_in_pattern(),
                "{code} is a dynamic or type error of the stylesheet"
            );
            assert!(
                !e.is_processor_failure(),
                "{code} is not a processor failure"
            );
        }
        for e in [
            XsltError::not_implemented("format-number()", "M3d"),
            XsltError::internal("broken invariant"),
        ] {
            assert!(
                !e.is_recoverable_in_pattern(),
                "{} must not be recovered from in a pattern",
                e.code
            );
            assert!(e.is_processor_failure());
        }
        // A termination is not a dynamic error either.
        assert!(!XsltError::new(codes::XTMM9000, "stop").is_recoverable_in_pattern());
        // …but it is the stylesheet's doing, not the processor's.
        assert!(!XsltError::new(codes::XTMM9000, "stop").is_processor_failure());
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

    /// The error QName of a raised error is kept whole, and its origin — not the
    /// spelling of its local name — decides what it is.
    #[test]
    fn a_raised_error_keeps_its_whole_qname() {
        let e = XsltError::raised("urn:e", "boom", "why");
        assert_eq!(e.code, "boom");
        assert_eq!(e.message, "why");
        assert_eq!(e.error_namespace(), Some("urn:e"));
        assert_eq!(e.error_qname(), Some(("urn:e", "boom")));
        assert_eq!(e.origin(), ErrorOrigin::Raised);
        assert!(e.is_raised());

        // An error QName in no namespace is still a raised error.
        let bare = XsltError::raised("", "bare", "plain");
        assert_eq!(bare.error_namespace(), Some(""));
        assert!(bare.is_raised());

        // A processor error has no error QName.
        let processor = XsltError::new(codes::XTDE0040, "no template");
        assert_eq!(processor.error_namespace(), None);
        assert_eq!(processor.error_qname(), None);
        assert_eq!(processor.origin(), ErrorOrigin::Processor);
        assert!(!processor.is_raised());
    }

    /// An error the stylesheet raised is a dynamic error by definition, so none
    /// of the classifications may be read off the spelling of its local name.
    #[test]
    fn a_raised_error_is_a_dynamic_error_whatever_its_local_name() {
        for local in [
            "boom",
            codes_internal::NOT_IMPLEMENTED,
            codes_internal::INTERNAL,
            codes::XTMM9000,
            codes::XTSE0010,
            // Signalled inside a pattern only when the processor detects the
            // circularity (`is_recoverable_in_pattern`).
            codes::XTDE0640,
            "nonsense",
        ] {
            for ns in ["urn:e", "", XQT_ERRORS_NAMESPACE] {
                let e = XsltError::raised(ns, local, "why");
                assert_eq!(e.kind(), ErrorKind::Dynamic, "{ns}/{local}");
                assert!(e.is_recoverable_in_pattern(), "{ns}/{local}");
                assert!(!e.is_not_implemented(), "{ns}/{local}");
                assert!(!e.is_internal(), "{ns}/{local}");
                assert!(!e.is_termination(), "{ns}/{local}");
                assert!(!e.is_processor_failure(), "{ns}/{local}");
            }
        }

        // The processor's own two codes keep their meaning.
        assert!(XsltError::not_implemented("format-number()", "M3d").is_not_implemented());
        assert!(XsltError::internal("broken").is_internal());
        assert!(XsltError::new(codes::XTMM9000, "stop").is_termination());
        assert!(!XsltError::not_implemented("format-number()", "M3d").is_recoverable_in_pattern());
        // A circularity the processor detects is signalled even inside a
        // pattern (§2.9 leaves signal-or-recover to the implementation); one a
        // stylesheet raises itself with `fn:error` keeps the general rule.
        assert!(!XsltError::new(codes::XTDE0640, "circular").is_recoverable_in_pattern());
    }

    /// `Display` shows a stylesheet's own error QName whole, and leaves the
    /// output of every ordinary coded error alone.
    #[test]
    fn display_shows_a_raised_qname_but_not_the_standard_namespace() {
        assert_eq!(
            XsltError::raised("urn:e", "boom", "why").to_string(),
            "Q{urn:e}boom: why"
        );
        // A QName in no namespace adds nothing.
        assert_eq!(
            XsltError::raised("", "bare", "plain").to_string(),
            "bare: plain"
        );
        // Neither does the namespace whose codes this crate prints bare — which
        // is what keeps `fn:error`'s default QName, and the engine's own
        // QName-carried codes, reading exactly as before.
        assert_eq!(
            XsltError::raised(XQT_ERRORS_NAMESPACE, "FOER0000", "oops").to_string(),
            "FOER0000: oops"
        );
        assert_eq!(
            XsltError::new(codes::XTSE0010, "bad").to_string(),
            "XTSE0010: bad"
        );
        // The location still follows.
        assert_eq!(
            XsltError::raised("urn:e", "boom", "why")
                .with_location(Location::new("a.xsl", 3, 7))
                .to_string(),
            "Q{urn:e}boom: why (at a.xsl:3:7)"
        );
    }

    /// `raised_error_to_xslt` and `From<XPathError>` agree, and both keep the
    /// QName.
    #[test]
    fn an_engine_error_identified_by_a_qname_arrives_as_a_raised_error() {
        use xsd_schema::xpath::XPathError;

        let engine = XPathError::raised("urn:e", "boom", Some("why"));
        let via_helper = raised_error_to_xslt(&engine).expect("an error QName");
        let via_from = XsltError::from(engine);
        assert_eq!(via_helper, via_from);
        assert_eq!(via_from.error_qname(), Some(("urn:e", "boom")));
        assert_eq!(via_from.message, "why");
        assert!(via_from.is_raised());

        // An error with no description reports its local name as the message.
        let engine = XPathError::raised("", "bare", None);
        let e = XsltError::from(engine);
        assert_eq!(e.error_qname(), Some(("", "bare")));
        assert_eq!(e.message, "bare");

        // An engine error with a variant of its own is not a raised error.
        let coded = XsltError::from(XPathError::more_than_one_item());
        assert!(!coded.is_raised());
        assert_eq!(coded.error_namespace(), None);
        assert!(raised_error_to_xslt(&XPathError::more_than_one_item()).is_none());
    }

    /// An `XsltError` the processor raises inside a function call crosses the
    /// engine as the host payload of an `XPathError::Raised` and comes back
    /// whole: code, message, location and origin.
    #[test]
    fn an_xslt_error_round_trips_through_the_engine() {
        use xsd_schema::xpath::XPathError;

        // A processor error: its QName is the code in the error namespace.
        let processor = XsltError::new(codes::XTDE1360, "no context item")
            .with_location(Location::new("a.xsl", 3, 7));
        let engine = processor.clone().into_xpath_error();
        assert!(matches!(engine, XPathError::Raised(_)));
        let qname = engine.raised_error().expect("an error QName");
        assert_eq!(
            (qname.namespace_uri, qname.local_name, qname.description),
            (XQT_ERRORS_NAMESPACE, "XTDE1360", Some("no context item"))
        );
        assert_eq!(raised_error_to_xslt(&engine), Some(processor.clone()));
        let back = XsltError::from(engine);
        assert_eq!(back, processor);
        assert_eq!(back.origin(), ErrorOrigin::Processor);

        // An error the stylesheet raised keeps its own QName and origin.
        let raised = XsltError::raised("urn:e", "boom", "why");
        let engine = raised.clone().into_xpath_error();
        assert_eq!(engine.raised_error().unwrap().namespace_uri, "urn:e");
        let back = XsltError::from(engine);
        assert_eq!(back, raised);
        assert!(back.is_raised());

        // A processor failure stays one.
        let failure = XsltError::not_implemented("x", "M2k");
        assert!(XsltError::from(failure.into_xpath_error()).is_not_implemented());
    }
}
