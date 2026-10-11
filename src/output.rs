//! `xsl:output` (§20): the merged output definitions a stylesheet declares.
//!
//! An output definition is the *stylesheet's* view of the serialization
//! parameters: every attribute an `xsl:output` declaration may carry, merged by
//! import precedence, with `cdata-section-elements` united and
//! `use-character-maps` concatenated and expanded into one character map at
//! compile time. What a serializer actually uses — the defaults that depend on
//! the method, the method chosen from the tree when none is given — is worked
//! out per result tree by [`crate::serialize::SerializationParams`].

use std::collections::BTreeMap;

use crate::error::{codes, XsltError};
use crate::names::{lexical_qname, QName};
use crate::serialize::CharacterMap;
use crate::whitespace;

/// The output method (§20, Serialization §3 `method`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputMethod {
    /// No `method` attribute: §20 chooses one from the tree being serialized.
    #[default]
    Unspecified,
    Xml,
    Xhtml,
    Html,
    Text,
    /// A prefixed QName: "the expanded-QName identifies the output method; the
    /// behavior in this case is not specified by this document" (§20). This
    /// processor implements none, so serializing with one is an error.
    Other(QName),
}

impl OutputMethod {
    /// The method's name as the spec spells it.
    pub fn name(&self) -> String {
        match self {
            OutputMethod::Unspecified | OutputMethod::Xml => "xml".into(),
            OutputMethod::Xhtml => "xhtml".into(),
            OutputMethod::Html => "html".into(),
            OutputMethod::Text => "text".into(),
            OutputMethod::Other(q) => q.to_string(),
        }
    }
}

/// One merged output definition (§20).
///
/// > For the `cdata-section-elements` attribute, the output definition uses the
/// > union of the values from all the constituent `xsl:output` declarations. For
/// > the `use-character-maps` attribute, the output definition uses the
/// > concatenation of the sequences of expanded QNames values from all the
/// > constituent `xsl:output` declarations, taking them in order of increasing
/// > import precedence, or where several have the same import precedence, in
/// > declaration order. For other attributes, the output definition uses the
/// > value of that attribute from the `xsl:output` declaration with the highest
/// > import precedence.
///
/// Each single-valued field remembers the precedence that set it (a
/// [`Setting`]), so a declaration of lower precedence cannot overwrite it
/// whatever order the module graph is walked in. A `doctype-*` attribute
/// written as the zero-length string is stored as such: "the serialization
/// parameter is not set (is "absent")", but it still overrides a value of lower
/// precedence (`output-0312`).
#[derive(Clone, Debug, Default)]
pub struct OutputDefinition {
    pub method: Setting<OutputMethod>,
    pub version: Setting<String>,
    pub encoding: Setting<String>,
    pub indent: Setting<bool>,
    pub omit_xml_declaration: Setting<bool>,
    /// `None` is `standalone="omit"`.
    pub standalone: Setting<Option<bool>>,
    pub doctype_public: Setting<String>,
    pub doctype_system: Setting<String>,
    pub media_type: Setting<String>,
    pub byte_order_mark: Setting<bool>,
    pub escape_uri_attributes: Setting<bool>,
    pub include_content_type: Setting<bool>,
    /// The token as written; checked against what the serializer supports when
    /// it is used (SESU0011).
    pub normalization_form: Setting<String>,
    pub undeclare_prefixes: Setting<bool>,
    /// The union of every declaration's `cdata-section-elements`.
    pub cdata_section_elements: Vec<QName>,
    /// `use-character-maps` of each declaration, keyed by (import precedence,
    /// declaration order) so that [`Self::character_map_names`] is in the
    /// order §20 concatenates them.
    pub use_character_maps: BTreeMap<(u32, u32), Vec<QName>>,
    /// The character map the names above expand to (§20.1), filled in once
    /// every `xsl:character-map` is known.
    pub character_map: CharacterMap,
}

/// A value plus the import precedence that set it.
#[derive(Clone, Debug)]
pub struct Setting<T> {
    value: Option<T>,
    precedence: u32,
    /// The highest import precedence at which two declarations gave this
    /// attribute different explicit values (§20, XTSE1560).
    conflict: Option<u32>,
}

impl<T> Default for Setting<T> {
    fn default() -> Self {
        Self {
            value: None,
            precedence: 0,
            conflict: None,
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

    /// Whether the winning value came from a precedence at which two
    /// declarations disagreed (§20, XTSE1560).
    ///
    /// A disagreement at a *lower* precedence is excused by the spec — "unless
    /// there is another `xsl:output` declaration within the same output
    /// definition that has higher import precedence and that specifies an
    /// explicit value for the same attribute" — which is exactly the case where
    /// `precedence` has since moved above the recorded conflict.
    pub fn conflicts(&self) -> bool {
        self.conflict == Some(self.precedence)
    }

    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    /// A copy of this setting with a value that overrides any declaration
    /// (the API override of §20: "An implementation may allow the attributes
    /// of the `xsl:output` declaration to be overridden").
    pub fn overridden(value: T) -> Self {
        Self {
            value: Some(value),
            precedence: u32::MAX,
            conflict: None,
        }
    }
}

impl<T: Copy> Setting<T> {
    pub fn get_or(&self, fallback: T) -> T {
        self.value.unwrap_or(fallback)
    }
}

impl<T: PartialEq> Setting<T> {
    /// Records `value`, remembering a disagreement with an equally-precedent
    /// declaration for §20's XTSE1560.
    pub fn set_checked(&mut self, value: T, precedence: u32) {
        if let Some(existing) = &self.value {
            if precedence == self.precedence && *existing != value {
                self.conflict = Some(precedence);
            }
        }
        self.set(value, precedence);
    }
}

impl OutputDefinition {
    /// The first serialization parameter two equally-precedent `xsl:output`
    /// declarations of this definition gave different explicit values for
    /// (§20, XTSE1560).
    ///
    /// `cdata-section-elements` and `use-character-maps` are exempt by the same
    /// rule — §20 unions them rather than overriding — and are not stored as
    /// [`Setting`]s at all, so they cannot appear.
    pub fn conflicting_attribute(&self) -> Option<&'static str> {
        [
            ("method", self.method.conflicts()),
            ("version", self.version.conflicts()),
            ("encoding", self.encoding.conflicts()),
            ("indent", self.indent.conflicts()),
            (
                "omit-xml-declaration",
                self.omit_xml_declaration.conflicts(),
            ),
            ("standalone", self.standalone.conflicts()),
            ("doctype-public", self.doctype_public.conflicts()),
            ("doctype-system", self.doctype_system.conflicts()),
            ("media-type", self.media_type.conflicts()),
            ("byte-order-mark", self.byte_order_mark.conflicts()),
            (
                "escape-uri-attributes",
                self.escape_uri_attributes.conflicts(),
            ),
            (
                "include-content-type",
                self.include_content_type.conflicts(),
            ),
            ("normalization-form", self.normalization_form.conflicts()),
            ("undeclare-prefixes", self.undeclare_prefixes.conflicts()),
        ]
        .into_iter()
        .find_map(|(name, conflicts)| conflicts.then_some(name))
    }

    /// The `use-character-maps` names of every constituent declaration, in
    /// increasing import precedence and then declaration order (§20).
    pub fn character_map_names(&self) -> impl Iterator<Item = &QName> {
        self.use_character_maps.values().flatten()
    }

    /// Adds a `cdata-section-elements` name unless it is already there (§20:
    /// "the union of the values").
    pub fn add_cdata_section_element(&mut self, name: QName) {
        if !self.cdata_section_elements.contains(&name) {
            self.cdata_section_elements.push(name);
        }
    }
}

/// Parses the `method` attribute (§20 \[ERR XTSE1570\]).
///
/// > The value must (if present) be a valid QName. If the QName does not have
/// > a prefix, then it identifies a method specified in \[XSLT and XQuery
/// > Serialization\] and must be one of `xml`, `html`, `xhtml`, or `text`. If the
/// > QName has a prefix, then the QName is expanded into an expanded-QName …
/// > the behavior in this case is not specified by this document.
///
/// `resolve` maps a prefix to its namespace URI in the declaration's scope.
pub fn parse_method(
    value: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> Result<OutputMethod, XsltError> {
    let value = whitespace::trim(value);
    let invalid = || {
        XsltError::new(
            codes::XTSE1570,
            format!(
                "the output method {value:?} is neither xml, html, xhtml, text nor a prefixed QName"
            ),
        )
    };
    let (prefix, local) = lexical_qname(value).ok_or_else(invalid)?;
    if prefix.is_empty() {
        return match local {
            "xml" => Ok(OutputMethod::Xml),
            "xhtml" => Ok(OutputMethod::Xhtml),
            "html" => Ok(OutputMethod::Html),
            "text" => Ok(OutputMethod::Text),
            _ => Err(invalid()),
        };
    }
    match resolve(prefix) {
        Some(uri) if !uri.is_empty() => Ok(OutputMethod::Other(QName::new(uri, local))),
        _ => Err(XsltError::new(
            codes::XTSE0280,
            format!("the prefix {prefix:?} of the output method {value:?} is not bound"),
        )),
    }
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
    fn equal_precedence_disagreement_is_a_conflict_unless_overridden() {
        let mut d = OutputDefinition::default();
        d.encoding.set_checked("UTF-8".into(), 1);
        d.encoding.set_checked("UTF-16".into(), 1);
        assert_eq!(d.conflicting_attribute(), Some("encoding"));
        d.encoding.set_checked("US-ASCII".into(), 2);
        assert_eq!(d.conflicting_attribute(), None);
    }

    #[test]
    fn methods_parse_as_the_four_names_or_a_prefixed_qname() {
        let none = |_: &str| None;
        assert_eq!(parse_method(" html ", none).unwrap(), OutputMethod::Html);
        assert_eq!(parse_method("text", none).unwrap(), OutputMethod::Text);
        assert_eq!(parse_method("adaptive", none).unwrap_err().code, "XTSE1570");
        assert_eq!(
            parse_method("your::xml", none).unwrap_err().code,
            "XTSE1570"
        );
        assert_eq!(
            parse_method("p:m", |_| Some("urn:m".into())).unwrap(),
            OutputMethod::Other(QName::new("urn:m", "m"))
        );
        assert_eq!(parse_method("p:m", none).unwrap_err().code, "XTSE0280");
    }

    #[test]
    fn character_map_names_concatenate_by_precedence_then_order() {
        let mut d = OutputDefinition::default();
        d.use_character_maps.insert((2, 1), vec![QName::local("b")]);
        d.use_character_maps.insert((1, 5), vec![QName::local("a")]);
        d.use_character_maps.insert((2, 0), vec![QName::local("c")]);
        let names: Vec<_> = d.character_map_names().map(|q| q.local.clone()).collect();
        assert_eq!(names, ["a", "c", "b"]);
    }
}
