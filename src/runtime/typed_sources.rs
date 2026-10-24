//! Typed source documents (M4b; M4 plan D-2, D-4, D-13): a source document validated against the
//! stylesheet's schema set as it is parsed, and the admission of typed trees.
//!
//! **One pass.** A source the host asks to validate ([`SourceValidation::Strict`] or
//! [`SourceValidation::Lax`]) is read once by `xsd-schema`'s validation driver, which builds the
//! typed tree as it validates (`TypedBuilderHandler`, upstream C14, with its defaults: defaults as
//! nodes) with this crate's own sink (every error, with its instance path, B8). The validator is
//! made once per transformation (its content models are built when it is made, D-12).
//!
//! **What the tree holds** is the XDM of a PSVI, which `xsd-schema`'s typed build gives since
//! `wave-b` `d5f2a8e` (batch (t)); nothing here repeats it:
//!
//! - whitespace in element-only content is no text node (C67) — XDM 1.0 §6.7.4 (Construction from a
//!   PSVI, read at w3.org): "if the resulting Text Node consists entirely of whitespace and the
//!   character information items used to construct this node have a parent and that parent is an
//!   element and its {content type} is not 'mixed', then the content of the Text Node is the
//!   zero-length string … an empty Text Node will be discarded when its parent is constructed";
//!   XSLT §4.4's note says the same ("elements that are defined in a DTD or a Schema to contain
//!   element-only content will have whitespace text nodes stripped, regardless of the
//!   xsl:strip-space and xsl:preserve-space declarations"). Until batch (t) this crate's own
//!   handler around the typed builder dropped it; the validating receiver of constructed trees
//!   still applies the rule itself (`validate::element_only`), since it does not use the typed
//!   build;
//! - the \[schema normalized value\] as the string value of a valid attribute and of a valid
//!   element with simple content (XDM 1.0 §6.2.4, §6.3.4; C66, `66e3e3f`);
//! - defaulted attributes as attribute nodes, and the `default` / `fixed` value of a valid empty
//!   element with simple content as its text node (XDM 1.0 §6.2.4: "Default and fixed attributes
//!   provided by XML Schema processing are added to the \[attributes\]", "where a fixed or default
//!   value for an element is defined in the schema, and the element takes this default value, a
//!   text node will be created to contain the value"; C39) — so §4.3's Note holds: "any defaulted
//!   elements and attributes that were added to the tree by the validation process will still be
//!   present" after `input-type-annotations="strip"` (`validation-0701` needs the defaulted
//!   `select` of `schema-for-xslt20.xsd`);
//! - an element whose typed value is a single `xs:ID` in the id index (C62), and the is-idrefs
//!   property of elements and attributes (C54).
//!
//! **An invalid document is an error** (D-4), reported with the code the loader reports a document
//! it cannot turn into a tree with: FODC0002 for `fn:doc` and `document()` — F&O 1.0 §15.5.4 groups
//! "an error in retrieving a resource, or in parsing and validating its content" — and XTDE1170 for
//! the principal source, as for its parse errors.
//!
//! **`Lax`** is XML Schema 1.1 §5.2's lax wildcard validation (upstream C36,
//! `ValidationRoot::Lax`; M4f): "If the ·validation root· and the schema determine an element
//! declaration (by the name of the element) … or a type definition (by xsi:type), then ·strict·
//! validation is performed. If they do not identify any declaration or definition, then ·lax
//! assessment· is performed" — a root with no declaration and no `xsi:type` is no error, its
//! declared descendants are validated. The typed build binds no type to an element it assessed
//! laxly without one (it is `xs:untyped` in the tree): XDM 1.0 §3.3.1.1's answer for "[validity]
//! … "notKnown", and the [validation attempted] property … "none"", not for "partial" (a validated
//! descendant), where it gives `xs:anyType` — the same as for every element a lax wildcard matches
//! in a typed source (decision D-f6 of the M4f report; upstream proposal C85).
//!
//! **Admission** of a typed tree (`Transformation::admit_typed`, used by every loader and for a
//! tree the host built) applies §4.3 and §4.4 in their order — "Stripping of type annotations
//! happens before stripping of whitespace text nodes" — in one copy ([`Transformation::copy_typed`]),
//! made only when one of them changes something.

use xsd_schema::document::{
    BufferDocument, BufferDocumentBuilder, BufferDocumentError, BufferDocumentOptions,
    TypedBuildError, TypedBuilderHandler,
};
use xsd_schema::navigator::{DomNavigator, DomNodeType, NamespaceAxisScope, TypedValue};
use xsd_schema::validation::{
    drive_quick_xml_with, ContentType, DriveWithError, SchemaValidator, ValidationError,
    ValidationFlags, ValidationRoot, ValidationSink, ValidationWarning,
};
use xsd_schema::{SchemaSet, TypeKey};

use super::{SourceValidation, Transformation};
use crate::error::{codes, XsltError};
use crate::names::XML_NS;

/// The XML Schema instance namespace (`xsi:nil`).
const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";
use crate::receiver::{kept_of_element, Nav};
use crate::whitespace;

/// How many validation errors a message about an invalid document quotes.
const QUOTED_ERRORS: usize = 3;

/// The validation errors of one document, as the runtime reports them.
#[derive(Default)]
struct Errors {
    first: Vec<ValidationError>,
    count: usize,
}

/// The sink of one validating parse: keeps the first errors and counts the rest; warnings are not
/// errors of the document.
struct Collect<'e>(&'e mut Errors);

impl ValidationSink for Collect<'_> {
    fn on_error(&mut self, error: ValidationError) {
        self.0.count += 1;
        if self.0.first.len() < QUOTED_ERRORS {
            self.0.first.push(error);
        }
    }

    fn on_warning(&mut self, _warning: ValidationWarning) {}
}

/// The schema set a typed document is bound to, compared with the stylesheet's by address (D-13).
fn same_set(doc: &BufferDocument<'_>, set: Option<&SchemaSet>) -> bool {
    match (doc.schema_set(), set) {
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

/// Whether `binding` gives an element a simple type or a complex type with simple content (§4.4).
fn has_simple_content(nav: &Nav<'_>) -> bool {
    match nav.schema_binding() {
        Some(binding) => match binding.type_key {
            TypeKey::Complex(_) => matches!(binding.content_type, Some(ContentType::TextOnly)),
            _ => true,
        },
        None => false,
    }
}

impl<'a, 't> Transformation<'a, 't> {
    /// The validator of this run, made on first use (D-12).
    pub(super) fn validator(&self, set: &'a SchemaSet) -> &SchemaValidator<'a> {
        self.validator.get_or_init(|| {
            // Identity constraints are part of schema validity assessment (XML Schema 1.1 Part 1
            // §3.11.4): enforced for a source document.
            //
            // The PSVI typed values the runtime can keep on each `SchemaInfo`
            // (`BUILD_PSVI_TYPED_VALUES`) are not read here: the typed builder stores bindings, and
            // the navigator computes a typed value from the binding and the string value when one
            // is asked for. The runtime still computes every value validity needs (facets, ID /
            // IDREF, identity constraints), so dropping the flag changes no result (review F9).
            SchemaValidator::new(
                set,
                (ValidationFlags::default() | ValidationFlags::PROCESS_IDENTITY_CONSTRAINTS)
                    - ValidationFlags::BUILD_PSVI_TYPED_VALUES,
            )
        })
    }

    /// Parses and validates `xml` against the stylesheet's schema set (`validation` is `Strict` or
    /// `Lax`), into the arena, unregistered — the typed counterpart of `DocumentStore::parse`.
    pub(super) fn parse_validated(
        &self,
        xml: &str,
        uri: Option<&str>,
        validation: SourceValidation,
        parse_code: &str,
    ) -> Result<&'t BufferDocument<'t>, XsltError> {
        let what = match uri {
            Some(uri) => format!("the document {uri:?}"),
            None => "a document".to_string(),
        };
        // §21.1: a basic processor has no schema, and the typed tree validation would make is input
        // it must refuse — XTDE1665.
        let (Some(set), Some(tree_set)) = (self.sheet.schema_set(), self.schema_set) else {
            return Err(XsltError::new(
                codes::XTDE1665,
                format!(
                    "{what} cannot be validated ({}): this is a basic XSLT processor, whose input \
                     must be untyped (§21.1)",
                    mode_name(validation)
                ),
            ));
        };
        let arena = self.store.borrow().arena();
        let mut errors = Errors::default();
        let (built, failure) = {
            let validator = self.validator(set);
            let mut runtime = validator.start_run(Collect(&mut errors));
            if validation == SourceValidation::Lax {
                runtime.set_validation_root(ValidationRoot::Lax);
            }
            let mut builder = BufferDocumentBuilder::new(
                arena,
                &tree_set.name_table,
                Some(tree_set),
                BufferDocumentOptions::default(),
            )
            .map_err(|e| XsltError::internal(format!("could not start a typed tree: {e}")))?;
            let mut handler = TypedBuilderHandler::new(&mut builder);
            let driven = drive_quick_xml_with(xml.as_bytes(), &mut runtime, set, &mut handler);
            drop(handler);
            let driven = driven.map_err(|e| match e {
                DriveWithError::Io(e) => BufferDocumentError::Io(e),
                DriveWithError::NotWellFormed(e) => BufferDocumentError::NotWellFormed(e),
                DriveWithError::LimitExceeded(e) => BufferDocumentError::LimitExceeded(e),
                DriveWithError::Hook(TypedBuildError::Document(e)) => e,
                DriveWithError::Hook(TypedBuildError::Handler(never)) => match never {},
                DriveWithError::Hook(e) => BufferDocumentError::InternalError(e.to_string()),
                e => BufferDocumentError::InternalError(e.to_string()),
            });
            let built = driven.and_then(|_| builder.finalize());
            // The checks that need the whole document (IDREF without an ID, identity constraints
            // across it) report here; an operational failure comes back as the result.
            let failure = runtime.end_validation().err();
            (built, failure)
        };
        let mut doc =
            built.map_err(|e| XsltError::new(parse_code, format!("could not read {what}: {e}")))?;
        if let Some(failure) = failure {
            if errors.count == 0 {
                errors.count = 1;
                errors.first.push(failure);
            }
        }
        if errors.count > 0 {
            let quoted: Vec<String> = errors.first.iter().map(describe).collect();
            let more = errors.count - errors.first.len();
            return Err(XsltError::new(
                parse_code,
                format!(
                    "{what} is not valid against the stylesheet's schema set ({} validation): {}{}",
                    mode_name(validation),
                    quoted.join("; "),
                    if more > 0 {
                        format!("; and {more} more")
                    } else {
                        String::new()
                    }
                ),
            ));
        }
        if let Some(uri) = uri {
            doc.set_document_base_uri(Some(arena.alloc_str(uri)));
        }
        Ok(arena.alloc(doc))
    }

    /// Admits a **typed** tree into the run (D-13), applying §4.3 and §4.4: returns the tree the
    /// run is to see — `doc` itself, or an admission copy — and whether the copy stands for it.
    ///
    /// * Annotations of a set other than the stylesheet's (a tree the host built elsewhere) cannot be
    ///   read: the tree is stripped (§4.3's stripping), with a warning (D-2).
    /// * `input-type-annotations="strip"` (§4.3) strips them; a basic processor admits a typed source
    ///   tree only that way ("This error will not arise if the input-type-annotations attribute is
    ///   set to strip", §21.1 XTDE1665 — the caller refuses it otherwise).
    /// * `xsl:strip-space` (§4.4) strips whitespace text nodes, except among the children of an
    ///   element whose annotation is a simple type or a complex type with simple content.
    ///
    /// `space_rules` says whether §4.4 applies: for a tree this run loads; not for a tree the host
    /// built (`admit_stored`), whose whitespace is the host's, as for an untyped one.
    pub(super) fn admit_typed(
        &self,
        doc: &'t BufferDocument<'t>,
        uri: Option<&str>,
        space_rules: bool,
    ) -> Result<&'t BufferDocument<'t>, XsltError> {
        let foreign = !same_set(doc, self.sheet.schema_set());
        if foreign && self.sheet.is_schema_aware() {
            self.warnings.borrow_mut().push(format!(
                "{} carries type annotations of another schema set than the stylesheet's, which \
                 the stylesheet cannot read; they were stripped, as §4.3 strips annotations",
                match uri {
                    Some(uri) => format!("the source document {uri:?}"),
                    None => "a source document".to_string(),
                }
            ));
        }
        let strip = foreign || self.sheet.strips_input_annotations();
        let space_rules = space_rules && !self.sheet.space_rules().is_empty();
        if !strip && !space_rules {
            return Ok(doc);
        }
        self.copy_typed(doc, uri, strip, space_rules)
    }

    /// The admission copy of a typed tree: annotations kept or stripped (`strip`; is-id and
    /// is-idrefs kept, §4.3), whitespace stripped by `xsl:strip-space` (§4.4).
    ///
    /// Under `strip` the ids the source's index files for an element are registered on the copy
    /// (`register_xml_id`): those its attributes answer to (an `xml:id`, an `xs:ID`-typed attribute)
    /// and the one its own typed value is (upstream C62, `receiver::kept_of_element`; review F4,
    /// `tests/m4b_review.rs`) — and every element and attribute with the is-idrefs property in the
    /// source has it on the copy (`register_idrefs`, upstream C54), so `fn:id` and `fn:idref` find
    /// on the stripped tree what they find on the typed one.
    fn copy_typed(
        &self,
        doc: &'t BufferDocument<'t>,
        uri: Option<&str>,
        strip: bool,
        space_rules: bool,
    ) -> Result<&'t BufferDocument<'t>, XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let set = if strip { None } else { doc.schema_set() };
        let mut builder =
            BufferDocumentBuilder::new(arena, names, set, BufferDocumentOptions::default())
                .map_err(|e| XsltError::internal(format!("could not start a source tree: {e}")))?;
        let mut child = doc.create_navigator();
        if child.move_to_first_child() {
            loop {
                self.copy_typed_node(&child, false, strip, space_rules, &mut builder)?;
                if !child.move_to_next_sibling() {
                    break;
                }
            }
        }
        let mut copy = builder
            .finalize()
            .map_err(|e| XsltError::internal(format!("could not finish a source tree: {e}")))?;
        let base = uri.or(doc.document_base_uri());
        if let Some(base) = base {
            copy.set_document_base_uri(Some(arena.alloc_str(base)));
        }
        Ok(arena.alloc(copy))
    }

    fn copy_typed_node(
        &self,
        nav: &Nav<'t>,
        preserve_space: bool,
        strip: bool,
        space_rules: bool,
        out: &mut BufferDocumentBuilder<'t>,
    ) -> Result<(), XsltError> {
        let refused = |e: BufferDocumentError| {
            XsltError::internal(format!("the source-tree builder refused: {e}"))
        };
        match nav.node_type() {
            DomNodeType::Element => {
                let mut preserve = preserve_space;
                let mut decls: Vec<(String, String)> = Vec::new();
                let mut ns = nav.clone();
                if ns.move_to_first_namespace(NamespaceAxisScope::Local) {
                    loop {
                        decls.push((ns.local_name().to_string(), ns.value()));
                        if !ns.move_to_next_namespace(NamespaceAxisScope::Local) {
                            break;
                        }
                    }
                }
                let decls: Vec<(&str, &str)> = decls
                    .iter()
                    .map(|(p, u)| (p.as_str(), u.as_str()))
                    .collect();
                let elem = out
                    .start_element(nav.local_name(), nav.namespace_uri(), nav.prefix(), &decls)
                    .map_err(refused)?;
                // §4.3: "The values of the is-id and is-idrefs properties are not changed" — the
                // ids the element answers to in the source (by its attributes and by its own typed
                // value), registered on the copy, and the is-idrefs property of the element and its
                // attributes.
                let mut ids: Vec<String> = Vec::new();
                // Whether the element carries `xsi:nil`: only then can it be nilled, so only then is
                // its typed value asked for (it is computed, not stored).
                let mut has_xsi_nil = false;
                let mut attr = nav.clone();
                if attr.move_to_first_attribute() {
                    loop {
                        let value = attr.value();
                        if attr.local_name() == "space" && attr.namespace_uri() == XML_NS {
                            preserve = whitespace::trim(&value) == "preserve";
                        }
                        if attr.local_name() == "nil" && attr.namespace_uri() == XSI_NS {
                            has_xsi_nil = true;
                        }
                        let node = out
                            .attribute(
                                attr.local_name(),
                                attr.namespace_uri(),
                                attr.prefix(),
                                &value,
                            )
                            .map_err(refused)?;
                        if strip {
                            let owner = nav.current_ref();
                            if attr.is_idrefs() {
                                out.register_idrefs(node);
                            }
                            if nav.document().get_element_by_id_in_tree(owner, &value)
                                == Some(owner)
                            {
                                ids.push(value);
                            }
                        } else if let Some(binding) = attr.schema_binding() {
                            out.set_node_binding(node, *binding).map_err(refused)?;
                        }
                        if !attr.move_to_next_attribute() {
                            break;
                        }
                    }
                }
                out.end_of_attributes();
                if !strip {
                    if let Some(binding) = nav.schema_binding() {
                        out.set_node_binding(elem, *binding).map_err(refused)?;
                    }
                    // Nilled follows the PSVI, whatever the children: XML Schema leaves a nilled
                    // element its comments and processing instructions (cvc-elt.3.2.1 forbids
                    // character and element children only; review F2).
                    if has_xsi_nil && matches!(nav.typed_value(), TypedValue::Nilled) {
                        out.set_nil(elem);
                    }
                }
                if strip {
                    // The element's own: the id its typed value is (C62), its is-idrefs (C54).
                    let kept = kept_of_element(nav);
                    ids.extend(kept.id);
                    if kept.idrefs {
                        out.register_idrefs(elem);
                    }
                }
                for id in &ids {
                    out.register_xml_id(id, elem).map_err(refused)?;
                }
                // §4.4: the simple-content exemption reads the annotation, which `strip` has
                // already removed ("Stripping of type annotations happens before stripping of
                // whitespace text nodes").
                let strip_children = space_rules
                    && !preserve
                    && self.strips(nav)
                    && (strip || !has_simple_content(nav));
                let mut child = nav.clone();
                if child.move_to_first_child() {
                    loop {
                        let drop = strip_children
                            && child.node_type().is_text_like()
                            && whitespace::is_all_xml_whitespace(&child.value());
                        if !drop {
                            self.copy_typed_node(&child, preserve, strip, space_rules, out)?;
                        }
                        if !child.move_to_next_sibling() {
                            break;
                        }
                    }
                }
                out.end_element().map_err(refused)
            }
            DomNodeType::Comment => out.comment(&nav.value()).map_err(refused),
            DomNodeType::ProcessingInstruction => out
                .processing_instruction(nav.local_name(), &nav.value())
                .map_err(refused),
            _ => {
                out.text(&nav.value());
                Ok(())
            }
        }
    }
}

/// `strict` or `lax`, for messages.
fn mode_name(validation: SourceValidation) -> &'static str {
    match validation {
        SourceValidation::Skip => "skip",
        SourceValidation::Strict => "strict",
        SourceValidation::Lax => "lax",
    }
}

/// One validation error, with the node it is about (B8's instance path).
fn describe(error: &ValidationError) -> String {
    match error.instance_path() {
        Some(path) => format!("[{}] {} at {path}", error.constraint, error.message),
        None => format!("[{}] {}", error.constraint, error.message),
    }
}
