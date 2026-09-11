//! Schema-aware XSLT 2.0 processor built on [`xsd_schema`].
//!
//! Specification: *XSL Transformations (XSLT) Version 2.0 (Second Edition)*,
//! kept locally at `xslt20.html` in the workspace root. The design sketch
//! `XSLT2_DESIGN_SKETCH.md` (workspace root) records the architecture, the
//! roadmap and the open questions; `XSLT2_FEASIBILITY.md` is the original memo.
//!
//! Status: M0 — the conformance harness (`tests/xslts`) exists and runs the
//! W3C suite against a stub engine; the processor itself is not implemented yet.

pub mod error;

pub use error::{ErrorKind, Location, XsltError};
