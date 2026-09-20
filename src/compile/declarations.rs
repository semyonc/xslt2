//! The stylesheet declarations of §7.3: global variables and parameters,
//! template rules, `xsl:output`, `xsl:strip-space`, `xsl:namespace-alias` and
//! `xsl:attribute-set`, plus the two passes that walk the module graph.

use std::collections::HashMap;

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::attributes::{check_attributes, STANDARD_ATTRS};
use super::Alias;
use super::Builder;
use super::ScopeEntry;
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::names::{parse_yes_no, split_qname, QName, XSLT_NS};
use crate::output::parse_method;
use crate::receiver::Nav;
use crate::stylesheet::{
    attr_local, attributes, is_stripped_stylesheet_text, DeclKind, Module, ModuleId,
    StaticContextId,
};
use crate::whitespace;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// §3.7: a simplified stylesheet module is one template rule matching `/`
    /// whose body is the literal result element.
    ///
    /// > The literal result element is treated as if it were the child of an
    /// > `xsl:template` element with a `match` attribute of `/`.
    fn simplified_template(
        &mut self,
        module_id: ModuleId,
        root: &Nav<'t>,
        precedence: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(root, module)?;
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let instruction = self.literal_element(root, self.modules.module(module_id), ctx)?;
        self.scope.truncate(scope_mark);
        let pattern = self.compile_pattern("/", ctx)?;
        self.decl_order += 1;
        let id = TemplateId(self.templates.len() as u32);
        let loc = self.modules.module(module_id).location(root);
        self.templates.push(TemplateRule {
            id,
            name: None,
            pattern: Some(pattern),
            modes: vec![ModeKey::Default],
            all_modes: false,
            explicit_priority: None,
            precedence,
            subtree_min: precedence,
            decl_order: self.decl_order,
            params: Vec::new(),
            body: Body(vec![instruction]),
            as_type: None,
            frame_size: self.frame_size,
            module: module_id,
            loc,
        });
        Ok(())
    }

    /// The two passes over the top-level declarations of every module.
    ///
    /// §3.12 needs no check here: `Unit::decls` is filled by the loader through
    /// [`Module::children`](crate::stylesheet::Module::children), so an excluded
    /// declaration never reaches it — which is also why the "unimplemented
    /// declaration" pass below could already do without one.
    pub(super) fn build_declarations(&mut self) -> Result<(), XsltError> {
        // Pass 1: the global variables and parameters, so that every
        // expression can be compiled against a complete global scope.
        let mut global_decls: Vec<(u32, ModuleId, Nav<'t>)> = Vec::new();
        for unit in &self.modules.units {
            for (module_id, nav, kind) in &unit.decls {
                if *kind != DeclKind::Declaration || nav.namespace_uri() != XSLT_NS {
                    continue;
                }
                if matches!(nav.local_name(), "variable" | "param") {
                    global_decls.push((unit.precedence, *module_id, nav.clone()));
                }
            }
        }

        // A declaration M1 does not implement is reported before any template
        // body is compiled: otherwise a template calling a function that
        // `xsl:function` would have declared fails first, and the case is
        // classified as a wrong answer rather than a missing feature.
        for unit in &self.modules.units {
            for (module_id, nav, kind) in &unit.decls {
                if *kind != DeclKind::Declaration || nav.namespace_uri() != XSLT_NS {
                    continue;
                }
                if let Some(milestone) = unimplemented_declaration(nav.local_name()) {
                    return Err(XsltError::not_implemented(
                        format!("xsl:{}", nav.local_name()),
                        milestone,
                    )
                    .with_location(self.modules.module(*module_id).location(nav)));
                }
            }
        }
        self.declare_globals(&global_decls)?;
        // §11.1.4 is a whole-stylesheet mapping resolved by import precedence,
        // so it is settled before the first literal result element compiles.
        self.collect_aliases()?;

        // Pass 2: everything, now that names resolve.
        for unit_index in 0..self.modules.units.len() {
            let precedence = self.modules.units[unit_index].precedence;
            let subtree_min = self.modules.units[unit_index].subtree_min;
            let decls: Vec<(ModuleId, Nav<'t>, DeclKind)> =
                self.modules.units[unit_index].decls.clone();
            for (module_id, nav, kind) in decls {
                let module = self.modules.module(module_id);
                if kind == DeclKind::SimplifiedRoot {
                    self.simplified_template(module_id, &nav, precedence)?;
                    continue;
                }
                if nav.namespace_uri() != XSLT_NS {
                    // §3.6.2: a top-level element in a non-null namespace that
                    // is not the XSLT namespace is user data and is ignored.
                    if nav.namespace_uri().is_empty() {
                        return Err(XsltError::new(
                            codes::XTSE0130,
                            format!(
                                "the top-level element {} is in no namespace; user-defined \
                                 top-level elements must be in a namespace",
                                nav.local_name()
                            ),
                        )
                        .with_location(module.location(&nav)));
                    }
                    continue;
                }
                self.declaration(&nav, module_id, precedence, subtree_min)?;
            }
        }
        Ok(())
    }
    // ── Declarations (§7.3) ──────────────────────────────────────────────

    fn declare_globals(&mut self, decls: &[(u32, ModuleId, Nav<'t>)]) -> Result<(), XsltError> {
        // Highest precedence wins; two at the top precedence is XTSE0630.
        let mut best: HashMap<QName, (u32, usize)> = HashMap::new();
        let mut entries: Vec<(QName, u32, ModuleId, Nav<'t>)> = Vec::new();
        for (precedence, module_id, nav) in decls {
            let module = self.modules.module(*module_id);
            let ctx = self.context_of(nav, module)?;
            let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0010,
                    format!("xsl:{} has no name attribute", nav.local_name()),
                )
                .with_location(module.location(nav))
            })?;
            entries.push((name, *precedence, *module_id, nav.clone()));
        }
        for (index, (name, precedence, _, nav)) in entries.iter().enumerate() {
            match best.get(name) {
                Some((existing, _)) if *existing > *precedence => {}
                Some((existing, _)) if *existing == *precedence => {
                    return Err(XsltError::new(
                        codes::XTSE0630,
                        format!(
                            "the global variable {name} is declared twice with the same import \
                             precedence"
                        ),
                    )
                    .with_location(self.modules.module(self.modules.principal).location(nav)));
                }
                _ => {
                    best.insert(name.clone(), (*precedence, index));
                }
            }
        }

        // Bind the winning declarations, in declaration order, then compile
        // their values with every global already in scope.
        let mut winners: Vec<usize> = best.values().map(|(_, i)| *i).collect();
        winners.sort_unstable();
        for (slot, index) in winners.iter().enumerate() {
            let (name, _, _, _) = &entries[*index];
            self.global_index.insert(name.clone(), slot);
            self.scope.push(ScopeEntry {
                name: name.clone(),
                location: VarLocation::Global(slot),
            });
        }
        for index in winners {
            let (name, precedence, module_id, nav) = &entries[index];
            let module = self.modules.module(*module_id);
            let ctx = self.context_of(nav, module)?;
            let is_param = nav.local_name() == "param";
            self.frame_size = 0;
            let as_type = match attr_local(nav, "as") {
                Some(text) => Some(self.compile_type(&text, ctx)?),
                None => None,
            };
            let value = self.var_value(nav, *module_id, ctx, as_type.is_some())?;
            let required = attr_local(nav, "required")
                .map(|v| parse_yes_no(&v, "required"))
                .transpose()?
                .unwrap_or(false);
            self.globals.push(GlobalVar {
                name: name.clone(),
                is_param,
                required,
                value,
                as_type,
                precedence: *precedence,
                frame_size: self.frame_size,
                loc: module.location(nav),
            });
        }
        Ok(())
    }

    fn declaration(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        precedence: u32,
        subtree_min: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        let local = nav.local_name().to_string();
        if !self.static_contexts.get(ctx).compat.is_forwards() {
            check_attributes(nav, &local, &module.location(nav))?;
        }
        match local.as_str() {
            "template" => self.template(nav, module_id, ctx, precedence, subtree_min),
            "variable" | "param" => Ok(()), // handled in pass 1
            "output" => self.output_declaration(nav, ctx, precedence, module),
            "strip-space" | "preserve-space" => {
                self.space_declaration(nav, ctx, precedence, module)
            }
            "attribute-set" => self.attribute_set(nav, module_id, ctx, precedence),
            // Collected by `collect_aliases` before this pass; only the
            // attribute check above still applies here.
            "namespace-alias" => Ok(()),
            name if unimplemented_declaration(name).is_some() => Err(XsltError::not_implemented(
                format!("xsl:{name}"),
                unimplemented_declaration(name).expect("checked"),
            )
            .with_location(module.location(nav))),
            other => {
                // §3.9: in forwards-compatible mode an unknown XSLT
                // declaration is ignored.
                if self.static_contexts.get(ctx).compat.is_forwards() {
                    Ok(())
                } else {
                    Err(XsltError::new(
                        codes::XTSE0010,
                        format!("xsl:{other} is not a stylesheet declaration"),
                    )
                    .with_location(module.location(nav)))
                }
            }
        }
    }

    fn output_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let name = self.qname_attr(nav, "name", ctx, module)?;
        let target = match &name {
            Some(name) => self.named_outputs.entry(name.clone()).or_default(),
            None => &mut self.output,
        };
        for (uri, local, _, value) in attributes(nav) {
            if !uri.is_empty() {
                continue;
            }
            match local.as_str() {
                "name" => {}
                "method" => target.method.set(parse_method(&value)?, precedence),
                "version" => target.version.set(value, precedence),
                "encoding" => target.encoding.set(value, precedence),
                "indent" => target
                    .indent
                    .set(parse_yes_no(&value, "indent")?, precedence),
                "omit-xml-declaration" => target
                    .omit_xml_declaration
                    .set(parse_yes_no(&value, "omit-xml-declaration")?, precedence),
                "standalone" => {
                    let value = match whitespace::trim(&value) {
                        "omit" => None,
                        other => Some(parse_yes_no(other, "standalone")?),
                    };
                    target.standalone.set(value, precedence);
                }
                "doctype-public" => target.doctype_public.set(value, precedence),
                "doctype-system" => target.doctype_system.set(value, precedence),
                "media-type" => target.media_type.set(value, precedence),
                // Serialization parameters M1 does not act on but must accept.
                "cdata-section-elements"
                | "escape-uri-attributes"
                | "include-content-type"
                | "normalization-form"
                | "undeclare-prefixes"
                | "use-character-maps"
                | "byte-order-mark" => {}
                other if STANDARD_ATTRS.contains(&other) => {}
                other => {
                    return Err(XsltError::new(
                        codes::XTSE0090,
                        format!("xsl:output has no attribute named {other:?}"),
                    )
                    .with_location(module.location(nav)))
                }
            }
        }
        Ok(())
    }

    fn space_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let strip = nav.local_name() == "strip-space";
        let elements = attr_local(nav, "elements").ok_or_else(|| {
            XsltError::new(
                codes::XTSE0010,
                format!("xsl:{} has no elements attribute", nav.local_name()),
            )
            .with_location(module.location(nav))
        })?;
        let sc = self.static_contexts.get(ctx).clone();
        for token in whitespace::tokens(&elements) {
            let test = if token == "*" {
                SpaceTest::Any
            } else if let Some(prefix) = token.strip_suffix(":*") {
                SpaceTest::Namespace(sc.resolve_prefix(prefix).ok_or_else(|| {
                    XsltError::new(
                        codes::XTSE0280,
                        format!("the prefix {prefix:?} in xsl:strip-space is not bound"),
                    )
                })?)
            } else if let Some(local) = token.strip_prefix("*:") {
                SpaceTest::LocalName(local.to_string())
            } else {
                let (prefix, local) = split_qname(token);
                let uri = if prefix.is_empty() {
                    // §4.4: an unprefixed name test uses the default namespace
                    // for elements, i.e. `xpath-default-namespace`.
                    sc.xpath_default_namespace.clone()
                } else {
                    sc.resolve_prefix(prefix).ok_or_else(|| {
                        XsltError::new(
                            codes::XTSE0280,
                            format!("the prefix {prefix:?} in xsl:strip-space is not bound"),
                        )
                    })?
                };
                SpaceTest::Name(QName::new(uri, local))
            };
            let priority = test.priority();
            self.decl_order += 1;
            self.space_rules.push(SpaceRule {
                test,
                strip,
                precedence,
                priority,
                decl_order: self.decl_order,
            });
        }
        Ok(())
    }

    /// §11.1.4 `xsl:namespace-alias`.
    ///
    /// Two rules the engine used to miss, both of them about what a prefix is
    /// allowed to be:
    ///
    /// > The default namespace (as declared by `xmlns`) may be specified by
    /// > using `#default` instead of a prefix. **If no default namespace is in
    /// > force, specifying `#default` denotes the null namespace URI.** This
    /// > allows elements that are in no namespace in the stylesheet to acquire
    /// > a namespace in the result document, or vice versa.
    /// >
    /// > \[ERR XTSE0812\] It is a static error if a value **other than
    /// > `#default`** is specified for either the `stylesheet-prefix` or the
    /// > `result-prefix` attributes of the `xsl:namespace-alias` element when
    /// > there is no in-scope binding for that namespace prefix.
    ///
    /// So `#default` is never an error — it simply denotes the null namespace
    /// URI when nothing is bound — and a real prefix that is unbound is
    /// XTSE0812, not the generic XTSE0280 of §5.1.
    ///
    /// > If a literal namespace URI is declared to be an alias for multiple
    /// > different target namespace URIs, then the declaration with the highest
    /// > import precedence is used.
    /// >
    /// > \[ERR XTSE0810\] It is a static error if there is more than one such
    /// > declaration with the same literal namespace URI and the same import
    /// > precedence and different values for the target namespace URI, unless
    /// > there is also an `xsl:namespace-alias` declaration with the same
    /// > literal namespace URI and a higher import precedence.
    fn namespace_alias(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        module: &Module<'t>,
        precedence: u32,
    ) -> Result<(String, Alias), XsltError> {
        let sc = self.static_contexts.get(ctx).clone();
        let resolve = |attr: &str| -> Result<(String, String), XsltError> {
            let token = attr_local(nav, attr).ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0010,
                    format!("xsl:namespace-alias has no {attr} attribute"),
                )
                .with_location(module.location(nav))
            })?;
            let token = whitespace::trim(&token);
            if token == "#default" {
                // The null namespace URI is a legitimate answer here.
                return Ok((sc.resolve_prefix("").unwrap_or_default(), String::new()));
            }
            let uri = sc.resolve_prefix(token).ok_or_else(|| {
                XsltError::new(
                    codes::XTSE0812,
                    format!("the prefix {token:?} in xsl:namespace-alias/@{attr} is not bound"),
                )
                .with_location(module.location(nav))
            })?;
            Ok((uri, token.to_string()))
        };
        let (literal, _) = resolve("stylesheet-prefix")?;
        let (target, target_prefix) = resolve("result-prefix")?;
        Ok((
            literal,
            Alias {
                uri: target,
                prefix: target_prefix,
                precedence,
            },
        ))
    }

    /// The `xsl:namespace-alias` pass (§11.1.4).
    ///
    /// It runs **before** any literal result element is compiled, and over the
    /// whole module graph, for two reasons. First, the aliasing of a literal
    /// namespace URI is a property of the stylesheet, not of the module the
    /// declaration sits in, so a declaration in the principal module has to
    /// reach a literal result element in an included one — with the alias
    /// collected in declaration order, `namespace-alias-4201` and `bug-0303`
    /// saw the literal URI reach the output. Second, XTSE0810 is only an error
    /// when no declaration of *higher* precedence exists for the same literal
    /// URI, which cannot be decided until every declaration has been seen.
    fn collect_aliases(&mut self) -> Result<(), XsltError> {
        // One entry per declaration, so the winner can be picked afterwards.
        let mut declared: Vec<(String, Alias)> = Vec::new();
        for unit_index in 0..self.modules.units.len() {
            let precedence = self.modules.units[unit_index].precedence;
            let decls: Vec<(ModuleId, Nav<'t>, DeclKind)> =
                self.modules.units[unit_index].decls.clone();
            for (module_id, nav, kind) in decls {
                if kind != DeclKind::Declaration
                    || nav.namespace_uri() != XSLT_NS
                    || nav.local_name() != "namespace-alias"
                {
                    continue;
                }
                let module = self.modules.module(module_id);
                let ctx = self.context_of(&nav, module)?;
                let module = self.modules.module(module_id);
                declared.push(self.namespace_alias(&nav, ctx, module, precedence)?);
            }
        }
        // Per literal namespace URI: the highest precedence wins, and only at
        // *that* precedence is a disagreement an error — "unless there is also
        // an xsl:namespace-alias declaration with the same literal namespace
        // URI and a higher import precedence". So the maximum has to be known
        // before any conflict is judged; two declarations that disagree are
        // legal as long as something above them overrides both.
        let mut highest: HashMap<&str, u32> = HashMap::new();
        for (literal, alias) in &declared {
            let slot = highest.entry(literal.as_str()).or_insert(alias.precedence);
            *slot = (*slot).max(alias.precedence);
        }
        let highest: HashMap<String, u32> = highest
            .into_iter()
            .map(|(uri, precedence)| (uri.to_string(), precedence))
            .collect();
        for (literal, alias) in declared {
            if highest.get(&literal).copied() != Some(alias.precedence) {
                continue;
            }
            if let Some(won) = self.aliases.get(&literal) {
                if won.uri != alias.uri {
                    return Err(XsltError::new(
                        codes::XTSE0810,
                        format!(
                            "two xsl:namespace-alias declarations of the same import precedence \
                             alias {literal:?} to {:?} and {:?}",
                            won.uri, alias.uri
                        ),
                    ));
                }
            }
            self.aliases.insert(literal, alias);
        }
        Ok(())
    }

    fn attribute_set(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        precedence: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:attribute-set has no name attribute")
                    .with_location(module.location(nav))
            })?;
        let uses = self.qname_list(nav, "use-attribute-sets", ctx, module)?;
        self.frame_size = 0;
        let scope_mark = self.scope.len();
        let mut attributes = Vec::new();
        for child in self.children(nav, module_id) {
            if child.node_type() == DomNodeType::Element {
                if child.namespace_uri() == XSLT_NS && child.local_name() == "attribute" {
                    attributes.push(self.instruction(&child, module_id)?);
                } else {
                    return Err(XsltError::new(
                        codes::XTSE0010,
                        "the content of xsl:attribute-set must be xsl:attribute elements only",
                    )
                    .with_location(self.modules.module(module_id).location(&child)));
                }
            }
        }
        self.scope.truncate(scope_mark);
        let _ = precedence;
        let entry = self
            .attribute_sets
            .entry(name.clone())
            .or_insert_with(|| AttributeSet {
                name,
                use_attribute_sets: Vec::new(),
                attributes: Vec::new(),
            });
        entry.use_attribute_sets.extend(uses);
        entry.attributes.extend(attributes);
        Ok(())
    }

    fn template(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        precedence: u32,
        subtree_min: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let loc = module.location(nav);
        let name = self.declared_name(nav, "name", ctx, module)?;
        let match_text = attr_local(nav, "match");
        if name.is_none() && match_text.is_none() {
            return Err(XsltError::new(
                codes::XTSE0500,
                "xsl:template must have a match attribute, a name attribute, or both",
            )
            .with_location(loc));
        }

        self.frame_size = 0;
        let scope_mark = self.scope.len();

        // The pattern is compiled with globals only in scope (§9.7).
        let pattern = match &match_text {
            Some(text) => Some(self.compile_pattern(text, ctx)?),
            None => None,
        };

        let (modes, all_modes) = self.modes_of(nav, ctx, module, pattern.is_some())?;
        // §6.4: the `priority` attribute belongs to the declaration and so
        // applies to every alternative of the pattern. Without it, each
        // alternative keeps the default priority `pattern::PathPattern`
        // computed from its own syntax, and conflict resolution treats the
        // alternatives as separate rules.
        let explicit_priority = match attr_local(nav, "priority") {
            Some(text) => Some(whitespace::trim(&text).parse::<f64>().map_err(|_| {
                XsltError::new(
                    codes::XTSE0530,
                    format!("the priority {text:?} is not a number"),
                )
                .with_location(loc.clone())
            })?),
            None => None,
        };
        let as_type = match attr_local(nav, "as") {
            Some(text) => Some(self.compile_type(&text, ctx)?),
            None => None,
        };

        let (params, body) = self.template_body(nav, module_id)?;
        self.scope.truncate(scope_mark);

        self.decl_order += 1;
        let id = TemplateId(self.templates.len() as u32);
        if let Some(name) = &name {
            match self.named.get(name) {
                Some((_, existing)) if *existing > precedence => {}
                Some((_, existing)) if *existing == precedence => {
                    return Err(XsltError::new(
                        codes::XTSE0660,
                        format!("two templates are named {name} with the same import precedence"),
                    )
                    .with_location(loc));
                }
                _ => {
                    self.named.insert(name.clone(), (id, precedence));
                }
            }
        }
        self.templates.push(TemplateRule {
            id,
            name,
            pattern,
            modes,
            all_modes,
            explicit_priority,
            precedence,
            subtree_min,
            decl_order: self.decl_order,
            params,
            body,
            as_type,
            frame_size: self.frame_size,
            module: module_id,
            loc,
        });
        Ok(())
    }

    /// The `mode` attribute of `xsl:template` (§6.5).
    fn modes_of(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        module: &Module<'t>,
        has_match: bool,
    ) -> Result<(Vec<ModeKey>, bool), XsltError> {
        let Some(text) = attr_local(nav, "mode") else {
            return Ok((
                if has_match {
                    vec![ModeKey::Default]
                } else {
                    Vec::new()
                },
                false,
            ));
        };
        if !has_match {
            return Err(XsltError::new(
                codes::XTSE0500,
                "xsl:template has a mode attribute but no match attribute",
            )
            .with_location(module.location(nav)));
        }
        let mut modes = Vec::new();
        let mut all = false;
        for token in whitespace::tokens(&text) {
            match token {
                "#all" => all = true,
                "#default" => modes.push(ModeKey::Default),
                name => modes.push(ModeKey::Named(self.resolve(name, ctx, codes::XTSE0550)?)),
            }
        }
        if all && modes.len() > 1 {
            return Err(XsltError::new(
                codes::XTSE0550,
                "mode=\"#all\" cannot be combined with other mode names",
            )
            .with_location(module.location(nav)));
        }
        if modes.is_empty() && !all {
            modes.push(ModeKey::Default);
        }
        Ok((modes, all))
    }

    /// The `xsl:param` children of a template, then its sequence constructor.
    fn template_body(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
    ) -> Result<(Vec<ParamDecl>, Body), XsltError> {
        let mut params = Vec::new();
        let mut first_non_param: Option<Nav<'t>> = None;
        for child in self.children(nav, module_id) {
            match child.node_type() {
                DomNodeType::Element => {
                    let is_param =
                        child.namespace_uri() == XSLT_NS && child.local_name() == "param";
                    if is_param && first_non_param.is_none() {
                        params.push(self.param_decl(&child, module_id, false)?);
                    } else {
                        if first_non_param.is_none() {
                            first_non_param = Some(child.clone());
                        }
                        if is_param {
                            let module = self.modules.module(module_id);
                            return Err(XsltError::new(
                                codes::XTSE0010,
                                "xsl:param must come before any other content of a template",
                            )
                            .with_location(module.location(&child)));
                        }
                    }
                }
                // Whitespace before the first instruction is stripped;
                // significant text opens the sequence constructor.
                kind if kind.is_text_like()
                    && first_non_param.is_none()
                    && !is_stripped_stylesheet_text(&child) =>
                {
                    first_non_param = Some(child.clone());
                }
                _ => {}
            }
        }
        let body = self.sequence_constructor_skipping(nav, module_id, "param")?;
        Ok((params, body))
    }

    fn param_decl(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        global: bool,
    ) -> Result<ParamDecl, XsltError> {
        let module = self.modules.module(module_id);
        let ctx = self.context_of(nav, module)?;
        let name = self.qname_attr(nav, "name", ctx, module)?.ok_or_else(|| {
            XsltError::new(codes::XTSE0010, "xsl:param has no name attribute")
                .with_location(module.location(nav))
        })?;
        let as_type = match attr_local(nav, "as") {
            Some(text) => Some(self.compile_type(&text, ctx)?),
            None => None,
        };
        let required = attr_local(nav, "required")
            .map(|v| parse_yes_no(&v, "required"))
            .transpose()?
            .unwrap_or(false);
        let tunnel = attr_local(nav, "tunnel")
            .map(|v| parse_yes_no(&v, "tunnel"))
            .transpose()?
            .unwrap_or(false);
        let value = self.var_value(nav, module_id, ctx, as_type.is_some())?;
        // §10.1.1: "It is a static error if a parameter that is declared as
        // required also specifies a default value."
        if required && !matches!(value, VarValue::Empty) {
            return Err(XsltError::new(
                codes::XTSE0010,
                format!("the required parameter {name} must not have a default value"),
            )
            .with_location(module.location(nav)));
        }
        let slot = self.frame_size;
        self.frame_size += 1;
        if !global {
            self.scope.push(ScopeEntry {
                name: name.clone(),
                location: VarLocation::Local(slot),
            });
        }
        Ok(ParamDecl {
            name,
            slot,
            value,
            as_type,
            required,
            tunnel,
            loc: module.location(nav),
        })
    }

    /// `select` xor content, the shape every variable-binding element shares.
    ///
    /// > \[ERR XTSE0620\] It is a static error if a variable-binding element
    /// > has a select attribute and has non-empty content.
    pub(super) fn var_value(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        has_as: bool,
    ) -> Result<VarValue, XsltError> {
        let select = attr_local(nav, "select");
        let has_content = self.has_content(nav, module_id);
        if select.is_some() && has_content {
            let module = self.modules.module(module_id);
            return Err(XsltError::new(
                codes::XTSE0620,
                format!(
                    "xsl:{} has both a select attribute and content",
                    nav.local_name()
                ),
            )
            .with_location(module.location(nav)));
        }
        Ok(match select {
            Some(source) => VarValue::Select(self.compile_expr(&source, ctx)?),
            None if has_content => {
                let body = self.sequence_constructor(nav, module_id)?;
                if has_as {
                    VarValue::Sequence(body)
                } else {
                    VarValue::Tree { body, ctx }
                }
            }
            None => VarValue::Empty,
        })
    }
}

/// The stylesheet declarations M1 parses but does not implement, and the
/// milestone that owns each.
fn unimplemented_declaration(local: &str) -> Option<&'static str> {
    Some(match local {
        "key" | "function" => "M2",
        "decimal-format" | "character-map" => "M3",
        "import-schema" => "M4",
        _ => return None,
    })
}
