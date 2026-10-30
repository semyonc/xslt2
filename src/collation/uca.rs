//! The UCA collations of F&O 3.1 §5.3.3 on ICU4X 1.5 (`icu_collator`, compiled
//! CLDR data) — the `icu-collation` Cargo feature.
//!
//! > \[F&O 3.1 §5.3.3\] This family of URIs use the scheme and path
//! > `http://www.w3.org/2013/collation/UCA` followed by an optional query part.
//! > The query part, if present, consists of a question mark followed by a
//! > sequence of zero or more semicolon-separated parameters. Each parameter is
//! > a keyword-value pair, the keyword and value being separated by an equals
//! > sign.
//!
//! XSLT 2.0 leaves the set of collation URIs to the implementation (§13.1.3);
//! recognizing this family is this processor's choice, and it follows the 3.1
//! rules for it.
//!
//! # Parameters
//!
//! | keyword | values honoured | ICU4X 1.5 |
//! |---|---|---|
//! | `fallback` | `yes`, `no` | — (decides what an unsupported parameter does) |
//! | `lang` | an `xs:language` that ICU4X parses as a BCP 47 tag, with the `-u-` collation keywords below | the locale of the tailoring |
//! | `version` | a UCA version up to [`UCA_VERSION`] (15.1.0) | the data's UCA version is fixed |
//! | `strength` | `primary` … `identical`, `1` … `5` | `Strength` |
//! | `maxVariable` | `space`, `punct`, `symbol`, `currency` | `MaxVariable` |
//! | `alternate` | `non-ignorable`, `shifted`, `blanked` (see below) | `AlternateHandling` |
//! | `backwards` | `yes`, `no` | `BackwardSecondLevel` |
//! | `normalization` | `yes`, `no` | always normalizes (see below) |
//! | `caseLevel` | `yes`, `no` | `CaseLevel` |
//! | `caseFirst` | `upper`, `lower` | `CaseFirst` |
//! | `numeric` | `yes`, `no` | `Numeric` |
//! | `reorder` | none | "Reordering … currently cannot be set via the API" |
//!
//! > If two query parameters use the same keyword then the last one wins. If a
//! > query parameter uses a keyword or value which is not defined in this
//! > specification then the meaning is implementation-defined. If the
//! > implementation recognizes the meaning of the keyword and value then it
//! > should interpret it accordingly; if it does not recognize the keyword or
//! > value then if the fallback parameter is present with the value `no` it
//! > should reject the collation as unsupported, otherwise it should ignore the
//! > unrecognized parameter.
//!
//! So an unsupported parameter (`reorder`, an unknown keyword, a value outside
//! the list) is ignored, unless the URI says `fallback=no`; then the registry
//! does not recognize the URI, and the caller raises its "unrecognized
//! collation" error:
//!
//! > If the `fallback` parameter is present with the value `no`, then the
//! > implementation must either use a collation that conforms with the rules in
//! > the Unicode specifications for the requested tailoring, or fail with a
//! > static or dynamic error indicating that it does not provide the collation
//! > (the error code should be the same as if the collation URI were not
//! > recognized).
//!
//! **Separators.** The specification separates parameters with `;` only. A
//! `&` is accepted as well, as this processor's reading of what F&O leaves
//! implementation-defined: no defined value can contain `&`, so a `&` could
//! only make a value unrecognized — and `UCA?lang=de&strength=primary` would
//! silently be the root collation at tertiary strength, and
//! `UCA?fallback=no&version=1` would silently lose `fallback=no`.
//!
//! **`alternate=blanked`.** UTS #10 §4 "Variable Weighting": "Blanked:
//! Variable collation elements and any subsequent ignorable collation elements
//! are reset so that all weights (except for the identical level) are zero.
//! This is equivalent to the Shifted option used with strength=tertiary."
//! ICU4X 1.5 has no blanked mode, so `blanked` is `shifted` with the strength
//! lowered to tertiary at most — exact for strengths 1 to 4 (blanked has no
//! quaternary level to compare). With `strength=identical` the identical level
//! that blanked keeps has no ICU4X equivalent after `shifted`: that
//! combination is approximated by tertiary strength, or refused with
//! `fallback=no`.
//!
//! **`version`.** "The version number of the UCA to be used." The compiled data
//! is ICU 75's (UCA 15.1.0, see [`UCA_VERSION`]). A version up to that one is
//! answered with the 15.1.0 tables (the same choice Saxon documents: "Requests
//! for versions less than or equal to the current supported version are
//! processed with the current version"); a later version, or a value that is
//! not a dotted version number, is an unrecognized value.
//!
//! `normalization` is honoured both ways because ICU4X always normalizes its
//! input ("Normalization is always enabled and cannot be turned off"):
//! `normalization=no` only lets an implementation skip that step for input
//! already in FCD form, with the same result, so normalizing is a conforming
//! answer to either value.
//!
//! **Unicode extensions in `lang`** (UTS #35 collation keywords). ICU4X 1.5
//! reads only `-u-co-` (the collation type) from the locale; this module reads
//! the others itself: `kf` (caseFirst: `upper`, `lower`, `false`), `kn`
//! (numeric), `ks` (strength: `level1` … `level4`, `identic`), `ka` (alternate:
//! `noignore`, `shifted`), `kb` (backwards), `kc` (caseLevel), `kv`
//! (maxVariable) and `kk` (normalization, see above). An explicit URI parameter
//! wins over the same setting in `lang` — the precedence of ICU and ECMA-402
//! (`new Intl.Collator("en-u-kf-upper", {caseFirst: "lower"})` resolves to
//! `lower`): a parameter is an explicit setting, the keyword part of a
//! language's description. A `co` type the data does not have for the
//! language (`de-u-co-bogus`, `en-u-co-phonebk`), the keywords ICU4X cannot
//! set (`kr`, `kh`, `vt`), an invalid keyword value, `-u-` attributes and the
//! other extensions (`-t-`, `-x-`, …) are unsupported: ignored, or with
//! `fallback=no` the URI is not recognized. Keywords that are not about
//! collation (`-u-nu-`, `-u-ca-`, …) do not change a collation and are
//! ignored either way.
//!
//! A parameter the URI leaves out takes the **tailoring's** default — for the
//! root collation exactly the defaults F&O lists (`strength` tertiary,
//! `maxVariable` punct, `alternate` non-ignorable, `backwards`, `caseLevel`,
//! `numeric` no); a language may change some of them as UTS #35 specifies:
//! Danish puts upper case first, Canadian French (`fr-CA`) compares accents
//! backwards, and Thai (`th`) is `alternate=shifted`, so under `UCA?lang=th`
//! spaces and punctuation are ignored up to the tertiary level
//! (`compare('a b', 'ab')` is 0). That is what "the language whose collation
//! conventions are to be used" asks for, and what ICU4C does; F&O: "The
//! meaning given for each parameter is non-normative; the normative
//! specification is found in \[UTS #35\]."
//!
//! A URI that is not well-formed by these rules — a parameter without `=`, or
//! anything other than a query part after the path — is not recognized, with or
//! without `fallback`: the obligation to accept ("if the collation URI is
//! well-formed according to the rules in this section, then the implementation
//! must accept the collation URI") covers only well-formed URIs. An empty
//! parameter (`?;` or a trailing `;`) is skipped.
//!
//! # Substring matching
//!
//! ICU4X 1.5 offers `Collator::compare` and nothing else: its collation
//! elements are private and "neither ECMA-402 nor ICU4X provides prefix
//! matching or substring matching API surface" (`icu_collator` 1.5,
//! `CollatorOptions` docs). The five substring functions are therefore
//! answered with `compare` alone, by testing substrings of `$arg1` cut at
//! **character** (code point) boundaries for collation-equality with `$arg2`
//! — UTS #10 DS2: "The pattern string P has a match at Q\[s,e\] according to
//! collation C if C generates the same sort key for P as for Q\[s,e\]", with
//! F&O 2.0 §7.5's boundary condition B approximated by character boundaries:
//!
//! * `starts-with` / `ends-with` (§7.5.2, §7.5.3: "starts with a sequence of
//!   collation units that provides a match"): some prefix (suffix) of `$arg1`
//!   is equal to `$arg2` — at most *n* + 1 comparisons;
//! * `contains`, `substring-before`, `substring-after` (§7.5.1, §7.5.4,
//!   §7.5.5: "the first occurrence of a sequence of collation units that
//!   provides a minimal match"): the leftmost **minimal** match, UTS #10 DS4:
//!   "The match is minimal if there is no match at Q\[s+i,e-j\] for any i and
//!   j such that i ≥ 0, j ≥ 0, and i + j > 0", and DS6 for "first": the least
//!   start offset. A minimal match neither begins nor ends with characters
//!   that are ignorable there, which is what F&O's examples ask:
//!   `substring-before("abc--d-e-fghi", "--d-e-", CollationA)` (with `-`
//!   ignorable) "returns "abc--"", and `substring-after` of the same "-fghi".
//!
//! Comparing every candidate would be cubic (*n*(*n* + 1)/2 candidates, each
//! comparison reading up to the shared prefix): 21.5 s for 2,000 characters.
//! `compare`'s answer alone cannot tell whether a longer candidate may still
//! match — a contraction or canonical reordering can turn an unequal
//! candidate into an equal one — so the search (`search.rs`) reads the
//! collation data to learn which characters' collation elements cannot
//! depend on their neighbours, and with that narrows the candidates exactly:
//! a filter on the primary weights (or on their number) picks the few
//! candidates worth comparing, and elsewhere the scan from a start stops at
//! the first safe boundary from which no candidate can match. Ordinary text
//! of 100,000 characters takes milliseconds. A long run of characters chained
//! by contractions or prefix contexts (Danish `aaaa…`, Japanese `カーカー…`)
//! is still searched candidate by candidate, within a work budget of about a
//! second; beyond it the function raises FOCH0004 rather than answer late —
//! never a wrong answer.
//!
//! The zero-length rules come first, as F&O orders them: "If the value of
//! $arg1 or $arg2 is the empty sequence, or contains only ignorable collation
//! units, it is interpreted as the zero-length string. If the value of $arg2
//! is the zero-length string, then the function returns true. If the value of
//! $arg1 is the zero-length string, the function returns false." An argument
//! that compares equal to `""` is all-ignorable.
//!
//! **What character boundaries cannot do.** A collation unit that spans
//! several characters (a contraction: Slovak `ch`) is split when a match
//! begins or ends inside it — `contains('chata', 'h', UCA?lang=sk)` is true
//! although `h` is not a collation unit of `ch` — and a character that
//! expands to several collation units (`ß` = `ss` at primary strength) is
//! never split — `contains('ß', 's', UCA?strength=primary)` is false, although
//! F&O's own example ("treating "ä" as representing two collation units, in
//! which case the expression fn:contains("Jäger", "eg") will return true")
//! would match inside it. Whole characters do match: `contains('Straße', 'ss',
//! UCA?strength=primary)` is true and `substring-before` gives `Stra`.
//!
//! # Sort keys
//!
//! ICU4X 1.5 has no sort keys either, so `=`, `distinct-values()` and
//! `index-of()` compare strings under a UCA collation pairwise (upstream), and
//! `xsl:key` and `group-by` keep them in an index ordered by `compare`
//! (`runtime/keys.rs`): O(log n) comparisons per lookup.

use std::cell::{OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::str::FromStr;

use icu_collator::provider::{
    Baked, CollationDataV1Marker, CollationDiacriticsV1Marker, CollationMetadataV1Marker,
    CollationReorderingV1Marker,
};
use icu_collator::{
    AlternateHandling, BackwardSecondLevel, CaseFirst, CaseLevel, Collator, CollatorOptions,
    MaxVariable, Numeric, Strength,
};
use icu_locid::extensions::unicode::{key, Key};
use icu_locid::Locale;
use icu_provider::{DataLocale, DataProvider, DataRequest, KeyedDataMarker};

use super::{CaseOrder, CaseOrderCollation, Collation, UCA_COLLATION_PREFIX};

mod search;

/// The UCA version of the compiled collation data: `icu_collator_data` 1.5.1
/// is generated from the ICU 75 export (`[package.metadata.sources.icuexport]
/// tagged = "icu4x/2024-05-16/75.x"`), and ICU 75 implements Unicode and the
/// UCA 15.1. The unit test `the_data_is_uca_15_1` checks it on the data itself:
/// a CJK Extension I ideograph (new in 15.1) has its implicit weight, a
/// Unicode 16.0 letter does not.
const UCA_VERSION: [u32; 3] = [15, 1, 0];

/// `strength`: "primary | secondary | tertiary | quaternary | identical, or
/// 1|2|3|4|5 as synonyms".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Level {
    Primary,
    Secondary,
    Tertiary,
    Quaternary,
    Identical,
}

/// `maxVariable`: "space | punct | symbol | currency".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Variable {
    Space,
    Punct,
    Symbol,
    Currency,
}

/// `caseFirst` (`upper`, `lower`), and the `-u-kf-false` of a language tag
/// (no case preference).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CaseFirstSetting {
    Upper,
    Lower,
    Off,
}

impl From<CaseOrder> for CaseFirstSetting {
    fn from(order: CaseOrder) -> Self {
        match order {
            CaseOrder::UpperFirst => CaseFirstSetting::Upper,
            CaseOrder::LowerFirst => CaseFirstSetting::Lower,
        }
    }
}

/// The collator options a tailoring asks for, every field `None` where the
/// request leaves it to the language's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Settings {
    strength: Option<Level>,
    max_variable: Option<Variable>,
    /// `alternate`: `shifted` (true) or `non-ignorable` (false); `blanked` is
    /// resolved to `shifted` by the parser.
    shifted: Option<bool>,
    backwards: Option<bool>,
    case_level: Option<bool>,
    case_first: Option<CaseFirstSetting>,
    numeric: Option<bool>,
}

impl Settings {
    /// `self`, with each setting it leaves open taken from `defaults`.
    fn or(self, defaults: Settings) -> Settings {
        Settings {
            strength: self.strength.or(defaults.strength),
            max_variable: self.max_variable.or(defaults.max_variable),
            shifted: self.shifted.or(defaults.shifted),
            backwards: self.backwards.or(defaults.backwards),
            case_level: self.case_level.or(defaults.case_level),
            case_first: self.case_first.or(defaults.case_first),
            numeric: self.numeric.or(defaults.numeric),
        }
    }
}

/// A tailoring of the UCA: what a UCA URI or an `xsl:sort` asks for. Two
/// requests that differ only in the order or spelling of their parameters are
/// one tailoring.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Tailoring {
    /// The locale handed to ICU4X, in its canonical spelling: the language
    /// identifier of `lang` and its `-u-co-` type when the data has it; `None`
    /// for the root collation.
    locale: Option<String>,
    settings: Settings,
}

impl Tailoring {
    fn options(&self) -> CollatorOptions {
        let settings = &self.settings;
        let mut options = CollatorOptions::new();
        options.strength = settings.strength.map(|level| match level {
            Level::Primary => Strength::Primary,
            Level::Secondary => Strength::Secondary,
            Level::Tertiary => Strength::Tertiary,
            Level::Quaternary => Strength::Quaternary,
            Level::Identical => Strength::Identical,
        });
        options.max_variable = settings.max_variable.map(|variable| match variable {
            Variable::Space => MaxVariable::Space,
            Variable::Punct => MaxVariable::Punctuation,
            Variable::Symbol => MaxVariable::Symbol,
            Variable::Currency => MaxVariable::Currency,
        });
        options.alternate_handling = settings.shifted.map(|shifted| {
            if shifted {
                AlternateHandling::Shifted
            } else {
                AlternateHandling::NonIgnorable
            }
        });
        options.backward_second_level = settings.backwards.map(|on| {
            if on {
                BackwardSecondLevel::On
            } else {
                BackwardSecondLevel::Off
            }
        });
        options.case_level =
            settings
                .case_level
                .map(|on| if on { CaseLevel::On } else { CaseLevel::Off });
        options.case_first = settings.case_first.map(|order| match order {
            CaseFirstSetting::Upper => CaseFirst::UpperFirst,
            CaseFirstSetting::Lower => CaseFirst::LowerFirst,
            CaseFirstSetting::Off => CaseFirst::Off,
        });
        options.numeric = settings
            .numeric
            .map(|on| if on { Numeric::On } else { Numeric::Off });
        options
    }

    /// The ICU4X locale of the tailoring: the root collation without one.
    fn locale(&self) -> DataLocale {
        self.locale
            .as_deref()
            .and_then(|tag| DataLocale::from_str(tag).ok())
            .unwrap_or_default()
    }
}

/// A parsed UCA collation URI.
#[derive(Debug, PartialEq, Eq)]
enum UcaUri {
    /// Well-formed, every parameter honoured or (with `fallback=yes`) ignored.
    Accepted {
        tailoring: Tailoring,
        fallback: bool,
    },
    /// Not a collation this processor provides: ill-formed, or `fallback=no`
    /// with a parameter it cannot honour.
    Unsupported,
}

/// Parses `uri` as a member of the UCA family (F&O 3.1 §5.3.3); `None` when it
/// is not one (it does not start with the family's scheme and path followed
/// by the end of the URI or a query part).
fn parse_uca_uri(uri: &str) -> Option<UcaUri> {
    let rest = uri.strip_prefix(UCA_COLLATION_PREFIX)?;
    let query = if rest.is_empty() {
        ""
    } else {
        rest.strip_prefix('?')?
    };
    // "If two query parameters use the same keyword then the last one wins":
    // first collect, so that `fallback` is known before any other parameter is
    // judged, wherever it appears. `;` is the specification's separator, `&`
    // this processor's addition (module docs).
    let mut params: Vec<(&str, &str)> = Vec::new();
    for parameter in query.split([';', '&']).filter(|p| !p.is_empty()) {
        let Some((keyword, value)) = parameter.split_once('=') else {
            return Some(UcaUri::Unsupported);
        };
        if keyword.is_empty() {
            return Some(UcaUri::Unsupported);
        }
        match params.iter_mut().find(|(k, _)| *k == keyword) {
            Some(slot) => slot.1 = value,
            None => params.push((keyword, value)),
        }
    }
    // "fallback: yes | no (default yes)". A value other than `no` is not a
    // request to refuse, so an unrecognized one leaves the default.
    let fallback = !params.contains(&("fallback", "no"));
    let mut explicit = Settings::default();
    let mut lang: Option<Lang> = None;
    let mut blanked = false;
    for (keyword, value) in params {
        let honoured = match keyword {
            "fallback" => matches!(value, "yes" | "no"),
            "lang" => match parse_lang(value) {
                Some(parsed) => {
                    let exact = parsed.exact;
                    lang = Some(parsed);
                    exact
                }
                None => false,
            },
            "version" => version_supported(value),
            "strength" => set(&mut explicit.strength, strength(value)),
            "maxVariable" => set(&mut explicit.max_variable, max_variable(value)),
            "alternate" => match value {
                "non-ignorable" => set(&mut explicit.shifted, Some(false)),
                "shifted" => set(&mut explicit.shifted, Some(true)),
                "blanked" => {
                    blanked = true;
                    true
                }
                _ => false,
            },
            "backwards" => set(&mut explicit.backwards, yes_no(value)),
            "caseLevel" => set(&mut explicit.case_level, yes_no(value)),
            "numeric" => set(&mut explicit.numeric, yes_no(value)),
            "caseFirst" => set(
                &mut explicit.case_first,
                match value {
                    "upper" => Some(CaseFirstSetting::Upper),
                    "lower" => Some(CaseFirstSetting::Lower),
                    _ => None,
                },
            ),
            // ICU4X always normalizes, which conforms to both values (module
            // docs).
            "normalization" => yes_no(value).is_some(),
            // `reorder`: not settable in ICU4X 1.5. Any other keyword is one
            // this processor does not define.
            _ => false,
        };
        if !honoured && !fallback {
            return Some(UcaUri::Unsupported);
        }
    }
    // An explicit parameter wins over the same setting in `lang`'s Unicode
    // extension (module docs).
    let (locale, from_lang) = match lang {
        Some(Lang {
            locale, settings, ..
        }) => (Some(locale), settings),
        None => (None, Settings::default()),
    };
    let mut settings = explicit.or(from_lang);
    if blanked {
        // UTS #10: blanked "is equivalent to the Shifted option used with
        // strength=tertiary" — exact up to quaternary strength, where blanked
        // has nothing to compare; the identical level it keeps has no
        // equivalent after `shifted`.
        if settings.strength == Some(Level::Identical) && !fallback {
            return Some(UcaUri::Unsupported);
        }
        settings.shifted = Some(true);
        settings.strength = Some(
            settings
                .strength
                .unwrap_or(Level::Tertiary)
                .min(Level::Tertiary),
        );
    }
    Some(UcaUri::Accepted {
        tailoring: Tailoring { locale, settings },
        fallback,
    })
}

/// Stores a recognized value; whether there was one.
fn set<T>(slot: &mut Option<T>, value: Option<T>) -> bool {
    let honoured = value.is_some();
    if honoured {
        *slot = value;
    }
    honoured
}

fn yes_no(value: &str) -> Option<bool> {
    match value {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn strength(value: &str) -> Option<Level> {
    match value {
        "primary" | "1" => Some(Level::Primary),
        "secondary" | "2" => Some(Level::Secondary),
        "tertiary" | "3" => Some(Level::Tertiary),
        "quaternary" | "4" => Some(Level::Quaternary),
        "identical" | "5" => Some(Level::Identical),
        _ => None,
    }
}

fn max_variable(value: &str) -> Option<Variable> {
    match value {
        "space" => Some(Variable::Space),
        "punct" => Some(Variable::Punct),
        "symbol" => Some(Variable::Symbol),
        "currency" => Some(Variable::Currency),
        _ => None,
    }
}

/// Whether `version` ("The version number of the UCA to be used") is a dotted
/// sequence of numbers no later than [`UCA_VERSION`] — missing components
/// count as 0, so `15.1` is `15.1.0` and `6` is `6.0.0`.
fn version_supported(version: &str) -> bool {
    let parts: Option<Vec<u32>> = version
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect();
    let Some(parts) = parts else {
        return false;
    };
    let width = parts.len().max(UCA_VERSION.len());
    let requested = (0..width).map(|i| parts.get(i).copied().unwrap_or(0));
    let available = (0..width).map(|i| UCA_VERSION.get(i).copied().unwrap_or(0));
    requested.cmp(available) != Ordering::Greater
}

/// A parsed `lang` value.
#[derive(Debug, PartialEq, Eq)]
struct Lang {
    /// The locale for ICU4X (see [`Tailoring::locale`]).
    locale: String,
    /// The collation settings of its Unicode extension.
    settings: Settings,
    /// Whether every part of the tag is honoured; with `fallback=no` only an
    /// exact tag is accepted.
    exact: bool,
}

/// `lang`: "a string in the lexical space of `xs:language`"
/// (`[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`) that ICU4X parses as a BCP 47 locale;
/// `None` otherwise (an unsupported value).
fn parse_lang(lang: &str) -> Option<Lang> {
    let lexical = !lang.is_empty()
        && lang.split('-').enumerate().all(|(index, part)| {
            (1..=8).contains(&part.len())
                && part.bytes().all(|b| {
                    if index == 0 {
                        b.is_ascii_alphabetic()
                    } else {
                        b.is_ascii_alphanumeric()
                    }
                })
        });
    if !lexical {
        return None;
    }
    let parsed = Locale::try_from_bytes(lang.as_bytes()).ok()?;
    let extensions = &parsed.extensions;
    let mut exact = extensions.unicode.attributes.is_empty()
        && extensions.transform.is_empty()
        && extensions.private.is_empty()
        && extensions.other.is_empty();
    let keywords = &extensions.unicode.keywords;
    let mut keys: Vec<Key> = Vec::new();
    keywords.clone().retain_by_key(|k| {
        keys.push(*k);
        false
    });
    let mut settings = Settings::default();
    let mut collation_type: Option<String> = None;
    for k in keys {
        let value = keywords
            .get(&k)
            .map(ToString::to_string)
            .unwrap_or_default();
        // "true" is spelled as no value at all.
        let boolean = match value.as_str() {
            "" | "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        let honoured = match k.as_str() {
            "co" => {
                collation_type = Some(value);
                true
            }
            "kf" => set(
                &mut settings.case_first,
                match value.as_str() {
                    "upper" => Some(CaseFirstSetting::Upper),
                    "lower" => Some(CaseFirstSetting::Lower),
                    "false" => Some(CaseFirstSetting::Off),
                    _ => None,
                },
            ),
            "kn" => set(&mut settings.numeric, boolean),
            "kb" => set(&mut settings.backwards, boolean),
            "kc" => set(&mut settings.case_level, boolean),
            "kk" => boolean.is_some(),
            "ks" => set(
                &mut settings.strength,
                match value.as_str() {
                    "level1" => Some(Level::Primary),
                    "level2" => Some(Level::Secondary),
                    "level3" => Some(Level::Tertiary),
                    "level4" => Some(Level::Quaternary),
                    "identic" => Some(Level::Identical),
                    _ => None,
                },
            ),
            "ka" => set(
                &mut settings.shifted,
                match value.as_str() {
                    "noignore" => Some(false),
                    "shifted" => Some(true),
                    _ => None,
                },
            ),
            "kv" => set(&mut settings.max_variable, max_variable(&value)),
            // Collation keywords ICU4X 1.5 cannot set: reordering, the
            // deprecated hiragana quaternary and variable top.
            "kr" | "kh" | "vt" => false,
            // Not a collation keyword: it does not change a collation.
            _ => true,
        };
        exact &= honoured;
    }
    let mut locale = DataLocale::from(&parsed.id);
    if let Some(collation_type) = collation_type {
        // `standard` is the default type; any other must be one the data has
        // for this language (ICU4X falls back to the standard collation
        // silently otherwise).
        if collation_type != "standard" {
            let mut typed = locale.clone();
            // `-u-co` without a type is not a collation type.
            let value = (!collation_type.is_empty())
                .then(|| collation_type.parse().ok())
                .flatten();
            match value {
                Some(value) => {
                    typed.set_unicode_ext(key!("co"), value);
                    if has_collation_type(&typed) {
                        locale = typed;
                    } else {
                        exact = false;
                    }
                }
                None => exact = false,
            }
        }
    }
    Some(Lang {
        locale: locale.to_string(),
        settings,
        exact,
    })
}

/// Whether the collation data has the `-u-co-` type of `locale` for its
/// language (or a parent ICU4X falls back to): the tailoring ICU4X loads for
/// it still carries the type.
fn has_collation_type(locale: &DataLocale) -> bool {
    let requested = locale.get_unicode_ext(&key!("co"));
    let response = DataProvider::<CollationDataV1Marker>::load(
        &Baked,
        DataRequest {
            locale,
            metadata: Default::default(),
        },
    );
    match response {
        Ok(response) => match response.metadata.locale {
            Some(resolved) => resolved.get_unicode_ext(&key!("co")) == requested,
            // An exact hit.
            None => true,
        },
        Err(_) => false,
    }
}

/// The locale whose data ICU4X uses for `locale` and marker `M` after its
/// locale fallback, or `None` when there is none.
fn resolved_locale<M: KeyedDataMarker>(locale: &DataLocale) -> Option<String>
where
    Baked: DataProvider<M>,
{
    let response = DataProvider::<M>::load(
        &Baked,
        DataRequest {
            locale,
            metadata: Default::default(),
        },
    )
    .ok()?;
    Some(match response.metadata.locale {
        Some(resolved) => resolved.to_string(),
        None => locale.to_string(),
    })
}

/// A UCA collation: an ICU4X 1.5 collator for one [`Tailoring`].
///
/// It has no sort key (module docs); the substring functions are answered by
/// comparing substrings (module docs, "Substring matching"), with the data
/// the search needs loaded on first use.
pub(crate) struct IcuCollation {
    collator: Collator,
    tailoring: Tailoring,
    /// What the substring search reads from the collation data (`search.rs`),
    /// loaded on the first substring function; `None` inside when it cannot
    /// be loaded, and the search then runs without it.
    search: OnceCell<Option<search::SearchData>>,
}

impl IcuCollation {
    /// The collator for `tailoring`; `None` when ICU4X cannot build one.
    fn new(tailoring: Tailoring) -> Option<Self> {
        let collator = Collator::try_new(&tailoring.locale(), tailoring.options()).ok()?;
        Some(Self {
            collator,
            tailoring,
            search: OnceCell::new(),
        })
    }

    /// What decides this collator's behaviour: the locales ICU4X actually
    /// loaded each kind of collation data for (after its locale fallback) and
    /// the options it resolved (the request merged with the locale's
    /// defaults). Two collators with the same key compare every pair of
    /// strings alike.
    fn canonical_key(&self) -> CanonicalKey {
        let locale = self.tailoring.locale();
        (
            [
                resolved_locale::<CollationMetadataV1Marker>(&locale),
                resolved_locale::<CollationDataV1Marker>(&locale),
                resolved_locale::<CollationReorderingV1Marker>(&locale),
                resolved_locale::<CollationDiacriticsV1Marker>(&locale),
            ],
            format!("{:?}", self.collator.resolved_options()),
        )
    }

    /// Whether `s` "contains only ignorable collation units" (F&O 2.0 §7.5):
    /// it compares equal to the zero-length string.
    fn ignorable(&self, s: &str) -> bool {
        s.is_empty() || self.compare(s, "") == Ordering::Equal
    }

    /// The collation data for the substring search, loaded once: the same
    /// locale, and the same options at primary strength without a case level
    /// for the primary filter (`search.rs`).
    fn search_data(&self) -> Option<&search::SearchData> {
        self.search
            .get_or_init(|| {
                let mut primary = self.tailoring.options();
                primary.strength = Some(Strength::Primary);
                primary.case_level = Some(CaseLevel::Off);
                search::SearchData::new(
                    &self.tailoring.locale(),
                    primary,
                    self.collator.resolved_options(),
                )
            })
            .as_ref()
    }

    /// A search with `budget` (`search::BUDGET` for `find`).
    fn searcher(&self, budget: usize) -> search::Search<'_> {
        search::Search::new(&self.collator, self.search_data(), budget)
    }
}

impl Collation for IcuCollation {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        // ICU4X 1.5 does not skip an identical prefix ("TODO(#2010)"); equal
        // strings are common enough in `=` and grouping to test first.
        if a == b {
            return Ordering::Equal;
        }
        self.collator.compare(a, b)
    }

    /// The leftmost minimal match (module docs, "Substring matching"), found
    /// as `search.rs` describes; `None` (FOCH0004) only when the search
    /// exceeds its work budget.
    fn find(&self, haystack: &str, needle: &str) -> Option<Option<(usize, usize)>> {
        // "If the value of $arg2 is the zero-length string, then the function
        // returns true. If the value of $arg1 is the zero-length string, the
        // function returns false." — `substring-before` gives "" and
        // `substring-after` all of `$arg1` for a match at (0, 0).
        if self.ignorable(needle) {
            return Some(Some((0, 0)));
        }
        if self.ignorable(haystack) {
            return Some(None);
        }
        self.searcher(search::BUDGET).find(haystack, needle).ok()
    }

    /// F&O 2.0 §7.5.2: "starts with a sequence of collation units that
    /// provides a match to the collation units of $arg2" — some prefix of
    /// `haystack` equal to `needle`. At most *n* comparisons, so no budget.
    fn starts_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        if self.ignorable(needle) || haystack.starts_with(needle) {
            return Some(true);
        }
        self.searcher(usize::MAX).starts_with(haystack, needle).ok()
    }

    /// F&O 2.0 §7.5.3, the mirror of [`starts_with`](Self::starts_with): some
    /// suffix of `haystack` equal to `needle`.
    fn ends_with(&self, haystack: &str, needle: &str) -> Option<bool> {
        if self.ignorable(needle) || haystack.ends_with(needle) {
            return Some(true);
        }
        self.searcher(usize::MAX).ends_with(haystack, needle).ok()
    }
}

impl fmt::Debug for IcuCollation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IcuCollation")
            .field("tailoring", &self.tailoring)
            .field("resolved", &self.collator.resolved_options())
            .finish()
    }
}

/// [`IcuCollation::canonical_key`]: the resolved locales of the collation
/// metadata, tailoring, reordering and diacritics data, and the resolved
/// options.
type CanonicalKey = ([Option<String>; 4], String);

/// A cache entry: the collator, or `None` for a tailoring ICU4X could not
/// build (remembered too, so a refused URI is not retried on every use).
type Built = Option<Rc<dyn Collation>>;

/// How many requested tailorings [`UcaCollations`] remembers before it starts
/// over (the collators themselves are kept: see there).
const REQUESTS_KEPT: usize = 1024;

/// The UCA collations of one registry, built on first use and kept: one
/// collator per **canonical** tailoring — the locales ICU4X resolves the
/// request to and the options it resolves ([`IcuCollation::canonical_key`]) —
/// so `lang=en-US`, `lang=en-GB` and no `lang` (all the root collation), or
/// `lang=da` with and without `caseFirst=upper` (Danish's default), share one
/// collator, and `fallback` plays no part.
///
/// The [`CollationResolver`](super::CollationResolver) contract asks for "the
/// same URI with the same collation throughout one evaluation run"; the cache
/// gives that for the life of the registry, and it keeps `xsl:sort`,
/// `xsl:key` and `xsl:for-each-group` — which resolve their collation each
/// time they are evaluated — from building a collator every time. A request
/// is first looked up as parsed (`requests`, which forgets everything after
/// [`REQUESTS_KEPT`] distinct requests — a `lang` taken from data can have any
/// number of spellings); only a new request builds a collator, to find its
/// canonical key. The canonical collators are never dropped, so a request
/// forgotten and asked again gets the same collator back; their number is
/// bounded by the locales of the data times the option combinations.
#[derive(Default)]
pub(crate) struct UcaCollations {
    requests: RefCell<HashMap<Tailoring, Built>>,
    canonical: RefCell<HashMap<CanonicalKey, Rc<dyn Collation>>>,
}

impl UcaCollations {
    /// The collation a URI of the UCA family names, or `None` when this
    /// processor does not provide it (the caller's "unrecognized collation"
    /// error).
    pub(crate) fn for_uri(&self, uri: &str) -> Option<Rc<dyn Collation>> {
        match parse_uca_uri(uri)? {
            UcaUri::Unsupported => None,
            UcaUri::Accepted {
                tailoring,
                fallback,
            } => self.get(tailoring, fallback),
        }
    }

    /// The collation of `xsl:sort lang=… case-order=…` without a `collation`
    /// attribute (§13.1.3): the UCA tailored for `lang`, with `caseFirst` from
    /// `case-order` (which wins over a `-u-kf-` in `lang`, as a URI parameter
    /// does). A `lang` ICU4X cannot parse chooses the root collation — the
    /// algorithm is implementation-defined, and `lang` was already checked
    /// against `xml:lang`.
    ///
    /// **`case-order` keeps a case level.** "The `case-order` attribute
    /// indicates whether the desired collation should sort upper-case letters
    /// before lower-case or vice versa" (§13.1.3), but `caseFirst` alone only
    /// reorders a difference the collation still sees: below tertiary
    /// strength — `lang="en-u-ks-level1"` or `-ks-level2` — case is ignored,
    /// `a` and `A` compare equal, and a stable sort keeps them in input order
    /// whatever `case-order` says. So when `case-order` is given and the
    /// strength is primary or secondary, `caseLevel` is switched on: UTS #35
    /// "a level consisting only of case characteristics will be inserted in
    /// front of tertiary level" — "To ignore accents but take case into
    /// account, set strength to primary and case level to on". That adds case
    /// and nothing else, so the language's own choice to ignore accents
    /// (level 1) or tertiary variants such as width (level 2) still holds;
    /// raising the strength to tertiary would bring those back. An explicit
    /// `case-order` wins over a `-u-kc-false` in `lang` as it wins over
    /// `-u-kf-`. At tertiary strength and above case is already a difference,
    /// and nothing changes. (ICU4X 1.5 locale data never changes the default
    /// strength, tertiary — `CollationMetadataV1` has no strength bit — so a
    /// strength the request leaves open is tertiary.) With a `collation`
    /// attribute none of this applies: "The `lang` and `case-order`
    /// attributes are ignored if a `collation` attribute is present."
    pub(crate) fn for_sort(
        &self,
        lang: Option<&str>,
        case_order: Option<CaseOrder>,
    ) -> Rc<dyn Collation> {
        let lang = lang.and_then(|lang| parse_lang(crate::whitespace::trim(lang)));
        let (locale, from_lang) = match lang {
            Some(Lang {
                locale, settings, ..
            }) => (Some(locale), settings),
            None => (None, Settings::default()),
        };
        let explicit = Settings {
            case_first: case_order.map(CaseFirstSetting::from),
            ..Settings::default()
        };
        let mut settings = explicit.or(from_lang);
        if case_order.is_some() && settings.strength.unwrap_or(Level::Tertiary) < Level::Tertiary {
            settings.case_level = Some(true);
        }
        let tailoring = Tailoring { locale, settings };
        self.get(tailoring, true).unwrap_or_else(|| {
            // Unreachable with compiled data (the root collator always
            // builds), but `xsl:sort` must get a collation.
            Rc::new(CaseOrderCollation {
                case_order: case_order.unwrap_or(CaseOrder::LowerFirst),
            })
        })
    }

    fn get(&self, tailoring: Tailoring, fallback: bool) -> Built {
        match self.build(&tailoring) {
            Some(collation) => Some(collation),
            // "If the fallback parameter is omitted or takes the value yes …
            // the implementation must accept the collation URI, and should use
            // the available collation that most closely reflects the user's
            // intentions": the same options on the root collation.
            None if fallback && tailoring.locale.is_some() => self.build(&Tailoring {
                locale: None,
                ..tailoring
            }),
            None => None,
        }
    }

    /// The collator of `tailoring`, from the cache or built.
    fn build(&self, tailoring: &Tailoring) -> Built {
        if let Some(built) = self.requests.borrow().get(tailoring) {
            return built.clone();
        }
        let built = IcuCollation::new(tailoring.clone()).map(|collation| {
            let key = collation.canonical_key();
            let mut canonical = self.canonical.borrow_mut();
            let shared = canonical
                .entry(key)
                .or_insert_with(|| Rc::new(collation) as Rc<dyn Collation>);
            Rc::clone(shared)
        });
        let mut requests = self.requests.borrow_mut();
        if requests.len() >= REQUESTS_KEPT {
            requests.clear();
        }
        requests.insert(tailoring.clone(), built.clone());
        built
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(uri: &str) -> Tailoring {
        match parse_uca_uri(uri) {
            Some(UcaUri::Accepted { tailoring, .. }) => tailoring,
            other => panic!("{uri}: {other:?}"),
        }
    }

    #[test]
    fn only_the_family_scheme_and_path_followed_by_a_query_is_uca() {
        assert!(parse_uca_uri("http://www.w3.org/2013/collation/UCA").is_some());
        assert!(parse_uca_uri("http://www.w3.org/2013/collation/UCA?").is_some());
        assert!(parse_uca_uri("http://www.w3.org/2013/collation/UCAX").is_none());
        assert!(parse_uca_uri("http://www.w3.org/2013/collation/UCA/x").is_none());
        assert!(parse_uca_uri("http://www.w3.org/2013/collation/uca").is_none());
        assert!(parse_uca_uri("urn:x").is_none());
    }

    #[test]
    fn parameters_are_parsed_and_the_last_one_wins() {
        let t = accepted(
            "http://www.w3.org/2013/collation/UCA?strength=1;lang=DE-de;strength=secondary;\
             caseFirst=upper;numeric=yes;alternate=shifted;maxVariable=space;backwards=yes;\
             caseLevel=no;normalization=yes;",
        );
        let s = &t.settings;
        assert_eq!(s.strength, Some(Level::Secondary));
        assert_eq!(t.locale.as_deref(), Some("de-DE"));
        assert_eq!(s.case_first, Some(CaseFirstSetting::Upper));
        assert_eq!(s.numeric, Some(true));
        assert_eq!(s.shifted, Some(true));
        assert_eq!(s.max_variable, Some(Variable::Space));
        assert_eq!(s.backwards, Some(true));
        assert_eq!(s.case_level, Some(false));
    }

    /// F5: `&` separates parameters too, mixed with `;` or alone.
    #[test]
    fn an_ampersand_separates_parameters() {
        for uri in [
            "http://www.w3.org/2013/collation/UCA?lang=de&strength=primary&fallback=no",
            "http://www.w3.org/2013/collation/UCA?lang=de;strength=primary&fallback=no",
        ] {
            match parse_uca_uri(uri) {
                Some(UcaUri::Accepted {
                    tailoring,
                    fallback,
                }) => {
                    assert_eq!(tailoring.locale.as_deref(), Some("de"), "{uri}");
                    assert_eq!(tailoring.settings.strength, Some(Level::Primary), "{uri}");
                    assert!(!fallback, "{uri}");
                }
                other => panic!("{uri}: {other:?}"),
            }
        }
        assert_eq!(
            parse_uca_uri("http://www.w3.org/2013/collation/UCA?fallback=no&version=99"),
            Some(UcaUri::Unsupported)
        );
    }

    #[test]
    fn unsupported_parameters_are_ignored_unless_fallback_is_no() {
        for parameter in [
            "version=15.2",
            "version=x",
            "reorder=Grek,Latn",
            "alternate=blanked;strength=identical",
            "strength=strong",
            "caseFirst=off",
            "lang=x-klingon",
            "lang=de-u-co-bogus",
            "lang=de-u-co",
            "lang=en-u-kf-sideways",
            "lang=en-u-kr-grek",
            "lang=en-x-private",
            "lang=",
            "frobnicate=yes",
        ] {
            let strict = [
                format!("http://www.w3.org/2013/collation/UCA?fallback=no;{parameter}"),
                // `fallback` decides wherever it appears.
                format!("http://www.w3.org/2013/collation/UCA?{parameter};fallback=no"),
            ];
            for strict in strict {
                assert_eq!(
                    parse_uca_uri(&strict),
                    Some(UcaUri::Unsupported),
                    "{strict}"
                );
            }
            let lenient = format!("http://www.w3.org/2013/collation/UCA?{parameter}");
            assert!(
                matches!(parse_uca_uri(&lenient), Some(UcaUri::Accepted { .. })),
                "{lenient}"
            );
        }
        // Ignored entirely when nothing of it can be honoured.
        for parameter in [
            "version=15.2",
            "reorder=Grek",
            "lang=x-klingon",
            "frobnicate=1",
        ] {
            let lenient = format!("http://www.w3.org/2013/collation/UCA?{parameter}");
            assert_eq!(accepted(&lenient), Tailoring::default(), "{lenient}");
        }
        // The last `fallback` wins too.
        assert!(matches!(
            parse_uca_uri(
                "http://www.w3.org/2013/collation/UCA?fallback=no;version=99;fallback=yes"
            ),
            Some(UcaUri::Accepted { fallback: true, .. })
        ));
    }

    /// F6: "The version number of the UCA to be used" — up to the data's.
    #[test]
    fn versions_up_to_the_data_s_are_supported() {
        for version in ["15.1.0", "15.1", "15", "6.2.0", "1", "15.0.99", "15.1.0.0"] {
            assert!(version_supported(version), "{version}");
        }
        for version in [
            "15.1.1",
            "15.2",
            "16",
            "16.0.0",
            "",
            "15.",
            ".1",
            "a",
            "15.1-beta",
        ] {
            assert!(!version_supported(version), "{version}");
        }
    }

    /// F3: UTS #10 blanked = shifted at tertiary strength at most.
    #[test]
    fn blanked_is_shifted_at_tertiary_strength() {
        for (query, strength) in [
            ("alternate=blanked", Level::Tertiary),
            ("alternate=blanked;strength=quaternary", Level::Tertiary),
            ("alternate=blanked;strength=secondary", Level::Secondary),
            ("strength=1;alternate=blanked;fallback=no", Level::Primary),
        ] {
            let t = accepted(&format!("http://www.w3.org/2013/collation/UCA?{query}"));
            assert_eq!(t.settings.shifted, Some(true), "{query}");
            assert_eq!(t.settings.strength, Some(strength), "{query}");
        }
        // Identical strength is approximated only with `fallback=yes`.
        let t = accepted("http://www.w3.org/2013/collation/UCA?alternate=blanked;strength=5");
        assert_eq!(t.settings.strength, Some(Level::Tertiary));
    }

    /// F7: the collation keywords of `lang`; an explicit parameter wins.
    #[test]
    fn lang_extensions_are_read_and_parameters_win() {
        let t = accepted(
            "http://www.w3.org/2013/collation/UCA?lang=en-u-kf-upper-kn-ks-level2-ka-shifted\
             -kb-kc-false-kv-space-kk-nu-arab;fallback=no",
        );
        assert_eq!(t.locale.as_deref(), Some("en"));
        let s = &t.settings;
        assert_eq!(s.case_first, Some(CaseFirstSetting::Upper));
        assert_eq!(s.numeric, Some(true));
        assert_eq!(s.strength, Some(Level::Secondary));
        assert_eq!(s.shifted, Some(true));
        assert_eq!(s.backwards, Some(true));
        assert_eq!(s.case_level, Some(false));
        assert_eq!(s.max_variable, Some(Variable::Space));
        let t = accepted(
            "http://www.w3.org/2013/collation/UCA?caseFirst=lower;lang=en-u-kf-upper-kn-false;numeric=yes",
        );
        assert_eq!(t.settings.case_first, Some(CaseFirstSetting::Lower));
        assert_eq!(t.settings.numeric, Some(true));
        // A collation type the data has is kept (region fallback included),
        // `standard` is the default.
        let t =
            accepted("http://www.w3.org/2013/collation/UCA?lang=de-CH-u-co-phonebk;fallback=no");
        assert_eq!(t.locale.as_deref(), Some("de-CH-u-co-phonebk"));
        let t = accepted("http://www.w3.org/2013/collation/UCA?lang=de-u-co-standard;fallback=no");
        assert_eq!(t.locale.as_deref(), Some("de"));
        // One the data lacks is dropped (ignored with fallback=yes).
        let t = accepted("http://www.w3.org/2013/collation/UCA?lang=de-u-co-bogus");
        assert_eq!(t.locale.as_deref(), Some("de"));
        let t = accepted("http://www.w3.org/2013/collation/UCA?lang=de-u-co");
        assert_eq!(t.locale.as_deref(), Some("de"));
        let t = accepted("http://www.w3.org/2013/collation/UCA?lang=en-u-co-phonebk");
        assert_eq!(t.locale.as_deref(), Some("en"));
        assert!(matches!(
            parse_uca_uri("http://www.w3.org/2013/collation/UCA?lang=en-u-co-phonebk;fallback=no"),
            Some(UcaUri::Unsupported)
        ));
    }

    #[test]
    fn an_ill_formed_query_is_unsupported() {
        for uri in [
            "http://www.w3.org/2013/collation/UCA?strength",
            "http://www.w3.org/2013/collation/UCA?=primary",
            "http://www.w3.org/2013/collation/UCA?lang=en;secondary",
        ] {
            assert_eq!(parse_uca_uri(uri), Some(UcaUri::Unsupported), "{uri}");
        }
    }

    /// F9: one collator per canonical tailoring.
    #[test]
    fn equal_tailorings_share_one_collator() {
        let cache = UcaCollations::default();
        let get = |uri: &str| cache.for_uri(uri).unwrap();
        let a = get("http://www.w3.org/2013/collation/UCA?lang=en;strength=primary");
        for same in [
            "http://www.w3.org/2013/collation/UCA?strength=1;lang=EN;version=9",
            "http://www.w3.org/2013/collation/UCA?strength=1;lang=en-US",
            "http://www.w3.org/2013/collation/UCA?strength=1;lang=en-GB;fallback=no",
            "http://www.w3.org/2013/collation/UCA?strength=1",
            "http://www.w3.org/2013/collation/UCA?strength=1;lang=en-u-co-phonebk",
        ] {
            assert!(Rc::ptr_eq(&a, &get(same)), "{same}");
        }
        // Danish's own default case order, asked for or not.
        let da = get("http://www.w3.org/2013/collation/UCA?lang=da");
        assert!(Rc::ptr_eq(
            &da,
            &get("http://www.w3.org/2013/collation/UCA?lang=da-DK;caseFirst=upper")
        ));
        assert!(!Rc::ptr_eq(&a, &da));
        assert!(!Rc::ptr_eq(
            &da,
            &get("http://www.w3.org/2013/collation/UCA?lang=da;caseFirst=lower")
        ));
        assert_eq!(cache.canonical.borrow().len(), 3);
    }

    /// F6's evidence: the data is UCA 15.1. U+2EBF0 (CJK Unified Ideographs
    /// Extension I, new in Unicode 15.1) has the implicit weight of an
    /// assigned unified ideograph (base FB80, before U+30000 of Extension G),
    /// not that of an unassigned code point (base FBC0); U+105C0 (Todhri, new
    /// in Unicode 16.0) is still unassigned, so it sorts after every Han
    /// ideograph instead of among the scripts.
    #[test]
    fn the_data_is_uca_15_1() {
        let root = IcuCollation::new(Tailoring::default()).unwrap();
        assert_eq!(root.compare("\u{2EBF0}", "\u{30000}"), Ordering::Less);
        assert_eq!(root.compare("\u{105C0}", "\u{4E00}"), Ordering::Greater);
        assert_eq!(root.compare("\u{105C0}", "\u{30000}"), Ordering::Greater);
        assert_eq!(UCA_VERSION, [15, 1, 0]);
    }

    // ─── R2 (second review): the substring search ─────────────────────────

    /// A deterministic generator (xorshift64*), so a failure reproduces.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % n
        }

        fn string(&mut self, alphabet: &[&str], max: usize) -> String {
            let len = self.below(max + 1);
            (0..len)
                .map(|_| alphabet[self.below(alphabet.len())])
                .collect()
        }
    }

    fn collation(uri: &str) -> IcuCollation {
        match parse_uca_uri(uri) {
            Some(UcaUri::Accepted { tailoring, .. }) => IcuCollation::new(tailoring).unwrap(),
            other => panic!("{uri}: {other:?}"),
        }
    }

    /// The definition, unpruned and without a budget: every candidate
    /// compared (the search before the second review).
    fn reference_find(c: &IcuCollation, h: &str, n: &str) -> Option<(usize, usize)> {
        if c.ignorable(n) {
            return Some((0, 0));
        }
        if c.ignorable(h) {
            return None;
        }
        let plain = search::Search::new(&c.collator, None, usize::MAX);
        search::Exhaustive::new(&plain, h, n, None)
            .leftmost_minimal()
            .unwrap()
    }

    fn reference_starts(c: &IcuCollation, h: &str, n: &str) -> bool {
        c.ignorable(n) || search::boundaries(h).iter().any(|&e| c.equals(&h[..e], n))
    }

    fn reference_ends(c: &IcuCollation, h: &str, n: &str) -> bool {
        c.ignorable(n) || search::boundaries(h).iter().any(|&s| c.equals(&h[s..], n))
    }

    /// Characters that exercise every branch of the classification: plain
    /// letters and their case and accent variants, precomposed and decomposed,
    /// table diacritics and other marks, contractions with combining and with
    /// starter suffixes (Slovak `ch`, Danish `aa`, Swedish `å`, Thai prevowels,
    /// Cyrillic `и` + breve), expansions (`ß`, `æ`, `ﬁ`, `½`), Hangul syllables
    /// and jamo, digits, prefix-context characters (`·` after `l`, Japanese
    /// `ー` and `ゝ` after kana), variable punctuation and spaces, completely
    /// ignorable characters (soft hyphen, CGJ), U+FFFF itself, and
    /// non-starters with a primary weight (U+0363 COMBINING LATIN SMALL
    /// LETTER A, U+094D DEVANAGARI SIGN VIRAMA), which canonical ordering
    /// can move.
    const ALPHABET: &[&str] = &[
        "a",
        "a",
        "a",
        "A",
        "b",
        "c",
        "h",
        "ch",
        "e",
        "é",
        "e\u{301}",
        "\u{301}",
        "\u{323}",
        "\u{302}",
        "ệ",
        "å",
        "a\u{30A}",
        "a\u{323}\u{30A}",
        "aa",
        "ä",
        "o",
        "ö",
        "s",
        "ß",
        "ss",
        "æ",
        "ae",
        "ﬁ",
        "fi",
        "½",
        "1",
        "2",
        "12",
        "-",
        " ",
        "*",
        "\u{AD}",
        "\u{34F}",
        "l",
        "l·",
        "·",
        "и",
        "\u{306}",
        "й",
        "เ",
        "ก",
        "\u{E48}",
        "\u{5B0}",
        "가",
        "\u{1100}",
        "\u{1161}",
        "カ",
        "ー",
        "ア",
        "か",
        "ゝ",
        "\u{FFFF}",
        "ı",
        "i\u{307}",
        "z",
        "Z",
        "\u{363}",
        "\u{94D}",
        "क",
        "क\u{94D}",
    ];

    /// Hand-picked cases for what random strings rarely build: a
    /// discontiguous contraction (Swedish `a` + U+0323 + U+030A is `å` with a
    /// dot below), non-starters with primary weights in either order, a
    /// Hangul syllable with a mark, prefix contexts.
    const CASES: &[(&str, &str, &str)] = &[
        ("?lang=sv;strength=primary", "xa\u{323}\u{30A}y", "å"),
        ("?lang=sv;strength=primary", "xa\u{323}\u{30A}y", "a"),
        ("?lang=sv;strength=primary", "xa\u{323}\u{30A}y", "xa"),
        ("?lang=sv", "a\u{323}\u{30A}a\u{30A}\u{323}", "ạ\u{30A}"),
        ("", "क\u{363}\u{94D}x", "क\u{94D}\u{363}"),
        ("", "xक\u{94D}\u{363}", "क\u{363}\u{94D}"),
        ("?strength=primary", "क\u{363}\u{94D}", "\u{94D}\u{363}"),
        ("", "가\u{302}나", "가"),
        ("?lang=ko", "가\u{302}나", "나"),
        ("?lang=ja", "xカーy", "カア"),
        ("?lang=ja;strength=primary", "xカーyアア", "カア"),
        ("?lang=ja;strength=primary", "カーカー", "アカ"),
        ("", "Ll·l·L", "l·"),
        ("?lang=ca", "Ll·l·L", "l·l"),
    ];

    const CONFIGURATIONS: &[&str] = &[
        "",
        "?strength=primary",
        "?strength=secondary",
        "?strength=quaternary;alternate=shifted",
        "?strength=identical",
        "?alternate=shifted",
        "?alternate=shifted;strength=primary",
        "?caseLevel=yes;strength=primary",
        "?numeric=yes",
        "?lang=sk",
        "?lang=da;strength=secondary",
        "?lang=sv;strength=primary",
        "?lang=de-u-co-phonebk;strength=primary",
        "?lang=es-u-co-trad",
        "?lang=ja",
        "?lang=th",
        "?lang=vi;strength=secondary",
        "?lang=lt",
        "?lang=ko",
        "?lang=ca",
        "?lang=fr-CA;backwards=yes",
    ];

    /// Every path of the search (the primary filter, the pruned search, the
    /// unpruned one) gives the definition's answer, for `find`,
    /// `starts_with` and `ends_with`, over random strings in every
    /// configuration. Runs in debug builds in a few seconds.
    #[test]
    fn the_search_agrees_with_the_definition() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut filtered = 0;
        let mut counted = 0;
        let mut pruned = 0;
        for query in CONFIGURATIONS {
            let uri = format!("http://www.w3.org/2013/collation/UCA{query}");
            let c = collation(&uri);
            let data = c.search_data().expect("data");
            for case in 0..1_500 {
                // Short random strings, or a needle cut from the haystack and
                // varied, so that matches are common.
                let h = rng.string(ALPHABET, if case % 3 == 0 { 24 } else { 9 });
                let n = if case % 2 == 0 && !h.is_empty() {
                    let b = search::boundaries(&h);
                    let i = rng.below(b.len() - 1);
                    let j = i + 1 + rng.below(b.len() - 1 - i);
                    let mut n = h[b[i]..b[j]].to_string();
                    if rng.below(3) == 0 {
                        n = n.to_uppercase();
                    }
                    n
                } else {
                    rng.string(ALPHABET, 4)
                };
                let hu = data.classify(&h);
                let nu = data.classify(&n);
                let all = || hu.iter().chain(&nu);
                if all().all(|u| matches!(u.primaries, Some(0 | 1))) {
                    filtered += 1;
                } else if all().all(|u| u.primaries.is_some()) && !query.contains("shifted") {
                    counted += 1;
                } else {
                    pruned += 1;
                }
                assert_eq!(
                    c.find(&h, &n),
                    Some(reference_find(&c, &h, &n)),
                    "find({h:?}, {n:?}) under {query}"
                );
                assert_eq!(
                    c.starts_with(&h, &n),
                    Some(reference_starts(&c, &h, &n)),
                    "starts_with({h:?}, {n:?}) under {query}"
                );
                assert_eq!(
                    c.ends_with(&h, &n),
                    Some(reference_ends(&c, &h, &n)),
                    "ends_with({h:?}, {n:?}) under {query}"
                );
            }
        }
        // Every path was exercised.
        eprintln!("{filtered} filtered, {counted} counted, {pruned} pruned");
        assert!(
            filtered > 3_000 && counted > 1_000 && pruned > 3_000,
            "{filtered} filtered, {counted} counted, {pruned} pruned"
        );
        for &(query, h, n) in CASES {
            let c = collation(&format!("http://www.w3.org/2013/collation/UCA{query}"));
            // Every substring of the haystack as a needle too.
            let b = search::boundaries(h);
            let needles = std::iter::once(n.to_string()).chain(
                (0..b.len())
                    .flat_map(|i| (i + 1..b.len()).map(move |j| (i, j)))
                    .map(|(i, j)| h[b[i]..b[j]].to_string()),
            );
            for n in needles {
                let n = n.as_str();
                assert_eq!(
                    c.find(h, n),
                    Some(reference_find(&c, h, n)),
                    "find({h:?}, {n:?}) under {query}"
                );
                assert_eq!(
                    c.starts_with(h, n),
                    Some(reference_starts(&c, h, n)),
                    "starts_with({h:?}, {n:?}) under {query}"
                );
                assert_eq!(
                    c.ends_with(h, n),
                    Some(reference_ends(&c, h, n)),
                    "ends_with({h:?}, {n:?}) under {query}"
                );
            }
        }
    }

    /// The budget decides only between an answer and FOCH0004: a search that
    /// runs out of it reports so (`Err`, which `find` turns into `None`),
    /// and one that does not gives the definition's answer — whatever the
    /// budget. Danish `a` chains into the contraction `aa`, so a long run of
    /// it has no safe boundary and is searched candidate by candidate.
    #[test]
    fn a_search_over_its_budget_raises_instead_of_answering() {
        let c = collation("http://www.w3.org/2013/collation/UCA?lang=da");
        let data = c.search_data();
        let haystack = "a".repeat(300);
        let needle = "a".repeat(150) + "b";
        let small = search::Search::new(&c.collator, data, 100_000);
        assert!(small.find(&haystack, &needle).is_err());
        let large = search::Search::new(&c.collator, data, usize::MAX);
        assert_eq!(large.find(&haystack, &needle).unwrap(), None);
        // Random strings and budgets: never a wrong answer.
        let mut rng = Rng(7);
        for query in [
            "?lang=da",
            "?lang=sk",
            "?strength=primary",
            "?lang=ja",
            "?numeric=yes",
        ] {
            let c = collation(&format!("http://www.w3.org/2013/collation/UCA{query}"));
            for _ in 0..300 {
                let h = rng.string(ALPHABET, 16);
                let n = rng.string(ALPHABET, 4);
                if c.ignorable(&h) || c.ignorable(&n) {
                    continue;
                }
                let budget = rng.below(400);
                let search = search::Search::new(&c.collator, c.search_data(), budget);
                if let Ok(found) = search.find(&h, &n) {
                    assert_eq!(found, reference_find(&c, &h, &n), "{h:?} {n:?} {query}");
                }
            }
        }
    }

    /// The pruning's premise, on the data: U+FFFF has its documented CE32
    /// (`SearchData::sentinel`), and every other character of the BMP sorts
    /// before it at primary strength — in the root collation and in
    /// tailorings that reorder scripts or tailor many characters. (A wider
    /// check — 78 tailorings, all of Unicode, shifted and numeric variants —
    /// was run once, outside the test suite.)
    #[test]
    fn u_ffff_has_the_greatest_primary_weight() {
        for query in [
            "",
            "?lang=sv",
            "?lang=ja",
            "?lang=zh",
            "?lang=el",
            "?lang=ru;alternate=shifted",
        ] {
            let c = collation(&format!("http://www.w3.org/2013/collation/UCA{query}"));
            let data = c.search_data().unwrap();
            assert!(data.sentinel_usable(), "{query}");
            let mut buffer = [0u8; 4];
            for c2 in ('\u{0}'..='\u{FFFE}').filter(|&ch| ch != '\u{FFFF}') {
                assert_eq!(
                    data.primary_compare(c2.encode_utf8(&mut buffer), "\u{FFFF}"),
                    Ordering::Less,
                    "U+{:04X} under {query}",
                    u32::from(c2)
                );
            }
        }
    }

    /// What the data says about characters, on examples of each rule.
    #[test]
    fn the_classification_reads_the_data() {
        let check = |query: &str, s: &str, expected: &[Option<u32>]| {
            let c = collation(&format!("http://www.w3.org/2013/collation/UCA{query}"));
            let got: Vec<Option<u32>> = c
                .search_data()
                .unwrap()
                .classify(s)
                .iter()
                .map(|u| u.primaries)
                .collect();
            assert_eq!(got, expected, "{s:?} under {query}");
        };
        // Plain letters; a precomposed letter (its diacritic from the
        // table); a decomposed one; an expansion; a Hangul syllable.
        check("", "aé", &[Some(1), Some(1)]);
        check("", "e\u{301}", &[Some(1), Some(0)]);
        check("", "ßﬁ가", &[Some(2), Some(2), Some(2)]);
        // A standalone conjoining jamo: ICU4X's own table, not read here.
        check("", "\u{1100}", &[None]);
        // A non-starter with a primary weight can be reordered.
        check("", "a\u{363}", &[Some(1), None]);
        // Contractions: Slovak `ch` (a starter suffix), Danish `aa`, Swedish
        // `å` from `a` + ring (a combining suffix), discontiguously too.
        check("?lang=sk", "chcx", &[None, None, Some(1), Some(1)]);
        check("?lang=da", "aab", &[None, None, Some(1)]);
        check(
            "?lang=sv",
            "a\u{30A}åab",
            &[None, None, Some(1), Some(1), Some(1)],
        );
        check("?lang=sv", "a\u{323}\u{30A}x", &[None, None, None, Some(1)]);
        // Digits form numbers only under numeric=yes.
        check("", "12", &[Some(1), Some(1)]);
        check("?numeric=yes", "x12y", &[Some(1), None, None, Some(1)]);
        // A prefix context: `ー` after a kana in Japanese, and the kana it
        // reads; `ー` with nothing before it that it can read stays alone.
        check("?lang=ja", "xカーy", &[Some(1), None, None, Some(1)]);
        check("?lang=ja", "x-ー", &[Some(1), Some(1), None]);
    }
}
