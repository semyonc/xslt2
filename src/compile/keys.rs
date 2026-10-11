//! `xsl:key` (§16.3.1): one declaration at a time, merged by name into the
//! key table.

use super::checks::visit;
use super::Builder;
use crate::collation::uca_not_implemented;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::FN_NS;
use crate::receiver::Nav;
use crate::stylesheet::{attr_local, ModuleId, StaticContextId};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Compiles one `xsl:key` declaration and adds it to the key of its name.
    ///
    /// > \[Definition: A key is defined as a set of `xsl:key` declarations in the
    /// > stylesheet that share the same name.\]
    ///
    /// Import precedence plays no part: "all the `xsl:key` declarations in the
    /// stylesheet are effective regardless of their import precedence".
    pub(super) fn key_declaration(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        // `name` and `match` are required by the element table (XTSE0010), so
        // their absence never reaches this point; the errors are a safety net.
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:key has no name attribute")
                    .with_location(loc.clone())
            })?;
        let match_text = attr_local(nav, "match").ok_or_else(|| {
            XsltError::new(codes::XTSE0010, "xsl:key has no match attribute")
                .with_location(loc.clone())
        })?;

        // > [ERR XTSE1205] It is a static error if an xsl:key declaration has a
        // > use attribute and has non-empty content, or if it has empty content
        // > and no use attribute.
        let use_text = attr_local(nav, "use");
        let has_content = self.has_content(nav, module_id);
        if use_text.is_some() == has_content {
            return Err(XsltError::new(
                codes::XTSE1205,
                if has_content {
                    format!("xsl:key {name} has both a use attribute and content")
                } else {
                    format!("xsl:key {name} has neither a use attribute nor content")
                },
            )
            .with_location(loc));
        }

        let collation = self.key_collation(nav, ctx, &loc)?;

        // DESIGN §9.1: the pattern and the key specifier see the global
        // variables only — a top-level declaration has no other variables in
        // scope, and the sequence constructor's own locals live in a frame of
        // their own.
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let pattern = self.compile_pattern(&match_text, ctx)?;
        // Every expression of the key specifier is compiled from here on, and
        // only those: expressions are numbered in the order they are compiled.
        let first_expr = self.exprs.len();
        let specifier = match use_text {
            Some(text) => ValueSource::Select(self.compile_expr(&text, ctx)?),
            None => ValueSource::Content(self.sequence_constructor(nav, module_id)?),
        };
        self.scope.truncate(scope_mark);
        let group_reach = self.group_reach(&specifier, first_expr);
        let definition = KeyDefinition {
            pattern,
            specifier,
            frame_size: self.frame_size,
            group_reach,
            loc: loc.clone(),
        };
        let compat = self.static_contexts.get(ctx).compat.is_backwards();

        match self.key_index.get(&name) {
            Some(id) => {
                let key = &mut self.keys[id.0 as usize];
                // > [ERR XTSE1220] It is a static error if there are several
                // > xsl:key declarations in the stylesheet with the same key
                // > name and different effective collations. Two collations are
                // > the same if their URIs are equal under the rules for
                // > comparing xs:anyURI values …
                if key.collation != collation {
                    return Err(XsltError::new(
                        codes::XTSE1220,
                        format!(
                            "the declarations of the key {name} have different effective \
                             collations: {:?} and {collation:?}",
                            key.collation
                        ),
                    )
                    .with_location(loc));
                }
                key.compat |= compat;
                key.definitions.push(definition);
            }
            None => {
                let id = KeyId(self.keys.len() as u32);
                self.key_index.insert(name.clone(), id);
                self.keys.push(Key {
                    name,
                    definitions: vec![definition],
                    collation,
                    compat,
                    // Settled by `settle_key_group_dependence` once every
                    // key is known.
                    observes_group: false,
                    observes_captures: false,
                });
            }
        }
        Ok(())
    }

    /// What the key specifier just compiled may observe of the current group
    /// of the `key()` call it is evaluated for (see [`GroupReach`]).
    ///
    /// `first_expr` is the number of the specifier's first expression: every
    /// expression from there to the last one compiled belongs to it — the
    /// `use` attribute, or every expression, attribute value template, sort
    /// key and type of its sequence constructor, at any depth. A function call
    /// is seen by its expanded name (`XPathExpr::function_calls`, the name the
    /// call was bound to): `current-group()` and `current-grouping-key()`
    /// read the group; `regex-group()` reads the current captured substrings
    /// (§15.2); `key()` evaluates another specifier with both. A call to a
    /// stylesheet function is none of these: §14.3 "During the evaluation of a
    /// stylesheet function, the current group and current grouping key are set
    /// to the empty sequence", and §15.2 empties the captured substrings
    /// during "a stylesheet function" too.
    ///
    /// The instructions of a sequence constructor that run other code with the
    /// caller's current group are the template invocations and the attribute
    /// sets (`instruction_reaches_group`); a template called from a key
    /// specifier may read the group and the captured substrings, so such a
    /// specifier counts as reading both. Global variables do not: they are
    /// evaluated with neither.
    ///
    /// The test is by name and conservative: a `regex-group()` call inside an
    /// `xsl:analyze-string` of the specifier itself reads that instruction's
    /// own substrings, not the caller's, but still counts.
    fn group_reach(&self, specifier: &ValueSource, first_expr: usize) -> GroupReach {
        let mut reach = GroupReach::default();
        for compiled in &self.exprs[first_expr..] {
            for call in compiled.expr.function_calls() {
                if call.namespace != FN_NS {
                    continue;
                }
                match call.local_name.as_str() {
                    "current-group" | "current-grouping-key" => reach.reads_group = true,
                    "regex-group" => reach.reads_captures = true,
                    "key" => reach.calls_key = true,
                    _ => {}
                }
            }
        }
        if let ValueSource::Content(body) = specifier {
            visit(body, &mut |instruction| {
                if instruction_reaches_group(&instruction.kind) {
                    // A template or an attribute set runs with the caller's
                    // current group *and* captured substrings (§15.2:
                    // "unaffected through calls of xsl:apply-templates,
                    // xsl:call-template, xsl:apply-imports or xsl:next-match,
                    // or by expansion of named attribute sets").
                    reach.reads_group = true;
                    reach.reads_captures = true;
                }
            });
        }
        reach
    }

    /// Decides, once every `xsl:key` declaration is compiled, which keys may
    /// observe the current group of the `key()` call ([`Key::observes_group`]).
    ///
    /// A key does if one of its declarations reads the group itself, or calls
    /// `key()` while some key of the stylesheet reads the group: the name a
    /// `key()` call asks for is an expression, so any key may be reached, and
    /// through any chain of `key()` calls — every chain that reaches the group
    /// ends at a key that reads it.
    ///
    /// The current captured substrings (§15.2) are settled the same way, into
    /// [`Key::observes_captures`].
    pub(super) fn settle_key_group_dependence(&mut self) {
        let definitions = || self.keys.iter().flat_map(|key| &key.definitions);
        let any_reads_group = definitions().any(|d| d.group_reach.reads_group);
        let any_reads_captures = definitions().any(|d| d.group_reach.reads_captures);
        for key in &mut self.keys {
            key.observes_group = key.definitions.iter().any(|definition| {
                definition.group_reach.reads_group
                    || (any_reads_group && definition.group_reach.calls_key)
            });
            key.observes_captures = key.definitions.iter().any(|definition| {
                definition.group_reach.reads_captures
                    || (any_reads_captures && definition.group_reach.calls_key)
            });
        }
    }

    /// The effective collation of one `xsl:key` declaration (§16.3.1).
    ///
    /// > The effective collation for an `xsl:key` declaration is the collation
    /// > specified in its `collation` attribute if present, resolved against
    /// > the base URI of the `xsl:key` element, or the default collation that
    /// > is in scope for the `xsl:key` declaration otherwise
    ///
    /// > \[ERR XTSE1210\] It is a static error if the `xsl:key` declaration has
    /// > a `collation` attribute whose value (after resolving against the base
    /// > URI) is not a URI recognized by the implementation as referring to a
    /// > collation.
    fn key_collation(
        &self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        loc: &crate::error::Location,
    ) -> Result<String, XsltError> {
        let sc = self.static_contexts.get(ctx);
        let Some(value) = attr_local(nav, "collation") else {
            return Ok(sc.default_collation.clone());
        };
        let uri = crate::resolver::resolve_uri(&sc.base_uri, whitespace::trim(&value));
        if self.functions.collations().recognizes(&uri) {
            return Ok(uri);
        }
        if let Some(missing) = uca_not_implemented(&uri) {
            return Err(missing.with_location(loc.clone()));
        }
        Err(XsltError::new(
            codes::XTSE1210,
            format!("xsl:key names the collation {uri:?}, which this processor does not recognize"),
        )
        .with_location(loc.clone()))
    }
}

/// Whether `kind` runs code other than its own expressions and sequence
/// constructors with the caller's current group and captured substrings: a
/// template invocation (§5.4.4 clears the current group for "calls on
/// stylesheet functions" only, so a template sees it; §15.2 leaves the
/// captured substrings "unaffected through calls of xsl:apply-templates,
/// xsl:call-template, xsl:apply-imports or xsl:next-match"), or the expansion
/// of attribute sets (evaluated in the caller's context, `runtime/eval.rs`).
/// Everything else an instruction evaluates is an expression or a sequence
/// constructor of its own, which [`Builder::group_reach`] sees anyway — the
/// bodies of `xsl:analyze-string` included.
///
/// Exhaustive on purpose: a new instruction has to be classified here.
fn instruction_reaches_group(kind: &InstructionKind) -> bool {
    match kind {
        InstructionKind::ApplyTemplates { .. }
        | InstructionKind::ApplyImports { .. }
        | InstructionKind::NextMatch { .. }
        | InstructionKind::CallTemplate { .. } => true,
        InstructionKind::LiteralElement { attr_sets, .. }
        | InstructionKind::Element { attr_sets, .. }
        | InstructionKind::Copy { attr_sets, .. } => !attr_sets.is_empty(),
        InstructionKind::ForEach { .. }
        | InstructionKind::ForEachGroup { .. }
        | InstructionKind::AnalyzeString { .. }
        | InstructionKind::PerformSort { .. }
        | InstructionKind::If { .. }
        | InstructionKind::Choose { .. }
        | InstructionKind::ValueOf { .. }
        | InstructionKind::Text { .. }
        | InstructionKind::Sequence { .. }
        | InstructionKind::Attribute { .. }
        | InstructionKind::Namespace { .. }
        | InstructionKind::Comment { .. }
        | InstructionKind::ProcessingInstruction { .. }
        | InstructionKind::Document { .. }
        | InstructionKind::CopyOf { .. }
        | InstructionKind::Variable { .. }
        | InstructionKind::Message { .. }
        | InstructionKind::Fallback
        | InstructionKind::NotImplemented { .. } => false,
    }
}
