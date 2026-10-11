//! The search behind the substring functions under a UCA collation —
//! [`IcuCollation`](super::IcuCollation)'s `find`, `starts_with` and
//! `ends_with` (F&O 2.0 §7.5). What is matched (collation-equal substrings at
//! character boundaries, the leftmost DS4-minimal match) is described in the
//! `uca.rs` module docs, "Substring matching"; this module finds the same
//! matches with far less work.
//!
//! # Why comparing every candidate is too slow
//!
//! ICU4X 1.5 offers `compare` and nothing else, so a match is found by
//! comparing candidates `H[i..j]` of the haystack with the needle `N`. There
//! are *n*(*n* + 1)/2 candidates, and one comparison reads both strings up to
//! their first primary difference, which for a candidate that shares a long
//! prefix with the needle is the length of that prefix: the work is cubic. A
//! haystack of 2,000 × `a` searched for 1,000 × `a` + `b` took 21.5 s in a
//! release build. Four facts make the search exact and fast. **W1** below is
//! the sequence of non-zero primary weights of a string's collation
//! elements, after variable weighting.
//!
//! * **Equality implies W1 equality.** At any strength, two strings the
//!   collator calls equal have equal primary weights, so `W1(H[i..j]) =
//!   W1(N)` is a necessary condition. The same tailoring at primary strength
//!   without a case level (`P`) compares W1 and nothing else.
//!
//! * **The data says which characters are context-free.** [`classify`]
//!   reads the collation data ICU4X compares with — the tailoring's and the
//!   root's CE32 tries and their contraction and prefix tries (ICU4X 1.5's
//!   public `provider` structs, in ICU's CE32 format), the diacritics table —
//!   and the canonical decompositions, and calls a character
//!   **context-free** when the collation elements it produces cannot depend
//!   on the characters around it and it changes none of theirs; for those it
//!   counts the collation elements with a non-zero primary weight. It follows
//!   what ICU4X does: contractions are matched over the canonically ordered
//!   text with discontiguous skips (here any non-starter may be skipped,
//!   which over-approximates), and a prefix context is matched against a
//!   two-character buffer of what came before. A character that may take
//!   part in a contraction with its neighbours or be read by a prefix
//!   context, a digit under `numeric=yes`, a conjoining jamo, a non-starter
//!   with a primary weight (canonical ordering could move it), or anything
//!   the data does not make plain is not context-free. The classification
//!   only errs on that side: a character it cannot prove context-free costs
//!   speed, never correctness.
//!
//! * **U+FFFF has the greatest primary weight.** UTS #35 Part 5: "U+FFFF:
//!   This code point is tailored to have a primary weight higher than all
//!   other characters. This allows the reliable specification of a range,
//!   such as “Sch” ≤ X ≤ “Sch\uFFFF”, to include all strings starting with
//!   "sch" or equivalent", and "U+FFFD..U+FFFF are not further tailorable,
//!   and nothing can tailor to them" — so no contraction contains it either.
//!   ICU4X 1.5's data agrees: U+FFFF's CE32 is `FFFF0505`, and at primary
//!   strength every other code point sorts below it in 78 tailorings,
//!   shifted and not, numeric and not, with no expansion or offset weight
//!   reaching it (`XSLT2_M2K_REVIEW2_FIXES.md`; `SearchData::new` re-checks
//!   the CE32, and the contraction walks check that U+FFFF continues none).
//!   So for a string `A` and a needle without U+FFFF, W1(`A`) is a prefix of
//!   W1(`N`) exactly when `P` finds `A ≤ N` and `N < A + U+FFFF`.
//!
//! * **Context-free characters add up.** A boundary before character `k` is
//!   **safe** when the characters on both sides are context-free and `k` is
//!   a starter: then W1(`H[i..j]`) = W1(`H[i..k]`) + W1(`H[k..j]`) for every
//!   `i < k ≤ j`.
//!
//! ## 1. Filters (every character of both strings context-free)
//!
//! With at most one primary weight per character, W1 of a candidate is the
//! sequence of its characters' primary weights, and two characters have the
//! same one exactly when `P` calls them equal. So the candidates with W1 =
//! W1(`N`) are exactly the windows in which the `P`-classes of the haystack's
//! primary-bearing characters spell the needle's: KMP over class numbers
//! finds them in O(*n* + *m*) ([`Filter`]). Each window fixes the
//! primary-bearing characters of a match; only its start (among the
//! primary-ignorable characters before the first of them) and its end (among
//! those after the last) are open, and only those candidates are compared
//! with the full collator. Every match lies in such a window, and a smaller
//! match inside a match lies in the same window, so the leftmost minimal
//! match of the first window that has one is the answer.
//!
//! With expansions (`ß`, ligatures, Hangul syllables: several primary
//! weights per character) and no variable weighting, W1 of a candidate has
//! exactly as many weights as its characters count between them, so only
//! the ends whose count equals the needle's are compared ([`Counted`]).
//!
//! ## 2. The pruned search (any other strings)
//!
//! Scanning the ends of a start in turn, at each safe boundary `k` the search
//! asks `P` whether W1(`H[i..k]`) is still a prefix of W1(`N`); once it is
//! not, no longer candidate from that start can match, and the scan stops
//! there ([`Exhaustive`] with its safe boundaries). A start in ordinary text
//! dies after a character or two, so a search costs about *n* comparisons
//! instead of *n*²/2. A long stretch without a safe boundary — a run of
//! characters chained by contractions or prefix contexts, such as Danish
//! `aaaa…` or Japanese `カーカー…` — is still searched candidate by
//! candidate.
//!
//! ## 3. A work budget
//!
//! Each comparison is charged the length of its candidate (ICU4X reads the
//! two strings in step and stops at the first difference, so about as much
//! of the needle as of the candidate). A search that would exceed [`BUDGET`]
//! — about a second in a release build, reached only by the candidate-by-
//! candidate search over long dependent stretches — stops, and the caller
//! raises FOCH0004. F&O 2.0 §7.5: "It is possible to define collations that
//! do not have the ability to decompose a string into units suitable for
//! substring matching. … Such a collation may cause the function to fail, or
//! to give unexpected results or it may be rejected as an unsuitable
//! argument. The ability to decompose strings into collation units is an
//! implementation-defined property of the collation", and "If the specified
//! collation does not support collation units an error may be raised
//! \[err:FOCH0004\]". It is never a wrong answer: a search either finishes
//! exactly or raises the error.
//!
//! [`classify`]: SearchData::classify

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashMap;

use icu_collator::provider::{
    Baked, CollationDataV1, CollationDataV1Marker, CollationDiacriticsV1Marker,
    CollationMetadataV1Marker,
};
use icu_collator::{
    AlternateHandling, Collator, CollatorOptions, Numeric, ResolvedCollatorOptions,
};
use icu_collections::char16trie::{Char16TrieIterator, TrieResult};
use icu_normalizer::properties::CanonicalCombiningClassMap;
use icu_normalizer::DecomposingNormalizer;
use icu_provider::{DataLocale, DataPayload, DataProvider, DataRequest, KeyedDataMarker};

/// The work a search may do before it gives up with FOCH0004, in the units of
/// `Search::charge`: the characters of each candidate compared, plus 8.
/// Calibrated on the slowest inputs measured (long runs of Vietnamese
/// vowels with two marks, chained contractions) so that a search gives up
/// after at most about a second in a release build; ordinary text of
/// 100,000 characters uses a small fraction of it
/// (`XSLT2_M2K_REVIEW2_FIXES.md`).
pub(super) const BUDGET: usize = 1 << 24;

/// The character with the greatest primary weight (module docs).
const SENTINEL: char = '\u{FFFF}';

// The CE32 format (ICU4C `collation.h`; ICU4X 1.5 `elements.rs`, `Tag`): a
// CE32 is special when its low byte is 0xC0 or more, and then its low four
// bits are the tag.
const SPECIAL_LOW_BYTE: u32 = 0xC0;
/// "Fall back to the base collator": the tailoring has no entry.
const FALLBACK_CE32: u32 = SPECIAL_LOW_BYTE;
const TAG_LONG_PRIMARY: u32 = 1;
const TAG_LONG_SECONDARY: u32 = 2;
const TAG_EXPANSION32: u32 = 5;
const TAG_EXPANSION: u32 = 6;
const TAG_PREFIX: u32 = 8;
const TAG_CONTRACTION: u32 = 9;
const TAG_DIGIT: u32 = 10;
const TAG_OFFSET: u32 = 14;
const TAG_IMPLICIT: u32 = 15;
/// `CollationMetadataV1` bit: the locale has its own tailoring data
/// (ICU4X 1.5 `provider.rs`, `TAILORED_MASK`).
const METADATA_TAILORED: u32 = 1 << 3;
/// `CollationMetadataV1` bit: the locale has its own diacritics table
/// (`TAILORED_DIACRITICS_MASK`).
const METADATA_TAILORED_DIACRITICS: u32 = 1 << 4;
/// The first combining diacritic ICU4X weighs from its diacritics table
/// rather than from the trie (`COMBINING_DIACRITICS_BASE`).
const DIACRITICS_BASE: u32 = 0x0300;
/// The most alternative paths followed through a contraction trie before a
/// walk gives up and treats the rest of the string as dependent.
const MAX_WALK_STATES: usize = 64;

fn is_special(ce32: u32) -> bool {
    ce32 & 0xFF >= SPECIAL_LOW_BYTE
}

fn tag(ce32: u32) -> u32 {
    ce32 & 0xF
}

/// A conjoining jamo: ICU4X reads these from its own jamo table, not from
/// the trie, so the data here says nothing about them.
fn is_jamo(c: char) -> bool {
    ('\u{1100}'..='\u{11FF}').contains(&c)
}

fn is_hangul_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// What the collation data of one collator says, loaded once per collator
/// (`IcuCollation::search_data`).
pub(super) struct SearchData {
    root: DataPayload<CollationDataV1Marker>,
    /// The locale's own data, when its metadata says it has some — the same
    /// choice `Collator::try_new` makes.
    tailoring: Option<DataPayload<CollationDataV1Marker>>,
    /// Digits form numbers (`numeric=yes`).
    numeric: bool,
    /// `alternate=shifted`: variable primary weights drop out of W1, so a
    /// count of a character's primary weights is only an upper bound.
    shifted: bool,
    /// The same tailoring at primary strength without a case level (`P`).
    primary: Collator,
    nfd: DecomposingNormalizer,
    ccc: CanonicalCombiningClassMap,
    /// U+FFFF has its documented CE32 here, so the pruning may use it.
    sentinel: bool,
    /// The length of the diacritics table: ICU4X gives the combining
    /// diacritics from U+0300 on that it covers a secondary weight from the
    /// table and never looks them up in the trie (where the root data leaves
    /// them out: their trie value is the implicit tag).
    diacritics: u32,
}

fn load<M: KeyedDataMarker>(locale: &DataLocale) -> Option<DataPayload<M>>
where
    Baked: DataProvider<M>,
{
    DataProvider::<M>::load(
        &Baked,
        DataRequest {
            locale,
            metadata: Default::default(),
        },
    )
    .ok()?
    .take_payload()
    .ok()
}

/// One character of a classified string.
#[derive(Clone, Copy, Debug)]
pub(super) struct Unit {
    /// Its canonical decomposition starts with a starter (ccc 0).
    pub(super) starter: bool,
    /// `Some(k)` when it is context-free (module docs), `k` being the number
    /// of its collation elements with a non-zero primary weight (variable
    /// ones included: an upper bound under `alternate=shifted`); `None`
    /// otherwise.
    pub(super) primaries: Option<u32>,
}

/// A character's canonical decomposition, with the combining class of each
/// character of it.
struct Decomposed {
    chars: Vec<char>,
    ccc: Vec<u8>,
}

/// How a contraction starting at one character resolves.
enum Walked {
    /// The characters up to and including this index may take part in it.
    Zone(usize),
    /// Nothing after the starter can continue it: the default CE32.
    Default(u32),
    /// The starter's own (single) combining mark completes it: this CE32.
    Joined(u32),
}

impl SearchData {
    /// The data of the collator for `locale`; `primary` are its options with
    /// primary strength and no case level, `full` the options the collator
    /// resolved. `None` when the data cannot be loaded (never with compiled
    /// data): the search then runs unpruned.
    pub(super) fn new(
        locale: &DataLocale,
        primary: CollatorOptions,
        full: ResolvedCollatorOptions,
    ) -> Option<Self> {
        let metadata = load::<CollationMetadataV1Marker>(locale)?;
        let tailoring = if metadata.get().bits & METADATA_TAILORED != 0 {
            Some(load::<CollationDataV1Marker>(locale)?)
        } else {
            None
        };
        let root_locale = DataLocale::default();
        let diacritics_locale = if metadata.get().bits & METADATA_TAILORED_DIACRITICS != 0 {
            locale
        } else {
            &root_locale
        };
        let diacritics = load::<CollationDiacriticsV1Marker>(diacritics_locale)?
            .get()
            .secondaries
            .len() as u32;
        let mut data = SearchData {
            root: load::<CollationDataV1Marker>(&root_locale)?,
            tailoring,
            numeric: full.numeric == Numeric::On,
            shifted: full.alternate_handling == AlternateHandling::Shifted,
            primary: Collator::try_new(locale, primary).ok()?,
            nfd: DecomposingNormalizer::new_nfd(),
            ccc: CanonicalCombiningClassMap::new(),
            sentinel: false,
            diacritics,
        };
        // A plain CE32 whose primary weight is FFFF, the greatest there is.
        let (ce32, _) = data.ce32(SENTINEL);
        data.sentinel = !is_special(ce32) && ce32 & 0xFFFF_0000 == 0xFFFF_0000;
        Some(data)
    }

    /// Whether the pruning may use U+FFFF (module docs).
    #[cfg(test)]
    pub(super) fn sentinel_usable(&self) -> bool {
        self.sentinel
    }

    /// `P`'s comparison.
    #[cfg(test)]
    pub(super) fn primary_compare(&self, a: &str, b: &str) -> Ordering {
        self.primary.compare(a, b)
    }

    /// The CE32 ICU4X starts from for `c`, and the data it belongs to: the
    /// tailoring's, or the root's when the tailoring has no entry.
    fn ce32(&self, c: char) -> (u32, &CollationDataV1<'_>) {
        if let Some(tailoring) = &self.tailoring {
            let ce32 = tailoring.get().trie.get32(u32::from(c));
            if ce32 != FALLBACK_CE32 {
                return (ce32, tailoring.get());
            }
        }
        let root = self.root.get();
        (root.trie.get32(u32::from(c)), root)
    }

    /// The number of collation elements with a non-zero primary weight that
    /// `ce32` stands for, when it stands for a fixed list of them; `None` for
    /// a contraction, a prefix, a digit under `numeric=yes`, or anything else
    /// that depends on context.
    fn primaries(&self, ce32: u32, data: &CollationDataV1<'_>) -> Option<u32> {
        if !is_special(ce32) {
            return Some(u32::from(ce32 & 0xFFFF_0000 != 0));
        }
        let index = (ce32 >> 13) as usize;
        let len = ((ce32 >> 8) & 31) as usize;
        match tag(ce32) {
            TAG_LONG_PRIMARY | TAG_OFFSET | TAG_IMPLICIT => Some(1),
            TAG_LONG_SECONDARY => Some(0),
            TAG_EXPANSION32 => (index..index + len).try_fold(0, |count, i| {
                let element = data.ce32s.get(i)?;
                // Each element is a self-contained CE32.
                let one = if !is_special(element) {
                    u32::from(element & 0xFFFF_0000 != 0)
                } else {
                    match tag(element) {
                        TAG_LONG_PRIMARY => 1,
                        TAG_LONG_SECONDARY => 0,
                        _ => return None,
                    }
                };
                Some(count + one)
            }),
            TAG_EXPANSION => (index..index + len).try_fold(0, |count, i| {
                Some(count + u32::from(data.ces.get(i)? >> 32 != 0))
            }),
            // Without numeric collation a digit is its stored CE32.
            TAG_DIGIT if !self.numeric => {
                let digit = data.ce32s.get(index)?;
                if is_special(digit) && tag(digit) == TAG_DIGIT {
                    None
                } else {
                    self.primaries(digit, data)
                }
            }
            _ => None,
        }
    }

    /// A combining diacritic ICU4X weighs from its diacritics table: a
    /// secondary weight only, never a contraction or a primary weight.
    fn table_diacritic(&self, c: char) -> bool {
        u32::from(c).wrapping_sub(DIACRITICS_BASE) < self.diacritics
    }

    /// The primary weights of a combining mark that follows a starter.
    fn mark_primaries(&self, mark: char) -> Option<u32> {
        if self.table_diacritic(mark) {
            return Some(0);
        }
        let (ce32, data) = self.ce32(mark);
        self.primaries(ce32, data)
    }

    fn decompose(&self, c: char) -> Decomposed {
        // Nothing below U+00C0 decomposes or combines.
        if u32::from(c) < 0xC0 {
            return Decomposed {
                chars: vec![c],
                ccc: vec![0],
            };
        }
        let chars: Vec<char> = self.nfd.normalize_iter(std::iter::once(c)).collect();
        let ccc = chars.iter().map(|&x| self.ccc.get(x).0).collect();
        Decomposed { chars, ccc }
    }

    /// What the data says about each character of `s` (module docs).
    pub(super) fn classify(&self, s: &str) -> Vec<Unit> {
        let mut table: Vec<Decomposed> = Vec::new();
        let mut seen: HashMap<char, usize> = HashMap::new();
        let mut which: Vec<usize> = Vec::new();
        let mut chars: Vec<char> = Vec::new();
        let mut units: Vec<Unit> = Vec::new();
        for c in s.chars() {
            let entry = *seen.entry(c).or_insert_with(|| {
                table.push(self.decompose(c));
                table.len() - 1
            });
            which.push(entry);
            chars.push(c);
            units.push(Unit {
                starter: table[entry].ccc[0] == 0,
                primaries: None,
            });
        }
        let n = units.len();
        let mut dependent = vec![false; n];
        for t in 0..n {
            let d = &table[which[t]];
            let first = d.chars[0];
            if is_hangul_syllable(chars[t]) {
                // Decomposed by ICU4X itself into two or three jamo with
                // self-contained collation elements; only a following
                // combining mark can reach the last of them.
                if t + 1 == n || units[t + 1].starter {
                    units[t].primaries = Some(d.chars.len() as u32);
                }
                continue;
            }
            // A conjoining jamo, or a decomposition with a second starter.
            if is_jamo(first) || d.ccc[1..].contains(&0) {
                continue;
            }
            let (ce32, data) = self.ce32(first);
            let mut marks_from = 1;
            let own = if !units[t].starter && self.table_diacritic(first) {
                // A leading diacritic is weighed from the table too (ICU4X
                // gives every string a placeholder starter it belongs to).
                Some(0)
            } else if is_special(ce32) && tag(ce32) == TAG_CONTRACTION {
                match self.contraction(ce32, data, t, &table, &which, &units) {
                    Walked::Zone(end) => {
                        dependent[t..=end].fill(true);
                        None
                    }
                    Walked::Default(value) | Walked::Joined(value)
                        if is_special(value)
                            && matches!(tag(value), TAG_CONTRACTION | TAG_PREFIX) =>
                    {
                        // A nested context: give up on the rest of the string.
                        dependent[t..].fill(true);
                        None
                    }
                    Walked::Default(value) => self.primaries(value, data),
                    Walked::Joined(value) => {
                        marks_from = 2;
                        self.primaries(value, data)
                    }
                }
            } else if is_special(ce32) && tag(ce32) == TAG_PREFIX {
                let (from, to) = self.prefix_zone(ce32, data, t, &table, &which, &units);
                dependent[from..=to].fill(true);
                None
            } else {
                self.primaries(ce32, data)
            };
            // Its own combining marks: each a fixed list of collation
            // elements without a primary weight.
            let marks = d.chars[marks_from..]
                .iter()
                .all(|&mark| self.mark_primaries(mark) == Some(0));
            units[t].primaries = match own {
                // A non-starter with a primary weight could be reordered.
                Some(count) if marks && (units[t].starter || count == 0) => Some(count),
                _ => None,
            };
        }
        for (unit, &dependent) in units.iter_mut().zip(&dependent) {
            if dependent {
                unit.primaries = None;
            }
        }
        units
    }

    /// Follows the contraction trie of the starter at `t` over what comes
    /// after it, in the order ICU4X matches it (each combining run sorted by
    /// combining class, as canonical ordering leaves it), allowing any
    /// non-starter to be skipped — more than ICU4X's discontiguous matching
    /// allows, so the zone found is never smaller than the real one. It also
    /// checks that U+FFFF could not continue the contraction wherever a
    /// candidate may end (before a starter, or at the end).
    fn contraction(
        &self,
        ce32: u32,
        data: &CollationDataV1<'_>,
        t: usize,
        table: &[Decomposed],
        which: &[usize],
        units: &[Unit],
    ) -> Walked {
        let n = units.len();
        let Some((default, trie)) = default_and_trie(data, (ce32 >> 13) as usize) else {
            return Walked::Zone(n - 1);
        };
        let own = &table[which[t]];
        let mut states = vec![trie.clone()];
        let mut reach: Option<usize> = None;
        // The first run: the starter's own marks and the non-starters after it.
        let mut run: Vec<(usize, char, u8)> = own.chars[1..]
            .iter()
            .zip(&own.ccc[1..])
            .map(|(&c, &k)| (t, c, k))
            .collect();
        let mut next = t + 1;
        loop {
            while next < n && !units[next].starter {
                let d = &table[which[next]];
                run.extend(d.chars.iter().zip(&d.ccc).map(|(&c, &k)| (next, c, k)));
                next += 1;
            }
            run.sort_by_key(|&(_, _, ccc)| ccc);
            for &(position, c, ccc) in &run {
                let mut alive = Vec::new();
                for state in &states {
                    if ccc != 0 {
                        // Skipped (discontiguous matching).
                        alive.push(state.clone());
                    }
                    let mut state = state.clone();
                    match state.next(c) {
                        TrieResult::NoMatch => {}
                        TrieResult::FinalValue(_) => reach = Some(position),
                        TrieResult::NoValue | TrieResult::Intermediate(_) => {
                            reach = Some(position);
                            alive.push(state);
                        }
                    }
                }
                if alive.len() > MAX_WALK_STATES {
                    return Walked::Zone(n - 1);
                }
                states = alive;
                if states.is_empty() {
                    break;
                }
            }
            run.clear();
            if states.is_empty() {
                break;
            }
            // A candidate may end here, followed by U+FFFF in the pruning.
            if states
                .iter()
                .any(|state| state.clone().next(SENTINEL) != TrieResult::NoMatch)
            {
                return Walked::Zone(n - 1);
            }
            if next == n {
                break;
            }
            // The next starter, contiguous, then its own run.
            let d = &table[which[next]];
            run.extend(d.chars.iter().zip(&d.ccc).map(|(&c, &k)| (next, c, k)));
            next += 1;
        }
        match reach {
            None => Walked::Default(default),
            Some(position) if position == t => {
                // Only the starter's own marks: ICU4X's own reading of a
                // starter with one combining mark followed by a starter.
                if own.chars.len() == 2 && (t + 1 == n || units[t + 1].starter) {
                    let mut state = trie;
                    match state.next(own.chars[1]) {
                        TrieResult::FinalValue(value) | TrieResult::Intermediate(value) => {
                            Walked::Joined(value as u32)
                        }
                        TrieResult::NoValue | TrieResult::NoMatch => Walked::Default(default),
                    }
                } else {
                    Walked::Zone(t)
                }
            }
            Some(position) => Walked::Zone(position),
        }
    }

    /// The characters a prefix-context character at `t` may depend on, and
    /// the ones it may reach. ICU4X matches its prefix trie against the last
    /// two values it pushed into a two-character buffer
    /// (`CollationElements::prefix`): each starter pushes itself, and so does
    /// a combining mark it does not take from its diacritics table, while a
    /// table diacritic overwrites the latest value with U+FFFF, the buffer's
    /// initial value. So the first step of the match can only see a character
    /// of the positions from the nearest pushing one on (or U+FFFF), and the
    /// second one of the positions from the pushing one before it; a step
    /// that no such character continues ends the match, and what came before
    /// it plays no part. The CE32 the match arrives at could in principle be a
    /// contraction over what follows.
    fn prefix_zone(
        &self,
        ce32: u32,
        data: &CollationDataV1<'_>,
        t: usize,
        table: &[Decomposed],
        which: &[usize],
        units: &[Unit],
    ) -> (usize, usize) {
        let n = units.len();
        let pushes = |p: usize| units[p].starter || !self.table_diacritic(table[which[p]].chars[0]);
        let mut pushers = (0..t).rev().filter(|&p| pushes(p));
        let nearest = pushers.next();
        let before = pushers.next();
        // Every character the positions from `from` up to `t` decompose to,
        // and U+FFFF.
        let seen = |from: Option<usize>| -> Vec<char> {
            from.map_or(0..0, |p| p..t)
                .flat_map(|q| table[which[q]].chars.iter().copied())
                .chain(std::iter::once(SENTINEL))
                .collect()
        };
        let Some((default, trie)) = default_and_trie(data, (ce32 >> 13) as usize) else {
            return (0, n - 1);
        };
        let mut values = vec![default];
        let mut from = t;
        let mut survivors = Vec::new();
        for c in seen(nearest) {
            let mut state = trie.clone();
            match state.next(c) {
                TrieResult::NoMatch => continue,
                TrieResult::FinalValue(value) => values.push(value as u32),
                TrieResult::Intermediate(value) => {
                    values.push(value as u32);
                    survivors.push(state);
                }
                TrieResult::NoValue => survivors.push(state),
            }
            from = nearest.unwrap_or(0);
        }
        let second = seen(before);
        for state in survivors {
            for &c in &second {
                let mut deeper = state.clone();
                match deeper.next(c) {
                    TrieResult::NoMatch => continue,
                    TrieResult::FinalValue(value) | TrieResult::Intermediate(value) => {
                        values.push(value as u32)
                    }
                    TrieResult::NoValue => {}
                }
                from = before.unwrap_or(0);
            }
        }
        let nested = values
            .iter()
            .any(|&value| is_special(value) && matches!(tag(value), TAG_CONTRACTION | TAG_PREFIX));
        (from.min(t), if nested { n - 1 } else { t })
    }
}

/// The default CE32 and the trie of a contraction or prefix entry
/// (`CollationDataV1::get_default_and_trie` in ICU4X 1.5).
fn default_and_trie<'d>(
    data: &'d CollationDataV1<'_>,
    index: usize,
) -> Option<(u32, Char16TrieIterator<'d>)> {
    let high = data.contexts.get(index)?;
    let low = data.contexts.get(index + 1)?;
    let trie = data.contexts.get_subslice(index + 2..data.contexts.len())?;
    Some((
        (u32::from(high) << 16) | u32::from(low),
        Char16TrieIterator::new(trie),
    ))
}

/// The search ran out of [`BUDGET`]: the caller raises FOCH0004.
#[derive(Debug)]
pub(super) struct Exhausted;

/// One substring search: the full collator, the data (when it could be
/// loaded), and the budget left.
pub(super) struct Search<'a> {
    full: &'a Collator,
    data: Option<&'a SearchData>,
    budget: Cell<usize>,
}

impl<'a> Search<'a> {
    pub(super) fn new(full: &'a Collator, data: Option<&'a SearchData>, budget: usize) -> Self {
        Search {
            full,
            data,
            budget: Cell::new(budget),
        }
    }

    /// Charges a comparison of a candidate of `chars` characters with the
    /// needle: its length and a constant. ICU4X decodes both strings in step,
    /// character by character, and stops at the first primary difference, so
    /// it reads about as much of the needle as of the candidate — charging
    /// the needle's own length would overcharge every short candidate of a
    /// long needle, though that comparison stops at once.
    fn charge(&self, chars: usize) -> Result<(), Exhausted> {
        let cost = chars + 8;
        let left = self.budget.get();
        if cost > left {
            return Err(Exhausted);
        }
        self.budget.set(left - cost);
        Ok(())
    }

    /// Whether the full collator calls `candidate` (`chars` characters long)
    /// and `needle` equal.
    fn equal(&self, candidate: &str, chars: usize, needle: &str) -> Result<bool, Exhausted> {
        self.charge(chars)?;
        Ok(candidate == needle || self.full.compare(candidate, needle) == Ordering::Equal)
    }

    /// `P`'s order of `candidate` and `needle`: their W1 compared.
    fn primary(
        &self,
        data: &SearchData,
        candidate: &str,
        chars: usize,
        needle: &str,
    ) -> Result<Ordering, Exhausted> {
        self.charge(chars)?;
        Ok(data.primary.compare(candidate, needle))
    }

    /// The leftmost minimal match of `needle` in `haystack` (neither
    /// ignorable: the zero-length rules come first, in the caller).
    pub(super) fn find(
        &self,
        haystack: &str,
        needle: &str,
    ) -> Result<Option<(usize, usize)>, Exhausted> {
        let Some(data) = self.data else {
            return Exhaustive::new(self, haystack, needle, None).leftmost_minimal();
        };
        let h = data.classify(haystack);
        let n = data.classify(needle);
        if let Some(filter) = Filter::new(data, haystack, needle, &h, &n) {
            return filter.find(self, haystack, needle);
        }
        if let Some(counted) = Counted::new(data, &h, &n) {
            return counted.find(self, haystack, needle);
        }
        let safe = self.pruning(data, needle, &h);
        Exhaustive::new(self, haystack, needle, safe).leftmost_minimal()
    }

    /// Whether some prefix of `haystack` equals `needle`.
    pub(super) fn starts_with(&self, haystack: &str, needle: &str) -> Result<bool, Exhausted> {
        let bounds = boundaries(haystack);
        let last = bounds.len() - 1;
        let Some(data) = self.data else {
            return self.any_prefix(haystack, needle, &bounds, None);
        };
        let h = data.classify(haystack);
        let n = data.classify(needle);
        if let Some(filter) = Filter::new(data, haystack, needle, &h, &n) {
            return filter.starts_with(self, haystack, needle, &bounds);
        }
        if let Some(counted) = Counted::new(data, &h, &n) {
            for end in counted.ends(0) {
                if self.equal(&haystack[..bounds[end]], end, needle)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let safe = self.pruning(data, needle, &h);
        debug_assert_eq!(h.len(), last);
        self.any_prefix(haystack, needle, &bounds, safe.as_deref())
    }

    /// Whether some suffix of `haystack` equals `needle`.
    pub(super) fn ends_with(&self, haystack: &str, needle: &str) -> Result<bool, Exhausted> {
        let bounds = boundaries(haystack);
        if let Some(data) = self.data {
            let h = data.classify(haystack);
            let n = data.classify(needle);
            if let Some(filter) = Filter::new(data, haystack, needle, &h, &n) {
                return filter.ends_with(self, haystack, needle, &bounds);
            }
            if let Some(counted) = Counted::new(data, &h, &n) {
                let last = bounds.len() - 1;
                for start in counted.starts(last) {
                    if self.equal(&haystack[bounds[start]..], last - start, needle)? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
        }
        let last = bounds.len() - 1;
        for (start, &offset) in bounds[..last].iter().enumerate() {
            if self.equal(&haystack[offset..], last - start, needle)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every prefix in turn, stopping at the first safe boundary from which
    /// none can match.
    fn any_prefix(
        &self,
        haystack: &str,
        needle: &str,
        bounds: &[usize],
        safe: Option<&[bool]>,
    ) -> Result<bool, Exhausted> {
        let last = bounds.len() - 1;
        for end in 1..=last {
            let candidate = &haystack[..bounds[end]];
            if let (Some(safe), Some(data)) = (safe, self.data) {
                if end < last && safe[end] {
                    match self.prefix_state(data, candidate, end, needle)? {
                        Prefix::Dead => return Ok(false),
                        Prefix::Proper => continue,
                        Prefix::Equal => {}
                    }
                }
            }
            if self.equal(candidate, end, needle)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The safe boundaries of the haystack (module docs, "The pruned
    /// search"), or `None` when the pruning cannot be used: U+FFFF does not
    /// have its documented weight, or the needle contains it.
    fn pruning(&self, data: &SearchData, needle: &str, h: &[Unit]) -> Option<Vec<bool>> {
        if !data.sentinel || needle.contains(SENTINEL) {
            return None;
        }
        let mut safe = vec![false; h.len() + 1];
        for k in 1..h.len() {
            safe[k] = h[k].starter && h[k].primaries.is_some() && h[k - 1].primaries.is_some();
        }
        Some(safe)
    }

    /// How W1(`candidate`) stands to W1(`needle`), by two `P` comparisons.
    fn prefix_state(
        &self,
        data: &SearchData,
        candidate: &str,
        chars: usize,
        needle: &str,
    ) -> Result<Prefix, Exhausted> {
        Ok(match self.primary(data, candidate, chars, needle)? {
            Ordering::Equal => Prefix::Equal,
            Ordering::Greater => Prefix::Dead,
            Ordering::Less => {
                let extended = format!("{candidate}{SENTINEL}");
                if self.primary(data, &extended, chars + 1, needle)? == Ordering::Less {
                    Prefix::Dead
                } else {
                    Prefix::Proper
                }
            }
        })
    }
}

/// W1 of a candidate against W1 of the needle.
enum Prefix {
    /// Equal.
    Equal,
    /// A proper prefix: longer candidates may still match.
    Proper,
    /// Not a prefix: no longer candidate from the same start can match (at a
    /// safe boundary).
    Dead,
}

/// The character boundaries of `s`: 0, the start of every later character,
/// and `s.len()`.
pub(super) fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(s.len()))
        .collect()
}

/// The primary filter (module docs, section 1): the `P`-classes of the
/// primary-bearing characters of both strings.
struct Filter {
    /// The haystack's primary-bearing characters: (character index, class).
    haystack: Vec<(usize, u32)>,
    /// The needle's classes, in order; not empty.
    needle: Vec<u32>,
    /// The number of characters of the haystack.
    length: usize,
}

impl Filter {
    /// `None` unless every character of both strings is context-free with at
    /// most one primary weight and the needle has at least one.
    fn new(
        data: &SearchData,
        haystack: &str,
        needle: &str,
        h: &[Unit],
        n: &[Unit],
    ) -> Option<Self> {
        let plain = |units: &[Unit]| units.iter().all(|u| matches!(u.primaries, Some(0 | 1)));
        if !plain(h) || !plain(n) {
            return None;
        }
        // The distinct primary-bearing characters, by `P`: a character
        // `P` calls equal to "" is primary-ignorable here (a variable one
        // under `alternate=shifted`).
        let mut distinct: Vec<char> = haystack
            .chars()
            .zip(h)
            .chain(needle.chars().zip(n))
            .filter(|(_, u)| u.primaries == Some(1))
            .map(|(c, _)| c)
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        let mut buffer = [0u8; 4];
        let mut strings: Vec<(char, String)> = distinct
            .into_iter()
            .map(|c| (c, c.encode_utf8(&mut buffer).to_string()))
            .filter(|(_, s)| data.primary.compare(s, "") != Ordering::Equal)
            .collect();
        strings.sort_by(|a, b| data.primary.compare(&a.1, &b.1));
        let mut classes: HashMap<char, u32> = HashMap::new();
        let mut class = 0;
        for i in 0..strings.len() {
            if i > 0 && data.primary.compare(&strings[i - 1].1, &strings[i].1) != Ordering::Equal {
                class += 1;
            }
            classes.insert(strings[i].0, class);
        }
        let needle: Vec<u32> = needle
            .chars()
            .filter_map(|c| classes.get(&c).copied())
            .collect();
        if needle.is_empty() {
            return None;
        }
        let mut length = 0;
        let haystack = haystack
            .chars()
            .enumerate()
            .inspect(|_| length += 1)
            .filter_map(|(i, c)| classes.get(&c).map(|&k| (i, k)))
            .collect();
        Some(Filter {
            haystack,
            needle,
            length,
        })
    }

    /// The occurrences of the needle's classes in the haystack's (KMP), as
    /// indexes into `self.haystack`, in increasing order.
    fn occurrences(&self) -> impl Iterator<Item = usize> + '_ {
        let m = self.needle.len();
        let mut failure = vec![0usize; m];
        let mut k = 0;
        for i in 1..m {
            while k > 0 && self.needle[i] != self.needle[k] {
                k = failure[k - 1];
            }
            if self.needle[i] == self.needle[k] {
                k += 1;
            }
            failure[i] = k;
        }
        let mut matched = 0;
        self.haystack
            .iter()
            .enumerate()
            .filter_map(move |(i, &(_, class))| {
                while matched > 0 && class != self.needle[matched] {
                    matched = failure[matched - 1];
                }
                if class == self.needle[matched] {
                    matched += 1;
                }
                if matched == m {
                    matched = failure[m - 1];
                    Some(i + 1 - m)
                } else {
                    None
                }
            })
    }

    /// The window of occurrence `u`: the starts (character indexes) from the
    /// character after the previous primary-bearing one to the first of the
    /// occurrence, and the ends from just after its last to the next
    /// primary-bearing character (or the end).
    fn window(
        &self,
        u: usize,
    ) -> (
        std::ops::RangeInclusive<usize>,
        std::ops::RangeInclusive<usize>,
    ) {
        let m = self.needle.len();
        let first = self.haystack[u].0;
        let last = self.haystack[u + m - 1].0;
        let before = if u == 0 {
            0
        } else {
            self.haystack[u - 1].0 + 1
        };
        let after = self
            .haystack
            .get(u + m)
            .map_or(self.length, |&(position, _)| position);
        (before..=first, last + 1..=after)
    }

    fn find(
        &self,
        search: &Search<'_>,
        haystack: &str,
        needle: &str,
    ) -> Result<Option<(usize, usize)>, Exhausted> {
        let bounds = boundaries(haystack);
        for u in self.occurrences() {
            let (starts, ends) = self.window(u);
            let mut window = Window::new(search, haystack, needle, &bounds, &starts, &ends);
            for start in starts.clone() {
                let Some(end) = window.shortest(start, *ends.end())? else {
                    continue;
                };
                // DS4: no match inside, which can only start later in the
                // same window.
                let mut minimal = true;
                for inner in start + 1..=*starts.end() {
                    if window.shortest(inner, end)?.is_some() {
                        minimal = false;
                        break;
                    }
                }
                if minimal {
                    return Ok(Some((bounds[start], bounds[end])));
                }
            }
        }
        Ok(None)
    }

    fn starts_with(
        &self,
        search: &Search<'_>,
        haystack: &str,
        needle: &str,
        bounds: &[usize],
    ) -> Result<bool, Exhausted> {
        let m = self.needle.len();
        if self.haystack.len() < m
            || self.haystack[..m]
                .iter()
                .map(|&(_, k)| k)
                .ne(self.needle.iter().copied())
        {
            return Ok(false);
        }
        let (_, ends) = self.window(0);
        for end in ends {
            if search.equal(&haystack[..bounds[end]], end, needle)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ends_with(
        &self,
        search: &Search<'_>,
        haystack: &str,
        needle: &str,
        bounds: &[usize],
    ) -> Result<bool, Exhausted> {
        let m = self.needle.len();
        let r = self.haystack.len();
        if r < m
            || self.haystack[r - m..]
                .iter()
                .map(|&(_, k)| k)
                .ne(self.needle.iter().copied())
        {
            return Ok(false);
        }
        let (starts, _) = self.window(r - m);
        let last = bounds.len() - 1;
        for start in starts {
            if search.equal(&haystack[bounds[start]..], last - start, needle)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The count filter (module docs, section 1): when every character of both
/// strings is context-free and nothing is variable-weighted, W1 of a
/// candidate has exactly as many weights as its characters have primary
/// weights between them, and a match needs as many as the needle. That
/// leaves, for each start, the few ends whose count is right — this is what
/// serves expansions such as `ß` (two primary weights), ligatures and Hangul
/// syllables, which the primary filter cannot take.
struct Counted {
    /// `sums[k]`: the primary weights of the haystack's first `k` characters.
    sums: Vec<usize>,
    /// The needle's primary weights.
    total: usize,
}

impl Counted {
    /// `None` under `alternate=shifted` (a variable weight drops out, so a
    /// count is only an upper bound) or when a character is not context-free.
    fn new(data: &SearchData, h: &[Unit], n: &[Unit]) -> Option<Self> {
        if data.shifted {
            return None;
        }
        let mut sums = Vec::with_capacity(h.len() + 1);
        sums.push(0);
        for unit in h {
            let count = unit.primaries? as usize;
            sums.push(sums[sums.len() - 1] + count);
        }
        let total = n
            .iter()
            .map(|u| u.primaries.map(|k| k as usize))
            .sum::<Option<usize>>()?;
        Some(Counted { sums, total })
    }

    /// The ends `j > start` whose candidate `start..j` has the needle's count:
    /// a range, the counts being sums of non-negative numbers.
    fn ends(&self, start: usize) -> std::ops::Range<usize> {
        let want = self.sums[start] + self.total;
        let low = self.sums.partition_point(|&sum| sum < want).max(start + 1);
        let high = self.sums.partition_point(|&sum| sum <= want);
        low..high.max(low)
    }

    /// The starts `i < last` whose candidate `i..last` (to the end of the
    /// haystack) has the needle's count.
    fn starts(&self, last: usize) -> std::ops::Range<usize> {
        let Some(want) = self.sums[last].checked_sub(self.total) else {
            return 0..0;
        };
        let sums = &self.sums[..last];
        let low = sums.partition_point(|&sum| sum < want);
        let high = sums.partition_point(|&sum| sum <= want);
        low..high
    }

    fn find(
        &self,
        search: &Search<'_>,
        haystack: &str,
        needle: &str,
    ) -> Result<Option<(usize, usize)>, Exhausted> {
        let bounds = boundaries(haystack);
        let last = bounds.len() - 1;
        // Per start: the shortest end with a match, once found, and the last
        // end compared — each candidate is compared at most once.
        let mut found: Vec<Option<usize>> = vec![None; last];
        let mut tested: Vec<usize> = (0..last).map(|start| self.ends(start).start - 1).collect();
        let mut shortest = |start: usize, limit: usize| -> Result<Option<usize>, Exhausted> {
            if let Some(end) = found[start] {
                return Ok((end <= limit).then_some(end));
            }
            let range = self.ends(start);
            while tested[start] + 1 < range.end && tested[start] < limit {
                tested[start] += 1;
                let end = tested[start];
                if search.equal(&haystack[bounds[start]..bounds[end]], end - start, needle)? {
                    found[start] = Some(end);
                    return Ok(Some(end));
                }
            }
            Ok(None)
        };
        for start in 0..last {
            let Some(end) = shortest(start, last)? else {
                continue;
            };
            let mut minimal = true;
            for inner in start + 1..end {
                if shortest(inner, end)?.is_some() {
                    minimal = false;
                    break;
                }
            }
            if minimal {
                return Ok(Some((bounds[start], bounds[end])));
            }
        }
        Ok(None)
    }
}

/// The candidates of one window of the primary filter, each compared at most
/// once: `shortest(start, limit)` resumes where an earlier call for the same
/// start stopped.
struct Window<'s, 'a> {
    search: &'s Search<'a>,
    haystack: &'s str,
    needle: &'s str,
    bounds: &'s [usize],
    first_start: usize,
    first_end: usize,
    /// Per start: the shortest end with a match, once found.
    found: Vec<Option<usize>>,
    /// Per start: the last end compared (`first_end - 1` before any).
    tested: Vec<usize>,
}

impl<'s, 'a> Window<'s, 'a> {
    fn new(
        search: &'s Search<'a>,
        haystack: &'s str,
        needle: &'s str,
        bounds: &'s [usize],
        starts: &std::ops::RangeInclusive<usize>,
        ends: &std::ops::RangeInclusive<usize>,
    ) -> Self {
        let count = starts.end() - starts.start() + 1;
        Window {
            search,
            haystack,
            needle,
            bounds,
            first_start: *starts.start(),
            first_end: *ends.start(),
            found: vec![None; count],
            tested: vec![*ends.start() - 1; count],
        }
    }

    fn shortest(&mut self, start: usize, limit: usize) -> Result<Option<usize>, Exhausted> {
        let i = start - self.first_start;
        if let Some(end) = self.found[i] {
            return Ok((end <= limit).then_some(end));
        }
        while self.tested[i] < limit {
            self.tested[i] += 1;
            let end = self.tested[i];
            debug_assert!(end >= self.first_end);
            let candidate = &self.haystack[self.bounds[start]..self.bounds[end]];
            if self.search.equal(candidate, end - start, self.needle)? {
                self.found[i] = Some(end);
                return Ok(Some(end));
            }
        }
        Ok(None)
    }
}

/// The search over every candidate, as F&O defines the match (`uca.rs`
/// module docs), with the pruning at safe boundaries when `safe` is given
/// (module docs, section 2). Without it this is the reference the tests
/// compare the other paths with.
pub(super) struct Exhaustive<'s, 'a> {
    search: &'s Search<'a>,
    haystack: &'s str,
    needle: &'s str,
    /// The character boundaries of `haystack`; positions below index them.
    bounds: Vec<usize>,
    /// `safe[k]`: the boundary before character `k` is safe.
    safe: Option<Vec<bool>>,
    /// For each start, the shortest end with a match, once one is found.
    found: Vec<Option<usize>>,
    /// For each start `i`, the last end compared: every end in
    /// `i + 1 ..= tested[i]` has been decided.
    tested: Vec<usize>,
    /// For each start, the first end from which none can match (pruning).
    dead: Vec<usize>,
}

impl<'s, 'a> Exhaustive<'s, 'a> {
    pub(super) fn new(
        search: &'s Search<'a>,
        haystack: &'s str,
        needle: &'s str,
        safe: Option<Vec<bool>>,
    ) -> Self {
        let bounds = boundaries(haystack);
        let positions = bounds.len();
        Exhaustive {
            search,
            haystack,
            needle,
            safe,
            found: vec![None; positions],
            tested: (0..positions).collect(),
            dead: vec![positions; positions],
            bounds,
        }
    }

    /// The shortest match that starts at position `start` and ends at or
    /// before `limit` (`needle` is not ignorable, so a match is never empty).
    fn shortest(&mut self, start: usize, limit: usize) -> Result<Option<usize>, Exhausted> {
        if let Some(end) = self.found[start] {
            return Ok((end <= limit).then_some(end));
        }
        let last = self.bounds.len() - 1;
        loop {
            let end = self.tested[start] + 1;
            if end > limit || end >= self.dead[start] {
                return Ok(None);
            }
            self.tested[start] = end;
            let candidate = &self.haystack[self.bounds[start]..self.bounds[end]];
            if let (Some(safe), Some(data)) = (&self.safe, self.search.data) {
                if end < last && safe[end] {
                    match self
                        .search
                        .prefix_state(data, candidate, end - start, self.needle)?
                    {
                        Prefix::Dead => {
                            self.dead[start] = end;
                            return Ok(None);
                        }
                        Prefix::Proper => continue,
                        Prefix::Equal => {}
                    }
                }
            }
            if self.search.equal(candidate, end - start, self.needle)? {
                self.found[start] = Some(end);
                return Ok(Some(end));
            }
        }
    }

    /// The leftmost minimal match, as a byte range of `haystack`.
    ///
    /// A match `[i, j)` is minimal (DS4) when no match lies within it: `j` is
    /// the shortest end from `i`, and no later start `k` in `i + 1 .. j` has a
    /// match ending at or before `j`.
    pub(super) fn leftmost_minimal(&mut self) -> Result<Option<(usize, usize)>, Exhausted> {
        let last = self.bounds.len() - 1;
        for start in 0..last {
            let Some(end) = self.shortest(start, last)? else {
                continue;
            };
            let mut minimal = true;
            for inner in start + 1..end {
                if self.shortest(inner, end)?.is_some() {
                    minimal = false;
                    break;
                }
            }
            if minimal {
                return Ok(Some((self.bounds[start], self.bounds[end])));
            }
        }
        Ok(None)
    }
}
