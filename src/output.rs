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
use crate::names::{lexical_qname, parse_yes_no, resolve_qname, split_qname, QName};
use crate::whitespace;

pub use crate::serialize::{
    CharacterMap, Encoding, Normalization, ResolvedMethod, SerializationParams,
};

/// The output method (§20, Serialization §3 `method`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputMethod {
    /// No `method` attribute: §20 chooses one from the tree being serialized.
    #[default]
    Unspecified,
    /// `method="xml"`.
    Xml,
    /// `method="xhtml"`.
    Xhtml,
    /// `method="html"`.
    Html,
    /// `method="text"`.
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
    /// `method`.
    pub method: Setting<OutputMethod>,
    /// `version`: the XML version (`xml`, `xhtml`) or HTML version (`html`).
    pub version: Setting<String>,
    /// `encoding`, as written.
    pub encoding: Setting<String>,
    /// `indent`.
    pub indent: Setting<bool>,
    /// `omit-xml-declaration`.
    pub omit_xml_declaration: Setting<bool>,
    /// `standalone`; `None` is `standalone="omit"`.
    pub standalone: Setting<Option<bool>>,
    /// `doctype-public`.
    pub doctype_public: Setting<String>,
    /// `doctype-system`.
    pub doctype_system: Setting<String>,
    /// `media-type`.
    pub media_type: Setting<String>,
    /// `byte-order-mark`.
    pub byte_order_mark: Setting<bool>,
    /// `escape-uri-attributes`.
    pub escape_uri_attributes: Setting<bool>,
    /// `include-content-type`.
    pub include_content_type: Setting<bool>,
    /// `normalization-form`: the token as written; checked against what the
    /// serializer supports when it is used (SESU0011).
    pub normalization_form: Setting<String>,
    /// `undeclare-prefixes`.
    pub undeclare_prefixes: Setting<bool>,
    /// The union of every declaration's `cdata-section-elements`.
    pub cdata_section_elements: Vec<QName>,
    /// `use-character-maps` of each declaration, keyed by (import precedence,
    /// declaration order) so that [`Self::character_map_names`] is in the
    /// order §20 concatenates them.
    pub(crate) use_character_maps: BTreeMap<(u32, u32), Vec<QName>>,
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
    pub(crate) fn set(&mut self, value: T, precedence: u32) {
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
    pub(crate) fn conflicts(&self) -> bool {
        self.conflict == Some(self.precedence)
    }

    /// The value, if a declaration (or the processor) set one.
    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Whether a value is set.
    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    /// A copy of this setting with a value that overrides any declaration
    /// (the API override of §20: "An implementation may allow the attributes
    /// of the `xsl:output` declaration to be overridden").
    pub(crate) fn overridden(value: T) -> Self {
        Self {
            value: Some(value),
            precedence: u32::MAX,
            conflict: None,
        }
    }
}

impl<T: Copy> Setting<T> {
    /// The value, or `fallback` when none is set.
    pub fn get_or(&self, fallback: T) -> T {
        self.value.unwrap_or(fallback)
    }
}

impl<T: PartialEq> Setting<T> {
    /// Records `value`, remembering a disagreement with an equally-precedent
    /// declaration for §20's XTSE1560.
    pub(crate) fn set_checked(&mut self, value: T, precedence: u32) {
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
    pub(crate) fn conflicting_attribute(&self) -> Option<&'static str> {
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
    pub(crate) fn add_cdata_section_element(&mut self, name: QName) {
        if !self.cdata_section_elements.contains(&name) {
            self.cdata_section_elements.push(name);
        }
    }
}

/// How the value of a serialization attribute joins an output definition.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Merge {
    /// An `xsl:output` declaration at this import precedence (§20): the
    /// declaration of highest precedence wins, and two of the same precedence
    /// that disagree are remembered for XTSE1560.
    Declaration(u32),
    /// An attribute of `xsl:result-document` (§19.1): "the effective value of
    /// an attribute actually present on this instruction takes precedence over
    /// the value defined in the selected output definition".
    Override,
}

fn put<T: PartialEq>(setting: &mut Setting<T>, value: T, merge: Merge) {
    match merge {
        Merge::Declaration(precedence) => setting.set_checked(value, precedence),
        Merge::Override => *setting = Setting::overridden(value),
    }
}

/// Parses one serialization attribute, named as `xsl:output` names it, and
/// records its value in `target` (§20, §19.1).
///
/// `resolve` maps a prefix to its namespace URI in the scope of the element
/// the attribute is on (`method`, `cdata-section-elements`). Returns `false`
/// for a name that is not one of these attributes — `name`,
/// `use-character-maps` and the standard attributes are the caller's — and
/// errors with the codes a *written* value takes: XTSE0020 for a value outside
/// the attribute's domain (§2.2), XTSE1570 / XTSE0280 for a bad `method`,
/// XTSE0280 for an unbound prefix. A caller evaluating an attribute value
/// template maps them to their dynamic counterparts.
///
/// `cdata-section-elements` is united with what `target` already holds,
/// whatever `merge` says: §20 unites the declarations of a definition, and
/// §19.1 unites the instruction's names with the definition's ("the value of
/// the serialization parameter is the union of the expanded names of the
/// elements named in this instruction and the elements named in the selected
/// output definition").
pub(crate) fn apply_serialization_attribute(
    target: &mut OutputDefinition,
    name: &str,
    value: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
    merge: Merge,
) -> Result<bool, XsltError> {
    match name {
        "method" => put(&mut target.method, parse_method(value, resolve)?, merge),
        "version" => put(&mut target.version, nmtoken(value, "version")?, merge),
        "encoding" => put(&mut target.encoding, encoding_name(value)?, merge),
        "indent" => put(&mut target.indent, parse_yes_no(value, "indent")?, merge),
        "omit-xml-declaration" => put(
            &mut target.omit_xml_declaration,
            parse_yes_no(value, "omit-xml-declaration")?,
            merge,
        ),
        "standalone" => {
            let value = match whitespace::trim(value) {
                "omit" => None,
                other => Some(parse_yes_no(other, "standalone")?),
            };
            put(&mut target.standalone, value, merge);
        }
        "doctype-public" => put(&mut target.doctype_public, doctype_public(value)?, merge),
        "doctype-system" => put(&mut target.doctype_system, doctype_system(value)?, merge),
        "media-type" => put(&mut target.media_type, value.to_string(), merge),
        "byte-order-mark" => put(
            &mut target.byte_order_mark,
            parse_yes_no(value, "byte-order-mark")?,
            merge,
        ),
        "escape-uri-attributes" => put(
            &mut target.escape_uri_attributes,
            parse_yes_no(value, "escape-uri-attributes")?,
            merge,
        ),
        "include-content-type" => put(
            &mut target.include_content_type,
            parse_yes_no(value, "include-content-type")?,
            merge,
        ),
        "undeclare-prefixes" => put(
            &mut target.undeclare_prefixes,
            parse_yes_no(value, "undeclare-prefixes")?,
            merge,
        ),
        "normalization-form" => put(
            &mut target.normalization_form,
            nmtoken(value, "normalization-form")?,
            merge,
        ),
        // §20: "In the case of an unprefixed name, the default namespace (that
        // is, the namespace declared using xmlns="uri") is used. Note: This
        // differs from the rule for most other QNames used in a stylesheet."
        // §19.1 says the same of `xsl:result-document`.
        "cdata-section-elements" => {
            for token in whitespace::tokens(value) {
                let (prefix, local) = split_qname(token);
                let name = if prefix.is_empty() {
                    let local = resolve_qname(local, |_| None, codes::XTSE0020)?.local;
                    QName::new(resolve("").unwrap_or_default(), local)
                } else {
                    resolve_qname(token, resolve, codes::XTSE0020)?
                };
                target.add_cdata_section_element(name);
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
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
pub(crate) fn parse_method(
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

/// An `xs:NMTOKEN` attribute value (`version`, `normalization-form`), else
/// XTSE0020 — the stylesheet form of Serialization's SEPM0016 ("a parameter
/// value is invalid for the given parameter"; §3: "It is the responsibility of
/// the host language to specify how invalid values should be handled").
fn nmtoken(value: &str, what: &str) -> Result<String, XsltError> {
    let value = whitespace::trim(value);
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '\u{B7}'));
    if ok {
        Ok(value.to_string())
    } else {
        Err(XsltError::new(
            codes::XTSE0020,
            format!("{what} must be an NMTOKEN, not {value:?}"),
        ))
    }
}

/// Serialization §3 `encoding`: "A string of Unicode characters in the range
/// #x21 to #x7E (that is, printable ASCII characters)". Whether the name is one
/// this serializer supports is decided when it serializes (SESU0007).
fn encoding_name(value: &str) -> Result<String, XsltError> {
    let value = whitespace::trim(value);
    if !value.is_empty() && value.chars().all(|c| ('\u{21}'..='\u{7E}').contains(&c)) {
        Ok(value.to_string())
    } else {
        Err(XsltError::new(
            codes::XTSE0020,
            format!("the encoding {value:?} is not a string of printable ASCII characters"),
        ))
    }
}

/// §20: "The value of doctype-public must conform to the rules for a
/// PubidLiteral (see \[XML 1.0\])" — `PubidChar ::= #x20 | #xD | #xA |
/// [a-zA-Z0-9] | [-'()+,./:=?;!*#@$_%]`.
fn doctype_public(value: &str) -> Result<String, XsltError> {
    let ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " \r\n-'()+,./:=?;!*#@$_%".contains(c));
    if ok {
        Ok(value.to_string())
    } else {
        Err(XsltError::new(
            codes::XTSE0020,
            format!(
                "doctype-public {value:?} contains a character a public identifier cannot hold"
            ),
        ))
    }
}

/// Serialization §3 `doctype-system`: "A string of Unicode characters that does
/// not include both an apostrophe (#x27) and a quotation mark (#x22)
/// character."
fn doctype_system(value: &str) -> Result<String, XsltError> {
    if value.contains('\'') && value.contains('"') {
        Err(XsltError::new(
            codes::XTSE0020,
            format!("doctype-system {value:?} contains both kinds of quotation mark"),
        ))
    } else {
        Ok(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_domains() {
        assert!(nmtoken("1.0", "version").is_ok());
        assert_eq!(nmtoken("a b", "version").unwrap_err().code, "XTSE0020");
        assert!(encoding_name("XXX-xx").is_ok());
        assert_eq!(encoding_name("utf 8").unwrap_err().code, "XTSE0020");
        assert!(doctype_public("-//W3C//DTD XHTML 1.0 Transitional//EN").is_ok());
        assert_eq!(doctype_public("\u{A3}[~~~]").unwrap_err().code, "XTSE0020");
        assert!(doctype_system("ABC\"DEF").is_ok());
        assert_eq!(doctype_system("'\"").unwrap_err().code, "XTSE0020");
    }

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
