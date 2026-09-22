//! The compiled stylesheet: instruction tree (§7.4), declaration tables (§7.3)
//! and the expression store every one of them points into.
//!
//! Nothing here borrows the module trees the compiler walked: a
//! [`CompiledStylesheet`] owns its instructions, its [`XPathExpr`]s and its
//! patterns, and borrows only the caller's [`NameTable`] and function set
//! (§6.6, M0b Q35). That is what lets the compiler drop its arena as soon as
//! compilation finishes.

use std::collections::HashMap;

use xsd_schema::namespace::NameTable;
use xsd_schema::xpath::{XPathContext, XPathExpr};

use crate::error::Location;
use crate::functions::XsltFunctions;
use crate::names::QName;
use crate::output::OutputDefinition;
use crate::pattern::Pattern;
use crate::stylesheet::{CompatMode, ModuleId, StaticContextId, StaticContexts};

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

impl RuleId {
    /// The rule made of a declaration's first (often only) alternative.
    pub fn first(template: TemplateId) -> Self {
        Self {
            template,
            alternative: 0,
        }
    }
}

/// Index into the attribute-set table of a [`CompiledStylesheet`] (`attribute_sets`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttrSetId(pub u32);

/// Where a variable reference resolves to (§7.5).
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
/// (`XPathExpr::references_external_var`, §7.5).
pub struct CompiledExpr {
    pub expr: XPathExpr,
    pub ctx: StaticContextId,
    pub vars: Vec<VarLocation>,
    /// Set when the expression could not be bound because it calls a function
    /// this processor does not have, in a region where that is only an error
    /// if the expression is evaluated (§3.9 forwards-compatible processing,
    /// §9.10 row 3 backwards-compatible behaviour). Evaluating it raises
    /// XTDE1425 with this message; not evaluating it costs nothing.
    pub unavailable: Option<String>,
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
}

impl Avt {
    /// An AVT that is one fixed string.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            parts: vec![AvtPart::Literal(text.into())],
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

impl Body {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `select` or contained sequence constructor — the two ways an instruction
/// says what its value is.
pub enum ValueSource {
    Select(ExprId),
    Content(Body),
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
    pub item_type: String,
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// The instruction set §7.4 lists, restricted to what M1 evaluates.
///
/// Constructs M1 compiles but does not run raise the internal
/// `XSLT2-NOTIMPL` code at the point they are reached, never a panic; the ones
/// M1 does not even compile are rejected by the compiler with the same code.
pub enum InstructionKind {
    ApplyTemplates {
        select: Option<ExprId>,
        mode: ModeRef,
        params: Vec<WithParam>,
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
        /// `disable-output-escaping` — stored, honoured by the serializer (M3).
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
        body: Body,
    },
    Element {
        name: Avt,
        namespace: NamespaceAvt,
        attr_sets: Vec<QName>,
        inherit_namespaces: bool,
        body: Body,
    },
    Attribute {
        name: Avt,
        namespace: NamespaceAvt,
        separator: Option<Avt>,
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
        body: Body,
    },
    Copy {
        copy_namespaces: bool,
        inherit_namespaces: bool,
        attr_sets: Vec<QName>,
        body: Body,
    },
    CopyOf {
        select: ExprId,
        copy_namespaces: bool,
    },
    Variable {
        slot: usize,
        value: VarValue,
        as_type: Option<TypeCheck>,
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
    /// `xsl:fallback` outside an unknown extension instruction: ignored
    /// (§18.2.3 "the xsl:fallback instruction … is ignored").
    Fallback,
    /// A construct the compiler recognises but this processor does not run yet;
    /// `milestone` is the roadmap sub-milestone that owns it (see
    /// `XsltError::not_implemented`).
    NotImplemented {
        feature: String,
        milestone: &'static str,
    },
}

/// One template rule or named template (§6, §10.1).
pub struct TemplateRule {
    pub id: TemplateId,
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
    /// whose precedence lies in `[subtree_min, precedence)` (§6.7, §7.2).
    pub subtree_min: u32,
    pub decl_order: u32,
    pub params: Vec<ParamDecl>,
    pub body: Body,
    pub as_type: Option<TypeCheck>,
    pub frame_size: usize,
    pub module: ModuleId,
    pub loc: Location,
}

/// A global `xsl:variable` or `xsl:param` (§9.5).
pub struct GlobalVar {
    pub name: QName,
    pub is_param: bool,
    pub required: bool,
    pub value: VarValue,
    pub as_type: Option<TypeCheck>,
    pub precedence: u32,
    pub frame_size: usize,
    pub loc: Location,
}

/// An `xsl:attribute-set` (§10.2), already merged across declarations of the
/// same name.
pub struct AttributeSet {
    pub name: QName,
    pub use_attribute_sets: Vec<QName>,
    /// The `xsl:attribute` instructions of every declaration of this name, in
    /// import-precedence order (lowest first, so the highest wins last).
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

/// The result of compiling a stylesheet: immutable, reusable across runs.
pub struct CompiledStylesheet<'a> {
    pub(crate) names: &'a NameTable,
    pub(crate) functions: &'a XsltFunctions,
    /// One [`XPathContext`] per interned [`StaticContextId`] (§7.2).
    pub(crate) contexts: Vec<XPathContext<'a>>,
    pub(crate) static_contexts: StaticContexts,
    pub(crate) exprs: Vec<CompiledExpr>,
    pub(crate) patterns: Vec<Pattern>,
    pub(crate) templates: Vec<TemplateRule>,
    pub(crate) named: HashMap<QName, TemplateId>,
    pub(crate) globals: Vec<GlobalVar>,
    pub(crate) global_index: HashMap<QName, usize>,
    pub(crate) modes: ModeTable,
    pub(crate) attribute_sets: HashMap<QName, AttributeSet>,
    pub(crate) output: OutputDefinition,
    pub(crate) named_outputs: HashMap<QName, OutputDefinition>,
    pub(crate) space_rules: Vec<SpaceRule>,
    pub(crate) module_uris: Vec<String>,
    /// The base URI of the principal module.
    pub(crate) base_uri: String,
    /// Compatibility mode of the principal module, for the §3.6 warning.
    pub(crate) principal_compat: CompatMode,
}

impl<'a> CompiledStylesheet<'a> {
    /// The name table every document of every run shares (§6.6).
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
    pub fn context(&self, id: StaticContextId) -> &XPathContext<'a> {
        &self.contexts[id.0 as usize]
    }

    pub fn expr(&self, id: ExprId) -> &CompiledExpr {
        &self.exprs[id.0 as usize]
    }

    pub fn pattern(&self, id: PatternId) -> &Pattern {
        &self.patterns[id.0 as usize]
    }

    pub fn template(&self, id: TemplateId) -> &TemplateRule {
        &self.templates[id.0 as usize]
    }

    /// The declaration a rule belongs to.
    pub fn rule(&self, id: RuleId) -> &TemplateRule {
        self.template(id.template)
    }

    /// The priority of one rule (§6.4).
    ///
    /// The `priority` attribute, if written, is the priority of every
    /// alternative; otherwise each alternative takes the default priority its
    /// own syntax gives it.
    pub fn rule_priority(&self, id: RuleId) -> f64 {
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
    pub fn rule_rank(&self, id: RuleId) -> (u32, f64, u32) {
        let rule = self.template(id.template);
        (rule.precedence, self.rule_priority(id), rule.decl_order)
    }

    pub fn templates(&self) -> &[TemplateRule] {
        &self.templates
    }

    pub fn named_template(&self, name: &QName) -> Option<TemplateId> {
        self.named.get(name).copied()
    }

    pub fn globals(&self) -> &[GlobalVar] {
        &self.globals
    }

    pub fn global_index(&self, name: &QName) -> Option<usize> {
        self.global_index.get(name).copied()
    }

    pub fn modes(&self) -> &ModeTable {
        &self.modes
    }

    pub fn attribute_set(&self, name: &QName) -> Option<&AttributeSet> {
        self.attribute_sets.get(name)
    }

    pub fn space_rules(&self) -> &[SpaceRule] {
        &self.space_rules
    }

    pub fn module_uri(&self, module: ModuleId) -> Option<&str> {
        self.module_uris.get(module.0 as usize).map(String::as_str)
    }

    /// Whether any mode of the stylesheet is named `mode`.
    pub fn has_mode(&self, mode: &ModeKey) -> bool {
        self.modes.modes.contains_key(mode)
    }

    /// Whether a `[xsl:]version` less than 2.0 is in force at `ctx`.
    pub fn is_backwards(&self, ctx: StaticContextId) -> bool {
        self.static_contexts.get(ctx).compat.is_backwards()
    }
}

impl CompiledStylesheet<'_> {
    /// Serializes a run's primary result per the merged `xsl:output` (§20).
    pub fn serialize_primary(
        &self,
        outcome: &crate::runtime::Outcome<'_>,
    ) -> Result<String, crate::XsltError> {
        match outcome.primary {
            Some(doc) => crate::runtime::serialize_result(self, doc),
            None => Ok(String::new()),
        }
    }
}
