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

use crate::collation::{Collation, CollationRegistry};
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
    /// The collations every expression of a stylesheet compiled against this
    /// set can name (§13.1.3, DESIGN §9.5). It lives here because this is the
    /// caller-owned object every `XPathContext` already borrows.
    collations: CollationRegistry,
}

impl Default for XsltFunctions {
    fn default() -> Self {
        Self::new()
    }
}

impl XsltFunctions {
    /// Registers a host collation under an absolute collation URI, for every
    /// stylesheet compiled against this set afterwards: `xsl:sort
    /// collation="…"`, `[xsl:]default-collation`, and the `$collation`
    /// argument of `compare()`, `contains()`, `distinct-values()` and the other
    /// collation-aware functions (§13.1.3, §3.6.1, F&O §7.3).
    ///
    /// The codepoint collation and
    /// [`HTML_ASCII_CASE_INSENSITIVE_URI`](crate::collation::HTML_ASCII_CASE_INSENSITIVE_URI)
    /// are built in. A host collation may replace the second, never the first:
    /// registering
    /// [`CODEPOINT_COLLATION_URI`](crate::collation::CODEPOINT_COLLATION_URI)
    /// is refused and returns `false` (see [`CollationRegistry::register`]).
    ///
    /// `compare` must be a total order — consistent, antisymmetric and
    /// transitive — because `xsl:sort` hands it to the standard library's sort,
    /// which may panic on a comparison function that is not one, and because
    /// the `xsl:key` and `group-by` indexes of a collation without sort keys
    /// are ordered by it (below).
    ///
    /// # Implement `sort_key` — keys and `=` need it to be fast
    ///
    /// [`Collation::sort_key`] is optional, but without it every lookup that
    /// hashes strings under the collation has to do without hashing:
    ///
    /// * an `xsl:key` or `xsl:for-each-group group-by` whose (effective)
    ///   collation is this one keeps its string values in an index **ordered
    ///   by `compare`**, so each lookup is a binary search — O(log N)
    ///   comparisons instead of a hash (the results are the same);
    /// * a general comparison `$a = $b` over large sequences under this
    ///   collation is evaluated pair by pair instead of through a hash index,
    ///   and so is `distinct-values()`.
    ///
    /// A sort key is any byte string whose equality and order agree with
    /// `compare` (`sort_key(a).cmp(&sort_key(b)) == compare(a, b)`); the
    /// example below has one.
    ///
    /// ```
    /// use std::cmp::Ordering;
    /// use std::rc::Rc;
    /// use xslt2::collation::Collation;
    /// use xslt2::XsltFunctions;
    ///
    /// /// Orders strings by length, then by codepoints.
    /// struct ByLength;
    /// impl Collation for ByLength {
    ///     fn compare(&self, a: &str, b: &str) -> Ordering {
    ///         a.chars().count().cmp(&b.chars().count()).then_with(|| a.cmp(b))
    ///     }
    ///
    ///     /// The length as a fixed-width big-endian number, then the UTF-8
    ///     /// bytes (whose order is codepoint order): byte order is `compare`'s
    ///     /// order, and equal keys are equal strings.
    ///     fn sort_key(&self, s: &str) -> Option<Vec<u8>> {
    ///         let mut key = (s.chars().count() as u64).to_be_bytes().to_vec();
    ///         key.extend_from_slice(s.as_bytes());
    ///         Some(key)
    ///     }
    /// }
    ///
    /// let by_length = ByLength;
    /// for (a, b) in [("b", "aa"), ("ab", "aa"), ("ab", "ab")] {
    ///     assert_eq!(by_length.sort_key(a).cmp(&by_length.sort_key(b)), by_length.compare(a, b));
    /// }
    ///
    /// let mut functions = XsltFunctions::new();
    /// assert!(functions.register_collation("urn:example:by-length", Rc::new(ByLength)));
    /// assert!(functions.collations().recognizes("urn:example:by-length"));
    /// ```
    pub fn register_collation(
        &mut self,
        uri: impl Into<String>,
        collation: std::rc::Rc<dyn Collation>,
    ) -> bool {
        self.collations.register(uri, collation)
    }

    /// The collations stylesheets compiled against this set can name.
    pub fn collations(&self) -> &CollationRegistry {
        &self.collations
    }

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
            collations: CollationRegistry::new(),
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
        // §18.1.1 defines both forms: `function-available($function-name as
        // xs:string) as xs:boolean` and `function-available($function-name as
        // xs:string, $arity as xs:integer) as xs:boolean`.
        this.add(
            XsltFn::FunctionAvailable,
            "function-available",
            vec![string()],
            SequenceType::boolean(),
        );
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
    ///
    /// §3.12 keeps "the core functions defined in \[Functions and Operators\]"
    /// in scope, and the constructor functions are among them (see
    /// [`is_constructor_function`]).
    pub fn is_available_in_use_when(&self, namespace: &str, local: &str, arity: usize) -> bool {
        if is_constructor_function(namespace, local, arity) {
            return true;
        }
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

    /// Whether one of the processor's functions of this name and arity is in
    /// the in-scope functions — what `function-available()` answers (§18.1.1)
    /// for everything but the stylesheet functions: the catalog, and the
    /// constructor functions the XPath binder resolves outside it (see
    /// [`is_constructor_function`]).
    ///
    /// A function registered by `add_stub` is in the catalog but is not
    /// *available*: it exists only so that a call to it is classified as a
    /// missing feature rather than as an unknown function.
    pub fn is_available(&self, namespace: &str, local: &str, arity: usize) -> bool {
        if is_constructor_function(namespace, local, arity) {
            return true;
        }
        match self.catalog.set.lookup(namespace, local, arity) {
            Some(handle) => !self.stubs.contains(&handle),
            None => false,
        }
    }

    /// `n` function handles that no handle of this set's own catalog can be
    /// confused with, for the stylesheet functions of one compilation (§10.3).
    ///
    /// The engine mints a custom handle only through `FunctionSet::register`,
    /// which numbers its entries from a fixed base, so two sets number theirs
    /// alike. The handles are therefore minted from a scratch set that first
    /// skips every number this set has used: a stylesheet-function handle then
    /// resolves to nothing here — [`Self::resolve`] says `None` and the
    /// catalog's `get_signature` finds no entry — and the evaluator can tell
    /// the two kinds apart. (A public `FunctionHandle` constructor, ROADMAP
    /// §3.5, would make the scratch set unnecessary.)
    pub(crate) fn mint_handles(&self, n: usize) -> Vec<FunctionHandle> {
        let mut scratch: FunctionSet<RoXmlNavigator<'static>> = FunctionSet::new();
        let placeholder = || {
            DynamicFunctionSignature::new(
                FALLBACK_NS,
                "#placeholder",
                vec![],
                SequenceType::empty(),
            )
        };
        for _ in 0..self.catalog.set.custom_count() {
            scratch.register(placeholder(), never_called);
        }
        (0..n)
            .map(|_| scratch.register(placeholder(), never_called))
            .collect()
    }
}

/// The implementation `mint_handles` registers with each handle: never called,
/// because the handle is only ever evaluated by xslt2's own `FunctionEvaluator`.
fn never_called(
    _: &mut xsd_schema::xpath::DynamicContext<'_, RoXmlNavigator<'static>>,
    _: Vec<xsd_schema::xpath::functions::XPathValue<RoXmlNavigator<'static>>>,
) -> Result<xsd_schema::xpath::functions::XPathValue<RoXmlNavigator<'static>>, XPathError> {
    Err(XPathError::internal(
        "a stylesheet function must be evaluated through xslt2's own FunctionEvaluator",
    ))
}

// ── Stylesheet functions in the static context (§10.3) ─────────────────────

/// The stylesheet functions of one compilation, as the XPath binder sees them:
/// the winning declaration of every expanded QName and arity (§10.3).
///
/// Built once, by the compiler's declaration pre-pass, before the first
/// expression of the stylesheet is compiled.
#[derive(Debug, Default)]
pub(crate) struct StylesheetFunctionTable {
    /// `(namespace URI, local name, arity)` → the handle and the `override`
    /// attribute of the declaration that won.
    entries: HashMap<(String, String, usize), (FunctionHandle, bool)>,
    /// What `get_signature` reports for each handle.
    signatures: HashMap<FunctionHandle, DynamicFunctionSignature>,
}

impl StylesheetFunctionTable {
    /// Records the winning declaration of `uri`/`local` with `arity`
    /// parameters.
    ///
    /// The signature declares every parameter and the result as `item()*`.
    /// §10.3 converts the arguments of a stylesheet function by "the rules …
    /// for the case where XPath 1.0 compatibility mode is set to false", and the
    /// engine applies the XPath 1.0 conversions only to a parameter declared as
    /// a single or optional single item, so a call written in a
    /// backwards-compatible region stays untouched by them; the real `as`
    /// conversion is the evaluator's (XTTE0790).
    pub(crate) fn insert(
        &mut self,
        uri: &str,
        local: &str,
        arity: usize,
        handle: FunctionHandle,
        override_vendor: bool,
    ) {
        self.entries.insert(
            (uri.to_string(), local.to_string(), arity),
            (handle, override_vendor),
        );
        self.signatures.insert(
            handle,
            DynamicFunctionSignature::new(
                uri,
                local,
                vec![SequenceType::any(); arity],
                SequenceType::any(),
            ),
        );
    }

    fn get(&self, uri: &str, local: &str, arity: usize) -> Option<(FunctionHandle, bool)> {
        self.entries
            .get(&(uri.to_string(), local.to_string(), arity))
            .copied()
    }
}

/// The function catalog the compiler binds an expression against: the
/// caller's [`XsltFunctions`] plus the stylesheet functions of the compilation
/// in progress (§10.3).
///
/// > A stylesheet function is included in the in-scope functions of the static
/// > context for all XPath expressions used in the stylesheet, unless … the
/// > `override` attribute has the value `no` and there is already a function
/// > with the same name and arity in the in-scope functions.
///
/// So a stylesheet function with `override="yes"` (the default) is looked up
/// before the processor's own functions, and one with `override="no"` after
/// them. Either way it comes before §3.8's fallback binding, which is only for
/// "any function call for which no implementation is available".
///
/// The catalog lives only as long as the compilation: the static contexts a
/// [`CompiledStylesheet`](crate::CompiledStylesheet) keeps for evaluation
/// point at [`XsltFunctions`] itself, which is all evaluation consults —
/// `get_signature`, for the XPath 1.0 conversions §10.3 says do not apply to a
/// stylesheet function, and which therefore finds nothing for one.
pub(crate) struct StylesheetCatalog<'a, 'm> {
    functions: &'a XsltFunctions,
    table: &'m std::cell::OnceCell<StylesheetFunctionTable>,
    /// Whether this is the catalog of an expression with backwards-compatible
    /// behaviour (§3.8).
    backwards: bool,
}

impl<'a, 'm> StylesheetCatalog<'a, 'm> {
    pub(crate) fn new(
        functions: &'a XsltFunctions,
        table: &'m std::cell::OnceCell<StylesheetFunctionTable>,
        backwards: bool,
    ) -> Self {
        Self {
            functions,
            table,
            backwards,
        }
    }
}

impl std::fmt::Debug for StylesheetCatalog<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StylesheetCatalog")
            .field("backwards", &self.backwards)
            .field("table", &self.table.get())
            .finish()
    }
}

impl FunctionCatalog for StylesheetCatalog<'_, '_> {
    fn lookup(&self, namespace: &str, local_name: &str, arity: usize) -> Option<FunctionHandle> {
        let processor = || {
            self.functions
                .catalog
                .set
                .lookup(namespace, local_name, arity)
        };
        if let Some((handle, override_vendor)) = self
            .table
            .get()
            .and_then(|table| table.get(namespace, local_name, arity))
        {
            // §10.3: "If the override attribute has the value yes, then this
            // function is used in preference; if it has the value no, then the
            // other function is used in preference."
            if override_vendor || processor().is_none() {
                return Some(handle);
            }
        }
        if self.backwards {
            self.functions.catalog.lookup(namespace, local_name, arity)
        } else {
            processor()
        }
    }

    fn get_signature(&self, handle: FunctionHandle) -> Option<DynamicFunctionSignature> {
        if let Some(signature) = self
            .table
            .get()
            .and_then(|table| table.signatures.get(&handle))
        {
            return Some(signature.clone());
        }
        self.functions.catalog.get_signature(handle)
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
                // *from here*, which is the use-when set, not the stylesheet's
                // — "stylesheet functions are not included in the context,
                // which means that the function function-available will return
                // false in respect of such functions". §18.1.1: the
                // two-argument form asks about one arity only.
                let arity = arity_argument(args.get(1)).map_err(|e| self.park(e))?;
                Ok(XPathValue::boolean(match arity {
                    None => (0..=MAX_ARITY)
                        .any(|arity| self.functions.is_available_in_use_when(&uri, local, arity)),
                    Some(Some(arity)) => {
                        self.functions.is_available_in_use_when(&uri, local, arity)
                    }
                    Some(None) => false,
                }))
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
/// `type-available`, `element-available` or the first argument of `key` — by
/// that function's rule for an
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
        // §16.3.2: "The value of the argument must be a lexical QName, which is
        // expanded as described in 5.1 Qualified Names" — §5.1's general rule,
        // no namespace for an unprefixed name — and "[ERR XTDE1260] It is a
        // non-recoverable dynamic error if the value is not a valid QName, or if
        // there is no namespace declaration in scope for the prefix of the
        // QName".
        XsltFn::Key => (Unprefixed::NoNamespace, codes::XTDE1260),
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
/// Every instruction of the static-checking table (`compile::checks`, one row
/// per XSLT 2.0 element) except those in `NOT_YET_IMPLEMENTED_INSTRUCTIONS`.
/// The XSLT declarations and the subordinate elements (`xsl:sort`,
/// `xsl:matching-substring`, …) are not instructions, so they answer false.
/// (A supported instruction may still have an option a later milestone
/// adds — a collation, a `type` — but the instruction itself runs.)
pub fn element_available(uri: &str, local: &str) -> bool {
    uri == crate::names::XSLT_NS
        && crate::compile::checks::is_instruction(local)
        && !NOT_YET_IMPLEMENTED_INSTRUCTIONS.contains(&local)
}

/// The XSLT 2.0 instructions this processor recognises but does not run yet:
/// each would compile to an `XSLT2-NOTIMPL` instruction
/// (`compile/instructions.rs`). Empty since M3c implemented `xsl:number`
/// (`xsl:result-document` left the list with M3b); `tests/m2h_regex.rs` pins
/// the answer for all 26 instructions.
const NOT_YET_IMPLEMENTED_INSTRUCTIONS: &[&str] = &[];

/// §18.1.1: whether one of the processor's own functions of this name is
/// available — at `arity`, or at any arity when `arity` is `None`.
///
/// > The two-argument version of the `function-available` function returns
/// > true if and only if there is an available function whose name matches the
/// > value of the `$function-name` argument and whose arity matches the value
/// > of the `$arity` argument. The single-argument version of the
/// > `function-available` function returns true if and only if there is at
/// > least one available function (with some arity) whose name matches the
/// > value of the `$function-name` argument.
///
/// Stylesheet functions are not the processor's: the run-time evaluator asks
/// the compiled stylesheet about those as well.
pub fn function_available(
    functions: &XsltFunctions,
    uri: &str,
    local: &str,
    arity: Option<usize>,
) -> bool {
    match arity {
        Some(arity) => functions.is_available(uri, local, arity),
        None => (0..=MAX_ARITY).any(|arity| functions.is_available(uri, local, arity)),
    }
}

/// The `$arity` argument of `function-available` (§18.1.1): `Ok(None)` for the
/// one-argument form; `Ok(Some(Some(n)))` for an arity a function can have;
/// `Ok(Some(None))` for one no function has (a negative or absurdly large
/// integer), of which the answer is simply false.
///
/// The signature is `function-available($function-name as xs:string, $arity as
/// xs:integer)`, so the argument goes through XPath 2.0 §3.1.5's function
/// conversion rules: "Atomization is applied to the given value", "Each item in
/// the atomic sequence that is of type xs:untypedAtomic is cast to the expected
/// atomic type", numeric promotion (B.1) can only widen — nothing is promoted
/// *to* `xs:integer` — and "If, after the above conversions, the resulting value
/// does not match the expected type according to the rules for SequenceType
/// Matching, a type error is raised \[err:XPTY0004\]": two items, the empty
/// sequence, an `xs:double`, an `xs:decimal` or a string. A failed cast is the
/// cast's own error (FORG0001). In XPath 1.0 compatibility mode the engine has
/// already applied the first of §3.1.5's extra steps ("the value V is
/// effectively replaced by V\[1\]"); the `fn:string` / `fn:number` steps name
/// only `xs:string` and `xs:double`, so an `xs:double` is XPTY0004 there too.
/// The run-time evaluator and the `use-when` evaluator both call this.
pub fn arity_argument<N: xsd_schema::navigator::DomNavigator>(
    value: Option<&xsd_schema::xpath::functions::XPathValue<N>>,
) -> Result<Option<Option<usize>>, crate::error::XsltError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let integer = integer_argument(value, "the $arity argument of function-available()")?;
    Ok(Some(integer.and_then(|n| usize::try_from(n).ok())))
}

/// An argument whose declared type is `xs:integer`, after XPath 2.0 §3.1.5's
/// function conversion rules (see [`arity_argument`] for the rules quoted):
/// the integer, or `Ok(None)` for one outside the `i64` range. `what` names the
/// argument in the XPTY0004 message.
///
/// `function-available($name, $arity)` (§18.1.1) and `regex-group($n)`
/// (§15.2) use it.
pub fn integer_argument<N: xsd_schema::navigator::DomNavigator>(
    value: &xsd_schema::xpath::functions::XPathValue<N>,
    what: &str,
) -> Result<Option<i64>, crate::error::XsltError> {
    use crate::error::XsltError;
    use xsd_schema::types::XmlTypeCode;
    let atoms = xsd_schema::xpath::functions::atomize_sequence(value.clone())?;
    let [atom] = atoms.as_slice() else {
        return Err(XsltError::new(
            "XPTY0004",
            format!(
                "{what} must be a single xs:integer, not a sequence of {} atomic values",
                atoms.len()
            ),
        ));
    };
    let integer = match atom.type_code {
        XmlTypeCode::UntypedAtomic => xsd_schema::xpath::cast::cast_to(atom, XmlTypeCode::Integer)?,
        code if is_integer_type(code) => atom.clone(),
        code => {
            return Err(XsltError::new(
                "XPTY0004",
                format!(
                    "{what} must be an xs:integer, not a value of type xs:{}",
                    code.local_name().unwrap_or("anyAtomicType")
                ),
            ))
        }
    };
    Ok(integer.to_string_value().parse::<i64>().ok())
}

/// `xs:integer` and the built-in types derived from it — the values that match
/// the sequence type `xs:integer` ("SequenceType Matching permit\[s\] a value of
/// a derived type to be substituted for a value of its base type", XPath 2.0
/// §3.1.5).
fn is_integer_type(code: xsd_schema::types::XmlTypeCode) -> bool {
    use xsd_schema::types::XmlTypeCode as T;
    matches!(
        code,
        T::Integer
            | T::NonPositiveInteger
            | T::NegativeInteger
            | T::Long
            | T::Int
            | T::Short
            | T::Byte
            | T::NonNegativeInteger
            | T::UnsignedLong
            | T::UnsignedInt
            | T::UnsignedShort
            | T::UnsignedByte
            | T::PositiveInteger
    )
}

/// Whether `{namespace}local` with `arity` arguments is a constructor function
/// of this processor.
///
/// XPath 2.0 §2.1.1: "The function signatures include the signatures of
/// constructor functions", and §3.10.4: "For every atomic type in the in-scope
/// schema types (except xs:NOTATION and xs:anyAtomicType, which are not
/// instantiable), a constructor function is implicitly defined", with the
/// signature `T($arg as xs:anyAtomicType?) as T?` — arity 1 only. For a basic
/// processor §21.1 limits the set: "The set of constructor functions available
/// are limited to those that construct values of the above atomic types", the
/// atomic types of §3.13 (see [`type_available`]). The XPath binder resolves a
/// constructor call outside the function catalog, so `function-available()`
/// has to be told about them here.
pub fn is_constructor_function(namespace: &str, local: &str, arity: usize) -> bool {
    arity == 1
        && namespace == crate::names::XSD_NS
        && BUILT_IN_TYPES.contains(&local)
        && !matches!(
            local,
            // Not atomic: no constructor at all.
            "anyType" | "anySimpleType" | "untyped"
            // Atomic but "not instantiable".
            | "anyAtomicType"
        )
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
