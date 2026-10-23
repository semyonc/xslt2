//! Validation of constructed nodes (§19.2; M4 plan §4 M4d, decisions D-5, D-10, D-12).
//!
//! Two halves: what `xsl:attribute` validates when it is evaluated
//! ([`Transformation::validate_attribute`]), and the validating receiver every tree pipeline of a
//! transformation has in front of its builder ([`Validating`]), which applies the annotation rule
//! over the boundary each element and document constructor announces and validates every
//! `strict` element boundary through `xsd-schema`'s push API.
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
//! [`Validating`]). `lax` and `type` on elements (M4f), and copies and document nodes with a
//! validation other than `strip` (M4e) stay fenced.

use std::cell::RefCell;
use std::rc::Rc;

use xsd_schema::document::NodeSchemaBinding;
use xsd_schema::ids::TypeKey;
use xsd_schema::namespace::context::NamespaceContextSnapshot;
use xsd_schema::parser::frames::SimpleTypeVariety;
use xsd_schema::types::value::{XmlAtomicValue, XmlValue, XmlValueKind};
use xsd_schema::types::XmlTypeCode;
use xsd_schema::validation::simple::fixed_values_equal;
use xsd_schema::validation::{
    validate_simple_type, ContentType, SchemaInfo, SchemaValidator, SchemaValidity,
    ValidationError, ValidationRuntime, ValidationSink, ValidationWarning,
};
use xsd_schema::xpath::XmlItem;
use xsd_schema::SchemaSet;

use super::eval::validation_fence;
use super::{typed_sources, Transformation};
use crate::error::{codes, XsltError};
use crate::model::{Constructor, Validation};
use crate::names::{QName, XSD_NS};
use crate::receiver::{Boundary, Name, Nav, Receiver, ResultBuilder};
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
            Validation::Type(type_name)
                if type_name.uri == XSD_NS && type_name.local == "untypedAtomic" =>
            {
                Ok((value, None))
            }
            Validation::Type(type_name) => {
                let set = self.validating_set()?;
                let type_key = resolve_type(set, type_name).ok_or_else(|| {
                    // `xs:anyAtomicType`, `xs:yearMonthDuration` and `xs:dayTimeDuration` are
                    // XPath 2.0 types (§3.13 lists them as in scope) that an XML Schema 1.0 set has
                    // no definitions of; a 1.1 set has them (M4g's mode).
                    XsltError::not_implemented(
                        format!(
                            "type=\"{type_name}\" on xsl:attribute with an XML Schema 1.0 schema \
                             set, which has no definition of the type (upstream proposal C72)"
                        ),
                        "M4g",
                    )
                })?;
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

    /// The validator the trees of this run validate constructed elements with — the run's one
    /// validator (D-12), made only when the stylesheet validates something (`validates`).
    pub(crate) fn construction_validator(&self) -> Option<&SchemaValidator<'a>> {
        let set = self.sheet.schema_set()?;
        self.sheet.validates.then(|| self.validator(set))
    }

    /// A [`Validating`] receiver over a new builder for a tree of this run.
    pub(crate) fn validating(&self, builder: ResultBuilder<'t>) -> Validating<'_, 't> {
        Validating::new(builder, self.schema_set, self.construction_validator())
    }

    /// A sequence collector of this run, whose groups validate as the trees do.
    pub(crate) fn collector(&self) -> crate::receiver::SequenceCollector<'_, 't> {
        crate::receiver::SequenceCollector::new(&self.store)
            .with_validator(self.construction_validator())
    }

    /// The schema set validation runs against: there is one whenever a validation other than
    /// `strip` gets this far, because a basic processor rejects any other value (XTSE1660, §21.1).
    fn validating_set(&self) -> Result<&'t SchemaSet, XsltError> {
        self.schema_set.ok_or_else(|| {
            XsltError::internal("validation of a constructed node without a schema set")
        })
    }
}

/// The boundary an element constructor (a literal result element, `xsl:element`, `xsl:copy` of an
/// element) announces for `validation`: `strip`, `preserve` and `strict` (§19.2.1.1); every other
/// value raises the fence of its owner — `lax` and `type` M4f, `xsl:copy` with anything but
/// `strip` M4e.
pub(super) fn element_boundary(
    validation: &Validation,
    constructor: Constructor,
    what: &str,
) -> Result<Boundary, XsltError> {
    match validation {
        Validation::Strip => Ok(Boundary::Strip),
        Validation::Preserve if constructor == Constructor::Element => Ok(Boundary::Preserve),
        Validation::Strict if constructor == Constructor::Element => Ok(Boundary::Strict),
        _ => {
            validation_fence(validation, constructor, what)?;
            unreachable!("only strip and preserve on an element constructor pass the fence")
        }
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
/// validation of every `strict` element boundary through `xsd-schema`'s push API.
///
/// **The annotation rule.** "Conceptually, the validation requested for a child element or
/// attribute node is applied before the validation requested for its parent element" — so the
/// outermost enclosing boundary that is not `preserve` decides a node's annotation ([`Decider`]):
/// under a `strip` one the node is untyped and not nilled (an attribute that loses an `xs:ID`
/// annotation still identifies its element: "The values of the is-id and is-idrefs properties are
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
/// optional C38 would make that a flag) — while "Identity-constraint Satisfied" is
/// (`PROCESS_IDENTITY_CONSTRAINTS`). When the root ends the run ends (`end_validation`), and any
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
///   content of the Text Node is the zero-length string" — `typed_sources::element_only`, the
///   rule the typed source build applies).
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
}

/// One open container (element or document node).
struct Frame {
    decider: Decider,
    /// The boundary the container's instruction announced.
    boundary: Option<Boundary>,
    /// The container is an element that roots the innermost run.
    roots_run: bool,
    /// The namespace declarations of the element, as the normalizer wrote them.
    namespaces: Vec<(String, String)>,
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

struct StartTag {
    name: Name,
    binding: Option<NodeSchemaBinding>,
    nilled: bool,
    attributes: Vec<(Name, String, Option<NodeSchemaBinding>)>,
}

/// One validation run: a `strict` boundary's.
struct Run<'v> {
    runtime: ValidationRuntime<'v, RunSink>,
    errors: Rc<RefCell<Vec<ValidationError>>>,
    root: Name,
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
        }
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

    /// The `xs:ID` value an attribute of type `type_key` with `value` has — its is-id property —
    /// as the upstream builder decides it for a bound attribute: the typed value is exactly one
    /// atomic value of type `xs:ID` or a type derived from it.
    fn id_value(&self, value: &str, type_key: TypeKey) -> Option<String> {
        let set = self.set?;
        let typed = validate_simple_type(value, type_key, set).ok()?.typed_value;
        single_id(&typed).map(str::to_string)
    }

    /// The decider of a container opened inside the current one with `boundary`.
    fn decider_of(&self, boundary: Option<Boundary>, run: Option<usize>) -> Decider {
        match self.decider() {
            Decider::Nothing => match (boundary, run) {
                (Some(Boundary::Strip), _) => Decider::Strip,
                (Some(Boundary::Strict), Some(run)) => Decider::Run(run),
                _ => Decider::Nothing,
            },
            outer => outer,
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
    fn through_runs(
        &mut self,
        views: &[View],
        mut text: String,
        end: bool,
        simple: bool,
    ) -> (String, bool) {
        let mut changed = false;
        for index in (0..self.runs.len()).rev() {
            let runtime = &mut self.runs[index].runtime;
            if !text.is_empty() {
                if whitespace::is_all_xml_whitespace(&text) {
                    runtime.validate_whitespace(&text);
                } else {
                    runtime.validate_text(&text);
                }
            }
            let view = views.get(index);
            if end {
                let info = runtime.validate_end_element();
                if let (
                    true,
                    Some(View::Simple {
                        type_key,
                        default_value,
                    }),
                ) = (simple, view)
                {
                    if info.validity == SchemaValidity::Valid && !info.is_nil {
                        if let Some(default) = default_value.as_ref().filter(|_| text.is_empty()) {
                            text = default.clone();
                            changed |= !text.is_empty();
                        }
                        if let Some(normalized) = self.set.and_then(|set| {
                            schema_normalized(info.normalized_value, &text, *type_key, set)
                        }) {
                            text = normalized;
                            changed = true;
                        }
                    }
                }
            }
            if matches!(view, Some(View::ElementOnly))
                && !text.is_empty()
                && whitespace::is_all_xml_whitespace(&text)
            {
                text.clear();
                changed = true;
            }
        }
        (text, changed)
    }

    /// Passes the pending text of the current text node on — through the open runs (`views`, `end`
    /// as for [`through_runs`](Self::through_runs)), and to the builder unless a run dropped it
    /// (whitespace in element-only content: the only change a run makes to text that is not held).
    fn pass_pending(&mut self, views: &[View], end: bool) -> Result<(), XsltError> {
        let mut chunks = std::mem::take(&mut self.pending);
        if chunks.len() <= 1 {
            let (text, disable_escaping) = chunks.pop().unwrap_or_default();
            let (text, changed) = self.through_runs(views, text, end, false);
            if !changed && !text.is_empty() {
                self.out.text(&text, disable_escaping)?;
            }
            return Ok(());
        }
        let text: String = chunks.iter().map(|(text, _)| text.as_str()).collect();
        if !self.through_runs(views, text, end, false).1 {
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
        let (value, changed) = self.through_runs(views, held_text(&held), end, end);
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

    /// A prefix bound to `uri` in the scope of the held start tag, or a new one declared on it.
    fn prefix_for(&mut self, uri: &str) -> String {
        let mut bound: Vec<(&str, &str)> = Vec::new();
        for frame in &self.frames {
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
        if let Some(frame) = self.frames.last_mut() {
            frame.namespaces.push((prefix.clone(), uri.to_string()));
        }
        prefix
    }

    /// The held start tag, now complete: through every open run, innermost first — each run
    /// seeing the attribute values the runs inside it normalized and the attributes they
    /// defaulted —, then on to the builder with the values the outermost run gave and the bindings
    /// the decider gives.
    fn finish_start_tag(&mut self, start: StartTag) -> Result<(), XsltError> {
        let set = self.set.ok_or_else(|| {
            XsltError::internal("validation of a constructed element without a schema set")
        })?;
        let snapshot = self.snapshot(set);
        let xsi = |local: &str| {
            start
                .attributes
                .iter()
                .find(|(name, _, _)| name.uri == XSI_NS && name.local == local)
                .map(|(_, value, _)| value.clone())
        };
        let (xsi_type, xsi_nil) = (xsi("type"), xsi("nil"));
        let runs = self.runs.len();
        let mut slots: Vec<AttributeSlot> = start
            .attributes
            .into_iter()
            .map(|(name, value, incoming)| AttributeSlot {
                name,
                value,
                incoming,
                infos: (0..runs).map(|_| None).collect(),
                defaulted_by: None,
            })
            .collect();
        let mut element_infos: Vec<Option<SchemaInfo>> = (0..runs).map(|_| None).collect();
        for index in (0..runs).rev() {
            let mut info = self.runs[index].runtime.validate_element(
                &start.name.local,
                &start.name.uri,
                xsi_type.as_deref(),
                xsi_nil.as_deref(),
                &snapshot,
            );
            for slot in &mut slots {
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
                        if let Some(value) = schema_normalized(
                            info.normalized_value.clone(),
                            &slot.value,
                            type_key,
                            set,
                        ) {
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
            let _ = self.runs[index].runtime.take_deferred_attribute_results();
            for default in self.runs[index].runtime.get_default_attributes() {
                let local = set.name_table.resolve(default.local_name);
                let uri = default
                    .namespace
                    .map(|ns| set.name_table.resolve(ns))
                    .unwrap_or_default();
                if slots
                    .iter()
                    .any(|slot| slot.name.local == local && slot.name.uri == uri)
                {
                    continue;
                }
                let prefix = if uri.is_empty() {
                    String::new()
                } else {
                    self.prefix_for(&uri)
                };
                let type_key = set.arenas.attributes[default.attribute_key]
                    .resolved_type
                    .unwrap_or_else(|| TypeKey::Simple(set.builtin_types().any_simple_type));
                let mut infos: Vec<Option<SchemaInfo>> = (0..runs).map(|_| None).collect();
                infos[index] = Some(SchemaInfo {
                    schema_type: Some(type_key),
                    attribute_decl: Some(default.attribute_key),
                    validity: SchemaValidity::Valid,
                    is_default: true,
                    ..SchemaInfo::empty()
                });
                // XML Schema 1.1 §3.4.5.1 (Attribute Default Value): its [schema normalized value]
                // is "The ·effective value constraint·'s {lexical form}", which §3.2.2.1 defines as
                // "the ·normalized value· (with respect to the {type definition}) of the
                // [attribute]" `default` / `fixed`. A run outside this one validates it like any
                // other attribute.
                let value = normalized(&default.value, type_key, set).unwrap_or(default.value);
                slots.push(AttributeSlot {
                    name: Name::qualified(&prefix, &local, &uri),
                    value,
                    incoming: None,
                    infos,
                    defaulted_by: Some(index),
                });
            }
            element_infos[index] = Some(info);
        }

        // What each run assessed the content as, for the values it gives the content.
        let views: Vec<View> = element_infos
            .iter()
            .map(|info| match info {
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
                    None if typed_sources::element_only(info) => View::ElementOnly,
                    None => View::Other,
                },
                _ => View::Other,
            })
            .collect();
        if let Some(frame) = self.frames.last_mut() {
            if views.iter().any(|view| matches!(view, View::Simple { .. })) {
                frame.held = Some(Vec::new());
            }
            frame.views = views;
        }

        let decider = self.decider();
        let (binding, nilled) = match decider {
            Decider::Strip => (None, false),
            Decider::Run(run) => {
                let info = element_infos[run]
                    .as_ref()
                    .expect("every run saw the element");
                let binding = match info.schema_type {
                    Some(type_key) => Some(NodeSchemaBinding {
                        type_key,
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
            Decider::Nothing => {
                let own = self.frames.last().and_then(|frame| frame.boundary);
                if own == Some(Boundary::Preserve) {
                    (self.any_type(), false)
                } else {
                    (start.binding, start.nilled)
                }
            }
        };
        self.out.start_element(&start.name, binding, nilled)?;
        let namespaces = self
            .frames
            .last()
            .map(|frame| frame.namespaces.clone())
            .unwrap_or_default();
        for (prefix, uri) in &namespaces {
            self.out.namespace(prefix, uri)?;
        }
        for slot in slots {
            let binding = match decider {
                Decider::Strip => {
                    // is-id as the binding the node had just before the strip: the one the
                    // outermost run inside the `strip` boundary gave it (run 0 — any run outside
                    // that boundary would decide instead), or its own.
                    let proposed = match slot.infos.first() {
                        Some(info) => info.as_ref().and_then(attribute_binding),
                        None => slot.incoming,
                    };
                    if let Some(binding) = proposed {
                        if let Some(id) = self.id_value(&slot.value, binding.type_key) {
                            self.out.file_id(id);
                        }
                    }
                    None
                }
                Decider::Run(run) => slot.infos[run].as_ref().and_then(attribute_binding),
                Decider::Nothing => slot.incoming,
            };
            self.out.attribute(&slot.name, &slot.value, binding)?;
        }
        self.out.end_start_tag()
    }

    /// Ends the innermost run, rooted at the element that just ended: XTTE1510 for any error it
    /// reported but the two ID/IDREF ones §19.2.1.3 excludes.
    fn end_run(&mut self) -> Result<(), XsltError> {
        let Some(mut run) = self.runs.pop() else {
            return Ok(());
        };
        let failure = run.runtime.end_validation().err();
        let mut errors: Vec<ValidationError> = std::mem::take(&mut *run.errors.borrow_mut());
        errors.extend(failure);
        // §19.2.1.3: "The validation rule "Validation Root Valid (ID/IDREF)" is not applied."
        errors.retain(|error| !matches!(error.constraint, "cvc-id.1" | "cvc-id.2"));
        let Some(first) = errors.first() else {
            return Ok(());
        };
        let more = match errors.len() {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        Err(XsltError::new(
            codes::XTTE1510,
            format!(
                "validation=\"strict\": the element {} is not valid against its top-level \
                 declaration: {first}{more}",
                display(&run.root)
            ),
        ))
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

/// The `xs:ID` value a typed value consists of, when it is exactly one atomic value of type
/// `xs:ID` or a type derived from it (every type derived from `xs:ID` validates to the `xs:ID`
/// type code); a union value counts by its member's value, a list value when it has exactly one
/// item.
fn single_id(value: &XmlValue) -> Option<&str> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::String(id)) if value.type_code == XmlTypeCode::Id => {
            Some(id)
        }
        XmlValueKind::Union(member) => single_id(member),
        XmlValueKind::List { items, .. } => match items.as_slice() {
            [member] => single_id(member),
            _ => None,
        },
        _ => None,
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
        let decider = self.decider_of(boundary, None);
        let transparent = !self.frames.is_empty();
        self.frames.push(Frame {
            decider,
            boundary,
            roots_run: false,
            namespaces: Vec::new(),
            transparent,
            views: Vec::new(),
            held: None,
        });
        self.out.start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        if self.set.is_some() {
            self.frames.pop();
        }
        self.out.end_document()
    }

    fn validation_boundary(&mut self, boundary: Boundary) -> Result<(), XsltError> {
        self.next_boundary = Some(boundary);
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
                Some(frame) if self.frames[frame].held.is_some() => self.release_held(frame)?,
                _ => self.flush_pending()?,
            }
        }
        let boundary = self.next_boundary.take();
        let mut run = None;
        if boundary == Some(Boundary::Strict) {
            let (Some(set), Some(validator)) = (self.set, self.validator) else {
                return Err(XsltError::internal(
                    "a strict element boundary in a tree built without a validator",
                ));
            };
            // XTTE1512: "there is no matching top-level declaration in the schema" — decided
            // before validating.
            let names = &set.name_table;
            let declared = names.get(&name.local).is_some_and(|local| {
                let ns = if name.uri.is_empty() {
                    Some(None)
                } else {
                    names.get(&name.uri).map(Some)
                };
                ns.is_some_and(|ns| set.lookup_element(ns, local).is_some())
            });
            if !declared {
                return Err(XsltError::new(
                    codes::XTTE1512,
                    format!(
                        "validation=\"strict\": there is no top-level declaration of the element {}",
                        display(name)
                    ),
                ));
            }
            let errors = Rc::new(RefCell::new(Vec::new()));
            self.runs.push(Run {
                runtime: validator.start_run(RunSink(Rc::clone(&errors))),
                errors,
                root: name.clone(),
            });
            run = Some(self.runs.len() - 1);
        }
        let decider = self.decider_of(boundary, run);
        self.frames.push(Frame {
            decider,
            boundary,
            roots_run: run.is_some(),
            namespaces: Vec::new(),
            transparent: false,
            views: Vec::new(),
            held: None,
        });
        if self.runs.is_empty() {
            let (binding, nilled) = match decider {
                Decider::Strip => (None, false),
                _ if boundary == Some(Boundary::Preserve) => (self.any_type(), false),
                _ => (binding, nilled),
            };
            return self.out.start_element(name, binding, nilled);
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
        let binding = match (self.decider(), binding) {
            (Decider::Strip, Some(stripped)) => {
                if let Some(id) = self.id_value(value, stripped.type_key) {
                    self.out.file_id(id);
                }
                None
            }
            (_, binding) => binding,
        };
        self.out.attribute(name, value, binding)
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
        let frame = self.frames.pop().expect("an element is open");
        if self.runs.is_empty() {
            return self.out.end_element();
        }
        match frame.held {
            Some(held) => self.settle(held, &frame.views, true)?,
            None => self.pass_pending(&frame.views, true)?,
        }
        if frame.roots_run {
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
