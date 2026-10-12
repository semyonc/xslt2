//! Final result trees (§19.1, §2.4): `xsl:result-document`, the base output
//! URI, and the two rules about where a final result tree may be created —
//! XTDE1480 (only in final output state) and XTDE1490 (never twice at one
//! URI).

use std::cell::Cell;
use std::collections::HashSet;

use xsd_schema::document::{BufferDocument, NodeSchemaBinding};
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::compile::result_document::{computed_value_error, format_name};
use crate::error::{codes, XsltError};
use crate::model::{Instruction, ResultDocument, ResultFormat};
use crate::output::{apply_serialization_attribute, Merge, OutputDefinition};
use crate::receiver::{
    ComplexContentNormalizer, CopyNamespaces, Name, Nav, Receiver, ResultBuilder,
};
use crate::serialize::EscapingMarks;
use crate::whitespace;

/// A final result tree an `xsl:result-document` instruction created (§19.1),
/// other than the primary result.
///
/// > The `xsl:result-document` instruction defines the URI of the result tree,
/// > and may optionally specify the output format to be used for serializing
/// > this tree.
///
/// Serialize it with `CompiledStylesheet::serialize_secondary` (or
/// `serialize_secondary_to_bytes`), which applies [`output`](Self::output) and
/// the tree's `disable-output-escaping` marks.
pub struct SecondaryResult<'t> {
    /// The URI that identifies the tree: the effective value of `href`
    /// (whitespace-trimmed), resolved against the base output URI by RFC 3986
    /// §5.2 (§19.1: "If the effective value is a relative URI, then it is
    /// resolved relative to the base output URI"). It is also the base URI of
    /// the tree's document node (unless it is empty).
    ///
    /// It is absolute when the base output URI is (or `href` is absolute);
    /// against a relative base output URI it is relative, and against an empty
    /// one it is the `href` itself with its dot segments removed — see
    /// `TransformOptions::base_output_uri`. It is spelled as resolution leaves
    /// it: a character a URI may not contain (`a b.xml`) is not escaped here,
    /// as no other base URI this crate reports is; the escaped and the
    /// syntax-normalized spellings are only what URIs are *compared* in
    /// (XTDE1490, `Outcome::result_document`).
    pub uri: String,
    /// The tree.
    pub document: &'t BufferDocument<'t>,
    /// The characters of the tree's text nodes written with
    /// `disable-output-escaping="yes"` (§20.2): the instruction's content is
    /// evaluated in final output state, so each tree has marks of its own.
    pub disabled_escaping: EscapingMarks,
    /// The effective output definition: the one `format` selects (the unnamed
    /// one when it is absent), overridden and supplemented by the serialization
    /// attributes written on the instruction (§19.1).
    ///
    /// The instruction's own `use-character-maps` are in it twice over, like
    /// a declaration's: expanded into
    /// [`character_map`](OutputDefinition::character_map), after the selected
    /// definition's maps ("the character maps referenced in this instruction
    /// supplement and take precedence over those defined in the selected output
    /// definition"), and as names in
    /// [`use_character_maps`](OutputDefinition::use_character_maps) under the
    /// key `(u32::MAX, 0)`, above every declaration, so that
    /// [`character_map_names`](OutputDefinition::character_map_names) lists
    /// them last — the order the map was built in, a later mapping winning.
    pub output: OutputDefinition,
}

/// What a run keeps about its final result trees.
///
/// URIs are compared in their `resolver::syntax_normalized` form (§19.1
/// XTDE1490 speaks of "the same URI"; RFC 3986 §6.2.2 spellings of one URI are
/// one URI); the trees keep the spelling resolution gave them.
#[derive(Default)]
pub(super) struct FinalResults<'t> {
    /// The base output URI of the run (§2.3), which is also the URI of the
    /// final result tree created implicitly (§2.4).
    pub(super) base_output_uri: String,
    /// [`Self::base_output_uri`], normalized for comparison.
    base_output_key: String,
    /// The URI of every final result tree an `xsl:result-document` has
    /// started, normalized for comparison, for XTDE1490.
    uris: HashSet<String>,
    /// The trees, in the order their instructions were evaluated; `None` while
    /// one is still being built (an `xsl:result-document` nested in another
    /// one finishes first).
    trees: Vec<Option<SecondaryResult<'t>>>,
    /// The index in `trees` of the one an `xsl:result-document` created at
    /// the base output URI, if one did.
    at_base_output_uri: Option<usize>,
}

impl<'t> FinalResults<'t> {
    pub(super) fn new(base_output_uri: String) -> Self {
        Self {
            base_output_key: crate::resolver::syntax_normalized(&base_output_uri),
            base_output_uri,
            ..Self::default()
        }
    }
}

/// The trees a run delivers: the primary result and the others.
pub(super) struct Delivered<'t> {
    pub(super) primary: Option<(&'t BufferDocument<'t>, EscapingMarks)>,
    /// `Some` when the primary result is a tree an `xsl:result-document` made.
    pub(super) primary_output: Option<OutputDefinition>,
    pub(super) secondary: Vec<SecondaryResult<'t>>,
}

impl<'a, 't> Transformation<'a, 't> {
    /// Evaluates one `xsl:result-document` (§19.1).
    pub(super) fn result_document(
        &self,
        instruction: &Instruction,
        rd: &ResultDocument,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<(), XsltError> {
        // > [ERR XTDE1480] It is a non-recoverable dynamic error to evaluate
        // > the xsl:result-document instruction in temporary output state.
        if !cx.final_output {
            return Err(XsltError::new(
                codes::XTDE1480,
                "xsl:result-document is evaluated in temporary output state: inside a variable, \
                 parameter, attribute, comment, processing instruction, namespace node, \
                 xsl:value-of, xsl:message, sort key, key, stylesheet function or xsl:document",
            ));
        }
        let sc = self.sheet.static_contexts.get(instruction.ctx);
        let resolve = |prefix: &str| sc.resolve_prefix(prefix);

        // The output definition `format` selects.
        let mut output = match &rd.format {
            None => self.sheet.output().clone(),
            Some(format) => {
                let name = match format {
                    ResultFormat::Named(name) => name.clone(),
                    ResultFormat::Computed(avt) => {
                        format_name(&self.avt(avt, frame, cx)?, &resolve)?
                    }
                };
                self.sheet.named_output(&name).cloned().ok_or_else(|| {
                    XsltError::new(
                        codes::XTDE1460,
                        format!("the format {name} matches no named output definition"),
                    )
                })?
            }
        };
        // "For any of these attributes that is present on the
        // xsl:result-document instruction, the effective value of the
        // attribute overrides or supplements the corresponding value from the
        // output definition."
        for (parameter, avt) in &rd.serialization {
            let value = self.avt(avt, frame, cx)?;
            apply_serialization_attribute(
                &mut output,
                parameter,
                &value,
                &resolve,
                Merge::Override,
            )
            .map_err(|e| {
                // A written value was checked when the stylesheet was
                // compiled; only a computed one can fail here.
                if avt.as_literal().is_some() {
                    e
                } else {
                    computed_value_error(e)
                }
            })?;
        }
        // "the character maps referenced in this instruction supplement and
        // take precedence over those defined in the selected output
        // definition": merged after them, so a later mapping wins (§20.1).
        for name in &rd.use_character_maps {
            let map = self.sheet.character_map(name).ok_or_else(|| {
                XsltError::new(
                    codes::XTSE1590,
                    format!("use-character-maps names {name}, which no xsl:character-map declares"),
                )
            })?;
            output.character_map.extend_from(map);
        }
        // The names too, after the definition's (`character_map_names()` is
        // then the order the map above was built in), under a precedence no
        // `xsl:output` declaration has — the one `Setting::overridden` uses.
        if !rd.use_character_maps.is_empty() {
            output
                .use_character_maps
                .insert((u32::MAX, 0), rd.use_character_maps.clone());
        }

        // "The href attribute is optional. The default value is the
        // zero-length string. … If the effective value is a relative URI, then
        // it is resolved relative to the base output URI."
        let href = match &rd.href {
            Some(avt) => self.avt(avt, frame, cx)?,
            None => String::new(),
        };
        let (slot, uri) = {
            let mut results = self.results.borrow_mut();
            let uri =
                crate::resolver::resolve_uri(&results.base_output_uri, whitespace::trim(&href));
            // > [ERR XTDE1490] It is a non-recoverable dynamic error for a
            // > transformation to generate two or more final result trees with
            // > the same URI.
            //
            // The same URI, not the same spelling: compared once normalized.
            // The tree created implicitly takes the base output URI once the
            // initial template's result is known to be non-empty (§2.4) —
            // which it already is if anything has been written to it.
            let key = crate::resolver::syntax_normalized(&uri);
            let at_base_output_uri = key == results.base_output_key;
            let implicit_taken = at_base_output_uri && self.primary_written.get();
            if implicit_taken || !results.uris.insert(key) {
                return Err(XsltError::new(
                    codes::XTDE1490,
                    format!("a final result tree with the URI {uri:?} has already been created"),
                ));
            }
            results.trees.push(None);
            let slot = results.trees.len() - 1;
            if at_base_output_uri {
                results.at_base_output_uri = Some(slot);
            }
            (slot, uri)
        };

        // "A document node is created, and the sequence obtained by evaluating
        // the sequence constructor is used to construct the content of the
        // document, as described in 5.7.1" — in final output state, the state
        // of the instruction itself, so `disable-output-escaping` is honoured
        // (§20.2) and recorded with this tree.
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::new(arena, names, None)?;
        let mut out = ComplexContentNormalizer::new(builder).honouring_disabled_escaping();
        out.start_document()?;
        self.eval_body(&rd.body, frame, cx, &mut out)?;
        out.end_document()?;
        let (mut doc, disabled_escaping) = out.into_inner().finish_with_marks()?;
        // "The base URI of the document node at the root of the final result
        // tree is based on the effective value of the href attribute."
        if !uri.is_empty() {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(&uri)));
        }
        let document = {
            let mut store = self.store.borrow_mut();
            let id = store.insert(doc, None);
            store.get(id).expect("just inserted").doc
        };
        self.results.borrow_mut().trees[slot] = Some(SecondaryResult {
            uri,
            document,
            disabled_escaping,
            output,
        });
        // "Technically, the result of evaluating the xsl:result-document
        // instruction is an empty sequence."
        Ok(())
    }

    /// The run's final result trees once the initial template has finished
    /// (§2.4), given the tree built from its result.
    ///
    /// > If the result sequence is non-empty, then it is used to construct an
    /// > implicit final result tree … An implicit result tree is also created
    /// > when the result sequence is empty, provided that no
    /// > xsl:result-document instruction has been evaluated during the course
    /// > of the transformation. In this situation the implicit result tree will
    /// > consist of a document node with no children.
    ///
    /// Otherwise there is no implicit tree, and the primary result is the tree
    /// an `xsl:result-document` created at the base output URI, if one did.
    pub(super) fn deliver(
        &self,
        implicit: (&'t BufferDocument<'t>, EscapingMarks),
    ) -> Result<Delivered<'t>, XsltError> {
        let mut results = std::mem::take(&mut *self.results.borrow_mut());
        if self.primary_written.get() || results.uris.is_empty() {
            // "If the result of the initial template is non-empty, and an
            // explicit xsl:result-document instruction has been evaluated with
            // the empty attribute href="", then an error will occur [see ERR
            // XTDE1490]".
            if results.at_base_output_uri.is_some() {
                return Err(XsltError::new(
                    codes::XTDE1490,
                    format!(
                        "the initial template's result is a final result tree at the base output \
                         URI {:?}, where an xsl:result-document has already created one",
                        results.base_output_uri
                    ),
                ));
            }
            return Ok(Delivered {
                primary: Some(implicit),
                primary_output: None,
                secondary: results.trees.into_iter().flatten().collect(),
            });
        }
        let explicit = results
            .at_base_output_uri
            .and_then(|slot| results.trees[slot].take());
        let secondary = results.trees.into_iter().flatten().collect();
        Ok(match explicit {
            Some(tree) => Delivered {
                primary: Some((tree.document, tree.disabled_escaping)),
                primary_output: Some(tree.output),
                secondary,
            },
            None => Delivered {
                primary: None,
                primary_output: None,
                secondary,
            },
        })
    }
}

/// The receiver the initial template writes to: the primary result's, which
/// it passes everything on to, noting whether anything at all arrived — that
/// is, whether the initial template's result sequence is non-empty (§2.4),
/// even if every item of it vanishes under §5.7.1 (a zero-length text node:
/// "It is possible to construct text nodes whose string value is
/// zero-length", §11.4). A zero-length text node that went through a
/// `SequenceCollector` first — the result of a template with `as` — arrives
/// as a zero-length `text` event too, after the template's other items
/// (`SequenceCollector::zero_length_text_nodes`).
pub(super) struct InitialResult<'r, 't> {
    pub(super) out: &'r mut dyn Receiver<'t>,
    pub(super) written: &'r Cell<bool>,
}

impl<'t> InitialResult<'_, 't> {
    fn mark(&mut self) -> &mut dyn Receiver<'t> {
        self.written.set(true);
        &mut *self.out
    }
}

impl<'t> Receiver<'t> for InitialResult<'_, 't> {
    fn start_document(&mut self) -> Result<(), XsltError> {
        self.mark().start_document()
    }

    fn end_document(&mut self) -> Result<(), XsltError> {
        self.out.end_document()
    }

    fn start_element(
        &mut self,
        name: &Name,
        binding: Option<NodeSchemaBinding>,
        nilled: bool,
    ) -> Result<(), XsltError> {
        self.mark().start_element(name, binding, nilled)
    }

    fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XsltError> {
        self.mark().namespace(prefix, uri)
    }

    fn attribute(
        &mut self,
        name: &Name,
        value: &str,
        binding: Option<NodeSchemaBinding>,
    ) -> Result<(), XsltError> {
        self.mark().attribute(name, value, binding)
    }

    fn end_start_tag(&mut self) -> Result<(), XsltError> {
        self.out.end_start_tag()
    }

    fn text(&mut self, s: &str, disable_escaping: bool) -> Result<(), XsltError> {
        self.mark().text(s, disable_escaping)
    }

    fn comment(&mut self, s: &str) -> Result<(), XsltError> {
        self.mark().comment(s)
    }

    fn pi(&mut self, target: &str, data: &str) -> Result<(), XsltError> {
        self.mark().pi(target, data)
    }

    fn end_element(&mut self) -> Result<(), XsltError> {
        self.out.end_element()
    }

    fn append_item(&mut self, item: &XmlItem<Nav<'t>>) -> Result<(), XsltError> {
        self.mark().append_item(item)
    }

    fn document_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        self.out.document_base_uri(uri)
    }

    fn element_base_uri(&mut self, uri: &str) -> Result<(), XsltError> {
        self.out.element_base_uri(uri)
    }

    fn no_inherit_namespaces(&mut self) -> Result<(), XsltError> {
        self.out.no_inherit_namespaces()
    }

    fn append_copy(
        &mut self,
        item: &XmlItem<Nav<'t>>,
        namespaces: CopyNamespaces,
    ) -> Result<(), XsltError> {
        self.mark().append_copy(item, namespaces)
    }
}
