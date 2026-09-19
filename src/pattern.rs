//! Patterns (§5.5) — the grammar, the match algorithm and the default
//! priorities of §6.4.
//!
//! A pattern is written in a subset of the XPath path syntax, so the text is
//! first handed to the upstream XPath parser: anything it rejects is not a
//! pattern either (XTSE0340). The structure is then read off the text by the
//! scanner below, which rejects everything outside §5.5.2's grammar and hands
//! each predicate back to the compiler as its own expression — the predicates
//! must be separately compiled because §5.5.3 evaluates them against a focus
//! this crate builds itself.

use crate::error::{codes, XsltError};
use crate::model::ExprId;
use crate::names::{is_ncname, split_qname, QName, XSD_NS};
use crate::whitespace;

/// A compiled pattern: one or more alternatives, each an independent rule for
/// priority purposes (§6.4).
#[derive(Debug)]
pub struct Pattern {
    pub alternatives: Vec<PathPattern>,
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
}

/// How a step connects to the step on its left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    /// `/` — the step to the left matches the parent.
    Parent,
    /// `//` — the step to the left matches some ancestor.
    Ancestor,
}

/// The two axes §5.5.2 allows in a pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternAxis {
    Child,
    Attribute,
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
    /// `element(*, T)` / `element(a, T)` where `T` is one of the two type names
    /// a processor **without a schema** can still decide: `xs:untyped` and
    /// `xs:anyType`.
    ///
    /// The two do not decide the same way, which is what `untyped_only`
    /// records. XPath 2.0 §2.5.4.3 says an `ElementTest` with a TypeName
    /// matches an element "if … derives from the type named by the TypeName",
    /// so:
    ///
    /// * `element(N, xs:untyped)` matches only an element whose type annotation
    ///   *is* `xs:untyped` — i.e. one that has not been schema-validated.
    ///   `untyped_only` is `true` and the annotation is tested.
    /// * `element(N, xs:anyType)` matches any element whatever, because every
    ///   type — `xs:untyped` included — derives from `xs:anyType`.
    ///   `untyped_only` is `false` and only the name is tested.
    ///
    /// §4.3 is why the first is usually indistinguishable from `element(N)`:
    /// with no schema the source is read "as if no schema processing had been
    /// performed, that is, effectively to strip all type annotations from
    /// elements and attributes on input, marking them instead as having type
    /// `xs:untyped` and `xs:untypedAtomic` respectively". But a *prebuilt*
    /// document handed to [`DocumentStore::insert`](crate::store::DocumentStore::insert)
    /// can carry real annotations, and then the two spellings part company.
    ///
    /// §6.4 gives the two-argument form a higher default priority than the
    /// one-argument form in both spellings; `untyped_only` does not affect it.
    TypedElement {
        name: Option<QName>,
        untyped_only: bool,
    },
    /// `attribute(*, T)` / `attribute(a, T)` for `T` in `xs:untypedAtomic`,
    /// `xs:anySimpleType`, `xs:anyAtomicType` — see [`NodeTest::TypedElement`].
    ///
    /// `untyped_only` is `true` only for `xs:untypedAtomic`; the other two are
    /// supertypes of every attribute type there is.
    TypedAttribute {
        name: Option<QName>,
        untyped_only: bool,
    },
    /// `document-node()` / `document-node(element(a))`
    DocumentNode(Option<Box<NodeTest>>),
}

/// Whether an `ElementTest`'s type name is one a processor with no schema can
/// decide, and if so whether deciding it means testing the annotation.
///
/// `Some(true)` — `xs:untyped`, the annotation an unvalidated element carries,
/// so the test holds only for an element that has none.
/// `Some(false)` — `xs:anyType`, which every element type derives from
/// (`xs:untyped` included), so the test holds for every element.
/// `None` — any other type name, which needs a schema (M4).
fn universal_element_type(uri: &str, local: &str) -> Option<bool> {
    if uri != XSD_NS {
        return None;
    }
    match local {
        "untyped" => Some(true),
        "anyType" => Some(false),
        _ => None,
    }
}

/// The attribute counterpart: `xs:untypedAtomic` is the annotation an
/// unvalidated attribute carries, and `xs:anySimpleType` and `xs:anyAtomicType`
/// are supertypes of every attribute type, `xs:untypedAtomic` included.
fn universal_attribute_type(uri: &str, local: &str) -> Option<bool> {
    if uri != XSD_NS {
        return None;
    }
    match local {
        "untypedAtomic" => Some(true),
        "anySimpleType" | "anyAtomicType" => Some(false),
        _ => None,
    }
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
    /// > | `attribute(A, T)` | 0.25 | (matches by name and type) |
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
            // "element(E, T)", "attribute(A, T)" — "matches by name and type".
            NodeTest::TypedElement { name: Some(_), .. }
            | NodeTest::TypedAttribute { name: Some(_), .. } => 0.25,
            // "NCName:*" and "*:NCName".
            NodeTest::NsWildcard(_) | NodeTest::LocalWildcard(_) => -0.25,
            // "document-node(ElementTest)" takes the priority of the test.
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
            (NodeTest::Attribute(Some(q)) | NodeTest::TypedAttribute { name: Some(q), .. }, _) => {
                IndexKey::Attribute(q.clone())
            }
            (NodeTest::NsWildcard(uri), PatternAxis::Child) => IndexKey::Namespace(uri.clone()),
            (NodeTest::LocalWildcard(local), PatternAxis::Child) => IndexKey::Local(local.clone()),
            _ => IndexKey::Other,
        }
    }
}

/// Which bucket of a [`ModeIndex`](crate::model::ModeIndex) a rule goes into.
pub enum IndexKey {
    Element(QName),
    Attribute(QName),
    Namespace(String),
    Local(String),
    Other,
}

// ── Parsing ──────────────────────────────────────────────────────────────

/// What the compiler must supply while a pattern is being read: prefix
/// resolution, the default element namespace, and a way to compile a
/// predicate's text into an expression.
pub trait PatternSink {
    /// Resolves a namespace prefix; `""` asks for the default namespace for
    /// element names (`[xsl:]xpath-default-namespace`).
    fn resolve_prefix(&self, prefix: &str) -> Option<String>;
    /// Compiles a predicate's source text.
    fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError>;
}

/// Parses `source` into a [`Pattern`].
///
/// > \[ERR XTSE0340\] Where an attribute is defined to contain a pattern, it
/// > is a static error if the pattern does not match the production Pattern.
pub fn parse(source: &str, sink: &mut dyn PatternSink) -> Result<Pattern, XsltError> {
    let mut alternatives = Vec::new();
    for alt in split_top_level(source, '|') {
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
    if alt.starts_with("id(") || alt.starts_with("key(") {
        return Err(XsltError::not_implemented(
            format!("the id()/key() pattern {alt:?}"),
            "M2",
        ));
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
        steps.push(parse_step(text, connector, source, sink)?);
    }
    steps.reverse();
    Ok(finish(PathPattern {
        anchor,
        steps,
        priority: 0.0,
    }))
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
    source: &str,
    sink: &mut dyn PatternSink,
) -> Result<PatternStep, XsltError> {
    // PatternAxis
    let (axis, rest) = if let Some(rest) = text.strip_prefix("child::") {
        (PatternAxis::Child, whitespace::trim_start(rest))
    } else if let Some(rest) = text.strip_prefix("attribute::") {
        (PatternAxis::Attribute, whitespace::trim_start(rest))
    } else if let Some(rest) = text.strip_prefix('@') {
        (PatternAxis::Attribute, whitespace::trim_start(rest))
    } else {
        (PatternAxis::Child, text)
    };

    // NodeTest, then the predicate list.
    let (test_text, predicate_text) = split_predicates(rest, source)?;
    let test = parse_node_test(whitespace::trim(&test_text), axis, source, sink)?;
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
            PatternAxis::Child => QName::new(sink.resolve_prefix("").unwrap_or_default(), local),
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
            let is_element = kind == "element";
            let axis = if is_element {
                PatternAxis::Child
            } else {
                PatternAxis::Attribute
            };
            // `element(N, T)` / `attribute(N, T)`: a processor with no schema
            // can decide the type part only for the handful of type names
            // whose answer does not depend on one — `xs:untyped` and
            // `xs:untypedAtomic`, which say the node was never validated, and
            // their supertypes, which every node satisfies.
            let (arg, type_name) = match arg.split_once(',') {
                Some((name, type_name)) => {
                    (whitespace::trim(name), Some(whitespace::trim(type_name)))
                }
                None => (arg, None),
            };
            let untyped_only = match type_name {
                None => None,
                Some(type_name) => {
                    // §6.4: "The presence or absence of the symbol `?`
                    // following a type name does not affect the priority"; nor,
                    // with no schema, does nillability affect what matches.
                    let type_name = whitespace::trim(type_name.trim_end_matches('?'));
                    let (prefix, local) = split_qname(type_name);
                    let uri = sink.resolve_prefix(prefix).unwrap_or_default();
                    let decided = if is_element {
                        universal_element_type(&uri, local)
                    } else {
                        universal_attribute_type(&uri, local)
                    };
                    match decided {
                        Some(untyped_only) => Some(untyped_only),
                        // Every other type name needs a schema (M4).
                        None => {
                            return Err(XsltError::not_implemented(
                                format!(
                                    "the typed node test {kind}({arg}, {type_name}) in a pattern"
                                ),
                                "M4",
                            ))
                        }
                    }
                }
            };
            let name = match arg {
                "" | "*" => None,
                name => {
                    let (prefix, local) = split_qname(name);
                    Some(expand(prefix, local, axis, source, sink)?)
                }
            };
            Ok(match (is_element, untyped_only) {
                (true, None) => NodeTest::Element(name),
                (true, Some(untyped_only)) => NodeTest::TypedElement { name, untyped_only },
                (false, None) => NodeTest::Attribute(name),
                (false, Some(untyped_only)) => NodeTest::TypedAttribute { name, untyped_only },
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
                NodeTest::Element(_) | NodeTest::TypedElement { .. } => {
                    Ok(NodeTest::DocumentNode(Some(Box::new(inner))))
                }
                _ => Err(bad_pattern(
                    source,
                    "document-node() takes an ElementTest or nothing",
                )),
            }
        }
        "schema-element" | "schema-attribute" => Err(XsltError::not_implemented(
            format!("the schema-aware node test {kind}({arg}) in a pattern"),
            "M4",
        )),
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
                _ => None,
            }
        }
        fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError> {
            self.0.push(source.to_string());
            Ok(ExprId(self.0.len() as u32 - 1))
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

    #[test]
    fn id_and_key_anchors_are_not_implemented_yet() {
        let mut sink = Sink(Vec::new());
        assert!(parse("id('x')", &mut sink)
            .unwrap_err()
            .is_not_implemented());
        assert!(parse("key('k', 'v')", &mut sink)
            .unwrap_err()
            .is_not_implemented());
    }
}
