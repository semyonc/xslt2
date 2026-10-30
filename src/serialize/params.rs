//! The serialization parameters of one result tree: an output definition
//! resolved against the tree it serializes and checked.

use unicode_normalization::UnicodeNormalization;
use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::codes::{SEPM0004, SEPM0009, SEPM0010, SEPM0016, SESU0011, SESU0013};
use super::{CharacterMap, Encoding, TreeContext};
use crate::error::XsltError;
use crate::names::QName;
use crate::output::{OutputDefinition, OutputMethod};
use crate::whitespace;

/// The XHTML namespace (§20, Serialization §6).
pub(crate) const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

/// One of the four methods Serialization specifies, after §20's default.
///
/// New variants may be added in a minor release (`#[non_exhaustive]`): a
/// `match` on it outside this crate needs a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolvedMethod {
    /// The XML output method (Serialization §5).
    Xml,
    /// The XHTML output method (Serialization §6).
    Xhtml,
    /// The HTML output method (Serialization §7).
    Html,
    /// The text output method (Serialization §8).
    Text,
}

/// The `normalization-form` values this serializer supports.
///
/// Serialization §5.1.8: "The values NFC and none MUST be supported by the
/// serializer. A serialization error \[err:SESU0011\] results if the value of
/// the normalization-form parameter specifies a normalization form that is not
/// supported by the serializer". NFD, NFKC and NFKD are supported as well;
/// `fully-normalized` is not (it needs the "relevant construct" analysis of XML
/// 1.1 §2.13), so it is SESU0011 — which §5.1.8 allows, and which `output-0193`
/// accepts beside SERE0012.
///
/// New variants may be added in a minor release (`#[non_exhaustive]`): a
/// `match` on it outside this crate needs a wildcard arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Normalization {
    /// `none`: no normalization.
    #[default]
    None,
    /// Unicode Normalization Form C.
    Nfc,
    /// Unicode Normalization Form D.
    Nfd,
    /// Unicode Normalization Form KC.
    Nfkc,
    /// Unicode Normalization Form KD.
    Nfkd,
}

impl Normalization {
    /// The form a `normalization-form` token names, or SESU0011.
    pub(crate) fn parse(token: &str) -> Result<Self, XsltError> {
        Ok(match whitespace::trim(token) {
            "none" => Normalization::None,
            "NFC" => Normalization::Nfc,
            "NFD" => Normalization::Nfd,
            "NFKC" => Normalization::Nfkc,
            "NFKD" => Normalization::Nfkd,
            other => {
                return Err(XsltError::new(
                    SESU0011,
                    format!("the normalization form {other:?} is not supported"),
                ))
            }
        })
    }

    /// `text` in this normalization form.
    pub(crate) fn apply(self, text: &str) -> String {
        match self {
            Normalization::None => text.to_string(),
            Normalization::Nfc => text.nfc().collect(),
            Normalization::Nfd => text.nfd().collect(),
            Normalization::Nfkc => text.nfkc().collect(),
            Normalization::Nfkd => text.nfkd().collect(),
        }
    }
}

/// The parameters one result tree is serialized with (Serialization §3), every
/// default filled in.
///
/// New fields may be added in a minor release (`#[non_exhaustive]`): the
/// stylesheet's output definitions make them, a program reads them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SerializationParams {
    /// `method`: the output method, after §20's default.
    pub method: ResolvedMethod,
    /// The XML version (`xml`, `xhtml`) or HTML version (`html`).
    pub version: String,
    /// `encoding`: the output encoding.
    pub encoding: Encoding,
    /// The encoding's name as the stylesheet wrote it, for the XML declaration
    /// and the content-type `meta` element.
    pub encoding_name: String,
    /// `byte-order-mark`.
    pub byte_order_mark: bool,
    /// `cdata-section-elements`; empty for the text method.
    pub cdata_section_elements: Vec<QName>,
    /// `doctype-public`; `None` when absent.
    pub doctype_public: Option<String>,
    /// `doctype-system`; `None` when absent.
    pub doctype_system: Option<String>,
    /// `escape-uri-attributes`; `false` unless the method is `html` or
    /// `xhtml`.
    pub escape_uri_attributes: bool,
    /// `include-content-type`; `false` unless the method is `html` or
    /// `xhtml`.
    pub include_content_type: bool,
    /// `indent` (§20: "yes in the case of the html and xhtml output methods,
    /// no in the case of the xml output method"); `false` for the text
    /// method.
    pub indent: bool,
    /// `media-type` (§20: by default `text/xml`, `text/html` for `html` and
    /// `xhtml`, `text/plain` for `text`).
    pub media_type: String,
    /// `normalization-form`.
    pub normalization: Normalization,
    /// `omit-xml-declaration`.
    pub omit_xml_declaration: bool,
    /// `standalone`; `None` is `omit`.
    pub standalone: Option<bool>,
    /// `undeclare-prefixes`.
    pub undeclare_prefixes: bool,
    /// The character map of `use-character-maps`, expanded (§20.1).
    pub character_map: CharacterMap,
}

impl SerializationParams {
    /// The parameters for serializing the tree at `root` with `definition`.
    ///
    /// Defaults are the ones §20 gives each attribute ("If none of the
    /// `xsl:output` declarations within an output definition specifies a value
    /// for a particular attribute, then the corresponding serialization
    /// parameter takes a default value. The default value depends on the chosen
    /// output method"), and a value for a parameter the method does not use is
    /// dropped ("If a value is supplied for an attribute that is inapplicable to
    /// the output method, its value is not passed to the serializer").
    pub(crate) fn resolve<N: DomNavigator>(
        definition: &OutputDefinition,
        root: &N,
        context: TreeContext,
    ) -> Result<Self, XsltError> {
        let method = match definition.method.get() {
            None | Some(OutputMethod::Unspecified) => default_method(root, context),
            Some(OutputMethod::Xml) => ResolvedMethod::Xml,
            Some(OutputMethod::Xhtml) => ResolvedMethod::Xhtml,
            Some(OutputMethod::Html) => ResolvedMethod::Html,
            Some(OutputMethod::Text) => ResolvedMethod::Text,
            // §20: "the expanded-QName identifies the output method; the
            // behavior in this case is not specified by this document". This
            // processor defines no method of its own, so there is nothing a
            // prefixed name can identify.
            Some(OutputMethod::Other(name)) => {
                return Err(XsltError::new(
                    SEPM0016,
                    format!(
                        "the output method {name} is not one this processor provides (it defines \
                         no implementation-defined methods)"
                    ),
                ))
            }
        };
        let markup = method != ResolvedMethod::Text;
        let xml_like = matches!(method, ResolvedMethod::Xml | ResolvedMethod::Xhtml);
        let html_like = matches!(method, ResolvedMethod::Html | ResolvedMethod::Xhtml);

        // §20: "The default value is implementation-defined, but in the case of
        // the xml and xhtml methods it must be either UTF-8 or UTF-16."
        let encoding_name = definition
            .encoding
            .get()
            .map(|e| whitespace::trim(e).to_string())
            .unwrap_or_else(|| "UTF-8".to_string());
        let encoding = Encoding::from_name(&encoding_name)?;
        let normalization = match definition.normalization_form.get() {
            Some(token) => Normalization::parse(token)?,
            None => Normalization::None,
        };
        let absent_if_empty = |s: Option<&String>| s.filter(|s| !s.is_empty()).cloned();
        let doctype_system = if markup {
            absent_if_empty(definition.doctype_system.get())
        } else {
            None
        };
        let doctype_public = if markup {
            absent_if_empty(definition.doctype_public.get())
        } else {
            None
        };
        let version = match method {
            ResolvedMethod::Xml | ResolvedMethod::Xhtml => definition
                .version
                .get()
                .map(|v| whitespace::trim(v).to_string())
                .unwrap_or_else(|| "1.0".to_string()),
            ResolvedMethod::Html => definition
                .version
                .get()
                .map(|v| whitespace::trim(v).to_string())
                .unwrap_or_else(|| "4.0".to_string()),
            ResolvedMethod::Text => String::new(),
        };
        let omit_xml_declaration = xml_like && definition.omit_xml_declaration.get_or(false);
        let standalone = if xml_like {
            definition.standalone.get().copied().flatten()
        } else {
            None
        };
        let undeclare_prefixes = xml_like && definition.undeclare_prefixes.get_or(false);

        if xml_like {
            // §5.1.5: "A serialization error [err:SEPM0009] results if the
            // omit-xml-declaration parameter has the value yes, and the
            // standalone parameter has a value other than omit; or the version
            // parameter has a value other than 1.0 and the doctype-system
            // parameter is specified."
            if omit_xml_declaration
                && (standalone.is_some() || (version != "1.0" && doctype_system.is_some()))
            {
                return Err(XsltError::new(
                    SEPM0009,
                    "omit-xml-declaration=\"yes\" cannot be combined with standalone, or with \
                     doctype-system for an XML version other than 1.0",
                ));
            }
            // §5.1.7: "If the output method is XML or XHTML, the value of the
            // undeclare-prefixes parameter is yes, and the value of the version
            // parameter is 1.0, a serialization error [err:SEPM0010] results".
            if undeclare_prefixes && version == "1.0" {
                return Err(XsltError::new(
                    SEPM0010,
                    "undeclare-prefixes=\"yes\" needs XML 1.1; namespace prefixes cannot be \
                     undeclared in XML 1.0",
                ));
            }
            // §5.1.1: "A serialization error [err:SESU0013] results if the value
            // of the version parameter specifies a version of XML that is not
            // supported by the serializer". This serializer writes XML 1.0.
            if version != "1.0" {
                return Err(XsltError::new(
                    SESU0013,
                    format!("XML version {version:?} is not supported (only 1.0 is)"),
                ));
            }
            // §5: "It is a serialization error [err:SEPM0004] to specify the
            // doctype-system parameter, or to specify the standalone parameter
            // with a value other than omit, if the instance of the data model
            // contains text nodes or multiple element nodes as children of the
            // root node. The serializer MUST either signal the error, or recover
            // …" — signalled.
            if (doctype_system.is_some() || standalone.is_some()) && !single_element(root) {
                return Err(XsltError::new(
                    SEPM0004,
                    "doctype-system or standalone needs a result with exactly one element and no \
                     text at the top level",
                ));
            }
        }
        if method == ResolvedMethod::Html && !matches!(version.as_str(), "4.0" | "4.01") {
            // §7.4.1: "If the serializer does not support the version of HTML
            // specified by this parameter, it MUST signal a serialization error
            // [err:SESU0013]."
            return Err(XsltError::new(
                SESU0013,
                format!("HTML version {version:?} is not supported (4.0 and 4.01 are)"),
            ));
        }

        let byte_order_mark = definition
            .byte_order_mark
            .get_or(encoding.default_byte_order_mark());
        Ok(Self {
            method,
            version,
            encoding,
            encoding_name,
            byte_order_mark,
            cdata_section_elements: if markup {
                definition.cdata_section_elements.clone()
            } else {
                Vec::new()
            },
            doctype_public,
            doctype_system,
            escape_uri_attributes: html_like && definition.escape_uri_attributes.get_or(true),
            include_content_type: html_like && definition.include_content_type.get_or(true),
            // §20: "The default value is yes in the case of the html and xhtml
            // output methods, no in the case of the xml output method."
            indent: markup && definition.indent.get_or(html_like),
            // §20: "The default value is text/xml in the case of the xml output
            // method, text/html in the case of the html and xhtml output
            // methods, and text/plain in the case of the text output method."
            media_type: definition.media_type.get().cloned().unwrap_or_else(|| {
                match method {
                    ResolvedMethod::Xml => "text/xml",
                    ResolvedMethod::Html | ResolvedMethod::Xhtml => "text/html",
                    ResolvedMethod::Text => "text/plain",
                }
                .to_string()
            }),
            normalization,
            omit_xml_declaration,
            standalone,
            undeclare_prefixes,
            character_map: definition.character_map.clone(),
        })
    }
}

/// The default output method (§20).
///
/// > If the document node of the final result tree has an element child, and
/// > any text nodes preceding the first element child of the document node of
/// > the result tree contain only whitespace characters, then: If the
/// > expanded-QName of this first element child has local part `html` (in lower
/// > case), and namespace URI `http://www.w3.org/1999/xhtml`, then the default
/// > output method is normally `xhtml`. However, if the version attribute of the
/// > `xsl:stylesheet` element of the principal stylesheet module has the value
/// > 1.0, and if the result tree is generated implicitly …, then the default
/// > output method in this situation is `xml`. If the expanded-QName of this
/// > first element child has local part `html` (in any combination of upper and
/// > lower case) and a null namespace URI, then the default output method is
/// > `html`. In all other cases, the default output method is `xml`.
pub(crate) fn default_method<N: DomNavigator>(root: &N, context: TreeContext) -> ResolvedMethod {
    let mut child = root.clone();
    if !child.move_to_first_child() {
        return ResolvedMethod::Xml;
    }
    loop {
        match child.node_type() {
            DomNodeType::Element => {
                let (uri, local) = (child.namespace_uri(), child.local_name());
                return if local == "html" && uri == XHTML_NS {
                    if context.xhtml_defaults_to_xml {
                        ResolvedMethod::Xml
                    } else {
                        ResolvedMethod::Xhtml
                    }
                } else if uri.is_empty() && local.eq_ignore_ascii_case("html") {
                    ResolvedMethod::Html
                } else {
                    ResolvedMethod::Xml
                };
            }
            kind if kind.is_text_like() && !whitespace::is_all_xml_whitespace(&child.value()) => {
                return ResolvedMethod::Xml;
            }
            _ => {}
        }
        if !child.move_to_next_sibling() {
            return ResolvedMethod::Xml;
        }
    }
}

/// Whether the document's top level is one element and no text — what §5's
/// SEPM0004 rule asks of a tree with a document type declaration or
/// `standalone`.
fn single_element<N: DomNavigator>(root: &N) -> bool {
    let mut elements = 0;
    let mut child = root.clone();
    if child.move_to_first_child() {
        loop {
            match child.node_type() {
                DomNodeType::Element => elements += 1,
                kind if kind.is_text_like() => return false,
                _ => {}
            }
            if !child.move_to_next_sibling() {
                break;
            }
        }
    }
    elements <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_forms_and_sesu0011() {
        assert_eq!(Normalization::parse("NFC").unwrap(), Normalization::Nfc);
        assert_eq!(Normalization::parse("none").unwrap(), Normalization::None);
        assert_eq!(Normalization::parse("ABC").unwrap_err().code, SESU0011);
        assert_eq!(
            Normalization::parse("fully-normalized").unwrap_err().code,
            SESU0011
        );
        assert_eq!(Normalization::Nfc.apply("A\u{301}"), "\u{C1}");
        assert_eq!(Normalization::Nfd.apply("\u{C1}"), "A\u{301}");
    }
}
