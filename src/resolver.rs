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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| {
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
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
/// input buffer" — so `urn:a` + `../b` is `urn:b`, not `urn:../b`; and rule C
/// leaves the `/` of a removed first segment in a rootless path, so
/// `example:x/y` + `../c` is `example:/c`.
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
/// keeping its scheme, authority, query and fragment intact — the same
/// removal [`resolve_uri`] applies to a resolved path, so that a URI and the
/// resolution of `"./x"` against it agree on the spelling of their common
/// part. A leading `..` of a relative path, which has nothing to remove, is
/// kept when `uri` has no scheme; with a scheme, §5.2.4 applies as written and
/// drops it.
pub fn normalize_uri(uri: &str) -> String {
    let parts = UriParts::parse(uri);
    let path = remove_dot_segments(parts.path, Dots::of(&parts));
    UriParts {
        path: &path,
        ..parts
    }
    .recompose()
}

/// The form in which the URIs of two final result trees are compared (§19.1:
/// "[ERR XTDE1490] It is a non-recoverable dynamic error for a transformation
/// to generate two or more final result trees with the same URI").
///
/// Two steps, each of which changes the spelling of a URI but not the URI:
///
/// 1. The characters a URI may not contain — a space, `<`, `>`, `"`, `{`,
///    `}`, `|`, `\`, `^`, `` ` ``, controls and every non-ASCII character —
///    are percent-encoded (as UTF-8) the way `fn:iri-to-uri` does it. §5.8:
///    "If the URI reference is relative, then it is resolved … according to
///    the rules of \[RFC3986\], after first escaping all characters that need
///    to be escaped to make it a valid RFC3986 URI reference", which is also
///    how XLink §5.4 (cited there) maps an `href` to a URI; so `a b.xml` and
///    `a%20b.xml` are one URI.
/// 2. RFC 3986 §6.2.2, "Syntax-Based Normalization": the scheme and the host
///    in lower case (§6.2.2.1), percent-encodings with upper-case hex digits
///    (§6.2.2.1), percent-encoded unreserved characters (`ALPHA`, `DIGIT`,
///    `-`, `.`, `_`, `~`) decoded (§6.2.2.2), and `.` / `..` segments removed
///    (§6.2.2.3) — in that order, so `%2E%2E` is a `..` segment.
///
/// Nothing else: an empty segment stays (`sub//a.xml` is not `sub/a.xml`,
/// RFC 3986 keeps the two apart), the case of the path stays, and §6.2.3's
/// scheme-based rules (`file://localhost/` for `file:///`, a default port)
/// are not applied.
pub(crate) fn syntax_normalized(uri: &str) -> String {
    let escaped = xsd_schema::xpath::string_ops::iri_to_uri(uri);
    let parts = UriParts::parse(&escaped);
    let scheme = parts.scheme.map(str::to_ascii_lowercase);
    let authority = parts.authority.map(normalized_authority);
    let path = remove_dot_segments(&normalized_percent_encoding(parts.path), Dots::of(&parts));
    let query = parts.query.map(normalized_percent_encoding);
    let fragment = parts.fragment.map(normalized_percent_encoding);
    UriParts {
        scheme: scheme.as_deref(),
        authority: authority.as_deref(),
        path: &path,
        query: query.as_deref(),
        fragment: fragment.as_deref(),
    }
    .recompose()
}

/// RFC 3986 §6.2.2.1 and §6.2.2.2 for one component: every percent-encoded
/// unreserved character decoded, every other percent-encoding in upper case.
/// `text` is ASCII ([`syntax_normalized`] escapes everything else first); a
/// `%` that does not start a percent-encoding is kept as it is.
fn normalized_percent_encoding(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(byte) = percent_encoded(bytes, i) {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// The octet `%XX` at `bytes[i..]` encodes, if a percent-encoding starts there.
fn percent_encoded(bytes: &[u8], i: usize) -> Option<u8> {
    if bytes.get(i) != Some(&b'%') {
        return None;
    }
    let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u8::from_str_radix(hex, 16).ok()
}

/// RFC 3986 §6.2.2 for an authority (`[ userinfo "@" ] host [ ":" port ]`,
/// §3.2): percent-encodings normalized throughout, the host in lower case.
/// The user information is case-sensitive and keeps its case.
fn normalized_authority(authority: &str) -> String {
    let (userinfo, host_port) = match authority.rfind('@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
        None => (None, authority),
    };
    // An IP literal (`[…]`, §3.2.2) contains colons of its own.
    let port_at = match host_port.rfind(']') {
        Some(close) => host_port[close..].find(':').map(|i| close + i),
        None => host_port.rfind(':'),
    };
    let (host, port) = host_port.split_at(port_at.unwrap_or(host_port.len()));
    let mut out = String::with_capacity(authority.len());
    if let Some(userinfo) = userinfo {
        out.push_str(&normalized_percent_encoding(userinfo));
        out.push('@');
    }
    // Lower case outside the percent-encodings, whose hex digits stay upper
    // case.
    let host = normalized_percent_encoding(host);
    let bytes = host.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if percent_encoded(bytes, i).is_some() {
            out.push_str(&host[i..i + 3]);
            i += 3;
        } else {
            out.push(bytes[i].to_ascii_lowercase() as char);
            i += 1;
        }
    }
    out.push_str(port);
    out
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

/// How [`remove_dot_segments`] treats a path that does not start with `/`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dots {
    /// RFC 3986 §5.2.4 as written, for a path that belongs to a URI with a
    /// scheme: rule A drops a leading `../`, and a `..` that removes the first
    /// segment of a rootless path leaves its `/` behind (`a/../c` is `/c`).
    Rfc3986,
    /// A relative reference resolved against nothing absolute, which is
    /// outside RFC 3986: it stays relative and keeps its way up (`../a` stays
    /// `../a`, `a/../c` is `c`).
    KeepLeading,
}

impl Dots {
    /// The policy for the path of `uri`: RFC 3986's when it has a scheme.
    fn of(uri: &UriParts<'_>) -> Self {
        if uri.scheme.is_some() {
            Dots::Rfc3986
        } else {
            Dots::KeepLeading
        }
    }
}

/// RFC 3986 §5.2.4, "Remove Dot Segments", under the policy `dots`.
///
/// Under [`Dots::Rfc3986`] — the path of a URI with a scheme — it is the
/// RFC's own two-buffer algorithm, step for step ([`rfc3986_dot_segments`]).
/// Under [`Dots::KeepLeading`] — a path with no scheme anywhere, outside RFC
/// 3986 — it is the segment stack [`relative_dot_segments`], which keeps a
/// relative path relative.
fn remove_dot_segments(path: &str, dots: Dots) -> String {
    match dots {
        Dots::Rfc3986 => rfc3986_dot_segments(path),
        Dots::KeepLeading => relative_dot_segments(path),
    }
}

/// RFC 3986 §5.2.4 transcribed: an input buffer holding the path and an
/// output buffer, initially empty; "While the input buffer is not empty, loop
/// as follows":
///
/// > A. If the input buffer begins with a prefix of "../" or "./", then
/// >    remove that prefix from the input buffer; otherwise,
/// > B. if the input buffer begins with a prefix of "/./" or "/.", where "."
/// >    is a complete path segment, then replace that prefix with "/" in the
/// >    input buffer; otherwise,
/// > C. if the input buffer begins with a prefix of "/../" or "/..", where
/// >    ".." is a complete path segment, then replace that prefix with "/" in
/// >    the input buffer and remove the last segment and its preceding "/" (if
/// >    any) from the output buffer; otherwise,
/// > D. if the input buffer consists only of "." or "..", then remove that
/// >    from the input buffer; otherwise,
/// > E. move the first path segment in the input buffer to the end of the
/// >    output buffer, including the initial "/" character (if any) and any
/// >    subsequent characters up to, but not including, the next "/" character
/// >    or the end of the input buffer.
///
/// For an absolute path this is what a segment stack computes. For a
/// **rootless** one (`example:a/../c`, the path of a URI with a scheme and no
/// authority, §3.3) it is not: step E moves `a` without a slash, step C
/// replaces `/../` by `/` and removes `a`, and step E then moves `/c` — so
/// `a/../c` is `/c` and `a/..` is `/`, where a stack gives `c` and the empty
/// path.
///
/// B and C replace a prefix by its own last character, so the input buffer is
/// always a suffix of `path` — except after `/.` or `/..` at the very end,
/// where it is `/`.
fn rfc3986_dot_segments(path: &str) -> String {
    /// C's "remove the last segment and its preceding "/" (if any) from the
    /// output buffer".
    fn remove_last_segment(output: &mut String) {
        let at = output.rfind('/').unwrap_or(0);
        output.truncate(at);
    }

    let mut input = path;
    let mut output = String::with_capacity(path.len());
    while !input.is_empty() {
        if let Some(rest) = input.strip_prefix("../") {
            // A
            input = rest;
        } else if let Some(rest) = input.strip_prefix("./") {
            // A
            input = rest;
        } else if input.starts_with("/./") {
            // B: "/./" becomes "/".
            input = &input[2..];
        } else if input == "/." {
            // B
            input = "/";
        } else if input.starts_with("/../") {
            // C: "/../" becomes "/".
            input = &input[3..];
            remove_last_segment(&mut output);
        } else if input == "/.." {
            // C
            input = "/";
            remove_last_segment(&mut output);
        } else if input == "." || input == ".." {
            // D
            input = "";
        } else {
            // E
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |i| start + i);
            output.push_str(&input[..end]);
            input = &input[end..];
        }
    }
    output
}

/// Dot-segment removal for a path with no scheme anywhere ([`Dots::KeepLeading`]),
/// as a segment stack: a `.` is dropped, a `..` removes the segment before it,
/// and either one in the **last** position leaves the path ending in `/`.
/// Empty segments (`a//b`) are kept.
///
/// A `..` with nothing to remove is dropped in an absolute path (where this is
/// exactly RFC 3986 §5.2.4) and **kept** in a relative one: such a path is a
/// relative reference still to be resolved against some base, and has to
/// keep meaning what it meant — `../a` stays `../a`. For the same reason
/// `a/../c` is `c`, not the `/c` §5.2.4 makes of it under a scheme
/// ([`rfc3986_dot_segments`]): against a base directory `/d/`, `a/../c` and
/// `c` both name `/d/c`, while `/c`, an absolute-path reference, would name
/// `/c`.
fn relative_dot_segments(path: &str) -> String {
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
                _ if absolute => {}
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
        // `file:///a/b` (empty authority) and `file://host/a` alike: the
        // path starts at the first slash.
        Some(rest) => &rest[rest.find('/')?..],
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

    /// With a scheme, `normalize_uri` removes dot segments exactly as
    /// `resolve_uri` does for a resolved path (§5.2.4 rule A drops a leading
    /// `../`), so a base and `href="./x"` resolved against it agree.
    #[test]
    fn normalize_uri_follows_resolution_for_a_uri_with_a_scheme() {
        assert_eq!(
            normalize_uri("file:///o/x/../main.xml"),
            "file:///o/main.xml"
        );
        assert_eq!(normalize_uri("urn:../b"), "urn:b");
        assert_eq!(resolve_uri("urn:../b", "./b"), "urn:b");
        assert_eq!(normalize_uri("../b"), "../b");
    }

    /// RFC 3986 §5.2.4's two published traces, and the rootless paths where a
    /// segment stack and the RFC's buffers part ways (review 2, F2): step C
    /// replaces `/../` by `/` in the input buffer, so the slash before the
    /// next segment survives the removal of the first one.
    #[test]
    fn dot_segments_under_a_scheme_follow_rfc_3986_step_by_step() {
        for (path, expected) in [
            // The two examples §5.2.4 traces.
            ("/a/b/c/./../../g", "/a/g"),
            ("mid/content=5/../6", "mid/6"),
            // Rootless, the first segment removed.
            ("a/../c", "/c"),
            ("a/..", "/"),
            ("a/b/../../c", "/c"),
            ("a/../../c", "/c"),
            ("./a/../c", "/c"),
            // Rootless, the first segment kept, or a leading `../` (rule A).
            ("a/./c", "a/c"),
            ("a/b/../c", "a/c"),
            ("a/.", "a/"),
            ("../a", "a"),
            ("./../a", "a"),
            (".", ""),
            ("..", ""),
            ("", ""),
            // Not dot segments.
            (".a/..", "/"),
            ("..a/b", "..a/b"),
            ("a/.b", "a/.b"),
            // Absolute paths and empty segments.
            ("/..", "/"),
            ("/a/..", "/"),
            ("/a//../b", "/a/b"),
            ("//..", "/"),
        ] {
            assert_eq!(rfc3986_dot_segments(path), expected, "{path:?}");
            assert_eq!(
                remove_dot_segments(path, Dots::Rfc3986),
                expected,
                "{path:?}"
            );
        }
        // Every function that removes dot segments under a scheme agrees, so a
        // reported URI and its comparison key cannot part.
        assert_eq!(normalize_uri("example:a/../c"), "example:/c");
        assert_eq!(normalize_uri("example:a/.."), "example:/");
        assert_eq!(resolve_uri("file:///o/", "example:a/../c"), "example:/c");
        assert_eq!(resolve_uri("example:x/y", "../c"), "example:/c");
        assert_eq!(syntax_normalized("example:a/../c"), "example:/c");
        assert_eq!(syntax_normalized("example:a/%2E%2E/c"), "example:/c");
        assert_ne!(
            syntax_normalized("example:a/%2E%2E/c"),
            syntax_normalized("example:c")
        );
        // With no scheme anywhere the path is a relative reference and stays
        // one: `a/../c` names what `c` names against any base directory.
        assert_eq!(normalize_uri("a/../c"), "c");
        assert_eq!(resolve_uri("x/y", "../c"), "c");
        assert_eq!(syntax_normalized("a/%2E%2E/c"), "c");
        assert_eq!(normalize_uri("a/../../c"), "../c");
    }

    /// For an absolute path the segment stack and the RFC's buffers agree:
    /// every path of up to five segments drawn from `a`, `b`, `.`, `..` and
    /// the empty segment.
    #[test]
    fn dot_segments_of_an_absolute_path_agree_under_both_policies() {
        let pieces = ["a", "b", ".", "..", ""];
        let mut paths = vec![String::new()];
        for _ in 0..5 {
            let longer: Vec<String> = paths
                .iter()
                .flat_map(|path| pieces.iter().map(move |piece| format!("{path}/{piece}")))
                .collect();
            for path in &longer {
                assert_eq!(
                    relative_dot_segments(path),
                    rfc3986_dot_segments(path),
                    "{path:?}"
                );
            }
            paths = longer;
        }
    }

    /// RFC 3986 §6.2.2 plus the escaping of §5.8 / `fn:iri-to-uri`: each pair
    /// is one URI; the second list are different URIs.
    #[test]
    fn syntax_normalization_equates_spellings_of_one_uri_only() {
        for (a, b) in [
            // §6.2.2.1: scheme and host are case-insensitive.
            ("FILE:///o/a.xml", "file:///o/a.xml"),
            ("http://Example.COM/a", "http://example.com/a"),
            // §6.2.2.1: hex digits of a percent-encoding in upper case.
            ("file:///o/%c3%a9.xml", "file:///o/%C3%A9.xml"),
            // §6.2.2.2: percent-encoded unreserved characters decoded.
            ("file:///o/a%2Exml", "file:///o/a.xml"),
            ("file:///o/%7Ea%2D%5F", "file:///o/~a-_"),
            ("file:///o/%41.xml", "file:///o/A.xml"),
            // §6.2.2.3: dot segments, also percent-encoded ones.
            ("file:///o/./x/../a.xml", "file:///o/a.xml"),
            ("file:///o/x/%2E%2E/a.xml", "file:///o/a.xml"),
            // §5.8: characters a URI may not contain are escaped first.
            ("file:///o/a b.xml", "file:///o/a%20b.xml"),
            ("file:///o/\u{e9}.xml", "file:///o/%C3%A9.xml"),
            // Query, fragment, user information and port are normalized too,
            // but only the host is case-folded.
            (
                "http://U%3asr@EXAMPLE.com:8080/p?q=%7e#f%2e",
                "http://U%3Asr@example.com:8080/p?q=~#f.",
            ),
            ("http://[FE80::1]:80/a", "http://[fe80::1]:80/a"),
            // No scheme: a leading `..` stays.
            ("../a/./b", "../a/b"),
            ("", ""),
        ] {
            assert_eq!(syntax_normalized(a), syntax_normalized(b), "{a} / {b}");
        }
        for (a, b) in [
            // RFC 3986 keeps empty segments: different paths.
            ("file:///o/sub//a.xml", "file:///o/sub/a.xml"),
            // The path is case-sensitive.
            ("file:///o/A.xml", "file:///o/a.xml"),
            // Reserved characters stay encoded (`%2F` is not `/`).
            ("file:///o/a%2Fb", "file:///o/a/b"),
            // §6.2.3 (scheme-based) is not applied.
            ("file://localhost/o/a.xml", "file:///o/a.xml"),
            ("http://example.com:80/", "http://example.com/"),
            // The user information keeps its case.
            ("http://U@h/", "http://u@h/"),
        ] {
            assert_ne!(syntax_normalized(a), syntax_normalized(b), "{a} / {b}");
        }
        assert_eq!(syntax_normalized("FILE:///o/a%2exml"), "file:///o/a.xml");
        assert_eq!(syntax_normalized("a%zz%"), "a%zz%");
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
