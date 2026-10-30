//! The compiled stylesheet: the instruction tree, the declaration tables and
//! the expression store every one of them points into.
//!
//! Nothing here borrows the module trees the compiler walked: a
//! [`CompiledStylesheet`] owns its instructions, its [`XPathExpr`]s and its
//! patterns, and borrows only the caller's [`NameTable`] and function set.
//! That is what lets the compiler drop its arena as soon as compilation
//! finishes.

use std::collections::HashMap;

use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use crate::error::{Location, XsltError};
use crate::functions::XsltFunctions;
use crate::names::QName;
use crate::output::OutputDefinition;
use crate::pattern::Pattern;
use crate::stylesheet::{CompatMode, ModuleId, StaticContextId, StaticContexts};

/// What `[xsl:]validation` / `[xsl:]type` asks of a node constructor (§19.2):
/// the attribute written on it, or else the `default-validation` of its
/// stylesheet module (§3.6), or else `strip`.
///
/// > If both attributes are omitted, the effect is the same as specifying the
/// > validation attribute with the value specified in the default-validation
/// > attribute of the containing xsl:stylesheet element; if this is not
/// > specified, the effect is the same as specifying validation="strip".
///
/// The static rules (XTSE1505, XTSE1520, XTSE1530, XTSE0020; XTSE1660 for a
/// basic processor) are the checker's (`compile/checks.rs`). Every value is
/// evaluated on every node constructor (`runtime/validate.rs`); a `type`
/// naming one of the XPath 2.0 types §3.13 puts in scope (`xs:untypedAtomic`,
/// `xs:anyAtomicType`, `xs:dayTimeDuration`, `xs:yearMonthDuration`) is found,
/// in either XML Schema version, by `xsd-schema`'s `SchemaSet::xpath_type`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Validation {
    /// The new node and the nodes it contains are untyped (§19.2.1.1).
    #[default]
    Strip,
    /// The contained nodes keep their annotations.
    Preserve,
    /// Validated against the top-level declaration of the node's name.
    Strict,
    /// Validated against the declaration when there is one.
    Lax,
    /// `[xsl:]type`: validated against this in-scope type (§19.2.1.2), its
    /// QName expanded with the `[xsl:]xpath-default-namespace` for an unprefixed
    /// one.
    Type(QName),
}

/// Index into the expression table of a [`CompiledStylesheet`] (`exprs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

/// Index into the pattern table of a [`CompiledStylesheet`] (`patterns`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PatternId(pub u32);

/// Index into [`CompiledStylesheet::templates`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TemplateId(pub u32);

/// One *template rule* in the sense §6.4 gives the term: an `xsl:template`
/// declaration together with **one** alternative of its match pattern.
///
/// > If the pattern contains multiple alternatives separated by `|`, then the
/// > template rule is treated equivalently to a set of template rules, one for
/// > each alternative.
///
/// Each of those rules has its own default priority and takes part in conflict
/// resolution on its own, while they all share one body — which is why this is
/// a pair rather than a duplicated [`TemplateRule`]. A declaration with no
/// `match` attribute, or with a single-alternative pattern, has exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId {
    pub template: TemplateId,
    /// Index into [`Pattern::alternatives`](crate::pattern::Pattern).
    pub alternative: u32,
}

/// Where a variable reference resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarLocation {
    /// A global `xsl:variable`/`xsl:param`, by index into
    /// [`CompiledStylesheet::globals`].
    Global(usize),
    /// A slot of the innermost frame.
    Local(usize),
}

/// One compiled XPath expression plus the variables it was compiled against.
///
/// `vars[i]` is where external slot `i` comes from, so the evaluator can bind
/// only the slots the expression actually references
/// (`XPathExpr::references_external_var`).
pub struct CompiledExpr {
    pub expr: XPathExpr,
    pub ctx: StaticContextId,
    pub vars: Vec<VarLocation>,
    /// A static error of the expression's source that §5.3's
    /// forwards-compatible exception defers to the time the expression is
    /// evaluated: `Transformation::eval_expr` returns it, as the processor's
    /// error with its own code, instead of evaluating `expr` (the empty
    /// sequence then). Kept here, not in the expression, so that nothing a
    /// stylesheet can write — an `fn:error` QName included — can produce such
    /// an error.
    pub(crate) deferred: Option<XsltError>,
}

/// One part of an attribute value template (§5.6).
pub enum AvtPart {
    Literal(String),
    Expr(ExprId),
}

/// An attribute value template (§5.6).
///
/// > \[Definition: An attribute value template is an attribute whose value
/// > may contain XPath expressions enclosed in curly brackets. …\]
pub struct Avt {
    pub parts: Vec<AvtPart>,
    /// Backwards-compatible behaviour is enabled for the attribute (§9.10
    /// row 6), so each enclosed expression contributes its first atomized item
    /// only:
    ///
    /// > \[§5.6\] If backwards compatible behavior is enabled for the attribute,
    /// > the rules for converting the value of the expression to a string are
    /// > modified as follows. After atomizing the result of the expression, all
    /// > items other than the first item in the resulting sequence are
    /// > discarded, and the effective value is obtained by converting the first
    /// > item in the sequence to a string. If the atomized sequence is empty,
    /// > the result is a zero-length string.
    ///
    /// Decided once, when the template is compiled, from the static context of
    /// the element the attribute is on (§3.8: an element's `[xsl:]version`
    /// governs "itself, its attributes, its descendants and their attributes").
    pub(crate) first_item_only: bool,
}

impl Avt {
    /// An AVT that is one fixed string.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            parts: vec![AvtPart::Literal(text.into())],
            first_item_only: false,
        }
    }

    /// The fixed value, when the template contains no expression.
    pub fn as_literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [] => Some(""),
            [AvtPart::Literal(text)] => Some(text),
            _ => None,
        }
    }
}

/// A sequence constructor (§5.7).
#[derive(Default)]
pub struct Body(pub Vec<Instruction>);

/// `select` or contained sequence constructor — the two ways an instruction
/// says what its value is.
pub enum ValueSource {
    Select(ExprId),
    Content(Body),
}

/// One *sort key component*: an `xsl:sort` element (§13.1).
///
/// > \[Definition: A sort key specification is a sequence of one or more
/// > adjacent `xsl:sort` elements which together define rules for sorting the
/// > items in an input sequence to form a sorted sequence.\]
///
/// The attributes that are attribute value templates are kept as [`Avt`]s:
/// §13.1 evaluates them "using the same focus as is used to evaluate the
/// `select` attribute of the containing instruction", once per sort, not once
/// per item. A fixed value has already been checked by the compiler (XTSE0020);
/// a computed one is checked when it is evaluated (XTDE0030).
pub struct SortKey {
    /// `select`, or the contained sequence constructor; `select="."` when
    /// there is neither (§13.1: "If neither is present, the default is
    /// select=\".\"").
    pub value: ValueSource,
    pub order: Option<Avt>,
    pub data_type: Option<Avt>,
    pub case_order: Option<Avt>,
    pub lang: Option<Avt>,
    pub collation: Option<Avt>,
    /// Only ever present on the first key (XTSE1017).
    pub stable: Option<Avt>,
    /// The static context of the `xsl:sort` element: its base URI resolves a
    /// relative collation URI (§13.1.3) and its compatibility mode decides the
    /// first-item rule of XTTE1020 (§13.1.2).
    pub ctx: StaticContextId,
    /// Where the `xsl:sort` element is, for its errors.
    pub loc: Location,
}

/// `order` on `xsl:sort` (§13.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    /// The effective value of the attribute: `ascending` or `descending`.
    pub fn parse(value: &str) -> Option<Self> {
        match crate::whitespace::trim(value) {
            "ascending" => Some(SortOrder::Ascending),
            "descending" => Some(SortOrder::Descending),
            _ => None,
        }
    }
}

/// `data-type` on `xsl:sort` (§13.1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDataType {
    /// No `data-type`: the atomized values are compared by the rules of their
    /// own type ("Any values of type `xs:untypedAtomic` are cast to
    /// `xs:string`").
    Typed,
    /// `text`: converted with `string()`.
    Text,
    /// `number`: converted with `number()`.
    Number,
}

impl SortDataType {
    /// The effective value of the attribute.
    ///
    /// > If the `data-type` attribute has any other effective value, then the
    /// > value must be a lexical QName with a non-empty prefix, and the effect
    /// > of the attribute is implementation-defined.
    ///
    /// This processor defines no such data type: a prefixed QName is accepted
    /// and the key is compared as if there were no `data-type` attribute.
    /// Anything else is `None` (XTSE0020 for a fixed value, XTDE0030 for a
    /// computed one). The prefix must still be declared — see
    /// [`qname_prefix`](Self::qname_prefix).
    pub fn parse(value: &str) -> Option<Self> {
        match crate::whitespace::trim(value) {
            "text" => Some(SortDataType::Text),
            "number" => Some(SortDataType::Number),
            _ if Self::qname_prefix(value).is_some() => Some(SortDataType::Typed),
            _ => None,
        }
    }

    /// The prefix of a `data-type` value that is a prefixed QName, which the
    /// defining element must bind.
    ///
    /// > \[§5.1\] \[ERR XTSE0280\] In the case of a prefixed QName used as the
    /// > value of an attribute in the stylesheet, or appearing within an XPath
    /// > expression in the stylesheet, it is a static error if the defining
    /// > element has no namespace node whose name matches the prefix of the
    /// > QName.
    ///
    /// > \[ERR XTDE0290\] Where the result of evaluating an XPath expression
    /// > (or an attribute value template) is required to be a lexical QName,
    /// > then unless otherwise specified it is a non-recoverable dynamic error
    /// > if the defining element has no namespace node whose name matches the
    /// > prefix of the lexical QName.
    pub fn qname_prefix(value: &str) -> Option<&str> {
        let (prefix, local) = crate::whitespace::trim(value).split_once(':')?;
        (crate::names::is_ncname(prefix) && crate::names::is_ncname(local)).then_some(prefix)
    }
}

/// `case-order` on `xsl:sort` (§13.1.3): `upper-first` or `lower-first`.
pub fn parse_case_order(value: &str) -> Option<crate::collation::CaseOrder> {
    match crate::whitespace::trim(value) {
        "upper-first" => Some(crate::collation::CaseOrder::UpperFirst),
        "lower-first" => Some(crate::collation::CaseOrder::LowerFirst),
        _ => None,
    }
}

/// Whether `value` is valid for `lang` on `xsl:sort`.
///
/// > \[§13.1.3\] The effective value of the attribute must be a value that
/// > would be valid for the `xml:lang` attribute (see \[XML 1.0\]).
///
/// XML 1.0 takes `xml:lang` values from BCP 47 and allows the empty string
/// ("no language information"); the syntax checked is that of `xs:language`,
/// `[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`, or empty.
pub fn is_valid_lang(value: &str) -> bool {
    let value = crate::whitespace::trim(value);
    if value.is_empty() {
        return true;
    }
    value.split('-').enumerate().all(|(index, part)| {
        (1..=8).contains(&part.len())
            && part.bytes().all(|b| {
                if index == 0 {
                    b.is_ascii_alphabetic()
                } else {
                    b.is_ascii_alphanumeric()
                }
            })
    })
}

/// How `xsl:for-each-group` allocates its population to groups: exactly one of
/// its four grouping attributes (§14.3, XTSE1080).
pub enum Grouping {
    /// `group-by`: a group per distinct grouping key; an item may be in
    /// several groups, or in none.
    By(ExprId),
    /// `group-adjacent`: runs of adjacent items with equal keys; each item has
    /// exactly one key (XTTE1100).
    Adjacent(ExprId),
    /// `group-starting-with`: a new group at each node that matches.
    StartingWith(PatternId),
    /// `group-ending-with`: a new group after each node that matches.
    EndingWith(PatternId),
}

/// Where the regular expression of an `xsl:analyze-string` comes from
/// (§15.1).
///
/// A compiled regular expression is opaque: it tells the effective `regex`
/// and `flags` it was compiled from, and nothing of the engine behind it
/// (`regexml`, which is not part of this crate's API). The unit test
/// `model::tests::a_fixed_regex_source_tells_its_effective_regex_and_flags`
/// reads them.
///
/// Neither this type nor the engine is reachable from outside the crate:
///
/// ```compile_fail
/// // The instruction tree, and the regular-expression engine behind it, are
/// // not part of the public API.
/// use xslt2::model::RegexSource;
/// ```
pub enum RegexSource {
    /// Neither `regex` nor `flags` contains an expression: the regular
    /// expression was compiled once, by the compiler.
    Fixed(std::sync::Arc<crate::regex::CompiledRegex>),
    /// `regex` or `flags` is computed: both are evaluated, and the regular
    /// expression compiled (or taken from the run's cache), each time the
    /// instruction is evaluated. An absent `flags` is the zero-length string
    /// (§15.1: "If the attribute is omitted, the effect is the same as
    /// supplying a zero-length string").
    Computed { regex: Avt, flags: Option<Avt> },
}

/// The value of a variable-binding element (§9.2).
pub enum VarValue {
    /// `select="…"` — the value is the expression's result.
    Select(ExprId),
    /// A sequence constructor with no `as`: a temporary tree (§9.4).
    ///
    /// `ctx` is the static context of the variable-binding element itself,
    /// because §9.4 takes the new document node's base URI from there: "The
    /// base URI of the document node is taken from the base URI of the variable
    /// binding element in the stylesheet."
    Tree { body: Body, ctx: StaticContextId },
    /// A sequence constructor with `as`: a sequence (§9.3).
    Sequence(Body),
    /// Neither: the empty sequence, or `""` when there is no `as`.
    Empty,
}

/// A required type (`as="…"`), with the expressions that check and convert a
/// value against it (§9.3, XPath's function conversion rules).
pub struct TypeCheck {
    /// The `as` attribute as written, for error messages.
    pub text: String,
    /// `$value instance of <text>`.
    pub test: ExprId,
    /// `$value cast as <item type>`, when the item type is an atomic type and
    /// the conversion rules may need it.
    pub cast: Option<ExprId>,
    /// The expected item type is an atomic type: the value is atomized first.
    pub atomic: bool,
    /// The item type, without its occurrence indicator, as written.
    #[allow(
        dead_code,
        reason = "kept with the compiled check: the type as written"
    )]
    pub item_type: String,
    /// The built-in atomic type the item type names, resolved in the static
    /// context of the `as` attribute — so `xsd:string`, and `double` under an
    /// `xpath-default-namespace` of the XML Schema namespace, are recognised
    /// as what they are. `None` for anything else.
    pub target: Option<xsd_schema::types::XmlTypeCode>,
}

/// `xsl:with-param` (§10.1.1).
pub struct WithParam {
    pub name: QName,
    pub value: VarValue,
    pub as_type: Option<TypeCheck>,
    pub tunnel: bool,
    pub loc: Location,
}

/// `xsl:param` inside a template (§10.1.1).
pub struct ParamDecl {
    pub name: QName,
    pub slot: usize,
    pub value: VarValue,
    pub as_type: Option<TypeCheck>,
    pub required: bool,
    pub tunnel: bool,
    pub loc: Location,
}

/// Which mode an `xsl:apply-templates` applies, or an `xsl:template` serves.
///
/// New variants may be added in a minor release (`#[non_exhaustive]`): a
/// `match` on it outside this crate needs a wildcard arm.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ModeKey {
    /// The unnamed mode.
    Default,
    /// `mode="name"`.
    Named(QName),
}

/// `mode` on `xsl:apply-templates` (§6.3).
pub enum ModeRef {
    /// `#default`, or no attribute at all.
    Default,
    /// `#current`.
    Current,
    /// A named mode.
    Named(QName),
}

/// A namespace for a constructed node: absent, or an AVT.
pub type NamespaceAvt = Option<Avt>;

/// One instruction of a sequence constructor, with where it was written.
pub struct Instruction {
    pub kind: InstructionKind,
    pub ctx: StaticContextId,
    pub module: ModuleId,
    pub line: u32,
    pub column: u32,
}

impl Instruction {
    /// The location this instruction reports errors at.
    pub fn location(&self, sheet: &CompiledStylesheet<'_>) -> Location {
        Location {
            module_uri: sheet.module_uri(self.module).map(str::to_string),
            line: self.line,
            column: self.column,
        }
    }
}

/// The XSLT instructions.
///
/// A construct the processor recognizes but does not implement raises
/// `XSLT2-NOTIMPL` where it is reached, never a panic.
pub enum InstructionKind {
    ApplyTemplates {
        select: Option<ExprId>,
        mode: ModeRef,
        params: Vec<WithParam>,
        /// The sort key specification (§13); empty when there is none.
        sort: Vec<SortKey>,
    },
    ApplyImports {
        params: Vec<WithParam>,
    },
    NextMatch {
        params: Vec<WithParam>,
    },
    CallTemplate {
        name: QName,
        params: Vec<WithParam>,
    },
    ForEach {
        select: ExprId,
        /// The sort key specification (§13); empty when there is none.
        sort: Vec<SortKey>,
        body: Body,
    },
    /// `xsl:for-each-group` (§14.3).
    ForEachGroup {
        /// The population: "the sequence of items to be grouped".
        select: ExprId,
        grouping: Grouping,
        /// `collation` (an attribute value template), allowed only with
        /// `group-by` or `group-adjacent` (XTSE1090). Without it, the default
        /// collation of the instruction's static context applies (§14.3: "If
        /// there is no collation attribute then the default collation is
        /// used").
        collation: Option<Avt>,
        /// The sort key specification that orders the groups (§14.3); empty
        /// for the order of first appearance.
        sort: Vec<SortKey>,
        body: Body,
    },
    /// `xsl:perform-sort` (§13.2): the initial sequence is `select`, or the
    /// sequence constructor, or empty when there is neither.
    PerformSort {
        select: Option<ExprId>,
        sort: Vec<SortKey>,
        body: Body,
    },
    If {
        test: ExprId,
        then: Body,
    },
    Choose {
        whens: Vec<(ExprId, Body)>,
        otherwise: Option<Body>,
    },
    ValueOf {
        value: ValueSource,
        separator: Option<Avt>,
        /// `disable-output-escaping` (§20.2), honoured by the serializer.
        disable_output_escaping: bool,
        /// Backwards-compatible behaviour: only the first item (§9.10 row 5).
        first_item_only: bool,
    },
    Text {
        text: String,
        disable_output_escaping: bool,
    },
    Sequence {
        select: Option<ExprId>,
    },
    LiteralElement {
        name: crate::receiver::Name,
        /// Namespace nodes the literal result element copies (§11.1.2), after
        /// `exclude-result-prefixes`.
        namespaces: Vec<(String, String)>,
        attrs: Vec<(crate::receiver::Name, Avt)>,
        attr_sets: Vec<QName>,
        /// §11.1's `[xsl:]inherit-namespaces`: whether this element's namespace
        /// nodes reach its descendants (§5.7.1's last rule).
        inherit_namespaces: bool,
        /// `xsl:validation` / `xsl:type` (§11.1.1).
        validation: Validation,
        body: Body,
    },
    Element {
        name: Avt,
        namespace: NamespaceAvt,
        attr_sets: Vec<QName>,
        inherit_namespaces: bool,
        validation: Validation,
        body: Body,
    },
    Attribute {
        name: Avt,
        namespace: NamespaceAvt,
        separator: Option<Avt>,
        validation: Validation,
        value: ValueSource,
    },
    Namespace {
        name: Avt,
        value: ValueSource,
    },
    Comment {
        value: ValueSource,
    },
    ProcessingInstruction {
        name: Avt,
        value: ValueSource,
    },
    Document {
        validation: Validation,
        body: Body,
    },
    Copy {
        copy_namespaces: bool,
        inherit_namespaces: bool,
        attr_sets: Vec<QName>,
        validation: Validation,
        body: Body,
    },
    CopyOf {
        select: ExprId,
        copy_namespaces: bool,
        validation: Validation,
    },
    Variable {
        slot: usize,
        value: VarValue,
        as_type: Option<TypeCheck>,
        #[allow(
            dead_code,
            reason = "kept with the compiled instruction: the variable's name"
        )]
        name: QName,
    },
    /// §17: "The content of the message may be specified by using either or
    /// both of the optional select attribute and the sequence constructor …
    /// The effect is then the same as if a single xsl:copy-of instruction with
    /// this select attribute were added to the start of the sequence
    /// constructor."
    Message {
        select: Option<ExprId>,
        body: Body,
        terminate: Avt,
    },
    /// `xsl:analyze-string` (§15.1).
    ///
    /// > The xsl:analyze-string instruction takes as input a string (the
    /// > result of evaluating the expression in the select attribute) and a
    /// > regular expression (the effective value of the regex attribute).
    ///
    /// An absent `xsl:matching-substring` or `xsl:non-matching-substring` is
    /// `None`: "If the element is absent, the effect is the same as if it were
    /// present with empty content." The `xsl:fallback` children are not kept:
    /// they "are ignored by an XSLT 2.0 processor".
    AnalyzeString {
        select: ExprId,
        regex: RegexSource,
        matching: Option<Body>,
        non_matching: Option<Body>,
    },
    /// `xsl:result-document` (§19.1).
    ResultDocument(Box<ResultDocument>),
    /// `xsl:number` (§12).
    Number(Box<NumberInstruction>),
    /// `xsl:fallback` outside an unknown extension instruction: ignored
    /// (§18.2.3 "When not performing fallback, evaluating an xsl:fallback
    /// element returns an empty sequence").
    Fallback,
    /// An extension instruction (§18.2): an element in a sequence constructor
    /// whose namespace an `[xsl:]extension-element-prefixes` in scope
    /// designates (§18.2.1). This processor implements no extension
    /// instruction, so evaluating one always performs fallback:
    ///
    /// > \[§18.2.3\] \[ERR XTDE1450\] When a processor performs fallback for an
    /// > extension instruction that is not recognized, if the instruction
    /// > element has one or more `xsl:fallback` children, then the content of
    /// > each of the `xsl:fallback` children must be evaluated; it is a
    /// > non-recoverable dynamic error if it has no `xsl:fallback` children.
    ///
    /// and never at compile time — §18.2.2: "An implementation must not signal
    /// an error merely because the stylesheet contains an extension
    /// instruction for which no implementation is available."
    ExtensionInstruction {
        /// The element's expanded name.
        name: QName,
        /// The element's name as written, for the XTDE1450 message.
        lexical: String,
        /// The contents of every `xsl:fallback` child, in document order;
        /// `None` when there is none (XTDE1450 when evaluated). The siblings
        /// of the `xsl:fallback` children are not compiled: their meaning
        /// belongs to the extension.
        fallback: Option<Body>,
    },
}

/// The `format` attribute of `xsl:result-document` (§19.1): which output
/// definition controls the serialization of the tree.
pub enum ResultFormat {
    /// Written without an expression: expanded and checked against the named
    /// output definitions when the stylesheet is compiled, so an unknown name
    /// is XTDE1460 signalled statically ("If the processor is able to detect
    /// the error statically (for example, when the format attribute contains
    /// no curly brackets), then the processor may optionally signal this as a
    /// static error").
    Named(QName),
    /// An attribute value template, expanded and checked when the instruction
    /// is evaluated.
    Computed(Avt),
}

/// `xsl:result-document` (§19.1).
///
/// > The `xsl:result-document` instruction is used to create a final result
/// > tree. … A document node is created, and the sequence obtained by
/// > evaluating the sequence constructor is used to construct the content of
/// > the document, as described in 5.7.1 Constructing Complex Content.
///
/// `validation` and `type`: see [`Validation`] — a basic processor accepts
/// only `validation="strip"`, which is what an untyped tree is already
/// (XTSE1660 otherwise).
pub struct ResultDocument {
    /// `None` selects the unnamed output definition: "If the format attribute
    /// is omitted, the unnamed output definition is used".
    pub format: Option<ResultFormat>,
    /// `None` is the default, "the zero-length string".
    pub href: Option<Avt>,
    /// The serialization attributes written on the instruction, each under the
    /// name `xsl:output` gives it (`output-version` is `version`), in a fixed
    /// order. A value with no expression was checked when the stylesheet was
    /// compiled (XTSE0020).
    pub serialization: Vec<(&'static str, Avt)>,
    /// `use-character-maps` (not an attribute value template): "the character
    /// maps referenced in this instruction supplement and take precedence over
    /// those defined in the selected output definition".
    pub use_character_maps: Vec<QName>,
    pub validation: Validation,
    pub body: Body,
}

/// `xsl:number` (§12).
///
/// > The `xsl:number` instruction performs two tasks: firstly, determining a
/// > place marker (this is a sequence of integers, to allow for hierarchic
/// > numbering schemes such as 1.12.2 or 3(c)ii), and secondly, formatting the
/// > place marker for output as a text node in the result sequence.
///
/// The six conversion attributes of §12.3 are attribute value templates; a
/// value written without an expression was checked when the stylesheet was
/// compiled (XTSE0020), a computed one is checked when the instruction is
/// evaluated (XTDE0030).
pub struct NumberInstruction {
    pub source: NumberSource,
    /// `format`; `None` when absent (the default `1`).
    pub(crate) format: Option<NumberFormatSource>,
    pub lang: Option<Avt>,
    pub letter_value: Option<Avt>,
    pub ordinal: Option<Avt>,
    pub grouping_separator: Option<Avt>,
    pub grouping_size: Option<Avt>,
    /// Backwards-compatible behaviour is enabled for the instruction (§12.1,
    /// §3.8): only the first item of `value`, an empty `value`
    /// is `NaN`, and a value that cannot be converted to an integer is
    /// formatted as `NaN` instead of raising XTDE0980.
    pub backwards_compatible: bool,
    /// The instruction's slot in the run's numbering memo (`runtime/number.rs`),
    /// one per `xsl:number` of the stylesheet.
    pub memo_slot: usize,
}

/// Where the place marker of `xsl:number` comes from (§12).
pub enum NumberSource {
    /// `value` (§12.1): "The place marker to be formatted may be specified by
    /// an expression."
    Value(ExprId),
    /// §12.2: "the position of a selected node within the tree that contains
    /// it". `select` absent: the context node.
    Node {
        select: Option<ExprId>,
        level: NumberLevel,
        /// `None`: "the pattern that matches any node with the same node kind
        /// as the selected node and, if the selected node has an expanded-QName,
        /// with the same expanded-QName as the selected node".
        count: Option<PatternId>,
        /// `None`: only "the root node of a tree" matches.
        from: Option<PatternId>,
    },
}

/// The `level` attribute of `xsl:number` (§12.2; default `single`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberLevel {
    Single,
    Multiple,
    Any,
}

/// The `format` attribute of `xsl:number`.
pub(crate) enum NumberFormatSource {
    /// Written without an expression: split into tokens once, by the compiler.
    Fixed(crate::numbering::NumberFormat),
    /// An attribute value template, expanded and split at every evaluation.
    Computed(Avt),
}

/// One template rule or named template (§6, §10.1).
pub struct TemplateRule {
    pub id: TemplateId,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: where and what it is"
    )]
    pub name: Option<QName>,
    pub pattern: Option<PatternId>,
    /// The modes this rule belongs to; empty for a named template with no
    /// `match`.
    pub modes: Vec<ModeKey>,
    /// `mode="#all"` (§6.5).
    pub all_modes: bool,
    /// The `priority` attribute, when one was written. §6.4 gives it to the
    /// rule as a whole, so it applies to **every** alternative of the pattern;
    /// without it each alternative takes the default priority computed from
    /// its own syntax.
    pub explicit_priority: Option<f64>,
    pub precedence: u32,
    /// The lowest import precedence among the modules this rule's module
    /// imports, transitively: `xsl:apply-imports` considers exactly the rules
    /// whose precedence lies in `[subtree_min, precedence)` (§6.7).
    pub subtree_min: u32,
    pub decl_order: u32,
    pub params: Vec<ParamDecl>,
    pub body: Body,
    pub as_type: Option<TypeCheck>,
    pub frame_size: usize,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: where and what it is"
    )]
    pub module: ModuleId,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: where and what it is"
    )]
    pub loc: Location,
}

/// Index into the stylesheet-function table of a [`CompiledStylesheet`]
/// (`functions_table`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StylesheetFunctionId(pub u32);

/// One `xsl:function` declaration that won for its expanded QName and arity
/// (§10.3) — the one the XPath function calls of the stylesheet are bound to.
pub struct StylesheetFunction {
    pub name: QName,
    /// The `xsl:param` children, in order: §10.3 "These are interpreted
    /// positionally."
    pub params: Vec<FunctionParam>,
    /// The `as` attribute: the result is converted to it, and XTTE0780 when it
    /// cannot be.
    pub as_type: Option<TypeCheck>,
    pub body: Body,
    pub frame_size: usize,
    /// `override` (§10.3; default `yes`).
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: its attributes as resolved"
    )]
    pub override_vendor: bool,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: its attributes as resolved"
    )]
    pub precedence: u32,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: its attributes as resolved"
    )]
    pub module: ModuleId,
    pub loc: Location,
}

impl StylesheetFunction {
    /// The function's arity: "the number of `xsl:param` elements in the
    /// function definition" (§10.3).
    pub fn arity(&self) -> usize {
        self.params.len()
    }
}

/// An `xsl:param` of a stylesheet function (§10.3): always mandatory, never a
/// tunnel parameter, and never with a default value (XTSE0760).
pub struct FunctionParam {
    pub name: QName,
    pub slot: usize,
    /// The required type; XTTE0790 when an argument cannot be converted to it.
    pub as_type: Option<TypeCheck>,
    pub loc: Location,
}

/// Index into the key table of a [`CompiledStylesheet`] (`keys`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyId(pub u32);

/// A *key* (§16.3.1): every `xsl:key` declaration of one name.
///
/// > \[Definition: A key is defined as a set of `xsl:key` declarations in the
/// > stylesheet that share the same name.\]
///
/// > An `xsl:key` declaration with higher import precedence does not override
/// > another of lower import precedence; all the `xsl:key` declarations in the
/// > stylesheet are effective regardless of their import precedence.
pub struct Key {
    pub name: QName,
    /// Every declaration of the name, in declaration order.
    pub definitions: Vec<KeyDefinition>,
    /// The effective collation, absolute; the same for every declaration
    /// (XTSE1220).
    pub collation: String,
    /// §16.3.2: "A key … is processed in backwards compatible mode if any of
    /// the `xsl:key` elements in the definition of the key enables backwards
    /// compatible behavior" — then every value is compared as a string.
    pub compat: bool,
    /// Whether the key specifier of some declaration may observe the current
    /// group or the current grouping key (§14.1, §14.2) of the `key()` call it
    /// is evaluated for: one declaration's [`GroupReach::reads_group`], or a
    /// [`GroupReach::calls_key`] while some key of the stylesheet reads the
    /// group. Settled when the whole stylesheet has been compiled. Such a key
    /// has an index per document **and current group**; any other key, one
    /// per document for the run.
    pub observes_group: bool,
    /// Whether the key specifier of some declaration may observe the current
    /// captured substrings (§15.2) of the `key()` call it is evaluated for:
    /// one declaration's [`GroupReach::reads_captures`], or a
    /// [`GroupReach::calls_key`] while some key of the stylesheet reads them.
    /// Settled with [`observes_group`](Self::observes_group). Such a key has
    /// an index per document and `xsl:matching-substring` activation.
    pub observes_captures: bool,
}

/// One `xsl:key` declaration (§16.3.1).
pub struct KeyDefinition {
    /// The `match` pattern, compiled with only the global variables in scope.
    pub pattern: PatternId,
    /// The *key specifier*: the `use` attribute or the sequence constructor.
    pub specifier: ValueSource,
    /// Slots the sequence constructor's local variables need.
    pub frame_size: usize,
    /// What the key specifier may observe of the current group of the `key()`
    /// call it is evaluated for.
    pub group_reach: GroupReach,
    pub loc: Location,
}

/// What a key specifier may observe of the current group and the current
/// grouping key (§14.1, §14.2), and of the current captured substrings
/// (§15.2), of the `key()` call it is evaluated for, as far as the compiler
/// can tell — conservatively: `true` means "may".
///
/// §16.3.1 changes only the focus ("The expression will be evaluated with the
/// node that matches the pattern as the context node"), and §5.4.4 clears the
/// current group and grouping key only for "calls on stylesheet functions", so
/// the specifier evaluated for a call made inside an `xsl:for-each-group`
/// sees that group. §5.4.4's table clears the current captured substrings for
/// "xsl:non-matching-substring; calls on stylesheet functions" and §15.2
/// empties them for "a pattern or a stylesheet function", so the same holds
/// for them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupReach {
    /// The specifier calls `current-group()` or `current-grouping-key()`, or
    /// its sequence constructor invokes a template (`xsl:call-template`,
    /// `xsl:apply-templates`, `xsl:apply-imports`, `xsl:next-match`) or
    /// expands an attribute set — both run with the caller's current group.
    /// A call to a stylesheet function does not count: its body has none.
    pub reads_group: bool,
    /// The same for the current captured substrings (§15.2): the specifier
    /// calls `regex-group()`, or its sequence constructor invokes a template
    /// or expands an attribute set — §15.2: "The value of the current
    /// captured substrings is unaffected through calls of
    /// xsl:apply-templates, xsl:call-template, xsl:apply-imports or
    /// xsl:next-match, or by expansion of named attribute sets."
    pub reads_captures: bool,
    /// The specifier calls `key()`, whose own specifier is then evaluated with
    /// the same current group and captured substrings.
    pub calls_key: bool,
}

/// A global `xsl:variable` or `xsl:param` (§9.5).
pub struct GlobalVar {
    pub name: QName,
    pub is_param: bool,
    pub required: bool,
    pub value: VarValue,
    pub as_type: Option<TypeCheck>,
    #[allow(
        dead_code,
        reason = "kept with the compiled declaration: its import precedence"
    )]
    pub precedence: u32,
    pub frame_size: usize,
    pub loc: Location,
}

/// An `xsl:attribute-set` (§10.2): every declaration of one name.
///
/// The declarations are kept apart, not merged, because §10.2 expands each
/// one in turn — its own `use-attribute-sets` first, then its own
/// `xsl:attribute` instructions:
///
/// > Each xsl:attribute-set declaration whose name matches is processed as
/// > follows. Where two such declarations have different import precedence,
/// > the one with lower import precedence is processed first. Where two
/// > declarations have the same import precedence, they are processed in
/// > declaration order. If the xsl:attribute-set declaration has a
/// > use-attribute-sets attribute, the attribute is expanded by applying this
/// > algorithm recursively. If the xsl:attribute-set declaration contains one
/// > or more xsl:attribute instructions, these instructions are evaluated …
/// > These attribute nodes are appended to the result sequence.
pub struct AttributeSet {
    #[allow(dead_code, reason = "kept with the compiled declaration: its name")]
    pub name: QName,
    /// In the order §10.2 processes them: ascending import precedence, then
    /// declaration order.
    pub declarations: Vec<AttributeSetDecl>,
}

/// One `xsl:attribute-set` declaration.
pub struct AttributeSetDecl {
    pub precedence: u32,
    pub use_attribute_sets: Vec<QName>,
    /// Its `xsl:attribute` instructions.
    pub attributes: Vec<Instruction>,
}

/// `xsl:strip-space` / `xsl:preserve-space` (§4.4).
pub struct SpaceRule {
    /// The name test, as one of the forms §4.4 allows.
    pub test: SpaceTest,
    pub strip: bool,
    pub precedence: u32,
    pub priority: f64,
    pub decl_order: u32,
}

/// The name tests `xsl:strip-space`/`xsl:preserve-space` accept.
pub enum SpaceTest {
    /// `*`
    Any,
    /// `prefix:*`
    Namespace(String),
    /// `*:local`
    LocalName(String),
    /// `prefix:local` or `local`
    Name(QName),
}

impl SpaceTest {
    /// Whether this test matches an element name.
    pub fn matches(&self, uri: &str, local: &str) -> bool {
        match self {
            SpaceTest::Any => true,
            SpaceTest::Namespace(ns) => ns == uri,
            SpaceTest::LocalName(name) => name == local,
            SpaceTest::Name(q) => q.uri == uri && q.local == local,
        }
    }

    /// The default priority §4.4 gives this form, reused for conflict
    /// resolution between strip and preserve rules.
    pub fn priority(&self) -> f64 {
        match self {
            SpaceTest::Any => -0.5,
            SpaceTest::Namespace(_) | SpaceTest::LocalName(_) => -0.25,
            SpaceTest::Name(_) => 0.0,
        }
    }
}

/// The per-mode candidate index (§8.5).
///
/// The buckets hold [`RuleId`]s, not [`TemplateId`]s: a pattern alternative is
/// what has an index key and a default priority (§6.4), so a declaration with
/// `match="a|@b"` appears in two buckets as two rules.
#[derive(Default)]
pub struct ModeIndex {
    /// Rules whose last step is a name test on an element.
    pub by_element: HashMap<QName, Vec<RuleId>>,
    /// Rules whose last step is a name test on an attribute.
    pub by_attribute: HashMap<QName, Vec<RuleId>>,
    /// Rules whose last step is `prefix:*`.
    pub by_namespace: HashMap<String, Vec<RuleId>>,
    /// Rules whose last step is `*:local`.
    pub by_local: HashMap<String, Vec<RuleId>>,
    /// Everything else: kind tests, wildcards, `/`, predicates on wildcards.
    pub other: Vec<RuleId>,
}

/// Every mode the stylesheet declares a rule for (§6.5).
#[derive(Default)]
pub struct ModeTable {
    pub modes: HashMap<ModeKey, ModeIndex>,
    /// The rules of declarations written `mode="#all"`.
    ///
    /// §6.5: `#all` makes a rule "applicable to all modes (that is, to the
    /// default mode and to every mode that is named in an
    /// `xsl:apply-templates` instruction or `xsl:template` declaration
    /// anywhere in the stylesheet)". They are kept in **one** index that every
    /// mode consults rather than copied into an enumerated list of modes, so
    /// that a mode named only by an `xsl:apply-templates` — or reached through
    /// a chain of `mode="#current"` — cannot be missed.
    pub all_modes: ModeIndex,
}

impl ModeTable {
    pub fn index(&self, mode: &ModeKey) -> Option<&ModeIndex> {
        self.modes.get(mode)
    }

    /// The index of the rules applicable to every mode (`mode="#all"`).
    pub fn all_modes(&self) -> &ModeIndex {
        &self.all_modes
    }
}

/// The decimal formats of a stylesheet (§16.4.1): "All the
/// `xsl:decimal-format` declarations in a stylesheet that share the same name
/// are grouped into a named decimal format; those that have no name are grouped
/// into a single unnamed decimal format", and "If a stylesheet does not contain
/// a declaration of the unnamed decimal format, a declaration equivalent to an
/// `xsl:decimal-format` element with no attributes is implied."
#[derive(Clone, Debug, Default)]
pub(crate) struct DecimalFormats {
    pub(crate) unnamed: crate::format_number::DecimalFormat,
    pub(crate) named: HashMap<QName, crate::format_number::DecimalFormat>,
}

/// The result of compiling a stylesheet: immutable, reusable across runs.
pub struct CompiledStylesheet<'a> {
    pub(crate) names: &'a NameTable,
    pub(crate) functions: &'a XsltFunctions,
    /// One [`XPathContext`] per interned [`StaticContextId`].
    pub(crate) contexts: Vec<XPathContext<'a>>,
    /// What a typed node test of a pattern (§5.5.3: `element(*, T)`,
    /// `schema-element(N)`, …) is matched in: the name table its names are
    /// interned in and, in schema-aware mode, the schema set its types and
    /// declarations come from. A resolved test needs nothing else of a static
    /// context.
    pub(crate) match_context: XPathContext<'a>,
    pub(crate) static_contexts: StaticContexts,
    pub(crate) exprs: Vec<CompiledExpr>,
    pub(crate) patterns: Vec<Pattern>,
    pub(crate) templates: Vec<TemplateRule>,
    pub(crate) named: HashMap<QName, TemplateId>,
    pub(crate) globals: Vec<GlobalVar>,
    pub(crate) modes: ModeTable,
    pub(crate) attribute_sets: HashMap<QName, AttributeSet>,
    /// The keys (§16.3.1), indexed by [`KeyId`].
    pub(crate) keys: Vec<Key>,
    pub(crate) key_index: HashMap<QName, KeyId>,
    pub(crate) output: OutputDefinition,
    pub(crate) named_outputs: HashMap<QName, OutputDefinition>,
    /// Every `xsl:character-map`, expanded (§20.1), by name.
    pub(crate) character_maps: HashMap<QName, crate::serialize::CharacterMap>,
    /// The decimal formats (§16.4.1), each merged attribute by attribute: the
    /// unnamed one (implied when no declaration names none) and the named ones.
    pub(crate) decimal_formats: DecimalFormats,
    /// Whether the principal module's `version` is 1.0, which makes `xml`
    /// rather than `xhtml` the default method of an XHTML result (§20).
    pub(crate) principal_version_1_0: bool,
    pub(crate) space_rules: Vec<SpaceRule>,
    /// The stylesheet functions that won for their name and arity (§10.3).
    pub(crate) functions_table: Vec<StylesheetFunction>,
    /// The handle an XPath function call to each of them was bound to.
    pub(crate) function_handles:
        HashMap<xsd_schema::xpath::functions::FunctionHandle, StylesheetFunctionId>,
    pub(crate) module_uris: Vec<String>,
    /// The base URI of the principal module.
    pub(crate) base_uri: String,
    /// Compatibility mode of the principal module, for the §3.6 warning.
    pub(crate) principal_compat: CompatMode,
    /// In schema-aware mode (§21.2), the schema set of the caller's
    /// `SchemaContext`; `None` for a basic processor.
    pub(crate) schema: Option<&'a xsd_schema::SchemaSet>,
    /// The validation of the implicit final result tree: the principal
    /// module's `default-validation` (§3.6: "It also determines the
    /// validation applied to the implicit final result tree created in the
    /// absence of an xsl:result-document instruction").
    pub(crate) final_validation: Validation,
    /// Some node constructor names `strict`, `lax` or a `type` (§19.2), so a run makes its
    /// validator for the trees it builds; otherwise none is made.
    pub(crate) validates: bool,
    /// §4.3: some module says `input-type-annotations="strip"`, so the annotations of every source
    /// tree are stripped.
    pub(crate) strip_input_annotations: bool,
    /// The resolver the stylesheet's modules were read through (`Compiler::with_resolver`), which
    /// every transformation of it reads documents through unless it is given another
    /// (`Transformation::with_resolver`).
    pub(crate) resolver: std::sync::Arc<dyn crate::resolver::Resolver>,
}

impl std::fmt::Debug for CompiledStylesheet<'_> {
    /// The principal module's base URI and counts of what was compiled; never the instruction
    /// tree or the declaration tables. The resolver is a placeholder.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledStylesheet")
            .field("base_uri", &self.base_uri)
            .field("modules", &self.module_uris.len())
            .field("templates", &self.templates.len())
            .field("stylesheet_functions", &self.functions_table.len())
            .field("global_variables", &self.globals.len())
            .field("schema_aware", &self.schema.is_some())
            .field("resolver", &crate::Opaque("<dyn Resolver>"))
            .finish_non_exhaustive()
    }
}

impl<'a> CompiledStylesheet<'a> {
    /// The name table the stylesheet was compiled with, which every document
    /// of every run of it shares.
    pub fn names(&self) -> &'a NameTable {
        self.names
    }

    /// The XSLT function catalog and evaluator this stylesheet was compiled
    /// against.
    pub fn functions(&self) -> &'a XsltFunctions {
        self.functions
    }

    /// The `xsl:output` definition, merged by import precedence (§20).
    pub fn output(&self) -> &OutputDefinition {
        &self.output
    }

    /// A named `xsl:output` definition.
    pub fn named_output(&self, name: &QName) -> Option<&OutputDefinition> {
        self.named_outputs.get(name)
    }

    /// Whether this stylesheet was compiled by a schema-aware processor
    /// (`Compiler::schema_aware`, §21.2).
    pub fn is_schema_aware(&self) -> bool {
        self.schema.is_some()
    }

    /// Whether the type annotations of source trees are stripped (§4.3: "Stripping of type
    /// annotations takes place if at least one stylesheet module in the stylesheet specifies
    /// input-type-annotations="strip"").
    pub(crate) fn strips_input_annotations(&self) -> bool {
        self.strip_input_annotations
    }

    /// The schema set of the `SchemaContext` a schema-aware stylesheet was
    /// compiled against.
    pub fn schema_set(&self) -> Option<&'a xsd_schema::SchemaSet> {
        self.schema
    }

    /// The in-scope schema types of the stylesheet's expressions (§3.13).
    pub(crate) fn type_scope(&self) -> crate::functions::TypeScope<'a> {
        match self.schema {
            Some(set) => crate::functions::TypeScope::Schema(set),
            None => crate::functions::TypeScope::Basic,
        }
    }

    /// The base URI of the principal module.
    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }

    /// Whether the principal module asks for backwards-compatible behaviour
    /// (§3.6 says a processor "should output a warning").
    pub fn is_backwards_compatible(&self) -> bool {
        self.principal_compat.is_backwards()
    }

    /// The static context an expression, pattern or instruction was compiled
    /// in.
    pub(crate) fn context(&self, id: StaticContextId) -> &XPathContext<'a> {
        &self.contexts[id.0 as usize]
    }

    pub(crate) fn expr(&self, id: ExprId) -> &CompiledExpr {
        &self.exprs[id.0 as usize]
    }

    pub(crate) fn pattern(&self, id: PatternId) -> &Pattern {
        &self.patterns[id.0 as usize]
    }

    pub(crate) fn template(&self, id: TemplateId) -> &TemplateRule {
        &self.templates[id.0 as usize]
    }

    /// The declaration a rule belongs to.
    pub(crate) fn rule(&self, id: RuleId) -> &TemplateRule {
        self.template(id.template)
    }

    /// The priority of one rule (§6.4).
    ///
    /// The `priority` attribute, if written, is the priority of every
    /// alternative; otherwise each alternative takes the default priority its
    /// own syntax gives it.
    pub(crate) fn rule_priority(&self, id: RuleId) -> f64 {
        let rule = self.template(id.template);
        if let Some(priority) = rule.explicit_priority {
            return priority;
        }
        match rule.pattern {
            Some(pattern) => self.pattern(pattern).alternatives[id.alternative as usize].priority,
            // A named template with no `match` never takes part in conflict
            // resolution; 0 is the neutral answer.
            None => 0.0,
        }
    }

    /// The key §6.4 sorts candidate rules by, all parts descending.
    pub(crate) fn rule_rank(&self, id: RuleId) -> (u32, f64, u32) {
        let rule = self.template(id.template);
        (rule.precedence, self.rule_priority(id), rule.decl_order)
    }

    #[cfg(test)]
    pub(crate) fn templates(&self) -> &[TemplateRule] {
        &self.templates
    }

    pub(crate) fn named_template(&self, name: &QName) -> Option<TemplateId> {
        self.named.get(name).copied()
    }

    pub(crate) fn globals(&self) -> &[GlobalVar] {
        &self.globals
    }

    pub(crate) fn modes(&self) -> &ModeTable {
        &self.modes
    }

    pub(crate) fn attribute_set(&self, name: &QName) -> Option<&AttributeSet> {
        self.attribute_sets.get(name)
    }

    pub(crate) fn key(&self, id: KeyId) -> &Key {
        &self.keys[id.0 as usize]
    }

    /// The decimal format `format-number` uses (§16.4): the unnamed one for
    /// `None`, otherwise the one with that expanded name, if declared.
    pub(crate) fn decimal_format(
        &self,
        name: Option<&QName>,
    ) -> Option<&crate::format_number::DecimalFormat> {
        match name {
            None => Some(&self.decimal_formats.unnamed),
            Some(name) => self.decimal_formats.named.get(name),
        }
    }

    /// The key whose declarations carry the expanded name `name`.
    pub(crate) fn key_named(&self, name: &QName) -> Option<KeyId> {
        self.key_index.get(name).copied()
    }

    pub(crate) fn space_rules(&self) -> &[SpaceRule] {
        &self.space_rules
    }

    /// Whether `uri` (an absolute URI; a fragment identifier is ignored) names the resource of one of
    /// this stylesheet's modules — what `document('')` reads (§16.1). The run's default source
    /// validation does not apply to such a document: a stylesheet module is not a source the
    /// host asked to validate.
    pub(crate) fn is_module_uri(&self, uri: &str) -> bool {
        let resource = |u: &str| {
            crate::resolver::resource_identity(
                u.split_once('#').map_or(u, |(document, _)| document),
            )
        };
        let wanted = resource(uri);
        self.module_uris
            .iter()
            .any(|module| resource(module) == wanted)
    }

    pub(crate) fn module_uri(&self, module: ModuleId) -> Option<&str> {
        self.module_uris.get(module.0 as usize).map(String::as_str)
    }

    /// Whether any mode of the stylesheet is named `mode`.
    pub(crate) fn has_mode(&self, mode: &ModeKey) -> bool {
        self.modes.modes.contains_key(mode)
    }

    /// The stylesheet function an XPath function call was bound to, if the
    /// handle is one (§10.3).
    pub(crate) fn stylesheet_function_of(
        &self,
        handle: xsd_schema::xpath::functions::FunctionHandle,
    ) -> Option<StylesheetFunctionId> {
        self.function_handles.get(&handle).copied()
    }

    pub(crate) fn stylesheet_function(&self, id: StylesheetFunctionId) -> &StylesheetFunction {
        &self.functions_table[id.0 as usize]
    }

    /// Every stylesheet function in the static context of the stylesheet's
    /// expressions (§10.3).
    #[cfg(test)]
    pub(crate) fn stylesheet_functions(&self) -> &[StylesheetFunction] {
        &self.functions_table
    }

    /// Whether a stylesheet function named `name` exists — with `arity`
    /// parameters, or with any number when `arity` is `None` (§10.3: "If a
    /// stylesheet function has been defined with a particular expanded-QName,
    /// then a call on `function-available` will return true when called with an
    /// argument that is a lexical QName that expands to this same
    /// expanded-QName").
    pub(crate) fn has_stylesheet_function(&self, name: &QName, arity: Option<usize>) -> bool {
        self.functions_table
            .iter()
            .any(|f| f.name == *name && arity.is_none_or(|a| a == f.arity()))
    }

    /// Whether a `[xsl:]version` less than 2.0 is in force at `ctx`.
    pub(crate) fn is_backwards(&self, ctx: StaticContextId) -> bool {
        self.static_contexts.get(ctx).compat.is_backwards()
    }
}

impl CompiledStylesheet<'_> {
    /// The output definition a run's primary result is serialized with, and
    /// whether the tree is the one created implicitly.
    fn primary_output<'o>(
        &'o self,
        outcome: &'o crate::runtime::Outcome<'_>,
    ) -> (&'o OutputDefinition, bool) {
        match &outcome.primary_output {
            Some(definition) => (definition, false),
            None => (&self.output, true),
        }
    }

    /// Serializes a run's primary result (§20), as characters: the encoding
    /// decides what is escaped and what the XML declaration says, and a byte
    /// order mark is a leading U+FEFF;
    /// [`serialize_primary_to_bytes`](Self::serialize_primary_to_bytes) encodes.
    ///
    /// The tree created implicitly is serialized with the unnamed output
    /// definition (§20: "It is also used when serializing the final result
    /// tree that is created implicitly"); a primary result an
    /// `xsl:result-document` created at the base output URI, with that
    /// instruction's effective output definition (§19.1).
    pub fn serialize_primary(
        &self,
        outcome: &crate::runtime::Outcome<'_>,
    ) -> Result<String, crate::XsltError> {
        let (definition, implicit) = self.primary_output(outcome);
        match outcome.primary {
            Some(doc) => crate::runtime::serialize_result(
                self,
                definition,
                doc,
                Some(&outcome.disabled_escaping),
                implicit,
            ),
            None => Ok(String::new()),
        }
    }

    /// [`Self::serialize_primary`] followed by the encoding phase: the octets
    /// in the output definition's encoding (Serialization §4).
    pub fn serialize_primary_to_bytes(
        &self,
        outcome: &crate::runtime::Outcome<'_>,
    ) -> Result<Vec<u8>, crate::XsltError> {
        let (definition, implicit) = self.primary_output(outcome);
        match outcome.primary {
            Some(doc) => crate::runtime::serialize_result_to_bytes(
                self,
                definition,
                doc,
                Some(&outcome.disabled_escaping),
                implicit,
            ),
            None => Ok(Vec::new()),
        }
    }

    /// The output definition [`Self::serialize_primary`] writes a run's
    /// primary result with — the counterpart of
    /// [`SecondaryResult::output`](crate::runtime::SecondaryResult::output):
    /// for a primary result an `xsl:result-document` created at the base
    /// output URI, that instruction's effective output definition (§19.1); for
    /// the tree created implicitly, the unnamed output definition (§20).
    /// `None` when there is no primary result.
    pub fn primary_output_definition<'o>(
        &'o self,
        outcome: &'o crate::runtime::Outcome<'_>,
    ) -> Option<&'o OutputDefinition> {
        outcome.primary.map(|_| self.primary_output(outcome).0)
    }

    /// The serialization parameters [`Self::serialize_primary`] writes a run's
    /// primary result with: its output definition resolved against the tree,
    /// §20's default method included (an XHTML result that a 1.0 stylesheet
    /// creates implicitly is `xml`). `None` when there is no primary result;
    /// an error is the one `serialize_primary` would report for the
    /// parameters.
    pub fn primary_serialization_params(
        &self,
        outcome: &crate::runtime::Outcome<'_>,
    ) -> Result<Option<crate::serialize::SerializationParams>, crate::XsltError> {
        let (definition, implicit) = self.primary_output(outcome);
        match outcome.primary {
            Some(doc) => crate::serialize::SerializationParams::resolve(
                definition,
                &doc.create_navigator(),
                crate::runtime::tree_context(self, implicit),
            )
            .map(Some),
            None => Ok(None),
        }
    }

    /// Serializes a secondary result (§19.1) with its effective output
    /// definition ([`SecondaryResult::output`](crate::runtime::SecondaryResult::output))
    /// and its `disable-output-escaping` marks, as characters, like
    /// [`Self::serialize_primary`].
    pub fn serialize_secondary(
        &self,
        result: &crate::runtime::SecondaryResult<'_>,
    ) -> Result<String, crate::XsltError> {
        crate::runtime::serialize_result(
            self,
            &result.output,
            result.document,
            Some(&result.disabled_escaping),
            false,
        )
    }

    /// [`Self::serialize_secondary`] followed by the encoding phase: the
    /// octets in the effective output definition's encoding.
    pub fn serialize_secondary_to_bytes(
        &self,
        result: &crate::runtime::SecondaryResult<'_>,
    ) -> Result<Vec<u8>, crate::XsltError> {
        crate::runtime::serialize_result_to_bytes(
            self,
            &result.output,
            result.document,
            Some(&result.disabled_escaping),
            false,
        )
    }

    /// The serialization parameters [`Self::serialize_secondary`] writes a
    /// secondary result with: its effective output definition resolved against
    /// the tree (§20's default method for an explicit `xsl:result-document`).
    pub fn secondary_serialization_params(
        &self,
        result: &crate::runtime::SecondaryResult<'_>,
    ) -> Result<crate::serialize::SerializationParams, crate::XsltError> {
        crate::serialize::SerializationParams::resolve(
            &result.output,
            &result.document.create_navigator(),
            crate::runtime::tree_context(self, false),
        )
    }

    /// A character map by name, expanded (§20.1) — what an
    /// `xsl:result-document/@use-character-maps` would add to its output
    /// definition's map.
    pub(crate) fn character_map(&self, name: &QName) -> Option<&crate::serialize::CharacterMap> {
        self.character_maps.get(name)
    }
}

#[cfg(test)]
mod tests;
