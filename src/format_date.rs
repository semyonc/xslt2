//! Date and time formatting — XSLT 2.0 §16.5, the functions `format-date`,
//! `format-time` and `format-dateTime`.
//!
//! This module is pure: it knows nothing of XPath values, trees or the
//! stylesheet. The function evaluator turns the XDM value into
//! [`DateTimeParts`] and the `$language` / `$calendar` arguments into a
//! [`Settings`] (`runtime/format.rs`); everything else is here: the picture
//! string (§16.5.1, XTDE1340), the components and their availability
//! (XTDE1350), the presentation and width modifiers, and the fallbacks of
//! §16.5.2. Numbers go through the §12.3 formatter of `numbering.rs`
//! ([`FormatToken::format_in_width`]), as §16.5.1 asks: the first
//! presentation modifier may be "any format token permitted in the format
//! string of the `xsl:number` instruction".
//!
//! # What this implementation supports (the implementation-defined parts)
//!
//! - **English** names: months, days of the week, `am`/`pm`, eras (`AD`,
//!   `BC`), calendars (`AD`, `ISO`) and a few timezones; words and ordinals as
//!   `xsl:number` writes them. Any other language falls back to English, and
//!   the result says so: "If the fallback representation uses a different
//!   language from that requested, the output string must identify the
//!   language actually used, for example by prefixing the string with
//!   \[Language: Y\]" — `[Language: en]`.
//! - The **Gregorian** calendars `AD` (the default) and `ISO`. Any other
//!   calendar falls back to `AD`, prefixed `[Calendar: AD]`.
//! - Weeks run Monday to Sunday and the first week of a year (`W`) or of a
//!   month (`w`) is the one with its first Thursday, the ISO 8601 rules, in
//!   both calendars; `F` numbers Monday 1 to Sunday 7.
//! - `Y` is the absolute value of the lexical year (§16.5.1: "year (absolute
//!   value)") and `E` is `AD` from the year 1 on and `BC` before it (`ISO`: the
//!   zero-length string / `-`), so `-0055` is 55 BC, as XML Schema 1.0 — which
//!   has no year zero — reads it and as the W3C catalog expects
//!   (format-date-en-029). The day of the week and the weeks are computed in
//!   the proleptic Gregorian calendar by the same reading, the one the upstream
//!   date arithmetic uses (`-0001-12-31` plus one day is `0001-01-01`), so
//!   `F`, `W` and `w` agree with `xs:date` arithmetic. A year `0000`, which the
//!   upstream parser accepts as XML Schema 1.1 does, is read as 1 BC, as XML
//!   Schema 1.1 Part 2 reads it (§3.3.7.1, the note: "a year value of zero
//!   represents 1 BCE"): its days are those of 1 BC, and in the `AD` calendar
//!   `Y` is `1` and `E` is `BC`, the same as for `-0001` and for the day
//!   before `0001-01-01`. The `ISO` calendar writes the lexical year as it
//!   stands (`0000` is `0`, ISO 8601's number for 1 BC).
//! - Widths (§16.5.1) count characters: a name longer than the maximum is
//!   abbreviated and never padded, a shorter one padded with spaces; an
//!   ordinal's suffix is part of a width modifier's count; a minimum width
//!   greater than the maximum is XTDE1340; the fractional seconds are rounded
//!   half-to-even and a carry out of the first digit is dropped (`.96` with
//!   `[f,1-1]` is `0`), as Saxon 9.1 does.

use crate::numbering::{is_alphanumeric, Conversion, FormatToken};

/// The components of the value being formatted that it has: an `xs:date` has
/// no time, an `xs:time` no date, and any of the three may have no timezone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DateTimeParts {
    pub(crate) date: Option<Date>,
    pub(crate) time: Option<Time>,
    /// The timezone offset in minutes, if the value has one.
    pub(crate) timezone: Option<i32>,
}

/// A proleptic Gregorian date, with its lexical year (negative before 1 AD).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Date {
    pub(crate) year: i64,
    pub(crate) month: u32,
    pub(crate) day: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Time {
    pub(crate) hour: u32,
    pub(crate) minute: u32,
    pub(crate) second: u32,
    /// The ASCII decimal digits of the fractional seconds, without trailing
    /// zeros (empty for none).
    pub(crate) fraction: String,
}

/// The calendars this implementation formats in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Calendar {
    /// "AD Anno Domini (Christian Era)", the Gregorian calendar — the default.
    Ad,
    /// "ISO ISO 8601 calendar".
    Iso,
}

/// What the language and calendar arguments settled (§16.5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) calendar: Calendar,
    /// A calendar was asked for that is valid but not supported: `AD` is used
    /// and the result starts with `[Calendar: AD]`.
    pub(crate) calendar_fallback: bool,
    /// A language other than English was asked for: English is used and the
    /// result starts with `[Language: en]`.
    pub(crate) language_fallback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            calendar: Calendar::Ad,
            calendar_fallback: false,
            language_fallback: false,
        }
    }
}

/// The 28 calendar designators of §16.5.2's table, "known to be in use during
/// the last hundred years".
const CALENDARS: [&str; 28] = [
    "AD", "AH", "AME", "AM", "AP", "AS", "BE", "CB", "CE", "CL", "CS", "EE", "FE", "ISO", "JE",
    "KE", "KY", "ME", "MS", "NS", "OS", "RS", "SE", "SH", "SS", "TE", "VE", "VS",
];

/// The calendar an unprefixed `$calendar` names (§16.5.2): "If the QName does
/// not have a prefix, then it identifies a calendar with the designator
/// specified below." `Err` for a name that is not one of them — an invalid
/// calendar, "classified as a non-recoverable dynamic error". `None` is the
/// absent argument, which takes "an implementation-defined value": `AD`.
///
/// A *prefixed* calendar name is the caller's: "the expanded-QName identifies
/// the calendar; the behavior in this case is implementation-defined" — here,
/// none is supported ([`Settings::calendar_fallback`]).
pub(crate) fn calendar_by_designator(name: Option<&str>) -> Result<(Calendar, bool), String> {
    match name {
        None | Some("AD") => Ok((Calendar::Ad, false)),
        Some("ISO") => Ok((Calendar::Iso, false)),
        Some(other) if CALENDARS.contains(&other) => Ok((Calendar::Ad, true)),
        Some(other) => Err(format!(
            "{other:?} is not one of the calendar designators of XSLT 2.0 §16.5.2 (an unprefixed \
             calendar name must be one of them)"
        )),
    }
}

/// Whether `$language` asks for a language other than English (§16.5.2). An
/// absent, zero-length or invalid value — "a value that would be valid for the
/// `xml:lang` attribute" is required — selects the default language, English,
/// with no prefix: "If the language argument is omitted or is set to an empty
/// sequence, or if it is set to an invalid value or a value that the
/// implementation does not recognize, then the processor uses an
/// implementation-defined language."
pub(crate) fn language_falls_back(language: Option<&str>) -> bool {
    let Some(language) = language.map(crate::whitespace::trim) else {
        return false;
    };
    if language.is_empty() || !crate::numbering::is_language_tag(language) {
        return false;
    }
    let primary = language.split('-').next().unwrap_or("");
    !primary.eq_ignore_ascii_case("en")
}

/// Which of the three functions is formatting: what the value has decides
/// which components are available (XTDE1350).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Function {
    Date,
    Time,
    DateTime,
}

impl Function {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Function::Date => "format-date",
            Function::Time => "format-time",
            Function::DateTime => "format-dateTime",
        }
    }
}

/// A failure of the picture string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PictureError {
    /// \[ERR XTDE1340\] "It is a non-recoverable dynamic error if the syntax
    /// of the picture is incorrect."
    Syntax(String),
    /// \[ERR XTDE1350\] "It is a non-recoverable dynamic error if a component
    /// specifier within the picture refers to components that are not
    /// available in the given type of $value".
    Unavailable(String),
}

/// The component specifiers of §16.5.1's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Component {
    /// `Y` year (absolute value)
    Year,
    /// `M` month in year
    Month,
    /// `D` day in month
    Day,
    /// `d` day in year
    DayInYear,
    /// `F` day of week
    DayOfWeek,
    /// `W` week in year
    WeekInYear,
    /// `w` week in month
    WeekInMonth,
    /// `H` hour in day (24 hours)
    Hour24,
    /// `h` hour in half-day (12 hours)
    Hour12,
    /// `P` am/pm marker
    AmPm,
    /// `m` minute in hour
    Minute,
    /// `s` second in minute
    Second,
    /// `f` fractional seconds
    Fraction,
    /// `Z` timezone as a time offset from UTC, or a timezone name
    Timezone,
    /// `z` timezone as a time offset using GMT
    GmtTimezone,
    /// `C` calendar
    CalendarName,
    /// `E` era
    Era,
}

impl Component {
    fn from_specifier(c: char) -> Option<Component> {
        Some(match c {
            'Y' => Component::Year,
            'M' => Component::Month,
            'D' => Component::Day,
            'd' => Component::DayInYear,
            'F' => Component::DayOfWeek,
            'W' => Component::WeekInYear,
            'w' => Component::WeekInMonth,
            'H' => Component::Hour24,
            'h' => Component::Hour12,
            'P' => Component::AmPm,
            'm' => Component::Minute,
            's' => Component::Second,
            'f' => Component::Fraction,
            'Z' => Component::Timezone,
            'z' => Component::GmtTimezone,
            'C' => Component::CalendarName,
            'E' => Component::Era,
            _ => return None,
        })
    }

    fn needs_date(self) -> bool {
        matches!(
            self,
            Component::Year
                | Component::Month
                | Component::Day
                | Component::DayInYear
                | Component::DayOfWeek
                | Component::WeekInYear
                | Component::WeekInMonth
                | Component::Era
        )
    }

    fn needs_time(self) -> bool {
        matches!(
            self,
            Component::Hour24
                | Component::Hour12
                | Component::AmPm
                | Component::Minute
                | Component::Second
                | Component::Fraction
        )
    }

    /// §16.5.1's "Default Presentation Modifier" column.
    fn default_presentation(self) -> Presentation {
        match self {
            Component::DayOfWeek | Component::AmPm | Component::CalendarName | Component::Era => {
                Presentation::Name(NameCase::Lower)
            }
            Component::Minute | Component::Second => Presentation::Number(FormatToken::parse("01")),
            _ => Presentation::Number(FormatToken::parse("1")),
        }
    }

    /// Whether this implementation can output the component by name.
    fn has_names(self) -> bool {
        matches!(
            self,
            Component::Month
                | Component::DayOfWeek
                | Component::AmPm
                | Component::Timezone
                | Component::CalendarName
                | Component::Era
        )
    }

    /// Whether it can be output as a number.
    fn has_numbers(self) -> bool {
        !matches!(
            self,
            Component::AmPm | Component::CalendarName | Component::Era
        )
    }
}

/// `N`, `n` or `Nn`: "the value of the component is to be output by name, in
/// lower-case, upper-case, or title-case respectively".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameCase {
    Upper,
    Lower,
    Title,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Presentation {
    Number(FormatToken),
    Name(NameCase),
}

/// One variable marker, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    component: Component,
    /// The first presentation modifier as written, if any.
    first: Option<String>,
    /// `o` (ordinal) or `t` (traditional).
    second: Option<char>,
    /// The width modifier, if any: `(min, max)`, `None` for `*`.
    width: Option<(Option<usize>, Option<usize>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Marker(Marker),
}

/// Splits a picture into literal substrings and variable markers (§16.5.1).
///
/// > The picture consists of a sequence of variable markers and literal
/// > substrings. A substring enclosed in square brackets is interpreted as a
/// > variable marker; substrings not enclosed in square brackets are taken as
/// > literal substrings. … If an opening or closing square bracket is required
/// > within a literal substring, it must be doubled.
fn parse_picture(picture: &str) -> Result<Vec<Segment>, PictureError> {
    let syntax = |why: String| PictureError::Syntax(format!("the picture {picture:?} {why}"));
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut chars = picture.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' if chars.peek() == Some(&'[') => {
                chars.next();
                literal.push('[');
            }
            ']' if chars.peek() == Some(&']') => {
                chars.next();
                literal.push(']');
            }
            ']' => {
                return Err(syntax(
                    "has a closing square bracket outside a variable marker; in a literal \
                     substring it must be doubled"
                        .into(),
                ))
            }
            '[' => {
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                let mut body = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some('[') => {
                            return Err(syntax(
                                "has a square bracket inside a variable marker".into(),
                            ))
                        }
                        // "Whitespace within a variable marker is ignored."
                        Some(c) if crate::whitespace::is_xml_whitespace(c) => {}
                        Some(c) => body.push(c),
                        None => {
                            return Err(syntax("has a variable marker with no closing ']'".into()))
                        }
                    }
                }
                segments.push(Segment::Marker(parse_marker(&body).map_err(syntax)?));
            }
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

/// One variable marker, whitespace removed: "a component specifier followed
/// optionally by one or two presentation modifiers and/or optionally by a width
/// modifier", the width modifier being `, min-width ("-" max-width)?`.
fn parse_marker(body: &str) -> Result<Marker, String> {
    let mut chars = body.chars();
    let Some(specifier) = chars.next() else {
        return Err("has an empty variable marker".into());
    };
    let component = Component::from_specifier(specifier).ok_or_else(|| {
        format!("has the variable marker [{body}], whose component specifier {specifier:?} is not one of Y M D d F W w H h P m s f Z z C E")
    })?;
    let rest = chars.as_str();
    let (modifiers, width) = match rest.split_once(',') {
        Some((modifiers, text)) => {
            let (min, max) = parse_width(text).ok_or_else(|| {
                format!(
                    "has the width modifier ,{text} in [{body}]; it must be min-width or \
                 min-width-max-width, each an integer greater than zero or *"
                )
            })?;
            // "min-width is … the minimum number of characters to be output,
            // … max-width is … the maximum number of characters to be output":
            // a minimum above the maximum asks for an output no string has, an
            // invalid picture (§16.5: "the use of an invalid picture … is
            // classified as a non-recoverable dynamic error").
            if let (Some(min), Some(max)) = (min, max) {
                if min > max {
                    return Err(format!(
                        "has the width modifier ,{text} in [{body}], whose minimum width {min} \
                         is greater than its maximum width {max}"
                    ));
                }
            }
            // Absurdly large widths are kept within reason rather than refused.
            let clamp = |w: Option<usize>| w.map(|w| w.min(1 << 16));
            (modifiers, Some((clamp(min), clamp(max))))
        }
        None => (rest, None),
    };
    let (first, second) = match modifiers.chars().last() {
        None => (None, None),
        Some(last @ ('o' | 't')) if modifiers.chars().count() >= 2 => (
            Some(modifiers[..modifiers.len() - 1].to_string()),
            Some(last),
        ),
        Some(_) => (Some(modifiers.to_string()), None),
    };
    // A presentation modifier is a format token, "a maximal sequence of
    // alphanumeric characters" (§12.3), or `n`, `N`, `Nn`.
    if let Some(first) = &first {
        if !first.chars().all(is_alphanumeric) {
            return Err(format!(
                "has the presentation modifier {first:?} in [{body}], which is not a format token"
            ));
        }
    }
    Ok(Marker {
        component,
        first,
        second,
        width,
    })
}

/// `min-width ("-" max-width)?`: "min-width is either an unsigned integer
/// indicating the minimum number of characters to be output, or * … if
/// max-width is omitted then * is assumed. Both integers, if present, must be
/// greater than zero."
fn parse_width(text: &str) -> Option<(Option<usize>, Option<usize>)> {
    let one = |part: &str| -> Option<Option<usize>> {
        if part == "*" {
            return Some(None);
        }
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: usize = part.parse().unwrap_or(usize::MAX);
        (n > 0).then_some(Some(n))
    };
    match text.split_once('-') {
        Some((min, max)) => Some((one(min)?, one(max)?)),
        None => Some((one(text)?, None)),
    }
}

/// Formats `parts` with `picture` (§16.5).
pub(crate) fn format(
    parts: &DateTimeParts,
    picture: &str,
    function: Function,
    settings: Settings,
) -> Result<String, PictureError> {
    let segments = parse_picture(picture)?;
    // §16.5.2: "the output string must identify the calendar actually used,
    // for example by prefixing the string with [Calendar: X] … the language
    // actually used, for example by prefixing the string with [Language: Y]".
    let mut out = String::new();
    if settings.calendar_fallback {
        out.push_str("[Calendar: AD]");
    }
    if settings.language_fallback {
        out.push_str("[Language: en]");
    }
    for segment in &segments {
        match segment {
            Segment::Literal(text) => out.push_str(text),
            Segment::Marker(marker) => {
                let c = marker.component;
                if (c.needs_date() && parts.date.is_none())
                    || (c.needs_time() && parts.time.is_none())
                {
                    return Err(PictureError::Unavailable(format!(
                        "{}(): the picture asks for {}, which the value does not have",
                        function.name(),
                        describe(c)
                    )));
                }
                out.push_str(&marker.format(parts, settings.calendar));
            }
        }
    }
    Ok(out)
}

fn describe(c: Component) -> &'static str {
    match c {
        Component::Year => "the year (Y)",
        Component::Month => "the month (M)",
        Component::Day => "the day in month (D)",
        Component::DayInYear => "the day in year (d)",
        Component::DayOfWeek => "the day of week (F)",
        Component::WeekInYear => "the week in year (W)",
        Component::WeekInMonth => "the week in month (w)",
        Component::Hour24 => "the hour (H)",
        Component::Hour12 => "the hour in half-day (h)",
        Component::AmPm => "the am/pm marker (P)",
        Component::Minute => "the minute (m)",
        Component::Second => "the second (s)",
        Component::Fraction => "the fractional seconds (f)",
        Component::Era => "the era (E)",
        Component::Timezone | Component::GmtTimezone | Component::CalendarName => "it",
    }
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Monday first, as `F` numbers them.
const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

impl Marker {
    /// The presentation actually used: the one asked for, or the component's
    /// default when this implementation cannot do it — "If the implementation
    /// does not support the use of the requested format token, it must use the
    /// default presentation modifier for that component."
    fn presentation(&self) -> Presentation {
        let c = self.component;
        let asked = match self.first.as_deref() {
            None => return c.default_presentation(),
            Some("N") => Presentation::Name(NameCase::Upper),
            Some("n") => Presentation::Name(NameCase::Lower),
            Some("Nn") => Presentation::Name(NameCase::Title),
            Some(token) => Presentation::Number(FormatToken::parse(token)),
        };
        let supported = match &asked {
            Presentation::Name(_) => c.has_names(),
            Presentation::Number(FormatToken::Other(_)) => false,
            Presentation::Number(_) => c.has_numbers(),
        };
        if supported {
            asked
        } else {
            c.default_presentation()
        }
    }

    /// The minimum and maximum width: the width modifier's, else the token's
    /// when it has leading zeros — "A format token containing leading zeroes,
    /// such as 001, sets the minimum and maximum width to the number of digits
    /// appearing in the format token; if a width modifier is also present, then
    /// the width modifier takes precedence."
    fn widths(&self, presentation: &Presentation) -> (Option<usize>, Option<usize>) {
        if let Some(width) = self.width {
            return width;
        }
        match presentation {
            Presentation::Number(FormatToken::Decimal { width, .. }) if *width > 1 => {
                (Some(*width), Some(*width))
            }
            _ => (None, None),
        }
    }

    fn format(&self, parts: &DateTimeParts, calendar: Calendar) -> String {
        let presentation = self.presentation();
        let (min, max) = self.widths(&presentation);
        let conversion = Conversion {
            ordinal: (self.second == Some('o')).then(|| "yes".to_string()),
            grouping: None,
        };
        let date = parts.date.unwrap_or(Date {
            year: 1,
            month: 1,
            day: 1,
        });
        let time = parts.time.clone().unwrap_or_default();
        // A width modifier counts characters, a token's own width digits (an
        // ordinal's suffix is the difference).
        let in_characters = self.width.is_some();
        let number = |n: u64, drop_high_order: bool| match &presentation {
            Presentation::Number(token) => {
                let min = min.unwrap_or(usize::from(matches!(token, FormatToken::Decimal { .. })));
                token.format_in_width(n, &conversion, min, max, drop_high_order, in_characters)
            }
            Presentation::Name(_) => n.to_string(),
        };
        let name = |full: &str, abbreviated: Option<&str>, case: NameCase| {
            fit_name(full, abbreviated, case, min, max)
        };
        match (self.component, &presentation) {
            (Component::Year, _) => number(displayed_year(date.year, calendar), true),
            (Component::Month, Presentation::Name(case)) => {
                let full = MONTHS[(date.month as usize).clamp(1, 12) - 1];
                name(full, Some(&full[..3]), *case)
            }
            (Component::Month, _) => number(u64::from(date.month), false),
            (Component::Day, _) => number(u64::from(date.day), false),
            (Component::DayInYear, _) => number(day_in_year(date), false),
            (Component::DayOfWeek, Presentation::Name(case)) => {
                let full = DAYS[day_of_week(date) as usize - 1];
                name(full, Some(&full[..3]), *case)
            }
            (Component::DayOfWeek, _) => number(day_of_week(date), false),
            (Component::WeekInYear, _) => number(week_in_year(date), false),
            (Component::WeekInMonth, _) => number(week_in_month(date), false),
            (Component::Hour24, _) => number(u64::from(time.hour), false),
            (Component::Hour12, _) => number(u64::from((time.hour + 11) % 12 + 1), false),
            (Component::AmPm, Presentation::Name(case)) => am_pm(time.hour, *case, min, max),
            (Component::Minute, _) => number(u64::from(time.minute), false),
            (Component::Second, _) => number(u64::from(time.second), false),
            (Component::Fraction, Presentation::Number(token)) => {
                fractional_seconds(&time.fraction, token, min, max)
            }
            (Component::Timezone | Component::GmtTimezone, _) => match parts.timezone {
                // "It is not an error to include a timezone component when the
                // supplied value has no timezone. In these circumstances the
                // timezone component will be ignored."
                None => String::new(),
                Some(offset) => self.timezone(offset, &presentation, min, max),
            },
            (Component::CalendarName, Presentation::Name(case)) => {
                let full = match calendar {
                    Calendar::Ad => "ad",
                    Calendar::Iso => "iso",
                };
                name(full, None, *case)
            }
            (Component::Era, Presentation::Name(case)) => match calendar {
                // "The era ("E" component) with this calendar is either a minus
                // sign (for negative years) or a zero-length string (for
                // positive years)."
                Calendar::Iso => if date.year < 0 { "-" } else { "" }.to_string(),
                Calendar::Ad => name(if date.year > 0 { "ad" } else { "bc" }, None, *case),
            },
            // A presentation `presentation()` never yields for the component.
            _ => String::new(),
        }
    }

    /// `Z` and `z` (§16.5.1). Formatting of timezones "is not fully defined by
    /// this specification. Some aspects of the formatting are
    /// implementation-dependent." Here:
    ///
    /// - by name (`Z` with `N`, `n`, `Nn`): the standard-time abbreviation of a
    ///   handful of offsets (`GMT`, `CET`, `EET`, `EST`, `CST`, `MST`, `PST`);
    ///   any other offset, and `z`, use the numeric form;
    /// - numerically: a sign, the hours and the minutes. The hours have two
    ///   digits unless a minimum width of 1 is asked for (`[z0]`, `[z,1]`); the
    ///   minutes are written when they are not zero, and always unless a
    ///   maximum width below 5 is asked for — so `[Z]` and `[z]` give `+05:00`,
    ///   `[z,2-2]` `+05` and `+05:30`, `[z0]` `+5` and `+5:30`, `[z,6-6]`
    ///   `+05:00`;
    /// - `z` is preceded by `GMT`: "For this component there is a fixed prefix
    ///   of GMT".
    fn timezone(
        &self,
        offset: i32,
        presentation: &Presentation,
        min: Option<usize>,
        max: Option<usize>,
    ) -> String {
        if let (Component::Timezone, Presentation::Name(case)) = (self.component, presentation) {
            if let Some(abbreviation) = timezone_name(offset) {
                return fit_name(abbreviation, None, *case, min, max);
            }
        }
        // `0` (a zero digit alone) asks for the hours without padding; it is
        // not a decimal format token of §12.3, so it is read here.
        let zero_token = self.first.as_deref().and_then(|t| {
            let mut chars = t.chars();
            let c = chars.next()?;
            (chars.next().is_none() && crate::numbering::decimal_digit(c) == Some((c, 0)))
                .then_some(c)
        });
        let (zero, min, max) = match (zero_token, presentation, self.width) {
            (Some(zero), _, None) => (zero, Some(1), Some(1)),
            (Some(zero), _, Some((min, max))) => (zero, min, max),
            (None, Presentation::Number(FormatToken::Decimal { zero, .. }), _) => (*zero, min, max),
            (None, _, _) => ('0', min, max),
        };
        let hours_width = min.map_or(2, |m| m.clamp(1, 2));
        let (hours, minutes) = (offset.unsigned_abs() / 60, offset.unsigned_abs() % 60);
        let digits = |n: u32, width: usize| -> String {
            format!("{n:0width$}")
                .bytes()
                .map(|d| char::from_u32(zero as u32 + u32::from(d - b'0')).unwrap_or('?'))
                .collect()
        };
        let mut out = String::new();
        if self.component == Component::GmtTimezone {
            out.push_str("GMT");
        }
        out.push(if offset < 0 { '-' } else { '+' });
        out.push_str(&digits(hours, hours_width));
        if minutes != 0 || max.is_none_or(|m| m >= 5) {
            out.push(':');
            out.push_str(&digits(minutes, 2));
        }
        out
    }
}

/// `Y`: "year (absolute value)" — the lexical year without its sign, except
/// that in the `AD` calendar the year `0000` is 1 BC (the module docs), so it
/// is the year 1 there, as `-0001` is. The `ISO` calendar writes the lexical
/// year as it stands.
fn displayed_year(year: i64, calendar: Calendar) -> u64 {
    match (calendar, year) {
        (Calendar::Ad, 0) => 1,
        _ => year.unsigned_abs(),
    }
}

/// A name in the case asked for, fitted to the widths as §16.5.1 says:
///
/// - "If the full representation of the value exceeds the specified maximum
///   width, then the processor should attempt to use an alternative shorter
///   representation that fits within the maximum width. Where the presentation
///   modifier is N, n, or Nn, this is done by abbreviating the name, using
///   either conventional abbreviations if available, or crude right-truncation
///   if not. For example, setting max-width to 4 indicates that four-letter
///   abbreviations should be used, though it would be acceptable to use a
///   three-letter abbreviation if this is in conventional use." The
///   conventional abbreviation (`abbreviated`, three letters) is used when it
///   is no shorter than `min` — "min-width is … the minimum number of
///   characters to be output" — and otherwise the name is right-truncated to
///   `max`: `[FNn,4-4]` gives `Tues`, `[MNn,4-4]` `Sept`, `[FNn,*-4]` and
///   `[FNn,3-4]` `Tue`.
/// - "If the full representation of the value is shorter than the specified
///   minimum width, then the processor should pad the value to the specified
///   width. … In other cases, it should be done by appending spaces." Only
///   the full name is padded (`[MNn,5-5]` gives `May  `); an abbreviation is
///   never shorter than `min` (a minimum above the maximum is XTDE1340).
///
/// `P` is not fitted here (see `am_pm`).
fn fit_name(
    full: &str,
    abbreviated: Option<&str>,
    case: NameCase,
    min: Option<usize>,
    max: Option<usize>,
) -> String {
    let length = full.chars().count();
    let min = min.unwrap_or(0);
    match max {
        Some(max) if length > max => {
            let name = match abbreviated {
                Some(short) if (min..=max).contains(&short.chars().count()) => short.to_string(),
                _ => full.chars().take(max).collect(),
            };
            apply_case(&name, case)
        }
        _ => {
            let mut out = apply_case(full, case);
            out.extend(std::iter::repeat_n(' ', min.saturating_sub(length)));
            out
        }
    }
}

fn apply_case(name: &str, case: NameCase) -> String {
    match case {
        NameCase::Upper => name.to_uppercase(),
        NameCase::Lower => name.to_lowercase(),
        NameCase::Title => {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        }
    }
}

/// `P`: `am` / `pm`, or `a.m.` / `p.m.` when a minimum width of four asks for
/// more, or `a` / `p` when the maximum width is one. The marker is not padded
/// with spaces: date-064 expects `12Am` from `[h][PNn,3-3]`.
fn am_pm(hour: u32, case: NameCase, min: Option<usize>, max: Option<usize>) -> String {
    let morning = hour < 12;
    let name = match (max, min) {
        (Some(1), _) => {
            if morning {
                "a"
            } else {
                "p"
            }
        }
        (_, Some(m)) if m >= 4 && max.is_none_or(|x| x >= 4) => {
            if morning {
                "a.m."
            } else {
                "p.m."
            }
        }
        _ => {
            if morning {
                "am"
            } else {
                "pm"
            }
        }
    };
    apply_case(name, case)
}

/// The fractional seconds (`f`) with a decimal token: the digits after the
/// point, "rounded to the specified size as if by applying the function
/// round-half-to-even(fractional-seconds, max-width)" when a maximum width is
/// given, and padded "by … appending zero digits" to the minimum. With neither
/// (`[f]`, `[f1]`: "an implementation-defined number of digits") every
/// significant digit is written, and at least one.
///
/// A rounding that carries out of the first digit gives the value 1
/// (`round-half-to-even(0.96, 1)` is `1.0`), whose fractional part is zero:
/// `.96` with `[f,1-1]` is `0`, `.995` with `[f,2-2]` `00`, `.9996` with
/// `[f001]` `000`. The carry is dropped and the seconds are not changed —
/// the text leaves open whether it reaches them, and `s`, `m`, `H` and the
/// date are formatted on their own. Saxon 9.1 gives the same; no catalog case
/// rounds into the next second. (Carrying into `[s]` is the other answer the
/// text allows.)
fn fractional_seconds(
    fraction: &str,
    token: &FormatToken,
    min: Option<usize>,
    max: Option<usize>,
) -> String {
    let zero = match token {
        FormatToken::Decimal { zero, .. } => *zero,
        _ => '0',
    };
    let mut digits: Vec<u8> = fraction.bytes().collect();
    if let Some(max) = max {
        if digits.len() > max {
            let first = digits[max];
            let rest_nonzero = digits[max + 1..].iter().any(|&d| d != b'0');
            digits.truncate(max);
            let last_odd = digits.last().is_some_and(|&d| (d - b'0') % 2 == 1);
            let up = first > b'5' || (first == b'5' && (rest_nonzero || last_odd));
            if up {
                // A carry out of the first digit falls off: all zeros.
                let mut i = digits.len();
                while i > 0 {
                    i -= 1;
                    if digits[i] == b'9' {
                        digits[i] = b'0';
                    } else {
                        digits[i] += 1;
                        break;
                    }
                }
            }
        }
    }
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    if digits.is_empty() {
        digits.push(b'0');
    }
    let wanted = min.unwrap_or(1);
    while digits.len() < wanted {
        digits.push(b'0');
    }
    digits
        .iter()
        .map(|&d| char::from_u32(zero as u32 + u32::from(d - b'0')).unwrap_or('?'))
        .collect()
}

/// The standard-time abbreviation of a timezone offset, for the few offsets
/// that have an unambiguous one in English usage.
fn timezone_name(offset: i32) -> Option<&'static str> {
    Some(match offset {
        0 => "GMT",
        60 => "CET",
        120 => "EET",
        -300 => "EST",
        -360 => "CST",
        -420 => "MST",
        -480 => "PST",
        _ => return None,
    })
}

// ── The Gregorian calendar ───────────────────────────────────────────────────

/// The astronomical year (…, −1, 0, 1, …) of a lexical one: XML Schema 1.0 has
/// no year zero, so `-0001` is 1 BC, the astronomical year 0, and the upstream
/// date arithmetic agrees (`-0001-12-31` plus one day is `0001-01-01`). A
/// lexical `0000` (XML Schema 1.1's 1 BC) is the astronomical year 0 too.
fn astronomical(year: i64) -> i64 {
    if year < 0 {
        year + 1
    } else {
        year
    }
}

/// The lexical year of an astronomical one.
fn lexical(year: i64) -> i64 {
    if year <= 0 {
        year - 1
    } else {
        year
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's
/// `days_from_civil`).
fn days_from_civil(date: Date) -> i64 {
    let year = astronomical(date.year);
    let (m, d) = (i64::from(date.month), i64::from(date.day));
    let y = if m <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `F` as a number: Monday 1 … Sunday 7 (ISO 8601).
fn day_of_week(date: Date) -> u64 {
    // 1970-01-01 was a Thursday (4).
    (days_from_civil(date) + 3).rem_euclid(7) as u64 + 1
}

/// `d`: 1 to 366.
fn day_in_year(date: Date) -> u64 {
    let first = Date {
        month: 1,
        day: 1,
        ..date
    };
    (days_from_civil(date) - days_from_civil(first) + 1) as u64
}

/// `W`: the ISO 8601 week number — "week 1 in any calendar year is the week
/// (from Monday to Sunday) that includes the first Thursday of that year" — so
/// the first days of January may be in week 52 or 53 of the year before.
fn week_in_year(date: Date) -> u64 {
    // The Thursday of the same week decides the week's year.
    let thursday = days_from_civil(date) + 4 - day_of_week(date) as i64;
    let year = astronomical(date.year);
    let jan1 = |year: i64| {
        days_from_civil(Date {
            year: lexical(year),
            month: 1,
            day: 1,
        })
    };
    let week_year = if thursday < jan1(year) {
        year - 1
    } else if thursday >= jan1(year + 1) {
        year + 1
    } else {
        year
    };
    ((thursday - jan1(week_year)) / 7 + 1) as u64
}

/// `w`: the week in the month, by the same rule — week 1 is the Monday-to-
/// Sunday week with the month's first Thursday, and the days before it belong
/// to the last week of the month before (format-date-011).
fn week_in_month(date: Date) -> u64 {
    let first = Date { day: 1, ..date };
    let first_dow = day_of_week(first) as i64;
    // The Monday of week 1, as an offset from the first of the month.
    let start = if first_dow <= 4 {
        1 - first_dow
    } else {
        8 - first_dow
    };
    let offset = i64::from(date.day) - 1 - start;
    if offset < 0 {
        let previous = if date.month == 1 {
            Date {
                year: lexical(astronomical(date.year) - 1),
                month: 12,
                day: 31,
            }
        } else {
            Date {
                month: date.month - 1,
                day: days_in_month(date.year, date.month - 1),
                ..date
            }
        };
        return week_in_month(previous);
    }
    (offset / 7 + 1) as u64
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(astronomical(year)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i64, month: u32, day: u32) -> DateTimeParts {
        DateTimeParts {
            date: Some(Date { year, month, day }),
            time: None,
            timezone: None,
        }
    }

    fn time(hour: u32, minute: u32, second: u32, fraction: &str) -> DateTimeParts {
        DateTimeParts {
            date: None,
            time: Some(Time {
                hour,
                minute,
                second,
                fraction: fraction.to_string(),
            }),
            timezone: None,
        }
    }

    fn date_time(d: (i64, u32, u32), t: (u32, u32, u32, &str), tz: Option<i32>) -> DateTimeParts {
        DateTimeParts {
            date: date(d.0, d.1, d.2).date,
            time: time(t.0, t.1, t.2, t.3).time,
            timezone: tz,
        }
    }

    fn fd(parts: &DateTimeParts, picture: &str) -> String {
        format(parts, picture, Function::DateTime, Settings::default())
            .unwrap_or_else(|e| panic!("{picture:?}: {e:?}"))
    }

    #[test]
    fn the_specs_gregorian_examples() {
        // §16.5.3 "Example: Gregorian Calendar", for 2002-12-31 and 15:58:45.762
        // (+02:00), each example that is in English.
        let d = date(2002, 12, 31);
        assert_eq!(fd(&d, "[Y0001]-[M01]-[D01]"), "2002-12-31");
        assert_eq!(fd(&d, "[M]-[D]-[Y]"), "12-31-2002");
        assert_eq!(fd(&d, "[D]-[M]-[Y]"), "31-12-2002");
        assert_eq!(fd(&d, "[D1] [MI] [Y]"), "31 XII 2002");
        assert_eq!(fd(&d, "[D1o] [MNn], [Y]"), "31st December, 2002");
        assert_eq!(fd(&d, "[D01] [MN,*-3] [Y0001]"), "31 DEC 2002");
        assert_eq!(fd(&d, "[MNn] [D], [Y]"), "December 31, 2002");
        assert_eq!(fd(&d, "[[[Y0001]-[M01]-[D01]]]"), "[2002-12-31]");
        // "Two Thousand and Three" is the example's 2003, not $d's 2002.
        assert_eq!(fd(&date(2003, 1, 1), "[YWw]"), "Two Thousand and Three");
        let t = DateTimeParts {
            timezone: Some(120),
            ..time(15, 58, 45, "762")
        };
        assert_eq!(fd(&t, "[h]:[m01] [PN]"), "3:58 PM");
        assert_eq!(fd(&t, "[h]:[m01]:[s01] [Pn]"), "3:58:45 pm");
        assert_eq!(fd(&t, "[H01]:[m01]"), "15:58");
        assert_eq!(fd(&t, "[H01]:[m01]:[s01].[f001]"), "15:58:45.762");
        assert_eq!(fd(&t, "[H01]:[m01]:[s01] [z,6-6]"), "15:58:45 GMT+02:00");
        let dt = date_time((2002, 12, 31), (15, 58, 45, "762"), Some(120));
        assert_eq!(
            fd(&dt, "[h].[m01][Pn] on [FNn], [D1o] [MNn]"),
            "3.58pm on Tuesday, 31st December"
        );
        assert_eq!(
            fd(&dt, "[M01]/[D01]/[Y0001] at [H01]:[m01]:[s01]"),
            "12/31/2002 at 15:58:45"
        );
        // The spec's `PDT` examples, with a -07:00 value: the names ignore
        // daylight saving time, so -07:00 is the standard-time name of that
        // offset, `MST` (the spec: "can only be done heuristically").
        let pdt = DateTimeParts {
            timezone: Some(-420),
            ..time(15, 58, 45, "")
        };
        assert_eq!(fd(&pdt, "[h]:[m01]:[s01] [PN] [ZN,*-3]"), "3:58:45 PM MST");
    }

    #[test]
    fn picture_syntax_errors_are_xtde1340() {
        let d = date(2003, 12, 7);
        for picture in [
            "[bla]", "[]", "[ ]", "[Y", "Y]", "[Y]]", "[[Y]", "[Y[M]]", "[Y,0]", "[Y,1-0]",
            "[Y,a]", "[Y,]", "[Y,-2]", "[Y,2-]", "[Y#]", "[Y1.1]", "[q]", "[Yno-t]",
        ] {
            let result = format(&d, picture, Function::Date, Settings::default());
            assert!(
                matches!(result, Err(PictureError::Syntax(_))),
                "{picture:?}: {result:?}"
            );
        }
        // A minimum width greater than the maximum asks for an output no
        // string has (compared before large widths are clamped).
        for picture in ["[Y,3-2]", "[MNn,5-4]", "[f,2-1]", "[D01,99999999-65537]"] {
            let result = format(&d, picture, Function::Date, Settings::default());
            assert!(
                matches!(&result, Err(PictureError::Syntax(m)) if m.contains("greater than its maximum")),
                "{picture:?}: {result:?}"
            );
        }
        assert_eq!(fd(&d, "[Y,4-4]|[MNn,3-3]|[D,2-2]"), "2003|Dec|07");
        // Escapes and whitespace inside markers.
        assert_eq!(fd(&d, "[[QQQ]]"), "[QQQ]");
        assert_eq!(fd(&d, "[ Y 0001 ]-[M 01]"), "2003-12");
        assert_eq!(fd(&d, ""), "");
        assert_eq!(fd(&d, "  "), "  ");
    }

    #[test]
    fn a_component_the_value_lacks_is_xtde1350() {
        for picture in ["[H]", "[h]", "[P]", "[m]", "[s]", "[f]"] {
            let result = format(
                &date(2003, 12, 7),
                picture,
                Function::Date,
                Settings::default(),
            );
            assert!(
                matches!(result, Err(PictureError::Unavailable(_))),
                "{picture}"
            );
        }
        for picture in ["[Y]", "[M]", "[D]", "[d]", "[F]", "[W]", "[w]", "[E]"] {
            let result = format(
                &time(9, 15, 6, "456"),
                picture,
                Function::Time,
                Settings::default(),
            );
            assert!(
                matches!(result, Err(PictureError::Unavailable(_))),
                "{picture}"
            );
        }
        // The timezone and the calendar are always available; a timezone the
        // value does not have is "ignored".
        assert_eq!(fd(&date(2003, 12, 7), "[Z][z][C]"), "ad");
        assert_eq!(fd(&time(9, 0, 0, ""), "[Z]|[z]|[CN]"), "||AD");
    }

    #[test]
    fn widths() {
        // format-date-013 (XSLT 2.0: fractional seconds are rounded).
        let t = date_time((985, 3, 1), (9, 15, 6, "456"), None);
        for (picture, expected) in [
            ("[Y,4-4]", "0985"),
            ("[Y,3-4]", "985"),
            ("[Y,2-5]", "985"),
            ("[Y,2-2]", "85"),
            ("[Y,2-*]", "985"),
            ("[Y,*-4]", "985"),
            ("[Y,3]", "985"),
            ("[M,4-4]", "0003"),
            ("[M,1-4]", "3"),
            ("[M,2-5]", "03"),
            ("[M,2-2]", "03"),
            ("[M,1-*]", "3"),
            ("[M,*-2]", "3"),
            ("[M,3]", "003"),
            ("[f,4-4]", "4560"),
            ("[f,1-4]", "456"),
            ("[f,2-5]", "456"),
            ("[f,2-2]", "46"),
            ("[f,1-*]", "456"),
            ("[f,*-2]", "46"),
            ("[f,3]", "456"),
            ("[f,1-1]", "5"),
            ("[f1,1-1]", "5"),
            ("[f01]", "46"),
            ("[f001]", "456"),
            ("[f0001]", "4560"),
            ("[f]", "456"),
        ] {
            assert_eq!(fd(&t, picture), expected, "{picture}");
        }
        // round-half-to-even(fractional-seconds, max-width); a carry out of
        // the first digit is dropped (round-half-to-even(0.96, 1) = 1.0, whose
        // fractional part is 0) and the seconds are left alone.
        for (fraction, expected) in [
            ("96", "07.0|07.96|07.960"),
            ("995", "07.0|07.00|07.995"),
            ("9996", "07.0|07.00|07.000"),
            ("04", "07.0|07.04|07.040"),
            ("05", "07.0|07.05|07.050"),
            ("15", "07.2|07.15|07.150"),
            ("125", "07.1|07.12|07.125"),
        ] {
            assert_eq!(
                fd(
                    &time(1, 2, 7, fraction),
                    "[s01].[f,1-1]|[s01].[f,2-2]|[s01].[f001]"
                ),
                expected,
                ".{fraction}"
            );
        }
        assert_eq!(fd(&time(1, 2, 3, "25"), "[f,1-1]"), "2");
        assert_eq!(fd(&time(1, 2, 3, "96"), "[f,*-1]|[f,2-3]"), "0|96");
        assert_eq!(fd(&time(1, 2, 3, ""), "[f]|[f001]"), "0|000");
        // "if a width modifier is also present, then the width modifier takes
        // precedence" over a token's own width (`0001`, `01`, `001`).
        assert_eq!(
            fd(
                &t,
                "[Y0001,2-2]|[Y01,*-*]|[M01,1]|[D001,2-2]|[f001,1-1]|[m01,1]|[s01,1-3]"
            ),
            "85|985|3|01|5|15|6"
        );
        // A token with leading zeros sets both widths; the year drops its
        // high-order digits, the others are written in full.
        assert_eq!(fd(&date(2003, 9, 7), "([Y01]-[M01]-[D01])"), "(03-09-07)");
        assert_eq!(fd(&date(12345, 9, 7), "[Y0001]"), "2345");
        assert_eq!(fd(&date(2003, 12, 7), "[M,1-1]"), "12");
        // Roman numerals cannot be shortened: full representation; a shorter
        // one is padded with spaces (format-date-006).
        assert_eq!(fd(&date(817, 1, 1), "[Yi,4-4]"), "dcccxvii");
        assert_eq!(fd(&date(1004, 1, 1), "[Yi,4-4]"), "miv ");
    }

    #[test]
    fn names_and_their_widths() {
        let d = date(2003, 9, 7); // a Sunday
        assert_eq!(fd(&d, "[MN]|[Mn]|[MNn]"), "SEPTEMBER|september|September");
        assert_eq!(fd(&d, "[MN,3-3]|[Mn,3-3]|[MNn,3-3]"), "SEP|sep|Sep");
        assert_eq!(fd(&d, "[FN]|[Fn]|[FNn]|[F]"), "SUNDAY|sunday|Sunday|sunday");
        assert_eq!(fd(&d, "[FNn,3-4]|[FNn,2-2]|[FNn,1-1]"), "Sun|Su|S");
        // Only a full name shorter than the minimum is padded, with spaces:
        // "If the full representation of the value is shorter than the
        // specified minimum width … In other cases, it should be done by
        // appending spaces."
        assert_eq!(fd(&date(2003, 5, 1), "[MNn,5-5]|"), "May  |");
        assert_eq!(fd(&date(2003, 5, 1), "[MNn,4-4]|[MNn,4]|"), "May |May |");
        // A name longer than the maximum is abbreviated, never padded: the
        // three-letter abbreviation when it is no shorter than the minimum,
        // else right-truncation to the maximum ("setting max-width to 4
        // indicates that four-letter abbreviations should be used … "Tuesday"
        // might be abbreviated to "Tues"").
        let tuesday = date(2002, 12, 31);
        for (picture, expected) in [
            ("[FNn,4-4] [MNn,4-4]", "Tues Dece"),
            ("[FNn,5-6]", "Tuesda"),
            ("[FNn,3-3] [MNn,3-3]", "Tue Dec"),
            ("[FNn,*-4] [MNn,*-4]", "Tue Dec"),
            ("[FNn,3-4] [MNn,3-5]", "Tue Dec"),
            ("[FNn] [MNn]", "Tuesday December"),
            ("[FNn,7-7] [MNn,8]", "Tuesday December"),
            ("[FN,4-4] [Mn,4-4]", "TUES dece"),
        ] {
            assert_eq!(fd(&tuesday, picture), expected, "{picture}");
        }
        assert_eq!(fd(&date(2002, 9, 4), "[MNn,4-4] [FNn,5-7]"), "Sept Wednesd");
        assert_eq!(fd(&date(2002, 5, 2), "[FNn,4-4]"), "Thur");
        // A name modifier on a component without names takes the default.
        assert_eq!(fd(&d, "[YN]|[DNn]"), "2003|7");
        // An unsupported token takes the default too (`[Mα]` is no sequence
        // here; `[mQ]` neither — the minute default is 01).
        assert_eq!(fd(&d, "[M\u{3b1}]"), "9");
        assert_eq!(fd(&time(9, 5, 0, ""), "[mQ]"), "05");
    }

    #[test]
    fn numbers_words_and_ordinals() {
        let d = date(2003, 12, 1);
        assert_eq!(fd(&d, "[D1o]|[Dwo]|[DWwo]|[DWo]"), "1st|first|First|FIRST");
        assert_eq!(
            fd(&date(2003, 12, 21), "[DWwo]|[Dw]"),
            "Twenty-First|twenty one"
        );
        assert_eq!(
            fd(&date(1990, 1, 1), "[YWwo]"),
            "One Thousand Nine Hundred and Ninetieth"
        );
        assert_eq!(
            fd(&date(2003, 12, 1), "[Y1o]|[YI]|[Ya]"),
            "2003rd|MMIII|bya"
        );
        // Alphabetic 0 falls back to the token 1 (format-date-012).
        assert_eq!(fd(&time(0, 0, 6, ""), "[mA].[sa]"), "0.f");
        // Other digit families (format-date-007, -008).
        assert_eq!(
            fd(
                &date(2003, 9, 7),
                "[Y\u{e50}\u{e50}\u{e50}\u{e51}]-[M\u{e50}\u{e51}]-[D\u{e51}]"
            ),
            "\u{e52}\u{e50}\u{e50}\u{e53}-\u{e50}\u{e59}-\u{e57}"
        );
        // `t` is accepted and changes nothing in English.
        assert_eq!(fd(&date(2003, 9, 7), "[Dit]|[D1t]"), "vii|7");
        // A width modifier counts characters ("the minimum number of
        // characters to be output"), the ordinal suffix included; a token's
        // own width counts digits, as for xsl:number.
        let third = date(2003, 12, 3);
        for (picture, expected) in [
            ("[D1o,2]", "3rd"),
            ("[D1o,3-3]", "3rd"),
            ("[D1o,4]", "03rd"),
            ("[D1o,5-5]", "003rd"),
            // Nothing fits two characters: the full representation.
            ("[D1o,1-2]", "3rd"),
            ("[D01o]", "03rd"),
            ("[D001o]", "003rd"),
            ("[D01o,2]", "3rd"),
            // The year leaves room for the suffix, and keeps all its digits
            // when not one fits beside it.
            ("[Y1o,4-4]", "03rd"),
            ("[Y1o,1-3]", "3rd"),
            ("[Y1o,2-2]", "2003rd"),
            ("[Y01o]", "03rd"),
            ("[Y0001o]", "2003rd"),
            ("[Dwo,*-3]", "third"),
            ("[Dwo,7]", "third  "),
        ] {
            assert_eq!(fd(&third, picture), expected, "{picture}");
        }
        assert_eq!(fd(&date(2011, 12, 23), "[D1o,2-2]|[Y1o,4-4]"), "23rd|11th");
    }

    #[test]
    fn hours_and_the_am_pm_marker() {
        for (hour, h, p) in [
            (0, "12", "am"),
            (1, "1", "am"),
            (11, "11", "am"),
            (12, "12", "pm"),
            (13, "1", "pm"),
            (23, "11", "pm"),
        ] {
            assert_eq!(
                fd(&time(hour, 10, 0, ""), "[h]|[P]"),
                format!("{h}|{p}"),
                "{hour}"
            );
        }
        // date-064's widths: never padded, a.m. when four characters are asked
        // for, a single letter for one.
        let t = time(9, 30, 0, "");
        assert_eq!(
            fd(
                &t,
                "[PNn,4-8]|[PNn,4-4]|[PNn,3-3]|[PNn,2-2]|[PNn,1-1]|[PN,2-2]|[Pn,2-2]"
            ),
            "A.m.|A.m.|Am|Am|A|AM|am"
        );
    }

    #[test]
    fn timezones() {
        let at = |minutes: i32| date_time((985, 3, 1), (9, 15, 6, ""), Some(minutes));
        // format-date-014 … -018.
        for (offset, z, gmt, gmt66, gmt22, gmt0) in [
            (-840, "-14:00", "GMT-14:00", "GMT-14:00", "GMT-14", "GMT-14"),
            (
                -810,
                "-13:30",
                "GMT-13:30",
                "GMT-13:30",
                "GMT-13:30",
                "GMT-13:30",
            ),
            (
                -570,
                "-09:30",
                "GMT-09:30",
                "GMT-09:30",
                "GMT-09:30",
                "GMT-9:30",
            ),
            (-540, "-09:00", "GMT-09:00", "GMT-09:00", "GMT-09", "GMT-9"),
            (
                -30,
                "-00:30",
                "GMT-00:30",
                "GMT-00:30",
                "GMT-00:30",
                "GMT-0:30",
            ),
            (0, "+00:00", "GMT+00:00", "GMT+00:00", "GMT+00", "GMT+0"),
            (
                330,
                "+05:30",
                "GMT+05:30",
                "GMT+05:30",
                "GMT+05:30",
                "GMT+5:30",
            ),
            (840, "+14:00", "GMT+14:00", "GMT+14:00", "GMT+14", "GMT+14"),
        ] {
            assert_eq!(
                fd(&at(offset), "[Z]|[z]|[z,6-6]|[z,2-2]|[z0]"),
                format!("{z}|{gmt}|{gmt66}|{gmt22}|{gmt0}"),
                "{offset}"
            );
        }
        assert_eq!(fd(&at(-300), "[ZN]|[Zn]"), "EST|est");
        assert_eq!(fd(&at(345), "[ZN]"), "+05:45");
        // "Component specifier "Z" with the presentation modifier "N" is used
        // to request timezone names such as "PST" or "CET"": every name of the
        // table, and the numeric form for the offsets around them.
        for (offset, name) in [
            (0, "GMT"),
            (60, "CET"),
            (120, "EET"),
            (-300, "EST"),
            (-360, "CST"),
            (-420, "MST"),
            (-480, "PST"),
            (-540, "-09:00"),
            (180, "+03:00"),
        ] {
            assert_eq!(fd(&at(offset), "[ZN]"), name, "{offset}");
        }
        assert_eq!(fd(&at(-480), "[ZNn]|[ZN,5-5]|[ZN,*-2]"), "Pst|PST  |PS");
    }

    #[test]
    fn weeks_and_days() {
        // format-date-009: [W] (ISO weeks), -010: [F01], -011: [w].
        for ((y, m, d), week) in [
            ((2004, 1, 1), 1),
            ((2005, 1, 1), 53),
            ((2006, 1, 1), 52),
            ((2007, 1, 1), 1),
            ((2004, 12, 1), 49),
            ((2007, 10, 1), 40),
        ] {
            assert_eq!(fd(&date(y, m, d), "[W]"), week.to_string(), "{y}-{m}-{d}");
        }
        for ((y, m, d), dow) in [
            ((2004, 1, 1), "04"),
            ((2004, 2, 1), "07"),
            ((2006, 5, 1), "01"),
        ] {
            assert_eq!(fd(&date(y, m, d), "[F01]"), dow, "{y}-{m}-{d}");
        }
        for ((y, m, d), week) in [
            ((2005, 12, 4), 1),
            ((2005, 12, 7), 2),
            ((2005, 12, 31), 5),
            ((2006, 1, 3), 1),
            ((2006, 1, 30), 5),
            ((2006, 2, 2), 1),
            ((2006, 2, 26), 4),
            ((2006, 3, 19), 3),
            ((2006, 4, 3), 1),
            ((2006, 4, 9), 1),
            // Before week 1: the last week of the month before.
            ((2006, 4, 1), 5),
            ((2006, 1, 1), 5),
        ] {
            assert_eq!(fd(&date(y, m, d), "[w]"), week.to_string(), "{y}-{m}-{d}");
        }
        assert_eq!(fd(&date(2004, 12, 31), "[d]"), "366");
        assert_eq!(fd(&date(2003, 12, 31), "[d]"), "365");
        assert_eq!(fd(&date(-1, 12, 31), "[F]"), day_name_of(&date(-1, 12, 31)));
    }

    fn day_name_of(parts: &DateTimeParts) -> String {
        DAYS[day_of_week(parts.date.unwrap()) as usize - 1].to_string()
    }

    #[test]
    fn eras_and_calendars() {
        // format-date-en-029: -0055 is 55 BC.
        assert_eq!(fd(&date(1990, 12, 1), "[Y][EN]"), "1990AD");
        assert_eq!(fd(&date(-55, 12, 1), "[Y][EN]"), "55BC");
        let iso = Settings {
            calendar: Calendar::Iso,
            ..Settings::default()
        };
        let in_iso = |parts: &DateTimeParts, picture: &str| {
            format(parts, picture, Function::Date, iso).unwrap()
        };
        assert_eq!(in_iso(&date(-55, 12, 1), "[E][Y]|[CN]"), "-55|ISO");
        assert_eq!(in_iso(&date(2004, 1, 1), "[E][Y]"), "2004");
        // The year 0000 is 1 BC (XML Schema 1.1: "a year value of zero
        // represents 1 BCE"), like -0001 (XML Schema 1.0); the day before
        // 0001-01-01, a Monday, is a Sunday, the last day of a leap year.
        for (day, expected) in [
            (date(1, 1, 1), "1 AD Monday 1"),
            (date(0, 12, 31), "1 BC Sunday 366"),
            (date(-1, 12, 31), "1 BC Sunday 366"),
            (date(0, 1, 1), "1 BC Saturday 1"),
            (date(-2, 12, 31), "2 BC Friday 365"),
        ] {
            assert_eq!(fd(&day, "[Y] [EN] [FNn] [d]"), expected, "{day:?}");
        }
        assert_eq!(fd(&date(0, 6, 1), "[Y0001][EN]|[Y,2-2]"), "0001BC|01");
        // The ISO calendar writes the lexical year (ISO 8601 numbers 1 BC 0).
        assert_eq!(in_iso(&date(0, 12, 31), "[E][Y0001]"), "0000");
        // The fallbacks of §16.5.2.
        let fallback = Settings {
            calendar_fallback: true,
            language_fallback: true,
            ..Settings::default()
        };
        assert_eq!(
            format(&date(2006, 3, 1), "[M01]", Function::Date, fallback).unwrap(),
            "[Calendar: AD][Language: en]03"
        );
    }

    #[test]
    fn calendar_designators_and_languages() {
        assert_eq!(calendar_by_designator(None), Ok((Calendar::Ad, false)));
        assert_eq!(
            calendar_by_designator(Some("AD")),
            Ok((Calendar::Ad, false))
        );
        assert_eq!(
            calendar_by_designator(Some("ISO")),
            Ok((Calendar::Iso, false))
        );
        for other in ["CB", "OS", "AH", "AM", "CE", "VS"] {
            assert_eq!(
                calendar_by_designator(Some(other)),
                Ok((Calendar::Ad, true)),
                "{other}"
            );
        }
        for invalid in ["ad", "Gregorian", "XX"] {
            assert!(calendar_by_designator(Some(invalid)).is_err(), "{invalid}");
        }
        for (language, falls_back) in [
            (None, false),
            (Some(""), false),
            (Some("en"), false),
            (Some("EN-gb"), false),
            (Some("en-US"), false),
            (Some("de"), true),
            (Some("xib"), true),
            (Some("sv"), true),
            (Some("12!"), false),
        ] {
            assert_eq!(language_falls_back(language), falls_back, "{language:?}");
        }
    }

    #[test]
    fn proleptic_gregorian_arithmetic() {
        assert_eq!(
            days_from_civil(Date {
                year: 1970,
                month: 1,
                day: 1
            }),
            0
        );
        assert_eq!(
            days_from_civil(Date {
                year: 2000,
                month: 3,
                day: 1
            }),
            11017
        );
        // 0001-01-01 follows 0000-12-31, a leap year (ISO 8601: 1 BC).
        assert_eq!(
            days_from_civil(Date {
                year: 1,
                month: 1,
                day: 1
            }) - days_from_civil(Date {
                year: 0,
                month: 12,
                day: 31
            }),
            1
        );
        assert_eq!(
            days_from_civil(Date {
                year: 1,
                month: 1,
                day: 1
            }) - days_from_civil(Date {
                year: 0,
                month: 1,
                day: 1
            }),
            366
        );
        assert_eq!(
            day_of_week(Date {
                year: 2002,
                month: 12,
                day: 31
            }),
            2
        );
        assert!(is_leap(2000) && !is_leap(1900) && is_leap(2004));
    }
}
