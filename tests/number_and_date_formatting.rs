//! number and date formatting (XSLT 2.0 §16.4, §16.5): `xsl:decimal-format`
//! and `format-number`, and `format-date`, `format-time` and `format-dateTime`.
//!
//! Every test goes through the public API — compile, transform,
//! `serialize_primary` — and cites the rule it checks.

mod common;

use std::sync::Arc;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, MemoryResolver, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn sheet_versioned(version: &str, body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"{version}\" xmlns:xsl=\"{NS}\" \
           xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
           exclude-result-prefixes=\"xs\">{body}</xsl:stylesheet>"
    )
}

fn sheet(body: &str) -> String {
    sheet_versioned("2.0", body)
}

/// Where a run failed: at compile time or at run time, with the code.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Static(String),
    Dynamic(String),
}

/// Compiles `main` (with `modules` beside it, for `xsl:import` /
/// `xsl:include`), runs it over `<doc/>` and returns the serialized primary
/// result without the default XML declaration.
fn run_modules(main: &str, modules: &[(&str, &str)]) -> Result<String, Failure> {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let mut mem = MemoryResolver::new();
    for (uri, text) in modules {
        mem.insert(*uri, *text);
    }
    let compiled = Compiler::new(&names, &functions)
        .with_resolver(Arc::new(mem))
        .compile_str(main, "file:///formatting/main.xsl")
        .map_err(|e| Failure::Static(e.code))?;
    let arena = bumpalo::Bump::new();
    let outcome = compiled
        .transformation(&arena, TransformOptions::default())
        .transform_source("file:///formatting/in.xml", "<doc><n>1234.5</n></doc>")
        .map_err(|e| Failure::Dynamic(e.code))?;
    let text = compiled
        .serialize_primary(&outcome)
        .map_err(|e| Failure::Dynamic(e.code))?;
    Ok(common::body(text))
}

fn run(text: &str) -> Result<String, Failure> {
    run_modules(text, &[])
}

/// `declarations`, then a template matching `/` whose `<out>` holds the value
/// of `select`; the text of `<out>`.
fn value_with(declarations: &str, select: &str) -> String {
    let text = sheet(&format!(
        r#"{declarations}<xsl:template match="/"><out><xsl:value-of select="{select}"/></out></xsl:template>"#
    ));
    unwrap_out(run(&text).unwrap_or_else(|f| panic!("{select}: {f:?}")))
}

fn value(select: &str) -> String {
    value_with("", select)
}

fn unwrap_out(all: String) -> String {
    if all == "<out/>" {
        return String::new();
    }
    all.strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .map(str::to_string)
        .unwrap_or(all)
}

fn static_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Static(code)) => code,
        other => panic!("expected a static error, got {other:?}"),
    }
}

fn dynamic_error(text: &str) -> String {
    match run(text) {
        Err(Failure::Dynamic(code)) => code,
        other => panic!("expected a dynamic error, got {other:?}"),
    }
}

/// The error of evaluating `select` after `declarations`, whichever phase.
fn error_with(declarations: &str, select: &str) -> Failure {
    let text = sheet(&format!(
        r#"{declarations}<xsl:template match="/"><out><xsl:value-of select="{select}"/></out></xsl:template>"#
    ));
    match run(&text) {
        Err(f) => f,
        Ok(out) => panic!("{select}: expected an error, got {out}"),
    }
}

// ═══ §16.4.1 Defining a decimal format ═════════════════════════════════════

/// "If a stylesheet does not contain a declaration of the unnamed decimal
/// format, a declaration equivalent to an `xsl:decimal-format` element with no
/// attributes is implied" — and §16.4.1's defaults: `.` `,` `Infinity` `-`
/// `NaN` `%` `‰` `0` `#` `;`.
#[test]
fn the_implied_unnamed_format_has_the_default_values() {
    assert_eq!(
        value("format-number(-1234567.891, '#,##0.00')"),
        "-1,234,567.89"
    );
    assert_eq!(value("format-number(0.5, '#%')"), "50%");
    assert_eq!(value("format-number(0.5, '#\u{2030}')"), "500\u{2030}");
    assert_eq!(value("format-number(1 div 0e0, '#')"), "Infinity");
    assert_eq!(value("format-number(0e0 div 0e0, '#')"), "NaN");
    assert_eq!(value("format-number(-5, '#;(#)')"), "(5)");
}

/// "For any unnamed decimal format, the effective value of each attribute is
/// taken from an `xsl:decimal-format` declaration that is unnamed, and that
/// specifies an explicit value for the required attribute. If there is no such
/// declaration, the default value of the attribute is used."
#[test]
fn every_attribute_sets_its_variable() {
    let declaration = r#"<xsl:decimal-format decimal-separator="," grouping-separator="."
        infinity="INF" minus-sign="~" NaN="nan!" percent="p" per-mille="m"
        zero-digit="&#x660;" digit="!" pattern-separator="|"/>"#;
    // 1234.5 with the Arabic-Indic digits, `.` grouping and `,` decimal.
    assert_eq!(
        value_with(
            declaration,
            "format-number(-1234.5, '!.!!&#x660;,&#x660;&#x660;')"
        ),
        "~\u{661}.\u{662}\u{663}\u{664},\u{665}\u{660}"
    );
    assert_eq!(
        value_with(declaration, "format-number(0.5, '!p')"),
        "\u{665}\u{660}p"
    );
    assert_eq!(
        value_with(declaration, "format-number(0.5, '!m')"),
        "\u{665}\u{660}\u{660}m"
    );
    assert_eq!(
        value_with(declaration, "format-number(1 div 0e0, '!')"),
        "INF"
    );
    assert_eq!(value_with(declaration, "format-number((), '!')"), "nan!");
    assert_eq!(
        value_with(declaration, "format-number(-2, '!|[!]')"),
        "[\u{662}]"
    );
    // The characters that are no longer active are passive: `#` and `0` here
    // are a prefix and a suffix.
    assert_eq!(
        value_with(declaration, "format-number(7, '#!0')"),
        "#\u{667}0"
    );
}

/// "All the `xsl:decimal-format` declarations in a stylesheet that share the
/// same name are grouped into a named decimal format"; the name "is expanded as
/// described in 5.1 Qualified Names" — in the static context of the call, so a
/// different prefix for the same URI finds it (format-number-063).
#[test]
fn a_named_format_is_found_by_its_expanded_name() {
    let declarations = r#"<xsl:decimal-format name="a:test" xmlns:a="urn:a" decimal-separator="," grouping-separator="."/>"#;
    let text = sheet(&format!(
        r#"{declarations}<xsl:template match="/"><out><xsl:value-of xmlns:b="urn:a" select="format-number(12.34, '0.000,00', 'b:test')"/></out></xsl:template>"#
    ));
    assert_eq!(run(&text).unwrap(), "<out>0.012,34</out>");
    // The unnamed format is untouched by a named one.
    assert_eq!(
        value_with(declarations, "format-number(12.34, '0.00')"),
        "12.34"
    );
}

/// §5.1: an unprefixed name "is expanded … the default namespace is not used",
/// so `d` names the format in no namespace even inside `xmlns="urn:d"`.
#[test]
fn an_unprefixed_format_name_is_in_no_namespace() {
    let text = sheet(
        r#"<xsl:decimal-format name="d" decimal-separator=","  grouping-separator="."/>
           <xsl:template match="/"><out xmlns="urn:d"><xsl:value-of select="format-number(1.5, '0,0', 'd')"/></out></xsl:template>"#,
    );
    assert_eq!(run(&text).unwrap(), r#"<out xmlns="urn:d">1,5</out>"#);
}

/// "If there is more than one such declaration, the one with highest import
/// precedence is used" — attribute by attribute, so an importing module's
/// `minus-sign` and an imported module's separators combine (format-number-055,
/// import-0001).
#[test]
fn the_variables_merge_by_import_precedence_one_attribute_at_a_time() {
    let imported = sheet(
        r#"<xsl:decimal-format name="q" grouping-separator="'" decimal-separator=","/>
           <xsl:decimal-format minus-sign="!" percent="p"/>"#,
    );
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:decimal-format name="q" minus-sign="~"/>
           <xsl:decimal-format minus-sign="_"/>
           <xsl:template match="/"><out><xsl:value-of select="format-number(-1931.4857, '0,00', 'q'), format-number(-0.5, '0p')"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///formatting/imported.xsl", &imported)]).unwrap(),
        "<out>~1931,49 _50p</out>"
    );
}

/// \[ERR XTSE1290\] "It is a static error if a named or unnamed decimal format
/// contains two conflicting values for the same attribute in different
/// `xsl:decimal-format` declarations having the same import precedence, unless
/// there is another definition of the same attribute with higher import
/// precedence."
#[test]
fn xtse1290_conflicting_values_at_the_same_precedence() {
    let conflict = r#"<xsl:decimal-format name="d" decimal-separator="."/>
                      <xsl:decimal-format name="d" decimal-separator=":"/>"#;
    assert_eq!(
        static_error(&sheet(&format!("{conflict}<xsl:template match='/'/>"))),
        "XTSE1290"
    );
    // The unnamed format too, and an included module has the includer's
    // precedence.
    let included = sheet(r#"<xsl:decimal-format percent="*"/>"#);
    let main = sheet(
        r#"<xsl:include href="inc.xsl"/><xsl:decimal-format percent="%"/><xsl:template match="/"/>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///formatting/inc.xsl", &included)]),
        Err(Failure::Static("XTSE1290".into()))
    );
    // The same value twice is no conflict (format-number-064, -065).
    assert_eq!(
        value_with(
            r#"<xsl:decimal-format name="d" decimal-separator="," grouping-separator="."/>
               <xsl:decimal-format name="d" decimal-separator="," grouping-separator="."/>"#,
            "format-number(12.34, '000,000', 'd')"
        ),
        "012,340"
    );
    // "unless there is another definition of the same attribute with higher
    // import precedence" (format-number-046).
    let imported = sheet(conflict);
    let main = sheet(
        r#"<xsl:import href="imported.xsl"/>
           <xsl:decimal-format name="d" decimal-separator="," grouping-separator=" "/>
           <xsl:template match="/"><out><xsl:value-of select="format-number(1.5, '0,0', 'd')"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&main, &[("file:///formatting/imported.xsl", &imported)]).unwrap(),
        "<out>1,5</out>"
    );
}

/// \[ERR XTSE1295\] "It is a static error if the character specified in the
/// zero-digit attribute is not a digit or is a digit that does not have the
/// numeric value zero."
#[test]
fn xtse1295_the_zero_digit_must_be_a_digit_zero() {
    for zero in ["2", "$", "a", "&#x661;"] {
        assert_eq!(
            static_error(&sheet(&format!(
                r#"<xsl:decimal-format name="d" zero-digit="{zero}"/><xsl:template match="/"/>"#
            ))),
            "XTSE1295",
            "{zero}"
        );
    }
    // Any Nd zero is one (Osmanya, astral: format-number-051).
    assert_eq!(
        value_with(
            r#"<xsl:decimal-format zero-digit="&#x104A0;"/>"#,
            "format-number(12, '&#x104A0;&#x104A0;&#x104A0;')"
        ),
        "\u{104A0}\u{104A1}\u{104A2}"
    );
}

/// \[ERR XTSE1300\] "It is a static error if, for any named or unnamed decimal
/// format, the variables representing characters used in a picture string do
/// not each have distinct values" — checked on the merged values, whichever
/// declarations they come from.
#[test]
fn xtse1300_the_picture_characters_must_be_distinct() {
    for attributes in [
        r#"zero-digit="0" digit="0""#,
        r#"percent="0" per-mille="0""#,
        r#"pattern-separator="'" decimal-separator="'""#,
        r#"decimal-separator=",""#, // the default grouping-separator is ','
    ] {
        assert_eq!(
            static_error(&sheet(&format!(
                r#"<xsl:decimal-format name="d" {attributes}/><xsl:template match="/"/>"#
            ))),
            "XTSE1300",
            "{attributes}"
        );
    }
    // Two declarations, each harmless alone.
    assert_eq!(
        static_error(&sheet(
            r#"<xsl:decimal-format digit="x"/><xsl:decimal-format percent="x"/><xsl:template match="/"/>"#
        )),
        "XTSE1300"
    );
    // minus-sign, infinity and NaN are not among the seven.
    assert_eq!(
        value_with(
            r##"<xsl:decimal-format minus-sign="." NaN="#" infinity="0"/>"##,
            "format-number(-1, '0')"
        ),
        ".1"
    );
}

/// "In each case the value must be a single character \[see ERR XTSE0020\]"; a
/// `char` is not trimmed (§2.2), so a space is a value and `" ,"` is two
/// characters. `name? = qname` must be a QName.
#[test]
fn xtse0020_single_characters_and_the_name() {
    for attributes in [
        r#"name="d" decimal-separator="must-be-a-char""#,
        r#"name="d" grouping-separator="""#,
        r#"name="d" minus-sign=" -""#,
        r#"name="{concat('f', 'f')}""#,
        r#"name="1d""#,
    ] {
        assert_eq!(
            static_error(&sheet(&format!(
                r#"<xsl:decimal-format {attributes}/><xsl:template match="/"/>"#
            ))),
            "XTSE0020",
            "{attributes}"
        );
    }
    // A space is one character (format-number-024).
    assert_eq!(
        value_with(
            r#"<xsl:decimal-format decimal-separator="," grouping-separator=" "/>"#,
            "format-number(7654321.4857, '### ### ###,#####')"
        ),
        "7 654 321,4857"
    );
    // `infinity` and `NaN` are strings.
    assert_eq!(
        value_with(
            r#"<xsl:decimal-format infinity="off the scale"/>"#,
            "format-number(-1 div 0e0, '#')"
        ),
        "-off the scale"
    );
}

/// §3.2 XTSE0080: "It is a static error to use a reserved namespace in the
/// name of … a decimal-format".
#[test]
fn xtse0080_a_decimal_format_in_a_reserved_namespace() {
    assert_eq!(
        static_error(&sheet(
            r#"<xsl:decimal-format name="xsl:d"/><xsl:template match="/"/>"#
        )),
        "XTSE0080"
    );
}

/// §3.9: an attribute XSLT 2.0 does not define is XTSE0090, unless
/// forwards-compatible behaviour is enabled (format-number-069c).
#[test]
fn an_unknown_attribute_is_xtse0090_unless_forwards_compatible() {
    assert_eq!(
        static_error(&sheet(
            r#"<xsl:decimal-format exponent-separator="E"/><xsl:template match="/"/>"#
        )),
        "XTSE0090"
    );
    let forwards = sheet_versioned(
        "3.0",
        r#"<xsl:decimal-format exponent-separator="E" decimal-separator="," grouping-separator="."/>
           <xsl:template match="/"><out><xsl:value-of select="format-number(1.5, '0,0')"/></out></xsl:template>"#,
    );
    assert_eq!(run(&forwards).unwrap(), "<out>1,5</out>");
}

/// §3.12: a declaration excluded by `use-when` does not exist, so its name is
/// not declared (XTDE1280) and its values do not merge.
#[test]
fn a_declaration_excluded_by_use_when_does_not_count() {
    let declarations = r#"<xsl:decimal-format name="d" use-when="false()"/>
                          <xsl:decimal-format minus-sign="~" use-when="false()"/>"#;
    assert_eq!(value_with(declarations, "format-number(-1, '0')"), "-1");
    assert_eq!(
        error_with(declarations, "format-number(1, '0', 'd')"),
        Failure::Dynamic("XTDE1280".into())
    );
}

// ═══ §16.4 format-number ════════════════════════════════════════════════════

/// \[ERR XTDE1280\] "It is a non-recoverable dynamic error if the name specified
/// as the `$decimal-format-name` argument is not a valid QName, or if its prefix
/// has not been declared in an in-scope namespace declaration, or if the
/// stylesheet does not contain a declaration of a decimal-format with a matching
/// expanded-QName."
#[test]
fn xtde1280_the_decimal_format_name() {
    let declarations = r#"<xsl:decimal-format name="d"/>"#;
    for name in ["'undefined'", "'1d'", "'nope:d'", "'xs:d'", "''"] {
        assert_eq!(
            error_with(declarations, &format!("format-number(1, '0', {name})")),
            Failure::Dynamic("XTDE1280".into()),
            "{name}"
        );
    }
    // A computed name works like a literal one; whitespace around it is not
    // part of the QName.
    assert_eq!(
        value_with(declarations, "format-number(1, '0', concat(' ', 'd '))"),
        "1"
    );
    // The error is dynamic: a call that is never evaluated raises nothing.
    assert_eq!(
        value_with(
            declarations,
            "if (false()) then format-number(1, '0', 'nope') else 'ok'"
        ),
        "ok"
    );
}

/// \[ERR XTDE1310\] "The picture string must conform to the following rules.
/// It is a non-recoverable dynamic error if the picture string does not satisfy
/// these rules" — every rule of §16.4.2 (error-1310a … i, format-number-057,
/// -060).
#[test]
fn xtde1310_every_picture_rule() {
    for picture in [
        "#;#;#",       // "must not contain more than one pattern-separator-sign"
        "#.#.#",       // "more than one decimal-separator-sign"
        "%0.0%",       // "more than one percent-sign or per-mille-sign"
        "%0.0‰",       // "… and it must not contain one of each"
        "fred.ginger", // "at least one digit-sign or zero-digit-sign"
        "#;",          // the negative sub-picture too
        "0$0", // "a passive character … preceded by an active character and … followed by another"
        "00,.0", // "a grouping-separator-sign adjacent to a decimal-separator-sign"
        "0#.00", // "a zero-digit-sign that is followed by a digit-sign"
        "00.##0", // "a digit-sign that is followed by a zero-digit-sign"
    ] {
        assert_eq!(
            error_with("", &format!("format-number(12, '{picture}')")),
            Failure::Dynamic("XTDE1310".into()),
            "{picture}"
        );
    }
    // The pattern-separator of the format in use decides (error-1310a).
    assert_eq!(
        error_with(
            r#"<xsl:decimal-format name="d" pattern-separator="!"/>"#,
            "format-number(12, '#!#!#', 'd')"
        ),
        Failure::Dynamic("XTDE1310".into())
    );
    // Dynamic: not raised by a call that is never evaluated.
    assert_eq!(
        value("if (false()) then format-number(1, '0.#0') else 'ok'"),
        "ok"
    );
}

/// "If the supplied value of the `$value` argument is an empty sequence, the
/// function behaves as if the supplied value were the `xs:double` value NaN",
/// and "If the input number is NaN (not a number), the result is the specified
/// NaN-symbol (with no prefix or suffix)."
#[test]
fn the_empty_sequence_is_nan() {
    assert_eq!(value("format-number((), '###.###')"), "NaN");
    assert_eq!(
        value_with(
            r#"<xsl:decimal-format NaN="none"/>"#,
            "format-number((), '[#]')"
        ),
        "none"
    );
}

/// "The `$value` argument may be of any numeric data type (`xs:double`,
/// `xs:float`, `xs:decimal`, or their subtypes including `xs:integer`). Note
/// that if an `xs:decimal` is supplied, it is not automatically promoted to an
/// `xs:double`, as such promotion can involve a loss of precision."
#[test]
fn decimals_and_integers_are_formatted_exactly() {
    assert_eq!(
        value("format-number(12345678901234567890.12345, '#,##0.00000')"),
        "12,345,678,901,234,567,890.12345"
    );
    assert_eq!(
        value("format-number(123456789012345678901234567890, '0')"),
        "123456789012345678901234567890"
    );
    assert_eq!(
        value("format-number(xs:unsignedLong('18446744073709551615'), '#,###')"),
        "18,446,744,073,709,551,615"
    );
    // As a double the same literal would lose its last digits.
    assert_eq!(
        value("format-number(xs:double(12345678901234567890.12345), '0.00')"),
        "12345678901234567000.00"
    );
}

/// §16.4.4: the adjusted number is converted to the `xs:decimal` "with the
/// smallest possible number of digits" that is "numerically equal to the
/// adjusted number" as a double, then rounded with `round-half-to-even`.
#[test]
fn doubles_floats_and_rounding() {
    for (select, expected) in [
        ("format-number(0.125e0, '0.00')", "0.12"),
        ("format-number(0.375e0, '0.00')", "0.38"),
        ("format-number(2.5e0, '0')", "2"),
        ("format-number(3.5e0, '0')", "4"),
        ("format-number(0.125, '0.00')", "0.12"),
        ("format-number(1e20, '#')", "100000000000000000000"),
        ("format-number(1e-10, '0.0000000000##')", "0.0000000001"),
        ("format-number(1e-10, '0.00')", "0.00"),
        (
            "format-number(1e100 div 3, '0')",
            &format!("{}{}", "3".repeat(16), "0".repeat(84)),
        ),
        ("format-number(xs:float('0.1'), '0.##########')", "0.1"),
        ("format-number(xs:float('-1.5'), '0')", "-2"),
        ("format-number(239236.588, '00000.00')", "239236.59"),
    ] {
        assert_eq!(value(select), expected, "{select}");
    }
    let largest = value("format-number(1.7976931348623157e308, '0')");
    assert!(
        largest.starts_with("17976931348623157") && largest.len() == 309,
        "{largest}"
    );
}

/// "Negative zero is taken as negative, positive zero as positive."
#[test]
fn negative_zero_takes_the_negative_sub_picture() {
    assert_eq!(value("format-number(-0e0, '0')"), "-0");
    assert_eq!(value("format-number(-0e0, '0;(0)')"), "(0)");
    assert_eq!(value("format-number(0e0, '0;(0)')"), "0");
    // The sign of the input number decides, not that of the rounded number.
    assert_eq!(value("format-number(-0.001, '0.00')"), "-0.00");
}

/// "If the sub-picture contains a percent-sign, the number is multiplied by
/// 100. If the sub-picture contains a per-mille-sign, the number is multiplied
/// by 1000."
#[test]
fn percent_and_per_mille() {
    assert_eq!(value("format-number(0.4857, '###.###%')"), "48.57%");
    assert_eq!(
        value("format-number(0.4857, '###.###&#x2030;')"),
        "485.7\u{2030}"
    );
    assert_eq!(value("format-number(0.25, '00%')"), "25%");
    assert_eq!(value("format-number(-0.25, '%0')"), "-%25");
}

/// The analysis of §16.4.3: grouping positions (regular and not), the minimum
/// integer part size (one when the sub-picture has no zero-digit-sign and no
/// decimal-separator-sign), prefix and suffix, and the negative prefix that
/// is "the minus-sign character and the prefix for the positive sub-picture".
#[test]
fn the_picture_analysis() {
    for (select, expected) in [
        (
            "format-number(987654321, '###,##0,00.00')",
            "9876,543,21.00",
        ),
        (
            "format-number(1E25, '#,######')",
            "10,000000,000000,000000,000000",
        ),
        ("format-number(1234567, '#,##,#0')", "1,23,45,67"),
        ("format-number(0.123456, '0.000,000')", "0.123,456"),
        ("format-number(0, '#')", "0"),
        ("format-number(0.0, '#')", "0"),
        ("format-number(0.5, '#.#')", ".5"),
        (
            "format-number(-26931.4, 'zzz-###,###.###')",
            "-zzz-26,931.4",
        ),
        (
            "format-number(-26931.4, '###,###.###;###,###.###')",
            "26,931.4",
        ),
        (
            "format-number(2.14*86.58, 'PREFIX##00.000###SUFFIX')",
            "PREFIX185.2812SUFFIX",
        ),
        ("format-number(1, '00.0')", "01.0"),
    ] {
        assert_eq!(value(select), expected, "{select}");
    }
}

/// XPath 2.0 §3.1.5 for `$value as numeric?`: an untyped node is atomized and
/// cast to `xs:double`; a string is not numeric (XPTY0004), nor is a sequence
/// of two numbers; an untyped value that is not a number is FORG0001.
#[test]
fn the_value_argument_conversion() {
    let text = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="format-number(doc/n, '#,##0.00')"/></out></xsl:template>"#,
    );
    assert_eq!(run(&text).unwrap(), "<out>1,234.50</out>");
    assert_eq!(
        error_with("", "format-number('12', '0')"),
        Failure::Dynamic("XPTY0004".into())
    );
    assert_eq!(
        error_with("", "format-number((1, 2), '0')"),
        Failure::Dynamic("XPTY0004".into())
    );
    assert_eq!(
        error_with("", "format-number(xs:untypedAtomic('twelve'), '0')"),
        Failure::Dynamic("FORG0001".into())
    );
    // A picture held in a temporary tree is atomized to a string
    // (data-manipulation-012).
    let text = sheet(
        r#"<xsl:variable name="p">#,###.00</xsl:variable>
           <xsl:template match="/"><out><xsl:value-of select="format-number(1234.78, $p)"/></out></xsl:template>"#,
    );
    assert_eq!(run(&text).unwrap(), "<out>1,234.78</out>");
}

/// §3.8 backwards-compatible behaviour: XPath 1.0 compatibility mode converts
/// the first argument with `fn:number`, so a string that is not a number is NaN
/// (format-number-014, -035) and the empty sequence too (math-3317).
#[test]
fn backwards_compatible_behaviour_converts_with_number() {
    let text = sheet_versioned(
        "1.0",
        r#"<xsl:decimal-format NaN="non-numeric"/>
           <xsl:template match="/"><out><xsl:value-of select="format-number('foo', '#')"/>|<xsl:value-of select="format-number('12.5', '0.0')"/>|<xsl:value-of select="format-number(() + (), '#.##')"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&text).unwrap(),
        "<out>non-numeric|12.5|non-numeric</out>"
    );
}

/// §18.1.2: `function-available` "returns true if and only if there is a
/// function available with the given name and arity" — `format-number` at
/// arities 2 and 3, not 1 or 4. §18.2.2: `element-available` "returns true if
/// and only if the expanded-QName is the name of an instruction";
/// `xsl:decimal-format` is a declaration.
#[test]
fn format_number_is_available_and_decimal_format_is_no_instruction() {
    assert_eq!(
        value(
            "function-available('format-number'), function-available('format-number', 2), \
             function-available('format-number', 3), function-available('format-number', 1), \
             function-available('format-number', 4)"
        ),
        "true true true false false"
    );
    assert_eq!(value("element-available('xsl:decimal-format')"), "false");
}

/// A dynamic error is not a static one: a stylesheet whose only bad picture is
/// in a template that never runs still runs (§2.9).
#[test]
fn a_bad_picture_in_a_template_that_never_runs() {
    let text = sheet(
        r#"<xsl:template match="/"><out>ok</out></xsl:template>
           <xsl:template match="never"><xsl:value-of select="format-number(1, 'abc', 'nope')"/></xsl:template>"#,
    );
    assert_eq!(run(&text).unwrap(), "<out>ok</out>");
    let text = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="format-number(1, 'abc')"/></out></xsl:template>"#,
    );
    assert_eq!(dynamic_error(&text), "XTDE1310");
}

// ═══ §16.5 Formatting dates and times ══════════════════════════════════════

/// §16.5.3 "Example: Gregorian Calendar" — every example in English, with the
/// spec's `$d` = 2002-12-31, `$t` = 15:58:45.762 and `$dt` their combination.
/// (The two `PDT` examples are at the end: the names ignore daylight saving
/// time, so −07:00 is `MST`, its standard-time name.)
#[test]
fn the_specs_gregorian_examples() {
    let d = "xs:date('2002-12-31')";
    let t = "xs:time('15:58:45.762+02:00')";
    let dt = "xs:dateTime('2002-12-31T15:58:45.762+02:00')";
    for (select, expected) in [
        (
            format!("format-date({d}, '[Y0001]-[M01]-[D01]')"),
            "2002-12-31",
        ),
        (format!("format-date({d}, '[M]-[D]-[Y]')"), "12-31-2002"),
        (format!("format-date({d}, '[D]-[M]-[Y]')"), "31-12-2002"),
        (format!("format-date({d}, '[D1] [MI] [Y]')"), "31 XII 2002"),
        (
            format!("format-date({d}, '[D1o] [MNn], [Y]', 'en', (), ())"),
            "31st December, 2002",
        ),
        (
            format!("format-date({d}, '[D01] [MN,*-3] [Y0001]', 'en', (), ())"),
            "31 DEC 2002",
        ),
        (
            format!("format-date({d}, '[MNn] [D], [Y]', 'en', (), ())"),
            "December 31, 2002",
        ),
        (
            format!("format-date({d}, '[[[Y0001]-[M01]-[D01]]]')"),
            "[2002-12-31]",
        ),
        (
            "format-date(xs:date('2003-05-05'), '[YWw]', 'en', (), ())".to_string(),
            "Two Thousand and Three",
        ),
        (
            format!("format-time({t}, '[h]:[m01] [PN]', 'en', (), ())"),
            "3:58 PM",
        ),
        (
            format!("format-time({t}, '[h]:[m01]:[s01] [Pn]', 'en', (), ())"),
            "3:58:45 pm",
        ),
        (format!("format-time({t}, '[H01]:[m01]')"), "15:58"),
        (
            format!("format-time({t}, '[H01]:[m01]:[s01].[f001]')"),
            "15:58:45.762",
        ),
        (
            format!("format-time({t}, '[H01]:[m01]:[s01] [z,6-6]', 'en', (), ())"),
            "15:58:45 GMT+02:00",
        ),
        (
            format!("format-dateTime({dt}, '[h].[m01][Pn] on [FNn], [D1o] [MNn]')"),
            "3.58pm on Tuesday, 31st December",
        ),
        (
            format!("format-dateTime({dt}, '[M01]/[D01]/[Y0001] at [H01]:[m01]:[s01]')"),
            "12/31/2002 at 15:58:45",
        ),
        // The `PDT` examples ("Component specifier "Z" with the presentation
        // modifier "N" is used to request timezone names such as "PST" or
        // "CET""), with −07:00 and −08:00 values.
        (
            "format-time(xs:time('15:58:45.762-07:00'), '[h]:[m01]:[s01] [PN] [ZN,*-3]', 'en', (), ())"
                .to_string(),
            "3:58:45 PM MST",
        ),
        (
            "format-time(xs:time('15:58:45.762-08:00'), &quot;[h]:[m01]:[s01] o'clock [PN] [ZN,*-3]&quot;, 'en', (), ())"
                .to_string(),
            "3:58:45 o'clock PM PST",
        ),
    ] {
        assert_eq!(value(&select), expected, "{select}");
    }
}

/// \[ERR XTDE1340\] "It is a non-recoverable dynamic error if the syntax of the
/// picture is incorrect" — an unknown component, an unclosed or empty marker,
/// a single `]` in a literal ("it must be doubled"), a bad width modifier ("Both
/// integers, if present, must be greater than zero").
#[test]
fn xtde1340_a_bad_picture() {
    // `[Y,3-2]`: "min-width is … the minimum number of characters to be
    // output, … max-width is … the maximum number of characters to be output";
    // no output is both, so the picture is invalid.
    for picture in [
        "[bla]", "[]", "[Y", "]", "[Y,0]", "[Y,2-x]", "[Y[M]]", "[Y,3-2]",
    ] {
        for function in [
            "format-date(xs:date('2003-12-07'), ",
            "format-dateTime(xs:dateTime('2003-12-07T09:15:06'), ",
        ] {
            assert_eq!(
                error_with("", &format!("{function}'{picture}')")),
                Failure::Dynamic("XTDE1340".into()),
                "{function}{picture}"
            );
        }
    }
    assert_eq!(
        error_with(
            "",
            "format-time(xs:time('09:15:06'), '[bla]', 'en', (), ())"
        ),
        Failure::Dynamic("XTDE1340".into())
    );
    // "If an opening or closing square bracket is required within a literal
    // substring, it must be doubled"; "Whitespace within a variable marker is
    // ignored."
    assert_eq!(
        value("format-date(xs:date('2003-12-07'), '[[ [ Y 0001 ] ]]')"),
        "[ 2003 ]"
    );
    // format-date-036: an empty picture, spaces, escapes alone.
    assert_eq!(
        value("concat(format-date(current-date(), ''), format-time(current-time(), '  '), format-dateTime(current-dateTime(), '[[QQQ]]'))"),
        "  [QQQ]"
    );
}

/// \[ERR XTDE1350\] "It is a non-recoverable dynamic error if a component
/// specifier within the picture refers to components that are not available in
/// the given type of $value, for example if the picture supplied to the
/// format-time refers to the year, month, or day component."
#[test]
fn xtde1350_a_component_the_value_does_not_have() {
    for component in ["H", "h", "P", "m", "s", "f"] {
        assert_eq!(
            error_with(
                "",
                &format!("format-date(xs:date('2003-12-07'), '[{component}]')")
            ),
            Failure::Dynamic("XTDE1350".into()),
            "{component}"
        );
    }
    for component in ["Y", "M", "D", "d", "F", "W", "w", "E"] {
        assert_eq!(
            error_with(
                "",
                &format!("format-time(xs:time('09:15:06'), '[{component}]')")
            ),
            Failure::Dynamic("XTDE1350".into()),
            "{component}"
        );
    }
    // "It is not an error to include a timezone component when the supplied
    // value has no timezone. In these circumstances the timezone component will
    // be ignored."
    assert_eq!(
        value("format-date(xs:date('2003-12-07'), '[D][Z][z]')"),
        "7"
    );
    assert_eq!(
        value("format-date(xs:date('2003-12-07-05:00'), '[D] [Z] [z]')"),
        "7 -05:00 GMT-05:00"
    );
    // A dateTime has every component.
    assert_eq!(
        value("format-dateTime(xs:dateTime('2003-12-07T09:15:06.5Z'), '[Y][M][D][d][F1][W][w][H][h][P][m][s][f][Z][E]')"),
        "2003127341749199am15065+00:00ad"
    );
}

/// "If $value is the empty sequence, the empty sequence is returned." — even
/// before the picture is looked at.
#[test]
fn the_empty_sequence_gives_the_empty_sequence() {
    assert_eq!(
        value("count(format-date((), '[Y]')), count(format-time((), '[H]', 'en', (), ())), count(format-dateTime((), '[bla]'))"),
        "0 0 0"
    );
}

/// XPath 2.0 §3.1.5 for `$value as xs:date?`: an untyped value is cast to
/// `xs:date` (copy-of-005); a value of any other type is XPTY0004 — there is no
/// promotion from `xs:dateTime` or `xs:string`.
#[test]
fn the_value_argument_conversion_for_dates() {
    assert_eq!(
        value("format-date(xs:untypedAtomic('2003-12-07'), '[D] [MNn]')"),
        "7 December"
    );
    assert_eq!(
        error_with("", "format-date(xs:untypedAtomic('7 Dec'), '[D]')"),
        Failure::Dynamic("FORG0001".into())
    );
    for select in [
        "format-date(xs:dateTime('2003-12-07T00:00:00'), '[D]')",
        "format-date('2003-12-07', '[D]')",
        "format-time(xs:date('2003-12-07'), '[H]')",
        "format-dateTime(xs:date('2003-12-07'), '[D]')",
        "format-date((xs:date('2003-12-07'), xs:date('2003-12-08')), '[D]')",
    ] {
        assert_eq!(
            error_with("", select),
            Failure::Dynamic("XPTY0004".into()),
            "{select}"
        );
    }
}

/// §16.5.2: "If the fallback representation uses a different language from that
/// requested, the output string must identify the language actually used, for
/// example by prefixing the string with \[Language: Y\]" (format-date-en-032);
/// an absent, empty or invalid language is the default, English, with no prefix.
#[test]
fn a_language_other_than_english_falls_back_with_a_prefix() {
    let d = "xs:date('2006-03-01')";
    for (language, expected) in [
        ("'xib'", "[Language: en]March"),
        ("'de'", "[Language: en]March"),
        ("'en'", "March"),
        ("'en-GB'", "March"),
        ("'EN'", "March"),
        ("()", "March"),
        ("''", "March"),
        ("'not a language'", "March"),
    ] {
        assert_eq!(
            value(&format!("format-date({d}, '[MNn]', {language}, (), ())")),
            expected,
            "{language}"
        );
    }
}

/// §16.5.2: "If the fallback representation uses a different calendar from that
/// requested, the output string must identify the calendar actually used, for
/// example by prefixing the string with \[Calendar: X\]" (format-date-en-033).
/// `AD` and `ISO` are supported; any other designator of the table, and any
/// prefixed calendar name ("the behavior in this case is
/// implementation-defined"), falls back to `AD`.
#[test]
fn an_unsupported_calendar_falls_back_with_a_prefix() {
    let d = "xs:date('2006-03-01')";
    for (calendar, expected) in [
        ("'CB'", "[Calendar: AD]03"),
        ("'OS'", "[Calendar: AD]03"),
        ("'CE'", "[Calendar: AD]03"),
        ("'AD'", "03"),
        ("'ISO'", "03"),
        ("()", "03"),
        ("'my:calendar'", "[Calendar: AD]03"),
    ] {
        assert_eq!(
            value_with_namespaces(&format!("format-date({d}, '[M01]', 'en', {calendar}, ())")),
            expected,
            "{calendar}"
        );
    }
    // Both fallbacks at once.
    assert_eq!(
        value(&format!("format-date({d}, '[M01]', 'de', 'AH', ())")),
        "[Calendar: AD][Language: en]03"
    );
    // format-date-037: an unprefixed calendar name is a designator even where a
    // default namespace is in scope.
    let text = sheet(
        r#"<xsl:template match="/"><out xmlns="urn:opf"><xsl:value-of select="format-date(xs:date('2006-03-01'), '[Y0001]', 'en', 'AD', ())"/></out></xsl:template>"#,
    );
    assert_eq!(run(&text).unwrap(), r#"<out xmlns="urn:opf">2006</out>"#);
}

/// The same with a namespace `my` in scope.
fn value_with_namespaces(select: &str) -> String {
    let text = sheet(&format!(
        r#"<xsl:template match="/"><out xmlns:my="urn:my"><xsl:value-of select="{select}"/></out></xsl:template>"#
    ));
    let out = run(&text).unwrap_or_else(|f| panic!("{select}: {f:?}"));
    unwrap_out(out.replace(r#" xmlns:my="urn:my""#, ""))
}

/// "A calendar value must be a valid QName. If the QName does not have a
/// prefix, then it identifies a calendar with the designator specified below"
/// and "the use of an invalid … calendar … argument is classified as a
/// non-recoverable dynamic error" — XTDE1340, §16.5's code for its arguments
/// (F&O 3.0 renamed it FOFD1340 and gives it this case).
#[test]
fn an_invalid_calendar_is_a_dynamic_error() {
    for calendar in ["'Gregorian'", "'ad'", "'1AD'", "'nope:cal'", "'a b'"] {
        assert_eq!(
            error_with(
                "",
                &format!("format-date(xs:date('2006-03-01'), '[M01]', 'en', {calendar}, ())")
            ),
            Failure::Dynamic("XTDE1340".into()),
            "{calendar}"
        );
    }
}

/// The ISO calendar (§16.5.2): "in the ISO calendar the days of the week are
/// numbered from 1 (Monday) to 7 (Sunday), and week 1 in any calendar year is
/// the week (from Monday to Sunday) that includes the first Thursday of that
/// year" (format-date-009, -010); "The era ("E" component) with this calendar is
/// either a minus sign (for negative years) or a zero-length string".
#[test]
fn the_iso_calendar() {
    for (date, expected) in [
        ("2004-01-01", "1 4"),
        ("2005-01-01", "53 6"),
        ("2006-01-01", "52 7"),
        ("2007-01-01", "1 1"),
        ("2004-12-31", "53 5"),
    ] {
        assert_eq!(
            value(&format!(
                "format-date(xs:date('{date}'), '[W] [F1]', (), 'ISO', ())"
            )),
            expected,
            "{date}"
        );
    }
    assert_eq!(
        value("format-date(xs:date('-0055-12-01'), '[E][Y0001]', (), 'ISO', ())"),
        "-0055"
    );
}

/// "Y year (absolute value)", and the era "E": `-0055` is 55 BC in XML Schema
/// 1.0, which has no year zero (format-date-en-029).
#[test]
fn years_before_the_christian_era() {
    assert_eq!(
        value("format-date(xs:date('1990-12-01'), '[Y][EN]', 'en', (), ()), format-date(xs:date('-0055-12-01'), '[Y][EN]', 'en', (), ())"),
        "1990AD 55BC"
    );
    // The weekdays follow the upstream date arithmetic, which has no year zero
    // either: the day before 0001-01-01 (a Monday in the proleptic Gregorian
    // calendar) is -0001-12-31, a Sunday, the last day of a leap year (1 BC).
    assert_eq!(
        value("format-date(xs:date('0001-01-01'), '[FNn]'), format-date(xs:date('0001-01-01') - xs:dayTimeDuration('P1D'), '[Y] [FNn] [d] [E]')"),
        "Monday 1 Sunday 366 bc"
    );
    // A lexical year 0000, which the upstream parser accepts (XML Schema 1.1:
    // "a year value of zero represents 1 BCE"), is 1 BC as well: the same
    // year, era and days as -0001 and as the day before 0001-01-01.
    for (date, expected) in [
        ("xs:date('0001-01-01')", "1 AD Monday 1"),
        ("xs:date('0000-12-31')", "1 BC Sunday 366"),
        ("xs:date('-0001-12-31')", "1 BC Sunday 366"),
        (
            "xs:date('0001-01-01') - xs:dayTimeDuration('P1D')",
            "1 BC Sunday 366",
        ),
        ("xs:date('0000-01-01')", "1 BC Saturday 1"),
    ] {
        assert_eq!(
            value(&format!("format-date({date}, '[Y] [EN] [FNn] [d]')")),
            expected,
            "{date}"
        );
    }
}

/// The fractional seconds in XSLT 2.0: "the value is rounded to the specified
/// size as if by applying the function round-half-to-even(fractional-seconds,
/// max-width)" (format-date-002, -003, -013 — XSLT 3.0 truncates instead,
/// format-date-002a).
#[test]
fn fractional_seconds_are_rounded() {
    let t = "xs:time('09:15:06.456')";
    for (picture, expected) in [
        ("[H]:[m]:[s].[f,1-1]", "9:15:06.5"),
        ("[H]:[m]:[s].[f1,1-1]", "9:15:06.5"),
        ("[H]:[m]:[s].[f01]", "9:15:06.46"),
        ("[H]:[m]:[s].[f001]", "9:15:06.456"),
        ("[f,4-4]", "4560"),
        ("[f,*-2]", "46"),
        ("[f]", "456"),
    ] {
        assert_eq!(
            value(&format!("format-time({t}, '{picture}')")),
            expected,
            "{picture}"
        );
    }
    // A rounding that carries out of the fraction (round-half-to-even(0.96,
    // 1) is 1.0): the fractional part of the rounded value, zero; the seconds
    // are not changed (Saxon 9.1 agrees).
    for (time, expected) in [
        ("09:05:07.96", "07.0 07.96 07.960"),
        ("09:05:07.995", "07.0 07.00 07.995"),
        ("09:05:07.04", "07.0 07.04 07.040"),
    ] {
        assert_eq!(
            value(&format!(
                "for $p in ('[s01].[f,1-1]', '[s01].[f,2-2]', '[s01].[f001]') return format-time(xs:time('{time}'), $p)"
            )),
            expected,
            "{time}"
        );
    }
}

/// The width modifier (§16.5.1): "A format token containing leading zeroes,
/// such as 001, sets the minimum and maximum width …; if a width modifier is
/// also present, then the width modifier takes precedence"; the year drops its
/// high-order digits; a name is abbreviated ("using either conventional
/// abbreviations if available, or crude right-truncation if not").
#[test]
fn width_modifiers() {
    let dt = "xs:dateTime('0985-03-01T09:15:06.456')";
    for (picture, expected) in [
        ("[Y,4-4]", "0985"),
        ("[Y,2-2]", "85"),
        ("[Y01]", "85"),
        ("[M,3]", "003"),
        ("[MN,3-3]", "MAR"),
        ("[MNn,*-3]", "Mar"),
        // 0985-03-01 was a Tuesday (proleptic Gregorian).
        ("[FNn,3-3]", "Tue"),
        ("[FNn,2-2]", "Tu"),
        ("[Yi,4-4]", "cmlxxxv"),
        // A name is padded only when its full form is shorter than the
        // minimum; an abbreviation is the conventional one if it is no shorter
        // than the minimum, else right-truncation to the maximum ("setting
        // max-width to 4 indicates that four-letter abbreviations should be
        // used").
        ("[FNn,4-4]", "Tues"),
        ("[MNn,4-4]", "Marc"),
        ("[FNn,5-6]", "Tuesda"),
        ("[MNn,3-3]", "Mar"),
        ("[MNn]", "March"),
        ("[MNn,6-6]", "March "),
        // A width modifier counts characters, an ordinal's suffix included.
        ("[D1o,2]", "1st"),
        ("[D1o,3-3]", "1st"),
        ("[D1o,4]", "01st"),
        ("[D01o]", "01st"),
        // "if a width modifier is also present, then the width modifier takes
        // precedence" over the token's own width.
        ("[Y0001,2-2]", "85"),
        ("[D01,1]", "1"),
        ("[f001,1-1]", "5"),
    ] {
        assert_eq!(
            value(&format!("format-dateTime({dt}, '{picture}')")),
            expected,
            "{picture}"
        );
    }
}

/// The first presentation modifier is "any format token permitted in the format
/// string of the xsl:number instruction", or `n`, `N`, `Nn`; the second is `o`
/// (ordinal) or `t` (traditional); "If the implementation does not support the
/// use of the requested format token, it must use the default presentation
/// modifier for that component."
#[test]
fn presentation_modifiers() {
    let d = "xs:date('2003-12-21')";
    for (picture, expected) in [
        ("[D1o]", "21st"),
        ("[Dwo]", "twenty-first"),
        ("[DWwo]", "Twenty-First"),
        ("[Dw]", "twenty one"),
        ("[DW]", "TWENTY ONE"),
        ("[DI]", "XXI"),
        ("[Da]", "u"),
        // ① … ⑳ continue as ㉑ … ㊿ (numbering's circled-digit sequence).
        ("[D&#x2460;]", "\u{3251}"),
        ("[D&#x661;]", "\u{662}\u{661}"),
        ("[D01t]", "21"),
        // Not supported: the component's default.
        ("[DNn]", "21"),
        ("[D&#x3b1;]", "21"),
        ("[MNn]", "December"),
        ("[Mn]", "december"),
        ("[M1]", "12"),
        ("[F]", "sunday"),
        ("[FNn]", "Sunday"),
        ("[F1]", "7"),
        ("[E]", "ad"),
        ("[ENn]", "Ad"),
        ("[C]", "ad"),
        ("[CN]", "AD"),
    ] {
        assert_eq!(
            value(&format!("format-date({d}, '{picture}')")),
            expected,
            "{picture}"
        );
    }
    let t = "xs:time('00:05:00')";
    for (picture, expected) in [
        ("[h] [P]", "12 am"),
        ("[H01]:[m]", "00:05"),
        ("[mA]", "E"),
        ("[PN,2-2]", "AM"),
        ("[PNn,1-1]", "A"),
    ] {
        assert_eq!(
            value(&format!("format-time({t}, '{picture}')")),
            expected,
            "{picture}"
        );
    }
}

/// §18.1.2: the three date formatting functions are available at arities 2 and
/// 5, and only there.
#[test]
fn the_date_formatting_functions_are_available() {
    for name in ["format-date", "format-time", "format-dateTime"] {
        assert_eq!(
            value(&format!(
                "for $n in (1 to 6) return function-available('{name}', $n)"
            )),
            "false true false false true false",
            "{name}"
        );
    }
}
