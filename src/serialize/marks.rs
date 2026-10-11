//! `disable-output-escaping` as a property of the final result tree (§20.2).
//!
//! > Conceptually, the final result tree provides an additional boolean
//! > property disable-escaping associated with every character in a text node.
//! > When this property is set, the normal action of the serializer to escape
//! > special characters such as & and < is suppressed.
//!
//! The result tree is a `BufferDocument`, which has no room for such a
//! property, so it is kept beside the tree: the character ranges, per text node,
//! that were written with escaping disabled. A text node is identified by its
//! ordinal among the text nodes of the document in document order — the order
//! the tree builder creates them in and the order the serializer meets them in
//! — so nothing has to be stored in the tree, and the tree every other reader
//! sees (`xsl:copy-of`, a host walking the result) is the plain one.

/// The `disable-output-escaping` ranges of one final result tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EscapingMarks {
    /// `(text node ordinal, byte start, byte end)`, sorted, non-overlapping
    /// and non-adjacent within a node.
    ranges: Vec<(u32, u32, u32)>,
}

impl EscapingMarks {
    /// Records that bytes `start..end` of the text node with this ordinal were
    /// written with escaping disabled. Ranges must arrive in document order.
    pub fn push(&mut self, ordinal: u32, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let (start, end) = (start as u32, end as u32);
        if let Some(last) = self.ranges.last_mut() {
            debug_assert!((last.0, last.2) <= (ordinal, start), "marks out of order");
            if last.0 == ordinal && last.2 == start {
                last.2 = end;
                return;
            }
        }
        self.ranges.push((ordinal, start, end));
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The byte ranges of text node `ordinal` that have escaping disabled.
    pub fn ranges_for(&self, ordinal: u32) -> impl Iterator<Item = (usize, usize)> + '_ {
        let first = self.ranges.partition_point(|r| r.0 < ordinal);
        self.ranges[first..]
            .iter()
            .take_while(move |r| r.0 == ordinal)
            .map(|r| (r.1 as usize, r.2 as usize))
    }

    /// `text` (text node `ordinal`) split into `(segment, escaping disabled)`
    /// runs, in order.
    pub fn segments<'s>(&self, ordinal: u32, text: &'s str) -> Vec<(&'s str, bool)> {
        let mut out = Vec::new();
        let mut at = 0;
        for (start, end) in self.ranges_for(ordinal) {
            let (start, end) = (start.min(text.len()), end.min(text.len()));
            if start > at {
                out.push((&text[at..start], false));
            }
            if end > start {
                out.push((&text[start..end], true));
            }
            at = end.max(at);
        }
        if at < text.len() || out.is_empty() {
            out.push((&text[at..], false));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_ranges_merge_and_lookup_is_per_node() {
        let mut m = EscapingMarks::default();
        m.push(0, 2, 4);
        m.push(0, 4, 6);
        m.push(3, 0, 1);
        assert_eq!(m.ranges_for(0).collect::<Vec<_>>(), [(2, 6)]);
        assert_eq!(m.ranges_for(1).count(), 0);
        assert_eq!(m.ranges_for(3).collect::<Vec<_>>(), [(0, 1)]);
        assert_eq!(
            m.segments(0, "ab<x>cd"),
            [("ab", false), ("<x>c", true), ("d", false)]
        );
        assert_eq!(m.segments(2, "plain"), [("plain", false)]);
    }
}
