//! Number to string conversion — XSLT 2.0 §12.3, "Number to String Conversion
//! Attributes".
//!
//! This module is pure: it knows nothing of XPath values, trees or the
//! dynamic context. It takes the integers of a place marker and the effective
//! values of `format`, `lang`, `ordinal`, `letter-value`, `grouping-separator`
//! and `grouping-size`, and returns the string. `xsl:number` (§12) is its
//! first caller; `format-date()` and its siblings (§16.5.1) are the
//! second — "the first presentation modifier … may be any format token
//! permitted in the format string of the `xsl:number` instruction", and the
//! `t` / `o` modifiers ask for `letter-value="traditional"` / ordinal numbering
//! — through [`FormatToken::parse`] (an unsupported token is
//! [`FormatToken::Other`]) and [`FormatToken::format_in_width`], which adds
//! §16.5.1's minimum and maximum widths.
//!
//! # What this implementation supports (the implementation-defined parts)
//!
//! - **Unicode 15.1** for every character class here — the alphanumeric
//!   characters that make up a format token (`Nd`, `Nl`, `No`, `Lu`, `Ll`,
//!   `Lt`, `Lm`, `Lo`, through `regexml`'s `\p{L}` and `\p{N}`) and the decimal
//!   digit families: the version `regexml` (ICU4X 1.5) implements, so a format
//!   token splits where the run's regular expressions see the same categories.
//!   A character Unicode 16.0 added is unassigned here, digit or letter, and
//!   separates tokens.
//! - **Decimal digits of every Unicode `Nd` family**, padded to the width of
//!   the token: `1`, `01`, `&#x661;` (Arabic-Indic), `&#xFF11;` (fullwidth),
//!   `&#x1D7CF;` (mathematical bold)… — the only sequences §12.3 prescribes in
//!   detail. No upper bound: an integer above `u64::MAX` is formatted from its
//!   decimal digits ([`Numeral::Big`]); the lower bound is 0, as for the token
//!   `1`.
//! - `A` / `a`: the English alphabet, continued as `Z`, `AA`, `AB` … (bijective
//!   base 26, the spec's "A B C ... Z AA AB AC...."), 1 to `u64::MAX`.
//! - `I` / `i`: standard Roman numerals (`IV`, `IX`, `XL` …), 1 to
//!   [`ROMAN_MAX`] = 3999.
//! - `W` / `w` / `Ww`: English cardinal words ("one hundred and thirty four",
//!   "two thousand and two" — tens and units separated by a space, `and`
//!   before a last group below one hundred, the short scale up to
//!   quintillion), 0 to `u64::MAX`.
//! - `ordinal`: English, for the decimal tokens (`1st 2nd 3rd 4th … 11th 12th
//!   13th … 21st … 101st 111th`) and the word tokens (`first second third …
//!   twentieth twenty-first … one hundred and first`; the final tens and units
//!   hyphenated, as Saxon writes them — `Twenty-First`, `TWENTY-FIRST`). With
//!   `A`, `a`, `I`, `i` the request is ignored, as §12.3 allows.
//! - `lang`: English only. "If a language is requested that is not supported,
//!   the processor uses the language that it would use if the lang attribute
//!   were omitted", which is English.
//! - `letter-value`: accepted and has no effect — in English the token alone
//!   is never ambiguous (`a` is the alphabetic sequence, `i` the traditional
//!   one, and each has only that one).
//! - **Seventeen sequences of consecutive code points** ([`SEQUENCES`]), each
//!   selected by its character for one as a token of its own and limited to
//!   what the Unicode repertoire encodes ("the formatting token &#x2460;
//!   (circled digit one) has a range of 1 to 20 imposed by the Unicode
//!   character repertoire"): circled digits `⓪ ① … ⑳ ㉑ … ㊿` (0–50, from
//!   three blocks), `🄀 ⒈ … ⒛` (0–20), `⓿ ❶ … ❿ ⓫ … ⓴` (0–20), `🄋 ➀ … ➉`
//!   and `🄌 ➊ … ➓` (0–10), `⑴ … ⒇` (1–20), `⓵ … ⓾`, `㈠ … ㈩`, `㊀ … ㊉` and
//!   the Aegean, Coptic epact, Rumi, Brahmi and Sinhala archaic one to ten
//!   (1–10), `🄁 🄂 … 🄊` (0–9), counting-rod units and Mende Kikakui digits
//!   (1–9) — the `combinations_for_numbering` values of the W3C catalog but
//!   the two Greek ones. Not decimal sequences, so not grouped; `ordinal` is
//!   ignored for them, as for `A` and `I`.
//! - **Every other format token falls back to `1`**, as §12.3 requires of an
//!   unsupported one: Greek, Hebrew, Cyrillic, Katakana, superscript and
//!   subscript digits…
//!
//! A number outside a sequence's range is formatted with the token `1` (§12.3:
//! "Numbers that fall outside this range must be formatted using the format
//! token 1"), with grouping and ordinal as for that token.

/// The largest number the Roman numeral tokens `I` and `i` format; above it
/// the token `1` is used. §12.3 asks for at least 1000.
pub(crate) const ROMAN_MAX: u64 = 3999;

/// One integer of a place marker (§12), or the `NaN` that backwards-compatible
/// behaviour puts in its place (§12.1: "the string NaN is inserted into the
/// formatted result string in its proper position").
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Numeral {
    /// A non-negative integer.
    Int(u64),
    /// A non-negative integer above `u64::MAX`, as its ASCII decimal digits
    /// without leading zeros. Only the decimal-digit tokens format it; every
    /// other token falls back to `1` for it.
    Big(String),
    /// Formatted as the string `NaN` by every token.
    NaN,
}

impl Numeral {
    /// The ASCII decimal digits of the number (`None` for `NaN`).
    fn ascii_digits(&self) -> Option<String> {
        match self {
            Numeral::Int(n) => Some(n.to_string()),
            Numeral::Big(digits) => Some(digits.clone()),
            Numeral::NaN => None,
        }
    }
}

/// How the word tokens case their output (§12.3: `w`, `W`, `Ww`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WordCase {
    /// `w`: "numbers written as lower-case words".
    Lower,
    /// `W`: "numbers written as upper-case words".
    Upper,
    /// `Ww`: "numbers written as title-case words".
    Title,
}

/// One format token: "a maximal sequence of alphanumeric characters" of the
/// format string (§12.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FormatToken {
    /// > Any token where the last character has a decimal digit value of 1 (as
    /// > specified in the Unicode character property database, see
    /// > \[Unicode\]), and the Unicode value of preceding characters is one less
    /// > than the Unicode value of the last character generates a decimal
    /// > representation of the number where each number is at least as long as
    /// > the format token.
    ///
    /// `zero` is the family's digit zero, `width` the token's length.
    Decimal { zero: char, width: usize },
    /// `A` / `a`.
    Alphabetic { upper: bool },
    /// `I` / `i`.
    Roman { upper: bool },
    /// `w` / `W` / `Ww`.
    Words(WordCase),
    /// > Any other format token indicates a numbering sequence in which that
    /// > token represents the number 1 (one)
    ///
    /// — one of [`SEQUENCES`], whose character for one is the whole token.
    Sequence(&'static NumberingSequence),
    /// > It is implementation-defined which numbering sequences, additional
    /// > to those listed above, are supported. If an implementation does not
    /// > support a numbering sequence represented by the given token, it must
    /// > use a format token of 1.
    ///
    /// Formatted as the token `1`.
    Other(String),
}

/// A numbering sequence of characters that the Unicode Standard encodes for
/// the numbers themselves — ① for one, ② for two … — in runs of consecutive
/// code points, with or without a character for zero (§12.3: "Any other
/// format token indicates a numbering sequence in which that token represents
/// the number 1 (one)").
///
/// Its range is what the repertoire has: "there may be implementation-defined
/// lower and upper bounds on the range of numbers that can be formatted using
/// this format token; indeed, for some numbering sequences there may be
/// intrinsic limits. For example, the formatting token &#x2460; (circled
/// digit one) has a range of 1 to 20 imposed by the Unicode character
/// repertoire" — here 0 to 50, because the repertoire also has `⓪` and
/// `㉑`–`㊿` in two other blocks. "Numbers that fall outside this range must
/// be formatted using the format token 1."
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NumberingSequence {
    /// The character for zero, if the repertoire has one.
    zero: Option<char>,
    /// The runs of consecutive code points, in increasing order of the
    /// numbers they stand for, each following on from the one before: the
    /// first number of the run, the code point for it, and how many numbers
    /// the run has. The first run starts at 1, with the token.
    runs: &'static [(u64, u32, u64)],
}

impl NumberingSequence {
    /// The token: the character for one.
    fn one(&self) -> char {
        char::from_u32(self.runs[0].1).unwrap_or('1')
    }

    /// The smallest and the largest number the sequence formats.
    #[cfg(test)]
    fn range(&self) -> (u64, u64) {
        let (first, _, length) = self.runs[self.runs.len() - 1];
        (u64::from(self.zero.is_none()), first + length - 1)
    }

    /// The character for `n`, or `None` outside the range.
    fn character(&self, n: u64) -> Option<char> {
        if n == 0 {
            return self.zero;
        }
        let &(first, code, _) = self
            .runs
            .iter()
            .find(|&&(first, _, length)| (first..first + length).contains(&n))?;
        // In range: `n - first` is less than the run's length, which fits
        // the code points the table names.
        char::from_u32(code + (n - first) as u32)
    }
}

/// The numbering sequences of consecutive code points this implementation
/// supports (§12.3: "It is implementation-defined which numbering sequences,
/// additional to those listed above, are supported"). Each entry is headed by
/// the Unicode name of its character for one, which is how the W3C catalog's
/// `combinations_for_numbering` dependency names the sequence. Every code
/// point is assigned in **Unicode 15.1**, the version of the rest of this
/// module (general category `No`: checked against `regexml`'s `\p{No}` by
/// `tests::every_sequence_character_is_no_in_the_regex_unicode`), and has the
/// numeric value it stands for (names and values checked against the Unicode
/// character database when the table was written).
///
/// Not here: the Greek letters (the catalog's `GREEK SMALL LETTER ALPHA`
/// cases want an alphabet run and its traditional numerals), and sequences
/// that continue past their consecutive run by addition (Aegean, Coptic
/// epact, Rumi, Brahmi and Sinhala archaic numbers go on with twenty, thirty
/// …, which this table does not compose: 11 and above use the token `1`).
pub(crate) const SEQUENCES: [NumberingSequence; 17] = [
    // CIRCLED DIGIT ONE
    NumberingSequence {
        // CIRCLED DIGIT ZERO
        zero: Some('\u{24EA}'),
        // ① … ⑳ (CIRCLED NUMBER TWENTY), ㉑ … ㉟, ㊱ … ㊿ (CIRCLED NUMBER FIFTY)
        runs: &[(1, 0x2460, 20), (21, 0x3251, 15), (36, 0x32B1, 15)],
    },
    // DIGIT ONE FULL STOP
    NumberingSequence {
        // DIGIT ZERO FULL STOP
        zero: Some('\u{1F100}'),
        // ⒈ … ⒛ (NUMBER TWENTY FULL STOP)
        runs: &[(1, 0x2488, 20)],
    },
    // DINGBAT NEGATIVE CIRCLED DIGIT ONE
    NumberingSequence {
        // NEGATIVE CIRCLED DIGIT ZERO
        zero: Some('\u{24FF}'),
        // ❶ … ❿ (DINGBAT NEGATIVE CIRCLED NUMBER TEN), ⓫ (NEGATIVE CIRCLED
        // NUMBER ELEVEN) … ⓴ (TWENTY)
        runs: &[(1, 0x2776, 10), (11, 0x24EB, 10)],
    },
    // DINGBAT CIRCLED SANS-SERIF DIGIT ONE
    NumberingSequence {
        // DINGBAT CIRCLED SANS-SERIF DIGIT ZERO
        zero: Some('\u{1F10B}'),
        // ➀ … ➉ (DINGBAT CIRCLED SANS-SERIF NUMBER TEN)
        runs: &[(1, 0x2780, 10)],
    },
    // DINGBAT NEGATIVE CIRCLED SANS-SERIF DIGIT ONE
    NumberingSequence {
        // DINGBAT NEGATIVE CIRCLED SANS-SERIF DIGIT ZERO
        zero: Some('\u{1F10C}'),
        // ➊ … ➓ (DINGBAT NEGATIVE CIRCLED SANS-SERIF NUMBER TEN)
        runs: &[(1, 0x278A, 10)],
    },
    // PARENTHESIZED DIGIT ONE
    NumberingSequence {
        zero: None,
        // ⑴ … ⒇ (PARENTHESIZED NUMBER TWENTY)
        runs: &[(1, 0x2474, 20)],
    },
    // DOUBLE CIRCLED DIGIT ONE
    NumberingSequence {
        zero: None,
        // ⓵ … ⓾ (DOUBLE CIRCLED NUMBER TEN)
        runs: &[(1, 0x24F5, 10)],
    },
    // PARENTHESIZED IDEOGRAPH ONE
    NumberingSequence {
        zero: None,
        // ㈠ … ㈩ (PARENTHESIZED IDEOGRAPH TEN)
        runs: &[(1, 0x3220, 10)],
    },
    // CIRCLED IDEOGRAPH ONE
    NumberingSequence {
        zero: None,
        // ㊀ … ㊉ (CIRCLED IDEOGRAPH TEN)
        runs: &[(1, 0x3280, 10)],
    },
    // AEGEAN NUMBER ONE
    NumberingSequence {
        zero: None,
        // AEGEAN NUMBER ONE … NINE, AEGEAN NUMBER TEN
        runs: &[(1, 0x10107, 10)],
    },
    // COPTIC EPACT DIGIT ONE
    NumberingSequence {
        zero: None,
        // COPTIC EPACT DIGIT ONE … NINE, COPTIC EPACT NUMBER TEN
        runs: &[(1, 0x102E1, 10)],
    },
    // RUMI DIGIT ONE
    NumberingSequence {
        zero: None,
        // RUMI DIGIT ONE … NINE, RUMI NUMBER TEN
        runs: &[(1, 0x10E60, 10)],
    },
    // BRAHMI NUMBER ONE
    NumberingSequence {
        zero: None,
        // BRAHMI NUMBER ONE … NINE, BRAHMI NUMBER TEN
        runs: &[(1, 0x11052, 10)],
    },
    // SINHALA ARCHAIC DIGIT ONE
    NumberingSequence {
        zero: None,
        // SINHALA ARCHAIC DIGIT ONE … NINE, SINHALA ARCHAIC NUMBER TEN
        runs: &[(1, 0x111E1, 10)],
    },
    // DIGIT ONE COMMA
    NumberingSequence {
        // DIGIT ZERO COMMA
        zero: Some('\u{1F101}'),
        // 🄂 … 🄊 (DIGIT NINE COMMA)
        runs: &[(1, 0x1F102, 9)],
    },
    // COUNTING ROD UNIT DIGIT ONE
    NumberingSequence {
        zero: None,
        // COUNTING ROD UNIT DIGIT ONE … NINE (the tens digits that follow
        // are another sequence)
        runs: &[(1, 0x1D360, 9)],
    },
    // MENDE KIKAKUI DIGIT ONE
    NumberingSequence {
        zero: None,
        // MENDE KIKAKUI DIGIT ONE … NINE
        runs: &[(1, 0x1E8C7, 9)],
    },
];

/// The token `1`: the default (§12.3 "If there are no format tokens, then a
/// format token of 1 is used to format all numbers") and the fallback.
const DECIMAL_ONE: FormatToken = FormatToken::Decimal {
    zero: '0',
    width: 1,
};

/// `grouping-separator` with `grouping-size` (§12.3). Only built when both are
/// present: "If only one of the grouping-separator and grouping-size attributes
/// is specified, then it is ignored."
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grouping {
    pub(crate) separator: String,
    /// 0 means no grouping.
    pub(crate) size: usize,
}

/// The effective values of the §12.3 attributes other than `format` that
/// change what English formatting produces.
///
/// `lang` and `letter-value` are not here: the caller checks their values
/// ([`check_attribute`]), and neither changes the result — every language is
/// formatted in English (the fallback §12.3 prescribes for an unsupported
/// one), and in English the token alone names the sequence (`a` the
/// alphabetic one, `i` the traditional one). A second language would add its
/// field here.
#[derive(Clone, Debug, Default)]
pub(crate) struct Conversion {
    /// `ordinal`; "if its value is not a zero-length string, this indicates a
    /// request to output ordinal numbers rather than cardinal numbers".
    pub(crate) ordinal: Option<String>,
    pub(crate) grouping: Option<Grouping>,
}

impl Conversion {
    fn ordinal_requested(&self) -> bool {
        self.ordinal.as_deref().is_some_and(|o| !o.is_empty())
    }
}

/// A `format` string split into its prefix, format tokens with their
/// separators, and suffix (§12.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NumberFormat {
    prefix: String,
    /// The format tokens in order, each with the separator token that precedes
    /// it (empty for the first, which has none).
    tokens: Vec<(String, FormatToken)>,
    suffix: String,
}

impl NumberFormat {
    /// Splits a `format` string (§12.3).
    ///
    /// > The format attribute is split into a sequence of tokens where each
    /// > token is a maximal sequence of alphanumeric characters or a maximal
    /// > sequence of non-alphanumeric characters. … Each non-alphanumeric token
    /// > is either a prefix, a separator, or a suffix. If there is a
    /// > non-alphanumeric token but no format token, then the single
    /// > non-alphanumeric token is used as both the prefix and the suffix. The
    /// > prefix, if it exists, is the non-alphanumeric token that precedes the
    /// > first format token … The suffix, if it exists, is the non-alphanumeric
    /// > token that follows the last format token … All other non-alphanumeric
    /// > tokens (those that occur between two format tokens) are separator
    /// > tokens.
    pub(crate) fn parse(format: &str) -> NumberFormat {
        let mut runs: Vec<(bool, String)> = Vec::new();
        for c in format.chars() {
            let alphanumeric = is_alphanumeric(c);
            match runs.last_mut() {
                Some((kind, text)) if *kind == alphanumeric => text.push(c),
                _ => runs.push((alphanumeric, c.to_string())),
            }
        }
        if !runs.iter().any(|(alphanumeric, _)| *alphanumeric) {
            // At most one run, and it is non-alphanumeric.
            let only = runs.pop().map(|(_, text)| text).unwrap_or_default();
            return NumberFormat {
                prefix: only.clone(),
                tokens: Vec::new(),
                suffix: only,
            };
        }
        let mut prefix = String::new();
        let mut tokens: Vec<(String, FormatToken)> = Vec::new();
        let mut pending = String::new();
        for (alphanumeric, text) in runs {
            if alphanumeric {
                tokens.push((std::mem::take(&mut pending), FormatToken::parse(&text)));
            } else if tokens.is_empty() {
                prefix = text;
            } else {
                pending = text;
            }
        }
        NumberFormat {
            prefix,
            tokens,
            suffix: pending,
        }
    }

    /// Formats a place marker (§12.3).
    ///
    /// > The nth format token is used to format the nth number in the
    /// > sequence. If there are more numbers than format tokens, then the last
    /// > format token is used to format remaining numbers. If there are no
    /// > format tokens, then a format token of 1 is used to format all numbers.
    /// > Each number after the first is separated from the preceding number by
    /// > the separator token preceding the format token used to format that
    /// > number, or, if that is the first format token, then by . (dot).
    ///
    /// The prefix and the suffix appear exactly once each, also around an
    /// empty sequence.
    pub(crate) fn format(&self, numbers: &[Numeral], conversion: &Conversion) -> String {
        let mut out = self.prefix.clone();
        for (i, number) in numbers.iter().enumerate() {
            let (separator, token) = match self.tokens.len() {
                0 => (".", &DECIMAL_ONE),
                len => {
                    let j = i.min(len - 1);
                    let (separator, token) = &self.tokens[j];
                    (if j == 0 { "." } else { separator.as_str() }, token)
                }
            };
            if i > 0 {
                out.push_str(separator);
            }
            out.push_str(&token.format(number, conversion));
        }
        out.push_str(&self.suffix);
        out
    }
}

impl FormatToken {
    /// Classifies one format token (an alphanumeric run of a `format` string,
    /// or a presentation modifier of `format-date()`).
    pub(crate) fn parse(token: &str) -> FormatToken {
        let chars: Vec<char> = token.chars().collect();
        if let Some(&last) = chars.last() {
            if let Some((zero, 1)) = decimal_digit(last) {
                if chars[..chars.len() - 1].iter().all(|&c| c == zero) {
                    return FormatToken::Decimal {
                        zero,
                        width: chars.len(),
                    };
                }
            }
        }
        match token {
            "A" => FormatToken::Alphabetic { upper: true },
            "a" => FormatToken::Alphabetic { upper: false },
            "I" => FormatToken::Roman { upper: true },
            "i" => FormatToken::Roman { upper: false },
            "W" => FormatToken::Words(WordCase::Upper),
            "w" => FormatToken::Words(WordCase::Lower),
            "Ww" => FormatToken::Words(WordCase::Title),
            other => match chars.as_slice() {
                [one] => SEQUENCES
                    .iter()
                    .find(|sequence| sequence.one() == *one)
                    .map_or_else(
                        || FormatToken::Other(other.to_string()),
                        FormatToken::Sequence,
                    ),
                _ => FormatToken::Other(other.to_string()),
            },
        }
    }

    /// Formats one number with this token.
    pub(crate) fn format(&self, number: &Numeral, conversion: &Conversion) -> String {
        if *number == Numeral::NaN {
            return "NaN".to_string();
        }
        let ordinal = conversion.ordinal_requested();
        let grouping = conversion.grouping.as_ref();
        let in_range = match (self, number) {
            (FormatToken::Decimal { zero, width }, _) => {
                return decimal(number, *zero, *width, grouping, ordinal)
            }
            (FormatToken::Alphabetic { upper }, Numeral::Int(n)) if *n >= 1 => {
                Some(alphabetic(*n, *upper))
            }
            (FormatToken::Roman { upper }, Numeral::Int(n)) if (1..=ROMAN_MAX).contains(n) => {
                Some(roman(*n, *upper))
            }
            (FormatToken::Words(case), Numeral::Int(n)) => Some(if ordinal {
                apply_case(&ordinal_words(*n), *case)
            } else {
                apply_case(&cardinal_words(*n), *case)
            }),
            // Ordinal numbering is not supported for these: "the request is
            // ignored and cardinal numbers are generated instead".
            (FormatToken::Sequence(sequence), Numeral::Int(n)) => {
                sequence.character(*n).map(String::from)
            }
            _ => None,
        };
        // "Numbers that fall outside this range must be formatted using the
        // format token 1", and so must every number of an unsupported token.
        in_range.unwrap_or_else(|| decimal(number, '0', 1, grouping, ordinal))
    }
}

impl FormatToken {
    /// Formats `n` for a variable marker of `format-date()` (§16.5.1), whose
    /// width modifier this token's own width does not decide: the caller
    /// passes the effective minimum and maximum width.
    ///
    /// - A decimal token pads with zero digits of its family to `min` ("For
    ///   decimal representations of numbers, this should be done by
    ///   prepending zero digits from the appropriate set of digit
    ///   characters"). When `drop_high_order` is set (the year), a number wider
    ///   than `max` keeps only its last `max` digits: "In the case of the year
    ///   component, setting max-width requests omission of high-order digits
    ///   from the year, for example, if max-width is set to 2 then the year
    ///   2003 will be output as 03." Any other number is left wider than `max`:
    ///   "If no mechanism is available for fitting the value within the
    ///   specified maximum width … then the value should be output in its full
    ///   representation."
    /// - Every other token (and the fallback to `1` for a number out of its
    ///   range) is padded with spaces to `min`: "In other cases, it should be
    ///   done by appending spaces."
    ///
    /// `in_characters` says what `min` and `max` count, which matters only for
    /// an ordinal decimal number (`3rd`):
    ///
    /// - `true`, a width modifier: characters — "min-width is either an
    ///   unsigned integer indicating the minimum number of characters to be
    ///   output", and padding applies "If the full representation of the value
    ///   is shorter than the specified minimum width" — so the suffix counts:
    ///   `[D1o,2]` and `[D1o,3-3]` are `3rd`, `[D1o,4]` is `03rd`, and a year
    ///   keeps fewer digits to leave room for it (`[Y1o,4-4]` is `03rd` for
    ///   2003). When not even one digit fits beside the suffix (`[Y1o,2-2]`),
    ///   no mechanism fits the value, and it is written in full;
    /// - `false`, the token's own width (`[D01o]`, no width modifier): digits,
    ///   as §12.3 reads the token for `xsl:number` ("each number is at least as
    ///   long as the format token") — `03rd`.
    ///
    /// `t` (`letter-value="traditional"`) needs nothing here: as for
    /// `xsl:number`, in English the token alone names the sequence.
    pub(crate) fn format_in_width(
        &self,
        n: u64,
        conversion: &Conversion,
        min: usize,
        max: Option<usize>,
        drop_high_order: bool,
        in_characters: bool,
    ) -> String {
        if let FormatToken::Decimal { zero, .. } = self {
            let ordinal = conversion.ordinal_requested();
            // What the suffix takes of a width in characters.
            let suffix = if ordinal && in_characters {
                ORDINAL_SUFFIX_LEN
            } else {
                0
            };
            let min_digits = min.saturating_sub(suffix);
            let ascii = n.to_string();
            let (n, width) = match max.map(|max| max.saturating_sub(suffix)) {
                Some(room) if drop_high_order && room > 0 && ascii.len() > room => {
                    let kept = &ascii[ascii.len() - room..];
                    (kept.parse().unwrap_or(0), room.max(min_digits))
                }
                _ => (n, min_digits),
            };
            return decimal(&Numeral::Int(n), *zero, width, None, ordinal);
        }
        let mut out = self.format(&Numeral::Int(n), conversion);
        let len = out.chars().count();
        out.extend(std::iter::repeat_n(' ', min.saturating_sub(len)));
        out
    }
}

/// A decimal representation in the digit family whose zero is `zero`, padded
/// with that zero to `width` digits, grouped, and with the English ordinal
/// suffix if asked for.
///
/// Grouping applies to the padded digits (`format="00000000000001"`
/// `grouping-separator="/"` `grouping-size="2"` gives `00/00/00/01/00/00/00`
/// for a million: `number-0803`).
fn decimal(
    number: &Numeral,
    zero: char,
    width: usize,
    grouping: Option<&Grouping>,
    ordinal: bool,
) -> String {
    let Some(ascii) = number.ascii_digits() else {
        return "NaN".to_string();
    };
    let mut digits: Vec<char> = Vec::with_capacity(ascii.len().max(width));
    digits.extend(std::iter::repeat_n(zero, width.saturating_sub(ascii.len())));
    digits.extend(
        ascii
            .bytes()
            .map(|d| char::from_u32(zero as u32 + u32::from(d - b'0')).unwrap_or(d as char)),
    );
    let mut out = String::new();
    match grouping {
        Some(g) if g.size > 0 && !g.separator.is_empty() => {
            // Groups of `size` counted from the right: the first, leftmost
            // group has the remainder (or a whole group).
            let mut left_in_group = match digits.len() % g.size {
                0 => g.size,
                rest => rest,
            };
            for d in digits {
                if left_in_group == 0 {
                    out.push_str(&g.separator);
                    left_in_group = g.size;
                }
                out.push(d);
                left_in_group -= 1;
            }
        }
        _ => out.extend(digits),
    }
    if ordinal {
        out.push_str(english_ordinal_suffix(&ascii));
    }
    out
}

/// The length of every English ordinal suffix ([`english_ordinal_suffix`]).
const ORDINAL_SUFFIX_LEN: usize = 2;

/// `st`, `nd`, `rd` or `th` for the number whose ASCII digits are `ascii`:
/// `th` for anything ending in 11, 12 or 13.
fn english_ordinal_suffix(ascii: &str) -> &'static str {
    let bytes = ascii.as_bytes();
    let last = bytes.last().copied().unwrap_or(b'0');
    let tens = if bytes.len() >= 2 {
        bytes[bytes.len() - 2]
    } else {
        b'0'
    };
    if tens == b'1' {
        return "th";
    }
    match last {
        b'1' => "st",
        b'2' => "nd",
        b'3' => "rd",
        _ => "th",
    }
}

/// `A B C ... Z AA AB AC ...` (or lower case): bijective base 26. `n` ≥ 1.
fn alphabetic(mut n: u64, upper: bool) -> String {
    let base = if upper { b'A' } else { b'a' };
    let mut reversed = Vec::new();
    while n > 0 {
        n -= 1;
        reversed.push((base + (n % 26) as u8) as char);
        n /= 26;
    }
    reversed.iter().rev().collect()
}

/// Standard Roman numerals; `n` in 1..=[`ROMAN_MAX`].
fn roman(mut n: u64, upper: bool) -> String {
    const VALUES: [(u64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, letters) in VALUES {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    if upper {
        out.to_ascii_uppercase()
    } else {
        out
    }
}

const UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const ORDINAL_UNITS: [&str; 20] = [
    "zeroth",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
];

/// Indexed by the tens digit (2–9).
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

const ORDINAL_TENS: [&str; 10] = [
    "",
    "",
    "twentieth",
    "thirtieth",
    "fortieth",
    "fiftieth",
    "sixtieth",
    "seventieth",
    "eightieth",
    "ninetieth",
];

/// The short-scale names, largest first; `u64::MAX` is about 18.4 quintillion.
const SCALES: [(u64, &str); 6] = [
    (1_000_000_000_000_000_000, "quintillion"),
    (1_000_000_000_000_000, "quadrillion"),
    (1_000_000_000_000, "trillion"),
    (1_000_000_000, "billion"),
    (1_000_000, "million"),
    (1_000, "thousand"),
];

/// English cardinal words, lower case: "one hundred and thirty four", "two
/// thousand and two", "one thousand one hundred".
fn cardinal_words(n: u64) -> String {
    for (scale, name) in SCALES {
        if n >= scale {
            let head = format!("{} {name}", cardinal_words(n / scale));
            let rest = n % scale;
            return match rest {
                0 => head,
                r if r < 100 => format!("{head} and {}", cardinal_words(r)),
                r => format!("{head} {}", cardinal_words(r)),
            };
        }
    }
    if n >= 100 {
        let head = format!("{} hundred", UNITS[(n / 100) as usize]);
        return match n % 100 {
            0 => head,
            r => format!("{head} and {}", cardinal_words(r)),
        };
    }
    if n < 20 {
        return UNITS[n as usize].to_string();
    }
    match n % 10 {
        0 => TENS[(n / 10) as usize].to_string(),
        u => format!("{} {}", TENS[(n / 10) as usize], UNITS[u as usize]),
    }
}

/// English ordinal words, lower case: "first", "twenty-first", "one hundredth",
/// "two thousand and second", "twenty one thousand and twenty-first".
///
/// As Saxon 9.1 writes them (checked from 1 to 1,234,567): the tens and the
/// units of the last group below one hundred are hyphenated when the number
/// ends in them ("twenty-first", "one hundred and ninety-ninth"), and
/// everything before stays as in the cardinal ("twenty one thousandth", "thirty
/// four thousand … sixty-seventh"). The cardinal words have no hyphen at all.
fn ordinal_words(n: u64) -> String {
    for (scale, name) in SCALES {
        if n >= scale {
            let head = format!("{} {name}", cardinal_words(n / scale));
            let rest = n % scale;
            return match rest {
                0 => format!("{head}th"),
                r if r < 100 => format!("{head} and {}", ordinal_words(r)),
                r => format!("{head} {}", ordinal_words(r)),
            };
        }
    }
    if n >= 100 {
        let head = format!("{} hundred", UNITS[(n / 100) as usize]);
        return match n % 100 {
            0 => format!("{head}th"),
            r => format!("{head} and {}", ordinal_words(r)),
        };
    }
    if n < 20 {
        return ORDINAL_UNITS[n as usize].to_string();
    }
    match n % 10 {
        0 => ORDINAL_TENS[(n / 10) as usize].to_string(),
        u => format!("{}-{}", TENS[(n / 10) as usize], ORDINAL_UNITS[u as usize]),
    }
}

/// `words` (lower case, ASCII) in the case the token asks for. Title case
/// capitalizes every word but `and`, and each part of a hyphenated one ("One
/// Hundred and Thirty Four", "Twenty-First").
fn apply_case(words: &str, case: WordCase) -> String {
    let capitalize = |part: &str| {
        let mut chars = part.chars();
        match chars.next() {
            Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
            None => String::new(),
        }
    };
    match case {
        WordCase::Lower => words.to_string(),
        WordCase::Upper => words.to_ascii_uppercase(),
        WordCase::Title => words
            .split(' ')
            .map(|word| {
                if word == "and" {
                    return word.to_string();
                }
                word.split('-')
                    .map(capitalize)
                    .collect::<Vec<_>>()
                    .join("-")
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The digit zero of every Unicode decimal digit family (general category
/// `Nd`), in code point order. The Unicode Standard encodes each family as ten
/// consecutive code points from zero to nine (Unicode §4.6, "Numeric Value"),
/// so these 68 values are all of `Nd`.
///
/// **Unicode 15.1**, the version [`is_alphanumeric`] classifies the other
/// characters by (through `regexml`, on ICU4X 1.5): generated from the
/// Unicode 16.0 character database less the eight families 16.0 added (Garay
/// `U+10D40`, Myanmar Pao `U+116D0`, Myanmar Eastern Pwo Karen `U+116DA`,
/// Sunuwar `U+11BF0`, Gurung Khema `U+16130`, Kirat Rai `U+16D70`, Outlined
/// `U+1CCF0`, Ol Onal `U+1E5F1`), which are unassigned in 15.1. So the two
/// tests of the tokenizer agree with each other and with the run's regular
/// expressions on every character;
/// `tests::the_digit_table_is_the_unicode_version_of_the_regexes` checks the
/// table against `regexml`'s `\p{Nd}`.
const DIGIT_ZEROS: [u32; 68] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0,
    0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950,
    0x1FBF0,
];

/// The family zero and the value of a decimal digit (`Nd`), or `None` for any
/// other character.
pub(crate) fn decimal_digit(c: char) -> Option<(char, u32)> {
    let code = c as u32;
    let index = DIGIT_ZEROS.partition_point(|&zero| zero <= code);
    let zero = *DIGIT_ZEROS.get(index.checked_sub(1)?)?;
    let value = code - zero;
    (value < 10).then(|| (char::from_u32(zero).unwrap_or('0'), value))
}

thread_local! {
    /// `\p{L}` and `\p{N}` are exactly §12.3's alphanumeric categories: L is
    /// Lu, Ll, Lt, Lm and Lo, N is Nd, Nl and No.
    static LETTER_OR_NUMBER: Option<regexml::Regex> =
        regexml::Regex::xpath(r"^[\p{L}\p{N}]$", "").ok();
}

/// Whether `c` is alphanumeric in §12.3's sense: "any character that has a
/// Unicode category of Nd, Nl, No, Lu, Ll, Lt, Lm or Lo".
///
/// Not [`char::is_alphanumeric`]: that is the `Alphabetic` property, which
/// also holds for combining marks (`Mn`, `Mc`) and the circled letters (`So`).
pub(crate) fn is_alphanumeric(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric();
    }
    if decimal_digit(c).is_some() {
        return true;
    }
    LETTER_OR_NUMBER.with(|regex| match regex {
        Some(regex) => regex.is_match(c.encode_utf8(&mut [0; 4])),
        // Never happens with a constant pattern; the nearest approximation.
        None => c.is_alphanumeric(),
    })
}

/// Whether `value` is a language tag in the form `xml:lang` takes — RFC 3066's
/// `Primary-subtag *( "-" Subtag )`, the pattern of `xs:language`:
/// `[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`. (The zero-length string, which
/// `xml:lang` also allows, is left to the caller.)
pub(crate) fn is_language_tag(value: &str) -> bool {
    let mut subtags = value.split('-');
    let primary = subtags.next().unwrap_or("");
    let size = |s: &str| (1..=8).contains(&s.len());
    size(primary)
        && primary.bytes().all(|b| b.is_ascii_alphabetic())
        && subtags.all(|s| size(s) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Whether `value` is a permitted value of the §12.3 attribute `name` (`lang`,
/// `letter-value`, `grouping-separator` or `grouping-size`): `Err` describes
/// the problem. The caller raises XTSE0020 for a value written without an
/// expression and XTDE0030 for an effective value computed at run time.
///
/// The permitted values, from the syntax summary (`lang? = { nmtoken }`,
/// `letter-value? = { "alphabetic" | "traditional" }`, `grouping-separator? =
/// { char }`, `grouping-size? = { number }`) and the text:
///
/// - `lang` "has the same range of values as `xml:lang`": a language tag, or
///   the zero-length string, which is taken as an absent attribute;
/// - `grouping-separator` is one character — `char` is one of the two types
///   whose value is not trimmed (§2.2: "Except where the set of allowed values
///   of an attribute is specified using the italicized name string or char,
///   leading and trailing whitespace in the attribute value is ignored");
/// - `grouping-size` is a non-negative integer, written in decimal digits.
pub(crate) fn check_attribute(name: &str, value: &str) -> Result<(), String> {
    let trimmed = crate::whitespace::trim(value);
    let ok = match name {
        "lang" => trimmed.is_empty() || is_language_tag(trimmed),
        "letter-value" => matches!(trimmed, "alphabetic" | "traditional"),
        "grouping-separator" => value.chars().count() == 1,
        "grouping-size" => !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()),
        _ => true,
    };
    if ok {
        return Ok(());
    }
    let expected = match name {
        "lang" => "a language tag",
        "letter-value" => "alphabetic or traditional",
        "grouping-separator" => "a single character",
        _ => "a non-negative integer",
    };
    Err(format!(
        "the {name} attribute of xsl:number must be {expected}, not {value:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: u64) -> Numeral {
        Numeral::Int(n)
    }

    fn fmt(format: &str, numbers: &[u64]) -> String {
        let numbers: Vec<Numeral> = numbers.iter().map(|&n| int(n)).collect();
        NumberFormat::parse(format).format(&numbers, &Conversion::default())
    }

    fn with(format: &str, n: u64, conversion: &Conversion) -> String {
        NumberFormat::parse(format).format(&[int(n)], conversion)
    }

    fn ordinal() -> Conversion {
        Conversion {
            ordinal: Some("yes".into()),
            ..Default::default()
        }
    }

    fn grouped(separator: &str, size: usize) -> Conversion {
        Conversion {
            grouping: Some(Grouping {
                separator: separator.into(),
                size,
            }),
            ..Default::default()
        }
    }

    /// `format_in_width` (§16.5.1): a width in characters counts an ordinal's
    /// suffix, a width in digits does not; the year leaves room for the suffix
    /// and keeps every digit when none fits; cardinals are the same either way.
    #[test]
    fn widths_in_characters_count_the_ordinal_suffix() {
        let one = FormatToken::parse("1");
        let w = |n: u64, conversion: &Conversion, min, max, year, chars| {
            one.format_in_width(n, conversion, min, max, year, chars)
        };
        let o = ordinal();
        assert_eq!(w(3, &o, 2, None, false, true), "3rd");
        assert_eq!(w(3, &o, 3, Some(3), false, true), "3rd");
        assert_eq!(w(3, &o, 4, None, false, true), "03rd");
        assert_eq!(w(3, &o, 2, Some(2), false, false), "03rd");
        assert_eq!(w(2003, &o, 4, Some(4), true, true), "03rd");
        assert_eq!(w(2003, &o, 2, Some(2), true, true), "2003rd");
        assert_eq!(w(2003, &o, 4, Some(4), true, false), "2003rd");
        assert_eq!(w(2011, &o, 1, Some(4), true, true), "11th");
        let c = Conversion::default();
        for chars in [false, true] {
            assert_eq!(w(3, &c, 2, None, false, chars), "03");
            assert_eq!(w(2003, &c, 1, Some(2), true, chars), "03");
            assert_eq!(w(123, &c, 1, Some(2), false, chars), "123");
        }
        // Not a decimal token: spaces, in characters.
        assert_eq!(
            FormatToken::parse("w").format_in_width(3, &o, 7, None, false, true),
            "third  "
        );
    }

    /// §12.3 example: "Given the sequence of numbers 5, 13, 7 and the format
    /// token A-001(i), the output will be the string E-013(vii)".
    #[test]
    fn the_spec_example_a_001_i() {
        assert_eq!(fmt("A-001(i)", &[5, 13, 7]), "E-013(vii)");
    }

    #[test]
    fn prefix_separators_suffix_and_the_last_token_reused() {
        assert_eq!(fmt("1.1 ", &[3, 4, 5]), "3.4.5 ");
        assert_eq!(fmt("A.1 ", &[1, 2]), "A.2 ");
        assert_eq!(fmt("(1) ", &[7]), "(7) ");
        assert_eq!(fmt("1-a:i", &[1, 2, 3, 4]), "1-b:iii:iv");
        // The first token used again: separated by ".".
        assert_eq!(fmt("1", &[1, 2, 3]), "1.2.3");
        assert_eq!(fmt("[1]", &[1, 2]), "[1.2]");
    }

    /// "If there is a non-alphanumeric token but no format token, then the
    /// single non-alphanumeric token is used as both the prefix and the
    /// suffix"; "If there are no format tokens, then a format token of 1 is
    /// used to format all numbers."
    #[test]
    fn no_format_token() {
        assert_eq!(fmt("*", &[3]), "*3*");
        assert_eq!(fmt("", &[3, 4]), "3.4");
        assert_eq!(fmt("--", &[]), "----");
    }

    #[test]
    fn an_empty_place_marker_keeps_prefix_and_suffix() {
        assert_eq!(fmt("[1]", &[]), "[]");
        assert_eq!(fmt("1", &[]), "");
    }

    #[test]
    fn decimal_tokens_pad_to_their_width() {
        assert_eq!(fmt("01", &[0, 5, 99, 100]), "00.05.99.100");
        assert_eq!(fmt("0001", &[7]), "0007");
        // The token `1` has the lower bound 0.
        assert_eq!(fmt("1", &[0]), "0");
        // A token whose leading characters are not the zero is not decimal.
        assert_eq!(FormatToken::parse("11"), FormatToken::Other("11".into()));
        assert_eq!(FormatToken::parse("2"), FormatToken::Other("2".into()));
        assert_eq!(FormatToken::parse("0"), FormatToken::Other("0".into()));
        assert_eq!(fmt("11", &[3]), "3");
    }

    /// "The digits used in the decimal representation are the set of digits
    /// containing the digit character used in the format token … A format
    /// token of &#x661; (Arabic-Indic digit one) generates the sequence ١ then
    /// ٢ then ٣ ..."
    #[test]
    fn every_nd_family() {
        assert_eq!(fmt("\u{661}", &[1, 2, 3, 10]), "١.٢.٣.١٠");
        assert_eq!(fmt("\u{660}\u{661}", &[7]), "٠٧");
        assert_eq!(fmt("\u{FF11}", &[42]), "４２");
        assert_eq!(fmt("\u{E51}", &[20]), "๒๐");
        // The five adjacent mathematical families do not run into each other.
        assert_eq!(fmt("\u{1D7CF}", &[9]), "\u{1D7D7}");
        assert_eq!(fmt("\u{1D7D9}", &[19]), "\u{1D7D9}\u{1D7E1}");
        assert_eq!(fmt("\u{1D7F7}", &[10]), "\u{1D7F7}\u{1D7F6}");
        // Every family: zero..nine, each digit one of its own.
        for zero in DIGIT_ZEROS {
            let zero_char = char::from_u32(zero).unwrap();
            let one = char::from_u32(zero + 1).unwrap();
            assert_eq!(decimal_digit(one), Some((zero_char, 1)), "{zero:X}");
            assert!(is_alphanumeric(one), "{zero:X}");
            let expected: String = [1u32, 2, 9]
                .iter()
                .map(|d| char::from_u32(zero + d).unwrap())
                .collect();
            assert_eq!(fmt(&one.to_string(), &[129]), expected, "{zero:X}");
        }
        assert_eq!(decimal_digit('\u{2F}'), None);
        assert_eq!(decimal_digit('\u{3A}'), None);
        assert_eq!(decimal_digit('\u{1D7CD}'), None);
    }

    #[test]
    fn alphabetic_continues_past_z() {
        assert_eq!(
            fmt("A", &[1, 26, 27, 52, 53, 702, 703]),
            "A.Z.AA.AZ.BA.ZZ.AAA"
        );
        // number-0801
        assert_eq!(fmt("a", &[13, 819]), "m.aem");
        assert_eq!(fmt("A", &[1999, 100000]), "BXW.EQXD");
        // Zero is below the range: the token `1`.
        assert_eq!(fmt("a", &[0]), "0");
    }

    #[test]
    fn roman_numerals_and_their_range() {
        assert_eq!(
            fmt("i", &[1, 4, 9, 14, 40, 90, 400, 900]),
            "i.iv.ix.xiv.xl.xc.cd.cm"
        );
        assert_eq!(fmt("I", &[1999, 3999]), "MCMXCIX.MMMCMXCIX");
        // number-0603
        assert_eq!(fmt("i", &[13, 819]), "xiii.dcccxix");
        // Out of range: the token `1` (§12.3), at both ends.
        assert_eq!(fmt("I", &[4000, 100000, 0]), "4000.100000.0");
    }

    #[test]
    fn english_words() {
        let words = |format: &str, n: u64| fmt(format, &[n]);
        assert_eq!(words("w", 0), "zero");
        assert_eq!(words("w", 13), "thirteen");
        assert_eq!(words("w", 21), "twenty one");
        assert_eq!(words("w", 100), "one hundred");
        assert_eq!(words("w", 134), "one hundred and thirty four");
        assert_eq!(words("W", 1005), "ONE THOUSAND AND FIVE");
        assert_eq!(words("Ww", 1100), "One Thousand One Hundred");
        assert_eq!(words("Ww", 588), "Five Hundred and Eighty Eight");
        assert_eq!(words("w", 2002), "two thousand and two");
        assert_eq!(words("w", 1990), "one thousand nine hundred and ninety");
        assert_eq!(
            words("Ww", 2_134_816),
            "Two Million One Hundred and Thirty Four Thousand Eight Hundred and Sixteen"
        );
        assert_eq!(
            words("w", u64::MAX),
            "eighteen quintillion four hundred and forty six quadrillion seven hundred and \
             forty four trillion seventy three billion seven hundred and nine million five \
             hundred and fifty one thousand six hundred and fifteen"
        );
    }

    #[test]
    fn english_ordinals() {
        let ord = |format: &str, n: u64| with(format, n, &ordinal());
        let suffixed: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111, 112, 2002, 0]
            .iter()
            .map(|&n| ord("1", n))
            .collect();
        assert_eq!(
            suffixed,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd",
                "101st", "111th", "112th", "2002nd", "0th"
            ]
        );
        assert_eq!(ord("01", 1), "01st");
        assert_eq!(ord("w", 1), "first");
        assert_eq!(ord("w", 12), "twelfth");
        assert_eq!(ord("w", 20), "twentieth");
        assert_eq!(ord("W", 21), "TWENTY-FIRST");
        assert_eq!(ord("Ww", 78), "Seventy-Eighth");
        assert_eq!(ord("w", 100), "one hundredth");
        assert_eq!(ord("w", 201), "two hundred and first");
        assert_eq!(ord("Ww", 1100), "One Thousand One Hundredth");
        assert_eq!(ord("w", 2000), "two thousandth");
        assert_eq!(ord("w", 2002), "two thousand and second");
        assert_eq!(ord("w", 0), "zeroth");
        assert_eq!(
            ord("w", 2_138_416),
            "two million one hundred and thirty eight thousand four hundred and sixteenth"
        );
        // Not supported for the letter tokens: cardinal (§12.3: "the request is
        // ignored and cardinal numbers are generated instead").
        assert_eq!(ord("a", 2), "b");
        assert_eq!(ord("I", 2), "II");
        // An empty value is not a request.
        let empty = Conversion {
            ordinal: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(with("1", 2, &empty), "2");
        // A fallback to `1` keeps the ordinal.
        assert_eq!(ord("i", 0), "0th");
    }

    /// Review F3: compound ordinal words as Saxon 9.1 writes them (its output
    /// for each value, all three cases) — the final tens and units joined by
    /// a hyphen, title case capitalizing both parts, the rest as in the
    /// cardinal — while the cardinal words keep their spaces (the catalog
    /// pins "Five Hundred and Eighty Eight").
    #[test]
    fn compound_ordinal_words_are_hyphenated_as_saxon_writes_them() {
        let saxon: [(u64, &str); 13] = [
            (21, "twenty-first"),
            (45, "forty-fifth"),
            (99, "ninety-ninth"),
            (101, "one hundred and first"),
            (120, "one hundred and twentieth"),
            (121, "one hundred and twenty-first"),
            (1001, "one thousand and first"),
            (1021, "one thousand and twenty-first"),
            (1121, "one thousand one hundred and twenty-first"),
            (2000, "two thousandth"),
            (21000, "twenty one thousandth"),
            (21021, "twenty one thousand and twenty-first"),
            (
                1_234_567,
                "one million two hundred and thirty four thousand five hundred and sixty-seventh",
            ),
        ];
        let ord = |format: &str, n: u64| with(format, n, &ordinal());
        for (n, lower) in saxon {
            assert_eq!(ord("w", n), lower, "w {n}");
            assert_eq!(ord("W", n), lower.to_ascii_uppercase(), "W {n}");
        }
        assert_eq!(ord("Ww", 21), "Twenty-First");
        assert_eq!(ord("Ww", 121), "One Hundred and Twenty-First");
        assert_eq!(ord("Ww", 1021), "One Thousand and Twenty-First");
        assert_eq!(ord("Ww", 21021), "Twenty One Thousand and Twenty-First");
        assert_eq!(
            ord("Ww", 1_234_567),
            "One Million Two Hundred and Thirty Four Thousand Five Hundred and Sixty-Seventh"
        );
        // The cardinals are unchanged.
        let words = |format: &str, n: u64| fmt(format, &[n]);
        assert_eq!(words("Ww", 588), "Five Hundred and Eighty Eight");
        assert_eq!(words("w", 21), "twenty one");
        assert_eq!(words("W", 1021), "ONE THOUSAND AND TWENTY ONE");
        assert_eq!(words("Ww", 21021), "Twenty One Thousand and Twenty One");
    }

    /// "grouping-separator="," and grouping-size="3" would produce numbers of
    /// the form 1,000,000 while grouping-separator="." and grouping-size="2"
    /// would produce numbers of the form 1.00.00.00."
    #[test]
    fn grouping() {
        assert_eq!(with("1", 1_000_000, &grouped(",", 3)), "1,000,000");
        assert_eq!(with("1", 1_000_000, &grouped(".", 2)), "1.00.00.00");
        assert_eq!(with("1", 999, &grouped(",", 3)), "999");
        // The padding is grouped too (number-0803).
        assert_eq!(
            with("00000000000001", 1_000_000, &grouped("/", 2)),
            "00/00/00/01/00/00/00"
        );
        // Size 0: no grouping.
        assert_eq!(with("1", 1_000_000, &grouped(",", 0)), "1000000");
        // Only the decimal sequences are grouped.
        assert_eq!(with("w", 1000, &grouped(",", 3)), "one thousand");
        // The fallback to `1` is a decimal sequence.
        assert_eq!(with("i", 10_000, &grouped(",", 3)), "10,000");
        // Grouping with a family's digits, and with an ordinal.
        assert_eq!(with("\u{661}", 1234, &grouped(",", 3)), "١,٢٣٤");
        let both = Conversion {
            ordinal: Some("yes".into()),
            ..grouped(",", 3)
        };
        assert_eq!(with("1", 1001, &both), "1,001st");
    }

    #[test]
    fn big_numbers_take_the_decimal_tokens_only() {
        let big = Numeral::Big("123456789012345678901234567890".into());
        let format = NumberFormat::parse("1");
        assert_eq!(
            format.format(std::slice::from_ref(&big), &grouped(",", 3)),
            "123,456,789,012,345,678,901,234,567,890"
        );
        for token in ["w", "a", "I", "\u{2460}"] {
            assert_eq!(
                NumberFormat::parse(token)
                    .format(std::slice::from_ref(&big), &Conversion::default()),
                "123456789012345678901234567890",
                "{token}"
            );
        }
    }

    #[test]
    fn nan_takes_its_place() {
        let format = NumberFormat::parse("(1)");
        assert_eq!(
            format.format(&[Numeral::NaN], &Conversion::default()),
            "(NaN)"
        );
        assert_eq!(
            NumberFormat::parse("1").format(&[Numeral::NaN], &ordinal()),
            "NaN"
        );
    }

    /// "If an implementation does not support a numbering sequence represented
    /// by the given token, it must use a format token of 1."
    #[test]
    fn unsupported_tokens_fall_back_to_one() {
        // SUPERSCRIPT ONE is No (alphanumeric) but not Nd, and no sequence.
        assert!(is_alphanumeric('\u{B9}'));
        assert_eq!(
            FormatToken::parse("\u{B9}"),
            FormatToken::Other("\u{B9}".into())
        );
        assert_eq!(fmt("\u{B9}", &[3]), "3");
        // A sequence's character for zero, or for two, does not name it: the
        // token "represents the number 1 (one)".
        for token in ["\u{24EA}", "\u{2461}", "\u{1F101}", "\u{2460}\u{2460}"] {
            assert_eq!(
                FormatToken::parse(token),
                FormatToken::Other(token.into()),
                "{token}"
            );
            assert_eq!(fmt(token, &[3]), "3", "{token}");
        }
        // Greek stays unsupported (user decision), whatever letter-value says.
        assert_eq!(fmt("\u{3B1}", &[3]), "3");
        assert_eq!(fmt("\u{5D0}", &[3]), "3");
        assert_eq!(fmt("x", &[3]), "3");
        assert_eq!(fmt("ab", &[3]), "3");
        assert_eq!(
            FormatToken::parse("Ww"),
            FormatToken::Words(WordCase::Title)
        );
        assert_eq!(
            FormatToken::parse("\u{660}\u{661}"),
            FormatToken::Decimal {
                zero: '\u{660}',
                width: 2
            }
        );
    }

    /// The seventeen sequences, each at the ends of its range, at zero, and
    /// just outside, as the W3C catalog's `combinations_for_numbering` cases
    /// expect them (`number-5001` … `number-5102`, `number-0819` … `0821`):
    /// the sequence to its last encoded number, then the token `1` (§12.3:
    /// "Numbers that fall outside this range must be formatted using the
    /// format token 1"). Each row: the token, then the expected result for
    /// 0, 1, 2, the last number of the range and the one after it.
    #[test]
    fn every_sequence_to_the_end_of_its_repertoire() {
        let rows: [(&str, [&str; 5], u64); 17] = [
            ("①", ["⓪", "①", "②", "㊿", "51"], 50),
            ("⒈", ["🄀", "⒈", "⒉", "⒛", "21"], 20),
            ("❶", ["⓿", "❶", "❷", "⓴", "21"], 20),
            ("➀", ["🄋", "➀", "➁", "➉", "11"], 10),
            ("➊", ["🄌", "➊", "➋", "➓", "11"], 10),
            ("⑴", ["0", "⑴", "⑵", "⒇", "21"], 20),
            ("⓵", ["0", "⓵", "⓶", "⓾", "11"], 10),
            ("㈠", ["0", "㈠", "㈡", "㈩", "11"], 10),
            ("㊀", ["0", "㊀", "㊁", "㊉", "11"], 10),
            ("𐄇", ["0", "𐄇", "𐄈", "𐄐", "11"], 10),
            ("𐋡", ["0", "𐋡", "𐋢", "𐋪", "11"], 10),
            ("𐹠", ["0", "𐹠", "𐹡", "𐹩", "11"], 10),
            ("𑁒", ["0", "𑁒", "𑁓", "𑁛", "11"], 10),
            ("𑇡", ["0", "𑇡", "𑇢", "𑇪", "11"], 10),
            ("🄂", ["🄁", "🄂", "🄃", "🄊", "10"], 9),
            ("𝍠", ["0", "𝍠", "𝍡", "𝍨", "10"], 9),
            ("𞣇", ["0", "𞣇", "𞣈", "𞣏", "10"], 9),
        ];
        for (token, expected, last) in rows {
            let got: Vec<String> = [0, 1, 2, last, last + 1]
                .iter()
                .map(|&n| fmt(token, &[n]))
                .collect();
            assert_eq!(got, expected, "{token}");
            assert!(
                matches!(FormatToken::parse(token), FormatToken::Sequence(_)),
                "{token}"
            );
        }
        // Every sequence of the table has a row, by its token.
        for sequence in &SEQUENCES {
            let one = sequence.one().to_string();
            let row = rows.iter().find(|(token, _, _)| *token == one);
            assert_eq!(row.map(|r| r.2), Some(sequence.range().1), "{one}");
        }
        // The circled digits through their three blocks, as `number-5014`
        // expects them.
        let circled: Vec<u64> = (0..=50).collect();
        assert_eq!(
            fmt(" ①", &circled).replace('.', ""),
            " ⓪①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮⑯⑰⑱⑲⑳㉑㉒㉓㉔㉕㉖㉗㉘㉙㉚㉛㉜㉝㉞㉟㊱㊲㊳㊴㊵㊶㊷㊸㊹㊺㊻㊼㊽㊾㊿"
        );
        // ❿ is the dingbat ten, ⓫ the eleven of the other block.
        assert_eq!(fmt("❶", &[9, 10, 11, 12]), "❾.❿.⓫.⓬");
        assert_eq!(fmt("⑴", &[19, 20, 21]), "⒆.⒇.21");
    }

    /// Outside its range a sequence is the token `1` in full — decimal, with
    /// grouping and ordinal — and inside it an ordinal request is ignored
    /// (§12.3: "the request is ignored and cardinal numbers are generated
    /// instead") and nothing is grouped (grouping is for "decimal numbering
    /// sequences"); a number beyond `u64` is outside every range.
    #[test]
    fn a_sequence_outside_its_range_is_the_token_1() {
        assert_eq!(with("①", 3, &ordinal()), "③");
        assert_eq!(with("①", 51, &ordinal()), "51st");
        assert_eq!(with("⑴", 0, &ordinal()), "0th");
        assert_eq!(with("①", 12, &grouped(",", 1)), "⑫");
        assert_eq!(with("①", 1234, &grouped(",", 3)), "1,234");
        let big = Numeral::Big("123456789012345678901234567890".into());
        assert_eq!(
            NumberFormat::parse("➊").format(&[big], &Conversion::default()),
            "123456789012345678901234567890"
        );
        assert_eq!(
            NumberFormat::parse("➊").format(&[Numeral::NaN], &Conversion::default()),
            "NaN"
        );
        // As a token among others, with separators.
        assert_eq!(fmt("①-a", &[2, 3]), "②-c");
        assert_eq!(fmt("(㈠)", &[1, 2, 11]), "(㈠.㈡.11)");
    }

    /// The table's characters are Unicode 15.1 — the version `regexml`, and so
    /// [`is_alphanumeric`], implements — and of category `No`: alphanumeric
    /// (a token on their own), not decimal digits (so never a `Decimal`
    /// token). The runs follow on from one another, and each token names one
    /// sequence.
    #[test]
    fn every_sequence_character_is_no_in_the_regex_unicode() {
        let no = regexml::Regex::xpath(r"^\p{No}$", "").expect("compiles");
        let mut tokens = Vec::new();
        for sequence in &SEQUENCES {
            let (low, high) = sequence.range();
            assert_eq!(low, u64::from(sequence.zero.is_none()));
            let mut next = 1;
            for &(first, _, length) in sequence.runs {
                assert_eq!(first, next, "{:?}", sequence.one());
                next = first + length;
            }
            for n in low..=high {
                let c = sequence.character(n).expect("in range");
                assert!(no.is_match(c.encode_utf8(&mut [0; 4])), "{:X}", c as u32);
                assert!(is_alphanumeric(c), "{:X}", c as u32);
                assert_eq!(decimal_digit(c), None, "{:X}", c as u32);
            }
            assert_eq!(sequence.character(high + 1), None);
            tokens.push(sequence.one());
        }
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), SEQUENCES.len());
    }

    #[test]
    fn the_alphanumeric_categories() {
        // Nd, Nl, No, Lu, Ll, Lt, Lm, Lo.
        for c in [
            '7', '\u{2160}', '\u{B2}', 'Q', 'q', '\u{1C5}', '\u{2B0}', '\u{5D0}', '\u{BA}',
        ] {
            assert!(is_alphanumeric(c), "{c:?}");
        }
        // Punctuation, symbols, separators, marks — including the combining
        // marks and circled letters that `char::is_alphanumeric` accepts.
        for c in [
            '.', '-', ' ', '\u{A0}', '\u{2014}', '\u{301}', '\u{345}', '\u{24B6}', '$',
        ] {
            assert!(!is_alphanumeric(c), "{c:?}");
        }
        // A combining mark splits a token.
        assert_eq!(fmt("1\u{301}1", &[2, 3]), "2\u{301}3");
    }

    /// Review F4: the decimal-digit table and the alphanumeric test are of one
    /// Unicode version, the one `regexml` implements: a digit is a decimal
    /// digit exactly when `\p{Nd}` matches it. Every character the standard
    /// library (a later Unicode) calls numeric is checked both ways, so a
    /// family missing from the table, or one the regular expressions do not
    /// know, fails here — as it will when `regexml` moves to a later Unicode
    /// and the table has to follow.
    #[test]
    fn the_digit_table_is_the_unicode_version_of_the_regexes() {
        let nd = regexml::Regex::xpath(r"^\p{Nd}$", "").expect("compiles");
        let is_nd = |c: char| nd.is_match(c.encode_utf8(&mut [0; 4]));
        for zero in DIGIT_ZEROS {
            for digit in zero..zero + 10 {
                assert!(is_nd(char::from_u32(digit).unwrap()), "{digit:X}");
            }
        }
        let mut digits = 0;
        for c in (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter(|c| c.is_numeric())
        {
            assert_eq!(decimal_digit(c).is_some(), is_nd(c), "{:X}", c as u32);
            digits += usize::from(is_nd(c));
        }
        assert_eq!(digits, 10 * DIGIT_ZEROS.len());
        // At the boundary: Unicode 16.0's Garay digit zero (`Nd` there) and
        // Garay capital letter A (`Lu` there) are both unassigned in 15.1, so
        // both separate tokens — before, the digit was alphanumeric and the
        // letter was not.
        for c in [
            '\u{10D40}',
            '\u{10D41}',
            '\u{10D50}',
            '\u{1E5F1}',
            '\u{1E5D0}',
        ] {
            assert!(!is_alphanumeric(c), "{:X}", c as u32);
            assert_eq!(decimal_digit(c), None, "{:X}", c as u32);
        }
        assert_eq!(fmt("1\u{10D41}1", &[5, 6]), "5\u{10D41}6");
        assert_eq!(fmt("1\u{10D50}1", &[5, 6]), "5\u{10D50}6");
        // Hanifi Rohingya, the family just before (Unicode 11.0), is a digit.
        assert_eq!(fmt("\u{10D31}", &[7]), "\u{10D37}");
        assert!(is_alphanumeric('\u{10D31}'));
    }

    #[test]
    fn language_tags() {
        for ok in [
            "en",
            "de",
            "en-US",
            "x-klingon",
            "i-navajo",
            "abcdefgh-12345678",
        ] {
            assert!(is_language_tag(ok), "{ok}");
        }
        for bad in [
            "",
            "42",
            "#####",
            "en_US",
            "en-",
            "-en",
            "abcdefghi",
            "en--us",
            "e n",
        ] {
            assert!(!is_language_tag(bad), "{bad}");
        }
    }
}
