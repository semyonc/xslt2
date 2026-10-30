//! Compiling a `match` pattern (§5.5.2) against its own static context.

use std::collections::HashSet;

use xsd_schema::namespace::{NameTable, QualifiedName};
use xsd_schema::types::{ItemType, NameTest, TypeTest};
use xsd_schema::xpath::XPathExpr;
use xsd_schema::{ElementKey, NameId, SchemaSet, XmlTypeCode};

use super::Builder;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::{QName, FN_NS, XSD_NS};
use crate::pattern::{self, PatternSink, SchemaTest, TestKind};
use crate::stylesheet::StaticContextId;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── Patterns ─────────────────────────────────────────────────────────

    pub(super) fn compile_pattern(
        &mut self,
        source: &str,
        ctx: StaticContextId,
    ) -> Result<PatternId, XsltError> {
        // Hand the whole pattern to the XPath parser first: a pattern is a
        // restriction of the path syntax, so anything the parser rejects as
        // *syntax* is XTSE0340 too — §5.5.2: "it is a static error if the
        // pattern does not match the production Pattern".
        //
        // A name the expression cannot resolve is a different error, and
        // keeps the XPath code. §5.3: it is a static error if an expression
        // "fails to satisfy other static constraints defined in the XPath
        // specification, for example that all variable references must refer
        // to variables that are in scope. Error codes are defined in
        // [XPath 2.0]" — and every pattern is an expression. That reading
        // needs the string to *be* a pattern, though: `$x` or `foo()` is not
        // one, whatever else is wrong with it, so it stays XTSE0340.
        //
        // An unknown function is XPST0017 here like anywhere else; only with
        // backwards-compatible behaviour, and only outside the standard
        // function namespace, does the context's catalog bind the call to
        // §3.8's fallback error function instead (`context_of`), so that it
        // compiles and raises XTDE1425 only if it is evaluated.
        let visible = self.visible_vars(ctx);
        let names: Vec<&str> = visible.iter().map(|(n, _)| n.as_str()).collect();
        match XPathExpr::compile_with_vars(source, self.compile_context(ctx), &names) {
            Err(e) => {
                return Err(match e.error_code() {
                    Some(code @ ("XPST0008" | "XPST0017" | "XPST0051" | "XPST0081"))
                        if has_pattern_shape(source) =>
                    {
                        XsltError::new(code, format!("cannot compile the pattern {source:?}: {e}"))
                    }
                    _ => {
                        XsltError::new(codes::XTSE0340, format!("{source:?} is not a pattern: {e}"))
                    }
                });
            }
            Ok(whole) => reject_grouping_functions(&whole, source)?,
        }

        let sc = self.static_contexts.get(ctx).clone();
        let mut sink = Sink {
            builder: self,
            ctx,
            sc,
        };
        let pattern = pattern::parse(source, &mut sink)?;
        self.patterns.push(pattern);
        Ok(PatternId(self.patterns.len() as u32 - 1))
    }
}

/// The two §14 functions that have no meaning in a pattern.
///
/// > \[§14.1\] \[ERR XTSE1060\] It is a static error if the current-group
/// > function is used within a pattern.
///
/// > \[§14.2\] \[ERR XTSE1070\] It is a static error if the
/// > current-grouping-key function is used within a pattern.
///
/// `whole` is the pattern compiled as one expression; its function calls are
/// every call anywhere in the pattern (`XPathExpr::function_calls`), in the
/// predicates of any step and in the argument of an `id()` / `key()` anchor.
/// A call inside the body of a stylesheet function the pattern calls is not
/// "used within a pattern": it is in the function's body.
fn reject_grouping_functions(whole: &XPathExpr, source: &str) -> Result<(), XsltError> {
    for call in whole.function_calls() {
        if call.namespace != FN_NS {
            continue;
        }
        let code = match call.local_name.as_str() {
            "current-group" => codes::XTSE1060,
            "current-grouping-key" => codes::XTSE1070,
            _ => continue,
        };
        return Err(XsltError::new(
            code,
            format!(
                "{}() is not allowed in the pattern {source:?}",
                call.local_name
            ),
        ));
    }
    Ok(())
}

/// Whether `source` matches the production Pattern, judged on its shape alone.
///
/// Asked only after the whole string has parsed as an XPath expression and
/// failed on a *name*, so the expressions inside its predicates are known to be
/// syntactically sound; what is left to decide is whether the outer structure
/// is a Pattern (§5.5.2). No prefix is resolved and no predicate is compiled.
/// A construct of the Pattern grammar that this processor does not implement
/// is still a Pattern (none is left: the typed node tests work).
fn has_pattern_shape(source: &str) -> bool {
    struct ShapeOnly;
    impl PatternSink for ShapeOnly {
        fn resolve_prefix(&self, _prefix: &str) -> Option<String> {
            Some(String::new())
        }
        fn compile_predicate(&mut self, _source: &str) -> Result<ExprId, XsltError> {
            Ok(ExprId(0))
        }
        // Nothing is resolved: whether a name is in scope is not a question
        // of shape.
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
    match pattern::parse(source, &mut ShapeOnly) {
        Ok(_) => true,
        Err(e) => e.is_not_implemented(),
    }
}

/// The [`PatternSink`] a pattern is read through: it resolves prefixes in the
/// pattern's own static context and compiles each predicate as an expression.
struct Sink<'b, 'a, 'm, 't> {
    builder: &'b mut Builder<'a, 'm, 't>,
    ctx: StaticContextId,
    sc: crate::stylesheet::StaticContext,
}

impl PatternSink for Sink<'_, '_, '_, '_> {
    fn resolve_prefix(&self, prefix: &str) -> Option<String> {
        if prefix.is_empty() {
            return Some(self.sc.xpath_default_namespace.clone());
        }
        self.sc.resolve_prefix(prefix)
    }

    fn compile_predicate(&mut self, source: &str) -> Result<ExprId, XsltError> {
        // §5.3's forwards-compatible exception is for expressions; a pattern
        // that does not match the production Pattern is XTSE0340 wherever it
        // is written (§5.5.2).
        self.builder.compile_expr_strictly(source, self.ctx)
    }

    /// The test as `xsd-schema`'s binder resolves a kind test
    /// (`xpath::node_test`, `resolve_type_test`): a TypeName in the XML Schema
    /// namespace is `xs:untyped` or a predefined type by its code — the
    /// predefined types of every static context (XPath 2.0 §2.5.1), a basic
    /// processor's included (§3.13) —, any other the key of a type of the
    /// stylesheet's schema set (§3.13's in-scope schema types).
    /// The names are interned in the stylesheet's name table, which in
    /// schema-aware mode is the set's own, so the set's lookups and
    /// the matcher read them alike.
    ///
    /// The whole pattern went through the binder first (`compile_pattern`),
    /// which raises XPST0008 for a type name outside the in-scope schema
    /// types; the same check here keeps the sink honest on its own.
    fn type_test(
        &self,
        kind: TestKind,
        name: Option<&QName>,
        type_name: &QName,
        nillable: bool,
    ) -> Result<ItemType, XsltError> {
        let names = self.builder.names;
        let name_test = Some(match name {
            None => NameTest::Wildcard,
            Some(name) => NameTest::QName(qualified(names, name)),
        });
        let undeclared = || {
            XsltError::new(
                "XPST0008",
                format!(
                    "the type Q{{{}}}{} is not in the in-scope schema types",
                    type_name.uri, type_name.local
                ),
            )
        };
        let type_test = if type_name.uri == XSD_NS {
            if type_name.local == "untyped" {
                TypeTest::Untyped
            } else {
                XmlTypeCode::from_local_name(&type_name.local)
                    .map(TypeTest::BuiltIn)
                    .ok_or_else(undeclared)?
            }
        } else {
            let set = self.builder.schema.ok_or_else(undeclared)?;
            let qname = qualified(names, type_name);
            set.lookup_type(qname.namespace_uri, qname.local_name)
                .map(TypeTest::Schema)
                .ok_or_else(undeclared)?
        };
        Ok(match kind {
            TestKind::Element => ItemType::element(name_test, Some(type_test), nillable),
            TestKind::Attribute => ItemType::attribute(name_test, Some(type_test)),
        })
    }

    /// `schema-element(N)` / `schema-attribute(N)` against the in-scope
    /// element / attribute declarations — the global declarations of the
    /// stylesheet's schema set (§3.13); a basic processor has none.
    fn schema_test(&self, kind: TestKind, name: &QName) -> Result<SchemaTest, XsltError> {
        let names = self.builder.names;
        let undeclared = || {
            let what = match kind {
                TestKind::Element => "element",
                TestKind::Attribute => "attribute",
            };
            XsltError::new(
                "XPST0008",
                format!(
                    "Q{{{}}}{} is not in the in-scope {what} declarations",
                    name.uri, name.local
                ),
            )
        };
        let set = self.builder.schema.ok_or_else(undeclared)?;
        let qname = qualified(names, name);
        Ok(match kind {
            TestKind::Element => {
                let head = set
                    .lookup_element(qname.namespace_uri, qname.local_name)
                    .ok_or_else(undeclared)?;
                SchemaTest {
                    test: ItemType::SchemaElement(qname),
                    members: substitution_group_members(set, head),
                }
            }
            TestKind::Attribute => {
                set.lookup_attribute(qname.namespace_uri, qname.local_name)
                    .ok_or_else(undeclared)?;
                SchemaTest {
                    test: ItemType::SchemaAttribute(qname),
                    members: Vec::new(),
                }
            }
        })
    }
}

/// `name` interned in `names`: no namespace is `None`, as the schema set
/// keys it.
fn qualified(names: &NameTable, name: &QName) -> QualifiedName {
    let uri = (!name.uri.is_empty()).then(|| names.add(&name.uri));
    QualifiedName::new(uri, names.add(&name.local), None)
}

/// The expanded names of the global element declarations in the substitution
/// group headed by `head`, at any depth, `head` itself excluded.
///
/// XPath 2.0 §2.5.4.4: a SchemaElementTest matches a candidate whose name
/// "matches the specified ElementName or matches the name of an element in a
/// substitution group headed by an element named ElementName". The dispatch
/// index files a template rule by the names its last step can
/// match, so it needs these names; whether a node with one of them matches is
/// still decided by `xsd-schema`'s matcher. A member is a global declaration
/// whose substitution-group affiliations (`resolved_substitution_groups`,
/// several in XSD 1.1) reach `head`, directly or through other members — the
/// relation `xsd-schema` tests for a candidate (`is_substitution_group_member`
/// walks a candidate's affiliations up to the head), here walked down from
/// the head over every global declaration.
fn substitution_group_members(set: &SchemaSet, head: ElementKey) -> Vec<QName> {
    let globals: Vec<(Option<NameId>, NameId, ElementKey)> = set
        .namespaces
        .iter()
        .flat_map(|(ns, table)| {
            table
                .elements
                .iter()
                .map(move |(local, key)| (*ns, *local, *key))
        })
        .collect();
    let mut group: HashSet<ElementKey> = HashSet::from([head]);
    let mut members = Vec::new();
    loop {
        let before = group.len();
        for &(ns, local, key) in &globals {
            if group.contains(&key) {
                continue;
            }
            let affiliated = set.arenas.elements.get(key).is_some_and(|decl| {
                decl.resolved_substitution_groups
                    .iter()
                    .any(|h| group.contains(h))
            });
            if affiliated {
                group.insert(key);
                let resolve = |id: NameId| {
                    set.name_table
                        .try_resolve_ref(id)
                        .unwrap_or_default()
                        .to_string()
                };
                members.push(QName::new(
                    ns.map(resolve).unwrap_or_default(),
                    resolve(local),
                ));
            }
        }
        if group.len() == before {
            return members;
        }
    }
}
