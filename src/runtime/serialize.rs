//! Serializing a run's result tree per the stylesheet's merged `xsl:output`
//! (§20); the serializer itself is [`crate::serialize`].

use xsd_schema::document::{BufferDocument, BufferDocumentOptions};

use crate::error::{Location, XsltError};
use crate::model::CompiledStylesheet;
use crate::output::OutputDefinition;
use crate::serialize::{EscapingMarks, TreeContext};

/// Serializes a final result tree with the output definition `definition` of
/// `sheet` (§20), honouring the tree's `disable-output-escaping` marks (§20.2).
///
/// `implicit` says whether the tree is the one the transformation created
/// implicitly (the primary result) rather than one `xsl:result-document` made,
/// which matters to §20's default method for an XHTML result of a 1.0
/// stylesheet.
pub fn serialize_result(
    sheet: &CompiledStylesheet<'_>,
    definition: &OutputDefinition,
    doc: &BufferDocument<'_>,
    marks: Option<&EscapingMarks>,
    implicit: bool,
) -> Result<String, XsltError> {
    crate::serialize::serialize(
        &doc.create_navigator(),
        definition,
        tree_context(sheet, implicit),
        marks,
    )
}

/// [`serialize_result`] followed by the encoding phase: octets in the output
/// definition's encoding.
pub fn serialize_result_to_bytes(
    sheet: &CompiledStylesheet<'_>,
    definition: &OutputDefinition,
    doc: &BufferDocument<'_>,
    marks: Option<&EscapingMarks>,
    implicit: bool,
) -> Result<Vec<u8>, XsltError> {
    crate::serialize::serialize_to_bytes(
        &doc.create_navigator(),
        definition,
        tree_context(sheet, implicit),
        marks,
    )
}

/// What §20's default method needs to know about how a result tree of `sheet`
/// was produced; `implicit` as for [`serialize_result`].
pub(crate) fn tree_context(sheet: &CompiledStylesheet<'_>, implicit: bool) -> TreeContext {
    TreeContext {
        xhtml_defaults_to_xml: implicit && sheet.principal_version_1_0,
    }
}

/// A run's default `BufferDocumentOptions` for source documents.
pub fn source_options() -> BufferDocumentOptions {
    BufferDocumentOptions::default()
}

/// Where an error without its own location is reported.
pub fn unknown_location() -> Location {
    Location {
        module_uri: None,
        line: 0,
        column: 0,
    }
}
