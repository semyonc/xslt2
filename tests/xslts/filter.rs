//! Dependency policy (design sketch §2.5) and the explicit exclusion list.
//!
//! A test case is *applicable* when its effective `spec` dependency admits an XSLT 2.0
//! processor. An applicable case is *runnable* when every other dependency is satisfied by
//! this implementation. Unknown dependency kinds are reported, never silently run.

use crate::catalog::{Dependency, EnvRef, TestCase, TestSet};

#[derive(Debug, Clone)]
pub struct Policy {
    /// `--schema-aware`: the engine is a schema-aware processor (§21.2).
    pub schema_aware: bool,
    /// XSD 1.1 schemas available (SchemaSet::xsd11) in schema-aware mode.
    pub xsd11: bool,
    /// `--strict`: recoverable errors are signalled (e.g. `on-multiple-match=error`).
    pub strict_recoverable: bool,
    /// `--all`: run cases on the exclusion list too.
    pub run_excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applicability {
    Run,
    /// Effective `spec` dependency excludes XSLT 2.0 processors (value given).
    NotXslt20(String),
    /// Applicable, but a dependency is not satisfied (reason given).
    Unsatisfied(String),
    /// On the exclusion list (reason given).
    Excluded(&'static str),
}

/// `spec` values that admit an XSLT 2.0 processor. `XSLT20` = 2.0 only (a 3.0 processor has a
/// twin test); `XSLT30+`, `XSLT10` do not. Absence of a `spec` dependency means any version.
pub const APPLICABLE_SPECS: &[&str] = &["XSLT10+", "XSLT20", "XSLT20+", "XSLT10 XSLT20"];

/// Cases excluded with a documented reason (W3C-disputed, or contradicting the 2.0 spec).
/// Keep every entry justified; `--all` runs them anyway. Empty at M0.
pub const EXCLUDED: &[(&str, &str)] = &[];

/// The last `spec` dependency wins (test-set level first, then test-case level).
pub fn effective_spec<'a>(set: &'a TestSet, case: &'a TestCase) -> Option<&'a str> {
    set.dependencies
        .iter()
        .chain(case.dependencies.iter())
        .rev()
        .find(|d| d.kind == "spec")
        .map(|d| d.value.as_str())
}

pub fn applicability(set: &TestSet, case: &TestCase, policy: &Policy) -> Applicability {
    if let Some(spec) = effective_spec(set, case) {
        if !APPLICABLE_SPECS.contains(&spec) {
            return Applicability::NotXslt20(spec.to_string());
        }
    }
    if !policy.run_excluded {
        if let Some((_, reason)) = EXCLUDED.iter().find(|(n, _)| *n == case.name) {
            return Applicability::Excluded(reason);
        }
    }
    // Test shapes that only exist in XSLT 3.0, whatever the spec dependency says.
    let env_has_package = match &case.environment {
        Some(EnvRef::Inline(e)) => e.has_package,
        Some(EnvRef::Ref(r)) => set.environments.get(r).map(|e| e.has_package).unwrap_or(false),
        None => false,
    };
    if case.test.has_package || env_has_package {
        return Applicability::Unsatisfied("XSLT 3.0 packages".into());
    }
    if case.test.has_posture_and_sweep {
        return Applicability::Unsatisfied("streaming: posture-and-sweep test".into());
    }
    if case.test.initial_function.is_some() {
        return Applicability::Unsatisfied("initial-function is XSLT 3.0".into());
    }
    for d in set.dependencies.iter().chain(case.dependencies.iter()) {
        if d.kind == "spec" {
            continue;
        }
        match dependency_supported(d, policy) {
            Some(true) if !d.satisfied => {
                return Applicability::Unsatisfied(format!("{}={} is supported but the test needs its absence", d.kind, d.value));
            }
            Some(false) if d.satisfied => {
                return Applicability::Unsatisfied(format!("{}={} not supported", d.kind, d.value));
            }
            None => return Applicability::Unsatisfied(format!("unknown dependency {}={}", d.kind, d.value)),
            _ => {}
        }
    }
    Applicability::Run
}

/// Whether this implementation satisfies a (non-`spec`) dependency. `None` = unknown kind.
/// This table is the single place to flip as features land.
fn dependency_supported(d: &Dependency, policy: &Policy) -> Option<bool> {
    let v = d.value.as_str();
    Some(match d.kind.as_str() {
        "feature" => match v {
            // Implemented by M3 (serialization, BC mode, d-o-e) or given by the XPath engine.
            "serialization" | "backwards_compatibility" | "namespace_axis" | "disabling_output_escaping" | "built_in_derived_types" => true,
            "schema_aware" => policy.schema_aware,
            "XSD_1.1" => policy.schema_aware && policy.xsd11,
            // dtd, XML_1.1, streaming, streaming-fallback, higher_order_functions, dynamic_evaluation,
            // xquery_invocation, HTML4, HTML5, xsl-stylesheet-processing-instruction: not planned for v1.
            _ => false,
        },
        // regexml uses ICU4X Unicode data (≥ 9.0); verify against misc/unicode-90 once the engine runs.
        "unicode-version" => true,
        // chrono-backed xs:date/xs:dateTime handle negative years and years above 9999.
        "year_component_values" => true,
        // unicode-normalization: NFC, NFD, NFKC, NFKD.
        "additional_normalization_form" => true,
        "default_output_encoding" => v.eq_ignore_ascii_case("UTF-8"),
        // Q28: non-UTF encodings for unparsed-text() are not planned for the first cut.
        "unparsed_text_encoding" => v.eq_ignore_ascii_case("UTF-8") || v.eq_ignore_ascii_case("UTF-16"),
        // Q23: English-only xsl:number words/ordinals in v1.
        "default_language_for_numbering" | "languages_for_numbering" => v == "en",
        // Q24: Gregorian ("AD") and ISO calendars only.
        "supported_calendars_in_date_formatting_functions" | "default_calendar_in_date_formatting_functions" => v == "AD" || v == "ISO",
        // XTRE0540 policy: recover by default, signal in --strict mode.
        "on-multiple-match" => match v {
            "error" => policy.strict_recoverable,
            "recover" => !policy.strict_recoverable,
            _ => false,
        },
        // xs:decimal is rust_decimal (28 significant digits).
        "maximum_number_of_decimal_digits" => v.parse::<u32>().map(|n| n <= 28).unwrap_or(false),
        // Unicode digit families for xsl:number (M3 decides), CLDR ordinals, streaming, packages,
        // extension functions, non-codepoint collations, HTML versions: not supported now.
        "combinations_for_numbering"
        | "ordinal_scheme_name"
        | "sweep_and_posture"
        | "detect_accumulator_cycles"
        | "enable_assertions"
        | "package_version_resolution"
        | "extension-function"
        | "available_documents"
        | "recognize_id_as_uri_fragment"
        | "ignore_doc_failure"
        | "collation_uri"
        | "statically_known_collations"
        | "default_html_version" => false,
        _ => return None,
    })
}

/// Execution profile of a runnable case, for per-profile reporting.
pub fn profile_of(set: &TestSet, case: &TestCase) -> &'static str {
    let has = |name: &str| {
        set.dependencies
            .iter()
            .chain(case.dependencies.iter())
            .any(|d| d.kind == "feature" && d.value == name && d.satisfied)
    };
    if has("schema_aware") {
        "schema-aware"
    } else if has("backwards_compatibility") {
        "backwards-compatible"
    } else if has("serialization") {
        "serialization"
    } else {
        "basic"
    }
}
