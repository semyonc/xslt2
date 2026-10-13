//! Schema-aware XSLT 2.0 processor built on [`xsd_schema`].
//!
//! Specification: *XSL Transformations (XSLT) Version 2.0 (Second Edition)*,
//! kept locally at `xslt20.html` in the workspace root. The design lives in
//! `xslt_doc/` (workspace root): `XSLT2_DESIGN.md` (the architecture),
//! `XSLT2_ROADMAP.md` (status, milestones, working method, open questions) and
//! `XSLT2_LOG.md` (history); `XSLT2_FEASIBILITY.md` is the original memo.
//!
//! Status: M1 — stylesheet loading and compilation ([`compile`],
//! [`stylesheet`], [`pattern`]), the instruction tree ([`model`]) and the
//! transformation runtime ([`runtime`]) are in place, on top of the M0b
//! architecture: [`store`] (document ownership, `doc()`) and [`receiver`] (the
//! §5.7.1 push pipeline).
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use xsd_schema::namespace::NameTable;
//! use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltFunctions};
//!
//! let names = NameTable::new();
//! let functions = XsltFunctions::new();
//! let sheet = Compiler::new(&names, &functions).compile_str(
//!     r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
//!          <xsl:template match="/"><out><xsl:value-of select="/a/@n"/></out></xsl:template>
//!        </xsl:stylesheet>"#,
//!     "urn:sheet",
//! )?;
//!
//! let arena = bumpalo::Bump::new();
//! let run = sheet.transformation(&arena, TransformOptions::default());
//! let outcome = run.transform_source("urn:in", r#"<a n="7"/>"#)?;
//! assert_eq!(
//!     sheet.serialize_primary(&outcome)?,
//!     r#"<?xml version="1.0" encoding="UTF-8"?><out>7</out>"#
//! );
//! # let _ = Arc::new(MemoryResolver::new());
//! # Ok::<(), xslt2::XsltError>(())
//! ```

pub mod collation;
pub mod compile;
pub mod error;
pub mod functions;
pub mod model;
pub mod names;
pub(crate) mod numbering;
pub mod output;
pub mod pattern;
pub mod receiver;
pub(crate) mod regex;
pub mod resolver;
pub mod runtime;
pub mod serialize;
pub mod store;
pub mod stylesheet;
pub mod whitespace;

pub use compile::Compiler;
pub use error::{ErrorKind, ErrorOrigin, Location, XsltError};
pub use functions::XsltFunctions;
pub use model::{CompiledStylesheet, ModeKey};
pub use names::QName;
pub use output::{OutputDefinition, OutputMethod};
pub use receiver::Nav;
pub use receiver::{
    Annotations, ComplexContentNormalizer, EventSerializer, Name, Receiver, ResultBuilder,
    SequenceCollector,
};
pub use resolver::{FileResolver, MemoryResolver, Resolver};
pub use runtime::{
    run_on_large_stack, Message, Outcome, SecondaryResult, TransformOptions, Transformation,
    DEFAULT_MAX_RECURSION_DEPTH, DEFAULT_STACK_SIZE, OTHER_THREAD_STACK_LIMIT,
};
pub use store::{DocId, DocumentStore, StoredDocument};
