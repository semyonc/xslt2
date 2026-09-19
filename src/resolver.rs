//! Where a stylesheet module, a `document()` target or an `unparsed-text()`
//! resource comes from, and how a relative reference is resolved against a base
//! URI (§5.8, §3.10.1).
//!
//! The suite hands the harness file paths and inline source text, so the crate
//! needs both a file-system resolver and an in-memory one; a host with its own
//! catalog implements [`Resolver`] instead. Everything the resolver sees is a
//! *URI string*: a `file:` URI, an absolute path, or a relative reference. The
//! helpers in this module normalize the three into one, because XSLT compares
//! base URIs for equality (`xsl:include` cycles, `document()` identity) and two
//! spellings of the same file must not become two documents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{codes, XsltError};

/// A source of stylesheet modules and documents, addressed by URI.
///
/// Implementations must be cheap to clone-share (`Arc`), because one resolver
/// serves the compiler and every transformation made from the compiled
/// stylesheet.
pub trait Resolver: std::fmt::Debug + Send + Sync {
    /// The text at `uri`, already decoded as UTF-8.
    ///
    /// `uri` is always absolute: [`resolve_uri`] has already been applied.
    fn resolve_text(&self, uri: &str) -> Result<String, String>;

    /// Whether `uri` can be read, without reading it. The default answer reads
    /// it, which is correct but not cheap.
    fn available(&self, uri: &str) -> bool {
        self.resolve_text(uri).is_ok()
    }
}

/// Reads from the local file system.
#[derive(Debug, Default, Clone)]
pub struct FileResolver;

impl Resolver for FileResolver {
    fn resolve_text(&self, uri: &str) -> Result<String, String> {
        let path = uri_to_path(uri)
            .ok_or_else(|| format!("{uri:?} is not a file URI or a path"))?;
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(decode_xml(&bytes))
    }

    fn available(&self, uri: &str) -> bool {
        uri_to_path(uri).is_some_and(|p| p.is_file())
    }
}

/// Serves text registered in advance, and falls back to `next` for anything
/// else.
///
/// The conformance harness needs this: a test case may give its source document
/// as inline `<content>` rather than as a file, and `xsl:include`/`document()`
/// inside that stylesheet must still reach the real files of the test-set
/// directory.
#[derive(Debug, Default)]
pub struct MemoryResolver {
    entries: HashMap<String, String>,
    next: Option<Arc<dyn Resolver>>,
}

impl MemoryResolver {
    /// An empty in-memory resolver that answers nothing else.
    pub fn new() -> Self {
        Self::default()
    }

    /// An in-memory resolver that falls back to `next`.
    pub fn with_fallback(next: Arc<dyn Resolver>) -> Self {
        Self { entries: HashMap::new(), next: Some(next) }
    }

    /// Registers the text served at `uri`.
    pub fn insert(&mut self, uri: impl Into<String>, text: impl Into<String>) {
        self.entries.insert(uri.into(), text.into());
    }

    /// Registers the text served at `uri`, builder-style.
    pub fn with(mut self, uri: impl Into<String>, text: impl Into<String>) -> Self {
        self.insert(uri, text);
        self
    }
}

impl Resolver for MemoryResolver {
    fn resolve_text(&self, uri: &str) -> Result<String, String> {
        if let Some(text) = self.entries.get(uri) {
            return Ok(text.clone());
        }
        match &self.next {
            Some(next) => next.resolve_text(uri),
            None => Err(format!("there is no resource at {uri:?}")),
        }
    }

    fn available(&self, uri: &str) -> bool {
        self.entries.contains_key(uri) || self.next.as_ref().is_some_and(|n| n.available(uri))
    }
}

/// The error a failed module load carries (§3.10.1).
pub(crate) fn load_failed(uri: &str, why: &str) -> XsltError {
    XsltError::new(codes::XTSE0165, format!("cannot load the stylesheet module {uri:?}: {why}"))
}

/// Decodes the bytes of an XML document into a Rust `String`.
///
/// The underlying parser reads UTF-8 only, so a document written in another
/// encoding has to be decoded first. A byte-order mark decides when there is
/// one; otherwise the `encoding` pseudo-attribute of the XML declaration does,
/// and the single-byte encodings the test corpora use (`iso-8859-1`,
/// `us-ascii`, `windows-1252`) map byte-for-byte onto the first 256 code
/// points. Anything unrecognized is decoded as UTF-8, replacing what does not
/// decode, so a document never fails to load for its encoding alone.
pub fn decode_xml(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, false);
    }
    match declared_encoding(bytes) {
        Some(encoding) => match encoding.as_str() {
            "iso-8859-1" | "latin1" | "latin-1" | "us-ascii" | "ascii" | "windows-1252"
            | "cp1252" => bytes.iter().map(|&b| b as char).collect(),
            "utf-16" | "utf-16le" => decode_utf16(bytes, true),
            "utf-16be" => decode_utf16(bytes, false),
            _ => String::from_utf8_lossy(bytes).into_owned(),
        },
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// The `encoding` pseudo-attribute of the XML declaration, lower-cased.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(256)];
    let head = String::from_utf8_lossy(head);
    let declaration = head.strip_prefix("<?xml")?;
    let end = declaration.find("?>")?;
    let declaration = &declaration[..end];
    let at = declaration.find("encoding")?;
    let rest = crate::whitespace::trim_start(&declaration[at + "encoding".len()..]);
    let rest = crate::whitespace::trim_start(rest.strip_prefix('=')?);
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(crate::whitespace::trim(&rest[..end]).to_ascii_lowercase())
}

// ── URI arithmetic ───────────────────────────────────────────────────────

/// Whether `uri` already has a scheme (`file:`, `http:`, `urn:`, …).
///
/// A single-letter "scheme" is a Windows drive letter, not a scheme.
pub fn has_scheme(uri: &str) -> bool {
    match uri.find(':') {
        Some(i) if i > 1 => uri[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')),
        _ => false,
    }
}

/// Resolves `reference` against `base`, RFC 3986-style but only as far as XSLT
/// needs: absolute references win, an empty reference is the base itself, a
/// fragment-only reference is appended, everything else is joined to the base's
/// directory and normalized (`.` and `..` removed).
pub fn resolve_uri(base: &str, reference: &str) -> String {
    if reference.is_empty() {
        return base.to_string();
    }
    if has_scheme(reference) || reference.starts_with('/') {
        return normalize_uri(reference);
    }
    if let Some(rest) = reference.strip_prefix('#') {
        let stem = base.split('#').next().unwrap_or(base);
        return format!("{stem}#{rest}");
    }
    let base = base.split('#').next().unwrap_or(base);
    let dir = match base.rfind('/') {
        Some(i) => &base[..=i],
        None => "",
    };
    normalize_uri(&format!("{dir}{reference}"))
}

/// Removes `.` and `..` segments, keeping any scheme and authority intact.
pub fn normalize_uri(uri: &str) -> String {
    let (head, path) = split_authority(uri);
    let (path, tail) = match path.find(['#', '?']) {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." => {
                // A leading `..` of a relative path has nothing to pop and must
                // be kept (`xsd-schema`'s own 0.2.0 bug, `resolver.rs`).
                match out.last() {
                    Some(&last) if last != ".." => {
                        out.pop();
                    }
                    _ if absolute => {}
                    _ => out.push(".."),
                }
            }
            "" => {}
            s => out.push(s),
        }
    }
    let trailing = if path.ends_with('/') && !out.is_empty() { "/" } else { "" };
    let body = out.join("/");
    let slash = if absolute { "/" } else { "" };
    format!("{head}{slash}{body}{trailing}{tail}")
}

/// Splits a `scheme://authority` (or `scheme:`) prefix off a URI.
fn split_authority(uri: &str) -> (&str, &str) {
    let Some(colon) = uri.find(':') else { return ("", uri) };
    if !has_scheme(uri) {
        return ("", uri);
    }
    let after = &uri[colon + 1..];
    match after.strip_prefix("//") {
        Some(rest) => {
            let end = rest.find('/').unwrap_or(rest.len());
            (&uri[..colon + 3 + end], &rest[end..])
        }
        None => (&uri[..colon + 1], after),
    }
}

/// The `file:` URI of a path, absolutized against the process's working
/// directory when necessary.
pub fn path_to_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let text = absolute.to_string_lossy().replace('\\', "/");
    let text = if text.starts_with('/') { text } else { format!("/{text}") };
    normalize_uri(&format!("file://{}", escape_path(&text)))
}

/// The local path a `file:` URI (or a bare path) names, if it names one.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let body = match uri.strip_prefix("file://") {
        Some(rest) => match rest.find('/') {
            // `file:///a/b` (empty authority) and `file://host/a` alike: the
            // path starts at the first slash.
            Some(i) => &rest[i..],
            None => return None,
        },
        None => match uri.strip_prefix("file:") {
            Some(rest) => rest,
            None if has_scheme(uri) => return None,
            None => uri,
        },
    };
    let body = body.split('#').next().unwrap_or(body);
    Some(PathBuf::from(unescape_path(body)))
}

/// Percent-escapes the characters a path may contain and a URI may not.
fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            c => out.push(c),
        }
    }
    out
}

/// Reverses [`escape_path`], and any other percent-escape.
fn unescape_path(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_references_join_the_base_directory() {
        assert_eq!(resolve_uri("file:///a/b/c.xsl", "d.xsl"), "file:///a/b/d.xsl");
        assert_eq!(resolve_uri("file:///a/b/c.xsl", "../d.xsl"), "file:///a/d.xsl");
        assert_eq!(resolve_uri("file:///a/b/c.xsl", "./e/d.xsl"), "file:///a/b/e/d.xsl");
        assert_eq!(resolve_uri("file:///a/b/c.xsl", "file:///z.xsl"), "file:///z.xsl");
        assert_eq!(resolve_uri("file:///a/b/c.xsl", ""), "file:///a/b/c.xsl");
        assert_eq!(resolve_uri("file:///a/b/c.xsl", "#x"), "file:///a/b/c.xsl#x");
    }

    #[test]
    fn a_leading_dot_dot_of_a_relative_reference_survives() {
        // The bug `xsd-schema` fixed in `resolver.rs::normalize_path`: a
        // relative `..` has nothing to pop and must not be dropped.
        assert_eq!(normalize_uri("../a/b"), "../a/b");
        assert_eq!(normalize_uri("../../a"), "../../a");
        assert_eq!(normalize_uri("/a/../../b"), "/b");
    }

    #[test]
    fn schemes_and_drive_letters_are_told_apart() {
        assert!(has_scheme("file:///a"));
        assert!(has_scheme("urn:x:y"));
        assert!(!has_scheme("c:/a/b"));
        assert!(!has_scheme("a/b"));
    }

    #[test]
    fn file_uris_round_trip_through_paths() {
        let uri = path_to_uri(Path::new("/tmp/a b/c.xml"));
        assert_eq!(uri, "file:///tmp/a%20b/c.xml");
        assert_eq!(uri_to_path(&uri), Some(PathBuf::from("/tmp/a b/c.xml")));
        assert_eq!(uri_to_path("/plain/path.xml"), Some(PathBuf::from("/plain/path.xml")));
        assert_eq!(uri_to_path("http://example.com/x"), None);
    }

    #[test]
    fn documents_in_other_encodings_still_decode() {
        // A Latin-1 document: the declaration decides, and the byte 0xE9 is é.
        let latin1 = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?>\n<a>\xe9</a>";
        // `str::trim_end` is fine here: this trims a Rust string in a test
        // assertion, not a value whose whitespace XSLT or XML defines.
        assert_eq!(decode_xml(latin1).trim_end(), "<?xml version=\"1.0\" encoding=\"iso-8859-1\"?>\n<a>\u{e9}</a>");

        // A UTF-8 BOM is removed.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"<a/>");
        assert_eq!(decode_xml(&bom), "<a/>");

        // UTF-16, little-endian, by BOM.
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "<a/>".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_xml(&utf16), "<a/>");

        // Plain UTF-8 with no declaration.
        assert_eq!(decode_xml("<a>\u{e9}</a>".as_bytes()), "<a>\u{e9}</a>");
    }

    #[test]
    fn the_memory_resolver_falls_back() {
        let mem = MemoryResolver::new().with("urn:a", "<a/>");
        assert_eq!(mem.resolve_text("urn:a").unwrap(), "<a/>");
        assert!(mem.resolve_text("urn:b").is_err());
        assert!(mem.available("urn:a") && !mem.available("urn:b"));
    }
}
