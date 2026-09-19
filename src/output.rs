//! `xsl:output` (§20) and the serialization parameters M1 honours.
//!
//! M1 serializes the `xml` method with the upstream serializer
//! (`xsd_schema::document::serialize`) and the `text` method by concatenating
//! the text nodes of the result. `html` and `xhtml` are M3, and reaching them
//! raises the internal not-implemented code rather than silently producing XML.

use std::collections::HashMap;

use xsd_schema::document::serialize::SerializeOptions;

use crate::error::XsltError;
use crate::names::QName;

/// The output method (§20).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMethod {
    /// No `method` attribute: §20 chooses `xml` unless the result looks like
    /// HTML. M1 always writes XML and records the choice here.
    #[default]
    Unspecified,
    Xml,
    Xhtml,
    Html,
    Text,
}

/// One merged `xsl:output` declaration.
///
/// §20: "If there are several `xsl:output` declarations … the values of the
/// attributes are combined … an attribute specified at a higher import
/// precedence takes precedence." Each field therefore remembers the precedence
/// that set it, so a later declaration of lower precedence cannot overwrite it.
#[derive(Clone, Debug, Default)]
pub struct OutputDefinition {
    pub method: Setting<OutputMethod>,
    pub version: Setting<String>,
    pub encoding: Setting<String>,
    pub indent: Setting<bool>,
    pub omit_xml_declaration: Setting<bool>,
    pub standalone: Setting<Option<bool>>,
    pub doctype_public: Setting<String>,
    pub doctype_system: Setting<String>,
    pub media_type: Setting<String>,
}

/// A value plus the import precedence that set it.
#[derive(Clone, Debug)]
pub struct Setting<T> {
    value: Option<T>,
    precedence: u32,
}

impl<T> Default for Setting<T> {
    fn default() -> Self {
        Self {
            value: None,
            precedence: 0,
        }
    }
}

impl<T> Setting<T> {
    /// Records `value` unless a higher-precedence declaration already set one.
    pub fn set(&mut self, value: T, precedence: u32) {
        if self.value.is_none() || precedence >= self.precedence {
            self.value = Some(value);
            self.precedence = precedence;
        }
    }

    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }
}

impl<T: Copy> Setting<T> {
    pub fn get_or(&self, fallback: T) -> T {
        self.value.unwrap_or(fallback)
    }
}

impl OutputDefinition {
    /// The method actually used, with `Unspecified` resolved to `xml`.
    ///
    /// §20 lets a processor pick `html` when the first element is `html`; M1
    /// does not implement the HTML method at all, so the default is always
    /// XML, which is also what backwards-compatible behaviour asks for
    /// (§9.10 row 11).
    pub fn effective_method(&self) -> OutputMethod {
        match self.method.get().copied().unwrap_or_default() {
            OutputMethod::Unspecified => OutputMethod::Xml,
            other => other,
        }
    }

    /// The highest character the declared encoding can write directly, or
    /// `None` when every character can be written as it stands.
    pub fn character_escaping(&self) -> Result<Option<char>, XsltError> {
        let Some(encoding) = self.encoding.get() else {
            return Ok(None);
        };
        match encoding_limit(encoding) {
            Some(limit) => Ok(limit),
            None => Err(XsltError::not_implemented(
                format!("the output encoding {encoding:?}"),
                "M3",
            )),
        }
    }

    /// The upstream serializer options this definition maps to.
    ///
    /// The default is compact output: most of the conformance suite's
    /// assertions re-parse the result and compare trees, and any indentation
    /// this layer invents would show up as extra text nodes. `indent="yes"`
    /// asks for two spaces per level, which is what the upstream serializer's
    /// `indent: Some(width)` produces.
    pub fn serialize_options(&self) -> Result<SerializeOptions, XsltError> {
        match self.effective_method() {
            OutputMethod::Xml => {}
            OutputMethod::Text => {}
            method => {
                return Err(XsltError::not_implemented(
                    format!("the {} output method", method_name(method)),
                    "M3",
                ))
            }
        }
        // Rejects an encoding M1 cannot honour at all; the escaping it implies
        // is applied by the caller, after serialization.
        self.character_escaping()?;
        if self.doctype_public.is_set() || self.doctype_system.is_set() {
            return Err(XsltError::not_implemented("xsl:output/@doctype-*", "M3"));
        }
        Ok(SerializeOptions {
            xml_declaration: !self.omit_xml_declaration.get_or(true),
            standalone: self.standalone.get().copied().flatten(),
            indent: if self.indent.get_or(false) {
                Some(2)
            } else {
                None
            },
        })
    }
}

/// The highest code point the declared `encoding` can hold directly; anything
/// above it is written as a numeric character reference.
///
/// §20: "the encoding … specifies the preferred encoding to use for
/// outputting the result tree". M1 writes UTF-8 bytes whatever the encoding
/// says, so all it can honour is the *escaping* an encoding forces: a
/// character the encoding cannot represent must appear as a character
/// reference. `None` means nothing needs escaping.
pub fn encoding_limit(encoding: &str) -> Option<Option<char>> {
    match crate::whitespace::trim(encoding)
        .to_ascii_lowercase()
        .as_str()
    {
        "utf-8" | "utf8" | "utf-16" | "utf16" | "utf-16le" | "utf-16be" => Some(None),
        "us-ascii" | "ascii" => Some(Some('\u{7F}')),
        "iso-8859-1" | "latin1" | "latin-1" => Some(Some('\u{FF}')),
        "windows-1252" | "cp1252" => Some(Some('\u{FF}')),
        _ => None,
    }
}

fn method_name(method: OutputMethod) -> &'static str {
    match method {
        OutputMethod::Unspecified | OutputMethod::Xml => "xml",
        OutputMethod::Xhtml => "xhtml",
        OutputMethod::Html => "html",
        OutputMethod::Text => "text",
    }
}

/// Parses the `method` attribute.
pub fn parse_method(value: &str) -> Result<OutputMethod, XsltError> {
    match crate::whitespace::trim(value) {
        "xml" => Ok(OutputMethod::Xml),
        "xhtml" => Ok(OutputMethod::Xhtml),
        "html" => Ok(OutputMethod::Html),
        "text" => Ok(OutputMethod::Text),
        // §20: the value may be a QName with a prefix, naming an
        // implementation-defined method. We implement none.
        other => Err(XsltError::not_implemented(
            format!("the output method {other:?}"),
            "M3",
        )),
    }
}

/// Every `xsl:output` declaration of a stylesheet, unnamed plus named.
#[derive(Default)]
pub struct OutputDefinitions {
    pub unnamed: OutputDefinition,
    pub named: HashMap<QName, OutputDefinition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_precedence_declaration_wins_whatever_the_order() {
        let mut d = OutputDefinition::default();
        d.indent.set(true, 3);
        d.indent.set(false, 1);
        assert_eq!(d.indent.get(), Some(&true));
        d.indent.set(false, 5);
        assert_eq!(d.indent.get(), Some(&false));
    }

    #[test]
    fn the_default_is_compact_xml_without_a_declaration() {
        let d = OutputDefinition::default();
        assert_eq!(d.effective_method(), OutputMethod::Xml);
        let options = d.serialize_options().unwrap();
        assert!(!options.xml_declaration);
        assert_eq!(options.indent, None);
    }

    #[test]
    fn indent_yes_asks_the_upstream_serializer_for_two_spaces() {
        let mut d = OutputDefinition::default();
        d.indent.set(true, 0);
        assert_eq!(d.serialize_options().unwrap().indent, Some(2));
    }

    #[test]
    fn an_encoding_decides_only_what_has_to_be_escaped() {
        let mut d = OutputDefinition::default();
        assert_eq!(d.character_escaping().unwrap(), None);
        d.encoding.set("US-ASCII".to_string(), 0);
        assert_eq!(d.character_escaping().unwrap(), Some('\u{7F}'));
        d.encoding.set("utf-8".to_string(), 1);
        assert_eq!(d.character_escaping().unwrap(), None);
        d.encoding.set("EBCDIC".to_string(), 2);
        assert!(d.character_escaping().unwrap_err().is_not_implemented());
    }

    #[test]
    fn html_and_xhtml_are_not_implemented_in_m1() {
        let mut d = OutputDefinition::default();
        d.method.set(OutputMethod::Html, 0);
        assert!(d.serialize_options().unwrap_err().is_not_implemented());
    }
}
