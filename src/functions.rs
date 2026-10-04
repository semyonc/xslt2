//! The XSLT function library (§16) as an XPath function catalog.
//!
//! §16 puts the functions XSLT adds — `document`, `key`, `current`,
//! `generate-id`, `system-property`, … — in the **standard function
//! namespace**, `http://www.w3.org/2005/xpath-functions`, so a stylesheet
//! calls them unprefixed. The XPath binder resolves an unprefixed function
//! name through `XPathContext::default_function_namespace()`, which is that
//! namespace, so registering them there is all that is needed
//! (`xsd-schema/src/xpath/bind.rs`).
//!
//! Registration mints a [`FunctionHandle`]; the closure a
//! [`FunctionSet`] stores with it is never called, because a `CustomFn` is
//! `Send + Sync + 'static` and cannot hold the borrowed engine state these
//! functions need. The real implementations live in
//! [`XsltEvaluator`](crate::runtime::XsltEvaluator), a `FunctionEvaluator`
//! that holds the running transformation (M0b, Q8).

use std::collections::{HashMap, HashSet};

use xsd_schema::types::sequence::{ItemType, SequenceType};
use xsd_schema::xpath::functions::{
    DynamicFunctionSignature, FunctionCatalog, FunctionHandle, FunctionSet,
};
use xsd_schema::xpath::XPathError;
use xsd_schema::RoXmlNavigator;

use crate::names::FN_NS;

/// Which XSLT function a [`FunctionHandle`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XsltFn {
    /// `current()` (§16.6.2).
    Current,
    /// `document($uri)` / `document($uri, $base)` (§16.1).
    Document,
    /// `key($name, $value)` / with a third argument (§16.3).
    Key,
    /// `generate-id()` / `generate-id($node)` (§16.6.1).
    GenerateId,
    /// `system-property($name)` (§16.6.5).
    SystemProperty,
    /// `element-available($name)` (§18.1.1).
    ElementAvailable,
    /// `function-available($name)` (§18.1.2).
    FunctionAvailable,
    /// `type-available($name)` (§18.1.3).
    TypeAvailable,
    /// `fn:doc($uri)` — absent upstream, implemented over the document store.
    Doc,
    /// `fn:doc-available($uri)`.
    DocAvailable,
    /// `fn:document-uri($node)` — the built-in reads the base URI of the
    /// document node, which is not the same thing as its document URI once a
    /// temporary tree has a base URI and no document URI (§9.4). The
    /// document store knows which document came from which URI, so the
    /// function is answered from there.
    DocumentUri,
    /// `unparsed-text($uri)` / `unparsed-text($uri, $encoding)` (§16.6.3).
    UnparsedText,
    /// `unparsed-text-available(…)` (§16.6.4).
    UnparsedTextAvailable,
    /// `unparsed-entity-uri($name)` (§16.6.6).
    UnparsedEntityUri,
    /// `unparsed-entity-public-id($name)` (§16.6.7).
    UnparsedEntityPublicId,
    /// `current-group()` (§14).
    CurrentGroup,
    /// `current-grouping-key()` (§14).
    CurrentGroupingKey,
    /// `regex-group($n)` (§15).
    RegexGroup,
    /// `format-number($value, $picture)` / with a decimal-format name (§16.4).
    FormatNumber,
    /// `format-dateTime($value, $picture)` / with language, calendar and
    /// country (§16.5).
    FormatDateTime,
    /// `format-date(…)` (§16.5).
    FormatDate,
    /// `format-time(…)` (§16.5).
    FormatTime,
    /// `fn:error()` … `fn:error($error, $description, $error-object)` — F&O's
    /// error-raising function. The engine ships the implementation but does not
    /// register it among its built-ins, so it is registered here.
    Error,
    /// §3.8's *fallback error function*: what a call to a function with no
    /// implementation is bound to under backwards-compatible behaviour (see
    /// `XsltFunctions::backwards_compatible_catalog`). Evaluating it raises
    /// XTDE1425.
    FallbackError,
}

impl XsltFn {
    /// Whether §3.12 puts this function in the static context of a `use-when`
    /// expression.
    ///
    /// > In scope functions: The core functions defined in \[Functions and
    /// > Operators\], together with the functions `element-available`,
    /// > `function-available`, `type-available`, and `system-property` defined in
    /// > this specification …
    ///
    /// So the four named ones are in scope, and so are the F&O functions this
    /// crate happens to register itself rather than inherit from the engine —
    /// `fn:doc`, `fn:doc-available`, `fn:document-uri` and `fn:error` are all
    /// F&O core, not XSLT additions. Every other §16 function is XSLT-specific
    /// and is simply absent, which makes a call to it XPST0017.
    pub fn in_use_when_scope(self) -> bool {
        matches!(
            self,
            XsltFn::ElementAvailable
                | XsltFn::FunctionAvailable
                | XsltFn::TypeAvailable
                | XsltFn::SystemProperty
                | XsltFn::Doc
                | XsltFn::DocAvailable
                | XsltFn::DocumentUri
                | XsltFn::Error
        )
    }

    /// The name a diagnostic uses.
    pub fn name(self) -> &'static str {
        match self {
            XsltFn::Current => "current",
            XsltFn::Document => "document",
            XsltFn::Key => "key",
            XsltFn::GenerateId => "generate-id",
            XsltFn::SystemProperty => "system-property",
            XsltFn::ElementAvailable => "element-available",
            XsltFn::FunctionAvailable => "function-available",
            XsltFn::TypeAvailable => "type-available",
            XsltFn::Doc => "doc",
            XsltFn::DocAvailable => "doc-available",
            XsltFn::DocumentUri => "document-uri",
            XsltFn::UnparsedText => "unparsed-text",
            XsltFn::UnparsedTextAvailable => "unparsed-text-available",
            XsltFn::UnparsedEntityUri => "unparsed-entity-uri",
            XsltFn::UnparsedEntityPublicId => "unparsed-entity-public-id",
            XsltFn::CurrentGroup => "current-group",
            XsltFn::CurrentGroupingKey => "current-grouping-key",
            XsltFn::RegexGroup => "regex-group",
            XsltFn::FormatNumber => "format-number",
            XsltFn::FormatDateTime => "format-dateTime",
            XsltFn::FormatDate => "format-date",
            XsltFn::FormatTime => "format-time",
            XsltFn::Error => "error",
            XsltFn::FallbackError => "the fallback error function",
        }
    }
}

/// The XSLT function catalog: hand this to
/// [`Compiler::new`](crate::Compiler::new) and keep it alive for as long as
/// the compiled stylesheet.
///
/// The set is a catalog only. Its navigator type parameter is irrelevant —
/// [`FunctionCatalog`] is not generic, and the stored closures are never
/// evaluated — so a `'static` navigator type is used and `XsltFunctions`
/// itself carries no lifetime.
pub struct XsltFunctions {
    /// The function set, wrapped with §3.8's fallback binding: `catalog.set`
    /// is the catalog of an ordinary expression, `catalog` itself that of an
    /// expression with backwards-compatible behaviour.
    catalog: BackwardsCompatibleCatalog,
    by_handle: HashMap<FunctionHandle, XsltFn>,
    names: HashMap<(&'static str, usize), XsltFn>,
    /// The handles registered by [`Self::add_stub`]: in the catalog so that a
    /// call compiles and reaches the evaluator, but **not** available (§18.1.2).
    stubs: HashSet<FunctionHandle>,
}

impl Default for XsltFunctions {
    fn default() -> Self {
        Self::new()
    }
}

impl XsltFunctions {
    /// The XPath built-ins plus the XSLT function library.
    pub fn new() -> Self {
        let mut this = Self {
            catalog: BackwardsCompatibleCatalog {
                set: FunctionSet::with_builtins(),
                // Replaced below, once the function is registered.
                fallback: None,
            },
            by_handle: HashMap::new(),
            names: HashMap::new(),
            stubs: HashSet::new(),
        };
        let string = SequenceType::string;
        let string_opt = SequenceType::string_optional;
        let item = SequenceType::item;
        let items = SequenceType::any;
        let node = SequenceType::node;
        let nodes = SequenceType::nodes;

        this.add(XsltFn::Current, "current", vec![], item());
        this.add(XsltFn::Document, "document", vec![items()], nodes());
        this.add(
            XsltFn::Document,
            "document",
            vec![items(), nodes()],
            nodes(),
        );
        this.add(XsltFn::Key, "key", vec![string(), items()], nodes());
        this.add(XsltFn::Key, "key", vec![string(), items(), node()], nodes());
        this.add(XsltFn::GenerateId, "generate-id", vec![], string());
        this.add(
            XsltFn::GenerateId,
            "generate-id",
            vec![SequenceType::optional(ItemType::AnyNode)],
            string(),
        );
        this.add(
            XsltFn::SystemProperty,
            "system-property",
            vec![string()],
            string(),
        );
        this.add(
            XsltFn::ElementAvailable,
            "element-available",
            vec![string()],
            SequenceType::boolean(),
        );
        this.add(
            XsltFn::FunctionAvailable,
            "function-available",
            vec![string()],
            SequenceType::boolean(),
        );
        // XSLT 2.0 defines only the one-argument form; the two-argument form
        // is XSLT 3.0's, and several forwards-compatible test stylesheets
        // guard 3.0 features with it, so it is offered as an extension.
        this.add(
            XsltFn::FunctionAvailable,
            "function-available",
            vec![string(), SequenceType::integer()],
            SequenceType::boolean(),
        );
        this.add(
            XsltFn::TypeAvailable,
            "type-available",
            vec![string()],
            SequenceType::boolean(),
        );
        this.add(
            XsltFn::Doc,
            "doc",
            vec![string_opt()],
            SequenceType::optional(ItemType::AnyNode),
        );
        this.add(
            XsltFn::DocAvailable,
            "doc-available",
            vec![string_opt()],
            SequenceType::boolean(),
        );
        // Shadows the built-in: `FunctionSet::register` takes precedence over
        // the built-in registry for the same name and arity.
        this.add(
            XsltFn::DocumentUri,
            "document-uri",
            vec![SequenceType::optional(ItemType::AnyNode)],
            SequenceType::optional(ItemType::AtomicType(xsd_schema::types::XmlTypeCode::AnyUri)),
        );
        this.add(
            XsltFn::UnparsedText,
            "unparsed-text",
            vec![string_opt()],
            string_opt(),
        );
        this.add(
            XsltFn::UnparsedText,
            "unparsed-text",
            vec![string_opt(), string()],
            string_opt(),
        );
        this.add(
            XsltFn::UnparsedTextAvailable,
            "unparsed-text-available",
            vec![string_opt()],
            SequenceType::boolean(),
        );
        this.add(
            XsltFn::UnparsedTextAvailable,
            "unparsed-text-available",
            vec![string_opt(), string()],
            SequenceType::boolean(),
        );
        this.add(
            XsltFn::UnparsedEntityUri,
            "unparsed-entity-uri",
            vec![string()],
            SequenceType::any_uri(),
        );
        this.add(
            XsltFn::UnparsedEntityPublicId,
            "unparsed-entity-public-id",
            vec![string()],
            string(),
        );
        this.add(XsltFn::CurrentGroup, "current-group", vec![], items());
        this.add(
            XsltFn::CurrentGroupingKey,
            "current-grouping-key",
            vec![],
            SequenceType::any_atomic_optional(),
        );
        this.add(
            XsltFn::RegexGroup,
            "regex-group",
            vec![SequenceType::integer()],
            string(),
        );

        // ── §16.4, §16.5: registered, not implemented ────────────────────
        //
        // M3d owns these four. They are in the catalog with their spec
        // signatures so that a stylesheet calling one *compiles* and fails at
        // evaluation with `XSLT2-NOTIMPL`; without them the call fails to bind
        // with XPST0017, which is indistinguishable from a wrong answer.
        // `function-available()` still says false — see `add_stub`.

        // §16.4:
        //   format-number($value as numeric?, $picture as xs:string)
        //     as xs:string
        //   format-number($value as numeric?, $picture as xs:string,
        //                 $decimal-format-name as xs:string) as xs:string
        //
        // `numeric?` is a union type the upstream signature model spells
        // `xs:double?`, exactly as it does for `fn:abs` and `fn:round`.
        let numeric_opt = SequenceType::double_optional;
        this.add_stub(
            XsltFn::FormatNumber,
            "format-number",
            vec![numeric_opt(), string()],
            string(),
        );
        this.add_stub(
            XsltFn::FormatNumber,
            "format-number",
            vec![numeric_opt(), string(), string()],
            string(),
        );

        // §16.5, each in a 2-argument and a 5-argument form:
        //   format-dateTime($value as xs:dateTime?, $picture as xs:string)
        //     as xs:string?
        //   format-dateTime($value as xs:dateTime?, $picture as xs:string,
        //                   $language as xs:string?, $calendar as xs:string?,
        //                   $country as xs:string?) as xs:string?
        // and likewise format-date over xs:date? and format-time over xs:time?.
        for (which, local, value) in [
            (
                XsltFn::FormatDateTime,
                "format-dateTime",
                SequenceType::optional(ItemType::AtomicType(
                    xsd_schema::types::XmlTypeCode::DateTime,
                )),
            ),
            (
                XsltFn::FormatDate,
                "format-date",
                SequenceType::optional(ItemType::AtomicType(xsd_schema::types::XmlTypeCode::Date)),
            ),
            (
                XsltFn::FormatTime,
                "format-time",
                SequenceType::optional(ItemType::AtomicType(xsd_schema::types::XmlTypeCode::Time)),
            ),
        ] {
            this.add_stub(which, local, vec![value.clone(), string()], string_opt());
            this.add_stub(
                which,
                local,
                vec![value, string(), string_opt(), string_opt(), string_opt()],
                string_opt(),
            );
        }

        // ── fn:error, arities 0 to 3 ─────────────────────────────────────
        //
        // The XPath engine provides the implementation
        // (`xpath::functions::special::error`) but deliberately leaves it out
        // of its built-in catalog, so a host that wants the function registers
        // it. It needs no running transformation, and the evaluator calls the
        // engine's function straight through. Registered with `add`, not
        // `add_stub`: it works, so `function-available('error')` is true.
        //
        // One signature per arity, with the types F&O gives that arity: the
        // one-argument form takes `xs:QName`, with no occurrence indicator, and
        // only the two- and three-argument forms accept an empty `$error`. The
        // engine's implementation enforces the same types itself, so a wrong
        // declaration here could not loosen them — but a right one is what
        // XPath 1.0 compatibility mode converts against.
        let qname = SequenceType::one(ItemType::AtomicType(xsd_schema::types::XmlTypeCode::QName));
        let qname_opt =
            SequenceType::optional(ItemType::AtomicType(xsd_schema::types::XmlTypeCode::QName));
        this.add(XsltFn::Error, "error", vec![], SequenceType::empty());
        this.add(XsltFn::Error, "error", vec![qname], SequenceType::empty());
        this.add(
            XsltFn::Error,
            "error",
            vec![qname_opt.clone(), string()],
            SequenceType::empty(),
        );
        this.add(
            XsltFn::Error,
            "error",
            vec![qname_opt, string(), items()],
            SequenceType::empty(),
        );

        // ── §3.8: the fallback error function ────────────────────────────
        //
        // One variadic entry with no declared parameter types, so that a call
        // of any arity binds to it and its arguments pass through untouched
        // (XPath 1.0 compatibility mode converts only the arguments a
        // signature declares). Its name is not a QName (`#` is not a name
        // character), so no expression can call it by name: it is reached
        // only through `BackwardsCompatibleCatalog`. Recorded as a stub, so
        // that `function-available()` could not answer true for it either.
        let handle = this.catalog.set.register(
            DynamicFunctionSignature::variadic(FALLBACK_NS, FALLBACK_LOCAL, 0, vec![], items()),
            |_ctx, _args| {
                Err(XPathError::internal(
                    "the fallback error function must be evaluated through xslt2's own \
                     FunctionEvaluator",
                ))
            },
        );
        this.by_handle.insert(handle, XsltFn::FallbackError);
        this.stubs.insert(handle);
        this.catalog.fallback = Some(handle);
        this
    }

    fn add(
        &mut self,
        which: XsltFn,
        local: &'static str,
        params: Vec<SequenceType>,
        result: SequenceType,
    ) -> FunctionHandle {
        let arity = params.len();
        let handle = self.catalog.set.register(
            DynamicFunctionSignature::new(FN_NS, local, params, result),
            move |_ctx, _args| {
                Err(XPathError::internal(format!(
                    "fn:{local} must be evaluated through xslt2's own FunctionEvaluator"
                )))
            },
        );
        self.by_handle.insert(handle, which);
        self.names.insert((local, arity), which);
        handle
    }

    /// Registers a §16 function this processor does **not** implement.
    ///
    /// The signature is the spec's, so a call binds and type-checks as it
    /// would against a complete processor, and the evaluator turns it into
    /// `XSLT2-NOTIMPL` naming the milestone that owns it. The handle is
    /// remembered as a stub so that [`Self::is_available`] — and with it
    /// `function-available()` (§18.1.2: "returns true if and only if there is
    /// a function available with the given name and arity") — keeps answering
    /// **false**, and so that a `use-when` expression cannot come to depend on
    /// a function that does not work.
    fn add_stub(
        &mut self,
        which: XsltFn,
        local: &'static str,
        params: Vec<SequenceType>,
        result: SequenceType,
    ) {
        let handle = self.add(which, local, params, result);
        self.stubs.insert(handle);
    }

    /// Pass this to `XPathContext::with_function_catalog`.
    pub fn catalog(&self) -> &dyn FunctionCatalog {
        &self.catalog.set
    }

    /// The catalog of an expression processed with backwards-compatible
    /// behaviour (§3.8): the same functions, and in addition "any function
    /// call for which no implementation is available (unless it uses the
    /// standard function namespace) is bound to a fallback error function
    /// whose effect when evaluated is to raise a dynamic error \[see ERR
    /// XTDE1425\]".
    ///
    /// Only the *call* is bound to the fallback, so the rest of the
    /// expression compiles and runs normally — a call guarded by
    /// `function-available()` is simply never evaluated (§3.8: "calls on
    /// extension functions that are not available in a particular
    /// implementation do not cause an error unless the function call is
    /// actually evaluated"). A missing name in the standard function namespace
    /// stays XPath's static XPST0017.
    pub(crate) fn backwards_compatible_catalog(&self) -> &dyn FunctionCatalog {
        &self.catalog
    }

    /// The catalog §3.12 gives a `use-when` expression: the same one with every
    /// XSLT-specific function removed (see [`XsltFn::in_use_when_scope`]).
    pub fn use_when_catalog(&self) -> UseWhenCatalog<'_> {
        UseWhenCatalog { functions: self }
    }

    /// Whether a name and arity is in scope in a `use-when` expression, which is
    /// what `function-available()` must answer there (§3.12: "The effect of this
    /// rule is to ensure that `function-available` returns true in respect of
    /// functions that can be called within the scope of the `use-when`
    /// attribute").
    pub fn is_available_in_use_when(&self, namespace: &str, local: &str, arity: usize) -> bool {
        match self.catalog.set.lookup(namespace, local, arity) {
            Some(handle) if self.stubs.contains(&handle) => false,
            Some(handle) => match self.by_handle.get(&handle) {
                Some(which) => which.in_use_when_scope(),
                // Not one of ours: an engine built-in, i.e. an F&O core
                // function, which §3.12 keeps in scope.
                None => true,
            },
            None => false,
        }
    }

    /// Which XSLT function a handle stands for, if it is one of ours.
    pub fn resolve(&self, handle: FunctionHandle) -> Option<XsltFn> {
        self.by_handle.get(&handle).copied()
    }

    /// Whether a name and arity in the standard function namespace resolves —
    /// what `function-available()` answers (§18.1.2).
    ///
    /// A function registered by `add_stub` is in the catalog but is not
    /// *available*: it exists only so that a call to it is classified as a
    /// missing feature rather than as an unknown function.
    pub fn is_available(&self, namespace: &str, local: &str, arity: usize) -> bool {
        match self.catalog.set.lookup(namespace, local, arity) {
            Some(handle) => !self.stubs.contains(&handle),
            None => false,
        }
    }
}

/// The namespace of §3.8's fallback error function. Never written by a
/// stylesheet: together with [`FALLBACK_LOCAL`], which is not a QName, it
/// only keeps the registration apart from every nameable function.
const FALLBACK_NS: &str = "urn:x-xslt2:fallback-error-function";

/// The local name of the fallback error function — deliberately not an
/// NCName, so that no function call can spell it.
const FALLBACK_LOCAL: &str = "#fallback";

/// The function catalog of an expression with backwards-compatible behaviour
/// (§3.8): the stylesheet's own catalog, plus the fallback binding. See
/// [`XsltFunctions::backwards_compatible_catalog`].
#[derive(Debug)]
struct BackwardsCompatibleCatalog {
    set: FunctionSet<RoXmlNavigator<'static>>,
    /// The fallback error function's handle; `None` only while
    /// [`XsltFunctions::new`] is still registering.
    fallback: Option<FunctionHandle>,
}

impl FunctionCatalog for BackwardsCompatibleCatalog {
    fn lookup(&self, namespace: &str, local_name: &str, arity: usize) -> Option<FunctionHandle> {
        match self.set.lookup(namespace, local_name, arity) {
            Some(handle) => Some(handle),
            // "(unless it uses the standard function namespace)": an unknown
            // name or arity there is XPST0017 whatever the mode.
            None if namespace == FN_NS => None,
            None => self.fallback,
        }
    }

    fn get_signature(&self, handle: FunctionHandle) -> Option<DynamicFunctionSignature> {
        self.set.get_signature(handle)
    }
}

/// The function catalog of a `use-when` expression (§3.12).
///
/// A view over [`XsltFunctions`] that hides every XSLT-specific function, so
/// that a `use-when` expression calling one fails to bind — XPST0017 — exactly
/// as it would against a processor that never defined it. Hiding them here
/// rather than refusing them in the evaluator is what makes
/// `function-available()`, the sequence-type checks and the error code all agree.
pub struct UseWhenCatalog<'f> {
    functions: &'f XsltFunctions,
}

impl std::fmt::Debug for UseWhenCatalog<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UseWhenCatalog")
    }
}

impl FunctionCatalog for UseWhenCatalog<'_> {
    fn lookup(&self, namespace: &str, local_name: &str, arity: usize) -> Option<FunctionHandle> {
        let handle = self
            .functions
            .catalog
            .set
            .lookup(namespace, local_name, arity)?;
        match self.functions.by_handle.get(&handle) {
            Some(which) if !which.in_use_when_scope() => None,
            _ => Some(handle),
        }
    }

    fn get_signature(&self, handle: FunctionHandle) -> Option<DynamicFunctionSignature> {
        self.functions.catalog.set.get_signature(handle)
    }
}

/// The XSLT functions that need no running transformation: `system-property`,
/// `element-available`, `function-available` and `type-available`.
///
/// `use-when` (§3.12) is evaluated while the stylesheet is still being
/// compiled, and those four are exactly what it is allowed to call.
pub struct StaticEvaluator<'f> {
    functions: &'f XsltFunctions,
    /// The in-scope namespace bindings of the element carrying the
    /// `use-when` attribute — §3.12's "In scope namespaces".
    namespaces: Vec<(String, String)>,
    /// §3.12's "Default element/type namespace: determined by the
    /// `xpath-default-namespace` attribute if present … otherwise the null
    /// namespace". Without it an unprefixed **type** name in a `use-when`
    /// expression would expand differently here than it does at run time.
    xpath_default_namespace: String,
    /// An error one of these functions raised, with its own code.
    /// `XPathError` has no host-error variant, so the engine carries an
    /// `XPathError::internal` with the same message and the caller collects
    /// the real error here with [`Self::take_error`] — the run-time evaluator's
    /// `parked_error`, for the `use-when` evaluator.
    parked: std::cell::RefCell<Option<crate::error::XsltError>>,
}

impl<'f> StaticEvaluator<'f> {
    pub fn new(
        functions: &'f XsltFunctions,
        namespaces: Vec<(String, String)>,
        xpath_default_namespace: String,
    ) -> Self {
        Self {
            functions,
            namespaces,
            xpath_default_namespace,
            parked: std::cell::RefCell::new(None),
        }
    }

    /// The error a function raised during the last evaluation, if any —
    /// consulted when the evaluation failed, since the engine's own error only
    /// carries its message.
    pub fn take_error(&self) -> Option<crate::error::XsltError> {
        self.parked.borrow_mut().take()
    }

    fn park(&self, e: crate::error::XsltError) -> XPathError {
        let carried = XPathError::internal(e.to_string());
        *self.parked.borrow_mut() = Some(e);
        carried
    }

    /// The static context these four functions expand their argument against —
    /// the same type the run-time evaluator builds.
    fn scope(&self) -> NameScope<'_> {
        NameScope {
            bindings: &self.namespaces,
            xpath_default_namespace: &self.xpath_default_namespace,
        }
    }
}

impl<N: xsd_schema::navigator::DomNavigator> xsd_schema::xpath::functions::FunctionEvaluator<N>
    for StaticEvaluator<'_>
{
    fn eval(
        &self,
        handle: FunctionHandle,
        ctx: &mut xsd_schema::xpath::DynamicContext<'_, N>,
        args: Vec<xsd_schema::xpath::functions::XPathValue<N>>,
    ) -> Result<xsd_schema::xpath::functions::XPathValue<N>, XPathError> {
        use xsd_schema::xpath::functions::{BuiltinEvaluator, XPathValue};
        let Some(which) = self.functions.resolve(handle) else {
            return BuiltinEvaluator.eval(handle, ctx, args);
        };
        let name = args
            .first()
            .and_then(|v| v.first())
            .map(|item| match item {
                xsd_schema::xpath::XmlItem::Atomic(v) => v.to_string_value(),
                xsd_schema::xpath::XmlItem::Node(n) => {
                    xsd_schema::navigator::DomNavigator::value(n)
                }
            })
            .unwrap_or_default();
        let scope = self.scope();
        // §3.12 does not exempt a `use-when` expression from these functions'
        // errors: a name that cannot be expanded is XTDE1390 / XTDE1400 /
        // XTDE1428 / XTDE1440 here exactly as at run time.
        let expand = || expand_name_argument(&scope, which, &name).map_err(|e| self.park(e));
        match which {
            XsltFn::SystemProperty => {
                let (uri, local) = expand()?;
                Ok(XPathValue::string(if uri == crate::names::XSLT_NS {
                    system_property(local)
                } else {
                    ""
                }))
            }
            XsltFn::ElementAvailable => {
                let (uri, local) = expand()?;
                Ok(XPathValue::boolean(element_available(&uri, local)))
            }
            XsltFn::FunctionAvailable => {
                let (uri, local) = expand()?;
                // §3.12: the answer is about the functions that can be called
                // *from here*, which is the use-when set, not the stylesheet's.
                Ok(XPathValue::boolean((0..=MAX_ARITY).any(|arity| {
                    self.functions.is_available_in_use_when(&uri, local, arity)
                })))
            }
            XsltFn::TypeAvailable => {
                let (uri, local) = expand()?;
                Ok(XPathValue::boolean(type_available(&uri, local)))
            }
            // \[Functions and Operators\] §15.5.4 `fn:doc`: if the argument is
            // the empty sequence the result is the empty sequence. No URI is
            // looked up, so §3.12's empty set of available documents is never
            // consulted and there is nothing to report — `empty(doc(()))` is
            // true in a `use-when` expression as it is anywhere else.
            //
            // (F&O is not available locally; this is the wording from memory,
            // corroborated by XQTS 1.0.2 `fn-doc-4`, whose whole query is
            // `fn:count(fn:doc(()))` with the expected result `0`.)
            XsltFn::Doc if args.first().is_none_or(XPathValue::is_empty) => Ok(XPathValue::empty()),
            // §3.12: "Statically known documents: None" and "Available
            // documents: None", so `fn:doc` can never find its document. F&O
            // gives that FODC0002 (`use-when-0128`). It is the processor's
            // error, not one the stylesheet raised through `fn:error`, so it
            // is parked like the four name errors above and collected with
            // `ErrorOrigin::Processor`; the engine itself only carries its
            // message.
            XsltFn::Doc => Err(self.park(crate::error::XsltError::new(
                "FODC0002",
                format!(
                    "a use-when expression has no available documents, so doc({name:?}) cannot \
                     be resolved"
                ),
            ))),
            // F&O `fn:doc-available`: "If `$uri` is the empty sequence, this
            // function returns false" (from memory) — and with no available
            // documents at all, false is the answer for every other argument
            // too, so one arm covers both.
            XsltFn::DocAvailable => Ok(XPathValue::boolean(false)),
            // F&O `fn:document-uri`: the empty sequence for an empty argument,
            // and for anything that is not a document node with a URI — and a
            // `use-when` expression has no nodes at all. XQTS
            // `K-DocumentURIFunc-3` (`empty(document-uri(()))` = `true`) and
            // `fn-document-uri-2` (`count(document-uri(()))` = `0`).
            XsltFn::DocumentUri => Ok(XPathValue::empty()),
            XsltFn::Error => BuiltinEvaluator.eval(handle, ctx, args),
            other => Err(XPathError::internal(format!(
                "{}() cannot be called from a use-when expression",
                other.name()
            ))),
        }
    }
}

/// The highest arity any function of XSLT 2.0 or \[Functions and Operators\] has
/// (`fn:format-dateTime`, `fn:format-date` and `fn:format-time` take five),
/// plus room for a variadic `fn:concat`.
///
/// `function-available()` with one argument asks whether the function exists at
/// *any* arity (§18.1.2), and the catalog can only be asked one arity at a time,
/// so the range has to cover every signature that could answer yes. Scanning to
/// 4 — as the helper did before M2c — made `function-available('format-date')`
/// and `function-available('concat')` answer false for the 5-argument forms.
const MAX_ARITY: usize = 20;

// ── Expanding the lexical QName these four functions are given ──────────────

/// The slice of a static context that `system-property()` and the three
/// `*-available()` functions need in order to expand their argument.
///
/// One type, built by the `use-when` evaluator (§3.12) and by the run-time
/// evaluator (§16) alike, so that a name cannot expand one way statically and
/// another way at run time.
pub struct NameScope<'a> {
    /// The expression's in-scope namespace bindings, prefix → URI. The entry for
    /// the empty prefix, when there is one, is the **default namespace of the
    /// defining element** (§5.1).
    pub bindings: &'a [(String, String)],
    /// `[xsl:]xpath-default-namespace` — the default namespace for element and
    /// type names (§5.2), and the "Default element/type namespace" of §3.12's
    /// static-context table for a `use-when` expression.
    pub xpath_default_namespace: &'a str,
}

impl NameScope<'_> {
    /// §5.1: a prefix is expanded "using the namespace declarations in effect on
    /// its defining element". The `xml` prefix is always bound.
    fn resolve_prefix(&self, prefix: &str) -> Option<&str> {
        if prefix == crate::names::XML_PREFIX {
            return Some(crate::names::XML_NS);
        }
        self.bindings
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
    }
}

/// Which namespace an **unprefixed** lexical QName gets. §5.1 is explicit that
/// this differs per function, and lists the exceptions:
///
/// > The default namespace of the defining element … is **not** used for
/// > unprefixed names. There are three cases where the default namespace of the
/// > defining element is used when expanding an unprefixed QName: … The default
/// > namespace is used when expanding the first argument of the function
/// > `element-available`. …
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unprefixed {
    /// §5.1's general rule: the name is in no namespace. `system-property()`
    /// (§16.6.5, which expands "as described in 5.1 Qualified Names").
    NoNamespace,
    /// §5.1's second exception: the default namespace of the defining element —
    /// the `xmlns=` default, *not* `xpath-default-namespace`. §18.2.2 says the
    /// same: "If there is a default namespace in scope, then it is used to
    /// expand an unprefixed QName." `element-available()`.
    DefaultElementNamespace,
    /// §18.1.1: "If the lexical QName is unprefixed, then the standard function
    /// namespace is used in the expanded QName." `function-available()`.
    FunctionNamespace,
    /// §18.1.4: "If the lexical QName is unprefixed, then the default namespace
    /// is used in the expanded QName" — for a *type* name that is the default
    /// namespace for element and type names, which §5.2 defines as the effective
    /// value of `[xsl:]xpath-default-namespace`. `type-available()`.
    TypeNamespace,
}

/// Why the argument of one of these functions cannot be expanded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// The argument is not a lexical QName, `NCName (":" NCName)?`.
    NotAQName,
    /// The argument has a prefix with no namespace declaration in scope.
    UnboundPrefix,
}

/// Expand the lexical QName `name` the way the function `unprefixed` belongs to
/// expands it, and return `(namespace URI, local name)`.
///
/// XML whitespace around the name is ignored. The error says which of the two
/// failures §16.6.5 and §18 name it was; [`expand_name_argument`] turns it
/// into the code of the function that was called.
pub fn expand_available_name<'n>(
    scope: &NameScope<'_>,
    name: &'n str,
    unprefixed: Unprefixed,
) -> Result<(String, &'n str), NameError> {
    let (prefix, local) =
        crate::names::lexical_qname(crate::whitespace::trim(name)).ok_or(NameError::NotAQName)?;
    if !prefix.is_empty() {
        return scope
            .resolve_prefix(prefix)
            .map(|uri| (uri.to_string(), local))
            .ok_or(NameError::UnboundPrefix);
    }
    let uri = match unprefixed {
        Unprefixed::NoNamespace => String::new(),
        Unprefixed::DefaultElementNamespace => {
            scope.resolve_prefix("").unwrap_or_default().to_string()
        }
        Unprefixed::FunctionNamespace => crate::names::FN_NS.to_string(),
        Unprefixed::TypeNamespace => scope.xpath_default_namespace.to_string(),
    };
    Ok((uri, local))
}

/// Expand the argument of `which` — `system-property`, `function-available`,
/// `type-available` or `element-available` — by that function's rule for an
/// unprefixed name, raising that function's error when it cannot be expanded.
///
/// Each of the four gives the same two failures one code of its own:
///
/// > \[§16.6.5\] \[ERR XTDE1390\] It is a non-recoverable dynamic error if the
/// > value is not a valid QName, or if there is no namespace declaration in
/// > scope for the prefix of the QName.
/// >
/// > \[§18.1.1\] \[ERR XTDE1400\] It is a non-recoverable dynamic error if the
/// > argument does not evaluate to a string that is a valid QName, or if there is
/// > no namespace declaration in scope for the prefix of the QName.
///
/// §18.1.4's XTDE1428 and §18.2.2's XTDE1440 are the same sentence as
/// XTDE1400. The `use-when` evaluator and the run-time evaluator both call this,
/// so a name cannot be an error on one path and `false` on the other; §3.12 does
/// not exempt a `use-when` expression from the error.
pub fn expand_name_argument<'n>(
    scope: &NameScope<'_>,
    which: XsltFn,
    name: &'n str,
) -> Result<(String, &'n str), crate::error::XsltError> {
    use crate::error::{codes, XsltError};
    let (unprefixed, code) = match which {
        XsltFn::SystemProperty => (Unprefixed::NoNamespace, codes::XTDE1390),
        XsltFn::FunctionAvailable => (Unprefixed::FunctionNamespace, codes::XTDE1400),
        XsltFn::TypeAvailable => (Unprefixed::TypeNamespace, codes::XTDE1428),
        XsltFn::ElementAvailable => (Unprefixed::DefaultElementNamespace, codes::XTDE1440),
        other => {
            return Err(XsltError::internal(format!(
                "{}() does not take a name argument",
                other.name()
            )))
        }
    };
    expand_available_name(scope, name, unprefixed).map_err(|e| {
        let function = which.name();
        XsltError::new(
            code,
            match e {
                NameError::NotAQName => {
                    format!("the argument of {function}(), {name:?}, is not a lexical QName")
                }
                NameError::UnboundPrefix => format!(
                    "the argument of {function}(), {name:?}, has a prefix with no namespace \
                     declaration in scope"
                ),
            },
        )
    })
}

// ── The three functions that are answerable without a running transformation

/// §16.6.5: the value of a system property in the XSLT namespace.
pub fn system_property(local: &str) -> &'static str {
    match local {
        "version" => "2.0",
        "vendor" => "semyonc",
        "vendor-url" => "https://github.com/semyonc/xsd-schema",
        "product-name" => "xslt2",
        "product-version" => env!("CARGO_PKG_VERSION"),
        "is-schema-aware" => "no",
        "supports-serialization" => "yes",
        "supports-backwards-compatibility" => "yes",
        _ => "",
    }
}

/// §18.1.1: whether this processor implements an XSLT instruction.
///
/// > The function returns true if and only if … the expanded-QName identifies
/// > an instruction that the implementation supports.
///
/// The XSLT declarations are not instructions, so they answer false.
pub fn element_available(uri: &str, local: &str) -> bool {
    uri == crate::names::XSLT_NS
        && matches!(
            local,
            "apply-templates"
                | "apply-imports"
                | "attribute"
                | "call-template"
                | "choose"
                | "comment"
                | "copy"
                | "copy-of"
                | "document"
                | "element"
                | "fallback"
                | "for-each"
                | "if"
                | "message"
                | "namespace"
                | "next-match"
                | "processing-instruction"
                | "sequence"
                | "text"
                | "value-of"
                | "variable"
        )
}

/// §18.1.2: whether a function of this name is available at any arity.
pub fn function_available(functions: &XsltFunctions, uri: &str, local: &str) -> bool {
    (0..=MAX_ARITY).any(|arity| functions.is_available(uri, local, arity))
}

/// §18.1.4: whether a schema type of this name is in the static context.
///
/// > A schema type (that is, a simple type or a complex type) is said to be
/// > available within an XPath expression if it is a type definition that is
/// > present in the in-scope schema types for that expression … The function
/// > returns true if and only if there is an available type whose name matches
/// > the value of the `$type-name` argument.
///
/// For a **basic** XSLT processor those are exactly the types §3.13 lists, and
/// §21.1 repeats the list: "The static context, which defines the full set of
/// type names recognized by an XSLT processor and also by the XPath processor,
/// includes these atomic types, plus `xs:anyType`, `xs:anySimpleType`,
/// `xs:untyped`, and `xs:anyAtomicType`." Nothing else is available, because
/// XTSE1650 forbids `xsl:import-schema`.
pub fn type_available(uri: &str, local: &str) -> bool {
    uri == crate::names::XSD_NS && BUILT_IN_TYPES.contains(&local)
}

/// §3.13 Built-in Types: every named type definition an XSLT 2.0 processor has
/// in its in-scope schema components without a schema.
///
/// "All the primitive atomic types defined in \[XML Schema Part 2\], with the
/// exception of `xs:NOTATION`", the derived `xs:integer`, `xs:anyType` and
/// `xs:anySimpleType`, and the five types \[XPath 2.0\] adds.
const BUILT_IN_TYPES: &[&str] = &[
    // The 18 primitive atomic types, less xs:NOTATION.
    "string",
    "boolean",
    "decimal",
    "double",
    "float",
    "date",
    "time",
    "dateTime",
    "duration",
    "QName",
    "anyURI",
    "gDay",
    "gMonthDay",
    "gMonth",
    "gYearMonth",
    "gYear",
    "base64Binary",
    "hexBinary",
    // The one derived atomic type.
    "integer",
    // The two structural types.
    "anyType",
    "anySimpleType",
    // The five from XPath 2.0.
    "yearMonthDuration",
    "dayTimeDuration",
    "anyAtomicType",
    "untyped",
    "untypedAtomic",
];

#[cfg(test)]
mod tests {
    use super::*;
    use xsd_schema::namespace::NameTable;
    use xsd_schema::xpath::{XPathContext, XPathExpr};

    #[test]
    fn the_xslt_functions_bind_unprefixed() {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let ctx = XPathContext::new(&names).with_function_catalog(functions.catalog());
        for source in [
            "current()",
            "document('a.xml')",
            "generate-id()",
            "generate-id(.)",
            "system-property('xsl:version')",
            "element-available('xsl:if')",
            "function-available('concat')",
            "doc('a.xml')",
            "doc-available('a.xml')",
            "unparsed-text('a.txt')",
            "key('k', 'v')",
        ] {
            XPathExpr::compile(source, &ctx)
                .unwrap_or_else(|e| panic!("{source} did not compile: {e}"));
        }
    }

    #[test]
    fn a_handle_resolves_back_to_its_function() {
        let functions = XsltFunctions::new();
        let handle = functions
            .catalog()
            .lookup(FN_NS, "generate-id", 0)
            .expect("generate-id/0 is registered");
        assert_eq!(functions.resolve(handle), Some(XsltFn::GenerateId));
        // A built-in handle is not one of ours.
        let concat = functions.catalog().lookup(FN_NS, "concat", 2).unwrap();
        assert_eq!(functions.resolve(concat), None);
    }

    #[test]
    fn builtin_functions_still_resolve_through_the_same_catalog() {
        let functions = XsltFunctions::new();
        assert!(functions.is_available(FN_NS, "substring", 2));
        assert!(functions.is_available(FN_NS, "current", 0));
        assert!(!functions.is_available(FN_NS, "no-such-function", 1));
    }
}
