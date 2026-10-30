//! The output encodings this serializer supports: UTF-8, UTF-16, US-ASCII and
//! ISO-8859-1, implemented natively (no encoding library).
//!
//! Serialization §5.1.2: "Serializers are REQUIRED to support values of UTF-8
//! and UTF-16. A serialization error \[err:SESU0007\] occurs if an output
//! encoding other than UTF-8 or UTF-16 is requested and the serializer does not
//! support that encoding." The two 8-bit encodings are the ones the conformance
//! suite asks for; every character either of them cannot hold is written as a
//! character reference where XML or HTML recognizes one, and is SERE0008
//! elsewhere (the callers decide which context they are in).
//!
//! The serializer's own result is a Rust `String` — Serialization §4 allows "an
//! option that allows the encoding phase to be skipped, so that the result of
//! serialization is a stream of Unicode characters" — so what an encoding
//! changes there is which characters are escaped and what the XML declaration
//! says; [`Encoding::encode`] performs the encoding phase itself for a host that
//! wants the octets.

use crate::error::XsltError;

/// One supported output encoding.
///
/// New variants may be added in a minor release (`#[non_exhaustive]`): a
/// `match` on it outside this crate needs a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Encoding {
    /// UTF-8.
    Utf8,
    /// UTF-16 with its byte order given by the byte order mark (big-endian
    /// when there is none).
    Utf16,
    /// UTF-16BE: the byte order is part of the name, and Unicode gives it no
    /// byte order mark.
    Utf16Be,
    /// UTF-16LE: the byte order is part of the name, and Unicode gives it no
    /// byte order mark.
    Utf16Le,
    /// US-ASCII.
    UsAscii,
    /// ISO-8859-1.
    Latin1,
}

/// Serialization's "requested output encoding is not supported" (§5.1.2,
/// §7.4.2, §8.1.2).
pub(crate) const SESU0007: &str = "SESU0007";

impl Encoding {
    /// The encoding an `encoding` parameter names, compared case-insensitively
    /// (IANA charset names are case-insensitive), or SESU0007.
    ///
    /// Besides the canonical names, the IANA aliases of the four are accepted.
    pub(crate) fn from_name(name: &str) -> Result<Self, XsltError> {
        let lower = crate::whitespace::trim(name).to_ascii_lowercase();
        Ok(match lower.as_str() {
            "utf-8" | "utf8" => Encoding::Utf8,
            "utf-16" | "utf16" => Encoding::Utf16,
            "utf-16be" => Encoding::Utf16Be,
            "utf-16le" => Encoding::Utf16Le,
            "us-ascii" | "ascii" | "iso646-us" | "ansi_x3.4-1968" | "ansi_x3.4-1986"
            | "iso_646.irv:1991" | "iso-ir-6" | "us" | "ibm367" | "cp367" | "csascii" => {
                Encoding::UsAscii
            }
            "iso-8859-1" | "iso_8859-1" | "iso_8859-1:1987" | "iso-ir-100" | "latin1"
            | "latin-1" | "l1" | "ibm819" | "cp819" | "csisolatin1" => Encoding::Latin1,
            _ => {
                return Err(XsltError::new(
                    SESU0007,
                    format!(
                        "the output encoding {name:?} is not supported (supported: UTF-8, UTF-16, \
                         US-ASCII, ISO-8859-1)"
                    ),
                ))
            }
        })
    }

    /// Whether `c` can be written in this encoding as it stands.
    pub(crate) fn can_represent(self, c: char) -> bool {
        match self {
            Encoding::UsAscii => (c as u32) < 0x80,
            Encoding::Latin1 => (c as u32) < 0x100,
            Encoding::Utf8 | Encoding::Utf16 | Encoding::Utf16Be | Encoding::Utf16Le => true,
        }
    }

    /// Whether the encoding has a byte order mark the `byte-order-mark`
    /// parameter can ask for (Serialization §3: "If the encoding defines no
    /// Byte Order Mark, or if the Byte Order Mark is prohibited for the specific
    /// Unicode encoding … then this parameter is ignored").
    pub(crate) fn has_byte_order_mark(self) -> bool {
        matches!(self, Encoding::Utf8 | Encoding::Utf16)
    }

    /// The `byte-order-mark` default of §20: "If the encoding is UTF-16, the
    /// default is yes; for UTF-8 it is implementation-defined, and for all
    /// other encodings it is no." This processor's UTF-8 default is no.
    pub(crate) fn default_byte_order_mark(self) -> bool {
        self == Encoding::Utf16
    }

    /// The encoding phase (Serialization §4, step 5): the serialized characters
    /// as octets. Every character of `text` must be representable — the
    /// serializer has already turned the others into references or errors — so
    /// a character outside the encoding is an internal error.
    pub(crate) fn encode(self, text: &str) -> Result<Vec<u8>, XsltError> {
        let narrow = |limit: u32| -> Result<Vec<u8>, XsltError> {
            text.chars()
                .map(|c| {
                    if (c as u32) < limit {
                        Ok(c as u32 as u8)
                    } else {
                        Err(XsltError::internal(format!(
                            "U+{:04X} reached the encoding phase unescaped",
                            c as u32
                        )))
                    }
                })
                .collect()
        };
        Ok(match self {
            Encoding::Utf8 => text.as_bytes().to_vec(),
            Encoding::Utf16 | Encoding::Utf16Be => {
                text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
            }
            Encoding::Utf16Le => text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
            Encoding::UsAscii => narrow(0x80)?,
            Encoding::Latin1 => narrow(0x100)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive_and_unknown_ones_are_sesu0007() {
        assert_eq!(Encoding::from_name("UTF-8").unwrap(), Encoding::Utf8);
        assert_eq!(Encoding::from_name(" utf-16 ").unwrap(), Encoding::Utf16);
        assert_eq!(Encoding::from_name("US-ASCII").unwrap(), Encoding::UsAscii);
        assert_eq!(Encoding::from_name("ISO-8859-1").unwrap(), Encoding::Latin1);
        assert_eq!(Encoding::from_name("XXX-xx").unwrap_err().code, SESU0007);
        // windows-1252 is not ISO-8859-1: it cannot hold most of U+0080–U+009F
        // and holds U+20AC and 26 other characters above U+00FF instead.
        assert_eq!(
            Encoding::from_name("windows-1252").unwrap_err().code,
            SESU0007
        );
    }

    #[test]
    fn representable_ranges() {
        assert!(Encoding::UsAscii.can_represent('\u{7F}'));
        assert!(!Encoding::UsAscii.can_represent('\u{E9}'));
        assert!(Encoding::Latin1.can_represent('\u{FF}'));
        assert!(!Encoding::Latin1.can_represent('\u{20AC}'));
        assert!(Encoding::Utf16.can_represent('\u{10000}'));
    }

    #[test]
    fn the_encoding_phase_writes_octets() {
        assert_eq!(Encoding::Latin1.encode("é").unwrap(), [0xE9]);
        assert_eq!(
            Encoding::Utf16.encode("\u{FEFF}a").unwrap(),
            [0xFE, 0xFF, 0, 0x61]
        );
        assert_eq!(Encoding::Utf16Le.encode("a").unwrap(), [0x61, 0]);
        assert!(Encoding::UsAscii.encode("é").is_err());
    }
}
