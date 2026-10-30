//! `xsl:analyze-string` at run time (§15.1): the input string, the regular
//! expression, the partition of the input into matching and non-matching
//! substrings, and the evaluation of `xsl:matching-substring` /
//! `xsl:non-matching-substring` once per substring with that substring as the
//! focus and — for a match — its captured substrings as the current captured
//! substrings (§15.2).
//!
//! The partition is `regexml`'s `Regex::analyze`, which yields the
//! substrings in order: `AnalyzeEntry::NonMatch(text)` and
//! `AnalyzeEntry::Match(entries)`, the entries of a match being its text with
//! every participating group nested as `MatchEntry::Group { nr, value }`. There
//! are no offsets, so the text of a group is rebuilt by concatenating what is
//! nested in it (`collect_match`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use regexml::{AnalyzeEntry, ErrorKind, MatchEntry};
use xsd_schema::types::XmlValue;
use xsd_schema::xpath::functions::{atomize_sequence, XPathValue};
use xsd_schema::xpath::XmlItem;

use super::context::{CapturedSubstrings, XsltContext};
use super::functions::{converts_to_string, describe_atoms};
use super::{Frame, Transformation};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::{Nav, Receiver};
use crate::regex::CompiledRegex;

/// How many computed regular expressions a run keeps compiled.
///
/// `regexml` builds the Unicode category sets behind `\p{…}`, `\w` and `\d`
/// once per process, but it still parses the pattern and builds a program on
/// every compilation, so recompiling the same effective value for every
/// evaluation of an instruction would dominate a loop over short strings.
/// The cache is keyed by the effective values of `regex` and
/// `flags`; when it is full it is simply emptied — a stylesheet that computes
/// more distinct regular expressions than this in one run pays one
/// compilation per evaluation, which is what it would pay without a cache.
const REGEX_CACHE_CAPACITY: usize = 64;

/// The run's cache of regular expressions compiled from computed `regex` /
/// `flags` values (`RegexSource::Computed`). A failed compilation is kept
/// too: its error depends on the two strings alone.
#[derive(Default)]
pub(super) struct RegexCache {
    compiled: HashMap<(String, String), Result<Arc<CompiledRegex>, XsltError>>,
}

impl RegexCache {
    fn get_or_compile(
        &mut self,
        pattern: String,
        flags: String,
    ) -> Result<Arc<CompiledRegex>, XsltError> {
        let key = (pattern, flags);
        if let Some(hit) = self.compiled.get(&key) {
            return hit.clone();
        }
        if self.compiled.len() >= REGEX_CACHE_CAPACITY {
            self.compiled.clear();
        }
        let compiled = CompiledRegex::new(&key.0, &key.1).map(Arc::new);
        self.compiled.insert(key, compiled.clone());
        compiled
    }
}

/// One substring of the partition (§15.1).
enum Substring {
    /// A substring that matches the regular expression, and its captured
    /// substrings (index 0: the whole match).
    Match {
        text: String,
        groups: Vec<Option<String>>,
    },
    /// A substring between two matches, or before the first or after the last.
    NonMatch(String),
}

impl<'a, 't> Transformation<'a, 't> {
    /// Evaluates an `xsl:analyze-string` instruction (§15.1).
    pub(super) fn analyze_string(
        &self,
        instruction: &Instruction,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
        out: &mut dyn Receiver<'t>,
    ) -> Result<(), XsltError> {
        let InstructionKind::AnalyzeString {
            select,
            regex,
            matching,
            non_matching,
        } = &instruction.kind
        else {
            return Err(XsltError::internal(
                "analyze_string called on another instruction",
            ));
        };
        let value = self.eval_expr(*select, frame, cx)?;
        let input = self.analyze_input(value, self.sheet.is_backwards(instruction.ctx))?;
        // The regular expression is compiled (or found) before the input is
        // looked at: XTDE1140, XTDE1145 and XTDE1150 are properties of the
        // effective values of `regex` and `flags`, so an empty input does not
        // hide them — XTDE1150 in particular is defined by `matches("", $r,
        // $f)`, whatever the input.
        let regex = match regex {
            RegexSource::Fixed(compiled) => Arc::clone(compiled),
            RegexSource::Computed { regex, flags } => {
                let pattern = self.avt(regex, frame, cx)?;
                let flags = match flags {
                    Some(avt) => self.avt(avt, frame, cx)?,
                    None => String::new(),
                };
                // Q33: compiling nests on the host stack as deep as the
                // pattern does (the engine's nesting limit bounds that), and this
                // instruction may itself sit deep in a body with no template
                // invocation in between to check the stack guard.
                self.check_stack()?;
                self.regexes.borrow_mut().get_or_compile(pattern, flags)?
            }
        };
        // "If the input is a zero-length string, the number of substrings will
        // be zero, so neither the xsl:matching-substring nor
        // xsl:non-matching-substring elements will be evaluated."
        if input.is_empty() {
            return Ok(());
        }
        let substrings = partition(&regex, &input, matching.is_some())?;

        // §15.1: "In processing each substring, the contents of the substring
        // will be the context item (as a value of type xs:string); the
        // position of the substring within the sequence of matching and
        // non-matching substrings will be the context position; and the
        // number of matching and non-matching substrings will be the context
        // size." `current()` is the substring too (§16.6.1).
        //
        // §6.7: "When an xsl:for-each, xsl:for-each-group, or
        // xsl:analyze-string instruction is evaluated … the current template
        // rule becomes null for the evaluation of that instruction"; §6.5: "No
        // other instruction changes the current mode". The frame is the
        // instruction's own.
        //
        // §15.2: "During the evaluation of an xsl:matching-substring
        // instruction it is set to the sequence of matched substrings for that
        // regex match. During the evaluation of an xsl:non-matching-substring
        // instruction … it is set to an empty sequence. On completion of an
        // instruction that changes the value, the variable reverts to its
        // previous value" — the caller's `cx`, untouched.
        let size = substrings.len();
        for (index, substring) in substrings.into_iter().enumerate() {
            let position = index + 1;
            match substring {
                Substring::Match { text, groups } => {
                    // "If the element is absent, the effect is the same as if
                    // it were present with empty content."
                    let Some(body) = matching else { continue };
                    let serial = self.next_scope_serial();
                    let mut inner =
                        cx.with_focus(XmlItem::Atomic(XmlValue::string(text)), position, size);
                    inner.current_rule = None;
                    inner.captured = Some(Rc::new(CapturedSubstrings { groups, serial }));
                    let outcome = self.eval_body(body, frame, &inner, out);
                    // These captured substrings are never current again: the
                    // key indexes built under them go now (`runtime/keys.rs`).
                    self.keys.borrow_mut().forget_scope(serial);
                    outcome?;
                }
                Substring::NonMatch(text) => {
                    let Some(body) = non_matching else { continue };
                    let mut inner =
                        cx.with_focus(XmlItem::Atomic(XmlValue::string(text)), position, size);
                    inner.current_rule = None;
                    inner.captured = None;
                    self.eval_body(body, frame, &inner, out)?;
                }
            }
        }
        Ok(())
    }

    /// The input string of the instruction: the value of `select`, converted
    /// to `xs:string`.
    ///
    /// > \[§15.1\] If the result of evaluating the select expression is not a
    /// > string, it is converted to a string by applying the function
    /// > conversion rules.
    ///
    /// XPath 2.0 §3.1.5 with the expected type `xs:string`: "Atomization is
    /// applied to the given value", an `xs:untypedAtomic` item "is cast to the
    /// expected atomic type", an `xs:anyURI` is promoted to `xs:string`, and
    /// "If, after the above conversions, the resulting value does not match
    /// the expected type according to the rules for SequenceType Matching, a
    /// type error is raised \[err:XPTY0004\]" — so the empty sequence (the
    /// suite's `analyze-string-045`: "Error in 2.0, allowed in 2.1"), several
    /// items, and a number are all XPTY0004.
    ///
    /// With backwards compatible behaviour (§3.8, XPath 1.0 compatibility
    /// mode) §3.1.5 first applies: "If the expected type calls for a single
    /// item or optional single item …, then the value V is effectively
    /// replaced by V\[1\]", and "If the expected type is xs:string or
    /// xs:string?, then the value V is effectively replaced by fn:string(V)" —
    /// so the string value of the first item, and "" for the empty sequence.
    fn analyze_input(
        &self,
        value: XPathValue<Nav<'t>>,
        backwards: bool,
    ) -> Result<String, XsltError> {
        if backwards {
            return Ok(match value.first() {
                None => String::new(),
                Some(XmlItem::Atomic(atom)) => atom.to_string_value(),
                Some(XmlItem::Node(node)) => {
                    use xsd_schema::navigator::DomNavigator;
                    node.value()
                }
            });
        }
        // The usual case, a string: nothing to atomize.
        if let XPathValue::Item(XmlItem::Atomic(atom)) = &value {
            if converts_to_string(atom) {
                return Ok(atom.to_string_value());
            }
        }
        let atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
        match atoms.as_slice() {
            [atom] if converts_to_string(atom) => Ok(atom.to_string_value()),
            _ => Err(XsltError::new(
                "XPTY0004",
                format!(
                    "the select expression of xsl:analyze-string must yield one string, not {}",
                    describe_atoms(&atoms)
                ),
            )),
        }
    }
}

/// Partitions `input` into matching and non-matching substrings (§15.1).
///
/// > The xsl:analyze-string instruction starts at the beginning of the input
/// > string and attempts to find the first substring that matches the regular
/// > expression. If there are several matches, the first match is defined to
/// > be the one whose starting position comes first in the string. If several
/// > alternatives within the regular expression both match at the same
/// > position in the input string, then the match that is chosen is the first
/// > alternative that matches. … Having found the first match, the instruction
/// > proceeds to find the second and subsequent matches by repeating the
/// > search, starting at the first character that was not included in the
/// > previous match. … Each substring will contain at least one character.
///
/// That is `regexml`'s `analyze` exactly (a backtracking engine tries the
/// alternatives in order, and a regular expression that could match a
/// zero-length string never gets here — XTDE1150); an empty non-matching
/// entry, which it does not produce, is dropped all the same, so that the
/// last sentence holds by construction. `with_groups` is false when there is
/// no `xsl:matching-substring` to read them: then only the number of
/// substrings matters.
fn partition(
    regex: &CompiledRegex,
    input: &str,
    with_groups: bool,
) -> Result<Vec<Substring>, XsltError> {
    // The engine's only error here is `ErrorKind::MatchesEmptyString`, which
    // `CompiledRegex::new` has already ruled out (XTDE1150); anything else
    // would be an engine defect, reported as this processor's own.
    let entries = regex.regex().analyze(input).map_err(|e| match e.kind() {
        ErrorKind::MatchesEmptyString => XsltError::new(
            codes::XTDE1150,
            format!(
                "xsl:analyze-string: the regular expression {:?} cannot be applied: {e}",
                regex.pattern()
            ),
        ),
        _ => XsltError::internal(format!(
            "xsl:analyze-string: the regular expression {:?} cannot be applied: {e}",
            regex.pattern()
        )),
    })?;
    let mut substrings = Vec::new();
    for entry in entries {
        match entry {
            AnalyzeEntry::NonMatch(text) => {
                if !text.is_empty() {
                    substrings.push(Substring::NonMatch(text));
                }
            }
            AnalyzeEntry::Match(entries) => {
                let mut groups = Vec::new();
                let mut text = String::new();
                collect_match(&entries, &mut text, with_groups.then_some(&mut groups));
                if text.is_empty() {
                    // Never produced for a regular expression that cannot
                    // match a zero-length string; kept out regardless.
                    continue;
                }
                if with_groups {
                    if groups.is_empty() {
                        groups.push(None);
                    }
                    groups[0] = Some(text.clone());
                }
                substrings.push(Substring::Match { text, groups });
            }
        }
    }
    Ok(substrings)
}

/// Appends the text of `entries` to `text` and, when `groups` is given, records
/// the text of every group nested in them under its number.
///
/// A group's text is the concatenation of everything nested in it, in order:
/// its own characters and the texts of the groups inside it. The recursion is
/// as deep as the groups of the regular expression are nested, not as long as
/// the input.
fn collect_match(
    entries: &[MatchEntry],
    text: &mut String,
    mut groups: Option<&mut Vec<Option<String>>>,
) {
    for entry in entries {
        match entry {
            MatchEntry::String(s) => text.push_str(s),
            MatchEntry::Group { nr, value } => {
                let start = text.len();
                collect_match(value, text, groups.as_deref_mut());
                if let Some(groups) = groups.as_deref_mut() {
                    if groups.len() <= *nr {
                        groups.resize(*nr + 1, None);
                    }
                    groups[*nr] = Some(text[start..].to_string());
                }
            }
        }
    }
}
