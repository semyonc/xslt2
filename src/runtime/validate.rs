//! Validation of constructed nodes (§19.2; M4 plan §4 M4d–M4f, decisions D-5, D-10, D-12).
//!
//! Two halves: what `xsl:attribute` validates when it is evaluated
//! ([`Transformation::validate_attribute`]), and the validating receiver every tree pipeline of a
//! transformation has in front of its builder ([`Validating`]), which applies the annotation rule
//! over the boundary each element and document constructor announces and validates every
//! `strict`, `lax` and `type` element boundary through `xsd-schema`'s push API.
//!
//! **Attributes.**
//!
//! - `type` on `xsl:attribute` (§19.2.1.2): "the attribute is considered valid if (in the
//!   terminology of XML Schema) the attribute's normalized value is locally valid with respect to
//!   that type definition according to the rules for "String Valid"" — `xsd-schema`'s
//!   [`validate_simple_type`]; otherwise XTTE1540.
//! - `validation="strict"` on `xsl:attribute` (§19.2.1.1): "a top-level attribute declaration is
//!   identified whose local name and namespace (if any) match the name of the attribute, and
//!   schema-validity assessment is carried out according to … "Schema-Validity Assessment
//!   (Attribute)"" — no declaration is XTTE1512, an invalid value XTTE1510. §19.2.1.3 assesses the
//!   attribute on a synthetic element declared as `<xs:attribute ref="A"/>`, whose attribute use
//!   has the declaration's value constraint; XML Schema 1.1 Part 1 §3.2.4.1 ("Attribute Locally
//!   Valid") then asks, besides String Valid (clause 3), that a `fixed` value constraint be met:
//!   "If D.{value constraint} is present and D.{value constraint}.{variety} = fixed, then A's
//!   ·actual value· is equal or identical to D.{value constraint}.{value}" (clause 4). The other
//!   things the element would add are absent here: namespace fixup on the synthetic element
//!   matters only to an `xs:QName` / `xs:NOTATION` value, which XTTE1545 excludes first, and
//!   "Validation Root Valid (ID/IDREF)" is not applied to element and attribute validation
//!   (§19.2.1.3).
//! - `validation="lax"` on `xsl:attribute` (decision D-d1): the same with a declaration, but an
//!   invalid value is XTTE1515; without one the attribute "is not validated" (§19.2.1.1) and is
//!   `xs:untypedAtomic`.
//! - XTTE1545 for a type "derived from, or constructed by list or union from, the primitive types
//!   xs:QName or xs:NOTATION", whichever way the type was chosen.
//! - The valid attribute is annotated with the type (a validated one also with its declaration —
//!   the binding the typed source build gives such an attribute), and its string value is the
//!   [schema normalized value]: §19.2.1.3 makes the validation "equivalent to" serializing,
//!   parsing, validating and mapping the PSVI back to the XDM, where a validated attribute's
//!   string value is its schema normalized value.
//! - `strip` and `preserve` validate nothing: "In the case of xsl:attribute, the effect [of
//!   preserve] is exactly the same as specifying validation="strip": that is, the new attribute
//!   will have the type annotation xs:untypedAtomic."
//!
//! Whether an annotation survives is decided by the tree the node lands in: the outermost
//! enclosing constructor whose validation is not `preserve` decides ([`Boundary`], applied by
//! [`Validating`]).
//!
//! **Copies and documents** (M4e). `xsl:copy` and `xsl:copy-of` announce, for the copy of each
//! element or document node they copy, the boundary their validation asks for
//! (`Transformation::copy_validation`, `ComplexContentNormalizer::copy_selected`): `strip` none — the normalizer
//! copies untyped itself, passing on the ids of `xs:ID` attributes (`Receiver::element_id`), so a
//! `strip` copy is untyped whichever receivers build it —, `preserve` none (every binding travels;
//! an enclosing boundary still decides), `strict` a [`Boundary::Strict`] — on an
//! element the validation of a constructed element, on a document node document-level validation
//! (§19.2.2). An attribute copied with `strict` is validated here, as `xsl:attribute`'s
//! validation is ([`Transformation::validate_attribute`]); XTTE0950 is decided before a
//! `preserve` copy is made ([`Transformation::check_namespace_sensitive_copy`]). `xsl:document`
//! and `xsl:result-document` announce `Transformation::document_boundary`. **Document-level validation**
//! ([`Validating`]): the document node's children are exactly one element, no text, any comments
//! and processing instructions (XTTE1550, checked as the children arrive and at the end); the
//! element is the root of a validation run that, unlike element-level validation, applies
//! "Validation Root Valid (ID/IDREF)" — its `cvc-id.1` / `cvc-id.2` are XTTE1555, any other error
//! XTTE1510; that run's verdict is given when the document node ends, after the structure rule.
//! The documents of variables (§9.4) and of `xsl:message` (§17) are never validated: "This is
//! equivalent to using validation="preserve" on xsl:document … Similarly, validation is not
//! applied to document nodes created using xsl:message" (§19.2.2).
//!
//! **Type-driven and lax roots** (M4f; upstream C36, `xsd_schema::validation::ValidationRoot`).
//! `[xsl:]type` on an element constructor, a copy or a document node is a [`Boundary::Type`]: a
//! run whose root is assessed "using this type definition as the "processor-stipulated type
//! definition"" (§19.2.1.2; `ValidationRoot::Type`), whatever its name — an `xsi:type` overrides
//! the type only when validly derived from it (the stipulated type's `block` applies), an abstract
//! one needs a concrete `xsi:type`, the element is never nilled; any error is XTTE1540.
//! `validation="lax"` is a [`Boundary::Lax`]: a run whose root is validated against its top-level
//! declaration when there is one and laxly otherwise (§19.2.1.1; `ValidationRoot::Lax`). The
//! undeclared root, and recursively every undeclared child of an element that is not validated
//! ("its attributes and children are validated, again with lax validation"), is the run's lax
//! region (`Run::lax_region`): its `xsi:type` and `xsi:nil` are not passed to the run — "the
//! presence of an xsi:type attribute will not of itself cause an element to be validated"; the
//! region ends at a declared element, below which XML Schema decides. Any error is XTTE1515, and
//! what the run assessed
//! nothing of is `xs:anyType` (elements) / `xs:untypedAtomic` (attributes). On a document node both
//! request document-level validation, the element child validated so. `type="xs:untyped"` is
//! [`Boundary::Untyped`] — `strip` on an element, plus the structure rule on a document node. An
//! attribute copied with `lax` or `type` is validated here, as `xsl:attribute`'s `lax` and `type`
//! are; with a complex type it is XTTE1540 (String Valid needs a simple type).
//!
//! **Workarounds for upstream gaps** (M4g; each names the upstream item that would replace it,
//! `xslt_doc/WAVE_B_PLAN.md`):
//!
//! - **C72 — XPath 2.0 types the schema set cannot validate against.** `[xsl:]type` may name
//!   `xs:untypedAtomic`, `xs:anyAtomicType`, `xs:dayTimeDuration` or `xs:yearMonthDuration`
//!   (§3.13 puts them in scope), but an XML Schema 1.0 set has no component for them, and an XML
//!   Schema 1.1 set none that validates `xs:untypedAtomic`. They are [`XPathType`]s: an element
//!   or document is a [`Boundary::XPathType`] run, assessed against the type's stand-in (the
//!   nearest type of every set: `xs:anySimpleType`, `xs:duration`) with what the stand-in lacks
//!   checked here — XDM 1.0 §2.6.2's patterns for the durations on the root's [schema normalized
//!   value], the derivation rule for an `xsi:type` on the root (Structures clause 1.2.1.2.4) —, an
//!   attribute is validated so with String Valid; any failure is XTTE1540. Under
//!   `xs:anyAtomicType` an `xsi:type` on the root naming one of the other three takes its place
//!   ([`Validating::xpath_root_xsi_type`]). The node is annotated with the set's own component
//!   where there is one (1.1), otherwise with the stand-in, and the run says so once per type
//!   where a node is validated against a stand-in (`Transformation::take_warnings`): `instance of`
//!   and the typed value then answer for the stand-in (`element(*, xs:untypedAtomic)` false). The
//!   two durations in a 1.0 set are **fenced** instead ([`DurationPolicy`]; decision D-g2 as the
//!   user revised it, review R3): their stand-in's typed value, an `xs:duration`, is not ordered, so
//!   they are `XSLT2-NOTIMPL` naming C72, pointing to the XML Schema 1.1 option. Replaced
//!   by C72 (the components in a 1.0 set) and C92 (a validator for a 1.1 set's
//!   `xs:untypedAtomic`).
//! - **C92 — `xsi:type="xs:untypedAtomic"` in a 1.1 set** (review R5): no workaround — a stand-in
//!   would fail the derivation check (`xs:anySimpleType` is not derived from `xs:anyAtomicType`),
//!   and a source's `xsi:type` is assessed upstream — so such an `xsi:type` reaching a run is
//!   `XSLT2-NOTIMPL` naming C92 (`Validating::start_in_run`), not a type error of the stylesheet.
//! - **C89 — a QName / NOTATION element default that needs a namespace declaration.** Upstream
//!   gives a host no way to declare a prefix on a start tag it already built, nor to add one to a
//!   namespace snapshot a run already holds, and a run reveals the {value} of an element's
//!   `default` / `fixed` value only at the element's end. So [`Validating`] holds the start tag
//!   of every element a run's assessment gives such a value constraint — from the builder and
//!   from the runs outside that one — until the element ends ([`Frame::deferred`]), declares the
//!   prefix the value needs on it, and only then lets the outer runs and the builder see it
//!   (`Validating::end_deferred`); an element child releases the hold early (the element then
//!   takes no value). Replaced by C89 (the {value} of a declaration's value constraint read
//!   before the element's content, so the prefix can be declared with the start tag).

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use xsd_schema::document::NodeSchemaBinding;
use xsd_schema::ids::TypeKey;
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::navigator::{DomNavigator, DomNodeType, TypedValue};
use xsd_schema::parser::frames::SimpleTypeVariety;
use xsd_schema::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use xsd_schema::types::XmlTypeCode;
use xsd_schema::validation::simple::fixed_values_equal;
use xsd_schema::validation::{
    validate_simple_type, ContentType, SchemaInfo, SchemaValidator, SchemaValidity,
    ValidationError, ValidationRoot, ValidationRuntime, ValidationSink, ValidationWarning,
};
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSet;

use super::Transformation;
use crate::error::{codes, XsltError};
use crate::model::Validation;
use crate::names::{QName, XML_NS, XSD_NS};
use crate::receiver::{
    kept_by_type, kept_of_attribute, kept_of_element, kept_of_value, xtte1550, Boundary,
    CopyNamespaces, CopyValidation, Kept, Name, Nav, Receiver, ResultBuilder, XPathType,
};
use crate::whitespace;

/// The XML Schema instance namespace (`xsi:type`, `xsi:nil`).
const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

impl<'a, 't> Transformation<'a, 't> {
    /// The value and the binding of the attribute `name` an `xsl:attribute` with `validation`
    /// constructs from `value` (see the module docs).
    pub(super) fn validate_attribute(
        &self,
        validation: &Validation,
        name: &Name,
        value: String,
    ) -> Result<(String, Option<NodeSchemaBinding>), XsltError> {
        match validation {
            Validation::Strip | Validation::Preserve => Ok((value, None)),
            // Every string is a valid `xs:untypedAtomic`, and an attribute annotated
            // `xs:untypedAtomic` is exactly an unvalidated one (XPath 2.0 §2.5.1: "An attribute
            // that has been validated in skip mode is represented in the data model by an
            // attribute node with the type annotation xs:untypedAtomic").
            Validation::Type(type_name) if is_xs(type_name, "untypedAtomic") => Ok((value, None)),
            Validation::Type(type_name) => {
                match self.resolve_validation_type(type_name, "xsl:attribute")? {
                    Named::Key(type_key) => {
                        self.attribute_against_type(type_key, &type_name.to_string(), name, value)
                    }
                    Named::XPath(xpath) => self.attribute_against_xpath_type(xpath, name, value),
                }
            }
            Validation::Strict | Validation::Lax => {
                let strict = matches!(validation, Validation::Strict);
                let (mode, invalid_code) = if strict {
                    ("strict", codes::XTTE1510)
                } else {
                    ("lax", codes::XTTE1515)
                };
                let set = self.validating_set()?;
                let lookup = || {
                    let names = &set.name_table;
                    let ns = if name.uri.is_empty() {
                        None
                    } else {
                        Some(names.get(&name.uri)?)
                    };
                    set.lookup_attribute(ns, names.get(&name.local)?)
                };
                let Some(decl_key) = lookup() else {
                    // §19.2.1.1, `lax`: "If no such declaration is available, then the element or
                    // attribute is not validated" — it is `xs:untypedAtomic`.
                    if !strict {
                        return Ok((value, None));
                    }
                    return Err(XsltError::new(
                        codes::XTTE1512,
                        format!(
                            "validation=\"strict\": there is no top-level declaration of the \
                             attribute {}",
                            display(name)
                        ),
                    ));
                };
                let decl = &set.arenas.attributes[decl_key];
                let type_key = decl
                    .resolved_type
                    .unwrap_or_else(|| TypeKey::Simple(set.builtin_types().any_simple_type));
                check_namespace_sensitive(
                    set,
                    type_key,
                    name,
                    "the type of its top-level declaration",
                )?;
                let invalid = |reason: String| {
                    XsltError::new(
                        invalid_code,
                        format!(
                            "validation=\"{mode}\": the attribute {} is not valid against its \
                             top-level declaration: {reason}",
                            display(name)
                        ),
                    )
                };
                let result = validate_simple_type(&value, type_key, set)
                    .map_err(|e| invalid(format!("the value {value:?}: {e}")))?;
                if let Some(fixed) = &decl.fixed_value {
                    if !fixed_values_equal(&value, fixed, Some(type_key), set) {
                        return Err(invalid(format!(
                            "the value {value:?} is not the fixed value {fixed:?}"
                        )));
                    }
                }
                let value = result.normalized_value.unwrap_or(value);
                Ok((
                    value,
                    Some(NodeSchemaBinding {
                        type_key,
                        element_decl: None,
                        attribute_decl: Some(decl_key),
                        content_type: None,
                    }),
                ))
            }
        }
    }

    /// An attribute's value validated against `type_key` as `[xsl:]type` asks (§19.2.1.2): "the
    /// attribute is considered valid if (in the terminology of XML Schema) the attribute's
    /// normalized value is locally valid with respect to that type definition according to the
    /// rules for "String Valid"" — otherwise XTTE1540; XTTE1545 first for a type built from
    /// `xs:QName` / `xs:NOTATION`. String Valid is defined against a simple type definition, so a
    /// complex type (which `xsl:attribute` cannot name, XTSE1530, but `xsl:copy` and `xsl:copy-of`
    /// can, for a copied attribute) leaves no attribute valid: XTTE1540 (decision D-f4; XSLT 3.0
    /// gave this case its own code, XTTE1535, which XSLT 2.0 does not have).
    fn attribute_against_type(
        &self,
        type_key: TypeKey,
        type_name: &str,
        name: &Name,
        value: String,
    ) -> Result<(String, Option<NodeSchemaBinding>), XsltError> {
        let set = self.validating_set()?;
        if let TypeKey::Complex(_) = type_key {
            return Err(XsltError::new(
                codes::XTTE1540,
                format!(
                    "the attribute {} cannot be valid for the type {type_name}: it is a complex \
                     type, and an attribute is validated with String Valid, against a simple type \
                     definition (§19.2.1.2)",
                    display(name)
                ),
            ));
        }
        check_namespace_sensitive(set, type_key, name, &format!("the type {type_name}"))?;
        let result = validate_simple_type(&value, type_key, set).map_err(|e| {
            XsltError::new(
                codes::XTTE1540,
                format!(
                    "the value {value:?} of the attribute {} is not valid for the type \
                     {type_name}: {e}",
                    display(name)
                ),
            )
        })?;
        let value = result.normalized_value.unwrap_or(value);
        Ok((
            value,
            Some(NodeSchemaBinding {
                type_key,
                element_decl: None,
                attribute_decl: None,
                content_type: None,
            }),
        ))
    }

    /// The type `[xsl:]type` names, in the in-scope schema types (M4 plan D-7: the whole set;
    /// XTSE1520 has checked that it exists).
    ///
    /// **Workaround for upstream proposal C72** (M4g): `xs:anyAtomicType`, `xs:untypedAtomic`,
    /// `xs:yearMonthDuration` and `xs:dayTimeDuration` are XPath 2.0 types (§3.13 lists them as in
    /// scope) that an XML Schema 1.0 set has no definitions of, and `xs:untypedAtomic` has none
    /// an XML Schema 1.1 set can validate against (upstream: "No validator for type code
    /// UntypedAtomic"; proposal C92). They are [`Named::XPath`]: validated by this crate against their
    /// definitions ([`XPathType`]) and annotated with the set's own component where there is one,
    /// otherwise with the nearest type of the set — said once per run and type in a warning
    /// (`Transformation::take_warnings`), because `instance of` and the typed value then answer
    /// for that nearest type. The warning is given where a node is validated against the stand-in
    /// (`note_xpath_type`), not here: naming the type annotates nothing yet (a `copy-of` may copy
    /// only atomic values; review R9). The two durations in a 1.0 set are refused here
    /// ([`DurationPolicy`]: `XSLT2-NOTIMPL` naming C72).
    fn resolve_validation_type(&self, type_name: &QName, what: &str) -> Result<Named, XsltError> {
        let set = self.validating_set()?;
        if type_name.uri == XSD_NS {
            if let Some(xpath) = XPathType::of_local_name(&type_name.local) {
                if xpath == XPathType::UntypedAtomic || !xpath.is_exact(set) {
                    check_xpath_type_policy(xpath, set, what)?;
                    return Ok(Named::XPath(xpath));
                }
            }
        }
        resolve_type(set, type_name).map(Named::Key).ok_or_else(|| {
            XsltError::internal(format!(
                "type=\"{type_name}\" on {what}: not a type of the schema set, although the static \
                 check (XTSE1520) found it"
            ))
        })
    }

    /// An attribute's value validated against an XPath 2.0 type the set cannot validate against
    /// (the C72 workaround, [`XPathType`]): String Valid (§19.2.1.2) for the type's definition —
    /// the stand-in's validation, then the rule it lacks ([`XPathType::admits`]) on the [schema
    /// normalized value] —, XTTE1540 otherwise; annotated as [`XPathType::annotation`] says.
    /// (`xs:untypedAtomic` never comes here: every string is one, and an unvalidated attribute is
    /// exactly an `xs:untypedAtomic` one.)
    fn attribute_against_xpath_type(
        &self,
        xpath: XPathType,
        name: &Name,
        value: String,
    ) -> Result<(String, Option<NodeSchemaBinding>), XsltError> {
        let set = self.validating_set()?;
        note_xpath_type(Some(&self.warnings), xpath, set, "an attribute");
        let invalid = |reason: String| {
            XsltError::new(
                codes::XTTE1540,
                format!(
                    "the value {value:?} of the attribute {} is not valid for the type {xpath}: \
                     {reason}",
                    display(name)
                ),
            )
        };
        let result = validate_simple_type(&value, xpath.stand_in(set), set)
            .map_err(|e| invalid(e.to_string()))?;
        let normalized = result.normalized_value.unwrap_or_else(|| value.clone());
        if !xpath.admits(&normalized) {
            return Err(invalid(xpath_rule(xpath).to_string()));
        }
        Ok((
            normalized,
            Some(NodeSchemaBinding {
                type_key: xpath.annotation(set),
                element_decl: None,
                attribute_decl: None,
                content_type: None,
            }),
        ))
    }

    /// The boundary an element constructor (a literal result element, `xsl:element`, `xsl:copy`
    /// of an element) announces for `validation`: `strip`, `preserve`, `strict`, `lax`
    /// (§19.2.1.1) and `type` (§19.2.1.2) — for `xsl:copy` of an element with `preserve`, "the
    /// copied element will have a type annotation of xs:anyType … but any contained nodes will
    /// have their type annotations retained in the same way as with xsl:element", which is
    /// [`Boundary::Preserve`]. `type="xs:untyped"` is [`Boundary::Untyped`] (decision D-f3).
    pub(super) fn element_boundary(
        &self,
        validation: &Validation,
        what: &str,
    ) -> Result<Boundary, XsltError> {
        Ok(match validation {
            Validation::Strip => Boundary::Strip,
            Validation::Preserve => Boundary::Preserve,
            Validation::Strict => Boundary::Strict,
            Validation::Lax => Boundary::Lax,
            Validation::Type(type_name) if is_xs(type_name, "untyped") => Boundary::Untyped,
            Validation::Type(type_name) => match self.resolve_validation_type(type_name, what)? {
                Named::Key(type_key) => Boundary::Type(type_key),
                Named::XPath(xpath) => Boundary::XPathType(xpath),
            },
        })
    }

    /// The boundary of a document node an instruction constructs (`xsl:document`,
    /// `xsl:result-document`, `xsl:copy` of a document node) for `validation` (§19.2.2):
    /// `strip` and `preserve` "do not request validation" ([`Boundary::Strip`],
    /// [`Boundary::Preserve`]); `strict`, `lax` and a `type` request document-level validation,
    /// "The single element node child is validated, using the supplied values of the validation
    /// and type attributes" — the boundary is the element child's.
    pub(super) fn document_boundary(
        &self,
        validation: &Validation,
        what: &str,
    ) -> Result<Boundary, XsltError> {
        self.element_boundary(validation, what)
    }

    /// What `xsl:copy-of` (and `xsl:copy` of a node it does not construct anew) asks of the copy
    /// of each node ([`CopyValidation`]).
    pub(super) fn copy_validation(
        &self,
        validation: &Validation,
        what: &str,
    ) -> Result<CopyValidation, XsltError> {
        Ok(match self.element_boundary(validation, what)? {
            Boundary::Strip => CopyValidation::Strip,
            Boundary::Preserve => CopyValidation::Preserve,
            Boundary::Strict => CopyValidation::Strict,
            Boundary::Lax => CopyValidation::Lax,
            Boundary::Type(type_key) => CopyValidation::Type(type_key),
            Boundary::Untyped => CopyValidation::Untyped,
            Boundary::XPathType(xpath) => CopyValidation::XPathType(xpath),
        })
    }

    /// The validator the trees of this run validate constructed elements with — the run's one
    /// validator (D-12), made only when the stylesheet validates something (`validates`).
    pub(crate) fn construction_validator(&self) -> Option<&SchemaValidator<'a>> {
        let set = self.sheet.schema_set()?;
        self.sheet.validates.then(|| self.validator(set))
    }

    /// A [`Validating`] receiver over a new builder for a tree of this run.
    pub(crate) fn validating(&self, builder: ResultBuilder<'t>) -> Validating<'_, 't> {
        Validating::new(builder, self.schema_set, self.construction_validator())
            .with_warnings(Some(&self.warnings))
    }

    /// A sequence collector of this run, whose groups validate as the trees do.
    pub(crate) fn collector(&self) -> crate::receiver::SequenceCollector<'_, 't> {
        crate::receiver::SequenceCollector::new(&self.store)
            .with_validator(self.construction_validator())
            .with_warnings(Some(&self.warnings))
    }

    /// The schema set validation runs against: there is one whenever a validation other than
    /// `strip` gets this far, because a basic processor rejects any other value (XTSE1660, §21.1).
    fn validating_set(&self) -> Result<&'t SchemaSet, XsltError> {
        self.schema_set.ok_or_else(|| {
            XsltError::internal("validation of a constructed node without a schema set")
        })
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// The copy `xsl:copy-of` or `xsl:copy` makes of an attribute node it copies, as `validation`
    /// asks: `strict` and `lax` validate it against the top-level declaration of its name as
    /// `xsl:attribute`'s `strict` / `lax` do (§19.2.1.1; XTTE1512, XTTE1510 / XTTE1515, and
    /// XTTE1545 — §19.2.1.3: "it is an error to validate a parentless attribute against such a
    /// type. This affects the instructions xsl:attribute, xsl:copy, and xsl:copy-of"), `type`
    /// against the type with String Valid (§19.2.1.2, XTTE1540), and pass it on as an attribute
    /// with its new annotation; `preserve` is XTTE0950 when the attribute has namespace-sensitive
    /// content (it is copied without its parent element); otherwise the copy is the pipeline's
    /// (`Receiver::append_copy`).
    pub(super) fn copy_attribute_node(
        &self,
        nav: &Nav<'t>,
        namespaces: CopyNamespaces,
        validation: CopyValidation,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        match validation {
            CopyValidation::Strict | CopyValidation::Lax => {
                let name = Name::of(nav);
                let as_written = if validation == CopyValidation::Strict {
                    Validation::Strict
                } else {
                    Validation::Lax
                };
                let (value, binding) = self.validate_attribute(&as_written, &name, nav.value())?;
                out.attribute(&name, &value, binding)
            }
            CopyValidation::Type(type_key) => {
                let name = Name::of(nav);
                let set = self.validating_set()?;
                let (value, binding) = self.attribute_against_type(
                    type_key,
                    &type_display(set, type_key),
                    &name,
                    nav.value(),
                )?;
                out.attribute(&name, &value, binding)
            }
            CopyValidation::XPathType(xpath) => {
                let name = Name::of(nav);
                let (value, binding) = match xpath {
                    // Every string is one; an unvalidated attribute is exactly one.
                    XPathType::UntypedAtomic => (nav.value(), None),
                    _ => self.attribute_against_xpath_type(xpath, &name, nav.value())?,
                };
                out.attribute(&name, &value, binding)
            }
            // `xs:untyped` is a complex type (XDM 1.0 §2.6.2): no attribute is String Valid
            // against it (decision D-f4).
            CopyValidation::Untyped => Err(XsltError::new(
                codes::XTTE1540,
                format!(
                    "the attribute {} cannot be valid for the type xs:untyped: it is a complex \
                     type, and an attribute is validated with String Valid, against a simple type \
                     definition (§19.2.1.2)",
                    display(&Name::of(nav))
                ),
            )),
            CopyValidation::Preserve => {
                self.check_same_schema_set(nav)?;
                self.check_namespace_sensitive_copy(nav, false)?;
                out.append_copy(&XmlItem::Node(nav.clone()), namespaces, validation)
            }
            CopyValidation::Strip => {
                out.append_copy(&XmlItem::Node(nav.clone()), namespaces, validation)
            }
        }
    }

    /// A `preserve` copy carries the bindings of the node it copies, which name components of the
    /// set its document is bound to — they mean nothing in another set (M4 plan D-2; upstream's
    /// own `document::copy` refuses that case with `CopyError::SchemaMismatch`). Through the public
    /// API no node of a run is typed against another set: admission strips such a tree, with a
    /// warning (M4b), and a typed parameter of another set is refused; so this is an internal
    /// error, checked rather than assumed.
    pub(super) fn check_same_schema_set(&self, nav: &Nav<'t>) -> Result<(), XsltError> {
        match (nav.document().schema_set(), self.schema_set) {
            (None, _) => Ok(()),
            (Some(source), Some(own)) if std::ptr::eq(source, own) => Ok(()),
            _ => Err(XsltError::internal(
                "validation=\"preserve\": a copy of a node typed against another schema set",
            )),
        }
    }

    /// XTTE0950 (§11.9.1): "It is a type error to use the xsl:copy or xsl:copy-of instruction to
    /// copy a node that has namespace-sensitive content if the copy-namespaces attribute has the
    /// value no and its explicit or implicit validation attribute has the value preserve. It is
    /// also a type error if either of these instructions (with validation="preserve") is used to
    /// copy an attribute having namespace-sensitive content, unless the parent element is also
    /// copied. A node has namespace-sensitive content if its typed value contains an item of type
    /// xs:QName or xs:NOTATION or a type derived therefrom."
    ///
    /// For a `preserve` copy of `nav`: an attribute copied on its own is checked whatever
    /// `copy-namespaces` says (its parent is not copied); with `copy-namespaces="no"` (`subtree`
    /// false: `xsl:copy`, which copies the node alone; true: `xsl:copy-of`, which copies the
    /// elements and attributes below it too) every copied element and attribute is.
    pub(super) fn check_namespace_sensitive_copy(
        &self,
        nav: &Nav<'t>,
        subtree: bool,
    ) -> Result<(), XsltError> {
        let found = match nav.node_type() {
            DomNodeType::Attribute => namespace_sensitive_content(nav).then(|| nav.clone()),
            _ if subtree => first_namespace_sensitive(nav),
            _ => namespace_sensitive_content(nav).then(|| nav.clone()),
        };
        match found {
            None => Ok(()),
            Some(node) => Err(XsltError::new(
                codes::XTTE0950,
                format!(
                    "validation=\"preserve\" would copy the {} {} without the namespaces its \
                     typed value, which contains an xs:QName or xs:NOTATION, depends on",
                    if node.node_type() == DomNodeType::Attribute {
                        "attribute"
                    } else {
                        "element"
                    },
                    display(&Name::of(&node))
                ),
            )),
        }
    }
}

/// Whether the node at `nav` has namespace-sensitive content (§11.9.1): "its typed value contains
/// an item of type xs:QName or xs:NOTATION or a type derived therefrom" — the items of its typed
/// value, so a union-typed node holding an integer has none, nor does an empty list. An untyped
/// node's typed value is its string value as `xs:untypedAtomic`.
fn namespace_sensitive_content(nav: &Nav<'_>) -> bool {
    if nav.schema_binding().is_none() {
        return false;
    }
    match nav.typed_value() {
        TypedValue::Value(value) => contains_qname(&value),
        _ => false,
    }
}

fn contains_qname(value: &XmlValue) -> bool {
    match &value.value {
        XmlValueKind::List { items, .. } => items.iter().any(contains_qname),
        XmlValueKind::Union(member) => contains_qname(member),
        XmlValueKind::Atomic(XmlAtomicValue::QName(_) | XmlAtomicValue::Notation(_)) => true,
        _ => matches!(value.type_code, XmlTypeCode::QName | XmlTypeCode::Notation),
    }
}

/// The first element or attribute in the subtree of `nav` (itself included) with
/// namespace-sensitive content, in document order.
fn first_namespace_sensitive<'t>(nav: &Nav<'t>) -> Option<Nav<'t>> {
    let mut stack = vec![nav.clone()];
    while let Some(node) = stack.pop() {
        if matches!(node.node_type(), DomNodeType::Element | DomNodeType::Root) {
            if namespace_sensitive_content(&node) {
                return Some(node);
            }
            let mut attr = node.clone();
            if attr.move_to_first_attribute() {
                loop {
                    if namespace_sensitive_content(&attr) {
                        return Some(attr);
                    }
                    if !attr.move_to_next_attribute() {
                        break;
                    }
                }
            }
            let mut children = Vec::new();
            let mut child = node.clone();
            if child.move_to_first_child() {
                loop {
                    if child.node_type() == DomNodeType::Element {
                        children.push(child.clone());
                    }
                    if !child.move_to_next_sibling() {
                        break;
                    }
                }
            }
            stack.extend(children.into_iter().rev());
        }
    }
    None
}

/// How `[xsl:]type` (or an `xsi:type` under `xs:anyAtomicType`) naming one of the two XPath 2.0
/// duration types is treated in an XML Schema 1.0 set. `Fence` (in force: decision D-g2 as the user
/// revised it, review R3) refuses it where the type is resolved, `XSLT2-NOTIMPL` naming upstream
/// proposal C72, the message pointing to the XML Schema 1.1 option, which has both types; `StandIn`
/// (the alternative, kept so that the decision can be reversed in one place) validates by the
/// type's XDM definition and annotates `xs:duration` — whose typed value is not ordered, so that
/// `lt`, arithmetic, `sum` / `max` and `xsl:sort` raise type errors a conforming stylesheet does not
/// cause. The other two XPath types (`xs:untypedAtomic`, `xs:anyAtomicType`) keep their stand-ins:
/// their typed value is exact.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DurationPolicy {
    #[allow(dead_code)]
    StandIn,
    Fence,
}

/// The policy in force (see [`DurationPolicy`]).
const DURATIONS_IN_1_0: DurationPolicy = DurationPolicy::Fence;

/// Whether `xpath` may go through the C72 workaround in `set` (see [`DurationPolicy`]); `what` names
/// the construct for the message.
fn check_xpath_type_policy(xpath: XPathType, set: &SchemaSet, what: &str) -> Result<(), XsltError> {
    let duration = matches!(
        xpath,
        XPathType::DayTimeDuration | XPathType::YearMonthDuration
    );
    if duration && !xpath.is_exact(set) && DURATIONS_IN_1_0 == DurationPolicy::Fence {
        return Err(XsltError::not_implemented(
            format!(
                "{xpath} on {what} in an XML Schema 1.0 schema context, whose schema set has no \
                 definition of the type (the XML Schema 1.1 option, \
                 SchemaContextBuilder::xsd_version, supports it natively)"
            ),
            "upstream proposal C72",
        ));
    }
    Ok(())
}

/// The warning of the C72 workaround, once per run and type, where a node is annotated with the
/// stand-in of `xpath` (`what`: an element, an attribute): what the annotation is instead of
/// `xpath`, and what that changes. Nothing when the set has the type (the annotation is exact).
fn note_xpath_type(
    warnings: Option<&RefCell<Vec<String>>>,
    xpath: XPathType,
    set: &SchemaSet,
    what: &str,
) {
    let Some(warnings) = warnings else {
        return;
    };
    if xpath.is_exact(set) {
        return;
    }
    let consequence = match xpath {
        XPathType::UntypedAtomic => {
            "their typed value is their string value as xs:untypedAtomic, as for the type itself, \
             but `element(*, xs:untypedAtomic)` and `element(*, xs:anyAtomicType)` are false for \
             them, in `instance of` and in typed patterns"
        }
        XPathType::AnyAtomicType => {
            "their typed value is their string value as xs:untypedAtomic, as for the type itself, \
             but `element(*, xs:anyAtomicType)` and `attribute(*, xs:anyAtomicType)` are false for \
             them, in `instance of` and in typed patterns"
        }
        // Only with `DurationPolicy::StandIn`.
        XPathType::DayTimeDuration | XPathType::YearMonthDuration => {
            "their typed value is an xs:duration, which is not ordered: `lt` / `gt`, arithmetic, \
             sum / avg / min / max and xsl:sort over it raise type errors, and `instance of` the \
             type itself and typed patterns that name it are false for them"
        }
    };
    let stand_in = match xpath {
        XPathType::UntypedAtomic | XPathType::AnyAtomicType => "xs:anySimpleType",
        XPathType::DayTimeDuration | XPathType::YearMonthDuration => "xs:duration",
    };
    let warning = format!(
        "type \"{xpath}\" (first on {what}): the XML Schema 1.0 schema set has no definition of \
         this XPath 2.0 type (upstream proposal C72), so the nodes validated against it are \
         validated by its XDM 1.0 definition and annotated {stand_in}, its nearest type in the set; \
         {consequence}. An XML Schema 1.1 schema context has the type"
    );
    let mut warnings = warnings.borrow_mut();
    if !warnings
        .iter()
        .any(|known| known.starts_with(&format!("type \"{xpath}\" ")))
    {
        warnings.push(warning);
    }
}

/// What an `[xsl:]type` attribute names, as validation uses it.
pub(super) enum Named {
    /// A type of the schema set.
    Key(TypeKey),
    /// An XPath 2.0 type the set cannot validate against: the C72 workaround ([`XPathType`]).
    XPath(XPathType),
}

/// The rule of an XPath 2.0 type its stand-in lacks, for a message.
fn xpath_rule(xpath: XPathType) -> &'static str {
    match xpath {
        XPathType::DayTimeDuration => {
            "XDM 1.0 §2.6.2 restricts xs:duration to the pattern [^YM]*[DT].* (days, hours, \
             minutes and seconds only)"
        }
        XPathType::YearMonthDuration => {
            "XDM 1.0 §2.6.2 restricts xs:duration to the pattern [^DT]* (years and months only)"
        }
        XPathType::UntypedAtomic | XPathType::AnyAtomicType => "every string is valid",
    }
}

/// The type `name` denotes in the in-scope schema types: a built-in type of the XML Schema
/// namespace, or a type of the set (M4 plan D-7: the whole set).
fn resolve_type(set: &SchemaSet, name: &QName) -> Option<TypeKey> {
    let names = &set.name_table;
    let local = names.get(&name.local)?;
    if name.uri == XSD_NS {
        return set.get_built_in_type_by_qname(names.get(XSD_NS), local);
    }
    let ns = if name.uri.is_empty() {
        None
    } else {
        Some(names.get(&name.uri)?)
    };
    set.lookup_type(ns, local)
}

/// Whether `name` is `xs:{local}`.
fn is_xs(name: &QName, local: &str) -> bool {
    name.uri == XSD_NS && name.local == local
}

/// A type for a message: `Q{namespace}name`, or its kind when it is anonymous.
fn type_display(set: &SchemaSet, type_key: TypeKey) -> String {
    let (name, namespace) = match type_key {
        TypeKey::Simple(key) => set
            .arenas
            .simple_types
            .get(key)
            .map(|def| (def.name, def.target_namespace))
            .unwrap_or_default(),
        TypeKey::Complex(key) => set
            .arenas
            .complex_types
            .get(key)
            .map(|def| (def.name, def.target_namespace))
            .unwrap_or_default(),
    };
    let names = &set.name_table;
    match name {
        Some(name) => {
            let local = names.resolve(name);
            match namespace {
                Some(ns) => format!("Q{{{}}}{local}", names.resolve(ns)),
                None => local.to_string(),
            }
        }
        None => "an anonymous type".to_string(),
    }
}

/// XTTE1545 (§19.2.1.3): "A type error occurs if a type or validation attribute is defined
/// (explicitly or implicitly) for an instruction that constructs a new attribute node, if the
/// effect of this is to cause the attribute value to be validated against a type that is derived
/// from, or constructed by list or union from, the primitive types xs:QName or xs:NOTATION."
fn check_namespace_sensitive(
    set: &SchemaSet,
    type_key: TypeKey,
    name: &Name,
    what: &str,
) -> Result<(), XsltError> {
    if namespace_sensitive(set, type_key, 0) {
        return Err(XsltError::new(
            codes::XTTE1545,
            format!(
                "the attribute {} would be validated against {what}, which is derived from, or \
                 constructed by list or union from, xs:QName or xs:NOTATION: a parentless \
                 attribute has no namespaces to resolve such a value with",
                display(name)
            ),
        ));
    }
    Ok(())
}

/// Whether values of `type_key` are, or contain, `xs:QName` / `xs:NOTATION` values: an atomic type
/// whose primitive type is one of them, a list whose item type is, a union with such a member.
/// (The same walk as `xsd-schema`'s crate-private `type_has_qname_values`.)
fn namespace_sensitive(set: &SchemaSet, type_key: TypeKey, depth: u32) -> bool {
    let TypeKey::Simple(key) = type_key else {
        // `xsl:attribute` cannot name a complex type (XTSE1530), and a top-level attribute
        // declaration has a simple one.
        return false;
    };
    if depth > 64 {
        return false;
    }
    if let Some(code) = set.get_type_code(key) {
        return matches!(code, XmlTypeCode::QName | XmlTypeCode::Notation);
    }
    let Some(def) = set.arenas.simple_types.get(key) else {
        return false;
    };
    match def.variety {
        SimpleTypeVariety::Atomic => def
            .resolved_base_type
            .is_some_and(|base| namespace_sensitive(set, base, depth + 1)),
        SimpleTypeVariety::List => def
            .resolved_item_type
            .is_some_and(|item| namespace_sensitive(set, item, depth + 1)),
        SimpleTypeVariety::Union => def
            .resolved_member_types
            .iter()
            .any(|&member| namespace_sensitive(set, member, depth + 1)),
    }
}

/// An attribute name for a message: its lexical form, and the namespace when it has one.
fn display(name: &Name) -> String {
    let lexical = if name.prefix.is_empty() {
        name.local.clone()
    } else {
        format!("{}:{}", name.prefix, name.local)
    };
    if name.uri.is_empty() {
        lexical
    } else {
        format!("{lexical} (Q{{{}}}{})", name.uri, name.local)
    }
}

// ── The validating receiver (M4 plan D-5) ────────────────────────────────────────────────────

/// The receiver between the normalizer and the [`ResultBuilder`] of a transformation's tree
/// pipelines: it applies §19.2.1.1's annotation rule over the [`Boundary`] events and runs the
/// validation of every `strict`, `lax` and `type` element boundary through `xsd-schema`'s push
/// API (one run per boundary, its root assessed as [`RunMode`] says; M4f).
///
/// **The annotation rule.** "Conceptually, the validation requested for a child element or
/// attribute node is applied before the validation requested for its parent element" — so the
/// outermost enclosing boundary that is not `preserve` decides a node's annotation ([`Decider`]):
/// under a `strip` one the node is untyped and not nilled (an attribute or element that loses an
/// `xs:ID` annotation still identifies its element, and one whose typed value held an `xs:IDREF`
/// keeps the is-idrefs property: "The values of the is-id and is-idrefs properties are
/// unchanged"); under a `strict` one the node has what that boundary's validation gave it; when
/// every enclosing boundary is `preserve`, a new element is `xs:anyType` and every other node
/// keeps what it arrived with (a copied node its source's annotation, an attribute what its
/// `xsl:attribute` validated it to). Every `strict` boundary validates what it contains, whatever
/// decides the annotations, for its errors.
///
/// **A `strict` boundary** (§19.2.1.1: "a top-level element declaration is identified whose local
/// name and namespace (if any) match the name of the element, and schema-validity assessment is
/// carried out") is one validation run (`SchemaValidator::start_run`) rooted at the element: no
/// top-level declaration of the name is XTTE1512, decided by `lookup_element` before the run
/// starts; otherwise every event of the element's subtree — the start tag (with the in-scope
/// namespaces as a snapshot, `xsi:type` / `xsi:nil`, each attribute), the text, the ends — goes
/// to every run open at the time, in the order the driver for parsed documents sends them
/// (`validate_element`, `validate_attribute` …, `validate_end_of_attributes`, `validate_text` /
/// `validate_whitespace`, `validate_end_element`). "The validation rule "Validation Root Valid
/// (ID/IDREF)" is not applied" (§19.2.1.3) — `cvc-id.1` / `cvc-id.2` are dropped (upstream's
/// optional C38 would make that a flag) — while "Identity-constraint Satisfied" is (upstream
/// always enforces it). When the root ends the run ends (`end_validation`), and any
/// error left is XTTE1510 — an inner run ends before an outer one, so the innermost failure is the
/// one reported (D-10).
///
/// **Values.** §19.2.1.3 makes the validation of an element "equivalent to" serializing it,
/// parsing it, validating it and mapping the PSVI back to the XDM, so what a validation changes in
/// the *values* of the tree is there whatever later decides the annotations, and it is what the
/// validation of an enclosing boundary sees. The runs open at a node are applied to it innermost
/// first, each to what the runs inside it made of it ([`Validating::through_runs`]):
///
/// - an attribute's value becomes its [schema normalized value] (XDM 1.0 §6.2.4 / §6.3.4);
/// - the defaulted attributes a run finds missing are created (`get_default_attributes`;
///   "If default values for elements or attributes are defined in the schema, the validation
///   process will where necessary create new nodes containing these default values") and are
///   attributes to the runs outside it;
/// - the text of an element a run assesses with simple content is its [schema normalized value],
///   and an empty one takes the declaration's default or fixed value — the element's content is
///   held until it ends to know it ([`Frame::held`]);
/// - whitespace text in the content of an element a run assesses with element-only or empty
///   content is no text node (XDM 1.0 §6.7.4, read at w3.org: "if the resulting Text Node consists
///   entirely of whitespace and the character information items used to construct this node have
///   a parent and that parent is an element and its {content type} is not "mixed", then the
///   content of the Text Node is the zero-length string" — [`element_only`]; the typed source
///   build applies the same rule, upstream's own since `wave-b` `d5f2a8e`, C67).
///
/// The tree gets what the outermost run made of each node — under a `strip` boundary too, which
/// changes annotations only (§19.2.1.1: "The typed value of the node is changed to be the same as
/// its string value").
///
/// **Document nodes** opened inside another container (`xsl:document`, `xsl:copy` of a document
/// node) contribute their children (§5.7.1: "Any document node within the result sequence is
/// replaced by a sequence containing each of its children"): their frame decides the annotations
/// of what they contain, but their content is the enclosing element's ([`Frame::transparent`]).
///
/// **Document-level validation** (§19.2.2; a document node opened with [`Boundary::Strict`] —
/// `xsl:document`, `xsl:result-document`, a copy of a document node — wherever it is, in element
/// content too: the document node is validated before §5.7.1 replaces it by its children).
/// Its children are checked as they arrive ([`DocumentCheck`]): a text node or a second element
/// child is XTTE1550 at once, no element child XTTE1550 at its end ("unless the children of the
/// document node comprise exactly one element node, no text nodes, and zero or more comment and
/// processing instruction nodes"). "The single element node child is validated, using the
/// supplied values of the validation and type attributes, as described in 19.2.1": a run of its
/// own rooted at it (XTTE1512 without a top-level declaration), outside the element's own `strict`
/// run if it has one, and the outermost enclosing boundary that is not `preserve` — at least this
/// document's — decides the annotations. Unlike element-level validation, "The validation rule
/// "Validation Root Valid (ID/IDREF)" is applied", so its `cvc-id.1` / `cvc-id.2` are XTTE1555;
/// the run's verdict waits for the end of the document node, where the structure rule has had its
/// say ([`Validating::end_run`]). Nested document nodes of document content count as one (their
/// children are the outer one's, [`Validating::document_parents`]); a document node in element
/// content that stays empty never reaches this receiver, so the normalizer raises XTTE1550 for it.
///
/// **The document's decider** ([`Decider::Document`]): a document node validated as a document,
/// where nothing outside it decides, decides what opens inside it — another document node or a
/// copy between it and its element child included — and hands over to its run at that element.
///
/// **Ids a `strip` copy keeps** arrive as `Receiver::element_id` after their (untyped) attribute:
/// filed for the element, unless runs are open — then the runs' assessment decides is-id.
///
/// A start tag is passed on to the builder as it arrives when no run is open (the common case);
/// under a run it is held until its last attribute, when the validation has seen it, and the text
/// of the element's content until the next event that ends a text node.
pub(crate) struct Validating<'v, 't> {
    out: ResultBuilder<'t>,
    set: Option<&'t SchemaSet>,
    validator: Option<&'v SchemaValidator<'v>>,
    next_boundary: Option<Boundary>,
    frames: Vec<Frame>,
    runs: Vec<Run<'v>>,
    /// The start tag held for the runs.
    start: Option<StartTag>,
    /// While runs are open: the text of the current text node (with its `disable-output-escaping`
    /// flags) not passed on yet — its fate in element-only content depends on all of it.
    pending: Vec<(String, bool)>,
    /// The element ending while runs are open, between its last content and its end tag
    /// ([`end_element`](Receiver::end_element) → [`through_runs`](Self::through_runs)).
    ending: Option<Ending>,
    /// With no run open: the binding a `strip` boundary took from the attribute passed last, when
    /// its type holds QNames (its is-id and is-idrefs then come from its source,
    /// [`attribute_copied_from`](Receiver::attribute_copied_from)).
    stripped_attribute: Option<NodeSchemaBinding>,
    /// With no run open: a `strip` boundary took the binding of the element whose start tag is
    /// open (its is-id and is-idrefs come from its source,
    /// [`element_copied_from`](Receiver::element_copied_from)).
    stripped_element: bool,
    /// The frame of the element whose start tag is going through the runs, when that is not the
    /// innermost frame — a start tag held for the C89 workaround ([`Frame::deferred`]) completed
    /// under a document node in its content: the frame [`prefix_for`](Self::prefix_for)
    /// declares on.
    tag_frame: Option<usize>,
    /// The run's warnings (`Transformation::take_warnings`), where a root annotated with the
    /// stand-in of an XPath 2.0 type says so (the C72 workaround).
    warnings: Option<&'v RefCell<Vec<String>>>,
}

/// What the end of an element needs to know, set while it ends with runs open.
struct Ending {
    /// The namespaces every run open at the element has in its snapshot — those in scope at it
    /// before the fixup of its start tag declared any —, for an element default's QNames.
    scope: Scope,
    /// A `strip` boundary decides the element: it keeps is-id and is-idrefs as the outermost run
    /// gives them (`kept`, set by `through_runs`).
    keep: bool,
    kept: Option<Kept>,
    /// The element's start tag was held to its end (the C89 workaround, [`Frame::deferred`]): a
    /// QName / NOTATION default may declare the prefix it needs on it.
    declare: bool,
}

/// Namespace bindings in scope, in the shape the QName fixup asks them.
#[derive(Clone, Default)]
struct Scope {
    default: Option<String>,
    /// Prefixed bindings, the innermost declaration of each prefix only, in declaration order.
    prefixed: Vec<(String, String)>,
}

impl Scope {
    /// The bindings `declarations` (outermost first) leave in scope; `xml` is implicit.
    fn of<'d>(declarations: impl Iterator<Item = &'d (String, String)>) -> Self {
        let mut scope = Scope::default();
        for (prefix, uri) in declarations {
            if prefix.is_empty() {
                scope.default = (!uri.is_empty()).then(|| uri.clone());
            } else if prefix != "xml" {
                scope.prefixed.retain(|(p, _)| p != prefix);
                if !uri.is_empty() {
                    scope.prefixed.push((prefix.clone(), uri.clone()));
                }
            }
        }
        scope
    }

    fn namespace_of(&self, prefix: &str) -> Option<&str> {
        self.prefixed
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.as_str())
    }

    /// A prefix bound to `uri` — the one declared last.
    fn prefix_of(&self, uri: &str) -> Option<&str> {
        self.prefixed
            .iter()
            .rev()
            .find(|(_, u)| u == uri)
            .map(|(p, _)| p.as_str())
    }
}

/// What decides the annotations in a container: the outermost enclosing boundary that is not
/// `preserve`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decider {
    /// No such boundary: nodes keep what they arrive with.
    Nothing,
    Strip,
    /// The validation run with this index in `Validating::runs`.
    Run(usize),
    /// The document-level validation of the document node whose frame has this index (§19.2.2):
    /// the decider of that document node and of what opens inside it until its element child, the
    /// root of the run that then decides ([`Decider::Run`]) — so that a `strip` or `preserve`
    /// container inside the validated document (another `xsl:document`, a copy) does not take its
    /// place.
    Document(usize),
}

/// One open container (element or document node).
struct Frame {
    decider: Decider,
    /// The boundary the container's instruction announced.
    boundary: Option<Boundary>,
    /// How many runs the container (an element) roots — the innermost ones: its own `strict`
    /// boundary's, and that of the document-level validation of the document node it is the
    /// element child of.
    roots_runs: usize,
    /// The container is a document node.
    document_node: bool,
    /// A document node whose validation is requested (§19.2.2): what its children were so far.
    document: Option<DocumentCheck>,
    /// The namespace declarations of the element, as the normalizer wrote them, then the prefixes
    /// the runs' namespace fixup declared on it (`prefix_for`).
    namespaces: Vec<(String, String)>,
    /// How many of `namespaces` the element arrived with — before the fixup of its held start tag
    /// added any (`finish_start_tag`): the bindings every run open at the element has in its
    /// namespace snapshot.
    written_namespaces: usize,
    /// A document node inside another container: its children are the container's (§5.7.1), so
    /// text and child elements count toward the enclosing element ([`Validating::content_frame`]).
    transparent: bool,
    /// For an element in the scope of runs: what each run (by index) assessed its content as.
    views: Vec<View>,
    /// The content of an element some run assessed with simple content, held until the element
    /// ends, when the runs have decided its text ([`Validating::settle`]); `None` once an element
    /// child arrived (the element is then invalid for that run, and its content goes out as it
    /// came).
    held: Option<Vec<Held>>,
    /// **The C89 workaround**: the start tag of an element that may take a `default` / `fixed`
    /// value of type `xs:QName` / `xs:NOTATION`, held — from the builder and from the runs
    /// outside the innermost one whose assessment gave it such a value constraint — until the
    /// element ends (or an element child shows it cannot take the value): see
    /// [`Validating::finish_start_tag`].
    deferred: Option<Tag>,
}

/// What one run assessed an element's content as, for the values it gives the content.
enum View {
    /// A simple type, or a complex type with simple content: the text is the [schema normalized
    /// value], an empty element's the declaration's default or fixed value.
    Simple {
        type_key: TypeKey,
        default_value: Option<String>,
    },
    /// A complex type with element-only or empty content: whitespace text is no text node.
    ElementOnly,
    /// Mixed content, a nilled element, or nothing assessed: the text stays.
    Other,
}

enum Held {
    Text(String, bool),
    Comment(String),
    Pi(String, String),
}

/// The children of a document node validated as a document so far (§19.2.2).
struct DocumentCheck {
    /// How the element child is validated ("using the supplied values of the validation and type
    /// attributes"): `None` for `type="xs:untyped"`, whose element child is untyped (no run).
    mode: Option<RunMode>,
    /// Element children.
    elements: u32,
    /// The error the validation of the element child ended with — given when the document node
    /// ends, after XTTE1550 ("unless the children of the document node comprise exactly one
    /// element node, no text nodes …" comes before "The single element node child is
    /// validated").
    deferred: Option<XsltError>,
}

struct StartTag {
    name: Name,
    binding: Option<NodeSchemaBinding>,
    nilled: bool,
    attributes: Vec<(Name, String, Option<NodeSchemaBinding>)>,
}

/// A complete start tag on its way through the runs, innermost first
/// ([`Validating::finish_start_tag`]) — or held in [`Frame::deferred`] with the runs below
/// `seen_from` still to see it (the C89 workaround).
struct Tag {
    name: Name,
    /// What the element arrived with (a copy's binding and nilled).
    binding: Option<NodeSchemaBinding>,
    nilled: bool,
    xsi_type: Option<String>,
    xsi_nil: Option<String>,
    /// The element has no top-level declaration and some run has its parent in the lax region.
    undeclared: bool,
    slots: Vec<AttributeSlot>,
    /// What each run (by index) said about the element at its start.
    element_infos: Vec<Option<SchemaInfo>>,
    /// The namespace snapshot the runs were given last, and the count of the element's own
    /// declarations it was taken at.
    snapshot: Option<(NamespaceContextSnapshot, usize)>,
    /// The runs with this index and above have seen the start tag.
    seen_from: usize,
}

/// One validation run: a `strict`, `lax` or `type` element boundary's, or the document-level
/// validation of the element child of a document node (`document`: the index of that document
/// node's frame).
struct Run<'v> {
    runtime: ValidationRuntime<'v, RunSink>,
    errors: Rc<RefCell<Vec<ValidationError>>>,
    root: Name,
    document: Option<usize>,
    mode: RunMode,
    /// For each element open in the run, root first: whether it is in the run's **lax region** —
    /// the elements §19.2.1.1 does not validate under `lax` ("If no such declaration is available,
    /// then the element or attribute is not validated, but its attributes and children are
    /// validated, again with lax validation"): a `lax` run's root with no top-level declaration,
    /// and, recursively, an undeclared child of an element in the region. Their `xsi:type` and
    /// `xsi:nil` are not passed to the run ("the presence of an xsi:type attribute will not of
    /// itself cause an element to be validated"; decision D-f2, fix round 1). Pushed when an
    /// element's start tag reaches the run, popped when it ends; always `false` in `strict` and
    /// `type` runs.
    lax_region: Vec<bool>,
    /// What this crate found invalid about the root of a [`RunMode::XPath`] run (the C72
    /// workaround): the rule the stand-in type lacks, an `xsi:type` the type does not admit.
    extra: Vec<String>,
    /// The XPath 2.0 type the root of a [`RunMode::XPath`] run is assessed against: the run's,
    /// or the one an `xsi:type` on the root names in its place
    /// ([`Validating::xpath_root_xsi_type`]); `None` in the other modes.
    root_xpath: Option<XPathType>,
}

impl Run<'_> {
    /// Whether the element whose start or end the run is processing is the run's root: the root
    /// is the first element pushed on [`Run::lax_region`] and the last popped. (Before its push
    /// — at its start — the stack is empty; at its end, before its pop, it holds the root alone.)
    fn at_root(&self, started: bool) -> bool {
        self.lax_region.len() == usize::from(started)
    }

    /// Whether the next element to start in the run has its parent in the lax region — the
    /// run's root counts as such ("the element … being validated" by the `lax` instruction), so it
    /// is in the region exactly when it has no top-level declaration.
    fn parent_in_lax_region(&self) -> bool {
        self.mode == RunMode::Lax && self.lax_region.last().copied().unwrap_or(true)
    }
}

/// How a run assesses its root (§19.2.1.1, §19.2.1.2; upstream C36's
/// `xsd_schema::validation::ValidationRoot`, XML Schema 1.1 §5.2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// `strict`: against the top-level declaration of the root's name, which exists (XTTE1512
    /// first) — `ValidationRoot::Declaration`; an invalid root is XTTE1510.
    Strict,
    /// `lax`: against the top-level declaration when there is one, laxly otherwise —
    /// `ValidationRoot::Lax` ("lax wildcard validation"); invalid is XTTE1515.
    Lax,
    /// `type`: with the type as the processor-stipulated type definition —
    /// `ValidationRoot::Type`; invalid is XTTE1540.
    Type(TypeKey),
    /// `type` naming an XPath 2.0 type the set cannot validate against — **the workaround for
    /// upstream proposal C72** ([`XPathType`]): `ValidationRoot::Type` with the type's stand-in,
    /// plus, for the root, what the stand-in lacks ([`XPathType::admits`], checked when the root
    /// ends) and the type's own rule for an `xsi:type` on it ([`Validating::xpath_root_xsi_type`]);
    /// the root is annotated [`XPathType::annotation`]. Invalid is XTTE1540.
    XPath(XPathType),
}

impl RunMode {
    /// The run a boundary starts: `strict`, `lax` and a `type` validate; `strip`, `preserve` and
    /// `type="xs:untyped"` do not.
    fn of(boundary: Boundary) -> Option<RunMode> {
        match boundary {
            Boundary::Strict => Some(RunMode::Strict),
            Boundary::Lax => Some(RunMode::Lax),
            Boundary::Type(type_key) => Some(RunMode::Type(type_key)),
            Boundary::XPathType(xpath) => Some(RunMode::XPath(xpath)),
            Boundary::Strip | Boundary::Preserve | Boundary::Untyped => None,
        }
    }

    fn root(self, set: &SchemaSet) -> ValidationRoot {
        match self {
            RunMode::Strict => ValidationRoot::Declaration,
            RunMode::Lax => ValidationRoot::Lax,
            RunMode::Type(type_key) => ValidationRoot::Type(type_key),
            RunMode::XPath(xpath) => ValidationRoot::Type(xpath.stand_in(set)),
        }
    }
}

/// Collects a run's errors.
struct RunSink(Rc<RefCell<Vec<ValidationError>>>);

impl ValidationSink for RunSink {
    fn on_error(&mut self, error: ValidationError) {
        self.0.borrow_mut().push(error);
    }
    fn on_warning(&mut self, _warning: ValidationWarning) {}
}

/// One attribute of a held start tag, with what each run said about it.
struct AttributeSlot {
    name: Name,
    /// The value the runs validated so far made of it: an outer run sees what the inner ones gave.
    value: String,
    incoming: Option<NodeSchemaBinding>,
    /// By run index.
    infos: Vec<Option<SchemaInfo>>,
    /// The run whose `get_default_attributes` created it.
    defaulted_by: Option<usize>,
}

impl<'v, 't> Validating<'v, 't> {
    /// A validating receiver in front of `out`, the tree built with `set`; `validator` is the
    /// transformation's one validator (D-12), needed when the stylesheet validates elements.
    pub(crate) fn new(
        out: ResultBuilder<'t>,
        set: Option<&'t SchemaSet>,
        validator: Option<&'v SchemaValidator<'v>>,
    ) -> Self {
        Self {
            out,
            set,
            validator,
            next_boundary: None,
            frames: Vec::new(),
            runs: Vec::new(),
            start: None,
            pending: Vec::new(),
            ending: None,
            stripped_attribute: None,
            stripped_element: false,
            tag_frame: None,
            warnings: None,
        }
    }

    /// The receiver of a transformation whose warnings are `warnings`: an element annotated with
    /// the stand-in of an XPath 2.0 type (the C72 workaround) is said there, once per run and
    /// type.
    pub(crate) fn with_warnings(mut self, warnings: Option<&'v RefCell<Vec<String>>>) -> Self {
        self.warnings = warnings;
        self
    }

    /// The builder, once the stream is finished.
    pub(crate) fn into_inner(self) -> ResultBuilder<'t> {
        self.out
    }

    fn decider(&self) -> Decider {
        self.frames
            .last()
            .map(|frame| frame.decider)
            .unwrap_or(Decider::Nothing)
    }

    /// The frame whose content the next node is part of: the innermost one that is not a document
    /// node inside another container.
    fn content_frame(&self) -> Option<usize> {
        self.frames.iter().rposition(|frame| !frame.transparent)
    }

    /// `xs:anyType` (§19.2.1.1: the annotation of a new element under `preserve`, and of an
    /// element a validation did not assess); its content is mixed, so its typed value is its string
    /// value as `xs:untypedAtomic` (the typed value XDM gives a complex type with mixed content,
    /// `xs:anyType` included; upstream's navigator computes it so).
    fn any_type(&self) -> Option<NodeSchemaBinding> {
        self.set.map(|set| NodeSchemaBinding {
            type_key: TypeKey::Complex(set.any_type_key()),
            element_decl: None,
            attribute_decl: None,
            content_type: Some(ContentType::Mixed),
        })
    }

    /// The decider of a container opened inside the current one with `boundary`.
    fn decider_of(&self, boundary: Option<Boundary>, run: Option<usize>) -> Decider {
        match self.decider() {
            Decider::Nothing => self.own_decider(boundary, run),
            outer => outer,
        }
    }

    /// The decider a container's own `boundary` (and the run it started) makes, when nothing
    /// encloses it that decides.
    fn own_decider(&self, boundary: Option<Boundary>, run: Option<usize>) -> Decider {
        match (boundary, run) {
            (Some(Boundary::Strip | Boundary::Untyped), _) => Decider::Strip,
            (
                Some(Boundary::Strict | Boundary::Lax | Boundary::Type(_) | Boundary::XPathType(_)),
                Some(run),
            ) => Decider::Run(run),
            _ => Decider::Nothing,
        }
    }

    /// Text of the content of an element, through every open run, innermost first: each run
    /// validates what the runs inside it passed on, and passes on what its validation makes of it
    /// (`views`, by run index: what each run assessed the element's content as). With `end` the
    /// element ends in each run after its text; with `simple` the text is the element's whole
    /// content, held for its simple content, and a run that assessed it so (and found it valid)
    /// passes on the [schema normalized value] — of the declaration's default or fixed value when
    /// the text is empty: XML Schema 1.1 §3.3.4.3 clause 5.1 ("If D has a {value constraint}, and E
    /// has neither element nor character [children], and E is not ·nilled· with respect to D"),
    /// §3.3.1: "if the element being ·validated· is empty, then for purposes of calculating the
    /// [schema normalized value] and other contributions to the ·post-schema-validation infoset·
    /// the element is treated as if {value constraint}.{lexical form} was used as the content of
    /// the element". The result is what the outermost run made of the text, and whether any run
    /// changed it.
    ///
    /// When the element ends ([`Validating::ending`]): an element default of type `xs:QName` /
    /// `xs:NOTATION` is written for the instance (the run's typed value, made with the
    /// declaration's bindings, is the {value}; `qname_lexical` with the bindings every run has in
    /// its snapshot), and the outermost run's typed value gives is-id and is-idrefs to an element a
    /// `strip` boundary untypes.
    fn through_runs(
        &mut self,
        views: &[View],
        mut text: String,
        end: bool,
        simple: bool,
    ) -> Result<(String, bool), XsltError> {
        let mut changed = false;
        for index in (0..self.runs.len()).rev() {
            self.run_text(
                index,
                views.get(index),
                &mut text,
                &mut changed,
                end,
                simple,
            )?;
        }
        Ok((text, changed))
    }

    /// One run's part of [`through_runs`](Self::through_runs): the run `index` validates `text`
    /// (what the runs inside it made of it), ends the element when `end` says so, and leaves in
    /// `text` what its validation makes of it (`view`: what it assessed the content as; `simple`
    /// as for `through_runs`). `changed` is set when the text changed.
    fn run_text(
        &mut self,
        index: usize,
        view: Option<&View>,
        text: &mut String,
        changed: &mut bool,
        end: bool,
        simple: bool,
    ) -> Result<(), XsltError> {
        // The root of a run for an XPath 2.0 type (C72) ending: the rule its stand-in lacks.
        let xpath_root = match self.runs[index].mode {
            RunMode::XPath(_) if end && self.runs[index].at_root(true) => {
                self.runs[index].root_xpath
            }
            _ => None,
        };
        let runtime = &mut self.runs[index].runtime;
        if !text.is_empty() {
            if whitespace::is_all_xml_whitespace(text) {
                runtime.validate_whitespace(text);
            } else {
                runtime.validate_text(text);
            }
        }
        if end {
            let info = runtime.validate_end_element();
            let typed = (index == 0).then(|| info.typed_value.clone()).flatten();
            let outermost = (index == 0).then(|| {
                (
                    info.schema_type,
                    info.content_type,
                    info.validity == SchemaValidity::Valid && !info.is_nil,
                )
            });
            if let (
                true,
                Some(View::Simple {
                    type_key,
                    default_value,
                }),
            ) = (simple, view)
            {
                if info.validity == SchemaValidity::Valid && !info.is_nil {
                    let defaulted = text.is_empty() && default_value.is_some();
                    if let Some(default) = default_value.as_ref().filter(|_| text.is_empty()) {
                        *text = default.clone();
                        *changed |= !text.is_empty();
                    }
                    if let Some(normalized) = self.set.and_then(|set| {
                        schema_normalized(info.normalized_value, text, *type_key, set)
                    }) {
                        *text = normalized;
                        *changed = true;
                    }
                    if defaulted {
                        if let (Some(value), Some(set)) = (info.typed_value.as_ref(), self.set) {
                            if let Some(written) = self.element_default_written(value, text, set)? {
                                *text = written;
                                *changed = true;
                            }
                        }
                    }
                }
            }
            if let (Some(xpath), true) = (xpath_root, info.validity == SchemaValidity::Valid) {
                if !xpath.admits(text) {
                    let message = format!("the value {text:?}: {}", xpath_rule(xpath));
                    self.runs[index].extra.push(message);
                }
            }
            if let (Some((schema_type, content_type, valid)), Some(set)) = (outermost, self.set) {
                if let Some(ending) = self.ending.as_mut().filter(|ending| ending.keep) {
                    ending.kept = Some(match (valid, typed) {
                        (false, _) => Kept::default(),
                        (true, Some(value)) => kept_of_value(&value),
                        (true, None) => match schema_type {
                            Some(type_key @ TypeKey::Simple(_)) => {
                                kept_of_binding(type_key, text, set)
                            }
                            Some(type_key) if content_type == Some(ContentType::TextOnly) => {
                                kept_by_type(text, type_key, set)
                            }
                            _ => Kept::default(),
                        },
                    });
                }
            }
        }
        if matches!(view, Some(View::ElementOnly))
            && !text.is_empty()
            && whitespace::is_all_xml_whitespace(text)
        {
            text.clear();
            *changed = true;
        }
        Ok(())
    }

    /// The text of an element that took its declaration's `default` / `fixed` value, when that is
    /// or holds `xs:QName` / `xs:NOTATION` values: `value` is the run's typed value, made with the
    /// declaration's bindings (the {value}, Structures §3.3.2; upstream since `wave-b` `d5f2a8e`),
    /// written with prefixes the element binds to their namespaces — those it was written with,
    /// which every run open at it has in its namespace snapshot, so an outer run reads the same
    /// value. Where none is bound, a namespace would have to be declared on an element whose start
    /// tag is built, and in the snapshots the runs already have: not possible through the public
    /// API (upstream proposal C89) — `XSLT2-NOTIMPL` rather than a QName that reads another
    /// namespace or none.
    fn element_default_written(
        &mut self,
        value: &XmlValue,
        lexical: &str,
        set: &SchemaSet,
    ) -> Result<Option<String>, XsltError> {
        let Some(ending) = self.ending.as_ref() else {
            return Ok(None);
        };
        if ending.declare {
            // The C89 workaround: the start tag is still held, and the runs outside this one have
            // not seen it, so a prefix declared on it now is in every snapshot that reads the value.
            return Ok(self
                .qname_lexical(value, lexical, set, None)
                .unwrap_or_default());
        }
        let scope = ending.scope.clone();
        self.qname_lexical(value, lexical, set, Some(&scope))
            .map_err(|uri| {
                XsltError::internal(format!(
                    "the default or fixed value {lexical:?} of a constructed element, a QName in \
                     the namespace {uri:?}, where no prefix is bound to that namespace, on an \
                     element whose start tag was not held for it (the C89 workaround holds every \
                     element a run gives such a value constraint)"
                ))
            })
    }

    /// Passes the pending text of the current text node on — through the open runs (`views`, `end`
    /// as for [`through_runs`](Self::through_runs)), and to the builder unless a run dropped it
    /// (whitespace in element-only content: the only change a run makes to text that is not held).
    fn pass_pending(&mut self, views: &[View], end: bool) -> Result<(), XsltError> {
        let mut chunks = std::mem::take(&mut self.pending);
        if chunks.len() <= 1 {
            let (text, disable_escaping) = chunks.pop().unwrap_or_default();
            let (text, changed) = self.through_runs(views, text, end, false)?;
            if !changed && !text.is_empty() {
                self.out.text(&text, disable_escaping)?;
            }
            return Ok(());
        }
        let text: String = chunks.iter().map(|(text, _)| text.as_str()).collect();
        if !self.through_runs(views, text, end, false)?.1 {
            for (text, disable_escaping) in chunks {
                self.out.text(&text, disable_escaping)?;
            }
        }
        Ok(())
    }

    /// Passes the pending text of the current text node on, before an event that ends it.
    fn flush_pending(&mut self) -> Result<(), XsltError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let frame = self.content_frame();
        let views = frame
            .map(|frame| std::mem::take(&mut self.frames[frame].views))
            .unwrap_or_default();
        let result = self.pass_pending(&views, false);
        if let Some(frame) = frame {
            self.frames[frame].views = views;
        }
        result
    }

    /// An element child arrived in the content of the element `frame` held for its simple
    /// content: the element is invalid for the runs that assessed it so, and what it held goes out
    /// as it came, through the runs as text.
    fn release_held(&mut self, frame: usize) -> Result<(), XsltError> {
        let Some(held) = self.frames[frame].held.take() else {
            return Ok(());
        };
        let views = std::mem::take(&mut self.frames[frame].views);
        let result = self.settle(held, &views, false);
        self.frames[frame].views = views;
        result
    }

    /// Passes held content on, its text through the runs (`views`, `end` as for
    /// [`through_runs`](Self::through_runs), the content held for its simple content when `end`):
    /// as it came when no run changed the text (its `disable-output-escaping` flags kept),
    /// otherwise with what the runs made of it as one text node where the first text was (first,
    /// if there was none), the comments and processing instructions in their order.
    fn settle(&mut self, held: Vec<Held>, views: &[View], end: bool) -> Result<(), XsltError> {
        let (value, changed) = self.through_runs(views, held_text(&held), end, end)?;
        self.settled(held, value, changed)
    }

    /// Passes held content on once the runs made `value` of its text (`changed`: whether they
    /// changed it): see [`settle`](Self::settle).
    fn settled(&mut self, held: Vec<Held>, value: String, changed: bool) -> Result<(), XsltError> {
        if !changed {
            return self.replay(held);
        }
        let mut value = Some(value).filter(|value| !value.is_empty());
        let mut kept = Vec::with_capacity(held.len() + 1);
        if !held.iter().any(|held| matches!(held, Held::Text(..))) {
            if let Some(value) = value.take() {
                kept.push(Held::Text(value, false));
            }
        }
        for held in held {
            match held {
                Held::Text(..) => {
                    if let Some(value) = value.take() {
                        kept.push(Held::Text(value, false));
                    }
                }
                other => kept.push(other),
            }
        }
        self.replay(kept)
    }

    /// The in-scope namespaces of the element whose start tag is held, as the runs want them.
    fn snapshot(&self, set: &SchemaSet) -> NamespaceContextSnapshot {
        let mut default: Option<&str> = None;
        let mut bindings: Vec<(&str, &str)> = Vec::new();
        for frame in &self.frames {
            for (prefix, uri) in &frame.namespaces {
                if prefix.is_empty() {
                    default = (!uri.is_empty()).then_some(uri.as_str());
                } else if prefix != "xml" {
                    bindings.retain(|(p, _)| p != prefix);
                    if !uri.is_empty() {
                        bindings.push((prefix, uri));
                    }
                }
            }
        }
        let names = &set.name_table;
        NamespaceContextSnapshot {
            default_ns: default.map(|uri| names.add(uri)),
            bindings: bindings
                .into_iter()
                .map(|(prefix, uri)| (names.add(prefix), names.add(uri)))
                .collect(),
        }
    }

    /// The `xsi:type` the root of a [`RunMode::XPath`] run (the C72 workaround) is assessed with,
    /// `value` being the one on the element:
    ///
    /// - none when it names the run's type itself (it changes nothing);
    /// - for `xs:anyAtomicType`, whose base type is `xs:anySimpleType` (XDM 1.0 §2.6.2), so that
    ///   `xs:anySimpleType` is not derived from it: `xs:anySimpleType` is an error of the run (the
    ///   1.1 path's cvc-elt.4.2); another of the four XPath 2.0 types, all derived from
    ///   `xs:anyAtomicType` (XDM 1.0 §2.6.2, §2.6.3) and in the in-scope schema types (§3.13), is
    ///   handled as `[xsl:]type` naming it is: the run's root is assessed against that type
    ///   ([`Run::root_xpath`]) and the run gets its stand-in (`xs:duration` written with the
    ///   `xsi:type`'s own prefix; nothing for `xs:untypedAtomic`, whose stand-in is the run's) —
    ///   subject to [`DurationPolicy`] like `[xsl:]type` (so the durations are refused in a 1.0
    ///   set); any other type is the one given (the
    ///   stand-in `xs:anySimpleType` checks the derivation, `start_in_run` that the type is
    ///   atomic);
    /// - for the other three, none, and the run is invalid — XML Schema Part 1 §3.3.4,
    ///   Schema-Validity Assessment (Element) clause 1.2.1.2.4: an `xsi:type` replaces the
    ///   processor-stipulated type only when "validly derived from that type definition", and no
    ///   type of a schema set is derived from `xs:untypedAtomic` ("No predefined schema types are
    ///   derived from xs:untypedAtomic", XPath 2.0 §2.5.1), nor, in an XML Schema 1.0 set, from
    ///   the two durations, which it has no component for.
    fn xpath_root_xsi_type<'x>(
        &mut self,
        index: usize,
        xpath: XPathType,
        value: &'x str,
        set: &SchemaSet,
    ) -> Result<Option<Cow<'x, str>>, XsltError> {
        if self.names_xs_type(value, xpath.local_name()) {
            return Ok(None);
        }
        if xpath == XPathType::AnyAtomicType {
            if self.names_xs_type(value, "anySimpleType") {
                self.runs[index].extra.push(format!(
                    "xsi:type=\"{value}\" names xs:anySimpleType, the base type of \
                     xs:anyAtomicType (XDM 1.0 §2.6.2: \"Its base type is xs:anySimpleType\"), \
                     not a type derived from it (XML Schema Part 1 §3.3.4, Schema-Validity \
                     Assessment (Element) clause 1.2.1.2.4)"
                ));
                return Ok(None);
            }
            let named = [
                XPathType::UntypedAtomic,
                XPathType::DayTimeDuration,
                XPathType::YearMonthDuration,
            ]
            .into_iter()
            .find(|named| !named.is_exact(set) && self.names_xs_type(value, named.local_name()));
            let Some(named) = named else {
                return Ok(Some(Cow::Borrowed(value)));
            };
            check_xpath_type_policy(named, set, "an xsi:type")?;
            self.runs[index].root_xpath = Some(named);
            return Ok(match named {
                XPathType::UntypedAtomic | XPathType::AnyAtomicType => None,
                XPathType::DayTimeDuration | XPathType::YearMonthDuration => {
                    let value = whitespace::trim(value);
                    Some(Cow::Owned(match value.split_once(':') {
                        Some((prefix, _)) => format!("{prefix}:duration"),
                        None => "duration".to_string(),
                    }))
                }
            });
        }
        self.runs[index].extra.push(format!(
            "xsi:type=\"{value}\" names no type validly derived from {xpath} (XML Schema Part 1 \
             §3.3.4, Schema-Validity Assessment (Element) clause 1.2.1.2.4)"
        ));
        Ok(None)
    }

    /// Whether the QName `lexical` (an `xsi:type` value), read with the namespaces in scope at the
    /// element whose start tag is held, is `xs:{local}`.
    fn names_xs_type(&self, lexical: &str, local: &str) -> bool {
        let lexical = whitespace::trim(lexical);
        let scope = Scope::of(self.frames.iter().flat_map(|frame| frame.namespaces.iter()));
        let (uri, name) = match lexical.split_once(':') {
            Some((prefix, name)) => (scope.namespace_of(prefix), name),
            None => (scope.default.as_deref(), lexical),
        };
        uri == Some(XSD_NS) && name == local
    }

    /// How many namespace declarations the innermost container has so far (its own, then the
    /// fixup's) — the namespace snapshot changes exactly when this does.
    fn declared_here(&self) -> usize {
        let frame = match self.tag_frame {
            Some(frame) => self.frames.get(frame),
            None => self.frames.last(),
        };
        frame.map_or(0, |frame| frame.namespaces.len())
    }

    /// The lexical form, in the scope of the element whose start tag is held (or of the element
    /// ending, `fixed`), of `typed` — the {value} of a value constraint, whose QNames / NOTATIONs
    /// hold the schema's expanded names —, when it holds any (`None` otherwise: `lexical`, its
    /// {lexical form} normalized, is then the text): each such name written with a prefix the
    /// element binds to its namespace (`qname_written`), the other list members keep their
    /// lexical forms — upstream's `TypedBuilderHandler::qname_lexical_in_scope`. With `fixed`, a
    /// namespace no prefix is bound to in it is `Err(uri)`; without, a prefix is declared.
    fn qname_lexical(
        &mut self,
        typed: &XmlValue,
        lexical: &str,
        set: &SchemaSet,
        fixed: Option<&Scope>,
    ) -> Result<Option<String>, String> {
        if !holds_qname(typed) {
            return Ok(None);
        }
        match &typed.value {
            XmlValueKind::Atomic(XmlAtomicValue::QName(qn) | XmlAtomicValue::Notation(qn)) => {
                self.qname_written(qn, set, fixed).map(Some)
            }
            XmlValueKind::Union(member) => self.qname_lexical(member, lexical, set, fixed),
            XmlValueKind::List { items, .. } => {
                let tokens: Vec<&str> = whitespace::tokens(lexical).collect();
                let mut out: Vec<String> = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let token = tokens.get(i).copied();
                    match self.qname_lexical(item, token.unwrap_or(""), set, fixed)? {
                        Some(written) => out.push(written),
                        None => {
                            out.push(token.map_or_else(|| item.to_string_value(), str::to_string))
                        }
                    }
                }
                Ok(Some(out.join(" ")))
            }
            _ => Ok(None),
        }
    }

    /// `qn` written for the element (upstream's order, its decision D-f3): the schema's own
    /// prefix when the element binds it to the same namespace; unprefixed when the schema wrote it
    /// so and the default namespace is the same; another prefix bound to the namespace; unprefixed
    /// under a default namespace that is it; else a prefix declared on the element (`ns_dN`, as for
    /// the attribute names fixup needs — `prefix_for`) or, with `fixed`, `Err(uri)`. The XML
    /// namespace is `xml`'s. A name in no namespace is unprefixed: no prefix denotes no namespace,
    /// so under a default namespace it reads as a name in that namespace (upstream's D-f2, the one
    /// case the fixup cannot cover).
    fn qname_written(
        &mut self,
        qn: &xsd_schema::types::QNameValue,
        set: &SchemaSet,
        fixed: Option<&Scope>,
    ) -> Result<String, String> {
        let local = qn.local_name();
        let Some(ns) = qn.name().namespace_uri else {
            return Ok(local.to_string());
        };
        let uri = set.name_table.resolve(ns);
        if uri == XML_NS {
            return Ok(format!("xml:{local}"));
        }
        let live;
        let scope = match fixed {
            Some(scope) => scope,
            None => {
                live = Scope::of(self.frames.iter().flat_map(|frame| frame.namespaces.iter()));
                &live
            }
        };
        match qn.prefix().filter(|prefix| !prefix.is_empty()) {
            Some(prefix) if scope.namespace_of(prefix) == Some(uri.as_str()) => {
                return Ok(format!("{prefix}:{local}"));
            }
            None if scope.default.as_deref() == Some(uri.as_str()) => {
                return Ok(local.to_string());
            }
            _ => {}
        }
        if let Some(prefix) = scope.prefix_of(&uri) {
            return Ok(format!("{prefix}:{local}"));
        }
        if scope.default.as_deref() == Some(uri.as_str()) {
            return Ok(local.to_string());
        }
        if fixed.is_some() {
            return Err(uri);
        }
        let prefix = self.prefix_for(&uri);
        Ok(format!("{prefix}:{local}"))
    }

    /// A prefix bound to `uri` in the scope of the held start tag, or a new one declared on it.
    fn prefix_for(&mut self, uri: &str) -> String {
        let target = self
            .tag_frame
            .unwrap_or_else(|| self.frames.len().saturating_sub(1));
        let mut bound: Vec<(&str, &str)> = Vec::new();
        for frame in &self.frames[..=target.min(self.frames.len().saturating_sub(1))] {
            for (prefix, ns) in &frame.namespaces {
                if !prefix.is_empty() {
                    bound.retain(|(p, _)| p != prefix);
                    bound.push((prefix, ns));
                }
            }
        }
        if let Some((prefix, _)) = bound.iter().rev().find(|(_, ns)| *ns == uri) {
            return (*prefix).to_string();
        }
        let prefix = (0u32..)
            .map(|n| format!("ns_d{n}"))
            .find(|candidate| !bound.iter().any(|(p, _)| p == candidate))
            .expect("u32 is not exhausted by the prefixes of one element");
        if let Some(frame) = self.frames.get_mut(target) {
            frame.namespaces.push((prefix.clone(), uri.to_string()));
        }
        prefix
    }

    /// The held start tag, now complete: through every open run, innermost first — each run
    /// seeing the attribute values the runs inside it normalized and the attributes they
    /// defaulted ([`start_in_run`](Self::start_in_run)) —, then on to the builder with the values
    /// the outermost run gave and the bindings the decider gives ([`emit_start`](Self::emit_start)).
    ///
    /// **The C89 workaround.** When a run's assessment gives the element a declaration with a
    /// `default` / `fixed` value whose type may hold `xs:QName` / `xs:NOTATION` values
    /// ([`may_take_a_namespace_sensitive_value`]), the element may end empty and take that value,
    /// whose {value} names a namespace the element may bind no prefix to (XML Schema 1.1 Part 1
    /// §3.3.1: an empty element "is treated as if {value constraint}.{lexical form} was used as the
    /// content of the element"; Datatypes §3.3.18: the {value} is read with the schema's
    /// bindings). Writing it then needs a namespace declaration on the element (the namespace
    /// fixup Structures §3.4.5.1's Note leaves to the processor), in the tree and in the namespace
    /// snapshot of every run outside the one that applies the value — and upstream offers no way
    /// to add one to a start tag already built or to a snapshot already given (upstream proposal
    /// C89). So the start tag goes no further than that run: the builder and the runs outside it
    /// see it only when the element ends ([`Validating::end_deferred`]), with any prefix the value
    /// needed declared, or when an element child shows that the element cannot take the value
    /// ([`Validating::complete_deferred`]). Such an element has simple content, so what it holds
    /// meanwhile is text, comments and processing instructions — held anyway for its simple
    /// content ([`Frame::held`]).
    fn finish_start_tag(&mut self, start: StartTag) -> Result<(), XsltError> {
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        let frame = self.frames.len() - 1;
        self.frames[frame].written_namespaces = self.frames[frame].namespaces.len();
        let xsi = |local: &str| {
            start
                .attributes
                .iter()
                .find(|(name, _, _)| name.uri == XSI_NS && name.local == local)
                .map(|(_, value, _)| value.clone())
        };
        let (xsi_type, xsi_nil) = (xsi("type"), xsi("nil"));
        let runs = self.runs.len();
        // §19.2.1.1, `lax`: a child of an element that is not validated is "validated, again with
        // lax validation" — validated against its top-level declaration if it has one (the lax
        // region ends there: below a declared element XML Schema's assessment decides), otherwise
        // not validated and in the region itself.
        let undeclared =
            self.runs.iter().any(Run::parent_in_lax_region) && self.declared(&start.name)?.is_err();
        let mut tag = Tag {
            slots: start
                .attributes
                .into_iter()
                .map(|(name, value, incoming)| AttributeSlot {
                    name,
                    value,
                    incoming,
                    infos: (0..runs).map(|_| None).collect(),
                    defaulted_by: None,
                })
                .collect(),
            name: start.name,
            binding: start.binding,
            nilled: start.nilled,
            xsi_type,
            xsi_nil,
            undeclared,
            element_infos: (0..runs).map(|_| None).collect(),
            snapshot: None,
            seen_from: runs,
        };
        let mut hold = false;
        for index in (0..runs).rev() {
            self.start_in_run(&mut tag, index, set)?;
            if tag.element_infos[index]
                .as_ref()
                .is_some_and(|info| may_take_a_namespace_sensitive_value(info, set))
            {
                hold = true;
                break;
            }
        }
        // What each run assessed the content as, for the values it gives the content (a run that
        // has not seen the start tag yet: nothing, until it does).
        let views: Vec<View> = tag
            .element_infos
            .iter()
            .map(|info| view_of(info.as_ref(), set))
            .collect();
        if views.iter().any(|view| matches!(view, View::Simple { .. })) {
            self.frames[frame].held = Some(Vec::new());
        }
        self.frames[frame].views = views;
        if hold {
            self.frames[frame].deferred = Some(tag);
            return Ok(());
        }
        self.emit_start(tag, frame, set)
    }

    /// The run `index` sees the start tag `tag` of the element whose frame is the innermost (or
    /// [`Validating::tag_frame`]): the element with the namespace snapshot it has by then — those
    /// it was written with, and those the namespace fixup of the runs inside declared for their
    /// defaulted attributes (§19.2.1.3: the outer validation is of what the inner ones produced) —,
    /// its attributes as the runs inside made them, the end of the attributes, and the attributes
    /// this run defaults, added to the tag.
    fn start_in_run(
        &mut self,
        tag: &mut Tag,
        index: usize,
        set: &SchemaSet,
    ) -> Result<(), XsltError> {
        let runs = self.runs.len();
        let run = &mut self.runs[index];
        let in_region = tag.undeclared && run.parent_in_lax_region();
        // The root of a run for an XPath 2.0 type the set cannot validate against (C72).
        let xpath_root = match run.mode {
            RunMode::XPath(xpath) if run.at_root(false) => Some(xpath),
            _ => None,
        };
        run.lax_region.push(in_region);
        // An element in the region is not validated: its `xsi:type` does not make it
        // validated, its `xsi:nil` does not nil it (XML Schema 1.1 §3.3.1 nils only "with
        // respect to some element declaration D"); both stay attributes of it, assessed as
        // attributes are.
        let (xsi_type, xsi_nil) = if in_region {
            (None, None)
        } else {
            (tag.xsi_type.as_deref(), tag.xsi_nil.as_deref())
        };
        let xsi_type = match (xpath_root, xsi_type) {
            (Some(xpath), Some(value)) => self.xpath_root_xsi_type(index, xpath, value, set)?,
            (_, xsi_type) => xsi_type.map(Cow::Borrowed),
        };
        // Upstream proposal C92: an XML Schema 1.1 set has `xs:untypedAtomic` but no validator for
        // it, so an `xsi:type` naming it would fail every assessment ("No validator for type code
        // UntypedAtomic") whatever the derivation says — fenced rather than reported as the
        // element's invalidity (`[xsl:]type` naming it goes through the C72 workaround instead).
        if let Some(value) = xsi_type.as_deref() {
            if XPathType::UntypedAtomic.is_exact(set)
                && self.names_xs_type(value, XPathType::UntypedAtomic.local_name())
            {
                return Err(XsltError::not_implemented(
                    format!(
                        "xsi:type=\"{value}\" on the element {} validated with an XML Schema \
                         1.1 schema set, which has no validator for xs:untypedAtomic",
                        display(&tag.name)
                    ),
                    "upstream proposal C92",
                ));
            }
        }
        let xsi_type = xsi_type.as_deref();
        let declared = self.declared_here();
        if tag.snapshot.as_ref().is_none_or(|(_, of)| *of != declared) {
            tag.snapshot = Some((self.snapshot(set), declared));
        }
        let snapshot = &tag.snapshot.as_ref().expect("taken just now").0;
        let run = &mut self.runs[index];
        let mut info = run.runtime.validate_element(
            &tag.name.local,
            &tag.name.uri,
            xsi_type,
            xsi_nil,
            snapshot,
        );
        for slot in &mut tag.slots {
            if slot.defaulted_by == Some(index) {
                continue;
            }
            let info = self.runs[index].runtime.validate_attribute(
                &slot.name.local,
                &slot.name.uri,
                &slot.value,
            );
            if info.validity == SchemaValidity::Valid {
                if let Some(type_key) = info.schema_type {
                    if let Some(value) =
                        schema_normalized(info.normalized_value.clone(), &slot.value, type_key, set)
                    {
                        slot.value = value;
                    }
                }
            }
            slot.infos[index] = Some(info);
        }
        let after = self.runs[index].runtime.validate_end_of_attributes();
        if after.schema_type.is_some() {
            info.schema_type = after.schema_type;
            info.content_type = after.content_type;
        }
        // The root of a run for an XPath 2.0 type, assessed against its stand-in (no `xsi:type`
        // replaced it): annotated with the stand-in unless the set has the type — said once per
        // run and type (review R9: where a node is validated, not where the type is named).
        if let (Some(_), Some(root), Some(governing)) =
            (xpath_root, self.runs[index].root_xpath, info.schema_type)
        {
            if governing == root.stand_in(set) {
                note_xpath_type(self.warnings, root, set, "an element");
            }
        }
        // `xs:anyAtomicType` "includes all atomic values (and no values that are not atomic)"
        // (XDM 1.0 §2.6.2): an `xsi:type` the stand-in `xs:anySimpleType` admits must also be
        // atomic to be derived from it.
        if let (Some(XPathType::AnyAtomicType), Some(governing)) = (xpath_root, info.schema_type) {
            if governing != XPathType::AnyAtomicType.stand_in(set)
                && !is_atomic_type(set, governing)
            {
                self.runs[index].extra.push(format!(
                    "the xsi:type {} is not an atomic type, so it is not derived from \
                     xs:anyAtomicType (XDM 1.0 §2.6.2: \"an atomic type that includes all \
                     atomic values (and no values that are not atomic)\")",
                    type_display(set, governing)
                ));
            }
        }
        let _ = self.runs[index].runtime.take_deferred_attribute_results();
        for default in self.runs[index].runtime.get_default_attributes() {
            let local = set.name_table.resolve(default.local_name);
            let uri = default
                .namespace
                .map(|ns| set.name_table.resolve(ns))
                .unwrap_or_default();
            if tag
                .slots
                .iter()
                .any(|slot| slot.name.local == local && slot.name.uri == uri)
            {
                continue;
            }
            let prefix = if uri.is_empty() {
                String::new()
            } else if uri == XML_NS {
                // Namespaces in XML 1.0 §3: "The prefix xml is by definition bound to the
                // namespace name http://www.w3.org/XML/1998/namespace … Other prefixes MUST
                // NOT be bound to this namespace name."
                "xml".to_string()
            } else {
                self.prefix_for(&uri)
            };
            let type_key = default
                .type_key
                .unwrap_or_else(|| TypeKey::Simple(set.builtin_types().any_simple_type));
            let mut infos: Vec<Option<SchemaInfo>> = (0..runs).map(|_| None).collect();
            infos[index] = Some(SchemaInfo {
                schema_type: Some(type_key),
                attribute_decl: default.attribute_key,
                validity: SchemaValidity::Valid,
                is_default: true,
                // The {value} (read with the schema element's bindings): present for a type
                // whose values can be or hold QNames, and what a `strip` boundary reads is-id
                // and is-idrefs from.
                typed_value: default.typed_value.clone(),
                ..SchemaInfo::empty()
            });
            // XML Schema 1.1 §3.4.5.1 (Attribute Default Value): its [schema normalized value]
            // is "The ·effective value constraint·'s {lexical form}", which §3.2.2.1 defines as
            // "the ·normalized value· (with respect to the {type definition}) of the
            // [attribute]" `default` / `fixed`. A run outside this one validates it like any
            // other attribute.
            let lexical = normalized(&default.value, type_key, set).unwrap_or(default.value);
            // A QName / NOTATION in it is the schema's expanded name (the {value}, read with
            // the schema element's bindings, Datatypes §3.3.18): written again with a prefix
            // the element binds to its namespace, one declared on it where none is — the
            // namespace fixup Structures §3.4.5.1's Note leaves to the processor, as upstream's
            // typed build performs it. The outer runs see that declaration (the snapshot
            // above), so they read the value as written.
            let value = match default.typed_value.as_ref() {
                Some(typed) => match self.qname_lexical(typed, &lexical, set, None) {
                    Ok(Some(written)) => written,
                    _ => lexical,
                },
                None => lexical,
            };
            tag.slots.push(AttributeSlot {
                name: Name::qualified(&prefix, &local, &uri),
                value,
                incoming: None,
                infos,
                defaulted_by: Some(index),
            });
        }
        tag.element_infos[index] = Some(info);
        tag.seen_from = index;
        Ok(())
    }

    /// The start tag `tag` of the element whose frame is `frame`, which every run has seen, on to
    /// the builder: the element with the binding and nilled property its decider gives, its
    /// namespace declarations (with the fixup's), its attributes with the values the outermost
    /// run gave and the bindings the decider gives.
    fn emit_start(&mut self, tag: Tag, frame: usize, set: &SchemaSet) -> Result<(), XsltError> {
        let decider = self.frames[frame].decider;
        let (binding, nilled) = match decider {
            Decider::Strip => (None, false),
            Decider::Run(run) => {
                let info = tag.element_infos[run]
                    .as_ref()
                    .expect("every run saw the element");
                let binding = match info.schema_type {
                    Some(type_key) => Some(NodeSchemaBinding {
                        // The C72 workaround: the root of a run for an XPath 2.0 type, assessed
                        // against its stand-in, is annotated with the type itself where the set
                        // has it (`XPathType::annotation`; an `xsi:type` that replaced the
                        // stand-in stays).
                        type_key: match (self.runs[run].mode, self.runs[run].root_xpath) {
                            (RunMode::XPath(_), Some(xpath))
                                if self.runs[run].at_root(true)
                                    && type_key == xpath.stand_in(set) =>
                            {
                                xpath.annotation(set)
                            }
                            _ => type_key,
                        },
                        element_decl: info.element_decl,
                        attribute_decl: None,
                        content_type: info.content_type,
                    }),
                    // §19.2.1.1: "If no validation is performed for a node … then the node is
                    // annotated as xs:anyType in the case of an element".
                    None => self.any_type(),
                };
                (binding, info.is_nil)
            }
            // An element's own decider is never `Document` (its start converts it).
            Decider::Nothing | Decider::Document(_) => {
                if self.frames[frame].boundary == Some(Boundary::Preserve) {
                    (self.any_type(), false)
                } else {
                    (tag.binding, tag.nilled)
                }
            }
        };
        self.out.start_element(&tag.name, binding, nilled)?;
        let namespaces = self.frames[frame].namespaces.clone();
        for (prefix, uri) in &namespaces {
            self.out.namespace(prefix, uri)?;
        }
        for slot in tag.slots {
            let binding = match decider {
                Decider::Strip => {
                    // is-id and is-idrefs as the node had them just before the strip: what the
                    // outermost run inside the `strip` boundary (run 0 — any run outside that
                    // boundary would decide instead) made of it, by its typed value there.
                    let kept = match slot.infos.first() {
                        Some(info) => info
                            .as_ref()
                            .map(|info| kept_of_info(info, &slot.value, set))
                            .unwrap_or_default(),
                        None => slot
                            .incoming
                            .map(|binding| kept_of_binding(binding.type_key, &slot.value, set))
                            .unwrap_or_default(),
                    };
                    self.out.attribute(&slot.name, &slot.value, None)?;
                    self.out.keep_attribute(kept);
                    continue;
                }
                Decider::Run(run) => slot.infos[run].as_ref().and_then(attribute_binding),
                // An element's own decider is never `Document` (its start converts it).
                Decider::Nothing | Decider::Document(_) => slot.incoming,
            };
            self.out.attribute(&slot.name, &slot.value, binding)?;
        }
        self.out.end_start_tag()
    }

    /// The C89 workaround, released early: an element child arrived in the content of the
    /// element whose frame is `frame` and whose start tag is held ([`Frame::deferred`]), so the
    /// element has content and takes no default or fixed value (XML Schema 1.1 Part 1 §3.3.4.3
    /// clause 5.1: only when "E has neither element nor character [children]"). The runs that
    /// have not seen the start tag see it now, innermost first, and the builder gets it — before
    /// what the element held goes out ([`release_held`](Self::release_held)).
    fn complete_deferred(&mut self, frame: usize) -> Result<(), XsltError> {
        let Some(mut tag) = self.frames[frame].deferred.take() else {
            return Ok(());
        };
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        self.tag_frame = Some(frame);
        let started = (0..tag.seen_from).rev().try_for_each(|index| {
            self.start_in_run(&mut tag, index, set)?;
            self.frames[frame].views[index] = view_of(tag.element_infos[index].as_ref(), set);
            Ok::<(), XsltError>(())
        });
        self.tag_frame = None;
        started?;
        if self.frames[frame]
            .views
            .iter()
            .any(|view| matches!(view, View::Simple { .. }))
            && self.frames[frame].held.is_none()
        {
            self.frames[frame].held = Some(Vec::new());
        }
        self.emit_start(tag, frame, set)
    }

    /// The C89 workaround at the end of an element whose start tag was held ([`Frame::deferred`];
    /// the element is the innermost frame): its held content goes through every run, innermost
    /// first, as at any element's end ([`run_text`](Self::run_text)) — except that a run that has
    /// not seen the start tag sees it first ([`start_in_run`](Self::start_in_run)), with the
    /// namespace declarations the element has by then: a run that gives the element its `default`
    /// / `fixed` value writes a QName in it with a prefix declared on the element where none is
    /// bound (`Ending::declare`), so every run after it reads the value it wrote. Then the start
    /// tag, the content and the end go to the builder, and the runs rooted at the element end.
    fn end_deferred(&mut self) -> Result<(), XsltError> {
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        let frame = self.frames.len() - 1;
        let mut tag = self.frames[frame]
            .deferred
            .take()
            .expect("checked by the caller");
        let held = self.frames[frame].held.take().unwrap_or_default();
        let mut views = std::mem::take(&mut self.frames[frame].views);
        self.ending = Some(Ending {
            scope: Scope::default(),
            keep: self.frames[frame].decider == Decider::Strip,
            kept: None,
            declare: true,
        });
        let mut text = held_text(&held);
        let mut changed = false;
        let mut through = || -> Result<(), XsltError> {
            for index in (0..self.runs.len()).rev() {
                if index < tag.seen_from {
                    self.start_in_run(&mut tag, index, set)?;
                    views[index] = view_of(tag.element_infos[index].as_ref(), set);
                }
                self.run_text(index, views.get(index), &mut text, &mut changed, true, true)?;
            }
            Ok(())
        };
        let through = through();
        let ending = self.ending.take();
        through?;
        self.emit_start(tag, frame, set)?;
        self.settled(held, text, changed)?;
        // §19.2.1.1 under `strip`: "The values of the is-id and is-idrefs properties are
        // unchanged" — those the outermost run inside the boundary gave the element.
        if let Some(kept) = ending.and_then(|ending| ending.kept) {
            self.out.keep_open_element(kept)?;
        }
        let frame = self.frames.pop().expect("an element is open");
        for run in &mut self.runs {
            run.lax_region.pop();
        }
        for _ in 0..frame.roots_runs {
            self.end_run()?;
        }
        self.out.end_element()
    }

    /// Ends the innermost run, rooted at the element that just ended.
    ///
    /// An element-level run: XTTE1510 for any error it reported but the two ID/IDREF ones
    /// §19.2.1.3 excludes. A document-level one (§19.2.2): "The validation rule "Validation Root
    /// Valid (ID/IDREF)" is applied to the single element node child of the document node" — its
    /// `cvc-id.1` (a dangling IDREF) and `cvc-id.2` (a duplicate ID) are XTTE1555 ("It is a type
    /// error if, when validating a document node, document-level constraints are not satisfied"),
    /// any other error is the element's own invalidity, XTTE1510, which comes first (decision
    /// D-e5: an identity constraint is part of the element's validation, §19.2.1.3); the verdict is
    /// kept on the document node's frame until it ends.
    fn end_run(&mut self) -> Result<(), XsltError> {
        let Some(mut run) = self.runs.pop() else {
            return Ok(());
        };
        let failure = run.runtime.end_validation().err();
        let mut errors: Vec<ValidationError> = std::mem::take(&mut *run.errors.borrow_mut());
        errors.extend(failure);
        // The C72 workaround's own findings (`RunMode::XPath`), after upstream's.
        let extra = std::mem::take(&mut run.extra);
        let is_id = |error: &ValidationError| matches!(error.constraint, "cvc-id.1" | "cvc-id.2");
        let (id_errors, errors): (Vec<ValidationError>, Vec<ValidationError>) =
            errors.into_iter().partition(is_id);
        let count = |n: usize| match n {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        let first = errors
            .first()
            .map(ToString::to_string)
            .or_else(|| extra.first().cloned());
        let error = if let Some(first) = first {
            // §19.2.1.3: "The validation rule "Validation Root Valid (ID/IDREF)" is not applied" to
            // element-level validation — and its errors are not the element's invalidity.
            let root = display(&run.root);
            let more = count(errors.len() + extra.len());
            match run.mode {
                RunMode::Strict => XsltError::new(
                    codes::XTTE1510,
                    format!(
                        "validation=\"strict\": the element {root} is not valid against its \
                         top-level declaration: {first}{more}"
                    ),
                ),
                // XTTE1515: "has the effective value lax, and schema validity assessment
                // concludes that the element or attribute is invalid".
                RunMode::Lax => XsltError::new(
                    codes::XTTE1515,
                    format!("validation=\"lax\": the element {root} is not valid: {first}{more}"),
                ),
                // XTTE1540: "the outcome of schema validity assessment against that type is that
                // the validity property of that element or attribute information item is other
                // than valid".
                RunMode::Type(type_key) => {
                    let type_name = self
                        .set
                        .map(|set| type_display(set, type_key))
                        .unwrap_or_default();
                    XsltError::new(
                        codes::XTTE1540,
                        format!(
                            "type=\"{type_name}\": the element {root} is not valid against the \
                             type: {first}{more}"
                        ),
                    )
                }
                RunMode::XPath(xpath) => XsltError::new(
                    codes::XTTE1540,
                    format!(
                        "type=\"{xpath}\": the element {root} is not valid against the type: \
                         {first}{more}"
                    ),
                ),
            }
        } else {
            match (run.document, id_errors.first()) {
                (Some(_), Some(first)) => XsltError::new(
                    codes::XTTE1555,
                    format!(
                        "the validated document whose element is {} breaks the ID/IDREF rules: \
                         {first}{}",
                        display(&run.root),
                        count(id_errors.len())
                    ),
                ),
                _ => return Ok(()),
            }
        };
        match run.document.and_then(|frame| self.frames.get_mut(frame)) {
            Some(Frame {
                document: Some(check),
                ..
            }) => {
                check.deferred.get_or_insert(error);
                Ok(())
            }
            _ => Err(error),
        }
    }

    /// Whether the element `name` has a top-level declaration: `Err` inside is XTTE1512, "If the
    /// validation attribute … has the effective value strict, and there is no matching top-level
    /// declaration in the schema, then a type error occurs" (§19.2.1.1) — for an element
    /// constructor and for the element child of a validated document node alike (§19.2.2: "The
    /// single element node child is validated … as described in 19.2.1").
    fn declared(&self, name: &Name) -> Result<Result<(), XsltError>, XsltError> {
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        let names = &set.name_table;
        let declared = names.get(&name.local).is_some_and(|local| {
            let ns = if name.uri.is_empty() {
                Some(None)
            } else {
                names.get(&name.uri).map(Some)
            };
            ns.is_some_and(|ns| set.lookup_element(ns, local).is_some())
        });
        Ok(if declared {
            Ok(())
        } else {
            Err(XsltError::new(
                codes::XTTE1512,
                format!(
                    "validation=\"strict\": there is no top-level declaration of the element {}",
                    display(name)
                ),
            ))
        })
    }

    /// Starts a validation run rooted at the element `name` about to open — the document-level
    /// validation of the document node whose frame is `document`, if given — assessing its root
    /// as `mode` says, and returns its index.
    ///
    /// A `lax` root with no top-level declaration, and every undeclared element below it whose
    /// parent is so ([`Run::lax_region`]), is not validated against its `xsi:type`: §19.2.1.1's
    /// note, "the presence of an xsi:type attribute will not of itself cause an element to be
    /// validated: if validation against a named type is required, as distinct from validation
    /// against a top-level element declaration, then it must be requested using the XSLT
    /// [xsl:]type attribute" (decision D-f2, revised in fix round 1; XML Schema's lax wildcard
    /// validation would validate it strictly against the `xsi:type`, so the run is not given it —
    /// the way `ValidationRoot::Lax`'s rustdoc names for a host).
    fn start_run(
        &mut self,
        name: &Name,
        document: Option<usize>,
        mode: RunMode,
    ) -> Result<usize, XsltError> {
        let Some(validator) = self.validator else {
            return Err(XsltError::internal(
                "a validating boundary in a tree built without a validator",
            ));
        };
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        let errors = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = validator.start_run(RunSink(Rc::clone(&errors)));
        runtime.set_validation_root(mode.root(set));
        self.runs.push(Run {
            runtime,
            errors,
            root: name.clone(),
            document,
            mode,
            lax_region: Vec::new(),
            extra: Vec::new(),
            root_xpath: match mode {
                RunMode::XPath(xpath) => Some(xpath),
                _ => None,
            },
        });
        Ok(self.runs.len() - 1)
    }

    /// The frames of the document nodes whose children the next node is — the innermost run of
    /// consecutive document-node frames (§5.7.1: a document node in document content is replaced
    /// by its children too) —, as a range of frame indices.
    fn document_parents(&self) -> std::ops::Range<usize> {
        let end = self.frames.len();
        let start = self
            .frames
            .iter()
            .rposition(|frame| !frame.document_node)
            .map_or(0, |element| element + 1);
        start..end
    }

    /// §19.2.2, XTTE1550, for a text node about to be a child of the document nodes being built: "A
    /// type error occurs unless the children of the document node comprise exactly one element
    /// node, no text nodes, and zero or more comment and processing instruction nodes".
    fn check_document_text(&self) -> Result<(), XsltError> {
        if self
            .document_parents()
            .any(|frame| self.frames[frame].document.is_some())
        {
            return Err(xtte1550("a text node"));
        }
        Ok(())
    }

    /// Passes on content that a simple-content element held.
    fn replay(&mut self, held: Vec<Held>) -> Result<(), XsltError> {
        for content in held {
            match content {
                Held::Text(text, disable_escaping) => self.out.text(&text, disable_escaping)?,
                Held::Comment(text) => self.out.comment(&text)?,
                Held::Pi(target, data) => self.out.pi(&target, &data)?,
            }
        }
        Ok(())
    }
}

/// The text of held content, its text nodes joined (comments and processing instructions do not
/// count).
fn held_text(held: &[Held]) -> String {
    held.iter()
        .filter_map(|held| match held {
            Held::Text(text, _) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Whether a typed value is, or holds, an `xs:QName` / `xs:NOTATION` value (upstream's
/// crate-private `holds_qname_value`).
fn holds_qname(value: &XmlValue) -> bool {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::QName(_) | XmlAtomicValue::Notation(_)) => true,
        XmlValueKind::Union(member) => holds_qname(member),
        XmlValueKind::List { items, .. } => items.iter().any(holds_qname),
        _ => false,
    }
}

/// is-id and is-idrefs of a node a run assessed, as the run's `SchemaInfo` gives them: from its
/// typed value — made by the run with the node's in-scope namespaces, as upstream classifies a
/// bound node —, or, where the run kept none, from its type when that holds no QName.
fn kept_of_info(info: &SchemaInfo, value: &str, set: &SchemaSet) -> Kept {
    match &info.typed_value {
        Some(typed) => kept_of_value(typed),
        None => match info.schema_type {
            Some(type_key @ TypeKey::Simple(_)) => kept_of_binding(type_key, value, set),
            _ => Kept::default(),
        },
    }
}

/// is-id and is-idrefs by a node's type and value alone (`kept_by_type`): exact only when the
/// type's values hold no `xs:QName` / `xs:NOTATION` — the classification of a union with a QName
/// member needs the node's namespaces, which only the run, or the source a copy reads, has.
fn kept_of_binding(type_key: TypeKey, value: &str, set: &SchemaSet) -> Kept {
    if namespace_sensitive(set, type_key, 0) {
        return Kept::default();
    }
    kept_by_type(value, type_key, set)
}

/// The binding a run's `SchemaInfo` gives an attribute: its type and declaration, if it has a
/// type (§19.2.1.1: an attribute no validation assessed is `xs:untypedAtomic`).
fn attribute_binding(info: &SchemaInfo) -> Option<NodeSchemaBinding> {
    info.schema_type.map(|type_key| NodeSchemaBinding {
        type_key,
        element_decl: None,
        attribute_decl: info.attribute_decl,
        content_type: None,
    })
}

/// What a run that said `info` about an element at its start assessed its content as
/// ([`View`]); nothing for a run that has not seen the start (`None`) or nilled the element.
fn view_of(info: Option<&SchemaInfo>, set: &SchemaSet) -> View {
    match info {
        Some(info) if !info.is_nil => match simple_content(info) {
            Some(type_key) => View::Simple {
                type_key,
                default_value: info.element_decl.and_then(|decl| {
                    let decl = &set.arenas.elements[decl];
                    decl.default_value
                        .clone()
                        .or_else(|| decl.fixed_value.clone())
                }),
            },
            None if element_only(info) => View::ElementOnly,
            None => View::Other,
        },
        _ => View::Other,
    }
}

/// Whether the element a run said `info` about at its start may take a `default` / `fixed` value
/// that is or holds `xs:QName` / `xs:NOTATION` values (the C89 workaround holds its start tag): not
/// nilled, governed by a declaration with a value constraint, simple content whose type may hold
/// such values (`namespace_sensitive`; for a complex type with simple content its {content type}'s
/// simple type definition, and when that is not known, conservatively yes).
fn may_take_a_namespace_sensitive_value(info: &SchemaInfo, set: &SchemaSet) -> bool {
    if info.is_nil {
        return false;
    }
    let Some(decl) = info
        .element_decl
        .and_then(|decl| set.arenas.elements.get(decl))
    else {
        return false;
    };
    if decl.default_value.is_none() && decl.fixed_value.is_none() {
        return false;
    }
    match simple_content(info) {
        Some(type_key @ TypeKey::Simple(_)) => namespace_sensitive(set, type_key, 0),
        Some(TypeKey::Complex(key)) => set
            .arenas
            .complex_types
            .get(key)
            .and_then(|def| def.resolved_simple_content_type)
            .is_none_or(|content| namespace_sensitive(set, content, 0)),
        None => false,
    }
}

/// Whether `type_key` is an atomic simple type (not `xs:anySimpleType`, not a list or union).
fn is_atomic_type(set: &SchemaSet, type_key: TypeKey) -> bool {
    let TypeKey::Simple(key) = type_key else {
        return false;
    };
    key != set.builtin_types().any_simple_type
        && set
            .arenas
            .simple_types
            .get(key)
            .is_some_and(|def| def.variety == SimpleTypeVariety::Atomic)
}

/// Whether the type a run assessed an element with is complex with element-only or empty content —
/// the elements whose whitespace text XDM 1.0 §6.7.4 makes no text node ([`View::ElementOnly`]).
/// (The typed source build applied the same rule in this crate until upstream's typed build took
/// it over, `wave-b` `d5f2a8e`, C67; upstream also drops whitespace in empty content only when the
/// element is nilled — whitespace in empty content that is not nilled makes the element invalid,
/// `cvc-complex-type.2.1`, which here is XTTE1510 before the tree is used.)
fn element_only(info: &SchemaInfo) -> bool {
    matches!(info.schema_type, Some(TypeKey::Complex(_)))
        && matches!(
            info.content_type,
            Some(ContentType::ElementOnly | ContentType::Empty)
        )
}

/// The simple type of an element a run assessed, when its content is simple: a simple type, or a
/// complex type with simple content.
fn simple_content(info: &SchemaInfo) -> Option<TypeKey> {
    match (info.schema_type?, info.content_type) {
        (type_key @ TypeKey::Simple(_), _) => Some(type_key),
        (type_key, Some(ContentType::TextOnly)) => Some(type_key),
        _ => None,
    }
}

/// The [schema normalized value] of `value` as a value of `type_key`, when it differs.
fn normalized(value: &str, type_key: TypeKey, set: &SchemaSet) -> Option<String> {
    validate_simple_type(value, type_key, set)
        .ok()?
        .normalized_value
        .filter(|normalized| normalized != value)
}

/// The [schema normalized value] a run reported for `value` (`SchemaInfo::normalized_value`, which
/// a run computes with the element's namespaces), or else the one `validate_simple_type` gives —
/// when it differs from `value`.
fn schema_normalized(
    reported: Option<String>,
    value: &str,
    type_key: TypeKey,
    set: &SchemaSet,
) -> Option<String> {
    match reported {
        Some(reported) => (reported != value).then_some(reported),
        None => normalized(value, type_key, set),
    }
}

impl<'t> Receiver<'t> for Validating<'_, 't> {
    // A basic processor's trees (no schema set) are never typed (§21.1): every event goes straight
    // to the builder.

    fn start_document(&mut self) -> Result<(), XsltError> {
        if self.set.is_none() {
            return self.out.start_document();
        }
        let boundary = self.next_boundary.take();
        // A document node validated as a document decides what it contains when nothing outside
        // it does (its run starts at its element child).
        let mode = boundary.and_then(RunMode::of);
        let decider = match (self.decider(), mode) {
            (Decider::Nothing, Some(_)) => Decider::Document(self.frames.len()),
            _ => self.decider_of(boundary, None),
        };
        let transparent = !self.frames.is_empty();
        self.frames.push(Frame {
            decider,
            boundary,
            roots_runs: 0,
            document_node: true,
            // §19.2.2: "When validation is requested for a document node (that is, when
            // validation is set to strict or lax, or when a type attribute is present)".
            document: boundary
                .filter(|boundary| boundary.requests_validation())
                .map(|_| DocumentCheck {
                    mode,
                    elements: 0,
                    deferred: None,
                }),
            namespaces: Vec::new(),
            written_namespaces: 0,
            transparent,
            views: Vec::new(),
            held: None,
            deferred: None,
        });
        self.out.start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        if self.set.is_some() {
            // (A document node in element content does not end the text node being built: its
            // children are the element's content, §5.7.1.)
            let frame = self.frames.pop().expect("a document node is open");
            if let Some(check) = frame.document {
                if check.elements == 0 {
                    return Err(xtte1550("no element child"));
                }
                if let Some(error) = check.deferred {
                    return Err(error);
                }
            }
        }
        self.out.end_document()
    }

    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        // A basic processor's trees are never typed (§21.1).
        if self.set.is_some() {
            self.next_boundary = Some(boundary);
        }
        Ok(())
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        if self.set.is_none() {
            return self.out.start_element(name, binding, nilled);
        }
        if !self.runs.is_empty() {
            // The element ends the text node before it; an element child of an element held for
            // its simple content makes it invalid, and what it held goes out as it came.
            match self.content_frame() {
                Some(frame) if self.frames[frame].held.is_some() => {
                    // A start tag held for the C89 workaround goes out first: the element has
                    // content, so it takes no default or fixed value.
                    self.complete_deferred(frame)?;
                    self.release_held(frame)?
                }
                _ => self.flush_pending()?,
            }
        }
        let boundary = self.next_boundary.take();
        let mut roots_runs = 0;
        // §19.2.2: the element child of a document node validation is requested for — "The
        // single element node child is validated, using the supplied values of the validation and
        // type attributes, as described in 19.2.1" — is the root of a run of its own, outside the
        // element's own (a parent's validation is applied after a child's, §19.2.1.1); a second
        // element child is XTTE1550.
        let mut document_runs: Vec<(usize, usize)> = Vec::new();
        for frame in self.document_parents() {
            let Some(check) = self.frames[frame].document.as_mut() else {
                continue;
            };
            check.elements += 1;
            if check.elements > 1 {
                return Err(xtte1550("more than one element child"));
            }
            // `type="xs:untyped"`: the element child is untyped, nothing to run.
            let Some(mode) = check.mode else {
                continue;
            };
            let declared = match mode {
                RunMode::Strict => self.declared(name)?,
                RunMode::Lax | RunMode::Type(_) | RunMode::XPath(_) => Ok(()),
            };
            match declared {
                Ok(()) => {
                    let run = self.start_run(name, Some(frame), mode)?;
                    document_runs.push((frame, run));
                    roots_runs += 1;
                }
                // Given when the document node ends, after its structure rule.
                Err(error) => {
                    let check = self.frames[frame].document.as_mut().expect("checked");
                    check.deferred.get_or_insert(error);
                }
            }
        }
        let mut run = None;
        if let Some(mode) = boundary.and_then(RunMode::of) {
            if mode == RunMode::Strict {
                // XTTE1512: "there is no matching top-level declaration in the schema" — decided
                // before validating. (`lax` validates laxly without one; `type` needs none.)
                self.declared(name)??;
            }
            run = Some(self.start_run(name, None, mode)?);
            roots_runs += 1;
        }
        // The outermost enclosing boundary that is not `preserve` decides: a validated document's
        // run (outside the element's own, and outside any container between the document node and
        // its element child), else what decides around the element.
        let decider = match self.decider() {
            Decider::Document(frame) => match document_runs.iter().find(|(f, _)| *f == frame) {
                Some(&(_, run)) => Decider::Run(run),
                // The element has no declaration: XTTE1512 when the document node ends.
                None => self.own_decider(boundary, run),
            },
            _ => self.decider_of(boundary, run),
        };
        self.frames.push(Frame {
            decider,
            boundary,
            roots_runs,
            document_node: false,
            document: None,
            namespaces: Vec::new(),
            written_namespaces: 0,
            transparent: false,
            views: Vec::new(),
            held: None,
            deferred: None,
        });
        if self.runs.is_empty() {
            let (built, nilled) = match decider {
                Decider::Strip => (None, false),
                _ if boundary == Some(Boundary::Preserve) => (self.any_type(), false),
                _ => (binding, nilled),
            };
            // An element arriving with a binding here is a copy (only the normalizer's copies
            // carry one): under `strip` it keeps the is-id and is-idrefs of its source, which the
            // copy names after its attributes (`element_copied_from`).
            self.stripped_element = decider == Decider::Strip && binding.is_some();
            return self.out.start_element(name, built, nilled);
        }
        self.start = Some(StartTag {
            name: name.clone(),
            binding,
            nilled,
            attributes: Vec::new(),
        });
        Ok(())
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        // The declarations are kept for the namespace snapshots of a run, so only where one can
        // start.
        if self.validator.is_some() {
            if let Some(frame) = self.frames.last_mut() {
                frame.namespaces.push((prefix.to_string(), uri.to_string()));
            }
        }
        if self.start.is_some() {
            return Ok(());
        }
        self.out.namespace(prefix, uri)
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        if let Some(start) = self.start.as_mut() {
            start
                .attributes
                .push((name.clone(), value.to_string(), binding));
            return Ok(());
        }
        match (self.decider(), binding) {
            // is-id and is-idrefs as the binding the strip takes away gives them (§19.2.1.1): a
            // copy's come with its events, read from its source; an attribute an instruction
            // validated (`xsl:attribute`, `copy_attribute_node`) has a type that holds no QName
            // (XTTE1545), for which classifying by the type alone is exact.
            (Decider::Strip, Some(stripped)) => {
                self.out.attribute(name, value, None)?;
                if let Some(set) = self.set {
                    if namespace_sensitive(set, stripped.type_key, 0) {
                        // Only a copy carries such a binding: its source decides.
                        self.stripped_attribute = Some(stripped);
                    } else {
                        self.stripped_attribute = None;
                        self.out
                            .keep_attribute(kept_of_binding(stripped.type_key, value, set));
                    }
                }
                Ok(())
            }
            (_, binding) => {
                self.stripped_attribute = None;
                self.out.attribute(name, value, binding)
            }
        }
    }

    /// The id an untyped copied attribute or element keeps (`Receiver::element_id`): filed for the
    /// element — unless validation runs are open, whose assessment of the node decides its is-id (a
    /// copy's nodes are re-assessed like any other; §19.2.1.3's serialize–parse–validate).
    fn element_id(&mut self, id: &str) -> Result<(), XsltError> {
        if self.start.is_none() {
            self.out.file_id(id.to_string());
        }
        Ok(())
    }

    /// A `preserve` copy of an attribute whose binding a `strip` boundary just took away, its type
    /// holding QNames: it keeps the is-id and is-idrefs its source has (`kept_of_attribute`, as
    /// upstream classified it with the source's namespaces). Without a run open only.
    fn attribute_copied_from(&mut self, source: &Nav<'t>) -> Result<(), XsltError> {
        if self.start.is_none() && self.stripped_attribute.take().is_some() {
            self.out.keep_attribute(kept_of_attribute(source));
        }
        Ok(())
    }

    /// A `preserve` copy of an element whose binding a `strip` boundary took away: it keeps the
    /// is-id it has by its own typed value and its is-idrefs as its source has them
    /// (`kept_of_element`). Without a run open only (a run's assessment decides otherwise).
    fn element_copied_from(&mut self, source: &Nav<'t>) -> Result<(), XsltError> {
        if self.start.is_none() && std::mem::take(&mut self.stripped_element) {
            let kept = kept_of_element(source);
            if let Some(id) = kept.id {
                self.out.file_id(id);
            }
            if kept.idrefs {
                self.out.element_idrefs()?;
            }
        }
        Ok(())
    }

    /// The is-idrefs property an untyped copied attribute keeps (`Receiver::attribute_idrefs`), as
    /// for its is-id ([`element_id`](Self::element_id)).
    fn attribute_idrefs(&mut self) -> Result<(), XsltError> {
        if self.start.is_none() {
            self.out.attribute_idrefs()?;
        }
        Ok(())
    }

    /// The is-idrefs property an untyped copied element keeps (`Receiver::element_idrefs`), as
    /// for its is-id ([`element_id`](Self::element_id)).
    fn element_idrefs(&mut self) -> Result<(), XsltError> {
        if self.start.is_none() {
            self.out.element_idrefs()?;
        }
        Ok(())
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        match self.start.take() {
            Some(start) => self.finish_start_tag(start),
            None => self.out.end_start_tag(),
        }
    }

    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        if s.is_empty() {
            return Ok(());
        }
        if self.set.is_some() {
            self.check_document_text()?;
        }
        if self.runs.is_empty() {
            return self.out.text(s, disable_escaping);
        }
        match self
            .content_frame()
            .and_then(|frame| self.frames[frame].held.as_mut())
        {
            Some(held) => held.push(Held::Text(s.to_string(), disable_escaping)),
            None => self.pending.push((s.to_string(), disable_escaping)),
        }
        Ok(())
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        if !self.runs.is_empty() {
            if let Some(held) = self
                .content_frame()
                .and_then(|frame| self.frames[frame].held.as_mut())
            {
                held.push(Held::Comment(s.to_string()));
                return Ok(());
            }
            self.flush_pending()?;
        }
        self.out.comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        if !self.runs.is_empty() {
            if let Some(held) = self
                .content_frame()
                .and_then(|frame| self.frames[frame].held.as_mut())
            {
                held.push(Held::Pi(target.to_string(), data.to_string()));
                return Ok(());
            }
            self.flush_pending()?;
        }
        self.out.pi(target, data)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        if self.set.is_none() {
            return self.out.end_element();
        }
        if self.runs.is_empty() {
            self.frames.pop().expect("an element is open");
            return self.out.end_element();
        }
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.deferred.is_some())
        {
            return self.end_deferred();
        }
        let scope = {
            let (frame, outer) = self.frames.split_last().expect("an element is open");
            Scope::of(
                outer
                    .iter()
                    .flat_map(|frame| frame.namespaces.iter())
                    .chain(frame.namespaces[..frame.written_namespaces].iter()),
            )
        };
        let frame = self.frames.pop().expect("an element is open");
        self.ending = Some(Ending {
            scope,
            keep: frame.decider == Decider::Strip,
            kept: None,
            declare: false,
        });
        let content = match frame.held {
            Some(held) => self.settle(held, &frame.views, true),
            None => self.pass_pending(&frame.views, true),
        };
        let ending = self.ending.take();
        content?;
        // §19.2.1.1 under `strip`: "The values of the is-id and is-idrefs properties are
        // unchanged" — those the outermost run inside the boundary gave the element.
        if let Some(kept) = ending.and_then(|ending| ending.kept) {
            self.out.keep_open_element(kept)?;
        }
        // Every open run saw the element start (`finish_start_tag`) and has now seen it end.
        for run in &mut self.runs {
            run.lax_region.pop();
        }
        for _ in 0..frame.roots_runs {
            self.end_run()?;
        }
        self.out.end_element()
    }

    /// Unreachable below a [`ComplexContentNormalizer`](crate::receiver::ComplexContentNormalizer),
    /// which decomposes every item into events before it reaches a tree builder.
    fn append_item(&mut self, _item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        Err(XsltError::internal(
            "Validating must sit behind a ComplexContentNormalizer",
        ))
    }
}
