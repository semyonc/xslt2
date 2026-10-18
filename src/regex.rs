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
//! `x`, in any order and repeatable; without `s`, "the meta-character .
//! matches any character except a newline (#x0A) character".
//!
//! The backend is `regexml` (the `regexml-fork` crate), compiled in its XPath
//! 2.0 dialect, `Regex::xpath20`, which is exactly that language: the flags
//! `s`, `m`, `i`, `x` (anything else, `q` or a `;` included, is
//! `ErrorKind::InvalidFlags`), no `(?` outside a character class (checked as
//! the pattern reads after the `x` flag's whitespace removal), and `.` that
//! excludes only #xA. `xsd-schema` compiles `fn:matches`, `fn:replace` and
//! `fn:tokenize` with the same constructor, so the two agree by construction.
//!
//! The error codes are XSLT's own (§15.1): an invalid regular expression is
//! XTDE1140 where F&O says FORX0002, invalid flags are XTDE1145 where F&O says
//! FORX0001, and a regular expression that matches the zero-length string is
//! XTDE1150.
//!
//! One error is not a syntax error: the engine refuses a pattern that nests
//! groups and character classes more than 64 levels deep
//! (`ErrorKind::NestingTooDeep`), because compiling and matching recurse once
//! per level on the host stack, and a deep enough pattern — which a computed
//! `regex` can take from the input document — would otherwise abort the
//! process. The pattern may well be valid, so this processor reports it as an
//! implementation limit (`XSLT2-INTERNAL`, see `too_deep`), not XTDE1140.
//!
//! > \[§21\] A conforming processor may impose limits on the processing
//! > resources consumed by the processing of a stylesheet.
//!
//! The engine's one limit replaces the 32 levels this crate used to impose
//! before the engine had one (Wave B decision D6). What it costs in stack,
//! measured as the smallest thread stack (arm64, 4 KiB steps, a thread's own
//! floor of 19 KiB included) that runs one phase at 32 / 64 levels:
//!
//! | Phase | debug | release |
//! |---|---|---|
//! | compile `((…(a)…))` (`Regex::xpath20`, its `is_match("")`, the drop) | 243 / 498 KiB | 35 / 83 KiB |
//! | compile `[a-z-[a-z-[…[a]…]]]` | 83 / 163 KiB | 19 / 19 KiB |
//! | match (`analyze`) `((…(a)+…)+)+` | 35 / 83 KiB | 19 / 19 KiB |
//!
//! and nothing grows with the length of a flat pattern — 10 000
//! alternatives or 10 000 `\p{L}` compile in the stack of one. A computed
//! regex is compiled after the run has checked its stack guard
//! (`TransformOptions::stack_limit`), so that compilation comes on top of what
//! the guard allowed; on a thread with Rust's default 2 MiB stack the guard's
//! default for such a thread (`OTHER_THREAD_STACK_LIMIT`, 1 MiB) leaves the
//! other 1 MiB for it — under 0.5 MiB at the limit in a debug build — and for
//! the evaluation between two checks (`tests/m2h_regex.rs`,
//! `a_recursion_compiling_regexes_at_the_limit_ends_in_the_stack_guard_error`,
//! which runs in both profiles).
//! Real regular expressions nest a handful of levels deep: the deepest
//! literal one in the W3C XSLT test suite (758 of them: `xsl:analyze-string`,
//! `matches`, `replace`, `tokenize`) nests 4, in XQTS 1.0.2 2.

use std::fmt;

use regexml::ErrorKind;

use crate::error::{codes, XsltError};

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
    /// The flags are checked before the pattern. A pattern nested deeper than
    /// the engine allows is the implementation-limit error (`XSLT2-INTERNAL`)
    /// — reported as soon as the engine's parser reaches that depth, so an
    /// error that comes later in the pattern is not seen (§2.9: "It is
    /// implementation-dependent which of the several errors is signaled").
    pub(crate) fn new(pattern: &str, flags: &str) -> Result<Self, XsltError> {
        let regex = regexml::Regex::xpath20(pattern, flags).map_err(|e| match e.kind() {
            ErrorKind::InvalidFlags => invalid_flags(flags),
            ErrorKind::NestingTooDeep => too_deep(&e),
            _ => invalid_regex(pattern, &e),
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

fn invalid_regex(pattern: &str, why: &regexml::Error) -> XsltError {
    XsltError::new(
        codes::XTDE1140,
        format!(
            "xsl:analyze-string: {pattern:?} is not a valid XPath 2.0 regular expression ({why})"
        ),
    )
}

/// The implementation-limit error for a pattern nested deeper than the engine
/// allows (`ErrorKind::NestingTooDeep`, 64 levels). Not a syntax error — the
/// pattern may well be a valid regular expression — so not XTDE1140: the same
/// internal code as the stack guard's limits (`TransformOptions::stack_limit`),
/// never recovered from. The engine's message names the limit.
fn too_deep(why: &regexml::Error) -> XsltError {
    XsltError::internal(format!(
        "xsl:analyze-string: the regular expression is refused by an implementation limit of \
         this processor ({why}): a deeper one could overflow the stack while it is compiled"
    ))
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

    /// The engine's nesting limit (`ErrorKind::NestingTooDeep`).
    const NESTING_LIMIT: usize = 64;

    fn assert_too_deep(error: &XsltError, what: &str) {
        assert_eq!(
            error.code,
            crate::error::codes_internal::INTERNAL,
            "{what}: {error}"
        );
        assert!(error.is_internal(), "{what}");
        assert!(
            error.message.contains("implementation limit")
                && error.message.contains(&format!("{NESTING_LIMIT} levels")),
            "{what}: {error}"
        );
    }

    /// The engine's limit: groups and classes, nested to it, compile; one
    /// level deeper is the implementation-limit error, which is not a syntax
    /// error (the pattern is a valid XPath 2.0 regular expression). On the
    /// test's own thread, with no extra stack, in whatever profile `cargo
    /// test` builds.
    #[test]
    fn nesting_up_to_the_limit_compiles_and_one_more_is_the_limit_error() {
        let at_limit = CompiledRegex::new(&groups(NESTING_LIMIT), "").unwrap();
        assert!(at_limit.regex().is_match("xay"));
        assert!(CompiledRegex::new(&subtractions(NESTING_LIMIT), "").is_ok());
        // A class inside groups: the two depths add up.
        let mixed = |groups: usize, classes: usize| {
            format!(
                "{}{}{}",
                "(".repeat(groups),
                subtractions(classes),
                ")".repeat(groups)
            )
        };
        assert!(CompiledRegex::new(&mixed(NESTING_LIMIT - 2, 2), "").is_ok());
        for pattern in [
            groups(NESTING_LIMIT + 1),
            subtractions(NESTING_LIMIT + 1),
            mixed(NESTING_LIMIT - 1, 2),
        ] {
            let error = CompiledRegex::new(&pattern, "").unwrap_err();
            assert_too_deep(&error, &pattern);
        }
    }

    /// What the nesting measure does not count: escaped parentheses, the
    /// parentheses of a character class, and length — a long flat pattern
    /// nests one level deep however long it is (`regexml` compiles it in the
    /// stack of a short one).
    #[test]
    fn only_nesting_counts() {
        let deep = NESTING_LIMIT * 4;
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
            "( ".repeat(NESTING_LIMIT + 1),
            " )".repeat(NESTING_LIMIT + 1)
        );
        assert_too_deep(&CompiledRegex::new(&spaced, "x").unwrap_err(), "spaced");
    }

    /// The flags are checked first; then the pattern is read from the left,
    /// so whichever of a dialect error and the nesting limit comes first in
    /// it is reported (§2.9: "It is implementation-dependent which of the
    /// several errors is signaled"). The limit holds for an invalid pattern
    /// too: 100 000 unclosed groups are refused at the 65th, before the
    /// recursion the limit avoids.
    #[test]
    fn the_flags_come_first_then_the_pattern_from_the_left() {
        let unclosed = "(".repeat(100_000);
        assert_too_deep(&CompiledRegex::new(&unclosed, "").unwrap_err(), "unclosed");
        let deep_then_non_capturing = format!("{}(?:a)", groups(NESTING_LIMIT + 1));
        assert_too_deep(
            &CompiledRegex::new(&deep_then_non_capturing, "").unwrap_err(),
            "deep, then (?:",
        );
        let non_capturing_then_deep = format!("(?:a){}", groups(NESTING_LIMIT + 1));
        assert_eq!(code(&non_capturing_then_deep, ""), codes::XTDE1140);
        assert_eq!(code(&groups(NESTING_LIMIT + 1), "q"), codes::XTDE1145);
    }

    /// F&O 1.0 §7.6.1.1: "If the s flag is not specified, the meta-character
    /// . matches any character except a newline (#x0A) character" — a
    /// carriage return is matched (F&O 3.1 later excluded it too). And the `x`
    /// flag removes "whitespace characters (#x9, #xA, #xD and #x20)" only, so
    /// a form feed stays.
    #[test]
    fn dot_and_the_x_flag_follow_f_and_o_1_0() {
        let dot = CompiledRegex::new("^a.b$", "").unwrap();
        assert!(dot.regex().is_match("a\rb"));
        assert!(!dot.regex().is_match("a\nb"));
        assert!(CompiledRegex::new("^a.b$", "s")
            .unwrap()
            .regex()
            .is_match("a\nb"));
        let form_feed = CompiledRegex::new("^(\u{C}?:a)$", "x").unwrap();
        assert!(form_feed.regex().is_match(":a"));
        assert!(form_feed.regex().is_match("\u{C}:a"));
        assert!(!form_feed.regex().is_match("a"));
    }

    /// The engine's own flags after a `;` are not XPath 2.0 flags.
    #[test]
    fn engine_flags_are_xtde1145() {
        for flags in [";k", "i;k", ";"] {
            assert_eq!(code("a", flags), codes::XTDE1145, "{flags:?}");
        }
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
