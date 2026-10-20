//! `xsl:result-document` (§19.1): the instruction's attributes, checked as far
//! as they can be before the run.

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::model::{InstructionKind, ResultDocument, ResultFormat};
use crate::names::{lexical_qname, QName};
use crate::output::{apply_serialization_attribute, Merge, OutputDefinition};
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, ModuleId, StaticContextId};
use crate::whitespace;

/// The serialization attributes of `xsl:result-document`, each with the name
/// of the `xsl:output` attribute it overrides (§19.1): "The output-version
/// attribute on the xsl:result-document instruction overrides the version
/// attribute on xsl:output … In all other cases, attributes correspond if they
/// have the same name." `use-character-maps` is not here: it is the one that
/// is not an attribute value template.
pub(crate) const SERIALIZATION_ATTRIBUTES: &[(&str, &str)] = &[
    ("method", "method"),
    ("byte-order-mark", "byte-order-mark"),
    ("cdata-section-elements", "cdata-section-elements"),
    ("doctype-public", "doctype-public"),
    ("doctype-system", "doctype-system"),
    ("encoding", "encoding"),
    ("escape-uri-attributes", "escape-uri-attributes"),
    ("include-content-type", "include-content-type"),
    ("indent", "indent"),
    ("media-type", "media-type"),
    ("normalization-form", "normalization-form"),
    ("omit-xml-declaration", "omit-xml-declaration"),
    ("standalone", "standalone"),
    ("undeclare-prefixes", "undeclare-prefixes"),
    ("output-version", "version"),
];

/// What `xsl:result-document` names that only exists once every declaration
/// is in: checked by [`Builder::check_result_document_references`].
pub(super) struct ResultDocumentReferences {
    /// The `format` written without curly brackets.
    format: Option<QName>,
    use_character_maps: Vec<QName>,
    location: Location,
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:result-document` (§19.1).
    pub(super) fn result_document(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        loc: &Location,
    ) -> Result<InstructionKind, XsltError> {
        let sc = self.static_contexts.get(ctx).clone();
        let resolve = |prefix: &str| sc.resolve_prefix(prefix);
        let at = |e: XsltError| e.with_location(loc.clone());

        let format = match attr_local(nav, "format") {
            None => None,
            Some(text) => {
                let avt = self.avt(&text, ctx)?;
                Some(match avt.as_literal() {
                    Some(fixed) => ResultFormat::Named(format_name(fixed, &resolve).map_err(at)?),
                    None => ResultFormat::Computed(avt),
                })
            }
        };
        let href = match attr_local(nav, "href") {
            Some(text) => Some(self.avt(&text, ctx)?),
            None => None,
        };

        // > [ERR XTSE0020] It is a static error if an attribute (other than an
        // > attribute written using curly brackets in a position where an
        // > attribute value template is permitted) contains a value that is not
        // > one of the permitted values for that attribute.
        //
        // A value with no expression in it is checked here, against a scratch
        // definition; one computed at run time is XTDE0030 there.
        let mut scratch = OutputDefinition::default();
        let mut serialization = Vec::new();
        for (attribute, parameter) in SERIALIZATION_ATTRIBUTES {
            let Some(text) = attr_local(nav, attribute) else {
                continue;
            };
            let avt = self.avt(&text, ctx)?;
            if let Some(fixed) = avt.as_literal() {
                apply_serialization_attribute(
                    &mut scratch,
                    parameter,
                    fixed,
                    &resolve,
                    Merge::Override,
                )
                .map_err(|e| at(written_value_error(e)))?;
            }
            serialization.push((*parameter, avt));
        }

        let use_character_maps = match attr_local(nav, "use-character-maps") {
            Some(text) => whitespace::tokens(&text)
                .map(|t| self.resolve(t, ctx, codes::XTSE0020))
                .collect::<Result<Vec<_>, _>>()
                .map_err(at)?,
            None => Vec::new(),
        };
        self.result_document_refs.push(ResultDocumentReferences {
            format: match &format {
                Some(ResultFormat::Named(name)) => Some(name.clone()),
                _ => None,
            },
            use_character_maps: use_character_maps.clone(),
            location: loc.clone(),
        });

        let validation = self.validation_of(nav, self.modules.module(module_id), ctx, false)?;
        let body = self.sequence_constructor(nav, module_id)?;
        Ok(InstructionKind::ResultDocument(Box::new(ResultDocument {
            format,
            href,
            serialization,
            use_character_maps,
            validation,
            body,
        })))
    }

    /// The static halves of XTDE1460 and XTSE1590 for every
    /// `xsl:result-document`, once every `xsl:output` and `xsl:character-map`
    /// is known.
    ///
    /// > \[ERR XTDE1460\] It is a non-recoverable dynamic error if the effective
    /// > value of the format attribute is not a valid lexical QName, or if it
    /// > does not match the expanded-QName of an output definition in the
    /// > stylesheet. If the processor is able to detect the error statically
    /// > (for example, when the format attribute contains no curly brackets),
    /// > then the processor may optionally signal this as a static error.
    ///
    /// §19.1 names no code for a `use-character-maps` name that no
    /// `xsl:character-map` declares; it is the error §20.1 gives the same
    /// attribute of `xsl:output` and `xsl:character-map` (XTSE1590), since the
    /// instruction's maps "supplement" the output definition's.
    pub(super) fn check_result_document_references(&self) -> Result<(), XsltError> {
        for refs in &self.result_document_refs {
            if let Some(name) = &refs.format {
                if !self.named_outputs.contains_key(name) {
                    return Err(XsltError::new(
                        codes::XTDE1460,
                        format!("the format {name} matches no named output definition"),
                    )
                    .with_location(refs.location.clone()));
                }
            }
            if let Some(name) = refs
                .use_character_maps
                .iter()
                .find(|n| !self.expanded_character_maps.contains_key(n))
            {
                return Err(XsltError::new(
                    codes::XTSE1590,
                    format!("use-character-maps names {name}, which no xsl:character-map declares"),
                )
                .with_location(refs.location.clone()));
            }
        }
        Ok(())
    }
}

/// The expanded QName of a `format` value (§19.1): "must be a lexical QName.
/// The QName is expanded using the namespace declarations in scope for the
/// xsl:result-document element." Anything else is XTDE1460 — the code §19.1
/// gives every way the value can fail to name an output definition — whether
/// the value was written or computed.
pub(crate) fn format_name(
    value: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<QName, XsltError> {
    let value = whitespace::trim(value);
    let (prefix, local) = lexical_qname(value).ok_or_else(|| {
        XsltError::new(
            codes::XTDE1460,
            format!("the format {value:?} is not a lexical QName"),
        )
    })?;
    if prefix.is_empty() {
        return Ok(QName::local(local));
    }
    match resolve(prefix) {
        Some(uri) if !uri.is_empty() => Ok(QName::new(uri, local)),
        _ => Err(XsltError::new(
            codes::XTDE1460,
            format!("the prefix {prefix:?} of the format {value:?} is not bound to a namespace"),
        )),
    }
}

/// A written serialization attribute's error: XTSE0020 (§2.2), whose
/// `xsl:output`-only twin XTSE1570 §19.1 does not cite for the instruction.
fn written_value_error(e: XsltError) -> XsltError {
    if e.code == codes::XTSE1570 {
        XsltError::new(codes::XTSE0020, e.message)
    } else {
        e
    }
}

/// A computed serialization attribute's error (§2.2): "\[ERR XTDE0030\] It is
/// a non-recoverable dynamic error if the effective value of an attribute
/// written using curly brackets, in a position where an attribute value
/// template is permitted, is a value that is not one of the permitted values
/// for that attribute." An unbound prefix in a computed QName (`method`,
/// `cdata-section-elements`) is the more specific XTDE0290 (§5.1: "Where the
/// result of evaluating an XPath expression (or an attribute value template)
/// is required to be a lexical QName, then unless otherwise specified it is a
/// non-recoverable dynamic error if the defining element has no namespace node
/// whose name matches the prefix of the lexical QName").
pub(crate) fn computed_value_error(e: XsltError) -> XsltError {
    let code = if e.code == codes::XTSE0280 {
        codes::XTDE0290
    } else {
        codes::XTDE0030
    };
    XsltError::new(code, e.message)
}
