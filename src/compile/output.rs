//! `xsl:output` (§20) and `xsl:character-map` (§20.1): the declarations,
//! their static errors, and the merge of each output definition.

use std::collections::{HashMap, HashSet};

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::Builder;
use crate::error::{codes, Location, XsltError};
use crate::names::{resolve_qname, QName, XSLT_NS};
use crate::output::{apply_serialization_attribute, Merge, OutputDefinition};
use crate::receiver::Nav;
use crate::serialize::CharacterMap;
use crate::stylesheet::{attr_local, attributes, Module, ModuleId, StaticContextId};
use crate::whitespace;

/// One `xsl:character-map` declaration, as written.
pub(super) struct CharacterMapDecl {
    name: QName,
    precedence: u32,
    uses: Vec<QName>,
    /// The `xsl:output-character` children, in document order.
    mappings: Vec<(char, String)>,
    location: Location,
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// One `xsl:output` declaration, merged into its output definition (§20).
    pub(super) fn output_declaration(
        &mut self,
        nav: &Nav<'t>,
        ctx: StaticContextId,
        precedence: u32,
        module: &Module<'t>,
    ) -> Result<(), XsltError> {
        let location = module.location(nav);
        let name = self.declared_name(nav, "name", ctx, module)?;
        self.decl_order += 1;
        let order = self.decl_order;
        let sc = self.static_contexts.get(ctx).clone();
        let target = match &name {
            Some(name) => self.named_outputs.entry(name.clone()).or_default(),
            None => &mut self.output,
        };
        let at = |e: XsltError| e.or_location(&location);
        for (uri, local, _, value) in attributes(nav) {
            if !uri.is_empty() {
                continue;
            }
            match local.as_str() {
                "name" => {}
                "use-character-maps" => {
                    let names = whitespace::tokens(&value)
                        .map(|t| resolve_qname(t, |p| sc.resolve_prefix(p), codes::XTSE0020))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(at)?;
                    target.use_character_maps.insert((precedence, order), names);
                }
                // Every other serialization attribute (`version` included,
                // which is also a standard attribute: on `xsl:output` it is the
                // serialization parameter).
                other
                    if apply_serialization_attribute(
                        target,
                        other,
                        &value,
                        &|p| sc.resolve_prefix(p),
                        Merge::Declaration(precedence),
                    )
                    .map_err(at)? => {}
                other if super::attributes::STANDARD_ATTRS.contains(&other) => {}
                // §3.9: "if an element has an attribute that XSLT 2.0 does not
                // allow the element to have, then the attribute must be
                // ignored" where forwards-compatible behaviour is enabled
                // (XSLT 3.0's `build-tree`, `html-version`, …).
                _ if sc.compat.is_forwards() => {}
                other => {
                    return Err(XsltError::new(
                        codes::XTSE0090,
                        format!("xsl:output has no attribute named {other:?}"),
                    )
                    .with_location(location))
                }
            }
        }
        Ok(())
    }

    /// One `xsl:character-map` declaration (§20.1), recorded for
    /// [`Self::finish_output_definitions`].
    pub(super) fn character_map_declaration(
        &mut self,
        nav: &Nav<'t>,
        module_id: ModuleId,
        ctx: StaticContextId,
        precedence: u32,
    ) -> Result<(), XsltError> {
        let module = self.modules.module(module_id);
        let location = module.location(nav);
        let name = self
            .declared_name(nav, "name", ctx, module)?
            .ok_or_else(|| {
                XsltError::new(codes::XTSE0010, "xsl:character-map has no name attribute")
                    .with_location(location.clone())
            })?;
        let uses = match attr_local(nav, "use-character-maps") {
            Some(text) => whitespace::tokens(&text)
                .map(|t| self.resolve(t, ctx, codes::XTSE0020))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.or_location(&location))?,
            None => Vec::new(),
        };
        let mut mappings = Vec::new();
        for child in self.children(nav, module_id) {
            if child.node_type() != DomNodeType::Element
                || child.namespace_uri() != XSLT_NS
                || child.local_name() != "output-character"
            {
                // The content model (`compile/checks.rs`) admits nothing else.
                continue;
            }
            let at = module.location(&child);
            // §20.1: `character = char`. A `char` is one character (§2.10).
            let character = attr_local(&child, "character").unwrap_or_default();
            let mut chars = character.chars();
            let c = match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => {
                    return Err(XsltError::new(
                        codes::XTSE0020,
                        format!(
                            "xsl:output-character/@character must be a single character, not \
                             {character:?}"
                        ),
                    )
                    .with_location(at))
                }
            };
            let string = attr_local(&child, "string").unwrap_or_default();
            mappings.push((c, string));
        }
        self.character_maps.push(CharacterMapDecl {
            name,
            precedence,
            uses,
            mappings,
            location,
        });
        Ok(())
    }

    /// The whole-stylesheet checks and the merge, once every declaration is in:
    /// XTSE1560 per output definition, then the character maps — XTSE1580,
    /// XTSE1590, XTSE1600 — and each output definition's expanded map.
    pub(super) fn finish_output_definitions(&mut self) -> Result<(), XsltError> {
        self.check_output_conflicts()?;
        let maps = resolve_character_maps(&self.character_maps, &self.output, &self.named_outputs)?;
        self.output.character_map = expand_for(&self.output, &maps);
        let names: Vec<QName> = self.named_outputs.keys().cloned().collect();
        for name in names {
            let map = expand_for(&self.named_outputs[&name], &maps);
            self.named_outputs
                .get_mut(&name)
                .expect("listed above")
                .character_map = map;
        }
        self.expanded_character_maps = maps;
        Ok(())
    }

    /// > \[ERR XTSE1560\] It is a static error if two `xsl:output` declarations
    /// > within an output definition specify explicit values for the same
    /// > attribute (other than `cdata-section-elements` and
    /// > `use-character-maps`), with the values of the attributes being not
    /// > equal, unless there is another `xsl:output` declaration within the same
    /// > output definition that has higher import precedence and that specifies
    /// > an explicit value for the same attribute.
    fn check_output_conflicts(&self) -> Result<(), XsltError> {
        let named: Vec<(Option<&QName>, &OutputDefinition)> = std::iter::once((None, &self.output))
            .chain(self.named_outputs.iter().map(|(n, d)| (Some(n), d)))
            .collect();
        for (name, definition) in named {
            if let Some(attribute) = definition.conflicting_attribute() {
                let which = match name {
                    Some(name) => format!("the output definition named {name}"),
                    None => "the unnamed output definition".to_string(),
                };
                return Err(XsltError::new(
                    codes::XTSE1560,
                    format!(
                        "two xsl:output declarations of {which} give different values for \
                         {attribute} at the same import precedence"
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// The winning declaration of every character-map name, expanded (§20.1).
///
/// > When a character map is used by an output definition or another character
/// > map, the character map with the highest import precedence is used.
/// > \[ERR XTSE1580\] It is a static error if the stylesheet contains two or
/// > more character maps with the same name and the same import precedence,
/// > unless it also contains another character map with the same name and
/// > higher import precedence. … \[ERR XTSE1590\] It is a static error if a
/// > name in the use-character-maps attribute of the `xsl:output` or
/// > `xsl:character-map` elements does not match the name attribute of any
/// > `xsl:character-map` in the stylesheet. \[ERR XTSE1600\] It is a static
/// > error if a character map references itself, directly or indirectly, via a
/// > name in the use-character-maps attribute. It is not an error if the same
/// > character map is referenced more than once, directly or indirectly.
fn resolve_character_maps(
    decls: &[CharacterMapDecl],
    unnamed: &OutputDefinition,
    named: &HashMap<QName, OutputDefinition>,
) -> Result<HashMap<QName, CharacterMap>, XsltError> {
    // The winner of each name: highest precedence; a tie there is XTSE1580.
    let mut winners: HashMap<&QName, &CharacterMapDecl> = HashMap::new();
    for decl in decls {
        match winners.get(&decl.name) {
            Some(best) if best.precedence > decl.precedence => {}
            _ => {
                winners.insert(&decl.name, decl);
            }
        }
    }
    for decl in decls {
        let best = winners[&decl.name];
        if !std::ptr::eq(best, decl) && best.precedence == decl.precedence {
            return Err(XsltError::new(
                codes::XTSE1580,
                format!(
                    "the character map {} is declared twice with the same import precedence",
                    decl.name
                ),
            )
            .with_location(decl.location.clone()));
        }
    }
    // XTSE1590 for every reference, from losing declarations too: the rule
    // speaks of "the use-character-maps attribute of the xsl:output or
    // xsl:character-map elements".
    let missing = |name: &QName| -> XsltError {
        XsltError::new(
            codes::XTSE1590,
            format!("use-character-maps names {name}, which no xsl:character-map declares"),
        )
    };
    for decl in decls {
        if let Some(name) = decl.uses.iter().find(|n| !winners.contains_key(n)) {
            return Err(missing(name).with_location(decl.location.clone()));
        }
    }
    for definition in std::iter::once(unnamed).chain(named.values()) {
        if let Some(name) = definition
            .character_map_names()
            .find(|n| !winners.contains_key(n))
        {
            return Err(missing(name));
        }
    }
    // XTSE1600: a depth-first walk over the winners' references.
    fn visit<'d>(
        name: &'d QName,
        winners: &HashMap<&'d QName, &'d CharacterMapDecl>,
        on_path: &mut Vec<&'d QName>,
        done: &mut HashSet<&'d QName>,
    ) -> Result<(), XsltError> {
        if done.contains(name) {
            return Ok(());
        }
        if on_path.contains(&name) {
            let decl = winners[name];
            return Err(XsltError::new(
                codes::XTSE1600,
                format!("the character map {name} references itself through use-character-maps"),
            )
            .with_location(decl.location.clone()));
        }
        on_path.push(name);
        for used in &winners[name].uses {
            visit(used, winners, on_path, done)?;
        }
        on_path.pop();
        done.insert(name);
        Ok(())
    }
    let mut done = HashSet::new();
    for name in winners.keys() {
        visit(name, &winners, &mut Vec::new(), &mut done)?;
    }
    // Expansion: "Within a single xsl:character-map element, the characters
    // defined in character maps referenced in the use-character-maps attribute
    // are considered before the characters defined in the child
    // xsl:output-character elements. The character maps referenced in a single
    // use-character-maps attribute are considered in the order in which they
    // are listed in that attribute. The expansion is depth-first: each
    // referenced character map is fully expanded before the next one is
    // considered. Two xsl:output-character elements appearing as children of
    // the same xsl:character-map element are considered in document order."
    // With "the last character mapping takes precedence", that is an insertion
    // in that order. No cycle is left, so the recursion ends.
    fn expand(
        name: &QName,
        winners: &HashMap<&QName, &CharacterMapDecl>,
        memo: &mut HashMap<QName, CharacterMap>,
    ) -> CharacterMap {
        if let Some(map) = memo.get(name) {
            return map.clone();
        }
        let decl = winners[name];
        let mut map = CharacterMap::default();
        for used in &decl.uses {
            map.extend_from(&expand(used, winners, memo));
        }
        for (c, s) in &decl.mappings {
            map.insert(*c, s.clone());
        }
        memo.insert(name.clone(), map.clone());
        map
    }
    let mut memo = HashMap::new();
    for name in winners.keys() {
        expand(name, &winners, &mut memo);
    }
    Ok(memo)
}

/// An output definition's character map: its `use-character-maps` names in
/// §20's concatenation order, each fully expanded, later ones taking
/// precedence.
fn expand_for(definition: &OutputDefinition, maps: &HashMap<QName, CharacterMap>) -> CharacterMap {
    let mut map = CharacterMap::default();
    for name in definition.character_map_names() {
        if let Some(expanded) = maps.get(name) {
            map.extend_from(expanded);
        }
    }
    map
}
