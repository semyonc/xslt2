//! The mode table and the dispatch index (§6.5, §8.5).

use super::Builder;
use crate::model::*;
use crate::pattern::IndexKey;

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    // ── The dispatch index (§8.5) ────────────────────────────────────────

    pub(super) fn build_mode_table(&mut self) -> ModeTable {
        let mut table = ModeTable::default();
        // Every mode a declaration's `mode` attribute names must exist, plus
        // the unnamed one. `#all` contributes no name — §6.5 defines it by
        // reference to the modes named elsewhere — and its rules go into a
        // single index every mode consults instead (see `ModeTable::all_modes`),
        // so a mode named only by an `xsl:apply-templates` still sees them.
        let mut mode_names: Vec<ModeKey> = vec![ModeKey::Default];
        for rule in &self.templates {
            for mode in &rule.modes {
                if !mode_names.contains(mode) {
                    mode_names.push(mode.clone());
                }
            }
        }
        for mode in &mode_names {
            table.modes.insert(mode.clone(), ModeIndex::default());
        }
        for rule in &self.templates {
            let Some(pattern_id) = rule.pattern else {
                continue;
            };
            let pattern = &self.patterns[pattern_id.0 as usize];
            for (index, alternative) in pattern.alternatives.iter().enumerate() {
                // §6.4: each alternative is a template rule of its own, so it
                // gets its own index key.
                let id = RuleId {
                    template: rule.id,
                    alternative: index as u32,
                };
                let key = alternative.index_key();
                if rule.all_modes {
                    push_into(&mut table.all_modes, key, id);
                    continue;
                }
                for mode in &rule.modes {
                    let bucket = table.modes.entry(mode.clone()).or_default();
                    push_into(bucket, alternative.index_key(), id);
                }
            }
        }
        // §6.4: candidates are tried in (precedence, priority, declaration
        // order) order, all descending, so the first match wins.
        let key = |id: &RuleId| {
            let rule = &self.templates[id.template.0 as usize];
            let priority = match rule.explicit_priority {
                Some(p) => p,
                None => match rule.pattern {
                    Some(pattern) => {
                        self.patterns[pattern.0 as usize].alternatives[id.alternative as usize]
                            .priority
                    }
                    None => 0.0,
                },
            };
            (rule.precedence, priority, rule.decl_order)
        };
        let sort = |list: &mut Vec<RuleId>| {
            list.sort_by(|a, b| {
                let (pa, ra, da) = key(a);
                let (pb, rb, db) = key(b);
                pb.cmp(&pa)
                    .then(rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal))
                    .then(db.cmp(&da))
            });
        };
        let sort_index = |index: &mut ModeIndex| {
            for list in index.by_element.values_mut() {
                sort(list);
            }
            for list in index.by_attribute.values_mut() {
                sort(list);
            }
            for list in index.by_namespace.values_mut() {
                sort(list);
            }
            for list in index.by_local.values_mut() {
                sort(list);
            }
            sort(&mut index.other);
        };
        for index in table.modes.values_mut() {
            sort_index(index);
        }
        sort_index(&mut table.all_modes);
        table
    }
}

/// Files one rule into the bucket its index key names (§8.5).
fn push_into(index: &mut ModeIndex, key: IndexKey, id: RuleId) {
    let bucket = match key {
        IndexKey::Element(name) => index.by_element.entry(name).or_default(),
        IndexKey::Attribute(name) => index.by_attribute.entry(name).or_default(),
        IndexKey::Namespace(uri) => index.by_namespace.entry(uri).or_default(),
        IndexKey::Local(local) => index.by_local.entry(local).or_default(),
        IndexKey::Other => &mut index.other,
    };
    if !bucket.contains(&id) {
        bucket.push(id);
    }
}
