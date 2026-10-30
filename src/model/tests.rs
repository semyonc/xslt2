//! Parts of the compiled stylesheet read directly: the table of stylesheet
//! functions (§10.3), moved from the integration tests, and what a compiled
//! `xsl:analyze-string` regular expression tells (§15.1), formerly a doctest
//! of `RegexSource`. Both became crate-internal with the instruction tree.

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;

use crate::model::RegexSource;
use crate::regex::CompiledRegex;
use crate::{Compiler, QName, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

/// Wraps top-level content in a `version="2.0"` stylesheet with the `xs` and
/// `f` prefixes bound.
fn sheet(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
             xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f"
             exclude-result-prefixes="xs f">{body}</xsl:stylesheet>"#
    )
}

/// The stack the transformations run on: a debug build's frames are large,
/// and a call through the XPath engine nests several of them.
const TEST_STACK: usize = 256 * 1024 * 1024;

/// A serialized primary result without the default XML declaration
/// `serialize_primary` writes (§20, `omit-xml-declaration` defaults to `no`).
fn body(serialized: String) -> String {
    match serialized.strip_prefix("<?xml version=\"1.0\" encoding=\"UTF-8\"?>") {
        Some(rest) => rest.to_string(),
        None => serialized,
    }
}

/// The table of stylesheet functions belongs to one compilation: a function
/// set reused for a second stylesheet does not see the first one's functions,
/// and each compiled stylesheet keeps calling its own (the function set is caller-owned
/// and may serve many compilations). Stylesheets A and B
/// mint the same handle numbers for different functions — A: `f:g#0`, `f:h#1`;
/// B: `f:k#0`, `f:g#1` — and are run in the order A, B, A after both are
/// compiled.
#[test]
fn a_function_set_reused_across_compilations_keeps_them_apart() {
    crate::run_on_large_stack(TEST_STACK, || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let first = Compiler::new(&names, &functions)
            .compile_str(
                &sheet(
                    r#"<xsl:function name="f:g"><xsl:sequence select="'A:g0'"/></xsl:function>
                       <xsl:function name="f:h"><xsl:param name="x"/><xsl:sequence select="concat('A:h1:', $x)"/></xsl:function>
                       <xsl:template match="/"><out><xsl:value-of select="f:g(), f:h('x')"/></out></xsl:template>"#,
                ),
                "urn:first",
            )
            .unwrap();
        let second = Compiler::new(&names, &functions)
            .compile_str(
                &sheet(
                    r#"<xsl:function name="f:k"><xsl:sequence select="'B:k0'"/></xsl:function>
                       <xsl:function name="f:g"><xsl:param name="x"/><xsl:sequence select="concat('B:g1:', $x)"/></xsl:function>
                       <xsl:template match="/"><out><xsl:value-of select="f:k(), f:g('y')"/></out></xsl:template>"#,
                ),
                "urn:second",
            )
            .unwrap();
        // Neither of them leaks into a third compilation.
        let third = Compiler::new(&names, &functions).compile_str(
            &sheet(r#"<xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#),
            "urn:third",
        );
        assert_eq!(third.err().map(|e| e.code), Some("XPST0017".to_string()));
        let run_one = |compiled: &crate::CompiledStylesheet<'_>| {
            let arena = Bump::new();
            let outcome = compiled
                .transformation(&arena, TransformOptions::default())
                .transform_source("urn:in", "<doc/>")
                .unwrap();
            compiled.serialize_primary(&outcome).map(body).unwrap()
        };
        assert_eq!(run_one(&first), "<out>A:g0 A:h1:x</out>");
        assert_eq!(run_one(&second), "<out>B:k0 B:g1:y</out>");
        assert_eq!(run_one(&first), "<out>A:g0 A:h1:x</out>");
        assert_eq!(first.stylesheet_functions().len(), 2);
        assert!(first.has_stylesheet_function(&QName::new("urn:f", "g"), Some(0)));
        assert!(first.has_stylesheet_function(&QName::new("urn:f", "g"), None));
        assert!(!first.has_stylesheet_function(&QName::new("urn:f", "g"), Some(1)));
        assert!(second.has_stylesheet_function(&QName::new("urn:f", "g"), Some(1)));
        assert!(!second.has_stylesheet_function(&QName::new("urn:f", "g"), Some(0)));
    });
}

/// A compiled regular expression is opaque: it tells the effective `regex` and
/// `flags` it was compiled from, and nothing of the engine behind it. (This
/// was a doctest of `RegexSource` while the instruction tree was public.)
#[test]
fn a_fixed_regex_source_tells_its_effective_regex_and_flags() {
    fn effective(regex: &RegexSource) -> Option<(&str, &str)> {
        match regex {
            RegexSource::Fixed(compiled) => Some((compiled.pattern(), compiled.flags())),
            RegexSource::Computed { .. } => None,
        }
    }
    let fixed = RegexSource::Fixed(std::sync::Arc::new(
        CompiledRegex::new("a+", "i").expect("a valid regular expression"),
    ));
    assert_eq!(effective(&fixed), Some(("a+", "i")));
}
