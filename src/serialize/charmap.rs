//! A character map (§20.1, Serialization §9): characters and the strings that
//! replace them during serialization.

use std::collections::HashMap;

/// The mappings an output definition passes to the serializer, after the
/// recursive expansion of `use-character-maps` (§20.1).
///
/// > An output definition, after recursive expansion of character maps
/// > referenced via its `use-character-maps` attribute, may contain several
/// > mappings for the same character. In this situation, the last character
/// > mapping takes precedence.
///
/// The map holds the mapping that wins for each character.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharacterMap {
    map: HashMap<char, String>,
}

impl CharacterMap {
    /// Maps `c` to `string`, replacing an earlier mapping of `c`.
    pub(crate) fn insert(&mut self, c: char, string: impl Into<String>) {
        self.map.insert(c, string.into());
    }

    /// Adds every mapping of `other`, which take precedence over these.
    pub(crate) fn extend_from(&mut self, other: &CharacterMap) {
        for (c, s) in &other.map {
            self.map.insert(*c, s.clone());
        }
    }

    /// The string `c` is replaced by, if it is mapped.
    pub fn get(&self, c: char) -> Option<&str> {
        self.map.get(&c).map(String::as_str)
    }

    /// Whether the map maps no character.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// How many characters the map maps.
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_mapping_wins() {
        let mut a = CharacterMap::default();
        a.insert('c', "[C]");
        let mut b = CharacterMap::default();
        b.insert('c', "{C}");
        b.insert('y', "{Y}");
        a.extend_from(&b);
        assert_eq!(a.get('c'), Some("{C}"));
        assert_eq!(a.get('y'), Some("{Y}"));
        assert_eq!(a.get('x'), None);
        assert_eq!(a.len(), 2);
    }
}
