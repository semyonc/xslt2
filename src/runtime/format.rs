//! §16.4 `format-number` and §16.5 `format-date`, `format-time` and
//! `format-dateTime`: the XDM side.
//!
//! The formatting itself is pure and lives in `format_number.rs` and
//! `format_date.rs`; this file converts the arguments (XPath 2.0
//! §3.1.5's function conversion rules, which the upstream engine leaves to the
//! function outside XPath 1.0 compatibility mode) and resolves what the
//! stylesheet supplies — the decimal format, the static namespaces.

use xsd_schema::types::{XmlAtomicValue, XmlTypeCode, XmlValue, XmlValueKind};
use xsd_schema::xpath::functions::XPathValue;

use super::functions::{converts_to_string, describe_atoms, XsltEvaluator};
use crate::error::{codes, XsltError};
use crate::format_date::{
    self, calendar_by_designator, language_falls_back, Calendar, Date, DateTimeParts, Function,
    PictureError, Settings, Time,
};
use crate::format_number::{Numeric, Picture};
use crate::functions::{expand_available_name, expand_name_argument, Unprefixed, XsltFn};
use crate::names::QName;
use crate::receiver::Nav;

impl<'t> XsltEvaluator<'_, '_, 't> {
    /// `format-number($value as numeric?, $picture as xs:string)` and the form
    /// with `$decimal-format-name as xs:string` (§16.4).
    pub(super) fn format_number(
        &self,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let mut args = args.into_iter();
        let value = self.numeric_argument(args.next().unwrap_or_else(XPathValue::empty))?;
        let picture = self.string_argument(
            args.next().unwrap_or_else(XPathValue::empty),
            "the second argument of format-number() ($picture as xs:string)",
        )?;
        let format = match args.next() {
            None => self.run.sheet.decimal_format(None),
            Some(name) => {
                let name = self.string_argument(
                    name,
                    "the third argument of format-number() ($decimal-format-name as xs:string)",
                )?;
                // "[ERR XTDE1280] It is a non-recoverable dynamic error if the
                // name specified as the $decimal-format-name argument is not a
                // valid QName, or if its prefix has not been declared in an
                // in-scope namespace declaration, or if the stylesheet does not
                // contain a declaration of a decimal-format with a matching
                // expanded-QName."
                let (uri, local) =
                    expand_name_argument(&self.name_scope(), XsltFn::FormatNumber, &name)?;
                let qname = QName::new(uri.as_str(), local);
                Some(self.run.sheet.decimal_format(Some(&qname)).ok_or_else(|| {
                    XsltError::new(
                        codes::XTDE1280,
                        format!(
                            "format-number(): the stylesheet declares no decimal format {qname}"
                        ),
                    )
                })?)
            }
        };
        let format =
            format.ok_or_else(|| XsltError::internal("the unnamed decimal format is missing"))?;
        let picture = Picture::parse(&picture, format)
            .map_err(|message| XsltError::new(codes::XTDE1310, message))?;
        Ok(XPathValue::string(picture.format(&value, format)))
    }

    /// `$value as numeric?`.
    ///
    /// > If the supplied value of the `$value` argument is an empty sequence,
    /// > the function behaves as if the supplied value were the `xs:double`
    /// > value NaN.
    ///
    /// Otherwise XPath 2.0 §3.1.5: atomization, then "For built-in functions
    /// where the expected type is specified as numeric, arguments of type
    /// `xs:untypedAtomic` are cast to `xs:double`", and a value that is still
    /// not numeric is XPTY0004. (With XPath 1.0 compatibility mode the engine
    /// has already replaced the value by `fn:number(V)`, an `xs:double`.)
    fn numeric_argument(&self, value: XPathValue<Nav<'t>>) -> Result<Numeric, XsltError> {
        let atoms = xsd_schema::xpath::functions::atomize_sequence(value)
            .map_err(|e| self.run.xpath_error(e))?;
        let atom = match atoms.as_slice() {
            [] => return Ok(Numeric::Double(f64::NAN)),
            [atom] => atom,
            _ => {
                return Err(XsltError::new(
                    "XPTY0004",
                    format!(
                        "the first argument of format-number() ($value as numeric?) must be at \
                         most one number, not {}",
                        describe_atoms(&atoms)
                    ),
                ))
            }
        };
        let atom = match &atom.value {
            XmlValueKind::Union(inner) => inner.as_ref(),
            _ => atom,
        };
        if let XmlValueKind::UntypedAtomic(_) = atom.value {
            let cast = xsd_schema::xpath::cast::cast_to(atom, XmlTypeCode::Double)
                .map_err(|e| self.run.xpath_error(e))?;
            return numeric(&cast).ok_or_else(|| XsltError::internal("a cast to xs:double"));
        }
        numeric(atom).ok_or_else(|| {
            XsltError::new(
                "XPTY0004",
                format!(
                    "the first argument of format-number() ($value as numeric?) must be a \
                     number, not {}",
                    describe_atoms(std::slice::from_ref(atom))
                ),
            )
        })
    }
}

impl<'t> XsltEvaluator<'_, '_, 't> {
    /// `format-dateTime`, `format-date` and `format-time` (§16.5), in the
    /// two-argument form and the five-argument form: "Calling the two-argument
    /// form of each of the three functions is equivalent to calling the
    /// five-argument form with each of the last three arguments set to an empty
    /// sequence."
    pub(super) fn format_date_time(
        &self,
        which: XsltFn,
        args: Vec<XPathValue<Nav<'t>>>,
    ) -> Result<XPathValue<Nav<'t>>, XsltError> {
        let function = match which {
            XsltFn::FormatDate => Function::Date,
            XsltFn::FormatTime => Function::Time,
            _ => Function::DateTime,
        };
        let name = function.name();
        let mut args = args.into_iter();
        let value = args.next().unwrap_or_else(XPathValue::empty);
        let picture = self.string_argument(
            args.next().unwrap_or_else(XPathValue::empty),
            &format!("the second argument of {name}() ($picture as xs:string)"),
        )?;
        let language = self.optional_string_argument(args.next(), name, "$language")?;
        let calendar = self.optional_string_argument(args.next(), name, "$calendar")?;
        // §16.5.2: "If the value is supplied, and is not the empty sequence,
        // then it should be a country code defined in [ISO 3166-1]" — "should":
        // any string is accepted, and nothing here depends on the country.
        let _country = self.optional_string_argument(args.next(), name, "$country")?;
        // "If $value is the empty sequence, the empty sequence is returned."
        let Some(parts) = self.date_time_argument(value, function)? else {
            return Ok(XPathValue::empty());
        };
        let settings = Settings {
            language_fallback: language_falls_back(language.as_deref()),
            ..self.calendar_settings(calendar.as_deref(), name)?
        };
        format_date::format(&parts, &picture, function, settings)
            .map(XPathValue::string)
            .map_err(|e| match e {
                PictureError::Syntax(message) => {
                    XsltError::new(codes::XTDE1340, format!("{name}(): {message}"))
                }
                PictureError::Unavailable(message) => XsltError::new(codes::XTDE1350, message),
            })
    }

    /// `$calendar` (§16.5.2): "A calendar value must be a valid QName. If the
    /// QName does not have a prefix, then it identifies a calendar with the
    /// designator specified below. If the QName has a prefix, then the QName is
    /// expanded into an expanded-QName as described in 5.1 Qualified Names; the
    /// expanded-QName identifies the calendar; the behavior in this case is
    /// implementation-defined."
    ///
    /// Absent (or zero-length): `AD`. An unprefixed designator from the table:
    /// `AD` or `ISO`, any other one falls back to `AD`. A prefixed name (its
    /// prefix bound in the static context, which §5.1 requires): no such
    /// calendar is supported, so it falls back to `AD` too. Anything else is an
    /// invalid calendar — "In general, the use of an invalid picture, language,
    /// calendar, or country argument is classified as a non-recoverable dynamic
    /// error" — and §16.5 names one code for its errors of this kind, XTDE1340
    /// (F&O 3.0 later named this case FOFD1340, the same code renamed).
    fn calendar_settings(&self, calendar: Option<&str>, name: &str) -> Result<Settings, XsltError> {
        let invalid = |why: String| {
            XsltError::new(
                codes::XTDE1340,
                format!(
                    "{name}(): the calendar {:?} is not valid: {why}",
                    calendar.unwrap_or("")
                ),
            )
        };
        let designator = match calendar.map(crate::whitespace::trim) {
            None | Some("") => None,
            // The default namespace is not used for an unprefixed name
            // (format-date-037).
            Some(text) => {
                match expand_available_name(&self.name_scope(), text, Unprefixed::NoNamespace) {
                    Ok((uri, local)) if uri.is_empty() => Some(local),
                    Ok(_) => {
                        return Ok(Settings {
                            calendar: Calendar::Ad,
                            calendar_fallback: true,
                            language_fallback: false,
                        })
                    }
                    Err(crate::functions::NameError::NotAQName) => {
                        return Err(invalid("it is not a lexical QName".into()))
                    }
                    Err(crate::functions::NameError::UnboundPrefix) => {
                        return Err(invalid(
                            "its prefix has no namespace declaration in scope".into(),
                        ))
                    }
                }
            }
        };
        let (calendar, calendar_fallback) = calendar_by_designator(designator).map_err(invalid)?;
        Ok(Settings {
            calendar,
            calendar_fallback,
            language_fallback: false,
        })
    }

    /// An optional `xs:string?` argument (`$language`, `$calendar`,
    /// `$country`): `None` for an absent argument or the empty sequence.
    fn optional_string_argument(
        &self,
        value: Option<XPathValue<Nav<'t>>>,
        function: &str,
        parameter: &str,
    ) -> Result<Option<String>, XsltError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if self.xpath1_compatible() {
            return Ok(value.first().map(|item| match item {
                xsd_schema::xpath::XmlItem::Atomic(v) => v.to_string_value(),
                xsd_schema::xpath::XmlItem::Node(n) => {
                    xsd_schema::navigator::DomNavigator::value(n)
                }
            }));
        }
        let atoms = xsd_schema::xpath::functions::atomize_sequence(value)
            .map_err(|e| self.run.xpath_error(e))?;
        match atoms.as_slice() {
            [] => Ok(None),
            [atom] if converts_to_string(atom) => Ok(Some(atom.to_string_value())),
            _ => Err(XsltError::new(
                "XPTY0004",
                format!(
                    "the {parameter} argument of {function}() (xs:string?) must be at most one \
                     string, not {}",
                    describe_atoms(&atoms)
                ),
            )),
        }
    }

    /// `$value as xs:dateTime?` / `xs:date?` / `xs:time?`: XPath 2.0 §3.1.5 —
    /// atomization, an `xs:untypedAtomic` cast to the expected type, and
    /// XPTY0004 for anything else (no promotion reaches a date or a time).
    fn date_time_argument(
        &self,
        value: XPathValue<Nav<'t>>,
        function: Function,
    ) -> Result<Option<DateTimeParts>, XsltError> {
        let (expected, type_name) = match function {
            Function::Date => (XmlTypeCode::Date, "xs:date?"),
            Function::Time => (XmlTypeCode::Time, "xs:time?"),
            Function::DateTime => (XmlTypeCode::DateTime, "xs:dateTime?"),
        };
        let atoms = xsd_schema::xpath::functions::atomize_sequence(value)
            .map_err(|e| self.run.xpath_error(e))?;
        let atom = match atoms.as_slice() {
            [] => return Ok(None),
            [atom] => atom,
            _ => {
                return Err(XsltError::new(
                    "XPTY0004",
                    format!(
                        "the first argument of {}() ($value as {type_name}) must be at most one \
                         value, not {}",
                        function.name(),
                        describe_atoms(&atoms)
                    ),
                ))
            }
        };
        let atom = match &atom.value {
            XmlValueKind::Union(inner) => inner.as_ref().clone(),
            _ => atom.clone(),
        };
        let atom = if let XmlValueKind::UntypedAtomic(_) = atom.value {
            xsd_schema::xpath::cast::cast_to(&atom, expected)
                .map_err(|e| self.run.xpath_error(e))?
        } else {
            atom
        };
        let parts = match (&atom.value, function) {
            (XmlValueKind::Atomic(XmlAtomicValue::DateTime(v)), Function::DateTime) => {
                DateTimeParts {
                    date: Some(date(i64::from(v.year), v.month, v.day)),
                    time: Some(time(v.hour, v.minute, &v.second.to_string())),
                    timezone: v.timezone.map(|tz| i32::from(tz.0)),
                }
            }
            (XmlValueKind::Atomic(XmlAtomicValue::Date(v)), Function::Date) => DateTimeParts {
                date: Some(date(i64::from(v.year), v.month, v.day)),
                time: None,
                timezone: v.timezone.map(|tz| i32::from(tz.0)),
            },
            (XmlValueKind::Atomic(XmlAtomicValue::Time(v)), Function::Time) => DateTimeParts {
                date: None,
                time: Some(time(v.hour, v.minute, &v.second.to_string())),
                timezone: v.timezone.map(|tz| i32::from(tz.0)),
            },
            _ => {
                return Err(XsltError::new(
                    "XPTY0004",
                    format!(
                        "the first argument of {}() ($value as {type_name}) must be one, not {}",
                        function.name(),
                        describe_atoms(std::slice::from_ref(&atom))
                    ),
                ))
            }
        };
        Ok(Some(parts))
    }
}

fn date(year: i64, month: u8, day: u8) -> Date {
    Date {
        year,
        month: u32::from(month),
        day: u32::from(day),
    }
}

/// The time of day, with the seconds from their exact decimal lexical form.
fn time(hour: u8, minute: u8, seconds: &str) -> Time {
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    Time {
        hour: u32::from(hour),
        minute: u32::from(minute),
        second: whole.parse().unwrap_or(0),
        fraction: fraction.trim_end_matches('0').to_string(),
    }
}

/// A numeric atomic value in its own type: `xs:double`, `xs:float`, or an
/// `xs:decimal` / `xs:integer` (and their subtypes) as its exact decimal digits.
fn numeric(value: &XmlValue) -> Option<Numeric> {
    match &value.value {
        XmlValueKind::Atomic(XmlAtomicValue::Double(x)) => Some(Numeric::Double(*x)),
        XmlValueKind::Atomic(XmlAtomicValue::Float(x)) => Some(Numeric::Float(*x)),
        XmlValueKind::Atomic(XmlAtomicValue::Decimal(d)) => Some(Numeric::Decimal(d.to_string())),
        XmlValueKind::Atomic(XmlAtomicValue::Integer(i)) => Some(Numeric::Decimal(i.to_string())),
        _ => None,
    }
}
