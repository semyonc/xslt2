//! Variables, parameters and global variables (§9.2–§9.5): the value a
//! binding takes, the temporary tree a sequence constructor builds (§9.4) and
//! the function conversion rules an `as` attribute applies (§9.3).

use xsd_schema::types::XmlTypeCode;
use xsd_schema::xpath::functions::{atomize_sequence, XPathValue};
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::{Frame, GlobalSlot, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::{ComplexContentNormalizer, Nav, Receiver, ResultBuilder, SequenceCollector};

impl<'a, 't> Transformation<'a, 't> {
    // ── Values ───────────────────────────────────────────────────────────

    pub(super) fn var_value(
        &self,
        value: &VarValue,
        as_type: Option<&TypeCheck>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        code: &'static str,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let raw = match value {
            VarValue::Select(id) => self.eval_expr(*id, frame, cx)?,
            VarValue::Sequence(body) => {
                let mut collector = SequenceCollector::new(&self.store);
                self.eval_body(body, frame, cx, &mut collector)?;
                XPathValue::from_sequence(collector.into_items())
            }
            VarValue::Tree { body, ctx: sc } => {
                let node = self.temporary_tree(body, *sc, frame, cx)?;
                XPathValue::from_node(node)
            }
            // §9.2: no select and no content means the empty sequence with
            // `as`, and a zero-length string without.
            VarValue::Empty => match as_type {
                Some(_) => XPathValue::empty(),
                None => XPathValue::string(String::new()),
            },
        };
        self.convert(raw, as_type, code)
    }

    /// The default value of an optional parameter that no caller supplied a
    /// value for (§9.2), with the two error codes §9.2 gives its two cases.
    ///
    /// > \[ERR XTTE0600\] If a default value is given explicitly, that is, if
    /// > there is either a `select` attribute or a non-empty sequence
    /// > constructor, then it is a type error if the default value cannot be
    /// > converted to the required type, using the function conversion rules.
    ///
    /// > \[ERR XTDE0610\] If an optional parameter has no `select` attribute
    /// > and has an empty sequence constructor, and if there is an `as`
    /// > attribute, then the default value of the parameter is an empty
    /// > sequence. If the empty sequence is not a valid instance of the required
    /// > type defined in the `as` attribute, then the parameter is treated as a
    /// > required parameter, which means that it is a non-recoverable dynamic
    /// > error if the caller supplies no value for the parameter.
    pub(super) fn param_default(
        &self,
        name: &crate::names::QName,
        value: &VarValue,
        as_type: Option<&TypeCheck>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        if let (VarValue::Empty, Some(check)) = (value, as_type) {
            let empty = XPathValue::empty();
            if self.instance_of(&empty, check)? {
                return Ok(empty);
            }
            return Err(XsltError::new(
                codes::XTDE0610,
                format!(
                    "no value was supplied for the parameter {name}, which has no default \
                     and whose required type {} does not allow the empty sequence",
                    check.text
                ),
            ));
        }
        self.var_value(value, as_type, frame, cx, codes::XTTE0600)
    }

    /// §9.4: a sequence constructor with no `as` builds a temporary tree.
    ///
    /// > The base URI of the document node is taken from the base URI of the
    /// > variable binding element in the stylesheet. … The `document-uri` and
    /// > `unparsed-entities` properties of the new document node are set to
    /// > empty.
    ///
    /// The document is therefore registered with **no** URI — which is what
    /// makes `document-uri()` empty — while its base URI is recorded on the
    /// document itself.
    fn temporary_tree(
        &self,
        body: &Body,
        sc: crate::stylesheet::StaticContextId,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Nav<'t>, XsltError> {
        let (arena, names) = {
            let store = self.store.borrow();
            (store.arena(), store.names())
        };
        let builder = ResultBuilder::new(arena, names, None)?;
        let mut out = ComplexContentNormalizer::new(builder);
        out.start_document()?;
        self.eval_body(body, frame, cx, &mut out)?;
        out.end_document()?;
        let mut doc = out.into_inner().finish()?;
        let base = &self.sheet.static_contexts.get(sc).base_uri;
        if !base.is_empty() {
            doc.set_document_base_uri(Some(self.store.borrow().intern_uri(base)));
        }
        let id = self.store.borrow_mut().insert(doc, None);
        Ok(self.store.borrow().navigator(id))
    }

    /// The function conversion rules of XPath §3.1.5, as XSLT applies them to
    /// an `as` attribute (§9.3).
    pub(super) fn convert(
        &self,
        value: XPathValue<Nav<'t>>,
        as_type: Option<&TypeCheck>,
        code: &'static str,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let Some(check) = as_type else {
            return Ok(value);
        };
        if self.instance_of(&value, check)? {
            return Ok(value);
        }
        if check.atomic {
            // XPath 2.0 §3.1.5, the function conversion rules XSLT §9.3 cites:
            // the value is atomized; an `xs:untypedAtomic` item is cast to the
            // expected type; type promotion (Appendix B.1) applies. Nothing
            // else is converted, so a genuine type mismatch stays one.
            let atomized = atomize_sequence(value.clone()).map_err(|e| self.xpath_error(e))?;
            let mut items: Vec<XmlItem<Nav<'t>>> = Vec::with_capacity(atomized.len());
            for atom in atomized {
                let needs_cast = atom.type_code == XmlTypeCode::UntypedAtomic
                    || check
                        .target
                        .is_some_and(|target| promotes_to(atom.type_code, target));
                let item = XPathValue::from_atomic(atom);
                let converted = match (&check.cast, needs_cast) {
                    (Some(cast), true) => match self.apply_probe(*cast, item.clone()) {
                        Ok(value) => value,
                        // A failed cast is the type error of the construct,
                        // not the raw FORG0001 of the cast.
                        Err(_) => {
                            return Err(XsltError::new(
                                code,
                                format!(
                                    "the value cannot be converted to the required type {}",
                                    check.text
                                ),
                            ))
                        }
                    },
                    _ => item,
                };
                items.extend(converted.into_vec());
            }
            let converted = XPathValue::from_sequence(items);
            if self.instance_of(&converted, check)? {
                return Ok(converted);
            }
        }
        Err(XsltError::new(
            code,
            format!("the value does not match the required type {}", check.text),
        ))
    }

    fn instance_of(
        &self,
        value: &XPathValue<Nav<'t>>,
        check: &TypeCheck,
    ) -> Result<bool, XsltError> {
        let result = self.apply_probe(check.test, value.clone())?;
        Ok(result.as_bool().unwrap_or(false))
    }

    /// Evaluates one of the helper expressions whose only variable is
    /// `$value`.
    fn apply_probe(
        &self,
        id: ExprId,
        value: XPathValue<Nav<'t>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let compiled = self.sheet.expr(id);
        let ctx = self.sheet.context(compiled.ctx);
        self.parked_error.borrow_mut().take();
        compiled
            .expr
            .evaluator(ctx)
            .run_with_node_and_setup::<Nav<'t>, _>(None, |ev| {
                ev.set_variable(0, value);
            })
            .map_err(|e| self.xpath_error(e))
    }
    /// A global variable, evaluated on first use (§9.5).
    pub(super) fn global(&self, index: usize) -> Result<XPathValue<Nav<'t>>, XsltError> {
        match &self.globals.borrow()[index] {
            GlobalSlot::Value(value) => return Ok(value.clone()),
            GlobalSlot::Evaluating => {
                let name = &self.sheet.globals()[index].name;
                return Err(XsltError::new(
                    codes::XTDE0640,
                    format!("the global variable {name} depends on itself"),
                ));
            }
            GlobalSlot::Unevaluated => {}
        }
        // Q33: a global's value may read another global — or call `key()`,
        // whose build may read a third — with no template invocation or
        // function call in between, so a long enough chain would overflow the
        // stack. After the circularity test, so that XTDE0640 stays XTDE0640;
        // before the marker, which the depth error must not leave behind.
        self.check_stack()?;
        self.globals.borrow_mut()[index] = GlobalSlot::Evaluating;
        let global = &self.sheet.globals()[index];
        let result = self.eval_global(global);
        match result {
            Ok(value) => {
                self.globals.borrow_mut()[index] = GlobalSlot::Value(value.clone());
                Ok(value)
            }
            Err(e) => {
                self.globals.borrow_mut()[index] = GlobalSlot::Unevaluated;
                Err(e.or_location(&global.loc))
            }
        }
    }

    fn eval_global(&self, global: &GlobalVar) -> Result<XPathValue<Nav<'t>>, XsltError> {
        // §9.1: a global is evaluated with a singleton focus on the root of
        // the tree containing the initial context node.
        let mut cx = XsltContext::empty();
        if let Some(root) = self.initial_root.borrow().clone() {
            cx.item = Some(XmlItem::Node(root.clone()));
            cx.position = 1;
            cx.size = 1;
            cx.current = cx.item.clone();
        }
        if global.is_param {
            if let Some(value) = self.params.borrow().get(&global.name).cloned() {
                return self.convert(value, global.as_type.as_ref(), codes::XTTE0590);
            }
            if global.required {
                return Err(XsltError::new(
                    codes::XTDE0050,
                    format!(
                        "no value was supplied for the required parameter {}",
                        global.name
                    ),
                ));
            }
        }
        let mut frame = Frame::with_slots(global.frame_size);
        if global.is_param {
            return self.param_default(
                &global.name,
                &global.value,
                global.as_type.as_ref(),
                &mut frame,
                &cx,
            );
        }
        self.var_value(
            &global.value,
            global.as_type.as_ref(),
            &mut frame,
            &cx,
            codes::XTTE0570,
        )
    }
}

/// XPath 2.0 Appendix B.1, type promotion — the only conversions between
/// atomic types the function conversion rules make besides casting
/// `xs:untypedAtomic`:
///
/// > A value of type `xs:float` (or any type derived by restriction from
/// > `xs:float`) can be promoted to the type `xs:double`. … A value of type
/// > `xs:decimal` (or any type derived by restriction from `xs:decimal`) can be
/// > promoted to either of the types `xs:float` or `xs:double`. … A value of
/// > type `xs:anyURI` (or any type derived by restriction from `xs:anyURI`) can
/// > be promoted to the type `xs:string`.
///
/// So `xs:double` is never promoted to `xs:float`: that is a type error.
fn promotes_to(source: XmlTypeCode, target: XmlTypeCode) -> bool {
    let decimal =
        source.is_numeric() && !matches!(source, XmlTypeCode::Float | XmlTypeCode::Double);
    match target {
        XmlTypeCode::Double => decimal || source == XmlTypeCode::Float,
        XmlTypeCode::Float => decimal,
        XmlTypeCode::String => source == XmlTypeCode::AnyUri,
        _ => false,
    }
}
