//! Regular expressions for `xsl:analyze-string` (§15).
//!
//! > The regular expressions used by this instruction, and the flags that
//! > control the interpretation of these regular expressions, must conform to
//! > the syntax defined in \[Functions and Operators\] (see Section 7.6.1
//! > Regular Expression Syntax), which is itself based on the syntax defined
//! > in \[XML Schema Part 2\].
//!
//! F&O 1.0 §7.6.1 (read at w3.org, paraphrased): the XML Schema syntax plus
//! `^` and `$`, reluctant quantifiers, back-references `\N` and the escape
//! `\$`; capturing groups are numbered by their opening parenthesis; there is
//! no `(?…)` syntax. §7.6.1.1 defines exactly four flags, `s`, `m`, `i` and
//! `x`, in any order and repeatable.
//!
//! The backend is `regexml`, which implements the *later* XPath dialect — the
//! one with the non-capturing group `(?:…)` and the `q` flag. The XPath
//! engine of `xsd-schema` faces the same mismatch for `fn:matches`,
//! `fn:replace` and `fn:tokenize` and solves it with a pre-pass over the
//! pattern before handing it to `regexml::Regex::xpath`; that pre-pass is
//! private to the engine, so `check_xpath20_dialect` here is a copy of it
//! (`xsd-schema/src/xpath/functions/regex.rs`, `check_xpath20_regex_dialect`).
//! Keep the two in step; the copy can go once the engine exposes its regex
//! compilation to hosts.
//!
//! The error codes are XSLT's own (§15.1): an invalid regular expression is
//! XTDE1140 where F&O says FORX0002, invalid flags are XTDE1145 where F&O says
//! FORX0001, and a regular expression that matches the zero-length string is
//! XTDE1150.
//!
//! One more check is this processor's own: a regular expression nested deeper
//! than [`MAX_NESTING`] is refused with an implementation-limit error
//! (`XSLT2-INTERNAL`) before `regexml` sees it, because `regexml` would use
//! host stack in proportion to the nesting and a deep enough pattern — which
//! a computed `regex` can take from the input document — would abort the
//! process.

use std::fmt;

use crate::error::{codes, XsltError};

/// The flags F&O 1.0 §7.6.1.1 defines.
const XPATH20_FLAGS: &str = "smix";

/// How deep the groups and character classes of a regular expression may nest
/// (`(` … `)` outside a class, `[` … `]` including a subtracted class
/// `[a-z-[aeiou]]`, counted together): an implementation limit.
///
/// > \[§21\] A conforming processor may impose limits on the processing
/// > resources consumed by the processing of a stylesheet.
///
/// `regexml` parses a pattern by recursive descent, and the program it builds,
/// the matcher that runs it, the match it reports and the drop of each are all
/// recursive in the nesting too — all on the host stack, with no stack guard
/// (`TransformOptions::stack_limit`) between their levels. Before this limit a
/// computed `regex="{@re}"` with 3 000 nested groups taken from the input
/// aborted the process on a 2 MiB thread, and 12 000 on the 8 MiB main thread.
///
/// The limit has to hold in **both** build profiles, so it is sized for the
/// debug build, whose frames are about six times larger. Measured for the M2h
/// reviews (`regexml` 0.2.2, and in parentheses the local fork with its
/// capture-restoring backtracking, where it differs; arm64, the stack painted
/// below the caller's frame and the deepest write found after each phase; the
/// cost per level is constant from about 16 levels on):
///
/// | Phase | debug, per level | debug, at 32 | release, per level | release, at 32 |
/// |---|---|---|---|---|
/// | compile `((…(a)…))` (`Regex::xpath`, incl. its `is_match("")`) | 5 568 B | 186 KiB | 960 B | 33 KiB |
/// | compile `[a-z-[a-z-[…[a]…]]]` | 2 432 B | 88 KiB | 512 B | 19 KiB |
/// | match (`analyze`), deepest shape tried, `((…(a)+…)+)+` | 1 632 B (1 936 B) | 59 KiB (68 KiB) | 352 B (478 B) | < 16 KiB (17 KiB) |
/// | the captured substrings (`collect_match`) / drop the match / drop the program | 320 / 208 / 256 B | < 16 KiB each | 128 / 64 / 128 B | < 16 KiB each |
///
/// and **nothing** grows with the length of a flat pattern: 10 000
/// alternatives, a sequence of 5 000 groups or of 10 000 quantified pieces, or
/// 10 000 `\p{L}` compile and match in the same stack as one (release). So
/// nesting is the only thing to bound, and the deepest phase is the
/// compilation; the phases run one after another, so the deepest of them is
/// what a regular expression adds. At this limit that is 186 KiB in a debug
/// build — under a quarter of the 1 MiB that lies, on a thread with Rust's
/// default 2 MiB stack, between the end of the stack and the
/// `OTHER_THREAD_STACK_LIMIT` (1 MiB) a run gets by default on a thread that
/// `run_on_large_stack` did not start. That quarter is the budget, because a
/// computed regex is compiled after the run has checked the stack guard, so
/// what the compilation takes is on top of what the guard allowed, and the
/// rest of the 1 MiB is for the evaluation between two checks.
///
/// Measured through `xslt2`'s public API: a whole job (compile the stylesheet,
/// transform, serialize) with a 32-deep regex peaks at 340 KB in a debug
/// build, as deep as the same job with no group at all (56 KB against 35 KB in
/// release); and a template recursion that compiles a new 32-deep computed
/// regex at every level until the guard's default 1 MiB stops it needs a
/// thread of 1 232 KiB in debug (the same as with a regex of no groups: the
/// evaluation between checks dominates) and 1 056 KiB in release, so Rust's
/// default 2 MiB leaves about 800 KiB to spare. The previous limit, 256, was
/// sized from release numbers alone and aborted that recursion in a debug
/// build (1.4 MB to compile at 256). For comparison, Rust's `regex-syntax` 0.8
/// sets its default `nest_limit` to 250 for the same reason; but real regular
/// expressions nest a handful of levels deep: the deepest literal one in the
/// W3C XSLT test suite (758 of them: `xsl:analyze-string`, `matches`,
/// `replace`, `tokenize`) nests 4, in XQTS 1.0.2 2.
pub(crate) const MAX_NESTING: usize = 32;

/// A regular expression compiled for `xsl:analyze-string`, together with the
/// effective values of `regex` and `flags` it was compiled from.
///
/// Only a regular expression that passed every check of §15.1 — XTDE1140,
/// XTDE1145 and XTDE1150 — is ever made into one.
pub struct CompiledRegex {
    regex: regexml::Regex,
    pattern: String,
    flags: String,
}

impl fmt::Debug for CompiledRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledRegex")
            .field("pattern", &self.pattern)
            .field("flags", &self.flags)
            .finish()
    }
}

impl CompiledRegex {
    /// Compiles the effective values of `regex` and `flags` (§15.1).
    ///
    /// > \[ERR XTDE1140\] It is a non-recoverable dynamic error if the
    /// > effective value of the regex attribute does not conform to the
    /// > required syntax for regular expressions, as specified in \[Functions
    /// > and Operators\].
    ///
    /// > \[ERR XTDE1145\] It is a non-recoverable dynamic error if the
    /// > effective value of the flags attribute has a value other than the
    /// > values defined in \[Functions and Operators\].
    ///
    /// > \[ERR XTDE1150\] It is a non-recoverable dynamic error if the
    /// > effective value of the regex attribute is a regular expression that
    /// > matches a zero-length string: or more specifically, if the regular
    /// > expression $r and flags $f are such that matches("", $r, $f) returns
    /// > true.
    ///
    /// XTDE1150 is decided here, by the very test the spec states — whatever
    /// string the instruction is later applied to, an empty one included.
    ///
    /// A pattern nested deeper than [`MAX_NESTING`] is refused first, with the
    /// implementation-limit error (`XSLT2-INTERNAL`), whether or not it is
    /// valid: finding out would take the recursion the limit is there to
    /// avoid.
    pub(crate) fn new(pattern: &str, flags: &str) -> Result<Self, XsltError> {
        check_xpath20_dialect(pattern, flags)?;
        let regex = regexml::Regex::xpath(pattern, flags).map_err(|e| match e {
            regexml::Error::InvalidFlags(_) => invalid_flags(flags),
            other => invalid_regex(pattern, &format!("{other:?}")),
        })?;
        if regex.is_match("") {
            return Err(XsltError::new(
                codes::XTDE1150,
                format!(
                    "xsl:analyze-string: the regular expression {pattern:?} (flags {flags:?}) \
                     matches a zero-length string"
                ),
            ));
        }
        Ok(Self {
            regex,
            pattern: pattern.to_string(),
            flags: flags.to_string(),
        })
    }

    /// The `regexml` program. Crate-private: `regexml` is not part of this
    /// crate's public API.
    pub(crate) fn regex(&self) -> &regexml::Regex {
        &self.regex
    }

    /// The effective value of `regex` this was compiled from.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// The effective value of `flags` this was compiled from.
    pub fn flags(&self) -> &str {
        &self.flags
    }
}

fn invalid_flags(flags: &str) -> XsltError {
    XsltError::new(
        codes::XTDE1145,
        format!(
            "xsl:analyze-string: the flags {flags:?} are not valid: XPath 2.0 defines only s, m, \
             i and x"
        ),
    )
}

fn invalid_regex(pattern: &str, why: &str) -> XsltError {
    XsltError::new(
        codes::XTDE1140,
        format!(
            "xsl:analyze-string: {pattern:?} is not a valid XPath 2.0 regular expression ({why})"
        ),
    )
}

/// The implementation-limit error for a pattern nested `depth` deep
/// ([`MAX_NESTING`]). Not a syntax error — the pattern may well be a valid
/// regular expression — so not XTDE1140: the same internal code as the stack
/// guard's limits (`TransformOptions::stack_limit`), never recovered from.
fn too_deep(depth: usize) -> XsltError {
    XsltError::internal(format!(
        "xsl:analyze-string: the regular expression nests groups and character classes \
         {depth} deep, more than the {MAX_NESTING} this processor allows (an implementation \
         limit: a deeper one could overflow the stack while it is compiled)"
    ))
}

/// Rejects what the XPath 2.0 dialect does not have and `regexml` accepts:
/// any flag other than `s`, `m`, `i` and `x` (XTDE1145), and a `(` followed by
/// `?` outside a character class (XTDE1140). Then refuses a pattern nested
/// deeper than [`MAX_NESTING`] (`XSLT2-INTERNAL`, see `too_deep`).
///
/// Copied from `xsd-schema`'s `check_xpath20_regex_dialect`
/// (`src/xpath/functions/regex.rs`), whose reasoning is unchanged: in the XPath
/// 2.0 grammar a `(` always opens a capturing group and is followed by a
/// branch, and `?` is a quantifier that needs an atom before it, so an
/// unescaped `(` can never be followed by `?`. Under the `x` flag the pattern
/// is checked as it reads after whitespace removal — F&O §7.6.1.1:
/// "whitespace characters (#x9, #xA, #xD and #x20) in the regular expression
/// are removed prior to matching with one exception: whitespace characters
/// within character class expressions (charClassExpr) are not removed" — so
/// `( ?:a)` is `(?:a)`, and a backslash escapes the next character that
/// survives the removal.
///
/// The nesting measure is this crate's addition (the engine's `fn:matches`,
/// `fn:replace` and `fn:tokenize` do not have it yet): the same walk counts
/// the unescaped `(` still open outside a class plus the classes still open,
/// and the deepest point of the pattern is its nesting. It is reported only
/// after the walk, so that a dialect error anywhere in the pattern still wins.
fn check_xpath20_dialect(pattern: &str, flags: &str) -> Result<(), XsltError> {
    if flags.chars().any(|f| !XPATH20_FLAGS.contains(f)) {
        return Err(invalid_flags(flags));
    }

    let ignore_whitespace = flags.contains('x');
    let is_removed = |c: char| matches!(c, '\u{9}' | '\u{A}' | '\u{D}' | ' ');

    let chars: Vec<char> = pattern.chars().collect();
    // `[` opens a character class and, after `-`, a nested subtracted class;
    // inside a class an unescaped `[` or `]` is not allowed otherwise, so a
    // plain depth counter tracks `[a-z-[(?]]` correctly.
    let mut class_depth: usize = 0;
    // The last character kept outside a class was an unescaped `(`.
    let mut after_open_paren = false;
    // Groups open outside a class, and the deepest nesting seen so far
    // (`MAX_NESTING`). A class is only ever open inside the innermost group,
    // so the two depths add up.
    let mut group_depth: usize = 0;
    let mut nesting: usize = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if ignore_whitespace && class_depth == 0 && is_removed(c) {
            continue;
        }
        let opened = after_open_paren;
        after_open_paren = false;
        match c {
            '\\' => {
                if ignore_whitespace && class_depth == 0 {
                    while chars.get(i).is_some_and(|&next| is_removed(next)) {
                        i += 1;
                    }
                }
                i += 1;
            }
            '[' => {
                class_depth += 1;
                nesting = nesting.max(group_depth + class_depth);
            }
            ']' => class_depth = class_depth.saturating_sub(1),
            '(' if class_depth == 0 => {
                after_open_paren = true;
                group_depth += 1;
                nesting = nesting.max(group_depth);
            }
            ')' if class_depth == 0 => group_depth = group_depth.saturating_sub(1),
            '?' if opened => {
                return Err(invalid_regex(
                    pattern,
                    "`(?` has no meaning in XPath 2.0: there are no non-capturing groups",
                ))
            }
            _ => {}
        }
    }
    if nesting > MAX_NESTING {
        return Err(too_deep(nesting));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(pattern: &str, flags: &str) -> String {
        CompiledRegex::new(pattern, flags).unwrap_err().code
    }

    #[test]
    fn the_four_flags_any_order_repeated() {
        for flags in ["", "s", "m", "i", "x", "smix", "xims", "ii", "mmxs"] {
            assert!(CompiledRegex::new("a", flags).is_ok(), "{flags:?}");
        }
    }

    #[test]
    fn other_flags_are_xtde1145() {
        for flags in ["q", "12345", "S", "g", " ", "i "] {
            assert_eq!(code("a", flags), codes::XTDE1145, "{flags:?}");
        }
    }

    #[test]
    fn non_capturing_groups_are_xtde1140() {
        assert_eq!(code("(?:AB)", ""), codes::XTDE1140);
        assert_eq!(code("( ?:AB)", "x"), codes::XTDE1140);
        // Escaped or inside a class it is two ordinary characters.
        assert!(CompiledRegex::new(r"\(?a", "").is_ok());
        assert!(CompiledRegex::new("[(?]", "").is_ok());
    }

    #[test]
    fn syntax_errors_are_xtde1140() {
        for pattern in [
            "[A-Z",
            "(a",
            "a{2,1}",
            r"\1(a)",
            r"(a\1)",
            r"[\1]",
            r"\p{IsNoSuchBlock}",
        ] {
            assert_eq!(code(pattern, ""), codes::XTDE1140, "{pattern:?}");
        }
    }

    fn groups(depth: usize) -> String {
        format!("{}a{}", "(".repeat(depth), ")".repeat(depth))
    }

    fn subtractions(depth: usize) -> String {
        // `[a-z-[a-z-[…[a]…]]]`: `depth` classes, one inside the other.
        format!("{}[a]{}", "[a-z-".repeat(depth - 1), "]".repeat(depth - 1))
    }

    /// `MAX_NESTING`: groups and classes, nested to the limit, compile; one
    /// level deeper is the implementation-limit error, which is not a syntax
    /// error (the pattern is a valid XPath 2.0 regular expression). On the
    /// test's own thread, with no extra stack: the limit is sized so that a
    /// debug build compiles a pattern at the limit in a quarter of the room a
    /// default thread has.
    #[test]
    fn nesting_up_to_the_limit_compiles_and_one_more_is_the_limit_error() {
        let at_limit = CompiledRegex::new(&groups(MAX_NESTING), "").unwrap();
        assert!(at_limit.regex().is_match("xay"));
        assert!(CompiledRegex::new(&subtractions(MAX_NESTING), "").is_ok());
        // A class inside groups: the two depths add up.
        let mixed = |groups: usize, classes: usize| {
            format!(
                "{}{}{}",
                "(".repeat(groups),
                subtractions(classes),
                ")".repeat(groups)
            )
        };
        assert!(CompiledRegex::new(&mixed(MAX_NESTING - 2, 2), "").is_ok());
        for pattern in [
            groups(MAX_NESTING + 1),
            subtractions(MAX_NESTING + 1),
            mixed(MAX_NESTING - 1, 2),
        ] {
            let error = CompiledRegex::new(&pattern, "").unwrap_err();
            assert_eq!(
                error.code,
                crate::error::codes_internal::INTERNAL,
                "{pattern}"
            );
            assert!(error.is_internal());
            assert!(
                error.message.contains(&format!("{} deep", MAX_NESTING + 1))
                    && error
                        .message
                        .contains(&format!("the {MAX_NESTING} this processor allows")),
                "{error}"
            );
        }
    }

    /// What the nesting measure does not count: escaped parentheses, the
    /// parentheses of a character class, and length — a long flat pattern
    /// nests one level deep however long it is (`regexml` compiles it in the
    /// stack of a short one).
    #[test]
    fn only_nesting_counts() {
        let deep = MAX_NESTING * 4;
        let escaped = format!("{}a", r"\(".repeat(deep));
        assert!(CompiledRegex::new(&escaped, "").is_ok());
        let in_a_class = format!("[{}]", "(".repeat(deep));
        assert!(CompiledRegex::new(&in_a_class, "").is_ok());
        let flat = "(a)".repeat(deep);
        assert!(CompiledRegex::new(&flat, "").is_ok());
        let alternatives = format!("{}a", "b|".repeat(deep));
        assert!(CompiledRegex::new(&alternatives, "").is_ok());
        // Under `x`, whitespace between the parentheses changes nothing.
        let spaced = format!(
            "{} a {}",
            "( ".repeat(MAX_NESTING + 1),
            " )".repeat(MAX_NESTING + 1)
        );
        assert!(CompiledRegex::new(&spaced, "x").unwrap_err().is_internal());
    }

    /// The limit is decided before `regexml` sees the pattern, so it holds for
    /// an invalid pattern too (finding out needs the recursion the limit
    /// avoids) — but a dialect error anywhere in the pattern still wins.
    #[test]
    fn the_limit_comes_after_the_dialect_check() {
        let unclosed = "(".repeat(100_000);
        assert!(CompiledRegex::new(&unclosed, "").unwrap_err().is_internal());
        let with_non_capturing = format!("{}(?:a)", groups(MAX_NESTING + 1));
        assert_eq!(code(&with_non_capturing, ""), codes::XTDE1140);
        assert_eq!(code(&groups(MAX_NESTING + 1), "q"), codes::XTDE1145);
    }

    #[test]
    fn zero_length_matches_are_xtde1150() {
        for pattern in ["", "a?", "a*", "(a|)", "^", "$", "[A-Z]?", "^[\t ]*$"] {
            assert_eq!(code(pattern, ""), codes::XTDE1150, "{pattern:?}");
        }
        assert_eq!(code("", "m"), codes::XTDE1150);
        assert!(CompiledRegex::new("a+", "").is_ok());
    }
}
