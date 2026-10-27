//! Sorting (§13): the sort key specification of `xsl:apply-templates`,
//! `xsl:for-each` and `xsl:perform-sort` applied to an initial sequence, and
//! of `xsl:for-each-group` applied to its groups (§14.3).
//!
//! The steps follow §13.1.1–§13.1.3:
//!
//! 1. the attribute value templates of every `xsl:sort` are evaluated once, with
//!    the focus of the containing instruction (§13.1);
//! 2. every sort key value is computed with the focus §13.1.1 defines — the
//!    item, its position in the *initial* sequence and the size of that
//!    sequence — with the current template rule null (§6.7) and the current
//!    mode the default mode (§6.5);
//! 3. the values are atomized and converted as `data-type` asks (§13.1.2);
//! 4. each key column is checked for a pair of ordinary values that the XPath
//!    `lt` operator cannot compare — XTDE1030 — *before* anything is sorted,
//!    and its values are turned into [`ColumnKey`]s, on which the comparison
//!    is a total order;
//! 5. the items are sorted — always stably, which §13.1 permits whatever
//!    `stable` says — by comparing those keys component by component, strings
//!    under the collation §13.1.3 chooses.
//!
//! Step 4 is what keeps the standard library's sort sound: it may panic when
//! handed a comparator that is not a total order, and "these two values cannot
//! be compared" is not an answer a comparator can give.

use std::cmp::Ordering;
use std::rc::Rc;

use xsd_schema::types::TimezoneOffset;
use xsd_schema::types::{PrimitiveTypeCode, XmlTypeCode, XmlValue, XmlValueKind};
use xsd_schema::xpath::ast::BinaryOpKind;
use xsd_schema::xpath::cast::cast_to;
use xsd_schema::xpath::functions::{atomize_sequence, XPathValue};
use xsd_schema::xpath::operators::eval_binary_with_timezone;
use xsd_schema::xpath::XmlItem;

use super::context::XsltContext;
use super::{Frame, Transformation};
use crate::collation::{
    uca_not_implemented, Collation, CollationResolver, CODEPOINT_COLLATION_URI,
};
use crate::error::{codes, XsltError};
use crate::model::*;
use crate::receiver::Nav;
use crate::whitespace;

/// The collation one sort key component compares strings with.
enum KeyCollation {
    /// `str::cmp`: UTF-8 byte order is codepoint order.
    Codepoint,
    /// A collation from the registry, or the `lang` / `case-order` one.
    Other(Rc<dyn Collation>),
}

impl KeyCollation {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        match self {
            KeyCollation::Codepoint => a.cmp(b),
            KeyCollation::Other(collation) => collation.compare(a, b),
        }
    }
}

/// What the rows being sorted are, for the messages that name one.
#[derive(Clone, Copy)]
pub(super) enum SortRows {
    /// The items of an initial sequence (§13.1.1).
    Items,
    /// The groups of `xsl:for-each-group` (§14.3).
    Groups,
}

impl SortRows {
    fn noun(self) -> &'static str {
        match self {
            SortRows::Items => "item",
            SortRows::Groups => "group",
        }
    }
}

/// The effective values of one `xsl:sort` element's attributes.
struct KeyRules {
    order: SortOrder,
    data_type: SortDataType,
    collation: KeyCollation,
    /// §13.1.2: with backwards-compatible behaviour the first item of a
    /// multi-item key is the key; otherwise it is XTTE1020.
    first_item_only: bool,
}

impl<'a, 't> Transformation<'a, 't> {
    /// Sorts `items` by the sort key specification `keys` (§13.1.1); with no
    /// keys the sequence is returned as it is.
    pub(super) fn sort_sequence(
        &self,
        items: Vec<XmlItem<Nav<'t>>>,
        keys: &[SortKey],
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<XmlItem<Nav<'t>>>, XsltError> {
        if keys.is_empty() {
            return Ok(items);
        }
        // §13.1.1: "The context item is the item in the initial sequence whose
        // sort key value is being computed. The context position is the
        // position of that item in the initial sequence. The context size is
        // the size of the initial sequence."
        let size = items.len();
        let order = self.sort_order(
            keys,
            SortRows::Items,
            size,
            |index| cx.with_focus(items[index].clone(), index + 1, size),
            frame,
            cx,
        )?;
        let mut slots: Vec<Option<XmlItem<Nav<'t>>>> = items.into_iter().map(Some).collect();
        Ok(order
            .into_iter()
            .map(|index| slots[index].take().expect("each index occurs once"))
            .collect())
    }

    /// The order in which `size` rows sort by the sort key specification
    /// `keys` (non-empty): a permutation of `0..size`. `rows` says what the
    /// rows are, for the messages of the errors that name one.
    ///
    /// `focus(i)` is the dynamic context row `i`'s sort key values are
    /// computed in — its focus and, for the groups of `xsl:for-each-group`, its
    /// current group (§14.3); the current template rule and mode are then
    /// cleared for it (`sort_key_value`). `cx` is the context of the
    /// containing instruction, in which the attribute value templates of the
    /// `xsl:sort` elements are evaluated (§13.1).
    pub(super) fn sort_order(
        &self,
        keys: &[SortKey],
        rows: SortRows,
        size: usize,
        focus: impl Fn(usize) -> XsltContext<'t>,
        frame: &mut Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<Vec<usize>, XsltError> {
        // §13.1: "Those attributes of the xsl:sort elements whose values are
        // attribute value templates are evaluated using the same focus as is
        // used to evaluate the select attribute of the containing instruction".
        // They are evaluated whenever the instruction is, even for an empty
        // initial sequence: XTDE1035 is a property of the `collation`
        // attribute, not of any comparison (`sort-027` sorts nothing and
        // expects it).
        let rules: Vec<KeyRules> = keys
            .iter()
            .map(|key| {
                self.key_rules(key, frame, cx)
                    .map_err(|e| e.or_location(&key.loc))
            })
            .collect::<Result<_, _>>()?;
        if size == 0 {
            return Ok(Vec::new());
        }

        // A date or time without a timezone is compared with the transformation's implicit
        // timezone, as the expressions' `lt` compares it (F&O 1.0 §10.4).
        let timezone = self.implicit_timezone();
        // columns[k][i]: the k-th sort key of the i-th row.
        let mut columns: Vec<Vec<ColumnKey>> = Vec::with_capacity(keys.len());
        for (key, rule) in keys.iter().zip(&rules) {
            // `None` is the empty sequence (§13.1.2's "empty values").
            let mut values: Vec<Option<XmlValue>> = Vec::with_capacity(size);
            for index in 0..size {
                let value = self
                    .sort_key_value(key, rule, rows, focus(index), frame)
                    .map_err(|e| e.or_location(&key.loc))?;
                values.push(value);
            }
            let column = column_keys(values, timezone)
                .map_err(|(a, b)| incomparable(&a, &b).with_location(key.loc.clone()))?;
            columns.push(column);
        }

        let mut order: Vec<usize> = (0..size).collect();
        // `sort_by` is a stable sort: items whose keys all compare equal keep
        // their order in the initial sequence, which is what §13.1.1 requires
        // of a stable sort key specification — and this processor treats every
        // specification as stable (`stable="no"` only *permits* another order).
        // `compare_keys` is a total order on every column `column_keys`
        // accepted (given a collation whose `compare` is one, which
        // `XsltFunctions::register_collation` requires of a host), ascending
        // or reversed, so the lexicographic combination is one too.
        order.sort_by(|&a, &b| {
            for (column, rule) in columns.iter().zip(&rules) {
                let ordering = compare_keys(&column[a], &column[b], &rule.collation, timezone);
                let ordering = match rule.order {
                    SortOrder::Ascending => ordering,
                    SortOrder::Descending => ordering.reverse(),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            Ordering::Equal
        });
        Ok(order)
    }

    /// The effective values of one `xsl:sort` element's attributes (§13.1).
    fn key_rules(
        &self,
        key: &SortKey,
        frame: &Frame<'t>,
        cx: &XsltContext<'t>,
    ) -> Result<KeyRules, XsltError> {
        let order = match &key.order {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                SortOrder::parse(&value).ok_or_else(|| bad_value("order", &value))?
            }
            None => SortOrder::Ascending,
        };
        let data_type = match &key.data_type {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                let data_type =
                    SortDataType::parse(&value).ok_or_else(|| bad_value("data-type", &value))?;
                // §5.1 XTDE0290: the prefix of a computed QName must be bound
                // by the defining element, the xsl:sort (a fixed value was
                // checked statically, XTSE0280).
                if let Some(prefix) = SortDataType::qname_prefix(&value) {
                    let sc = self.sheet.static_contexts.get(key.ctx);
                    if sc.resolve_prefix(prefix).is_none() {
                        return Err(XsltError::new(
                            codes::XTDE0290,
                            format!(
                                "the data-type {value:?} of xsl:sort uses the prefix {prefix:?}, \
                                 which is not declared"
                            ),
                        ));
                    }
                }
                data_type
            }
            None => SortDataType::Typed,
        };
        // `stable` is checked but changes nothing: every sort here is stable.
        if let Some(avt) = &key.stable {
            let value = self.avt(avt, frame, cx)?;
            if !matches!(whitespace::trim(&value), "yes" | "no") {
                return Err(bad_value("stable", &value));
            }
        }
        let case_order = match &key.case_order {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                Some(parse_case_order(&value).ok_or_else(|| bad_value("case-order", &value))?)
            }
            None => None,
        };
        let lang = match &key.lang {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                if !is_valid_lang(&value) {
                    return Err(bad_value("lang", &value));
                }
                // An empty `xml:lang` value says "no language" (XML 1.0
                // §2.12), so it chooses no language-specific collation.
                (!whitespace::trim(&value).is_empty()).then_some(value)
            }
            None => None,
        };
        let collation = match &key.collation {
            Some(avt) => {
                let value = self.avt(avt, frame, cx)?;
                self.sort_collation(key, &value)?
            }
            // §13.1.3: "in the absence of a collation attribute, these
            // attributes provide input to an implementation-defined algorithm
            // to locate a suitable collation" — `CaseOrderCollation`, whatever
            // the language, lower-first unless `case-order` says otherwise;
            // with the `icu-collation` feature the UCA collation tailored for
            // the language (`CollationRegistry::sort_collation`).
            None if case_order.is_some() || lang.is_some() => KeyCollation::Other(
                self.sheet
                    .functions()
                    .collations()
                    .sort_collation(lang.as_deref(), case_order),
            ),
            // "If none of the collation, lang or case-order attributes is
            // present, the collation is chosen in an implementation-defined
            // way": the codepoint collation. §3.6.1: "The
            // [xsl:]default-collation attribute does not affect the collation
            // used by xsl:sort."
            None => KeyCollation::Codepoint,
        };
        Ok(KeyRules {
            order,
            data_type,
            collation,
            first_item_only: self.sheet.is_backwards(key.ctx),
        })
    }

    /// The collation an `xsl:sort` `collation` attribute names (§13.1.3).
    ///
    /// > If the effective value of the `collation` attribute of `xsl:sort` is a
    /// > relative URI, then it is resolved against the base URI of the
    /// > `xsl:sort` element.
    ///
    /// > \[ERR XTDE1035\] It is a non-recoverable dynamic error if the
    /// > `collation` attribute of `xsl:sort` (after resolving against the base
    /// > URI) is not a URI that is recognized by the implementation as
    /// > referring to a collation.
    fn sort_collation(&self, key: &SortKey, value: &str) -> Result<KeyCollation, XsltError> {
        let base = &self.sheet.static_contexts.get(key.ctx).base_uri;
        let uri = crate::resolver::resolve_uri(base, whitespace::trim(value));
        if uri == CODEPOINT_COLLATION_URI {
            return Ok(KeyCollation::Codepoint);
        }
        if let Some(collation) = self.sheet.functions().collations().resolve(&uri) {
            return Ok(KeyCollation::Other(collation));
        }
        if let Some(missing) = uca_not_implemented(&uri) {
            return Err(missing);
        }
        Err(XsltError::new(
            codes::XTDE1035,
            format!(
                "xsl:sort names the collation {uri:?}, which this processor does not recognize"
            ),
        ))
    }

    /// One sort key value (§13.1.1, §13.1.2): `None` for the empty sequence.
    ///
    /// `inner` carries the focus the key is computed with — for an initial
    /// sequence:
    ///
    /// > This evaluation is done with the focus set as follows: The context
    /// > item is the item in the initial sequence whose sort key value is being
    /// > computed. The context position is the position of that item in the
    /// > initial sequence. The context size is the size of the initial
    /// > sequence.
    ///
    /// and for the groups of `xsl:for-each-group` the initial item of the group,
    /// its position among the initial items and the number of groups, with the
    /// group as the current group (§14.3).
    ///
    /// `current()` is that item too (the §13.1.1 Note). §6.7: "when evaluating
    /// a sequence constructor contained in an xsl:sort or xsl:key element … the
    /// current template rule becomes null for the evaluation of that
    /// instruction or function"; §6.5: "While evaluating global variables and
    /// parameters, and the sequence constructor contained in xsl:key or
    /// xsl:sort, the current mode is set to the default mode." A `select`
    /// expression cannot observe either, so both are cleared for every key.
    fn sort_key_value(
        &self,
        key: &SortKey,
        rule: &KeyRules,
        rows: SortRows,
        mut inner: XsltContext<'t>,
        frame: &mut Frame<'t>,
    ) -> Result<Option<XmlValue>, XsltError> {
        let position = inner.position;
        inner.current_rule = None;
        inner.mode = ModeKey::Default;
        // §19.1: "… xsl:sort, and xsl:message always evaluate the
        // instructions in their contained sequence constructor in temporary
        // output state."
        inner.final_output = false;
        let value = match &key.value {
            ValueSource::Select(id) => self.eval_expr(*id, frame, &inner)?,
            ValueSource::Content(body) => {
                let mut collector = self.collector();
                self.eval_body(body, frame, &inner, &mut collector)?;
                XPathValue::from_sequence(collector.into_items())
            }
        };
        let mut atoms = atomize_sequence(value).map_err(|e| self.xpath_error(e))?;
        if atoms.len() > 1 {
            // > [ERR XTTE1020] If any sort key value, after atomization and any
            // > type conversion required by the data-type attribute, is a
            // > sequence containing more than one item, then the effect depends
            // > on whether the xsl:sort element is evaluated with backwards
            // > compatible behavior. With backwards compatible behavior, the
            // > effective sort key value is the first item in the sequence. In
            // > other cases, this is a type error.
            if !rule.first_item_only {
                return Err(XsltError::new(
                    codes::XTTE1020,
                    format!(
                        "the sort key value of {} {position} is a sequence of {} items",
                        rows.noun(),
                        atoms.len()
                    ),
                ));
            }
            atoms.truncate(1);
        }
        let Some(atom) = atoms.pop() else {
            // The conversion below uses `string()` / `number()`, which map the
            // empty sequence to "" and NaN, and §13.1.2 classifies the values
            // as empty or ordinary only "after any conversion": with a
            // `data-type` an empty key is an ordinary value.
            return Ok(match rule.data_type {
                SortDataType::Text => Some(XmlValue::string("")),
                SortDataType::Number => Some(XmlValue::double(f64::NAN)),
                SortDataType::Typed => None,
            });
        };
        // §13.1.2: "If this has the effective value text, the atomized sort key
        // values are converted to strings before being compared. If it has the
        // effective value number, the atomized sort key values are converted to
        // doubles before being compared. The conversion is done by using the
        // string or number function as appropriate." Without it, "Any values of
        // type xs:untypedAtomic are cast to xs:string."
        Ok(Some(match rule.data_type {
            SortDataType::Text => XmlValue::string(atom.to_string_value()),
            // fn:number: NaN for anything that cannot be cast to xs:double.
            SortDataType::Number => XmlValue::double(
                cast_to(&atom, XmlTypeCode::Double)
                    .ok()
                    .and_then(|d| d.as_double())
                    .unwrap_or(f64::NAN),
            ),
            SortDataType::Typed if atom.is_untyped() => XmlValue::string(atom.to_string_value()),
            SortDataType::Typed => atom,
        }))
    }
}

/// XTDE0030 for a computed `xsl:sort` attribute value that is not permitted.
fn bad_value(attr: &str, value: &str) -> XsltError {
    XsltError::new(
        codes::XTDE0030,
        format!("{value:?} is not a permitted value of the {attr} attribute of xsl:sort"),
    )
}

/// XTDE1030 for a pair of sort key values that `lt` cannot compare.
fn incomparable(a: &XmlValue, b: &XmlValue) -> XsltError {
    XsltError::new(
        codes::XTDE1030,
        format!(
            "the sort key values {:?} (xs:{}) and {:?} (xs:{}) cannot be compared",
            a.to_string_value(),
            a.type_code.local_name().unwrap_or("anyAtomicType"),
            b.to_string_value(),
            b.type_code.local_name().unwrap_or("anyAtomicType")
        ),
    )
}

/// The classes of values between which the XPath `lt` operator is defined.
///
/// XPath 2.0 B.2: "The operator mapping tables in this section list the
/// combinations of types for which the various operators of XPath are
/// defined." For `lt` they pair numeric with numeric, and `xs:boolean`,
/// `xs:string`, `xs:date`, `xs:time`, `xs:dateTime`, `xs:yearMonthDuration`,
/// `xs:dayTimeDuration` and `xs:anyURI` each with itself — and since "a value of
/// type xs:anyURI (or any type derived by restriction from xs:anyURI) can be
/// promoted to the type xs:string" (B.1), strings and URIs form one class.
/// Two values of different classes, or two of a type with no entry at all
/// (`xs:QName`, `xs:duration`, `xs:gYear`, `xs:hexBinary`, …), make `lt` an
/// error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LtClass {
    /// `xs:string`, `xs:anyURI` and the types derived from them by
    /// restriction — compared under the collation (§13.1.3).
    String,
    /// `xs:decimal`, `xs:float`, `xs:double` and the types derived from them.
    Numeric,
    Boolean,
    Date,
    Time,
    /// `xs:dateTime` and `xs:dateTimeStamp`.
    DateTime,
    YearMonthDuration,
    DayTimeDuration,
    /// A type `lt` is not defined for: any pair is an error.
    Unordered,
}

impl LtClass {
    fn of(value: &XmlValue) -> Self {
        match value.type_code {
            XmlTypeCode::YearMonthDuration => return LtClass::YearMonthDuration,
            XmlTypeCode::DayTimeDuration => return LtClass::DayTimeDuration,
            _ => {}
        }
        match value.primitive_type() {
            Some(PrimitiveTypeCode::String | PrimitiveTypeCode::AnyUri) => LtClass::String,
            Some(
                PrimitiveTypeCode::Decimal | PrimitiveTypeCode::Float | PrimitiveTypeCode::Double,
            ) => LtClass::Numeric,
            Some(PrimitiveTypeCode::Boolean) => LtClass::Boolean,
            Some(PrimitiveTypeCode::Date) => LtClass::Date,
            Some(PrimitiveTypeCode::Time) => LtClass::Time,
            Some(PrimitiveTypeCode::DateTime) => LtClass::DateTime,
            _ => LtClass::Unordered,
        }
    }

    /// Whether the values of this class are compared with the engine's own
    /// `lt` ([`value_lt`]) rather than through a key built here.
    fn uses_value_lt(self) -> bool {
        matches!(
            self,
            LtClass::Date
                | LtClass::Time
                | LtClass::DateTime
                | LtClass::YearMonthDuration
                | LtClass::DayTimeDuration
        )
    }
}

/// The widest numeric type of a column: §13.1.2's "most specific type to which
/// all the values can be converted by subtype substitution and/or type
/// promotion".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NumericWidth {
    Decimal,
    Float,
    Double,
}

/// One sort key value, in the form [`compare_keys`] orders.
///
/// [`column_keys`] gives every ordinary value of a column the same variant, so
/// within a column the comparison is that variant's — a total order in each
/// case (see [`compare_keys`]).
enum ColumnKey {
    /// The empty sequence.
    Empty,
    /// A string or URI, compared under the key's collation.
    String(String),
    /// A number of a column whose widest type is `xs:float` or `xs:double`,
    /// promoted to that type (an `xs:float` is held exactly as an `f64`); NaN
    /// included.
    Float(f64),
    /// A number of a column of decimals and integers only, held exactly.
    Decimal(DecimalKey),
    Boolean(bool),
    /// A date, time, dateTime or duration of one of the ordered duration types,
    /// compared with [`value_lt`]; or the single value of an unordered type in
    /// a column with no other ordinary value.
    Other(XmlValue),
}

impl ColumnKey {
    /// A fixed order between the variants, used only for a pair
    /// [`column_keys`] never produces.
    fn rank(&self) -> u8 {
        match self {
            ColumnKey::Empty => 0,
            ColumnKey::String(_) => 1,
            ColumnKey::Float(_) => 2,
            ColumnKey::Decimal(_) => 3,
            ColumnKey::Boolean(_) => 4,
            ColumnKey::Other(_) => 5,
        }
    }
}

/// An exact image of an `xs:decimal` or `xs:integer` value whose derived
/// order is the numeric order: the sign, the integer digits without leading
/// zeros and the fraction digits without trailing zeros (zero is
/// non-negative with no digits). It needs no arithmetic, so no pair of values
/// can fail to compare, whatever their magnitude.
#[derive(Debug, PartialEq, Eq)]
struct DecimalKey {
    negative: bool,
    int: String,
    frac: String,
}

impl DecimalKey {
    /// Parses `[+-]?digits(.digits)?`.
    fn parse(lexical: &str) -> Option<Self> {
        let s = whitespace::trim(lexical);
        let (negative, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
        if (int.is_empty() && frac.is_empty())
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let zero = int.is_empty() && frac.is_empty();
        Some(DecimalKey {
            negative: negative && !zero,
            int: int.to_string(),
            frac: frac.to_string(),
        })
    }

    fn of(value: &XmlValue) -> Self {
        DecimalKey::parse(&value.to_string_value())
            // `f64`'s `Display` never uses an exponent.
            .or_else(|| {
                value
                    .as_double()
                    .and_then(|d| DecimalKey::parse(&d.to_string()))
            })
            .unwrap_or(DecimalKey {
                negative: false,
                int: String::new(),
                frac: String::new(),
            })
    }

    fn magnitude(&self, other: &Self) -> Ordering {
        self.int
            .len()
            .cmp(&other.int.len())
            .then_with(|| self.int.cmp(&other.int))
            .then_with(|| self.frac.cmp(&other.frac))
    }
}

impl Ord for DecimalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, false) => self.magnitude(other),
            (true, true) => other.magnitude(self),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for DecimalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Checks one sort key column for XTDE1030 and turns it into keys (§13.1.2).
///
/// > \[ERR XTDE1030\] It is a non-recoverable dynamic error if, for any sort
/// > key component, the set of sort key values evaluated for all the items in
/// > the initial sequence, after any type conversion requested, contains a pair
/// > of ordinary values for which the result of the XPath `lt` operator is an
/// > error.
///
/// `lt` is an error for a pair of ordinary values exactly when they are of
/// different [`LtClass`]es or of the class `Unordered`, so comparing every
/// ordinary value with the *first* one decides it for every pair. For the
/// classes compared with [`value_lt`] the engine is asked too: its date, time
/// and duration comparisons reduce each value to a number on its own, so a
/// value it cannot compare with the first one is one it can compare with no
/// value, and one it can compare with the first it can compare with all.
/// `Err` holds the incomparable pair.
///
/// > To ensure a total ordering, the same implementation of the `lt` operator
/// > must be used for all the comparisons: the one that is chosen is the one
/// > appropriate to the most specific type to which all the values can be
/// > converted by subtype substitution and/or type promotion. For example, if
/// > the sequence contains both `xs:decimal` and `xs:double` values, then the
/// > values are compared using `xs:double` comparison, even when comparing two
/// > `xs:decimal` values.
///
/// So a numeric column holding an `xs:double` becomes `xs:double` keys
/// throughout, one holding an `xs:float` (and no double) `xs:float` keys, and
/// one of decimals and integers only exact [`DecimalKey`]s.
fn column_keys(
    values: Vec<Option<XmlValue>>,
    timezone: TimezoneOffset,
) -> Result<Vec<ColumnKey>, (XmlValue, XmlValue)> {
    let values: Vec<Option<XmlValue>> = values.into_iter().map(|v| v.map(unwrap_union)).collect();
    let mut first: Option<(&XmlValue, LtClass)> = None;
    let mut width = NumericWidth::Decimal;
    for value in values.iter().flatten() {
        let class = LtClass::of(value);
        match first {
            None => first = Some((value, class)),
            Some((head, head_class)) => {
                if class != head_class
                    || class == LtClass::Unordered
                    || (class.uses_value_lt() && value_lt(head, value, timezone).is_err())
                {
                    return Err((head.clone(), value.clone()));
                }
            }
        }
        if class == LtClass::Numeric {
            width = width.max(match value.primitive_type() {
                Some(PrimitiveTypeCode::Double) => NumericWidth::Double,
                Some(PrimitiveTypeCode::Float) => NumericWidth::Float,
                _ => NumericWidth::Decimal,
            });
        }
    }
    let class = first.map(|(_, class)| class);
    Ok(values
        .into_iter()
        .map(|value| {
            let Some(value) = value else {
                return ColumnKey::Empty;
            };
            match class {
                Some(LtClass::String) => ColumnKey::String(value.to_string_value()),
                Some(LtClass::Numeric) => match width {
                    NumericWidth::Double => ColumnKey::Float(to_double(&value)),
                    NumericWidth::Float => ColumnKey::Float(to_float(&value)),
                    NumericWidth::Decimal => ColumnKey::Decimal(DecimalKey::of(&value)),
                },
                Some(LtClass::Boolean) => ColumnKey::Boolean(value.as_boolean().unwrap_or(false)),
                _ => ColumnKey::Other(value),
            }
        })
        .collect())
}

/// The engine's `lt` on two values (XPath 2.0 §B.2), a date or time without a timezone read with
/// `timezone`, the transformation's implicit timezone (`eval_binary_with_timezone`, upstream C94).
fn value_lt(
    left: &XmlValue,
    right: &XmlValue,
    timezone: TimezoneOffset,
) -> Result<bool, xsd_schema::xpath::XPathError> {
    eval_binary_with_timezone(BinaryOpKind::ValueLt, left, right, timezone)
        .map(|result| result.as_boolean().unwrap_or(false))
}

/// The member value of a union-typed value; any other value as it is.
fn unwrap_union(value: XmlValue) -> XmlValue {
    match value.value {
        XmlValueKind::Union(inner) => unwrap_union(*inner),
        _ => value,
    }
}

/// XPath 2.0 B.1: "A value of type xs:float (or any type derived by restriction
/// from xs:float) can be promoted to the type xs:double. The result is the
/// xs:double value that is the same as the original value. A value of type
/// xs:decimal (or any type derived by restriction from xs:decimal) can be
/// promoted to either of the types xs:float or xs:double. The result of this
/// promotion is created by casting the original value to the required type."
fn to_double(value: &XmlValue) -> f64 {
    value.as_double().unwrap_or(f64::NAN)
}

/// A decimal cast to `xs:float` (one rounding, from the decimal digits), or an
/// `xs:float` as it is; widened exactly to `f64`.
fn to_float(value: &XmlValue) -> f64 {
    if value.primitive_type() == Some(PrimitiveTypeCode::Float) {
        return to_double(value);
    }
    match value.to_string_value().parse::<f32>() {
        Ok(f) => f64::from(f),
        Err(_) => to_double(value) as f32 as f64,
    }
}

/// Orders two sort keys of one column (§13.1.2, §13.1.3).
///
/// > The empty sort key values … are considered for sorting purposes to be
/// > equal to each other, but less than any other value.
///
/// > NaN values, for sorting purposes, are considered to be equal to each
/// > other, and less than any other numeric value.
///
/// > If the `xsl:sort` element has a `collation` attribute, then the strings
/// > are compared according to the rules for the named collation: that is, they
/// > are compared using the XPath function call `compare($a, $b, $collation)`.
///
/// Each arm is a total order: empty below everything; strings by the collation;
/// floats with NaN lowest and `-0` equal to `0` (any other pair of non-NaN
/// `f64`s has a `partial_cmp`); exact decimals; `false < true`; and the engine's
/// `lt` on dates, times and durations, which [`column_keys`] has checked
/// succeeds for the column. A pair of different variants never occurs within a
/// column; it is ordered by variant so that even it cannot break totality.
fn compare_keys(
    a: &ColumnKey,
    b: &ColumnKey,
    collation: &KeyCollation,
    timezone: TimezoneOffset,
) -> Ordering {
    match (a, b) {
        (ColumnKey::Empty, ColumnKey::Empty) => Ordering::Equal,
        (ColumnKey::Empty, _) => Ordering::Less,
        (_, ColumnKey::Empty) => Ordering::Greater,
        (ColumnKey::String(x), ColumnKey::String(y)) => collation.compare(x, y),
        (ColumnKey::Float(x), ColumnKey::Float(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        },
        (ColumnKey::Decimal(x), ColumnKey::Decimal(y)) => x.cmp(y),
        (ColumnKey::Boolean(x), ColumnKey::Boolean(y)) => x.cmp(y),
        (ColumnKey::Other(x), ColumnKey::Other(y)) => {
            if value_lt(x, y, timezone).unwrap_or(false) {
                Ordering::Less
            } else if value_lt(y, x, timezone).unwrap_or(false) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        _ => a.rank().cmp(&b.rank()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> DecimalKey {
        DecimalKey::parse(s).unwrap()
    }

    /// The decimal key orders as the numbers do, at any magnitude.
    #[test]
    fn decimal_keys_order_numerically() {
        let ascending = [
            "-123456789012345678901234567890.5",
            "-10",
            "-9.99",
            "-0.5",
            "-0.05",
            "0",
            "0.05",
            "0.123",
            "0.5",
            "1",
            "1.00000000000000001",
            "1.00000000000000002",
            "9.99",
            "10",
            "123456789012345678901234567890",
        ];
        for (i, a) in ascending.iter().enumerate() {
            for (j, b) in ascending.iter().enumerate() {
                assert_eq!(key(a).cmp(&key(b)), i.cmp(&j), "{a} vs {b}");
            }
        }
        assert_eq!(key("-0.00"), key("0"));
        assert_eq!(key("+1.50"), key("001.5"));
        assert_eq!(DecimalKey::parse("1e5"), None);
        assert_eq!(DecimalKey::parse("."), None);
    }
}
