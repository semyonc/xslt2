//! Helpers shared by the integration tests (`mod common;` in each file).

/// The XML declaration `CompiledStylesheet::serialize_primary` writes for a
/// stylesheet that says nothing about it: XSLT 2.0 §20, `omit-xml-declaration`
/// — "The default value is no" — and Serialization §5.1.5, "The XML
/// declaration MUST include both version information and an encoding
/// declaration", with this processor's default encoding UTF-8.
#[allow(dead_code)]
pub const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

/// A serialized primary result as the tests written before M3a compare it:
/// without that default declaration (any other declaration is kept).
#[allow(dead_code)]
pub fn body(serialized: String) -> String {
    match serialized.strip_prefix(DECLARATION) {
        Some(rest) => rest.to_string(),
        None => serialized,
    }
}
