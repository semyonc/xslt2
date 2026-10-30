// ── The XSLT function evaluator (§16, §9.6) ──────────────────────────────

//!
//! One `match` over [`XsltFn`], run as the `FunctionEvaluator` the XPath
//! engine calls back into.

use xsd_schema::navigator::{DomNavigator, DomNodeType};
use xsd_schema::types::{XmlAtomicValue, XmlTypeCode, XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::{
    BuiltinEvaluator, FunctionEvaluator, FunctionHandle, XPathValue,
};
use xsd_schema::xpath::node_ops::compare_document_order;
use xsd_schema::xpath::{DynamicContext, XPathError, XmlItem};

use super::context::XsltContext;
use super::Transformation;
use crate::error::{codes, XsltError};
use crate::functions::{expand_name_argument, NameScope, XsltFn};
use crate::names::XSLT_NS;
use crate::receiver::Nav;
use crate::resolver;
use crate::stylesheet::CompatMode;

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
        // §10.3: a call bound to a stylesheet function runs its body. Its
        // handle is a custom one, and never one of the function library's.
        if let Some(id) = handle
            .is_custom()
            .then(|| self.run.sheet.stylesheet_function_of(handle))
            .flatten()
        {
            return self
                .run
                .call_stylesheet_function(id, args)
                .map_err(XsltError::into_xpath_error);
        }
        let Some(which) = self.run.sheet.functions().resolve(handle) else {
            return BuiltinEvaluator.eval(handle, ctx, args);
        };
        // The engine carries the real error out as its host payload, and
        // `xpath_error` takes it back unchanged.
        self.call(which, ctx, args)
            .map_err(XsltError::into_xpath_error)
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
                    // With the focus undefined — the body of a stylesheet
                    // function (§10.3: "any attempt to reference the context
                    // item … is a non-recoverable dynamic error. [XPDY0002]"),
                    // or a run with no initial context node — there is no
                    // context node to default to; only an *explicitly* empty
                    // argument gives the zero-length string.
                    None => Some(self.context_item(ctx).ok_or_else(|| {
                        XsltError::new(
                            "XPDY0002",
                            "generate-id() with no argument needs a context node, and the \
                             focus is undefined",
                        )
                    })?),
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
                // A node's base URI is XDM's `base-uri` accessor — its `xml:base`
                // chain resolved against its document's base URI
                // (`stylesheet::node_base_uri`); `DomNavigator::base_uri` is the
                // unresolved `xml:base` in scope (`xsd-schema`'s contract).
                let node_base =
                    |nav: &Nav<'t>| crate::stylesheet::node_base_uri(nav, self.static_base_uri());
                let explicit_base = args.get(1).and_then(|value| match value.first() {
                    Some(XmlItem::Node(nav)) => Some(node_base(nav)),
                    _ => None,
                });
                let items = match args.first() {
                    Some(value) => value.clone().into_vec(),
                    None => Vec::new(),
                };
                let mut ids: Vec<crate::store::DocId> = Vec::new();
                for item in items {
                    let (uri, item_base) = match &item {
                        XmlItem::Atomic(value) => (value.to_string_value(), None),
                        XmlItem::Node(nav) => (nav.value(), Some(node_base(nav))),
                    };
                    let base = explicit_base
                        .clone()
                        .or(item_base)
                        .unwrap_or_else(|| self.static_base_uri().to_string());
                    let id = match fragment_split(&uri) {
                        None => {
                            // §16.1: an empty URI means the stylesheet module
                            // itself, which falls out of resolving "" against
                            // the base URI.
                            self.run.load_for_doc(&base, &uri)?
                        }
                        // F&O §15.5.4 `fn:doc`: "If the Available documents
                        // provides no mapping for the string, an error is
                        // raised [err:FODC0005]" — and they map no URI that
                        // carries a fragment identifier (`doc-available()`
                        // agrees, below).
                        Some(_) if which == XsltFn::Doc => {
                            return Err(XsltError::new(
                                "FODC0005",
                                format!("doc(): no document is available at {uri:?}"),
                            ))
                        }
                        Some((document, _)) => {
                            self.document_with_fragment(&base, &uri, document)?
                        }
                    };
                    // §16.1: "The sequence of nodes returned by the function
                    // is in document order, with no duplicates." Every node
                    // here is a document node, and the store returns the same
                    // document for the same URI, so a duplicate is a repeated
                    // id.
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                let store = self.run.store.borrow();
                let mut nodes: Vec<Nav<'t>> =
                    ids.into_iter().map(|id| store.navigator(id)).collect();
                // "This order has no necessary relationship to the order in
                // which URIs were supplied in the $uri-sequence argument": the
                // engine's document order, the one `|` and path expressions
                // use (between documents, a stable order of the documents).
                nodes.sort_by(compare_document_order);
                Ok(XPathValue::from_sequence(
                    nodes.into_iter().map(XmlItem::Node).collect(),
                ))
            }
            XsltFn::DocAvailable => {
                let uris = string_args(args.first())?;
                // F&O `fn:doc-available`: "returns true if and only if the
                // function call `fn:doc($uri)` would return a document node".
                // A failure to retrieve or parse the resource is that answer —
                // `false`. A failure of the *processor* (a feature it does not
                // implement, a broken invariant in the tree builder) is
                // not: it means this call cannot tell, and answering `false`
                // would silently send the stylesheet down its fallback path.
                let available = match uris.first() {
                    // `doc()` maps no URI with a fragment identifier (FODC0005,
                    // above), so neither does this — whatever the resolver could
                    // serve for it (`FileResolver` drops the fragment when it
                    // maps the URI to a path).
                    Some(uri) if fragment_split(uri).is_some() => false,
                    Some(uri) => match self.run.load_for_doc(self.static_base_uri(), uri) {
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
                Ok(XPathValue::boolean(self.element_available(&name)?))
            }
            XsltFn::FunctionAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                let arity = crate::functions::arity_argument(args.get(1))?;
                Ok(XPathValue::boolean(self.function_available(&name, arity)?))
            }
            XsltFn::TypeAvailable => {
                let name = single_string(args.first()).unwrap_or_default();
                Ok(XPathValue::boolean(self.type_available(&name)?))
            }
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
            XsltFn::Key => self.key(ctx, args),
            // §14.1: "While an xsl:for-each-group instruction is being
            // evaluated, the current group will be non-empty. At other times,
            // it will be an empty sequence." Like `current()`, these read the
            // XSLT half of the context, which nested XPath scopes (predicates,
            // `/` steps) do not change.
            XsltFn::CurrentGroup => Ok(match &self.cx.group {
                Some(group) => XPathValue::from_sequence(group.items.clone()),
                None => XPathValue::empty(),
            }),
            // §14.2: "While an xsl:for-each-group instruction with a group-by
            // or group-adjacent attribute is being evaluated, the current
            // grouping key will be a single atomic value. At other times, it
            // will be the empty sequence."
            XsltFn::CurrentGroupingKey => {
                Ok(match self.cx.group.as_ref().and_then(|g| g.key.clone()) {
                    Some(key) => XPathValue::from_atomic(key),
                    None => XPathValue::empty(),
                })
            }
            // §15.2: "regex-group($group-number as xs:integer) as xs:string" —
            // the Nth captured substring of the current match, "" whenever
            // there is none: outside xsl:matching-substring (the current
            // captured substrings are then the empty sequence), for a negative
            // or non-existent group, and for a group that did not take part
            // in the match or matched a zero-length string. Like `current()`
            // it reads the XSLT half of the context, so a predicate or a path
            // step inside the expression does not change the answer.
            XsltFn::RegexGroup => {
                let argument = args.first().cloned().unwrap_or_else(XPathValue::empty);
                let n = crate::functions::integer_argument(
                    &argument,
                    "the argument of regex-group() ($group-number as xs:integer)",
                )?;
                let group = match (&self.cx.captured, n) {
                    (Some(captured), Some(n)) => captured.group(n).to_string(),
                    _ => String::new(),
                };
                Ok(XPathValue::string(group))
            }
            // §16.4 and §16.5: `runtime/format.rs`.
            XsltFn::FormatNumber => self.format_number(args),
            XsltFn::FormatDateTime | XsltFn::FormatDate | XsltFn::FormatTime => {
                self.format_date_time(which, args)
            }
            // §3.8: a call bound to the fallback error function — reached only
            // with backwards-compatible behaviour — raises XTDE1425 when, and
            // only when, it is evaluated. §18.1.2: "When backwards compatible
            // behavior is enabled, it is a non-recoverable dynamic error to
            // evaluate an extension function call if no implementation of the
            // extension function is available."
            XsltFn::FallbackError => Err(XsltError::new(
                codes::XTDE1425,
                format!(
                    "a call to a function with no implementation ({} argument(s)) was \
                     evaluated with backwards-compatible behaviour enabled",
                    args.len()
                ),
            )),
        }
    }

    /// `document()` of a URI reference that carries a fragment identifier
    /// (§16.1); `document` is the reference without it.
    ///
    /// > \[ERR XTRE1160\] When a URI reference contains a fragment identifier,
    /// > it is a recoverable dynamic error if the media type is not one that is
    /// > recognized by the processor … The optional recovery action is to
    /// > ignore the fragment identifier and return the document node. The set
    /// > of media types recognized by a processor is implementation-defined.
    ///
    /// This processor recognizes no media type's fragment identifiers, so the
    /// error always applies, and `TransformOptions::strict_recoverable` decides
    /// between signalling it and taking the recovery action. When the recovery
    /// cannot be completed because the document itself cannot be retrieved,
    /// two errors have arisen, and §2.9: "If more than one error arises, an
    /// implementation is not required to signal any errors other than the
    /// first one that it detects. It is implementation-dependent which of the
    /// several errors is signaled." XTRE1160 is the one detected first, and
    /// the retrieval failure goes into its message.
    fn document_with_fragment(
        &self,
        base: &str,
        uri: &str,
        document: &str,
    ) -> Result<crate::store::DocId, XsltError> {
        let unrecognized = |why: String| {
            XsltError::new(
                codes::XTRE1160,
                format!(
                    "document(): the fragment identifier in {uri:?} cannot be interpreted: this \
                     processor recognizes no media type's fragment identifiers{why}"
                ),
            )
        };
        if self.run.options.strict_recoverable {
            return Err(unrecognized(String::new()));
        }
        match self.run.load_for_doc(base, document) {
            Ok(id) => Ok(id),
            Err(e) if e.is_processor_failure() => Err(e),
            Err(e) => Err(unrecognized(format!(
                ", and the recovery action (ignoring it) failed too: {e}"
            ))),
        }
    }

    /// `key($key-name, $key-value)` and `key($key-name, $key-value, $top)`
    /// (§16.3.2).
    ///
    /// > \[ERR XTDE1270\] It is a non-recoverable dynamic error to call the
    /// > key function with two arguments if there is no context node, or if the
    /// > root of the tree containing the context node is not a document node;
    /// > or to call the function with three arguments if the root of the tree
    /// > containing the node supplied in the third argument is not a document
    /// > node.
    ///
    /// "There is no context node" includes a call where the focus is absent —
    /// the body of a stylesheet function (§10.3) — and one whose context item is
    /// an atomic value; both are XTDE1270, the code §16.3.2 gives the
    /// condition, rather than the generic XPDY0002 / XPTY0004.
    ///
    /// The arguments are checked against §16.3.2's signatures, `key($key-name
    /// as xs:string, $key-value as xs:anyAtomicType*) as node()*` and
    /// `key($key-name as xs:string, $key-value as xs:anyAtomicType*, $top as
    /// node()) as node()*`, here: the engine hands this library its arguments
    /// as evaluated, without applying the declared types.
    fn key(
        &self,
        ctx: &DynamicContext<'_, Nav<'t>>,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let mut args = args.into_iter();
        let name = self.key_name_argument(args.next().unwrap_or_else(XPathValue::empty))?;
        // > [ERR XTDE1260] It is a non-recoverable dynamic error if the value
        // > is not a valid QName, or if there is no namespace declaration in
        // > scope for the prefix of the QName, or if the name obtained by
        // > expanding the QName is not the same as the expanded name of any
        // > xsl:key declaration in the stylesheet.
        let (uri, local) = expand_name_argument(&self.name_scope(), XsltFn::Key, &name)?;
        let qname = crate::names::QName::new(uri.as_str(), local);
        let key = self.run.sheet.key_named(&qname).ok_or_else(|| {
            XsltError::new(
                codes::XTDE1260,
                format!("key(): the stylesheet declares no key named {qname}"),
            )
        })?;
        let value = args.next().unwrap_or_else(XPathValue::empty);
        // "The third argument is used to identify the selected subtree. … If
        // the argument is omitted, the selected subtree is the document
        // containing the context node."
        let (top, which) = match args.next() {
            Some(top) => (
                Some(self.key_top_argument(top)?),
                "the node supplied as the third argument",
            ),
            None => (None, "the context node"),
        };
        let start = match &top {
            Some(nav) => nav.clone(),
            None => match self.context_item(ctx) {
                Some(XmlItem::Node(nav)) => nav,
                _ => {
                    return Err(XsltError::new(
                        codes::XTDE1270,
                        "key() with two arguments was called where there is no context node",
                    ))
                }
            },
        };
        let mut root = start;
        root.move_to_root();
        if root.node_type() != DomNodeType::Root {
            return Err(XsltError::new(
                codes::XTDE1270,
                format!("key(): the root of the tree containing {which} is not a document node"),
            ));
        }
        // "The set of requested key values is formed by atomizing the supplied
        // value of the argument".
        let sought = xsd_schema::xpath::functions::atomize_sequence(value)
            .map_err(|e| self.run.xpath_error(e))?;
        // The key specifier is evaluated with the current group and captured
        // substrings of this call (see `Transformation::key_lookup`).
        let caller = super::keys::KeyCaller {
            group: self.cx.group.as_ref(),
            captured: self.cx.captured.as_ref(),
        };
        let nodes = self
            .run
            .key_lookup(key, sought, &root, top.as_ref(), caller)?;
        Ok(XPathValue::from_sequence(
            nodes.into_iter().map(XmlItem::Node).collect(),
        ))
    }

    /// Whether the expression being evaluated has backwards-compatible
    /// behaviour, i.e. XPath 1.0 compatibility mode (§3.8).
    pub(super) fn xpath1_compatible(&self) -> bool {
        self.sc.compat == CompatMode::BackwardsCompatible
    }

    /// `$key-name as xs:string`, after XPath 2.0 §3.1.5's function conversion
    /// rules: atomization; "Each item in the atomic sequence that is of type
    /// xs:untypedAtomic is cast to the expected atomic type"; "For each item of
    /// type xs:anyURI in the atomic sequence that can be promoted to the
    /// expected atomic type using URI promotion … the promotion is done"; then
    /// "If, after the above conversions, the resulting value does not match
    /// the expected type according to the rules for SequenceType Matching, a
    /// type error is raised \[err:XPTY0004\]" — so exactly one value, of
    /// `xs:string` or a type derived from it.
    ///
    /// With XPath 1.0 compatibility mode, §3.1.5 first applies: "If the
    /// expected type calls for a single item or optional single item …, then
    /// the value V is effectively replaced by `V[1]`", and "If the expected type
    /// is xs:string or xs:string?, then the value V is effectively replaced by
    /// fn:string(V)."
    fn key_name_argument(&self, value: XPathValue<Nav<'t>>) -> Result<String, XsltError> {
        self.string_argument(
            value,
            "the first argument of key() ($key-name as xs:string)",
        )
    }

    /// One argument whose declared type is `xs:string`, converted as
    /// [`Self::key_name_argument`] describes; `what` names the parameter in the
    /// XPTY0004 message.
    pub(super) fn string_argument(
        &self,
        value: XPathValue<Nav<'t>>,
        what: &str,
    ) -> Result<String, XsltError> {
        if self.xpath1_compatible() {
            return Ok(match value.first() {
                None => String::new(),
                Some(XmlItem::Atomic(v)) => v.to_string_value(),
                Some(XmlItem::Node(n)) => n.value(),
            });
        }
        // The usual call, a string literal: nothing to atomize.
        if let XPathValue::Item(XmlItem::Atomic(atom)) = &value {
            if converts_to_string(atom) {
                return Ok(atom.to_string_value());
            }
        }
        let atoms = xsd_schema::xpath::functions::atomize_sequence(value)
            .map_err(|e| self.run.xpath_error(e))?;
        match atoms.as_slice() {
            [atom] if converts_to_string(atom) => Ok(atom.to_string_value()),
            _ => Err(XsltError::new(
                "XPTY0004",
                format!("{what} must be one string, not {}", describe_atoms(&atoms)),
            )),
        }
    }

    /// `$top as node()`: exactly one node — otherwise XPTY0004 (XPath 2.0
    /// §3.1.5). With XPath 1.0 compatibility mode the value is first replaced
    /// by its first item ("V is effectively replaced by `V[1]`").
    fn key_top_argument(&self, value: XPathValue<Nav<'t>>) -> Result<Nav<'t>, XsltError> {
        let items = if self.xpath1_compatible() {
            value.first().cloned().into_iter().collect()
        } else {
            value.into_vec()
        };
        let found = match items.as_slice() {
            [XmlItem::Node(nav)] => return Ok(nav.clone()),
            [] => "an empty sequence".to_string(),
            [XmlItem::Atomic(v)] => format!("a value of type xs:{}", type_name(v)),
            several => format!("a sequence of {} items", several.len()),
        };
        Err(XsltError::new(
            "XPTY0004",
            format!("the third argument of key() ($top as node()) must be one node, not {found}"),
        ))
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

    /// The static context §16.6.5's and §18.1's name-answering functions expand
    /// their argument against.
    ///
    /// §5.4.1: the in-scope namespaces of a static context are "the namespaces
    /// in scope for the containing element in the stylesheet", and nothing else.
    /// A prefix bound somewhere else in the stylesheet is not bound here, and
    /// [`expand_name_argument`] turns that — or an argument that is not a
    /// lexical QName — into XTDE1390/XTDE1400/XTDE1428/XTDE1440 as the called
    /// function's own definition requires.
    ///
    /// The **same** type the `use-when` evaluator builds, and the expansion goes
    /// through the **same** [`expand_name_argument`], so a name cannot answer
    /// one way in `use-when` and another way here.
    pub(super) fn name_scope(&self) -> NameScope<'_> {
        NameScope {
            bindings: &self.sc.namespaces,
            xpath_default_namespace: &self.sc.xpath_default_namespace,
        }
    }

    /// §16.6.5 system-property. §5.1's general rule applies to the unprefixed
    /// case: "The default namespace of the defining element … is not used for
    /// unprefixed names."
    fn system_property_of(&self, name: &str) -> Result<String, XsltError> {
        let (uri, local) = expand_name_argument(&self.name_scope(), XsltFn::SystemProperty, name)?;
        if uri != XSLT_NS {
            return Ok(String::new());
        }
        Ok(crate::functions::system_property(local, self.run.sheet.is_schema_aware()).to_string())
    }

    /// §18.2.2 element-available. §5.1 lists it as one of only three places
    /// where "the default namespace of the defining element is used when
    /// expanding an unprefixed QName", and §18.2.2 says the same: "If there is
    /// a default namespace in scope, then it is used to expand an unprefixed
    /// QName." That is the `xmlns=` default, not `xpath-default-namespace`.
    fn element_available(&self, name: &str) -> Result<bool, XsltError> {
        let (uri, local) =
            expand_name_argument(&self.name_scope(), XsltFn::ElementAvailable, name)?;
        Ok(crate::functions::element_available(&uri, local))
    }

    /// §18.1.1 function-available: an unprefixed name takes the standard
    /// function namespace; the two-argument form asks about one arity
    /// (`arity` is `Some`, see `functions::arity_argument`).
    ///
    /// "A function is said to be available within an XPath expression if it is
    /// present in the in-scope functions for that expression", and §10.3 puts
    /// every stylesheet function there, so they count as well as the
    /// processor's own.
    fn function_available(
        &self,
        name: &str,
        arity: Option<Option<usize>>,
    ) -> Result<bool, XsltError> {
        let (uri, local) =
            expand_name_argument(&self.name_scope(), XsltFn::FunctionAvailable, name)?;
        let arity = match arity {
            None => None,
            Some(Some(arity)) => Some(arity),
            Some(None) => return Ok(false),
        };
        let name = crate::names::QName::new(uri.as_str(), local);
        Ok(self.run.sheet.has_stylesheet_function(&name, arity)
            || crate::functions::function_available(
                self.run.sheet.functions(),
                &uri,
                local,
                arity,
                self.run.sheet.type_scope(),
            ))
    }

    /// §18.1.4 type-available.
    ///
    /// > The value of the `$type-name` argument must be a string containing a
    /// > lexical QName. The lexical QName is expanded into an expanded-QName
    /// > using the namespace declarations in scope for the expression. If the
    /// > lexical QName is unprefixed, then the **default namespace** is used in
    /// > the expanded QName.
    ///
    /// The default namespace for a *type* name is `[xsl:]xpath-default-namespace`,
    /// the one §5.2 gives unprefixed element and type names in expressions —
    /// which is normally absent, so an unprefixed name is then in no namespace
    /// and no built-in type matches it.
    fn type_available(&self, name: &str) -> Result<bool, XsltError> {
        let (uri, local) = expand_name_argument(&self.name_scope(), XsltFn::TypeAvailable, name)?;
        Ok(self.run.sheet.type_scope().type_available(&uri, local))
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

/// Whether an atomized argument value matches `xs:string` after the
/// conversions of XPath 2.0 §3.1.5: a string (or a type derived from it), an
/// `xs:untypedAtomic` (cast to `xs:string`), or an `xs:anyURI` (promoted to
/// `xs:string`, §B.1).
pub(super) fn converts_to_string(value: &XmlValue) -> bool {
    let value = match &value.value {
        XmlValueKind::Union(inner) => inner,
        _ => value,
    };
    value.type_code.is_string_derived()
        || matches!(
            value.type_code,
            XmlTypeCode::UntypedAtomic | XmlTypeCode::AnyUri
        )
}

/// The `xs:` local name of a value's type, for a message.
fn type_name(value: &XmlValue) -> &'static str {
    value.type_code.local_name().unwrap_or("anyAtomicType")
}

/// An atomized argument, described for a message.
pub(super) fn describe_atoms(atoms: &[XmlValue]) -> String {
    match atoms {
        [] => "an empty sequence".to_string(),
        [atom] => format!("a value of type xs:{}", type_name(atom)),
        several => format!("a sequence of {} values", several.len()),
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

/// Splits a URI reference at its fragment identifier: `Some((document,
/// fragment))` when it has one (RFC 3986 §3.5: the fragment is everything
/// after the first `#`), `None` otherwise.
fn fragment_split(uri: &str) -> Option<(&str, &str)> {
    uri.split_once('#')
}
