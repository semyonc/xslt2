//! Serialization (§20 and *XSLT 2.0 and XQuery 1.0 Serialization (Second
//! Edition)*, REC-xslt-xquery-serialization-20101214, read at w3.org): the
//! `xml`, `xhtml`, `html` and `text` output methods over a final result tree.
//!
//! The pieces:
//!
//! * [`SerializationParams`] (`params.rs`) — the parameters for one result
//!   tree: an [`OutputDefinition`] resolved against the tree (the default
//!   method of §20, the per-method defaults) and checked (SEPM0004, SEPM0009,
//!   SEPM0010, SESU0007, SESU0011, SESU0013).
//! * `markup.rs` — the `xml`, `xhtml` and `html` methods, one writer with the
//!   method's rules as switches; `html.rs` holds the HTML/XHTML tables.
//! * the text method, here.
//! * [`Encoding`] (`encoding.rs`), [`CharacterMap`] (`charmap.rs`) and
//!   [`EscapingMarks`] (`marks.rs`, `disable-output-escaping`, §20.2).
//!
//! **Sequence normalization** (Serialization §2) is the identity here: an XSLT
//! 2.0 final result tree is already a document node whose children are built
//! by §5.7.1 (adjacent text merged, zero-length text dropped, no parentless
//! attribute or namespace node), which is exactly S7. Step S5 ("if the item is a
//! document node, copy its children") is that tree's own child list.
//!
//! **The result is a `String`.** Serialization §4 permits "an option that
//! allows the encoding phase to be skipped, so that the result of serialization
//! is a stream of Unicode characters"; [`serialize`] is that option, and the
//! encoding shapes what it writes — which characters become references, the XML
//! declaration's `encoding`, a leading U+FEFF for `byte-order-mark` — while
//! [`Encoding::encode`] performs the encoding phase for a host that wants
//! octets ([`serialize_to_bytes`]).

mod charmap;
mod encoding;
mod html;
mod marks;
mod markup;
mod params;

pub use charmap::CharacterMap;
pub use encoding::Encoding;
pub use marks::EscapingMarks;
pub use params::{Normalization, ResolvedMethod, SerializationParams};

use xsd_schema::navigator::DomNavigator;

use crate::error::XsltError;
use crate::output::OutputDefinition;

/// Serialization error codes (Serialization Appendix B). They are not in
/// XSLT's Appendix E, so they are not among `error::codes`; like every `SE…`
/// code they classify as `ErrorKind::Serialization`.
pub mod codes {
    /// Not well-formed output that the serializer cannot avoid (§5).
    pub const SERE0003: &str = "SERE0003";
    /// `doctype-system` or `standalone` with a tree that is not a document
    /// with one element child (§5).
    pub const SEPM0004: &str = "SEPM0004";
    /// A character the requested XML version does not permit (§5.1.1).
    pub const SERE0006: &str = "SERE0006";
    /// An unsupported encoding (§5.1.2).
    pub const SESU0007: &str = super::encoding::SESU0007;
    /// An unrepresentable character where no character reference may stand
    /// (§5.1.2, §7.4.2, §8.1.2, §9).
    pub const SERE0008: &str = "SERE0008";
    /// `omit-xml-declaration="yes"` with `standalone`, or with a version
    /// other than 1.0 and `doctype-system` (§5.1.5).
    pub const SEPM0009: &str = "SEPM0009";
    /// `undeclare-prefixes="yes"` with XML version 1.0 (§5.1.7).
    pub const SEPM0010: &str = "SEPM0010";
    /// An unsupported `normalization-form` (§5.1.8).
    pub const SESU0011: &str = "SESU0011";
    /// An unsupported XML or HTML `version` (§5.1.1, §7.4.1).
    pub const SESU0013: &str = "SESU0013";
    /// #x7F–#x9F with the HTML method (§7.3).
    pub const SERE0014: &str = "SERE0014";
    /// `>` in a processing instruction with the HTML method (§7.3).
    pub const SERE0015: &str = "SERE0015";
    /// An invalid parameter value (§3); in a stylesheet it is XTSE0020.
    pub const SEPM0016: &str = "SEPM0016";
}

/// What the serializer needs to know about how the tree was produced, beyond
/// the output definition.
#[derive(Clone, Copy, Debug, Default)]
pub struct TreeContext {
    /// §20's exception to the `xhtml` default: "if the `version` attribute of
    /// the `xsl:stylesheet` element of the principal stylesheet module has the
    /// value 1.0, and if the result tree is generated implicitly (rather than
    /// by an explicit `xsl:result-document` instruction), then the default
    /// output method in this situation is `xml`."
    pub xhtml_defaults_to_xml: bool,
}

/// Serializes the document `root` belongs to with the output definition
/// `definition` (§20), honouring `disable-output-escaping` marks (§20.2).
pub fn serialize<N: DomNavigator>(
    root: &N,
    definition: &OutputDefinition,
    context: TreeContext,
    marks: Option<&EscapingMarks>,
) -> Result<String, XsltError> {
    let mut root = root.clone();
    root.move_to_root();
    let params = SerializationParams::resolve(definition, &root, context)?;
    serialize_with(&root, &params, marks)
}

/// [`serialize`] with parameters already resolved.
pub fn serialize_with<N: DomNavigator>(
    root: &N,
    params: &SerializationParams,
    marks: Option<&EscapingMarks>,
) -> Result<String, XsltError> {
    let mut root = root.clone();
    root.move_to_root();
    let mut out = String::new();
    // Serialization §3 `byte-order-mark`: "This parameter indicates whether the
    // serialized sequence of octets is to be preceded by a Byte Order Mark …
    // If the encoding defines no Byte Order Mark … then this parameter is
    // ignored." As a character stream, the mark is U+FEFF.
    if params.byte_order_mark && params.encoding.has_byte_order_mark() {
        out.push('\u{FEFF}');
    }
    match params.method {
        ResolvedMethod::Text => text_method(&root, params, &mut out)?,
        _ => markup::write_document(&root, params, marks, &mut out)?,
    }
    Ok(out)
}

/// [`serialize`] followed by the encoding phase (Serialization §4): the octets
/// in the requested encoding.
pub fn serialize_to_bytes<N: DomNavigator>(
    root: &N,
    definition: &OutputDefinition,
    context: TreeContext,
    marks: Option<&EscapingMarks>,
) -> Result<Vec<u8>, XsltError> {
    let mut root = root.clone();
    root.move_to_root();
    let params = SerializationParams::resolve(definition, &root, context)?;
    let text = serialize_with(&root, &params, marks)?;
    params.encoding.encode(&text)
}

/// The text output method (Serialization §8).
///
/// > The Text output method serializes the instance of the data model by
/// > outputting the string value of the document node created by the markup
/// > generation step of the phases of serialization without any escaping.
///
/// Character maps apply (§8.1.10), then Unicode normalization of the unmapped
/// characters — of the whole string, not node by node (erratum E8: "Unicode
/// normalization applies to all characters that might be adjacent in the
/// serialized result produced by the text output method, including those that
/// are in text nodes that are separated by element nodes"). A character the
/// encoding cannot hold is SERE0008 (§8.1.2): the text method has no character
/// references. `disable-output-escaping` has no effect (§20.2: "The text output
/// method ignores the disable-output-escaping attribute, since it does not
/// perform any output escaping").
fn text_method<N: DomNavigator>(
    root: &N,
    params: &SerializationParams,
    out: &mut String,
) -> Result<(), XsltError> {
    let value = root.value();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| -> Result<(), XsltError> {
        let normalized = params.normalization.apply(run);
        check_representable(params.encoding, &normalized, "the text output method")?;
        out.push_str(&normalized);
        run.clear();
        Ok(())
    };
    for c in value.chars() {
        match params.character_map.get(c) {
            Some(mapped) => {
                flush(&mut run, out)?;
                check_representable(params.encoding, mapped, "a character-map string")?;
                out.push_str(mapped);
            }
            None => run.push(c),
        }
    }
    flush(&mut run, out)
}

/// SERE0008 unless every character of `text` is representable.
pub(crate) fn check_representable(
    encoding: Encoding,
    text: &str,
    context: &str,
) -> Result<(), XsltError> {
    match text.chars().find(|&c| !encoding.can_represent(c)) {
        None => Ok(()),
        Some(c) => Err(unrepresentable(c, context)),
    }
}

/// SERE0008 for a character `c` the output encoding cannot represent, found
/// where no character reference can stand in for it.
pub(crate) fn unrepresentable(c: char, context: &str) -> XsltError {
    XsltError::new(
        codes::SERE0008,
        format!(
            "U+{:04X} cannot be represented in the output encoding and occurs in {context}, where \
             a character reference is not allowed",
            c as u32
        ),
    )
}
