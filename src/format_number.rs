//! Number formatting — XSLT 2.0 §16.4, the `format-number` function and the
//! variables of `xsl:decimal-format`.
//!
//! This module is pure: it knows nothing of XPath values, trees or the
//! stylesheet. The compiler resolves every `xsl:decimal-format` into a
//! [`DecimalFormat`] (`compile/decimal_format.rs`), and the function evaluator
//! turns the XDM value into a [`Numeric`] (`runtime/format.rs`); what happens
//! in between is here, in the spec's two phases:
//!
//! - **analysis** (§16.4.2, §16.4.3): [`Picture::parse`] checks the rules whose
//!   breach is XTDE1310 (the caller adds the code) and derives, per
//!   sub-picture, the prefix and suffix, the grouping positions, the minimum
//!   integer part size and the minimum and maximum fractional part sizes;
//! - **formatting** (§16.4.4): [`Picture::format`].
//!
//! # Numbers
//!
//! §16.4.4 converts the adjusted number to an `xs:decimal` "using an
//! implementation of `xs:decimal` that imposes no limits on the totalDigits or
//! fractionDigits facets", choosing, for an `xs:double` or `xs:float`, the value
//! with "the smallest possible number of digits" that converts back to the
//! same number — the shortest round-trip representation, which is what Rust's
//! `{:e}` formatting of `f64` and `f32` produces. The digits are kept as a
//! string ([`Digits`]), so nothing is lost between that conversion and the
//! output: `1E100 div 3` gives sixteen threes and eighty-four zeros, the largest
//! double its 309 digits. An `xs:decimal` or `xs:integer` arrives as its
//! lexical form and is never converted to a double ("if an `xs:decimal` is
//! supplied, it is not automatically promoted to an `xs:double`, as such
//! promotion can involve a loss of precision").
//!
//! Rounding is `round-half-to-even` on those decimal digits:
//! `format-number(0.125, '0.00')` is `0.12` (0.125 is a double exactly equal to
//! the decimal 0.125, a tie), `format-number(2.5, '0')` is `2`.
//!
//! # Zero and `#.#`
//!
//! Zero formatted with a sub-picture that has a decimal-separator-sign and no
//! zero-digit-sign is the **empty string** (`format-number(0, '#.#')`,
//! `format-number(0, '#,###.##')`): the literal XSLT 2.0 reading, kept on
//! purpose. §16.4.3 sets the minimum-integer-part-size to one only "if the
//! sub-picture contains no zero-digit-sign and no decimal-separator-sign";
//! §16.4.4 says "(The value zero will at this stage be represented by a
//! decimal-separator-sign on its own.)" and then "if there are no digits to the
//! right of the decimal-separator-sign character in the string, then the
//! decimal-separator-sign character is removed from the string". Nothing is
//! left. The alternatives: F&O 3.1 §4.7.4 later sets the
//! minimum-fractional-part-size to 1 when it and the minimum-integer-part-size
//! are both zero, which gives `.0`; Saxon 9.1 gives `0`. No catalog case
//! decides it.

use crate::numbering::decimal_digit;

/// The variables an `xsl:decimal-format` declaration sets (§16.4.1), with the
/// spec's defaults ([`Default`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecimalFormat {
    /// `decimal-separator`, "the character used for the decimal-separator-sign;
    /// the default value is the period character (.)".
    pub(crate) decimal_separator: char,
    /// `grouping-separator`: "the default value is the comma character (,)".
    pub(crate) grouping_separator: char,
    /// `infinity`: "the default value is the string Infinity".
    pub(crate) infinity: String,
    /// `minus-sign`: "the default value is the hyphen-minus character (-,
    /// #x2D)".
    pub(crate) minus_sign: char,
    /// `NaN`: "the default value is the string NaN".
    pub(crate) nan: String,
    /// `percent`: "the default value is the percent character (%)".
    pub(crate) percent: char,
    /// `per-mille`: "the default value is the Unicode per-mille character
    /// (#x2030)".
    pub(crate) per_mille: char,
    /// `zero-digit`: "the default value is the digit zero (0)". It also picks
    /// the digits of the result: "Unicode is organized so that each set of
    /// decimal digits forms a contiguous block of characters in numerical
    /// sequence."
    pub(crate) zero_digit: char,
    /// `digit`: "the default value is the number sign character (#)".
    pub(crate) digit: char,
    /// `pattern-separator`: "the default value is the semi-colon character
    /// (;)".
    pub(crate) pattern_separator: char,
}

impl Default for DecimalFormat {
    fn default() -> Self {
        DecimalFormat {
            decimal_separator: '.',
            grouping_separator: ',',
            infinity: "Infinity".to_string(),
            minus_sign: '-',
            nan: "NaN".to_string(),
            percent: '%',
            per_mille: '\u{2030}',
            zero_digit: '0',
            digit: '#',
            pattern_separator: ';',
        }
    }
}

impl DecimalFormat {
    /// The first two of the seven picture-string variables that share a
    /// character, with that character — XTSE1300:
    ///
    /// > It is a static error if, for any named or unnamed decimal format, the
    /// > variables representing characters used in a picture string do not each
    /// > have distinct values. These variables are decimal-separator-sign,
    /// > grouping-sign, percent-sign, per-mille-sign, digit-zero-sign,
    /// > digit-sign, and pattern-separator-sign.
    pub(crate) fn clashing_picture_characters(&self) -> Option<(&'static str, &'static str, char)> {
        let variables = [
            ("decimal-separator", self.decimal_separator),
            ("grouping-separator", self.grouping_separator),
            ("percent", self.percent),
            ("per-mille", self.per_mille),
            ("zero-digit", self.zero_digit),
            ("digit", self.digit),
            ("pattern-separator", self.pattern_separator),
        ];
        for (i, &(first, a)) in variables.iter().enumerate() {
            if let Some(&(second, _)) = variables[i + 1..].iter().find(|&&(_, b)| b == a) {
                return Some((first, second, a));
            }
        }
        None
    }
}

/// Whether `c` may be a `zero-digit` (XTSE1295): "This character must be a
/// digit (category Nd in the Unicode property database), and it must have the
/// numeric value zero."
pub(crate) fn is_zero_digit(c: char) -> bool {
    decimal_digit(c) == Some((c, 0))
}

/// A number to format: the `$value` of `format-number` after the function
/// conversion rules, in its own type (§16.4: "The `$value` argument may be of
/// any numeric data type (`xs:double`, `xs:float`, `xs:decimal`, or their
/// subtypes including `xs:integer`)").
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Numeric {
    Double(f64),
    Float(f32),
    /// An `xs:decimal` or `xs:integer`, as its lexical form: an optional sign,
    /// decimal digits, and optionally a period and more digits.
    Decimal(String),
}

/// Percent or per-mille: "If the sub-picture contains a percent-sign, the
/// number is multiplied by 100. If the sub-picture contains a per-mille-sign,
/// the number is multiplied by 1000."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scale {
    One,
    Percent,
    PerMille,
}

impl Scale {
    fn shift(self) -> usize {
        match self {
            Scale::One => 0,
            Scale::Percent => 2,
            Scale::PerMille => 3,
        }
    }
}

/// The integer-part-grouping-positions of §16.4.3.
#[derive(Clone, Debug, PartialEq, Eq)]
enum IntegerGrouping {
    /// "if these integer-part-grouping-positions are at regular intervals
    /// (that is, if they form a sequence N, 2N, 3N, ... for some integer value
    /// N, including the case where there is only one number in the list), then
    /// the sequence contains all integer multiples of N as far as necessary to
    /// accommodate the largest possible number."
    Regular(usize),
    /// Any other list, as written (possibly empty).
    Positions(Vec<usize>),
}

impl IntegerGrouping {
    fn from_positions(positions: Vec<usize>) -> IntegerGrouping {
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        match sorted.first() {
            // N = 0 would put every multiple at the same place; a grouping
            // separator at the end of the integer part is kept as written.
            Some(&n) if n > 0 && sorted.iter().zip(1..).all(|(&p, k)| p == n * k) => {
                IntegerGrouping::Regular(n)
            }
            _ => IntegerGrouping::Positions(positions),
        }
    }

    /// Whether a separator follows the integer digit that has `right` digits
    /// between it and the decimal separator, and how many.
    fn separators_after(&self, right: usize) -> usize {
        match self {
            IntegerGrouping::Regular(n) => {
                usize::from(right > 0 && right.checked_rem(*n) == Some(0))
            }
            IntegerGrouping::Positions(positions) => {
                positions.iter().filter(|&&p| p == right).count()
            }
        }
    }
}

/// The variables §16.4.3 derives from one sub-picture.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SubPicture {
    /// "all passive characters in the sub-picture to the left of the leftmost
    /// active character" (for the negative sub-picture of a one-sub-picture
    /// picture, preceded by the minus-sign).
    prefix: String,
    /// The passive characters to the right of the rightmost active character.
    suffix: String,
    integer_grouping: IntegerGrouping,
    minimum_integer_part_size: usize,
    fractional_grouping: Vec<usize>,
    minimum_fractional_part_size: usize,
    maximum_fractional_part_size: usize,
    has_decimal_separator: bool,
    scale: Scale,
}

/// An analysed picture string: the variables for positive and for negative
/// numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    positive: SubPicture,
    negative: SubPicture,
}

impl Picture {
    /// The analysis phase (§16.4.2, §16.4.3). `Err` is the XTDE1310 message:
    ///
    /// > \[ERR XTDE1310\] The picture string must conform to the following
    /// > rules. It is a non-recoverable dynamic error if the picture string
    /// > does not satisfy these rules.
    pub(crate) fn parse(picture: &str, format: &DecimalFormat) -> Result<Picture, String> {
        let chars: Vec<char> = picture.chars().collect();
        let separators: Vec<usize> = chars
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c == format.pattern_separator)
            .map(|(i, _)| i)
            .collect();
        match separators.as_slice() {
            // "If there is only one sub-picture, then the values for both cases
            // are derived from this sub-picture" — except the prefix: "the
            // prefix for the negative sub-picture is set by concatenating the
            // minus-sign character and the prefix for the positive sub-picture
            // (if any), in that order."
            [] => {
                let positive = SubPicture::parse(&chars, format, "the picture string")?;
                let mut negative = positive.clone();
                negative.prefix = format!("{}{}", format.minus_sign, positive.prefix);
                Ok(Picture { positive, negative })
            }
            // "If the picture-string contains two sub-pictures, the first is
            // used for positive values and the second for negative values."
            &[at] => Ok(Picture {
                positive: SubPicture::parse(&chars[..at], format, "the positive sub-picture")?,
                negative: SubPicture::parse(&chars[at + 1..], format, "the negative sub-picture")?,
            }),
            _ => Err(format!(
                "the picture string {picture:?} contains more than one pattern-separator-sign \
                 ({:?}); \"A picture-string must not contain more than one \
                 pattern-separator-sign\"",
                format.pattern_separator
            )),
        }
    }

    /// The formatting phase (§16.4.4).
    pub(crate) fn format(&self, value: &Numeric, format: &DecimalFormat) -> String {
        // "If the input number is NaN (not a number), the result is the
        // specified NaN-symbol (with no prefix or suffix)."
        let negative = match value {
            Numeric::Double(x) if x.is_nan() => return format.nan.clone(),
            Numeric::Float(x) if x.is_nan() => return format.nan.clone(),
            // "Negative zero is taken as negative, positive zero as positive."
            Numeric::Double(x) => x.is_sign_negative(),
            Numeric::Float(x) => x.is_sign_negative(),
            // `xs:decimal` has no negative zero.
            Numeric::Decimal(lexical) => {
                Digits::from_lexical(lexical).is_some_and(|(n, d)| n && !d.is_zero())
            }
        };
        // "the positive sub-picture and its associated variables are used if
        // the input number is positive, and the negative sub-picture and its
        // associated variables are used otherwise."
        let sub = if negative {
            &self.negative
        } else {
            &self.positive
        };
        let infinite = match value {
            Numeric::Double(x) => x.is_infinite(),
            Numeric::Float(x) => x.is_infinite(),
            Numeric::Decimal(_) => false,
        };
        if infinite {
            // "the concatenation of the appropriate prefix, the
            // infinity-symbol, and the appropriate suffix."
            return format!("{}{}{}", sub.prefix, format.infinity, sub.suffix);
        }
        let mut digits = sub.adjusted(value);
        // "calling the function round-half-to-even with this converted number
        // as the first argument and the maximum-fractional-part-size as the
        // second argument".
        digits.round_half_to_even(sub.maximum_fractional_part_size);
        sub.render(&digits, format)
    }
}

impl SubPicture {
    fn parse(sub: &[char], f: &DecimalFormat, which: &str) -> Result<SubPicture, String> {
        let text: String = sub.iter().collect();
        let fail = |rule: &str| Err(format!("{which} {text:?} breaks the rule \"{rule}\""));
        // §16.4.2: "the characters assigned to the variables
        // decimal-separator-sign, grouping-sign, zero-digit-sign, digit-sign
        // and pattern-separator-sign are classified as active characters, and
        // all other characters (including the percent-sign and per-mille-sign)
        // are classified as passive characters."
        let active = |c: char| {
            c == f.decimal_separator
                || c == f.grouping_separator
                || c == f.zero_digit
                || c == f.digit
                || c == f.pattern_separator
        };
        let is_digit = |c: char| c == f.digit || c == f.zero_digit;
        let count = |wanted: char| sub.iter().filter(|&&c| c == wanted).count();

        if count(f.decimal_separator) > 1 {
            return fail("A sub-picture must not contain more than one decimal-separator-sign.");
        }
        let (percents, per_milles) = (count(f.percent), count(f.per_mille));
        if percents + per_milles > 1 {
            return fail(
                "A sub-picture must not contain more than one percent-sign or per-mille-sign, \
                 and it must not contain one of each.",
            );
        }
        let (Some(first), Some(last)) = (
            sub.iter().position(|&c| active(c)),
            sub.iter().rposition(|&c| active(c)),
        ) else {
            return fail("A sub-picture must contain at least one digit-sign or zero-digit-sign.");
        };
        if !sub.iter().any(|&c| is_digit(c)) {
            return fail("A sub-picture must contain at least one digit-sign or zero-digit-sign.");
        }
        if sub[first..=last].iter().any(|&c| !active(c)) {
            return fail(
                "A sub-picture must not contain a passive character that is preceded by an \
                 active character and that is followed by another active character.",
            );
        }
        let decimal = sub.iter().position(|&c| c == f.decimal_separator);
        if let Some(d) = decimal {
            if (d > 0 && sub[d - 1] == f.grouping_separator)
                || sub.get(d + 1) == Some(&f.grouping_separator)
            {
                return fail(
                    "A sub-picture must not contain a grouping-separator-sign adjacent to a \
                     decimal-separator-sign.",
                );
            }
        }
        // "The integer part of the sub-picture is defined as the part that
        // appears to the left of the decimal-separator-sign if there is one,
        // or the entire sub-picture otherwise. The fractional part … is a
        // zero-length string otherwise."
        let (integer, fraction): (&[char], &[char]) = match decimal {
            Some(d) => (&sub[..d], &sub[d + 1..]),
            None => (sub, &[]),
        };
        if let Some(z) = integer.iter().position(|&c| c == f.zero_digit) {
            if integer[z + 1..].contains(&f.digit) {
                return fail(
                    "The integer part of a sub-picture must not contain a zero-digit-sign that \
                     is followed by a digit-sign.",
                );
            }
        }
        if let Some(d) = fraction.iter().position(|&c| c == f.digit) {
            if fraction[d + 1..].contains(&f.zero_digit) {
                return fail(
                    "The fractional part of a sub-picture must not contain a digit-sign that is \
                     followed by a zero-digit-sign.",
                );
            }
        }

        let digits_in = |part: &[char]| part.iter().filter(|&&c| is_digit(c)).count();
        let zeros_in = |part: &[char]| part.iter().filter(|&&c| c == f.zero_digit).count();
        // "For each grouping-separator-sign that appears within the integer
        // part of the sub-picture, this sequence contains an integer that is
        // equal to the total number of digit-sign and zero-digit-sign
        // characters that appear within the integer part of the sub-picture
        // and to the right of the grouping-separator-sign."
        let integer_positions = integer
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c == f.grouping_separator)
            .map(|(i, _)| digits_in(&integer[i + 1..]))
            .collect();
        // "… within the fractional part of the sub-picture and to the left of
        // the grouping-separator-sign."
        let fractional_grouping = fraction
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c == f.grouping_separator)
            .map(|(i, _)| digits_in(&fraction[..i]))
            .collect();
        // "It is normally set to the number of zero-digit-sign characters found
        // in the integer part of the sub-picture. But if the sub-picture
        // contains no zero-digit-sign and no decimal-separator-sign, it is set
        // to one."
        let minimum_integer_part_size = if decimal.is_none() && count(f.zero_digit) == 0 {
            1
        } else {
            zeros_in(integer)
        };
        Ok(SubPicture {
            prefix: sub[..first].iter().collect(),
            // §16.4.3 says "all passive characters to the right of the
            // rightmost active character in the fractional part of the
            // sub-picture"; without a decimal-separator-sign the fractional
            // part is empty, and `00%` must still keep its `%` (string-121
            // expects `25%`), so the suffix is read from the whole sub-picture,
            // which is the same thing whenever there is a fractional part.
            suffix: sub[last + 1..].iter().collect(),
            integer_grouping: IntegerGrouping::from_positions(integer_positions),
            minimum_integer_part_size,
            fractional_grouping,
            minimum_fractional_part_size: zeros_in(fraction),
            maximum_fractional_part_size: digits_in(fraction),
            has_decimal_separator: decimal.is_some(),
            scale: if percents == 1 {
                Scale::Percent
            } else if per_milles == 1 {
                Scale::PerMille
            } else {
                Scale::One
            },
        })
    }

    /// The absolute value of the adjusted number, as decimal digits: "If the
    /// sub-picture contains a percent-sign, the number is multiplied by 100 …
    /// The adjusted number is converted (if necessary) to an `xs:decimal`
    /// value". The multiplication is done in the number's own type, as the
    /// spec orders it (an `xs:double` times 100 is an `xs:double`); a product
    /// that overflows to infinity is computed on the digits instead.
    fn adjusted(&self, value: &Numeric) -> Digits {
        let factor = match self.scale {
            Scale::One => 1.0,
            Scale::Percent => 100.0,
            Scale::PerMille => 1000.0,
        };
        match value {
            Numeric::Double(x) => {
                let scaled = x * factor;
                if scaled.is_finite() {
                    Digits::from_f64(scaled)
                } else {
                    Digits::from_f64(*x).shifted(self.scale.shift())
                }
            }
            Numeric::Float(x) => {
                let scaled = x * factor as f32;
                if scaled.is_finite() {
                    Digits::from_f32(scaled)
                } else {
                    Digits::from_f32(*x).shifted(self.scale.shift())
                }
            }
            Numeric::Decimal(lexical) => Digits::from_lexical(lexical)
                .map(|(_, digits)| digits)
                .unwrap_or_default()
                .shifted(self.scale.shift()),
        }
    }

    /// The rest of §16.4.4, from the rounded number to the result string.
    fn render(&self, digits: &Digits, f: &DecimalFormat) -> String {
        // "The absolute value of the rounded number is converted to a string in
        // decimal notation, with no insignificant leading or trailing zeroes
        // … If the number of digits to the left of the decimal-separator-sign
        // is less than minimum-integer-part-size, leading zero-digit-sign
        // characters are added to pad out to that size."
        let mut integer = digits.integer.clone();
        let pad = self.minimum_integer_part_size.saturating_sub(integer.len());
        integer.insert_str(0, &"0".repeat(pad));
        // "… trailing zero-digit-sign characters are added to pad out to that
        // size."
        let mut fraction = digits.fraction.clone();
        let pad = self
            .minimum_fractional_part_size
            .saturating_sub(fraction.len());
        fraction.push_str(&"0".repeat(pad));

        let digit =
            |d: u8| char::from_u32(f.zero_digit as u32 + u32::from(d - b'0')).unwrap_or('?');
        let mut out = self.prefix.clone();
        // "For each integer N in the integer-part-grouping-positions list, a
        // grouping-separator-sign character is inserted into the string
        // immediately after that digit that appears in the integer part of the
        // number and has N digits between it and the decimal-separator-sign, if
        // there is such a digit."
        let len = integer.len();
        for (i, d) in integer.bytes().enumerate() {
            out.push(digit(d));
            for _ in 0..self.integer_grouping.separators_after(len - 1 - i) {
                out.push(f.grouping_separator);
            }
        }
        // "If there is no decimal-separator-sign in the sub-picture, or if there
        // are no digits to the right of the decimal-separator-sign character in
        // the string, then the decimal-separator-sign character is removed from
        // the string".
        if self.has_decimal_separator && !fraction.is_empty() {
            out.push(f.decimal_separator);
            // "… a grouping-separator-sign character is inserted into the
            // string immediately before that digit that appears in the
            // fractional part of the number and has N digits between it and
            // the decimal-separator-sign, if there is such a digit."
            for (i, d) in fraction.bytes().enumerate() {
                for _ in self.fractional_grouping.iter().filter(|&&p| p == i) {
                    out.push(f.grouping_separator);
                }
                out.push(digit(d));
            }
        }
        // "The result of the function is the concatenation of the appropriate
        // prefix, the string conversion of the number as obtained above, and
        // the appropriate suffix."
        out.push_str(&self.suffix);
        out
    }
}

/// The absolute value of a finite number in decimal notation: ASCII digits,
/// `integer` without leading zeros and `fraction` without trailing zeros (zero
/// is two empty strings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Digits {
    integer: String,
    fraction: String,
}

impl Digits {
    fn is_zero(&self) -> bool {
        self.integer.is_empty() && self.fraction.is_empty()
    }

    /// The digits `mantissa` (no point) times 10^(`point` − its length):
    /// `point` digits of it are the integer part.
    fn from_scientific(mantissa: &str, point: i64) -> Digits {
        let len = mantissa.len() as i64;
        let (integer, fraction) = if point <= 0 {
            (
                String::new(),
                "0".repeat(point.unsigned_abs() as usize) + mantissa,
            )
        } else if point >= len {
            (
                mantissa.to_string() + &"0".repeat((point - len) as usize),
                String::new(),
            )
        } else {
            let (i, f) = mantissa.split_at(point as usize);
            (i.to_string(), f.to_string())
        };
        Digits { integer, fraction }.normalized()
    }

    /// The shortest decimal that converts back to `x` (finite).
    fn from_f64(x: f64) -> Digits {
        Digits::from_exponential(&format!("{:e}", x.abs()))
    }

    /// The shortest decimal that converts back to the `xs:float` `x`.
    fn from_f32(x: f32) -> Digits {
        Digits::from_exponential(&format!("{:e}", x.abs()))
    }

    /// Reads Rust's `{:e}` form, `d.ddde±x`.
    fn from_exponential(text: &str) -> Digits {
        let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
        let exponent: i64 = exponent.parse().unwrap_or(0);
        let (head, tail) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        Digits::from_scientific(&format!("{head}{tail}"), exponent + head.len() as i64)
    }

    /// Reads a decimal lexical form (`-12.50`, `+3`, `.5`): the sign and the
    /// absolute value, or `None` if it is not one.
    fn from_lexical(lexical: &str) -> Option<(bool, Digits)> {
        let text = crate::whitespace::trim(lexical);
        let (negative, unsigned) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if (integer.is_empty() && fraction.is_empty())
            || !all_digits(integer)
            || !all_digits(fraction)
        {
            return None;
        }
        Some((
            negative,
            Digits {
                integer: integer.to_string(),
                fraction: fraction.to_string(),
            }
            .normalized(),
        ))
    }

    fn normalized(mut self) -> Digits {
        let leading = self.integer.bytes().take_while(|&b| b == b'0').count();
        self.integer.drain(..leading);
        let kept = self.fraction.trim_end_matches('0').len();
        self.fraction.truncate(kept);
        self
    }

    /// Multiplied by 10^`places`.
    fn shifted(mut self, places: usize) -> Digits {
        let mut moved: String = self.fraction.chars().take(places).collect();
        moved.push_str(&"0".repeat(places - moved.len()));
        self.fraction.drain(..places.min(self.fraction.len()));
        self.integer.push_str(&moved);
        self.normalized()
    }

    /// F&O `round-half-to-even($arg, $precision)` on the absolute value:
    /// "the value is rounded to $precision digits after the decimal point …
    /// If two such values are equally near (e.g. if the fractional part in
    /// $arg is exactly .500…), the function returns the one whose least
    /// significant digit is even."
    fn round_half_to_even(&mut self, places: usize) {
        if self.fraction.len() <= places {
            return;
        }
        let dropped = &self.fraction.as_bytes()[places..];
        let first = dropped[0];
        let rest_nonzero = dropped[1..].iter().any(|&b| b != b'0');
        let mut kept: Vec<u8> = self.integer.bytes().collect();
        kept.extend_from_slice(&self.fraction.as_bytes()[..places]);
        let last_odd = kept.last().is_some_and(|&b| (b - b'0') % 2 == 1);
        let up = first > b'5' || (first == b'5' && (rest_nonzero || last_odd));
        if up {
            let mut i = kept.len();
            loop {
                if i == 0 {
                    kept.insert(0, b'1');
                    break;
                }
                i -= 1;
                if kept[i] == b'9' {
                    kept[i] = b'0';
                } else {
                    kept[i] += 1;
                    break;
                }
            }
        }
        let split = kept.len() - places;
        let text = String::from_utf8(kept).unwrap_or_default();
        *self = Digits {
            integer: text[..split].to_string(),
            fraction: text[split..].to_string(),
        }
        .normalized();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df() -> DecimalFormat {
        DecimalFormat::default()
    }

    fn fmt(value: Numeric, picture: &str) -> String {
        fmt_with(value, picture, &df())
    }

    fn fmt_with(value: Numeric, picture: &str, f: &DecimalFormat) -> String {
        Picture::parse(picture, f)
            .unwrap_or_else(|e| panic!("{picture:?}: {e}"))
            .format(&value, f)
    }

    fn dec(s: &str) -> Numeric {
        Numeric::Decimal(s.to_string())
    }

    fn dbl(x: f64) -> Numeric {
        Numeric::Double(x)
    }

    #[test]
    fn the_defaults_are_the_specs() {
        // §16.4.1: the default of every attribute.
        let f = df();
        assert_eq!(
            (
                f.decimal_separator,
                f.grouping_separator,
                f.infinity.as_str(),
                f.minus_sign,
                f.nan.as_str(),
                f.percent,
                f.per_mille,
                f.zero_digit,
                f.digit,
                f.pattern_separator
            ),
            ('.', ',', "Infinity", '-', "NaN", '%', '\u{2030}', '0', '#', ';')
        );
        assert_eq!(f.clashing_picture_characters(), None);
    }

    #[test]
    fn xtse1300_names_the_first_two_clashing_variables() {
        let f = DecimalFormat { digit: '0', ..df() };
        assert_eq!(
            f.clashing_picture_characters(),
            Some(("zero-digit", "digit", '0'))
        );
        let f = DecimalFormat {
            percent: '.',
            ..df()
        };
        assert_eq!(
            f.clashing_picture_characters(),
            Some(("decimal-separator", "percent", '.'))
        );
        // minus-sign, infinity and NaN are not picture characters.
        let f = DecimalFormat {
            minus_sign: '.',
            ..df()
        };
        assert_eq!(f.clashing_picture_characters(), None);
    }

    #[test]
    fn xtse1295_zero_digit_is_an_nd_zero() {
        assert!(is_zero_digit('0'));
        assert!(is_zero_digit('\u{660}'));
        assert!(is_zero_digit('\u{104A0}'));
        assert!(!is_zero_digit('2'));
        assert!(!is_zero_digit('a'));
        assert!(!is_zero_digit('$'));
        assert!(!is_zero_digit('\u{661}'));
    }

    #[test]
    fn every_xtde1310_rule_is_checked() {
        let f = df();
        for (picture, rule) in [
            ("#;#;#", "more than one pattern-separator-sign"),
            ("#.#.#", "more than one decimal-separator-sign"),
            ("%0.0%", "more than one percent-sign or per-mille-sign"),
            (
                "%0.0\u{2030}",
                "more than one percent-sign or per-mille-sign",
            ),
            ("abc", "at least one digit-sign"),
            ("fred.ginger", "at least one digit-sign"),
            ("#;", "at least one digit-sign"),
            (";#", "at least one digit-sign"),
            ("0$0", "passive character"),
            ("#,##a0", "passive character"),
            ("00,.0", "grouping-separator-sign adjacent"),
            ("00.,0", "grouping-separator-sign adjacent"),
            ("0#.00", "zero-digit-sign that is followed by a digit-sign"),
            ("0,#", "zero-digit-sign that is followed by a digit-sign"),
            ("00.##0", "digit-sign that is followed by a zero-digit-sign"),
            (
                "000.##0",
                "digit-sign that is followed by a zero-digit-sign",
            ),
        ] {
            let err = Picture::parse(picture, &f).expect_err(picture);
            assert!(err.contains(rule), "{picture:?}: {err}");
        }
        // "followed" is "anywhere", not "immediately" — but only within the
        // part: a zero in the integer part may precede a digit-sign of the
        // fractional part.
        assert!(Picture::parse("0.##", &f).is_ok());
        assert!(Picture::parse("#.0#", &f).is_ok());
    }

    #[test]
    fn the_catalog_examples() {
        // format-number-001 … -067, string-014 … -121, as decimals or doubles
        // the way XPath gives them.
        assert_eq!(fmt(dec("87504.4812"), "000,000.000000"), "087,504.481200");
        assert_eq!(
            fmt(dec("1235464.8812"), "##,###,000.000###"),
            "1,235,464.8812"
        );
        assert_eq!(fmt(dec("-102136.4812"), "000,000.000###"), "-102,136.4812");
        assert_eq!(
            fmt(dec("-87504.4812"), "000,000.000###;###,###.000###"),
            "87,504.4812"
        );
        assert_eq!(fmt(dec("0.4857"), "###.###%"), "48.57%");
        assert_eq!(fmt(dec("0.4857"), "###.###\u{2030}"), "485.7\u{2030}");
        assert_eq!(fmt(dec("95.4857"), "\u{a4}###.####"), "\u{a4}95.4857");
        assert_eq!(
            fmt(dec("185.2812"), "PREFIX##00.000###SUFFIX"),
            "PREFIX185.2812SUFFIX"
        );
        assert_eq!(fmt(dec("987654321"), "###,##0,00.00"), "9876,543,21.00");
        assert_eq!(fmt(dec("239236.588"), "00000.00"), "239236.59");
        assert_eq!(fmt(dbl(f64::INFINITY), "#####"), "Infinity");
        assert_eq!(fmt(dbl(f64::NEG_INFINITY), "#####"), "-Infinity");
        assert_eq!(fmt(dbl(f64::NAN), "#####"), "NaN");
        assert_eq!(fmt(dbl(1234.0), "0000.####"), "1234");
        assert_eq!(fmt(dec("1234.00"), "0000.####"), "1234");
        assert_eq!(fmt(dec("12.34"), "##.##"), "12.34");
        assert_eq!(fmt(dec("123456789.34"), "#,###.##"), "123,456,789.34");
        assert_eq!(fmt(dec("1"), "#,##0"), "1");
        assert_eq!(fmt(dec("1000"), "###0"), "1000");
        assert_eq!(fmt(dec("1000"), "#,##0"), "1,000");
        assert_eq!(fmt(dec("1"), "00"), "01");
        assert_eq!(fmt(dec("1"), "00.0"), "01.0");
        assert_eq!(fmt(dec("0.25"), "00%"), "25%");
        assert_eq!(fmt(dbl(700.0), "#,###0.00"), "700.00");
        for zero in [dec("0"), dec("0.0"), dbl(0.0), Numeric::Float(0.0)] {
            assert_eq!(fmt(zero, "#"), "0");
        }
    }

    #[test]
    fn large_doubles_keep_their_shortest_digits() {
        // format-number-044.
        assert_eq!(fmt(dbl(1e25), "#,######"), "10,000000,000000,000000,000000");
        assert_eq!(
            fmt(dbl(1e20), "#####################"),
            "100000000000000000000"
        );
        assert_eq!(
            fmt(dbl(1e100 / 3.0), "#"),
            format!("{}{}", "3".repeat(16), "0".repeat(84))
        );
        let largest = fmt(dbl(f64::MAX), "#");
        assert_eq!(largest.len(), 309);
        assert!(largest.starts_with("17976931348623157"));
        assert_eq!(fmt(dbl(1e-10), "0.000000000000"), "0.000000000100");
        assert_eq!(fmt(dbl(1e-10), "0.00"), "0.00");
        assert_eq!(fmt(dbl(5e-324), "0.0"), "0.0");
        // Shortest digits of a float, not of the double it widens to.
        assert_eq!(fmt(Numeric::Float(0.1), "0.##########"), "0.1");
    }

    #[test]
    fn rounding_is_half_to_even_on_the_decimal() {
        assert_eq!(fmt(dbl(0.125), "0.00"), "0.12");
        assert_eq!(fmt(dbl(0.375), "0.00"), "0.38");
        assert_eq!(fmt(dbl(2.5), "0"), "2");
        assert_eq!(fmt(dbl(3.5), "0"), "4");
        assert_eq!(fmt(dec("0.5"), "0"), "0");
        assert_eq!(fmt(dec("1.5"), "0"), "2");
        assert_eq!(fmt(dec("2.50001"), "0"), "3");
        assert_eq!(fmt(dec("9.995"), "0.00"), "10.00");
        assert_eq!(fmt(dec("999.5"), "#,###"), "1,000");
        assert_eq!(fmt(dec("0.0049"), "0.00"), "0.00");
    }

    #[test]
    fn negative_zero_is_negative() {
        // §16.4.4: "Negative zero is taken as negative, positive zero as
        // positive."
        assert_eq!(fmt(dbl(-0.0), "0"), "-0");
        assert_eq!(fmt(dbl(0.0), "0"), "0");
        assert_eq!(fmt(dbl(-0.0), "0;(0)"), "(0)");
        assert_eq!(fmt(Numeric::Float(-0.0), "0"), "-0");
        // The input number decides, not the rounded one.
        assert_eq!(fmt(dbl(-0.001), "0.00"), "-0.00");
        // xs:decimal has no negative zero.
        assert_eq!(fmt(dec("-0.0"), "0"), "0");
    }

    #[test]
    fn prefixes_suffixes_and_the_minus_sign() {
        let f = DecimalFormat {
            minus_sign: '_',
            ..df()
        };
        // format-number-028.
        assert_eq!(
            fmt_with(dec("-26931.4"), "-###,###.###", &df()),
            "--26,931.4"
        );
        assert_eq!(
            fmt_with(dec("-26931.4"), "zzz-###,###.###", &f),
            "_zzz-26,931.4"
        );
        assert_eq!(fmt_with(dec("-26931.4"), "_###,###.###", &f), "__26,931.4");
        // format-number-029: an explicit negative sub-picture has no minus.
        assert_eq!(
            fmt_with(dec("-26931.4"), "###,###.###;###,###.###", &f),
            "26,931.4"
        );
        // Infinity takes the prefix and suffix; NaN takes neither.
        let f = DecimalFormat {
            infinity: "huge".into(),
            nan: "not a number".into(),
            ..df()
        };
        assert_eq!(fmt_with(dbl(f64::NEG_INFINITY), "<#>", &f), "-<huge>");
        assert_eq!(fmt_with(dbl(f64::NAN), "<#>", &f), "not a number");
    }

    #[test]
    fn custom_characters_and_digit_families() {
        // format-number-009.
        let f = DecimalFormat {
            decimal_separator: '|',
            grouping_separator: '.',
            ..df()
        };
        assert_eq!(fmt_with(dec("931.4857"), "000.000|###", &f), "000.931|486");
        // format-number-010.
        let f = DecimalFormat {
            digit: '!',
            pattern_separator: '\\',
            ..df()
        };
        assert_eq!(
            fmt_with(dec("26931.4"), "+!!!,!!!.!!!\\-!!,!!!.!!!", &f),
            "+26,931.4"
        );
        assert_eq!(
            fmt_with(dec("-26931.4"), "+!!,!!!.!!!\\-!!!,!!!.!!!", &f),
            "-26,931.4"
        );
        // format-number-031: `#` and `0` are passive once digit is `!` and
        // zero-digit is U+0660.
        let f = DecimalFormat {
            digit: '!',
            zero_digit: '\u{660}',
            ..df()
        };
        assert_eq!(
            fmt_with(
                dec("4030201.0506"),
                "#!!!,!!!,\u{660}\u{660}\u{660}.\u{660}\u{660}\u{660}\u{660}\u{660}\u{660}0",
                &f
            ),
            "#\u{664},\u{660}\u{663}\u{660},\u{662}\u{660}\u{661}.\u{660}\u{665}\u{660}\u{666}\u{660}\u{660}0"
        );
        // format-number-050: astral separators.
        let f = DecimalFormat {
            decimal_separator: '\u{1ADB0}',
            grouping_separator: '\u{1ADB1}',
            ..df()
        };
        assert_eq!(
            fmt_with(dbl(1234567890.123456), "\u{1ADB1}000\u{1ADB0}000", &f),
            "1\u{1ADB1}234\u{1ADB1}567\u{1ADB1}890\u{1ADB0}123"
        );
        // format-number-024.
        let f = DecimalFormat {
            decimal_separator: ',',
            grouping_separator: ' ',
            ..df()
        };
        assert_eq!(
            fmt_with(dec("7654321.4857"), "### ### ###,#####", &f),
            "7 654 321,4857"
        );
    }

    #[test]
    fn grouping_positions() {
        // Regular: every multiple of N.
        assert_eq!(fmt(dec("1234567890"), "#,##0"), "1,234,567,890");
        assert_eq!(fmt(dec("1234567890"), "#,##,#0"), "12,34,56,78,90");
        // Irregular (5 and 3 are not N, 2N): only the positions written.
        assert_eq!(fmt(dec("1234567890"), "#,##,##0"), "12345,67,890");
        // Fractional grouping counts from the separator.
        assert_eq!(fmt(dec("0.123456"), "0.000,000"), "0.123,456");
        // The padding zeros are digits of the fractional part too.
        assert_eq!(fmt(dec("0.12"), "0.00,0###"), "0.12,0");
        // "if there is such a digit".
        assert_eq!(fmt(dec("0.12"), "0.00,###"), "0.12");
        // A separator at the end of the integer part comes after the last digit.
        assert_eq!(fmt(dec("12"), "#,"), "12,");
    }

    #[test]
    fn no_zero_digit_and_no_separator_gives_one_integer_digit() {
        // "if the sub-picture contains no zero-digit-sign and no
        // decimal-separator-sign, it is set to one."
        assert_eq!(fmt(dec("0"), "###"), "0");
        // With a decimal separator the minimum is 0.
        assert_eq!(fmt(dec("0.5"), "#.#"), ".5");
        // and zero is "a decimal-separator-sign on its own", then removed.
        assert_eq!(fmt(dec("0"), "#.#"), "");
    }

    /// Zero with a picture that has a decimal-separator-sign and no
    /// zero-digit-sign is the **empty string** — the literal XSLT 2.0 text,
    /// kept on purpose (module docs, "Zero and `#.#`"). F&O 3.1 §4.7.4 would
    /// give `.0`; Saxon 9.1 gives `0`.
    #[test]
    fn zero_without_a_zero_digit_is_the_empty_string_in_xslt_2_0() {
        for picture in ["#.#", "#,###.##", "#.###", "#,##,###.#"] {
            assert_eq!(fmt(dec("0"), picture), "", "{picture}");
            assert_eq!(fmt(dbl(0.0), picture), "", "{picture}");
            // Rounded to zero: the same.
            assert_eq!(fmt(dec("0.0001"), picture), "", "{picture}");
        }
        // Negative zero takes the negative sub-picture, so only the minus sign
        // is left; a prefix and a suffix stay.
        assert_eq!(fmt(dbl(-0.0), "#.#"), "-");
        assert_eq!(fmt(dec("0"), "$#.#;($#.#)"), "$");
        assert_eq!(fmt(dec("0"), "#.#\u{2030}"), "\u{2030}");
        // A zero-digit-sign on either side keeps a digit.
        assert_eq!(fmt(dec("0"), "#.0"), ".0");
        assert_eq!(fmt(dec("0"), "0.#"), "0");
        assert_eq!(fmt(dec("0"), "#"), "0");
    }

    #[test]
    fn percent_and_per_mille_scale_in_the_values_type() {
        assert_eq!(fmt(dbl(0.4857), "##.##%"), "48.57%");
        assert_eq!(fmt(dec("0.4857"), "#.#\u{2030}"), "485.7\u{2030}");
        assert_eq!(fmt(dec("-0.5"), "0%"), "-50%");
        assert_eq!(fmt(dec("12"), "0%"), "1200%");
        // A double product that overflows is scaled on the digits.
        let huge = fmt(dbl(f64::MAX), "0%");
        assert!(huge.starts_with("17976931348623157") && huge.ends_with("00%"));
        assert_eq!(huge.len(), 309 + 2 + 1);
    }

    #[test]
    fn integers_beyond_u64_and_long_decimals_are_exact() {
        assert_eq!(
            fmt(dec("123456789012345678901234567890"), "#,###"),
            "123,456,789,012,345,678,901,234,567,890"
        );
        assert_eq!(
            fmt(
                dec("-0.1234567890123456789012345678"),
                "0.0###########################"
            ),
            "-0.1234567890123456789012345678"
        );
    }

    #[test]
    fn lexical_decimals() {
        assert_eq!(
            Digits::from_lexical("-0012.500"),
            Some((
                true,
                Digits {
                    integer: "12".into(),
                    fraction: "5".into()
                }
            ))
        );
        assert_eq!(
            Digits::from_lexical("+.5"),
            Some((
                false,
                Digits {
                    integer: "".into(),
                    fraction: "5".into()
                }
            ))
        );
        assert_eq!(Digits::from_lexical("1e5"), None);
        assert_eq!(Digits::from_lexical("."), None);
        assert_eq!(Digits::from_lexical(""), None);
    }
}
