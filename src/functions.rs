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
}

impl XsltFn {
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
    set: FunctionSet<RoXmlNavigator<'static>>,
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
            set: FunctionSet::with_builtins(),
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
        let handle = self.set.register(
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
        &self.set
    }

    /// Which XSLT function a handle stands for, if it is one of ours.
    pub fn resolve(&self, handle: FunctionHandle) -> Option<XsltFn> {
        self.by_handle.get(&handle).copied()
    }

    /// Whether a name and arity in the standard function namespace resolves —
    /// what `function-available()` answers (§18.1.2).
    ///
    /// A function registered by [`Self::add_stub`] is in the catalog but is not
    /// *available*: it exists only so that a call to it is classified as a
    /// missing feature rather than as an unknown function.
    pub fn is_available(&self, namespace: &str, local: &str, arity: usize) -> bool {
        match self.set.lookup(namespace, local, arity) {
            Some(handle) => !self.stubs.contains(&handle),
            None => false,
        }
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
    /// `use-when` attribute.
    namespaces: Vec<(String, String)>,
}

impl<'f> StaticEvaluator<'f> {
    pub fn new(functions: &'f XsltFunctions, namespaces: Vec<(String, String)>) -> Self {
        Self {
            functions,
            namespaces,
        }
    }

    fn resolve(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            return Some(String::new());
        }
        if prefix == crate::names::XML_PREFIX {
            return Some(crate::names::XML_NS.to_string());
        }
        self.namespaces
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.clone())
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
        let (prefix, local) = crate::names::split_qname(crate::whitespace::trim(&name));
        match which {
            XsltFn::SystemProperty => {
                let uri = self.resolve(prefix).unwrap_or_default();
                Ok(XPathValue::string(if uri == crate::names::XSLT_NS {
                    system_property(local)
                } else {
                    ""
                }))
            }
            XsltFn::ElementAvailable => {
                let uri = self.resolve(prefix).unwrap_or_default();
                Ok(XPathValue::boolean(element_available(&uri, local)))
            }
            XsltFn::FunctionAvailable => {
                let uri = if prefix.is_empty() {
                    crate::names::FN_NS.to_string()
                } else {
                    self.resolve(prefix).unwrap_or_default()
                };
                Ok(XPathValue::boolean(function_available(
                    self.functions,
                    &uri,
                    local,
                )))
            }
            XsltFn::TypeAvailable => Ok(XPathValue::boolean(false)),
            other => Err(XPathError::internal(format!(
                "{}() cannot be called from a use-when expression",
                other.name()
            ))),
        }
    }
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

/// §18.1.2: whether a function of this name is available at any arity M1
/// knows about.
pub fn function_available(functions: &XsltFunctions, uri: &str, local: &str) -> bool {
    (0..=4).any(|arity| functions.is_available(uri, local, arity))
}

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
