//! Schema-aware XSLT 2.0 processor built on [`xsd_schema`].
//!
//! Specification: *XSL Transformations (XSLT) Version 2.0 (Second Edition)*,
//! kept locally at `xslt20.html` in the workspace root. The design sketch
//! `XSLT2_DESIGN_SKETCH.md` (workspace root) records the architecture, the
//! roadmap and the open questions; `XSLT2_FEASIBILITY.md` is the original memo.
//!
//! Status: M0b — the conformance harness (`tests/xslts`) runs the W3C suite
//! against a stub engine, and the architecture prototype of §14 is in place:
//! [`store`] (document ownership, `doc()`), [`receiver`] (the §5.7.1 push
//! pipeline) and [`proto`] (frames, a stand-in compiled stylesheet, pattern
//! predicates). The instruction set itself is M1.

pub mod error;
pub mod proto;
pub mod receiver;
pub mod store;

pub use error::{ErrorKind, Location, XsltError};
pub use receiver::{
    Annotations, ComplexContentNormalizer, EventSerializer, Name, Receiver, ResultBuilder,
    SequenceCollector,
};
pub use store::{DocId, DocumentStore, ProtoEvaluator, ProtoFunctions, StoredDocument, PROTO_NS};
