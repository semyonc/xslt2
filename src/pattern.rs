//! Patterns (§5.5) — the grammar, the match algorithm and the default
//! priorities of §6.4.
//!
//! A pattern is written in a subset of the XPath path syntax, so the text is
//! first handed to the upstream XPath parser: anything it rejects is not a
//! pattern either (XTSE0340). The structure is then read off the text by the
//! scanner below, which rejects everything outside §5.5.2's grammar and hands
//! each predicate back to the compiler as its own expression — the predicates
//! must be separately compiled because §5.5.3 evaluates them against a focus
//! this crate builds itself. XPath comments are removed before the scanner
//! sees the text: they may stand wherever XPath allows whitespace, inside a
//! predicate or anywhere else ([`parse`]).

use std::borrow::Cow;

use xsd_schema::types::ItemType;

use crate::error::{codes, XsltError};
use crate::model::ExprId;
use crate::names::{is_ncname, split_qname, QName};
use crate::whitespace;

/// A compiled pattern: one or more alternatives, each an independent rule for
/// priority purposes (§6.4).
#[derive(Debug)]
pub struct Pattern {
    pub alternatives: Vec<PathPattern>,
    #[allow(
        dead_code,
        reason = "kept with the compiled pattern: its text as written"
    )]
    pub source: String,
}

/// One alternative of a pattern.
#[derive(Debug)]
pub struct PathPattern {
    pub anchor: Anchor,
    /// Steps stored **last first**: matching runs right to left (§5.5.3).
    pub steps: Vec<PatternStep>,
    pub priority: f64,
}

/// What the left end of a path pattern is fixed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// A relative pattern: the leftmost step may match anywhere.
    None,
    /// `/…`: the leftmost step's parent is the root of the tree.
    Root,
    /// `//…`: the leftmost step is a descendant of the root of the tree.
    RootDescendant,
    /// `id(…)` or `key(…, …)`, optionally followed by `/` or `//` and a
    /// RelativePathPattern (§5.5.2's `IdKeyPattern`).
    IdKey(IdKeyAnchor),
}

/// The `IdKeyPattern` a path pattern starts with (§5.5.2):
///
/// > `IdKeyPattern ::= 'id' '(' IdValue ')' | 'key' '(' StringLiteral ',' KeyValue ')'`
/// > `IdValue ::= StringLiteral | VarRef`
/// > `KeyValue ::= Literal | VarRef`
///
/// The call is compiled as the XPath expression it is. §5.5.3 defines the
/// pattern by evaluating `root(.)//(EE)` with a singleton focus on the
/// candidate, and every node of `root(.)//…` is in the candidate's tree, so the
/// call is evaluated with the candidate itself as the context node: `id()` and
/// `key()` with no node argument search the document containing the context
/// node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdKeyAnchor {
    /// The `id(…)` / `key(…)` call.
    pub call: ExprId,
    /// How the leftmost step connects to the nodes the call selects; unused
    /// when the pattern has no steps (it is the call alone).
    pub connector: Connector,
}

/// How a step connects to the step on its left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    /// `/` — the step to the left matches the parent.
    Parent,
    /// `//` — the step to the left matches some ancestor.
    Ancestor,
}

/// The two axes §5.5.2 allows in a pattern, plus the `self` axis §5.5.3
/// substitutes for one particular step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternAxis {
    Child,
    Attribute,
    /// §5.5.3: "If the NodeTest in PS is `document-node()` (optionally with
    /// arguments), and if no explicit axis is specified, then the axis in step
    /// PS is taken as `self` rather than `child`" — where PS is the first step
    /// of a RelativePathPattern. Never written by a user; only that step
    /// carries it.
    SelfAxis,
}

/// One step of a path pattern.
#[derive(Debug)]
pub struct PatternStep {
    pub axis: PatternAxis,
    pub test: NodeTest,
    pub predicates: Vec<ExprId>,
    /// The connector between this step and the one to its left; meaningless
    /// for the leftmost step, where [`PathPattern::anchor`] takes over.
    pub connector: Connector,
}

/// The node tests §5.5.3 allows in a pattern step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeTest {
    /// `a`, `p:a` — an expanded name.
    Name(QName),
    /// `p:*` — any local name in this namespace URI.
    NsWildcard(String),
    /// `*:a` — this local name in any namespace.
    LocalWildcard(String),
    /// `*` — any element (or attribute, on the attribute axis).
    AnyName,
    /// `node()`
    AnyNode,
    /// `text()`
    Text,
    /// `comment()`
    Comment,
    /// `processing-instruction()` / `processing-instruction('x')`
    Pi(Option<String>),
    /// `element()` / `element(a)`
    Element(Option<QName>),
    /// `attribute()` / `attribute(a)`
    Attribute(Option<QName>),
    /// `element(*, T)`, `element(N, T)` and `element(N, T?)` — an `ElementTest`
    /// with a TypeName (XPath 2.0 §2.5.4.3):
    ///
    /// > `element(ElementName, TypeName)` matches an element node whose name is
    /// > ElementName if derives-from(AT, TypeName ) is true, where AT is the
    /// > type annotation of the element node, and the nilled property of the
    /// > node is false.
    ///
    /// and with `TypeName ?` "The nilled property of the node may be either
    /// true or false". `name` is the expanded ElementName (`None` for `*`),
    /// kept for the dispatch index and §6.4's priority; `test` is the same
    /// test as `xsd-schema` resolves it — names interned in the stylesheet's
    /// name table, the type a [`TypeTest`](xsd_schema::types::TypeTest) of
    /// the in-scope schema types — and is what decides a match
    /// (`ItemType::matches_item`), so a pattern and an `instance of` agree on
    /// every node, typed or untyped.
    TypedElement { name: Option<QName>, test: ItemType },
    /// `attribute(*, T)` and `attribute(N, T)` — an `AttributeTest` with a
    /// TypeName (XPath 2.0 §2.5.4.5): "matches an attribute node whose name is
    /// AttributeName if derives-from(AT, TypeName ) is true". As
    /// [`NodeTest::TypedElement`]; an attribute has no nilled property.
    TypedAttribute { name: Option<QName>, test: ItemType },
    /// `schema-element(N)` (XPath 2.0 §2.5.4.4): the candidate's name is `N` or
    /// "the name of an element in a substitution group headed by an element
    /// named ElementName", its annotation derives from the type `N` is
    /// declared with, and it is not nilled unless that declaration is
    /// nillable. `members` are the expanded names of the other global element
    /// declarations in `N`'s substitution group, transitively — the names the
    /// dispatch index files the rule under besides `N`, so that
    /// a name-keyed bucket cannot miss a member; `test` decides the match.
    SchemaElement {
        name: QName,
        members: Vec<QName>,
        test: ItemType,
    },
    /// `schema-attribute(N)` (XPath 2.0 §2.5.4.6): the candidate's name is `N`
    /// and its annotation derives from the type `N` is declared with.
    SchemaAttribute { name: QName, test: ItemType },
    /// `document-node()` / `document-node(element(a))`
    DocumentNode(Option<Box<NodeTest>>),
}

impl NodeTest {
    /// The default priority §6.4 gives a one-step pattern with this test.
    ///
    /// §6.4's table for an `ElementTest` or `AttributeTest`, where `E`, `A` and
    /// `T` stand for an element name, an attribute name and a type name:
    ///
    /// > | Format | Priority | Notes |
    /// > | `element()`, `element(*)` | −0.5 | (equivalent to `*`) |
    /// > | `attribute()`, `attribute(*)` | −0.5 | (equivalent to `@*`) |
    /// > | `element(E)` | 0 | (equivalent to `E`) |
    /// > | `element(*, T)` | 0 | (matches by type only) |
    /// > | `attribute(A)` | 0 | (equivalent to `@A`) |
    /// > | `attribute(*, T)` | 0 | (matches by type only) |
    /// > | `element(E, T)` | 0.25 | (matches by name and type) |
    /// > | `schema-element(E)` | 0.25 | (matches by substitution group and type) |
    /// > | `attribute(A, T)` | 0.25 | (matches by name and type) |
    /// > | `schema-attribute(A)` | 0.25 | (matches by name and type) |
    ///
    /// > The presence or absence of the symbol `?` following a type name does
    /// > not affect the priority.
    fn priority(&self) -> f64 {
        match self {
            // "a QName preceded by an optional PatternAxis" and
            // "processing-instruction('x')".
            NodeTest::Name(_) | NodeTest::Pi(Some(_)) => 0.0,
            // "element(E)", "attribute(A)".
            NodeTest::Element(Some(_)) | NodeTest::Attribute(Some(_)) => 0.0,
            // "element(*, T)", "attribute(*, T)" — "matches by type only".
            NodeTest::TypedElement { name: None, .. }
            | NodeTest::TypedAttribute { name: None, .. } => 0.0,
            // "element(E, T)", "attribute(A, T)" — "matches by name and type";
            // "schema-element(E)" — "matches by substitution group and type";
            // "schema-attribute(A)" — "matches by name and type".
            NodeTest::TypedElement { name: Some(_), .. }
            | NodeTest::TypedAttribute { name: Some(_), .. }
            | NodeTest::SchemaElement { .. }
            | NodeTest::SchemaAttribute { .. } => 0.25,
            // "NCName:*" and "*:NCName".
            NodeTest::NsWildcard(_) | NodeTest::LocalWildcard(_) => -0.25,
            // "If it does include an ElementTest or SchemaElementTest, then the
            // priority is the same as the priority of that ElementTest or
            // SchemaElementTest".
            NodeTest::DocumentNode(Some(inner)) => inner.priority(),
            // Everything else: "*", "node()", "text()", "comment()",
            // "processing-instruction()", "element()", "attribute()",
            // "document-node()".
            _ => -0.5,
        }
    }
}

impl PathPattern {
    /// §6.4's default-priority table, transcribed.
    ///
    /// > If the pattern consists of a single PathPattern of the form
    /// > `child::ElementName` or `attribute::AttributeName` … the default
    /// > priority is 0. … If the pattern has the form of a QName preceded by a
    /// > PatternAxis … Otherwise, the priority is 0.5.
    fn compute_priority(&self) -> f64 {
        if let Anchor::IdKey(_) = self.anchor {
            // An IdKeyPattern, with or without steps, is none of the forms
            // §6.4 lists: "Otherwise, the priority is 0.5."
            return 0.5;
        }
        if self.steps.is_empty() {
            // The pattern `/`.
            return -0.5;
        }
        let single = self.steps.len() == 1
            && self.anchor == Anchor::None
            && self.steps[0].predicates.is_empty();
        if single {
            self.steps[0].test.priority()
        } else {
            0.5
        }
    }

    /// The bucket of the dispatch index (§8.5) this rule belongs in, taken
    /// from its last step.
    ///
    /// A test that matches by type only — `element(*, T)`, `attribute(*, T)` —
    /// has no name to be filed under and goes with the other name-less tests
    /// (`IndexKey::Other`, consulted for every node). `schema-element(N)` is
    /// filed under `N` and under every member of `N`'s substitution group
    /// (XPath 2.0 §2.5.4.4), the only names an element it matches can have.
    pub fn index_key(&self) -> IndexKey {
        let Some(last) = self.steps.first() else {
            return IndexKey::Other;
        };
        match (&last.test, last.axis) {
            (NodeTest::Name(q), PatternAxis::Child) => IndexKey::Element(q.clone()),
            (NodeTest::Name(q), PatternAxis::Attribute) => IndexKey::Attribute(q.clone()),
            (NodeTest::Element(Some(q)) | NodeTest::TypedElement { name: Some(q), .. }, _) => {
                IndexKey::Element(q.clone())
            }
            (NodeTest::SchemaElement { name, members, .. }, _) => {
                let mut names = Vec::with_capacity(members.len() + 1);
                names.push(name.clone());
                names.extend(members.iter().cloned());
                IndexKey::Elements(names)
            }
            (
                NodeTest::Attribute(Some(q))
                | NodeTest::TypedAttribute { name: Some(q), .. }
                | NodeTest::SchemaAttribute { name: q, .. },
                _,
            ) => IndexKey::Attribute(q.clone()),
            (NodeTest::NsWildcard(uri), PatternAxis::Child) => IndexKey::Namespace(uri.clone()),
            (NodeTest::LocalWildcard(local), PatternAxis::Child) => IndexKey::Local(local.clone()),
            _ => IndexKey::Other,
        }
    }
}

/// Which bucket of a [`ModeIndex`](crate::model::ModeIndex) a rule goes into.
pub enum IndexKey {
    Element(QName),
    /// Every one of these element names (`schema-element(N)`: `N` and the
    /// members of its substitution group).
    Elements(Vec<QName>),
    Attribute(QName),
    Namespace(String),
    Local(String),
    Other,
}

// ── Parsing ──────────────────────────────────────────────────────────────

/// What the compiler must supply while a pattern is being read: prefix
/// resolution, the default element namespace, a way to compile a predicate's
/// text into an expression, and the in-scope schema components a typed node
/// test names.
pub trait PatternSink {
    /// Resolves a namespace prefix; `""` asks for the default namespace for
    /// element names (`[xsl:]xpath-default-namespace`).
    fn resolve_prefix(&self, prefix: &str) -> Option<String>;
    /// Compiles a predicate's source text.
    fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError>;
    /// The test `element(name, type_name)` (`nillable`: `type_name?`) or
    /// `attribute(name, type_name)` — `name` `None` for `*` — against the
    /// in-scope schema types (§3.13). A type name that is not one of them is
    /// XPST0008 (XPath 2.0 §2.5.4.3, §2.5.4.5: "TypeName must be present in
    /// the in-scope schema types").
    fn type_test(
        &self,
        kind: TestKind,
        name: Option<&QName>,
        type_name: &QName,
        nillable: bool,
    ) -> Result<ItemType, XsltError>;
    /// The test `schema-element(name)` / `schema-attribute(name)` against the
    /// in-scope element / attribute declarations, with the names of the other
    /// members of an element's substitution group. A name that is not
    /// declared is XPST0008 (XPath 2.0 §2.5.4.4, §2.5.4.6).
    fn schema_test(&self, kind: TestKind, name: &QName) -> Result<SchemaTest, XsltError>;
}

/// Whether a typed node test is about elements or attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKind {
    Element,
    Attribute,
}

/// What [`PatternSink::schema_test`] resolves `schema-element(N)` /
/// `schema-attribute(N)` to.
#[derive(Clone, Debug)]
pub struct SchemaTest {
    /// The test that decides a match.
    pub test: ItemType,
    /// For `schema-element(N)`: the expanded names of the global element
    /// declarations in `N`'s substitution group other than `N`, at any depth;
    /// empty for `schema-attribute(N)`.
    pub members: Vec<QName>,
}

/// Parses `source` into a [`Pattern`].
///
/// > \[ERR XTSE0340\] Where an attribute is defined to contain a pattern, it
/// > is a static error if the pattern does not match the production Pattern.
///
/// §5.5.2: "Every pattern is a legal XPath expression", and the constructs
/// the Pattern grammar borrows "are part of the XPath expression language, and
/// are defined in \[XPath 2.0\]" — so a pattern is read with XPath's lexical
/// rules, comments included. XPath 2.0 §2.6: "Comments are lexical constructs
/// only, and do not affect expression processing. … Comments may be nested. A
/// comment may be used anywhere ignorable whitespace is allowed". Each comment
/// is therefore replaced by one space before the structure is read
/// (`without_comments`).
pub fn parse(source: &str, sink: &mut dyn PatternSink) -> Result<Pattern, XsltError> {
    let text = without_comments(source)?;
    let mut alternatives = Vec::new();
    for alt in split_top_level(&text, '|') {
        let alt = whitespace::trim(&alt);
        if alt.is_empty() {
            return Err(bad_pattern(source, "an alternative is empty"));
        }
        alternatives.push(parse_alternative(alt, source, sink)?);
    }
    if alternatives.is_empty() {
        return Err(bad_pattern(
            source,
            "a pattern must have at least one alternative",
        ));
    }
    Ok(Pattern {
        alternatives,
        source: source.to_string(),
    })
}

/// `source` with every XPath comment replaced by a single space.
///
/// XPath 2.0 §2.6: "`Comment ::= "(:" (CommentContents | Comment)* ":)"`" and
/// A.1.3 (grammar-note: comments): "A comment can contain nested comments, as
/// long as all "(:" and ":)" patterns are balanced, no matter where they occur
/// within the outer comment." A space, because A.2.4.1 says "Comments may also
/// act as "whitespace" to prevent two adjacent terminals from being recognized
/// as one": `a(: c :)b` is two names, not `ab`. Where whitespace is not
/// allowed — inside a QName (A.1.2: "comments are not permissible on either
/// side of the colon"), a wildcard, a numeric literal — the space is just as
/// much an error as the comment.
///
/// A string literal is not scanned for comments: in `[@v = '(: x :)']` the
/// characters belong to the literal, and a quote inside a comment opens no
/// literal. A comment that is not closed is a syntax error, and the pattern
/// does not match the production Pattern (XTSE0340).
fn without_comments(source: &str) -> Result<Cow<'_, str>, XsltError> {
    if !source.contains("(:") {
        return Ok(Cow::Borrowed(source));
    }
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            // `StringLiteral`: a doubled delimiter closes and reopens it, which
            // leaves the same characters in the output.
            Some(q) => {
                if c == q {
                    quote = None;
                }
                out.push(c);
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                out.push(c);
            }
            None if c == '(' && chars.peek() == Some(&':') => {
                chars.next();
                let mut depth = 1usize;
                while depth > 0 {
                    match chars.next() {
                        Some('(') if chars.peek() == Some(&':') => {
                            chars.next();
                            depth += 1;
                        }
                        Some(':') if chars.peek() == Some(&')') => {
                            chars.next();
                            depth -= 1;
                        }
                        Some(_) => {}
                        None => {
                            return Err(bad_pattern(
                                source,
                                "an XPath comment opened with `(:` is not closed with `:)`",
                            ))
                        }
                    }
                }
                out.push(' ');
            }
            None => out.push(c),
        }
    }
    Ok(Cow::Owned(out))
}

fn bad_pattern(source: &str, why: impl std::fmt::Display) -> XsltError {
    XsltError::new(
        codes::XTSE0340,
        format!("{source:?} is not a pattern: {why}"),
    )
}

fn parse_alternative(
    alt: &str,
    source: &str,
    sink: &mut dyn PatternSink,
) -> Result<PathPattern, XsltError> {
    if let Some((call, rest)) = split_id_key_call(alt, source)? {
        return parse_id_key(call, rest, source, sink);
    }

    let (anchor, rest) = if let Some(rest) = alt.strip_prefix("//") {
        (Anchor::RootDescendant, whitespace::trim_start(rest))
    } else if let Some(rest) = alt.strip_prefix('/') {
        (Anchor::Root, whitespace::trim_start(rest))
    } else {
        (Anchor::None, alt)
    };

    if rest.is_empty() {
        return match anchor {
            // `/` on its own matches the root of the tree.
            Anchor::Root => Ok(finish(PathPattern {
                anchor: Anchor::Root,
                steps: Vec::new(),
                priority: 0.0,
            })),
            _ => Err(bad_pattern(
                source,
                "a path pattern needs at least one step",
            )),
        };
    }

    // Split into steps, remembering the connector that preceded each.
    let pieces = split_steps(rest, source)?;
    let mut steps: Vec<PatternStep> = Vec::with_capacity(pieces.len());
    for (index, (connector, text)) in pieces.iter().enumerate() {
        // The leftmost step's connector is the anchor's business.
        let connector = if index == 0 {
            Connector::Parent
        } else {
            *connector
        };
        // §5.5.3 adjusts "the first PatternStep PS" of a RelativePathPattern
        // only — the leftmost step of an alternative with no `/` or `//`.
        let first_of_relative = index == 0 && anchor == Anchor::None;
        steps.push(parse_step(
            text,
            connector,
            first_of_relative,
            source,
            sink,
        )?);
    }
    steps.reverse();
    Ok(finish(PathPattern {
        anchor,
        steps,
        priority: 0.0,
    }))
}

/// If `alt` starts with an `id(…)` or `key(…)` call, splits it into the call
/// (up to its closing parenthesis) and what follows.
///
/// XPath's own rule decides what a function call is: a name followed,
/// possibly after whitespace, by `(`. So `id/x` and `key` are path patterns
/// over elements named `id` and `key`, and `idx(…)` is no IdKeyPattern at all.
fn split_id_key_call<'s>(
    alt: &'s str,
    source: &str,
) -> Result<Option<(&'s str, &'s str)>, XsltError> {
    let name_end = alt
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
        .unwrap_or(alt.len());
    if !matches!(&alt[..name_end], "id" | "key") {
        return Ok(None);
    }
    let after_name = whitespace::trim_start(&alt[name_end..]);
    if !after_name.starts_with('(') {
        return Ok(None);
    }
    let open = alt.len() - after_name.len();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, ch) in alt[open..].char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"') | (None, '\'') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    let end = open + i + 1;
                    return Ok(Some((&alt[..end], &alt[end..])));
                }
            }
            _ => {}
        }
    }
    Err(bad_pattern(source, "unbalanced parentheses"))
}

/// Parses an alternative that starts with an `IdKeyPattern` (§5.5.2):
///
/// > `PathPattern ::= … | IdKeyPattern (('/' | '//') RelativePathPattern)?`
///
/// The arguments are restricted: "provided that the value to be matched is
/// supplied as either a literal or a reference to a variable or parameter, and
/// the key name (in the case of the key function) is supplied as a string
/// literal."
fn parse_id_key(
    call: &str,
    rest: &str,
    source: &str,
    sink: &mut dyn PatternSink,
) -> Result<PathPattern, XsltError> {
    let open = call.find('(').expect("split_id_key_call found one");
    let function = whitespace::trim(&call[..open]);
    let args: Vec<String> = split_top_level(&call[open + 1..call.len() - 1], ',')
        .iter()
        .map(|arg| whitespace::trim(arg).to_string())
        .collect();
    let valid = match (function, args.as_slice()) {
        // `IdValue ::= StringLiteral | VarRef`
        ("id", [value]) => is_string_literal(value) || is_var_ref(value),
        // `'key' '(' StringLiteral ',' KeyValue ')'`, `KeyValue ::= Literal | VarRef`
        ("key", [name, value]) => {
            is_string_literal(name)
                && (is_string_literal(value) || is_numeric_literal(value) || is_var_ref(value))
        }
        _ => false,
    };
    if !valid {
        return Err(bad_pattern(
            source,
            format!(
                "{call:?} is not an IdKeyPattern: id() takes one string literal or variable \
                 reference, key() a string literal and a literal or variable reference"
            ),
        ));
    }
    let call = sink.compile_predicate(call)?;

    let rest = whitespace::trim_start(rest);
    if rest.is_empty() {
        return Ok(finish(PathPattern {
            anchor: Anchor::IdKey(IdKeyAnchor {
                call,
                connector: Connector::Parent,
            }),
            steps: Vec::new(),
            priority: 0.0,
        }));
    }
    let (connector, relative) = if let Some(relative) = rest.strip_prefix("//") {
        (Connector::Ancestor, relative)
    } else if let Some(relative) = rest.strip_prefix('/') {
        (Connector::Parent, relative)
    } else {
        return Err(bad_pattern(
            source,
            "an IdKeyPattern can only be followed by `/` or `//` and a relative path pattern",
        ));
    };
    let relative = whitespace::trim_start(relative);
    if relative.is_empty() {
        return Err(bad_pattern(source, "a path pattern must not end with `/`"));
    }
    let pieces = split_steps(relative, source)?;
    let mut steps: Vec<PatternStep> = Vec::with_capacity(pieces.len());
    for (index, (step_connector, text)) in pieces.iter().enumerate() {
        let step_connector = if index == 0 {
            Connector::Parent
        } else {
            *step_connector
        };
        // The leftmost step follows `/` or `//`, so it is not the first step of
        // a RelativePathPattern standing alone and §5.5.3 adjusts nothing.
        steps.push(parse_step(text, step_connector, false, source, sink)?);
    }
    steps.reverse();
    Ok(finish(PathPattern {
        anchor: Anchor::IdKey(IdKeyAnchor { call, connector }),
        steps,
        priority: 0.0,
    }))
}

/// XPath's `StringLiteral`: `'…'` or `"…"`, the delimiter doubled inside.
fn is_string_literal(text: &str) -> bool {
    let Some(quote) = text.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
        return false;
    };
    if text.len() < 2 || !text.ends_with(quote) {
        return false;
    }
    // Inside, every delimiter must come as a doubled pair.
    let inner = &text[1..text.len() - 1];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == quote && chars.next() != Some(quote) {
            return false;
        }
    }
    true
}

/// XPath's `NumericLiteral`: `IntegerLiteral | DecimalLiteral | DoubleLiteral`.
fn is_numeric_literal(text: &str) -> bool {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = (!int.is_empty() || !frac.is_empty())
        && digits(int)
        && digits(frac)
        && (mantissa.contains('.') || !int.is_empty());
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    mantissa_ok && exponent_ok
}

/// XPath's `VarRef`: `$` followed by a QName (whitespace is allowed after the
/// `$`).
fn is_var_ref(text: &str) -> bool {
    let Some(name) = text.strip_prefix('$') else {
        return false;
    };
    let name = whitespace::trim_start(name);
    let (prefix, local) = split_qname(name);
    is_ncname(local) && (prefix.is_empty() || is_ncname(prefix))
}

fn finish(mut p: PathPattern) -> PathPattern {
    p.priority = p.compute_priority();
    p
}

/// Splits a relative path pattern into `(connector-to-its-left, step-text)`
/// pieces, ignoring `/` inside predicates, parentheses and string literals.
fn split_steps(text: &str, source: &str) -> Result<Vec<(Connector, String)>, XsltError> {
    let bytes: Vec<char> = text.chars().collect();
    let mut out: Vec<(Connector, String)> = Vec::new();
    let mut connector = Connector::Parent;
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"') | (None, '\'') => quote = Some(ch),
            (None, '[') | (None, '(') => depth += 1,
            (None, ']') | (None, ')') => depth -= 1,
            (None, '/') if depth == 0 => {
                let piece: String = bytes[start..i].iter().collect();
                let piece = whitespace::trim(&piece).to_string();
                if piece.is_empty() {
                    return Err(bad_pattern(source, "an empty step"));
                }
                out.push((connector, piece));
                if bytes.get(i + 1) == Some(&'/') {
                    connector = Connector::Ancestor;
                    i += 1;
                } else {
                    connector = Connector::Parent;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 || quote.is_some() {
        return Err(bad_pattern(source, "unbalanced brackets or quotes"));
    }
    let piece: String = bytes[start..].iter().collect();
    let piece = whitespace::trim(&piece).to_string();
    if piece.is_empty() {
        return Err(bad_pattern(source, "a path pattern must not end with `/`"));
    }
    out.push((connector, piece));
    Ok(out)
}

fn parse_step(
    text: &str,
    connector: Connector,
    first_of_relative: bool,
    source: &str,
    sink: &mut dyn PatternSink,
) -> Result<PatternStep, XsltError> {
    // PatternAxis, if one is written.
    let (explicit_axis, rest) = if let Some(rest) = strip_axis(text, "child") {
        (Some(PatternAxis::Child), rest)
    } else if let Some(rest) = strip_axis(text, "attribute") {
        (Some(PatternAxis::Attribute), rest)
    } else if let Some(rest) = text.strip_prefix('@') {
        (Some(PatternAxis::Attribute), whitespace::trim_start(rest))
    } else {
        (None, text)
    };

    // NodeTest, then the predicate list.
    let (test_text, predicate_text) = split_predicates(rest, source)?;
    let test_text = whitespace::trim(&test_text);
    // XPath 2.0 §3.2.4: "If the axis name is omitted from an axis step, the
    // default axis is child unless the axis step contains an AttributeTest or
    // SchemaAttributeTest; in that case, the default axis is attribute."
    // §5.5.3 makes a pattern mean its equivalent XPath expression, so the
    // same default applies to a pattern step.
    let axis = explicit_axis.unwrap_or_else(|| {
        if is_attribute_kind_test(test_text) {
            PatternAxis::Attribute
        } else {
            PatternAxis::Child
        }
    });
    let test = parse_node_test(test_text, axis, source, sink)?;
    // §5.5.3: "If the NodeTest in PS is document-node() (optionally with
    // arguments), and if no explicit axis is specified, then the axis in step
    // PS is taken as self rather than child." Anywhere else — an explicit
    // `child::document-node()`, or a step that is not the first of a
    // RelativePathPattern — the step keeps its axis and can never select a
    // document node, which is never a child or an attribute.
    let axis = match test {
        NodeTest::DocumentNode(_) if explicit_axis.is_none() && first_of_relative => {
            PatternAxis::SelfAxis
        }
        _ => axis,
    };
    let mut predicates = Vec::new();
    for predicate in predicate_text {
        predicates.push(sink.compile_predicate(&predicate)?);
    }
    Ok(PatternStep {
        axis,
        test,
        predicates,
        connector,
    })
}

/// What follows `axis::` at the start of `text`, if the step starts with that
/// axis.
///
/// `PatternAxis ::= ('child' '::' | 'attribute' '::' | '@')`: the axis name
/// and `::` are two terminals, and XPath 2.0 A.2.4.1 says "Whitespace is
/// allowed between any two terminals" — so `child :: a` is `child::a`, and so
/// is `child (: c :) :: a` once its comment is a space. `child`, `child/x` and
/// `childish::x` do not start with the axis.
fn strip_axis<'s>(text: &'s str, axis: &str) -> Option<&'s str> {
    let rest = whitespace::trim_start(text.strip_prefix(axis)?);
    Some(whitespace::trim_start(rest.strip_prefix("::")?))
}

/// Whether a node test is an `AttributeTest` or a `SchemaAttributeTest` —
/// the two kind tests that give an axis step the `attribute` axis by default
/// (XPath 2.0 §3.2.4).
fn is_attribute_kind_test(test: &str) -> bool {
    test.split_once('(')
        .is_some_and(|(kind, _)| matches!(whitespace::trim(kind), "attribute" | "schema-attribute"))
}

/// Splits `nodetest[p1][p2]…` into the node test and the predicate sources.
fn split_predicates(text: &str, source: &str) -> Result<(String, Vec<String>), XsltError> {
    let bytes: Vec<char> = text.chars().collect();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut test_end = bytes.len();
    let mut predicates: Vec<String> = Vec::new();
    let mut open: Option<usize> = None;
    for (i, &ch) in bytes.iter().enumerate() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"') | (None, '\'') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, '[') => {
                depth += 1;
                if depth == 1 {
                    if open.is_none() && test_end == bytes.len() {
                        test_end = i;
                    }
                    open = Some(i + 1);
                }
            }
            (None, ']') => {
                depth -= 1;
                if depth == 0 {
                    let start = open
                        .take()
                        .ok_or_else(|| bad_pattern(source, "a stray `]`"))?;
                    predicates.push(bytes[start..i].iter().collect());
                }
            }
            _ => {}
        }
    }
    if depth != 0 || quote.is_some() || open.is_some() {
        return Err(bad_pattern(source, "unbalanced predicates"));
    }
    Ok((bytes[..test_end].iter().collect(), predicates))
}

fn parse_node_test(
    text: &str,
    axis: PatternAxis,
    source: &str,
    sink: &dyn PatternSink,
) -> Result<NodeTest, XsltError> {
    if let Some(open) = text.find('(') {
        if !text.ends_with(')') {
            return Err(bad_pattern(source, "a kind test must end with `)`"));
        }
        // XPath's own production `S` is the same four characters (§4).
        let kind = whitespace::trim(&text[..open]);
        let arg = whitespace::trim(&text[open + 1..text.len() - 1]);
        return parse_kind_test(kind, arg, source, sink);
    }
    // A name test: `*`, `p:*`, `*:a`, `a`, `p:a`.
    if text == "*" {
        return Ok(NodeTest::AnyName);
    }
    let (prefix, local) = split_qname(text);
    if local == "*" {
        let uri = sink
            .resolve_prefix(prefix)
            .ok_or_else(|| unbound_prefix(prefix, source))?;
        return Ok(NodeTest::NsWildcard(uri));
    }
    if prefix == "*" {
        if !is_ncname(local) {
            return Err(bad_pattern(source, "`*:` must be followed by an NCName"));
        }
        return Ok(NodeTest::LocalWildcard(local.to_string()));
    }
    if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
        return Err(bad_pattern(source, format!("{text:?} is not a node test")));
    }
    Ok(NodeTest::Name(expand(prefix, local, axis, source, sink)?))
}

/// Expands a name test's QName.
///
/// §5.5.3: a pattern is evaluated as an XPath expression, so
/// `[xsl:]xpath-default-namespace` applies to an unprefixed element name test
/// — and, per XPath, never to an attribute name test.
fn expand(
    prefix: &str,
    local: &str,
    axis: PatternAxis,
    source: &str,
    sink: &dyn PatternSink,
) -> Result<QName, XsltError> {
    if prefix.is_empty() {
        return Ok(match axis {
            PatternAxis::Child | PatternAxis::SelfAxis => {
                QName::new(sink.resolve_prefix("").unwrap_or_default(), local)
            }
            PatternAxis::Attribute => QName::local(local),
        });
    }
    let uri = sink
        .resolve_prefix(prefix)
        .ok_or_else(|| unbound_prefix(prefix, source))?;
    Ok(QName::new(uri, local))
}

fn unbound_prefix(prefix: &str, source: &str) -> XsltError {
    XsltError::new(
        codes::XTSE0280,
        format!("the prefix {prefix:?} used in the pattern {source:?} is not bound"),
    )
}

fn parse_kind_test(
    kind: &str,
    arg: &str,
    source: &str,
    sink: &dyn PatternSink,
) -> Result<NodeTest, XsltError> {
    match kind {
        "node" if arg.is_empty() => Ok(NodeTest::AnyNode),
        "text" if arg.is_empty() => Ok(NodeTest::Text),
        "comment" if arg.is_empty() => Ok(NodeTest::Comment),
        "processing-instruction" => {
            if arg.is_empty() {
                Ok(NodeTest::Pi(None))
            } else {
                let name = unquote(arg).unwrap_or_else(|| arg.to_string());
                if !is_ncname(&name) {
                    return Err(bad_pattern(source, "a PI target must be an NCName"));
                }
                Ok(NodeTest::Pi(Some(name)))
            }
        }
        "element" | "attribute" => {
            let (test_kind, axis) = if kind == "element" {
                (TestKind::Element, PatternAxis::Child)
            } else {
                (TestKind::Attribute, PatternAxis::Attribute)
            };
            let (arg, type_name) = match arg.split_once(',') {
                Some((name, type_name)) => {
                    (whitespace::trim(name), Some(whitespace::trim(type_name)))
                }
                None => (arg, None),
            };
            let name = match arg {
                "" | "*" => None,
                name => {
                    let (prefix, local) = split_qname(name);
                    Some(expand(prefix, local, axis, source, sink)?)
                }
            };
            let Some(type_name) = type_name else {
                return Ok(match test_kind {
                    TestKind::Element => NodeTest::Element(name),
                    TestKind::Attribute => NodeTest::Attribute(name),
                });
            };
            // XPath 2.0 §2.5.4.3: `element(ElementName, TypeName ?)` — the `?`
            // is a terminal of its own, so whitespace may precede it (A.2.4.1).
            // An AttributeTest has no `?` (§2.5.4.5), and the XPath parser has
            // already rejected one there.
            let (type_name, nillable) = match type_name.strip_suffix('?') {
                Some(type_name) => (whitespace::trim(type_name), true),
                None => (type_name, false),
            };
            // §2.5.4: "QNames appearing in a sequence type have their prefixes
            // expanded … by means of the statically known namespaces and
            // (where applicable) the default element/type namespace" — a
            // TypeName is an element/type name, so `expand` on the child axis.
            let (prefix, local) = split_qname(type_name);
            let type_name = expand(prefix, local, PatternAxis::Child, source, sink)?;
            let test = sink.type_test(test_kind, name.as_ref(), &type_name, nillable)?;
            Ok(match test_kind {
                TestKind::Element => NodeTest::TypedElement { name, test },
                TestKind::Attribute => NodeTest::TypedAttribute { name, test },
            })
        }
        "document-node" => {
            if arg.is_empty() {
                return Ok(NodeTest::DocumentNode(None));
            }
            let inner = parse_node_test(arg, PatternAxis::Child, source, sink)?;
            match inner {
                // §6.4: "if it does include an ElementTest … the priority is
                // the same as the priority of that ElementTest", which
                // `NodeTest::priority` reads off the boxed test.
                NodeTest::Element(_)
                | NodeTest::TypedElement { .. }
                | NodeTest::SchemaElement { .. } => {
                    Ok(NodeTest::DocumentNode(Some(Box::new(inner))))
                }
                _ => Err(bad_pattern(
                    source,
                    "document-node() takes an ElementTest, a SchemaElementTest or nothing",
                )),
            }
        }
        // XPath 2.0 §2.5.4.4 / §2.5.4.6: `schema-element(ElementName)`,
        // `schema-attribute(AttributeName)`. The name is expanded as a name test
        // on the axis of its kind: an unprefixed ElementName takes the default
        // element namespace, an unprefixed AttributeName none.
        "schema-element" | "schema-attribute" => {
            let (test_kind, axis) = if kind == "schema-element" {
                (TestKind::Element, PatternAxis::Child)
            } else {
                (TestKind::Attribute, PatternAxis::Attribute)
            };
            let (prefix, local) = split_qname(arg);
            if !is_ncname(local) || (!prefix.is_empty() && !is_ncname(prefix)) {
                return Err(bad_pattern(source, format!("{kind}() takes a QName")));
            }
            let name = expand(prefix, local, axis, source, sink)?;
            let SchemaTest { test, members } = sink.schema_test(test_kind, &name)?;
            Ok(match test_kind {
                TestKind::Element => NodeTest::SchemaElement {
                    name,
                    members,
                    test,
                },
                TestKind::Attribute => NodeTest::SchemaAttribute { name, test },
            })
        }
        other => Err(bad_pattern(source, format!("{other}() is not a node test"))),
    }
}

fn unquote(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        Some(text[1..text.len() - 1].to_string())
    } else {
        None
    }
}

/// Splits `text` on `sep`, ignoring separators inside brackets, parentheses
/// and string literals.
fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    for ch in text.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(ch);
            }
            (Some(_), _) => current.push(ch),
            (None, '"') | (None, '\'') => {
                quote = Some(ch);
                current.push(ch);
            }
            (None, '[') | (None, '(') => {
                depth += 1;
                current.push(ch);
            }
            (None, ']') | (None, ')') => {
                depth -= 1;
                current.push(ch);
            }
            (None, c) if c == sep && depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    out.push(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sink(Vec<String>);

    impl PatternSink for Sink {
        fn resolve_prefix(&self, prefix: &str) -> Option<String> {
            match prefix {
                "" => Some(String::new()),
                "p" => Some("urn:p".to_string()),
                "xs" => Some("http://www.w3.org/2001/XMLSchema".to_string()),
                _ => None,
            }
        }
        fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError> {
            self.0.push(source.to_string());
            Ok(ExprId(self.0.len() as u32 - 1))
        }
        // The grammar and the priorities do not look at the resolved test.
        fn type_test(
            &self,
            _kind: TestKind,
            _name: Option<&QName>,
            _type_name: &QName,
            _nillable: bool,
        ) -> Result<ItemType, XsltError> {
            Ok(ItemType::AnyNode)
        }
        fn schema_test(&self, kind: TestKind, name: &QName) -> Result<SchemaTest, XsltError> {
            Ok(SchemaTest {
                test: ItemType::AnyNode,
                members: match kind {
                    TestKind::Element => vec![QName::new(name.uri.clone(), "member")],
                    TestKind::Attribute => Vec::new(),
                },
            })
        }
    }

    fn parse_ok(source: &str) -> (Pattern, Vec<String>) {
        let mut sink = Sink(Vec::new());
        let pattern = parse(source, &mut sink).unwrap();
        (pattern, sink.0)
    }

    #[test]
    fn alternatives_become_separate_rules() {
        let (p, _) = parse_ok("a | @b | node()");
        assert_eq!(p.alternatives.len(), 3);
        assert_eq!(p.alternatives[0].priority, 0.0);
        assert_eq!(p.alternatives[1].priority, 0.0);
        assert_eq!(p.alternatives[2].priority, -0.5);
        assert_eq!(p.alternatives[1].steps[0].axis, PatternAxis::Attribute);
    }

    #[test]
    fn steps_are_stored_last_first_with_their_connectors() {
        let (p, _) = parse_ok("/a//b/c");
        let alt = &p.alternatives[0];
        assert_eq!(alt.anchor, Anchor::Root);
        assert_eq!(alt.steps.len(), 3);
        assert_eq!(alt.steps[0].test, NodeTest::Name(QName::local("c")));
        assert_eq!(alt.steps[0].connector, Connector::Parent);
        assert_eq!(alt.steps[1].test, NodeTest::Name(QName::local("b")));
        assert_eq!(alt.steps[1].connector, Connector::Ancestor);
        assert_eq!(alt.steps[2].test, NodeTest::Name(QName::local("a")));
        assert_eq!(alt.priority, 0.5);
    }

    #[test]
    fn the_root_pattern_has_no_steps() {
        let (p, _) = parse_ok("/");
        assert_eq!(p.alternatives[0].anchor, Anchor::Root);
        assert!(p.alternatives[0].steps.is_empty());
        assert_eq!(p.alternatives[0].priority, -0.5);
    }

    #[test]
    fn predicates_are_handed_out_in_order_and_do_not_split_steps() {
        let (p, predicates) = parse_ok("a[b/c = 'x/y'][2]");
        assert_eq!(predicates, vec!["b/c = 'x/y'".to_string(), "2".to_string()]);
        assert_eq!(p.alternatives[0].steps.len(), 1);
        assert_eq!(p.alternatives[0].steps[0].predicates.len(), 2);
        // A predicate makes the pattern "anything else" for priority purposes.
        assert_eq!(p.alternatives[0].priority, 0.5);
    }

    #[test]
    fn the_default_priority_table_of_6_4() {
        let cases: &[(&str, f64)] = &[
            ("/", -0.5),
            ("a", 0.0),
            ("p:a", 0.0),
            ("@a", 0.0),
            ("processing-instruction('x')", 0.0),
            ("element()", -0.5),
            ("element(a)", 0.0),
            ("attribute()", -0.5),
            ("attribute(a)", 0.0),
            ("document-node()", -0.5),
            ("document-node(element(a))", 0.0),
            ("p:*", -0.25),
            ("*:a", -0.25),
            ("*", -0.5),
            ("node()", -0.5),
            ("text()", -0.5),
            ("comment()", -0.5),
            ("@*", -0.5),
            ("a/b", 0.5),
            ("//a", 0.5),
            ("a[1]", 0.5),
        ];
        for (source, expected) in cases {
            let (p, _) = parse_ok(source);
            assert_eq!(
                p.alternatives[0].priority, *expected,
                "default priority of {source:?}"
            );
        }
    }

    /// §6.4's table for the typed forms: `element(*,T)` / `attribute(*,T)` 0,
    /// `element(E,T)`, `schema-element(E)`, `attribute(A,T)`,
    /// `schema-attribute(A)` 0.25; "The presence or absence of the symbol ?
    /// following a type name does not affect the priority"; a DocumentTest
    /// takes the priority of its ElementTest or SchemaElementTest; with a
    /// PatternAxis the same; with a predicate or a second step 0.5.
    #[test]
    fn the_default_priorities_of_typed_tests() {
        let cases: &[(&str, f64)] = &[
            ("element(*, xs:integer)", 0.0),
            ("element(*, p:t)", 0.0),
            ("element(*, p:t?)", 0.0),
            ("element(a, p:t)", 0.25),
            ("element(p:a, p:t ?)", 0.25),
            ("child::element(a, p:t)", 0.25),
            ("attribute(*, p:t)", 0.0),
            ("@attribute(*, p:t)", 0.0),
            ("attribute(a, p:t)", 0.25),
            ("attribute::attribute(a, xs:string)", 0.25),
            ("schema-element(p:a)", 0.25),
            ("child::schema-element(a)", 0.25),
            ("schema-attribute(p:a)", 0.25),
            ("@schema-attribute(a)", 0.25),
            ("document-node(element(*, p:t))", 0.0),
            ("document-node(element(a, p:t?))", 0.25),
            ("document-node(schema-element(p:a))", 0.25),
            ("schema-element(p:a)[1]", 0.5),
            ("a/element(*, p:t)", 0.5),
            ("//schema-attribute(a)", 0.5),
        ];
        for (source, expected) in cases {
            let (p, _) = parse_ok(source);
            assert_eq!(
                p.alternatives[0].priority, *expected,
                "default priority of {source:?}"
            );
        }
    }

    /// XPath 2.0 §3.2.4: "the default axis is child unless the axis step
    /// contains an AttributeTest or SchemaAttributeTest"; §2.5.4: an
    /// unprefixed ElementName takes the default element namespace, an
    /// unprefixed AttributeName none; the dispatch index files
    /// `schema-element(N)` under N and its substitution-group members,
    /// `element(*, T)` with the name-less tests.
    #[test]
    fn typed_tests_have_their_axis_names_and_index_keys() {
        let (p, _) = parse_ok("schema-attribute(p:a)");
        let step = &p.alternatives[0].steps[0];
        assert_eq!(step.axis, PatternAxis::Attribute);
        assert!(
            matches!(&step.test, NodeTest::SchemaAttribute { name, .. } if *name == QName::new("urn:p", "a"))
        );
        assert!(
            matches!(p.alternatives[0].index_key(), IndexKey::Attribute(q) if q == QName::new("urn:p", "a"))
        );

        let (p, _) = parse_ok("schema-element(p:a)");
        assert_eq!(p.alternatives[0].steps[0].axis, PatternAxis::Child);
        match p.alternatives[0].index_key() {
            IndexKey::Elements(names) => assert_eq!(
                names,
                vec![QName::new("urn:p", "a"), QName::new("urn:p", "member")]
            ),
            _ => panic!("schema-element is filed under its names"),
        }

        let (p, _) = parse_ok("element(*, p:t?)");
        assert!(matches!(
            &p.alternatives[0].steps[0].test,
            NodeTest::TypedElement { name: None, .. }
        ));
        assert!(matches!(p.alternatives[0].index_key(), IndexKey::Other));

        let (p, _) = parse_ok("attribute(b, p:t) | element(b, p:t)");
        assert_eq!(p.alternatives[0].steps[0].axis, PatternAxis::Attribute);
        assert!(
            matches!(p.alternatives[0].index_key(), IndexKey::Attribute(q) if q == QName::local("b"))
        );
        assert!(
            matches!(p.alternatives[1].index_key(), IndexKey::Element(q) if q == QName::local("b"))
        );

        let (p, _) = parse_ok("document-node(schema-element(p:a))");
        assert_eq!(p.alternatives[0].steps[0].axis, PatternAxis::SelfAxis);
        assert!(matches!(p.alternatives[0].index_key(), IndexKey::Other));

        let mut sink = Sink(Vec::new());
        for bad in [
            "schema-element(*)",
            "schema-element()",
            "schema-attribute(a b)",
            "document-node(schema-attribute(a))",
            "document-node(attribute(a, p:t))",
        ] {
            let e = parse(bad, &mut sink).unwrap_err();
            assert_eq!(e.code, codes::XTSE0340, "{bad:?} should be XTSE0340: {e}");
        }
        // An unbound prefix in a TypeName or a schema test.
        for bad in ["element(*, q:t)", "schema-element(q:a)"] {
            let e = parse(bad, &mut sink).unwrap_err();
            assert_eq!(e.code, codes::XTSE0280, "{bad:?}: {e}");
        }
    }

    #[test]
    fn name_tests_use_the_default_element_namespace_but_attributes_do_not() {
        struct Defaulting;
        impl PatternSink for Defaulting {
            fn resolve_prefix(&self, prefix: &str) -> Option<String> {
                match prefix {
                    "" => Some("urn:d".to_string()),
                    _ => None,
                }
            }
            fn compile_predicate(&mut self, _source: &str) -> Result<ExprId, XsltError> {
                Ok(ExprId(0))
            }
            fn type_test(
                &self,
                _kind: TestKind,
                _name: Option<&QName>,
                _type_name: &QName,
                _nillable: bool,
            ) -> Result<ItemType, XsltError> {
                Ok(ItemType::AnyNode)
            }
            fn schema_test(&self, _kind: TestKind, _name: &QName) -> Result<SchemaTest, XsltError> {
                Ok(SchemaTest {
                    test: ItemType::AnyNode,
                    members: Vec::new(),
                })
            }
        }
        let p = parse("a | @a", &mut Defaulting).unwrap();
        assert_eq!(
            p.alternatives[0].steps[0].test,
            NodeTest::Name(QName::new("urn:d", "a"))
        );
        assert_eq!(
            p.alternatives[1].steps[0].test,
            NodeTest::Name(QName::local("a"))
        );
    }

    #[test]
    fn everything_outside_the_grammar_is_xtse0340() {
        let mut sink = Sink(Vec::new());
        for source in ["", "a/", "a[", "..", "ancestor::a", "foo()", "a/*/"] {
            let e = parse(source, &mut sink).unwrap_err();
            assert_eq!(e.code, codes::XTSE0340, "{source:?} should be XTSE0340");
        }
    }

    /// §5.5.2: `IdKeyPattern (('/' | '//') RelativePathPattern)?`, with the
    /// call handed to the compiler as an expression and a priority of 0.5
    /// (§6.4 "Otherwise").
    #[test]
    fn id_and_key_anchors() {
        let (p, exprs) = parse_ok("id('x')");
        let alt = &p.alternatives[0];
        assert!(matches!(alt.anchor, Anchor::IdKey(_)));
        assert!(alt.steps.is_empty());
        assert_eq!(alt.priority, 0.5);
        assert_eq!(exprs, vec!["id('x')".to_string()]);

        let (p, exprs) = parse_ok("key('k', $v)//a/b[1] | id ( \"y\" )/@c");
        assert_eq!(exprs[0], "key('k', $v)");
        let alt = &p.alternatives[0];
        let Anchor::IdKey(anchor) = alt.anchor else {
            panic!("an IdKey anchor")
        };
        assert_eq!(anchor.connector, Connector::Ancestor);
        assert_eq!(alt.steps.len(), 2);
        assert_eq!(alt.steps[1].axis, PatternAxis::Child);
        let Anchor::IdKey(anchor) = p.alternatives[1].anchor else {
            panic!("an IdKey anchor")
        };
        assert_eq!(anchor.connector, Connector::Parent);
        assert_eq!(p.alternatives[1].steps[0].axis, PatternAxis::Attribute);

        // Elements named `id` and `key` are ordinary name tests.
        let (p, _) = parse_ok("id/key");
        assert_eq!(p.alternatives[0].anchor, Anchor::None);

        let mut sink = Sink(Vec::new());
        for bad in [
            "id('a', $x)",
            "id(1)",
            "id(@a)",
            "key('k', 1+1)",
            "key($k, 'v')",
            "key('k')",
            "key('k', 'v')[1]",
            "key('k', 'v')x",
            "key('k', 'v')/",
        ] {
            let e = parse(bad, &mut sink).unwrap_err();
            assert_eq!(e.code, codes::XTSE0340, "{bad:?} should be XTSE0340");
        }
        for good in [
            "key('k', 5)",
            "key('k', 1.5e3)",
            "key('k', .5)",
            "key('k', \"it''s\")",
        ] {
            assert!(parse(good, &mut sink).is_ok(), "{good:?}");
        }
    }

    /// XPath 2.0 §2.6: "A comment may be used anywhere ignorable whitespace is
    /// allowed" and "Comments may be nested"; A.1.3: "A comment can contain
    /// nested comments, as long as all "(:" and ":)" patterns are balanced, no
    /// matter where they occur within the outer comment." A pattern is read with
    /// XPath's lexical rules (§5.5.2 "Every pattern is a legal XPath
    /// expression").
    #[test]
    fn xpath_comments_are_whitespace() {
        let shape = |source: &str| {
            let (p, exprs) = parse_ok(source);
            let alternatives: Vec<String> = p
                .alternatives
                .iter()
                .map(|a| format!("{:?}/{}/{}", a.anchor, a.steps.len(), a.priority))
                .collect();
            (alternatives, exprs)
        };
        for (with_comments, plain) in [
            ("key('k', 'v') (: c :)", "key('k', 'v')"),
            ("key( (: c :) 'k', 'v')", "key('k', 'v')"),
            ("key('k' (: c :), (: d :) $v (: e :))", "key('k', $v)"),
            ("key (: c :) ('k', 'v')", "key('k', 'v')"),
            ("key('k', $(: c :)v)", "key('k', $v)"),
            ("id('a') (: c :) / b", "id('a')/b"),
            ("id('a')(: c :)//(: d :)b", "id('a')//b"),
            ("(: c :) a", "a"),
            ("a (: c :)", "a"),
            ("a(: c :)", "a"),
            ("a (: c :) [1] (: d :)", "a[1]"),
            ("a (: x (: nested :) y :) / b", "a/b"),
            ("a (: | :) | b", "a | b"),
            ("a (: it's [ :) / b", "a/b"),
            ("a/(: c :)@(: d :)b", "a/@b"),
            ("child (: c :) :: a / attribute (: d :) :: b", "a/@b"),
            ("node (: c :) ( (: d :) )", "node()"),
            (
                "processing-instruction( (: c :) 'x' )",
                "processing-instruction('x')",
            ),
            (
                "document-node( element( a ) (: c :) )",
                "document-node(element(a))",
            ),
            ("/ (: c :)", "/"),
            ("(: c :) / (: d :) a", "/a"),
            ("(: c :) // (: d :) a", "//a"),
        ] {
            let (a, a_exprs) = shape(with_comments);
            let (b, b_exprs) = shape(plain);
            assert_eq!(a, b, "{with_comments:?} reads as {plain:?}");
            assert_eq!(a_exprs.len(), b_exprs.len(), "{with_comments:?}");
        }
        // A predicate's comment reaches the predicate's compiler as a space.
        let (_, exprs) = parse_ok("a[(: c :)1 (: d :)]");
        assert_eq!(exprs, vec![" 1  ".to_string()]);
        // Inside a string literal `(:` and `:)` are characters of the literal.
        let (_, exprs) = parse_ok("a[@v = '(: x :)'][. = \"it''s (:\"]");
        assert_eq!(
            exprs,
            vec!["@v = '(: x :)'".to_string(), ". = \"it''s (:\"".to_string()]
        );
        let (_, exprs) = parse_ok("key('k', '(: v :)')");
        assert_eq!(exprs, vec!["key('k', '(: v :)')".to_string()]);
        // Whitespace between an axis name and `::` (A.2.4.1 "Whitespace is
        // allowed between any two terminals"); a name that merely starts with
        // an axis name is a name.
        let (p, _) = parse_ok("child :: a/attribute ::b");
        assert_eq!(p.alternatives[0].steps[0].axis, PatternAxis::Attribute);
        assert_eq!(p.alternatives[0].steps[1].axis, PatternAxis::Child);
        let (p, _) = parse_ok("children/attributes");
        assert_eq!(
            p.alternatives[0].steps[1].test,
            NodeTest::Name(QName::local("children"))
        );

        let mut sink = Sink(Vec::new());
        for bad in [
            // Not closed.
            "a (: c",
            "a (: x (: nested :) y",
            "a (:)",
            // A.1.3: "(: "this is just a string :)" :) will cause a syntax
            // error" — the comment ends inside what looks like a literal.
            "a (: \"x :)\" :)",
            // Nothing but a comment.
            "(: c :)",
            "a | (: c :)",
            // A.1.2: "comments are not permissible on either side of the
            // colon" of a QName; nor inside a wildcard or a name.
            "p:(: c :)a",
            "p(: c :):a",
            "*:(: c :)a",
            "a(: c :)b",
        ] {
            let e = parse(bad, &mut sink).unwrap_err();
            assert_eq!(e.code, codes::XTSE0340, "{bad:?} should be XTSE0340: {e}");
        }
    }
}
