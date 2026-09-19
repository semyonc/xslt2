// ── The XSLT function evaluator (§16, §9.6) ──────────────────────────────

//!
//! One `match` over [`XsltFn`], run as the `FunctionEvaluator` the XPath
//! engine calls back into.

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::types::{XmlAtomicValue, XmlTypeCode, XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::{
    BuiltinEvaluator, FunctionEvaluator, FunctionHandle, XPathValue,
};
use xsd_schema::xpath::{DynamicContext, XPathError, XmlItem};

use super::context::XsltContext;
use super::Transformation;
use crate::error::{codes, XsltError};
use crate::functions::XsltFn;
use crate::names::{is_ncname, split_qname, XML_NS, XSLT_NS};
use crate::receiver::Nav;
use crate::resolver;
use crate::whitespace;

/// Runs the XSLT function library over the running transformation.
pub struct XsltEvaluator<'r, 'a, 't> {
    pub(super) run: &'r Transformation<'a, 't>,
    pub(super) cx: &'r XsltContext<'t>,
    /// The lexical static context of the expression being evaluated (§5.4.1):
    /// `system-property()`, `element-available()` and `function-available()`
    /// resolve the prefix of their argument here.
    pub(super) sc: &'r crate::stylesheet::StaticContext,
}

impl<'t> FunctionEvaluator<Nav<'t>> for XsltEvaluator<'_, '_, 't> {
    fn eval(
        &self,
        handle: FunctionHandle,
        ctx: &mut DynamicContext<'_, Nav<'t>>,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XPathError> {
        let Some(which) = self.run.sheet.functions().resolve(handle) else {
            return BuiltinEvaluator.eval(handle, ctx, args);
        };
        match self.call(which, ctx, args) {
            Ok(value) => Ok(value),
            // `XPathError` has no host-error variant, so the real error is
            // parked on the transformation and picked up by `xpath_error`.
            Err(e) => {
                let message = e.to_string();
                *self.run.parked_error.borrow_mut() = Some(e);
                Err(XPathError::internal(message))
            }
        }
    }
}

impl<'t> XsltEvaluator<'_, '_, 't> {
    /// The base URI relative references in *this* expression resolve against.
    ///
    /// §5.4.1 gives the static context "Base URI: The base URI of the
    /// containing element in the stylesheet", after `xml:base`, and the
    /// containing element may be in an imported or included module — which is
    /// exactly why this is the expression's own static context and not the
    /// principal module's.
    fn static_base_uri(&self) -> &str {
        &self.sc.base_uri
    }

    /// The XPath context item — what a function with a context-dependent
    /// default argument sees.
    ///
    /// This is **not** `self.cx.item`: the XSLT-level focus is the focus of the
    /// innermost *instruction*, which inside a predicate or on the right of `/`
    /// is a different node. Only `current()` (§16.6.1) wants that one.
    fn context_item(&self, ctx: &DynamicContext<'_, Nav<'t>>) -> Option<XmlItem<Nav<'t>>> {
        ctx.context_item.clone()
    }

    fn call(
        &self,
        which: XsltFn,
        ctx: &mut DynamicContext<'_, Nav<'t>>,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        match which {
            // §16.6.1: "returns the item that was the context item at the
            // point where the expression was invoked from the XSLT
            // stylesheet" — the one function that takes the XSLT-level focus.
            XsltFn::Current => match &self.cx.current {
                Some(item) => Ok(XPathValue::from_item(item.clone())),
                None => Err(XsltError::new(
                    codes::XTDE1360,
                    "current() was called where there is no context item",
                )),
            },
            XsltFn::GenerateId => {
                // §16.6.4: "If the argument is omitted, it defaults to the
                // context node", i.e. the XPath context item.
                let node = match args.first() {
                    None => self.context_item(ctx),
                    Some(value) => value.first().cloned(),
                };
                Ok(match node {
                    Some(XmlItem::Node(nav)) => {
                        XPathValue::string(self.run.store.borrow().generate_id(&nav))
                    }
                    Some(XmlItem::Atomic(_)) => {
                        return Err(XsltError::new(
                            "XPTY0004",
                            "generate-id() takes a node, not an atomic value",
                        ))
                    }
                    None => XPathValue::string(String::new()),
                })
            }
            XsltFn::Document | XsltFn::Doc => {
                // §16.1: "The second argument, if present, is a node whose base
                // URI is used to resolve any relative URI references contained
                // in the first argument." Otherwise a string argument resolves
                // against the base URI from the static context, and a *node*
                // argument against the base URI of the node that contained it.
                let explicit_base = args.get(1).and_then(|value| match value.first() {
                    Some(XmlItem::Node(nav)) => Some(nav.base_uri().to_string()),
                    _ => None,
                });
                let items = match args.first() {
                    Some(value) => value.clone().into_vec(),
                    None => Vec::new(),
                };
                let mut nodes = Vec::new();
                for item in items {
                    let (uri, item_base) = match &item {
                        XmlItem::Atomic(value) => (value.to_string_value(), None),
                        XmlItem::Node(nav) => (nav.value(), Some(nav.base_uri().to_string())),
                    };
                    if uri.contains('#') {
                        return Err(XsltError::not_implemented(
                            "a fragment identifier in document()",
                            "M2",
                        ));
                    }
                    let base = explicit_base
                        .clone()
                        .or(item_base)
                        .unwrap_or_else(|| self.static_base_uri().to_string());
                    // §16.1: an empty URI means the stylesheet module itself,
                    // which falls out of resolving "" against the base URI.
                    let id = self.run.load_relative_to(&base, &uri)?;
                    nodes.push(XmlItem::Node(self.run.store.borrow().navigator(id)));
                }
                Ok(XPathValue::from_sequence(nodes))
            }
            XsltFn::DocAvailable => {
                let uris = string_args(args.first())?;
                // F&O `fn:doc-available`: "returns true if and only if the
                // function call `fn:doc($uri)` would return a document node".
                // A failure to retrieve or parse the resource is that answer —
                // `false`. A failure of the *processor* (a feature this
                // milestone lacks, a broken invariant in the tree builder) is
                // not: it means this call cannot tell, and answering `false`
                // would silently send the stylesheet down its fallback path.
                let available = match uris.first() {
                    Some(uri) => match self.run.load_relative_to(self.static_base_uri(), uri) {
                        Ok(_) => true,
                        Err(e) if e.is_processor_failure() => return Err(e),
                        Err(_) => false,
                    },
                    None => false,
                };
                Ok(XPathValue::boolean(available))
            }
            XsltFn::DocumentUri => {
                // F&O `fn:document-uri`: the `document-uri` property of the
                // argument, which only a document node has, and which §9.4
                // sets to empty for a temporary tree. The store is the only
                // place that knows which document came from which URI.
                let node = match args.first() {
                    None => self.context_item(ctx),
                    Some(value) => value.first().cloned(),
                };
                let Some(XmlItem::Node(nav)) = node else {
                    return Ok(XPathValue::empty());
                };
                if nav.node_type() != DomNodeType::Root {
                    return Ok(XPathValue::empty());
                }
                Ok(match self.run.store.borrow().uri_of(&nav) {
                    Some(uri) => XPathValue::from_atomic(any_uri(uri)),
                    None => XPathValue::empty(),
                })
            }
            XsltFn::SystemProperty => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::string(self.system_property_of(&name)?))
            }
            XsltFn::ElementAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::boolean(self.element_available(&name)))
            }
            XsltFn::FunctionAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::boolean(self.function_available(&name)))
            }
            XsltFn::TypeAvailable => Ok(XPathValue::boolean(false)),
            // §16.2: "If the URI is a relative URI, then it is resolved
            // relative to the base URI from the static context."
            XsltFn::UnparsedText => {
                let uri = single_string(args.first()).unwrap_or_default();
                let absolute = resolver::resolve_uri(self.static_base_uri(), &uri);
                let text = self.run.resolver.resolve_text(&absolute).map_err(|e| {
                    XsltError::new(codes::XTDE1170, format!("cannot read {absolute:?}: {e}"))
                })?;
                Ok(XPathValue::string(text))
            }
            XsltFn::UnparsedTextAvailable => {
                let uri = single_string(args.first()).unwrap_or_default();
                let absolute = resolver::resolve_uri(self.static_base_uri(), &uri);
                Ok(XPathValue::boolean(self.run.resolver.available(&absolute)))
            }
            // §16.6.2/§16.6.3: the entity is looked up "in the document
            // containing the context node". No source document carries DTD
            // information here, so there is never such an entity and the
            // result is the empty value — but the context-node conditions are
            // errors in their own right and are checked.
            XsltFn::UnparsedEntityUri => {
                self.require_document_context(ctx, codes::XTDE1370)?;
                Ok(XPathValue::from_atomic(any_uri("")))
            }
            XsltFn::UnparsedEntityPublicId => {
                self.require_document_context(ctx, codes::XTDE1380)?;
                Ok(XPathValue::string(String::new()))
            }
            XsltFn::Key => Err(XsltError::not_implemented("key()", "M2")),
            XsltFn::CurrentGroup | XsltFn::CurrentGroupingKey => {
                Err(XsltError::not_implemented(which.name(), "M2"))
            }
            XsltFn::RegexGroup => Err(XsltError::not_implemented("regex-group()", "M2")),
            // §16.4, §16.5. These are in the catalog as stubs only, so that a
            // call to one is reported as a missing feature rather than as an
            // unknown function (XPST0017) at compile time.
            XsltFn::FormatNumber
            | XsltFn::FormatDateTime
            | XsltFn::FormatDate
            | XsltFn::FormatTime => Err(XsltError::not_implemented(
                format!("{}()", which.name()),
                "M3d",
            )),
        }
    }

    /// The precondition `unparsed-entity-uri` and `unparsed-entity-public-id`
    /// share (§16.6.2, §16.6.3).
    ///
    /// > It is a non-recoverable dynamic error if the function is called when
    /// > there is no context node, or when the root of the tree containing the
    /// > context node is not a document node.
    fn require_document_context(
        &self,
        ctx: &DynamicContext<'_, Nav<'t>>,
        code: &'static str,
    ) -> Result<(), XsltError> {
        let Some(XmlItem::Node(nav)) = self.context_item(ctx) else {
            return Err(XsltError::new(code, "there is no context node"));
        };
        let mut root = nav.clone();
        root.move_to_root();
        if root.node_type() != DomNodeType::Root {
            return Err(XsltError::new(
                code,
                "the root of the tree containing the context node is not a document node",
            ));
        }
        Ok(())
    }

    /// §16.6.5 system-property.
    fn system_property_of(&self, name: &str) -> Result<String, XsltError> {
        let (prefix, local) = split_qname(whitespace::trim(name));
        if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
            return Err(XsltError::new(
                codes::XTDE1390,
                format!("{name:?} is not a QName"),
            ));
        }
        let uri = self.resolve_static_prefix(prefix).ok_or_else(|| {
            XsltError::new(
                codes::XTDE1390,
                format!("the prefix {prefix:?} of {name:?} is not bound"),
            )
        })?;
        if uri != XSLT_NS {
            return Ok(String::new());
        }
        Ok(crate::functions::system_property(local).to_string())
    }

    /// §18.1.1 element-available.
    fn element_available(&self, name: &str) -> bool {
        let (prefix, local) = split_qname(whitespace::trim(name));
        match self.resolve_static_prefix(prefix) {
            Some(uri) => crate::functions::element_available(&uri, local),
            None => false,
        }
    }

    /// §18.1.2 function-available.
    fn function_available(&self, name: &str) -> bool {
        let (prefix, local) = split_qname(whitespace::trim(name));
        let uri = if prefix.is_empty() {
            crate::names::FN_NS.to_string()
        } else {
            match self.resolve_static_prefix(prefix) {
                Some(uri) => uri,
                None => return false,
            }
        };
        crate::functions::function_available(self.run.sheet.functions(), &uri, local)
    }

    fn resolve_static_prefix(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            return Some(String::new());
        }
        if prefix == "xml" {
            return Some(XML_NS.to_string());
        }
        // §5.4.1: the in-scope namespaces of a static context are "the
        // namespaces in scope for the containing element in the stylesheet",
        // and nothing else. A prefix bound somewhere else in the stylesheet is
        // not bound here, and the caller turns that into XTDE1390/XTDE1400/
        // XTDE1440 as its own definition requires.
        self.sc.resolve_prefix(prefix)
    }
}
/// An `xs:anyURI` atomic value.
fn any_uri(uri: &str) -> XmlValue {
    XmlValue::new(
        XmlTypeCode::AnyUri,
        XmlValueKind::Atomic(XmlAtomicValue::AnyUri(uri.to_string())),
    )
}
fn single_string<N: DomNavigator>(value: Option<&XPathValue<N>>) -> Option<String> {
    match value?.first()? {
        XmlItem::Atomic(v) => Some(v.to_string_value()),
        XmlItem::Node(n) => Some(n.value()),
    }
}

/// The string values of every item of an argument.
///
/// `XPathValue::as_slice` returns an empty slice for the single-item variant
/// (`functions/mod.rs`, documented there as a limitation), so the value is
/// materialized instead.
fn string_args<N: DomNavigator>(value: Option<&XPathValue<N>>) -> Result<Vec<String>, XsltError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    Ok(value
        .clone()
        .into_vec()
        .into_iter()
        .map(|item| match item {
            XmlItem::Atomic(v) => v.to_string_value(),
            XmlItem::Node(n) => n.value(),
        })
        .collect())
}
