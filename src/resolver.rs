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
        let path =
            uri_to_path(uri).ok_or_else(|| format!("{uri:?} is not a file URI or a path"))?;
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
        Self {
            entries: HashMap::new(),
            next: Some(next),
        }
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
    XsltError::new(
        codes::XTSE0165,
        format!("cannot load the stylesheet module {uri:?}: {why}"),
    )
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

/// Resolves `reference` against `base` by RFC 3986 §5.2 ("Relative
/// Resolution"): §5.2.2's transform, §5.2.3's merge and §5.2.4's
/// `remove_dot_segments`, on the five components Appendix B's parse yields.
///
/// Two consequences the earlier ad-hoc join got wrong (`base-uri-013`,
/// `-014`, `document-2003`): an absolute-path reference `/main/` keeps the
/// base's scheme and authority (§5.2.2: "if (R.path starts-with "/") then
/// T.path = remove_dot_segments(R.path) … T.authority = Base.authority"), and a
/// final `..` or `.` segment leaves the trailing slash behind (§5.2.4:
/// `/a/b/..` is `/a/`). A scheme is any `ALPHA *( ALPHA / DIGIT / "+" / "-" /
/// "." )` before the first `:` (§3.1), a one-letter one included.
///
/// A base that is itself relative (no scheme) is outside RFC 3986, which
/// requires an absolute base; it is merged the same way, and a leading `..`
/// that has nothing to remove is kept rather than dropped. Whenever the result
/// has a scheme, §5.2.4 applies as written — rule A: "If the input buffer
/// begins with a prefix of "../" or "./", then remove that prefix from the
/// input buffer" — so `urn:a` + `../b` is `urn:b`, not `urn:../b`.
pub fn resolve_uri(base: &str, reference: &str) -> String {
    let r = UriParts::parse(reference);
    let b = UriParts::parse(base);
    // RFC 3986 governs whenever the target URI has a scheme (§5.2.2 takes it
    // from the reference or the base); only with no scheme at all is the
    // relative-path behaviour kept.
    let dots = if r.scheme.is_some() || b.scheme.is_some() {
        Dots::Rfc3986
    } else {
        Dots::KeepLeading
    };
    let (scheme, authority, path, query);
    if r.scheme.is_some() {
        scheme = r.scheme;
        authority = r.authority;
        path = remove_dot_segments(r.path, dots);
        query = r.query;
    } else {
        if r.authority.is_some() {
            authority = r.authority;
            path = remove_dot_segments(r.path, dots);
            query = r.query;
        } else {
            if r.path.is_empty() {
                path = b.path.to_string();
                query = r.query.or(b.query);
            } else {
                path = if r.path.starts_with('/') {
                    remove_dot_segments(r.path, dots)
                } else {
                    remove_dot_segments(&merge_paths(&b, r.path), dots)
                };
                query = r.query;
            }
            authority = b.authority;
        }
        scheme = b.scheme;
    }
    UriParts {
        scheme,
        authority,
        path: &path,
        query,
        fragment: r.fragment,
    }
    .recompose()
}

/// Removes `.` and `..` segments from the path of `uri` (RFC 3986 §5.2.4),
/// keeping its scheme, authority, query and fragment intact. A leading `..`
/// of a relative path, which has nothing to remove, is kept.
pub fn normalize_uri(uri: &str) -> String {
    let parts = UriParts::parse(uri);
    let path = remove_dot_segments(parts.path, Dots::KeepLeading);
    UriParts {
        path: &path,
        ..parts
    }
    .recompose()
}

/// The five components of a URI reference (RFC 3986 §3, parsed as Appendix B
/// does): `scheme ":" "//" authority path "?" query "#" fragment`, each but
/// the path optional.
struct UriParts<'u> {
    scheme: Option<&'u str>,
    authority: Option<&'u str>,
    path: &'u str,
    query: Option<&'u str>,
    fragment: Option<&'u str>,
}

impl<'u> UriParts<'u> {
    fn parse(text: &'u str) -> Self {
        let (rest, fragment) = match text.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment)),
            None => (text, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, Some(query)),
            None => (rest, None),
        };
        let (scheme, rest) = match rest.find([':', '/']) {
            Some(i) if rest.as_bytes()[i] == b':' && is_scheme(&rest[..i]) => {
                (Some(&rest[..i]), &rest[i + 1..])
            }
            _ => (None, rest),
        };
        let (authority, path) = match rest.strip_prefix("//") {
            Some(after) => {
                let end = after.find('/').unwrap_or(after.len());
                (Some(&after[..end]), &after[end..])
            }
            None => (None, rest),
        };
        UriParts {
            scheme,
            authority,
            path,
            query,
            fragment,
        }
    }

    /// RFC 3986 §5.3, "Component Recomposition".
    fn recompose(&self) -> String {
        let mut out = String::new();
        if let Some(scheme) = self.scheme {
            out.push_str(scheme);
            out.push(':');
        }
        if let Some(authority) = self.authority {
            out.push_str("//");
            out.push_str(authority);
        }
        out.push_str(self.path);
        if let Some(query) = self.query {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        out
    }
}

/// The identity of the resource `uri` names, for **comparing** documents —
/// never for retrieving them.
///
/// RFC 3986 keeps empty path segments, so `file:///d//m.xsl` and
/// `file:///d/m.xsl` are different URIs (and resolution must keep them
/// apart); but a file system ignores empty segments, so for a `file:` URI the
/// two name the same file. The stylesheet loader's cycle check (§3.10.2
/// XTSE0180, §3.10.3 XTSE0210) compares modules by this identity, or a
/// self-include spelled `.//m.xsl` would load itself under an ever longer
/// name until the operating system refused the path (review R6). For a `file:`
/// URI the runs of `/` in the path are collapsed; every other URI is its own
/// identity.
pub(crate) fn resource_identity(uri: &str) -> String {
    let parts = UriParts::parse(uri);
    if !parts
        .scheme
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file"))
    {
        return uri.to_string();
    }
    let mut path = String::with_capacity(parts.path.len());
    for ch in parts.path.chars() {
        if ch == '/' && path.ends_with('/') {
            continue;
        }
        path.push(ch);
    }
    UriParts {
        path: &path,
        ..parts
    }
    .recompose()
}

/// RFC 3986 §3.1: `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
fn is_scheme(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// RFC 3986 §5.2.3, "Merge Paths".
///
/// > If the base URI has a defined authority component and an empty path,
/// > then return a string consisting of "/" concatenated with the reference's
/// > path; otherwise, return a string consisting of the reference's path
/// > component appended to all but the last segment of the base URI's path
/// > (i.e., excluding any characters after the right-most "/" in the base URI
/// > path, or excluding the entire base URI path if it does not contain any
/// > "/" characters).
fn merge_paths(base: &UriParts<'_>, reference: &str) -> String {
    if base.authority.is_some() && base.path.is_empty() {
        return format!("/{reference}");
    }
    match base.path.rfind('/') {
        Some(i) => format!("{}{reference}", &base.path[..=i]),
        None => reference.to_string(),
    }
}

/// What [`remove_dot_segments`] does with a `..` that has no segment before
/// it to remove, in a path that does not start with `/`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dots {
    /// RFC 3986 §5.2.4 as written (rule A drops a leading `../`): for a path
    /// that belongs to a URI with a scheme.
    Rfc3986,
    /// Keep it: a relative reference resolved against nothing absolute, which
    /// is outside RFC 3986, must not lose its way up (`../a` stays `../a`).
    KeepLeading,
}

/// RFC 3986 §5.2.4, "Remove Dot Segments", as a segment stack: a `.` is
/// dropped, a `..` removes the segment before it, and either one in the
/// **last** position leaves the path ending in `/`. Empty segments (`a//b`)
/// are kept. A `..` with nothing to remove is dropped in an absolute path and
/// under [`Dots::Rfc3986`], and kept under [`Dots::KeepLeading`].
fn remove_dot_segments(path: &str, dots: Dots) -> String {
    let absolute = path.starts_with('/');
    let body = if absolute { &path[1..] } else { path };
    let segments: Vec<&str> = body.split('/').collect();
    let last = segments.len() - 1;
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            "." => {}
            ".." => match out.last() {
                Some(&previous) if previous != ".." => {
                    out.pop();
                }
                _ if absolute || dots == Dots::Rfc3986 => {}
                _ => out.push(".."),
            },
            other => {
                out.push(other);
                continue;
            }
        }
        if index == last {
            out.push("");
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
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
    let text = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
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
        assert_eq!(
            resolve_uri("file:///a/b/c.xsl", "d.xsl"),
            "file:///a/b/d.xsl"
        );
        assert_eq!(
            resolve_uri("file:///a/b/c.xsl", "../d.xsl"),
            "file:///a/d.xsl"
        );
        assert_eq!(
            resolve_uri("file:///a/b/c.xsl", "./e/d.xsl"),
            "file:///a/b/e/d.xsl"
        );
        assert_eq!(
            resolve_uri("file:///a/b/c.xsl", "file:///z.xsl"),
            "file:///z.xsl"
        );
        assert_eq!(resolve_uri("file:///a/b/c.xsl", ""), "file:///a/b/c.xsl");
        assert_eq!(
            resolve_uri("file:///a/b/c.xsl", "#x"),
            "file:///a/b/c.xsl#x"
        );
    }

    /// RFC 3986 §5.4.1, "Normal Examples", against its base
    /// `http://a/b/c/d;p?q` — every row of the table.
    #[test]
    fn rfc_3986_normal_examples() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
        ] {
            assert_eq!(resolve_uri(base, reference), expected, "{reference:?}");
        }
    }

    /// RFC 3986 §5.4.2, "Abnormal Examples" (the strict-parser column).
    #[test]
    fn rfc_3986_abnormal_examples() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g#s/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
            ("http:g", "http:g"),
        ] {
            assert_eq!(resolve_uri(base, reference), expected, "{reference:?}");
        }
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
        assert_eq!(
            uri_to_path("/plain/path.xml"),
            Some(PathBuf::from("/plain/path.xml"))
        );
        assert_eq!(uri_to_path("http://example.com/x"), None);
    }

    #[test]
    fn documents_in_other_encodings_still_decode() {
        // A Latin-1 document: the declaration decides, and the byte 0xE9 is é.
        let latin1 = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?>\n<a>\xe9</a>";
        // `str::trim_end` is fine here: this trims a Rust string in a test
        // assertion, not a value whose whitespace XSLT or XML defines.
        assert_eq!(
            decode_xml(latin1).trim_end(),
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\"?>\n<a>\u{e9}</a>"
        );

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
