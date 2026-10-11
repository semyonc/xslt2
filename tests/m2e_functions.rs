//! M2e — stylesheet functions (`xsl:function`, §10.3).
//!
//! Every test quotes the sentence of XSLT 2.0 (`xslt20.html`) it checks. The
//! sections follow the work items of ROADMAP §4 "M2e": registration and lookup,
//! `override`, the static errors, the conversions of arguments and result, the
//! dynamic context of a function body (DESIGN §9.1), recursion depth (Q33),
//! `function-available` and `use-when`, and the places a function can be called
//! from.

mod common;

use std::sync::Arc;

use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xsd_schema::navigator::DomNavigator;

use xslt2::{Compiler, MemoryResolver, QName, TransformOptions, XsltError, XsltFunctions};

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

/// The stack every transformation of this file runs on. A debug build's frames
/// are large, and a call through the XPath engine nests several of them, so
/// even ten levels of function recursion outgrow a test thread's default stack
/// (Q33: the same reason the harness runs on `run_on_large_stack`).
const TEST_STACK: usize = 256 * 1024 * 1024;

/// Compiles `main` (with the extra modules `modules`, URI → text, available to
/// `xsl:import` / `xsl:include`) and runs it over `source`, on [`TEST_STACK`].
fn run_full(
    modules: &[(&str, &str)],
    main: &str,
    source: &str,
    options: TransformOptions,
) -> Result<String, XsltError> {
    let modules: Vec<(String, String)> = modules
        .iter()
        .map(|(u, t)| (u.to_string(), t.to_string()))
        .collect();
    let (main, source) = (main.to_string(), source.to_string());
    xslt2::run_on_large_stack(TEST_STACK, move || {
        let functions = XsltFunctions::new();
        let names = NameTable::new();
        let mut mem = MemoryResolver::new();
        for (uri, text) in &modules {
            mem.insert(uri.as_str(), text.as_str());
        }
        let resolver: Arc<dyn xslt2::Resolver> = Arc::new(mem);
        let compiled = Compiler::new(&names, &functions)
            .with_resolver(Arc::clone(&resolver))
            .compile_str(&main, "file:///m2e/main.xsl")?;
        let arena = Bump::new();
        let outcome = compiled
            .transformation(&arena, options)
            .with_resolver(resolver)
            .transform_source("file:///m2e/in.xml", &source)?;
        compiled.serialize_primary(&outcome).map(common::body)
    })
}

fn run(main: &str, source: &str) -> Result<String, XsltError> {
    run_full(&[], main, source, TransformOptions::default())
}

fn run_modules(modules: &[(&str, &str)], main: &str) -> Result<String, XsltError> {
    run_full(modules, main, "<doc/>", TransformOptions::default())
}

/// The error `main` fails with, compiling or running.
fn error_of(main: &str) -> XsltError {
    match run(main, "<doc/>") {
        Ok(out) => panic!("expected an error, got {out:?}"),
        Err(e) => e,
    }
}

fn assert_code(main: &str, code: &str) {
    let error = error_of(main);
    assert_eq!(error.code, code, "{error}");
}

// ── Registration and lookup ─────────────────────────────────────────────────

/// §10.3: "The `xsl:function` declaration defines a stylesheet function that
/// can be called from any XPath expression used in the stylesheet" — the
/// spec's own `str:reverse` example, recursive.
#[test]
fn the_spec_example_str_reverse_runs() {
    let main = r#"<xsl:transform xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
          xmlns:xs="http://www.w3.org/2001/XMLSchema"
          xmlns:str="http://example.com/namespace" version="2.0" exclude-result-prefixes="str">
        <xsl:function name="str:reverse" as="xs:string">
          <xsl:param name="sentence" as="xs:string"/>
          <xsl:sequence select="if (contains($sentence, ' '))
                   then concat(str:reverse(substring-after($sentence, ' ')), ' ',
                               substring-before($sentence, ' '))
                   else $sentence"/>
        </xsl:function>
        <xsl:template match="/"><output><xsl:value-of select="str:reverse('DOG BITES MAN')"/></output></xsl:template>
      </xsl:transform>"#;
    // The example excludes only `str`, so the `xs` namespace node is copied
    // onto the literal result element (§11.1.3).
    assert_eq!(
        run(main, "<doc/>").unwrap(),
        r#"<output xmlns:xs="http://www.w3.org/2001/XMLSchema">MAN BITES DOG</output>"#
    );
}

/// §10.3: "the function that is executed as the result of a function call is
/// identified by looking in the in-scope functions of the static context for a
/// function whose name and arity matches the name and number of arguments in
/// the function call" — two functions of one name, told apart by arity.
#[test]
fn functions_of_one_name_are_overloaded_by_arity() {
    let main = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="'zero'"/></xsl:function>
           <xsl:function name="f:g"><xsl:param name="a"/><xsl:sequence select="concat('one:', $a)"/></xsl:function>
           <xsl:function name="f:g"><xsl:param name="a"/><xsl:param name="b"/>
             <xsl:sequence select="concat('two:', $a, $b)"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:g(), f:g('x'), f:g('x', 'y')" separator="|"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out>zero|one:x|two:xy</out>"
    );
}

/// §10.3: "A stylesheet function is included in the in-scope functions of the
/// static context for all XPath expressions used in the stylesheet" — a global
/// variable declared before the function, and a call to an arity that does not
/// exist is still XPST0017.
#[test]
fn a_function_is_in_scope_before_its_declaration() {
    let main = sheet(
        r#"<xsl:variable name="v" select="f:twice(21)"/>
           <xsl:template match="/"><out><xsl:value-of select="$v"/></out></xsl:template>
           <xsl:function name="f:twice"><xsl:param name="n"/><xsl:sequence select="$n * 2"/></xsl:function>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), "<out>42</out>");
    let wrong_arity = sheet(
        r#"<xsl:function name="f:twice"><xsl:param name="n"/><xsl:sequence select="$n * 2"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:twice(1, 2)"/></out></xsl:template>"#,
    );
    assert_code(&wrong_arity, "XPST0017");
}

/// §10.3: a stylesheet function is in scope "unless there is another
/// stylesheet function with the same name and arity, and higher import
/// precedence".
#[test]
fn the_higher_import_precedence_wins() {
    let imported = sheet(
        r#"<xsl:function name="f:who"><xsl:sequence select="'imported'"/></xsl:function>
           <xsl:function name="f:only-there"><xsl:sequence select="'also imported'"/></xsl:function>"#,
    );
    let main = sheet(
        r#"<xsl:import href="imp.xsl"/>
           <xsl:function name="f:who"><xsl:sequence select="'importer'"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:who(), f:only-there()" separator="|"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&[("file:///m2e/imp.xsl", &imported)], &main).unwrap(),
        "<out>importer|also imported</out>"
    );
}

/// §10.3: "If the override attribute has the value yes, then this function is
/// used in preference; if it has the value no, then the other function is used
/// in preference. The default value is yes."
///
/// The XPath engine also answers, as a vendor extension, for the functions of
/// the 2010 namespace `http://www.w3.org/xpath-functions` — which §3.2 does not
/// reserve — so a stylesheet function there has a processor function of the
/// same name and arity to override, or to give way to.
#[test]
fn override_decides_between_a_stylesheet_and_a_processor_function() {
    let with = |override_attr: &str| {
        format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:v="http://www.w3.org/xpath-functions" exclude-result-prefixes="v">
               <xsl:function name="v:upper-case" {override_attr}>
                 <xsl:param name="s"/><xsl:sequence select="'mine'"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="v:upper-case('abc')"/></out></xsl:template>
               </xsl:stylesheet>"#
        )
    };
    assert_eq!(run(&with(""), "<doc/>").unwrap(), "<out>mine</out>");
    assert_eq!(
        run(&with(r#"override="yes""#), "<doc/>").unwrap(),
        "<out>mine</out>"
    );
    assert_eq!(
        run(&with(r#"override=" no ""#), "<doc/>").unwrap(),
        "<out>ABC</out>"
    );
}

/// §10.3: with `override="no"` the stylesheet function gives way only "if …
/// there is already a function with the same name and arity in the in-scope
/// functions" — which §3.8's fallback error function is not (§18.1.1: "no
/// implementation is available (other than the fallback error function …)").
#[test]
fn override_no_still_beats_the_backwards_compatible_fallback() {
    let main = sheet(
        r#"<xsl:function name="f:g" override="no"><xsl:sequence select="'stylesheet'"/></xsl:function>
           <xsl:template match="/"><out xsl:version="1.0"><xsl:value-of select="f:g()"/></out></xsl:template>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), "<out>stylesheet</out>");
}

/// The table of stylesheet functions belongs to one compilation: a function
/// set reused for a second stylesheet does not see the first one's functions,
/// and each compiled stylesheet keeps calling its own (DESIGN §6.6: the function
/// set is caller-owned and may serve many compilations). Stylesheets A and B
/// mint the same handle numbers for different functions — A: `f:g#0`, `f:h#1`;
/// B: `f:k#0`, `f:g#1` — and are run in the order A, B, A after both are
/// compiled.
#[test]
fn a_function_set_reused_across_compilations_keeps_them_apart() {
    xslt2::run_on_large_stack(TEST_STACK, || {
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
        let run_one = |compiled: &xslt2::CompiledStylesheet<'_>| {
            let arena = Bump::new();
            let outcome = compiled
                .transformation(&arena, TransformOptions::default())
                .transform_source("urn:in", "<doc/>")
                .unwrap();
            compiled
                .serialize_primary(&outcome)
                .map(common::body)
                .unwrap()
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

// ── Static errors ───────────────────────────────────────────────────────────

/// "\[ERR XTSE0740\] A stylesheet function must have a prefixed name, to
/// remove any risk of a clash with a function in the default function
/// namespace. It is a static error if the name has no prefix."
#[test]
fn xtse0740_an_unprefixed_name() {
    assert_code(
        &sheet(
            r#"<xsl:function name="g"><xsl:sequence select="1"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#,
        ),
        "XTSE0740",
    );
}

/// §10.3: "The prefix must not refer to a reserved namespace: \[see ERR
/// XTSE0080\]" — "\[ERR XTSE0080\] It is a static error to use a reserved
/// namespace in the name of … a stylesheet function".
#[test]
fn xtse0080_a_name_in_a_reserved_namespace() {
    for uri in [
        "http://www.w3.org/1999/XSL/Transform",
        "http://www.w3.org/2005/xpath-functions",
        "http://www.w3.org/2001/XMLSchema",
    ] {
        let main = sheet(&format!(
            r#"<xsl:function name="r:g" xmlns:r="{uri}"><xsl:sequence select="1"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_code(&main, "XTSE0080");
    }
}

/// §2.4/§3.4 XTSE0020: a `name` that is not a QName, and an `override` that is
/// neither `yes` nor `no` (`override? = "yes" | "no"`).
#[test]
fn xtse0020_a_bad_name_or_override_value() {
    assert_code(
        &sheet(
            r#"<xsl:function name="f:g?"><xsl:sequence select="1"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#,
        ),
        "XTSE0020",
    );
    assert_code(
        &sheet(
            r#"<xsl:function name="f:g" override="maybe"><xsl:sequence select="1"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#,
        ),
        "XTSE0020",
    );
}

/// "\[ERR XTSE0760\] Because arguments to a stylesheet function call must all
/// be specified, the xsl:param elements within an xsl:function element must not
/// specify a default value: this means they must be empty, and must not have a
/// select attribute."
#[test]
fn xtse0760_a_parameter_with_a_default_value() {
    for param in [
        r#"<xsl:param name="a" select="1"/>"#,
        r#"<xsl:param name="a">1</xsl:param>"#,
        r#"<xsl:param name="a"><xsl:sequence select="1"/></xsl:param>"#,
    ] {
        let main = sheet(&format!(
            r#"<xsl:function name="f:g">{param}<xsl:sequence select="$a"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_code(&main, "XTSE0760");
    }
}

/// §9.2: "The optional required attribute … must not be specified for function
/// parameters, which are always mandatory" (XTSE0090, as the W3C suite expects
/// in `function-0119a` / `-0120a`); and "the value yes may be specified only
/// for template parameters" for `tunnel` (XTSE0020) — while `tunnel="no"` is
/// the default and is accepted.
#[test]
fn required_and_tunnel_on_a_function_parameter() {
    let with = |attrs: &str| {
        sheet(&format!(
            r#"<xsl:function name="f:g"><xsl:param name="a" {attrs}/><xsl:sequence select="$a"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:g(7)"/></out></xsl:template>"#
        ))
    };
    assert_code(&with(r#"required="yes""#), "XTSE0090");
    assert_code(&with(r#"required="no""#), "XTSE0090");
    assert_code(&with(r#"tunnel="yes""#), "XTSE0020");
    assert_eq!(
        run(&with(r#"tunnel="no""#), "<doc/>").unwrap(),
        "<out>7</out>"
    );
}

/// "\[ERR XTSE0770\] It is a static error for a stylesheet to contain two or
/// more functions with the same expanded-QName, the same arity, and the same
/// import precedence, unless there is another function with the same
/// expanded-QName and arity, and a higher import precedence."
#[test]
fn xtse0770_two_functions_of_one_name_arity_and_precedence() {
    // Same module — judged by expanded QName, so `f:g` and `f2:g` with both
    // prefixes bound to `urn:f` are the same name; and an included module,
    // which has the importer's precedence.
    assert_code(
        &sheet(
            r#"<xsl:function name="f:g"><xsl:sequence select="1"/></xsl:function>
               <xsl:function name="f2:g" xmlns:f2="urn:f"><xsl:sequence select="2"/></xsl:function>
               <xsl:template match="/"><out/></xsl:template>"#,
        ),
        "XTSE0770",
    );
    let included = sheet(r#"<xsl:function name="f:g"><xsl:sequence select="2"/></xsl:function>"#);
    let main = sheet(
        r#"<xsl:include href="inc.xsl"/>
           <xsl:function name="f:g"><xsl:sequence select="1"/></xsl:function>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    let error = run_modules(&[("file:///m2e/inc.xsl", &included)], &main).unwrap_err();
    assert_eq!(error.code, "XTSE0770", "{error}");
    // Different arities never clash.
    let arities = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="1"/></xsl:function>
           <xsl:function name="f:g"><xsl:param name="a"/><xsl:sequence select="2"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:g() + f:g(0)"/></out></xsl:template>"#,
    );
    assert_eq!(run(&arities, "<doc/>").unwrap(), "<out>3</out>");
}

/// XTSE0770's "unless" clause: two declarations at one precedence in an
/// imported module are overridden by a third in the importer.
#[test]
fn xtse0770_not_when_a_higher_precedence_declaration_exists() {
    let imported = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="'a'"/></xsl:function>
           <xsl:function name="f:g"><xsl:sequence select="'b'"/></xsl:function>"#,
    );
    let main = sheet(
        r#"<xsl:import href="imp.xsl"/>
           <xsl:function name="f:g"><xsl:sequence select="'top'"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#,
    );
    assert_eq!(
        run_modules(&[("file:///m2e/imp.xsl", &imported)], &main).unwrap(),
        "<out>top</out>"
    );
}

/// §2.9: "A static error must be signaled even if it occurs in a part of the
/// stylesheet that is never evaluated" — the body of a function that lost to a
/// higher import precedence is still compiled.
#[test]
fn a_static_error_in_an_overridden_function_body_is_reported() {
    let imported =
        sheet(r#"<xsl:function name="f:g"><xsl:sequence select="$undeclared"/></xsl:function>"#);
    let main = sheet(
        r#"<xsl:import href="imp.xsl"/>
           <xsl:function name="f:g"><xsl:sequence select="1"/></xsl:function>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    let error = run_modules(&[("file:///m2e/imp.xsl", &imported)], &main).unwrap_err();
    assert_eq!(error.code, "XPST0008", "{error}");
}

// ── Conversion of arguments and result ──────────────────────────────────────

/// §10.3: "The as attribute of the xsl:param element defines the required type
/// of the parameter. The rules for converting the values of the actual
/// arguments supplied in the function call to the types required by each
/// xsl:param element are defined in \[XPath 2.0\]" — atomization and the cast
/// of `xs:untypedAtomic`, numeric and `xs:anyURI` promotion.
#[test]
fn arguments_are_converted_by_the_function_conversion_rules() {
    let main = sheet(
        r#"<xsl:function name="f:inc" as="xs:integer"><xsl:param name="n" as="xs:integer"/>
             <xsl:sequence select="$n + 1"/></xsl:function>
           <xsl:function name="f:half" as="xs:double"><xsl:param name="d" as="xs:double"/>
             <xsl:sequence select="$d div 2"/></xsl:function>
           <xsl:function name="f:len"><xsl:param name="s" as="xs:string"/>
             <xsl:sequence select="string-length($s)"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of
             select="f:inc(/doc/@n), f:half(3), f:len(xs:anyURI('abc'))" separator="|"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, r#"<doc n="41"/>"#).unwrap(),
        "<out>42|1.5|3</out>"
    );
}

/// "\[ERR XTTE0790\] If the value of a parameter to a stylesheet function
/// cannot be converted to the required type, a type error is signaled."
#[test]
fn xtte0790_an_argument_of_the_wrong_type() {
    let with = |arg: &str| {
        sheet(&format!(
            r#"<xsl:function name="f:inc"><xsl:param name="n" as="xs:integer"/><xsl:sequence select="$n + 1"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:inc({arg})"/></out></xsl:template>"#
        ))
    };
    assert_code(&with("'abc'"), "XTTE0790");
    assert_code(&with("(1, 2)"), "XTTE0790");
    assert_code(&with("()"), "XTTE0790");
    assert_code(&with("1.5"), "XTTE0790");
}

/// §10.3: "The rules that apply are those for the case where XPath 1.0
/// compatibility mode is set to false" — even when the call is written in a
/// backwards-compatible region, where a built-in function's argument would be
/// truncated to its first item.
#[test]
fn arguments_are_not_converted_by_the_xpath_1_0_rules() {
    let main = sheet(
        r#"<xsl:function name="f:id"><xsl:param name="n" as="xs:integer"/><xsl:sequence select="$n"/></xsl:function>
           <xsl:template match="/"><out xsl:version="1.0"><xsl:value-of select="f:id((1, 2))"/></out></xsl:template>"#,
    );
    assert_code(&main, "XTTE0790");
}

/// "\[ERR XTTE0780\] If the as attribute is specified, then the result
/// evaluated by the sequence constructor … is converted to the required type,
/// using the function conversion rules. It is a type error if this conversion
/// fails. If the as attribute is omitted, the calculated result is used as
/// supplied, and no conversion takes place." The spec's `num:roman` example
/// relies on a text node being atomized to `xs:string`.
#[test]
fn the_result_is_converted_and_xtte0780() {
    let main = sheet(
        r#"<xsl:function name="f:text" as="xs:string"><xsl:value-of select="'from a text node'"/></xsl:function>
           <xsl:function name="f:raw"><xsl:value-of select="'raw'"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of
             select="f:text() instance of xs:string, f:raw() instance of text()" separator="|"/></out></xsl:template>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), "<out>true|true</out>");
    assert_code(
        &sheet(
            r#"<xsl:function name="f:g" as="xs:integer"><xsl:sequence select="'abc'"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#,
        ),
        "XTTE0780",
    );
    assert_code(
        &sheet(
            r#"<xsl:function name="f:g" as="xs:integer"><xsl:sequence select="1, 2"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#,
        ),
        "XTTE0780",
    );
}

/// §10.3 returns the value of "a sequence constructor that defines the value to
/// be returned by the function": a sequence, so two constructed elements are
/// two parentless items, not one tree.
#[test]
fn a_function_returns_a_sequence_not_a_tree() {
    let main = sheet(
        r#"<xsl:function name="f:pair"><a/><b/></xsl:function>
           <xsl:template match="/"><out n="{count(f:pair())}"
             p="{count(f:pair()/..)}"><xsl:copy-of select="f:pair()"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        r#"<out n="2" p="0"><a/><b/></out>"#
    );
}

// ── The dynamic context of the body (DESIGN §9.1) ───────────────────────────

/// §10.3: "Within the body of a stylesheet function, the focus is initially
/// undefined; this means that any attempt to reference the context item,
/// context position, or context size is a non-recoverable dynamic error.
/// \[XPDY0002\]" — and with it `current()`: "\[ERR XTDE1360\] If the current
/// function is evaluated within an expression that is evaluated when the
/// context item is undefined, a non-recoverable dynamic error occurs."
#[test]
fn the_focus_is_absent_in_a_function_body() {
    for (body, code) in [
        (r#"<xsl:sequence select="."/>"#, "XPDY0002"),
        (r#"<xsl:sequence select="position()"/>"#, "XPDY0002"),
        (r#"<xsl:sequence select="last()"/>"#, "XPDY0002"),
        (r#"<xsl:sequence select="name()"/>"#, "XPDY0002"),
        // §16.6.4: "If the argument is omitted, it defaults to the context
        // node" — of which there is none.
        (r#"<xsl:sequence select="generate-id()"/>"#, "XPDY0002"),
        (r#"<xsl:apply-templates/>"#, "XPDY0002"),
        (r#"<xsl:sequence select="current()"/>"#, "XTDE1360"),
    ] {
        let main = sheet(&format!(
            r#"<xsl:function name="f:g">{body}</xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#
        ));
        let error = run(&main, "<doc/>").unwrap_err();
        assert_eq!(error.code, code, "{body}: {error}");
    }
    // "If the argument is the empty sequence, the result is the zero-length
    // string" still holds.
    let empty = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="generate-id(())"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="string-length(f:g())"/></out></xsl:template>"#,
    );
    assert_eq!(run(&empty, "<doc/>").unwrap(), "<out>0</out>");
    // An argument still carries the caller's context item in.
    let main = sheet(
        r#"<xsl:function name="f:name"><xsl:param name="n"/><xsl:sequence select="name($n)"/></xsl:function>
           <xsl:template match="/*"><out><xsl:value-of select="f:name(.)"/></out></xsl:template>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), "<out>doc</out>");
}

/// §6.7: "when a stylesheet function is called (see 10.3 Stylesheet Functions),
/// the current template rule becomes null for the evaluation of that
/// instruction or function", and "\[ERR XTDE0560\] It is a non-recoverable
/// dynamic error if xsl:apply-imports or xsl:next-match is evaluated when the
/// current template rule is null."
#[test]
fn the_current_template_rule_is_null_in_a_function_body() {
    for instruction in ["<xsl:next-match/>", "<xsl:apply-imports/>"] {
        let main = sheet(&format!(
            r#"<xsl:function name="f:g">{instruction}</xsl:function>
               <xsl:template match="doc"><out><xsl:copy-of select="f:g()"/></out></xsl:template>"#
        ));
        let error = run(&main, "<doc/>").unwrap_err();
        assert_eq!(error.code, "XTDE0560", "{instruction}: {error}");
    }
}

/// §6.5: "When a stylesheet function is called, the current mode is set to
/// the default mode. … on return from a stylesheet function call, the current
/// mode reverts to its previous value." So `mode="#current"` inside the body
/// is the default mode, and the caller's mode is intact afterwards.
#[test]
fn the_current_mode_is_the_default_mode_in_a_function_body() {
    let main = sheet(
        r##"<xsl:function name="f:apply"><xsl:param name="n"/>
             <xsl:apply-templates select="$n" mode="#current"/></xsl:function>
           <xsl:template match="/"><out><xsl:apply-templates select="doc" mode="m"/></out></xsl:template>
           <xsl:template match="doc" mode="m">
             <m><xsl:copy-of select="f:apply(item)"/></m><xsl:apply-templates select="item" mode="#current"/>
           </xsl:template>
           <xsl:template match="item">[default]</xsl:template>
           <xsl:template match="item" mode="m">[m]</xsl:template>"##,
    );
    assert_eq!(
        run(&main, "<doc><item/></doc>").unwrap(),
        "<out><m>[default]</m>[m]</out>"
    );
}

/// §10.1.2: "Tunnel parameters are not passed in calls to stylesheet
/// functions", and for an instruction inside a stylesheet function "the base
/// set is empty" — a template called from the body does not see the caller's
/// tunnel parameter.
#[test]
fn tunnel_parameters_do_not_pass_through_a_function() {
    let main = sheet(
        r#"<xsl:function name="f:via"><xsl:call-template name="leaf"/></xsl:function>
           <xsl:template match="/"><out><xsl:call-template name="mid">
             <xsl:with-param name="t" select="'tunnelled'" tunnel="yes"/></xsl:call-template></out></xsl:template>
           <xsl:template name="mid"><direct><xsl:call-template name="leaf"/></direct><via><xsl:sequence select="f:via()"/></via></xsl:template>
           <xsl:template name="leaf"><xsl:param name="t" select="'default'" tunnel="yes"/><xsl:value-of select="$t"/></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out><direct>tunnelled</direct><via>default</via></out>"
    );
}

/// §10.3: "It is not possible within the body of the stylesheet function to
/// access the values of local variables that were in scope in the place where
/// the function call was written. Global variables, however, remain
/// available."
#[test]
fn a_function_body_sees_globals_and_not_the_callers_locals() {
    let globals = sheet(
        r#"<xsl:variable name="g" select="'global'"/>
           <xsl:function name="f:g"><xsl:sequence select="$g"/></xsl:function>
           <xsl:template match="/"><xsl:variable name="g" select="'local'"/><out><xsl:value-of select="f:g(), $g"/></out></xsl:template>"#,
    );
    assert_eq!(run(&globals, "<doc/>").unwrap(), "<out>global local</out>");
    let locals = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="$local"/></xsl:function>
           <xsl:template match="/"><xsl:variable name="local" select="1"/><out><xsl:value-of select="f:g()"/></out></xsl:template>"#,
    );
    assert_code(&locals, "XPST0008");
}

// ── Recursion (Q33) ─────────────────────────────────────────────────────────

/// §9.8: "Implementations are required to signal circularity as a dynamic
/// error, but they are not required to detect non-terminating recursion" —
/// this one does, through `TransformOptions::max_recursion_depth`, in which a
/// function call counts as 8 template levels (measured), and through the stack
/// guard (`TransformOptions::stack_limit`; heavy bodies are tested below).
#[test]
fn runaway_recursion_through_functions_is_the_depth_error() {
    let main = sheet(
        r#"<xsl:function name="f:down" as="xs:integer"><xsl:param name="n" as="xs:integer"/>
             <xsl:sequence select="if ($n = 0) then 0 else f:down($n - 1) + 1"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:down(/doc/@n)"/></out></xsl:template>"#,
    );
    let options = || TransformOptions {
        max_recursion_depth: 200,
        ..Default::default()
    };
    // 20 nested calls (+ the template) fit in 200; 100 do not.
    assert_eq!(
        run_full(&[], &main, r#"<doc n="20"/>"#, options()).unwrap(),
        "<out>20</out>"
    );
    let error = run_full(&[], &main, r#"<doc n="100"/>"#, options()).unwrap_err();
    assert!(
        error.message.contains("max_recursion_depth"),
        "expected the depth error, got {error}"
    );
    // With the default limits on the default large stack (the release
    // measurement is in `runtime/calls.rs`). A debug build's frames are about
    // thirteen times larger (some 220 KB per call of this function), so a
    // thousand calls would come close to the stack limit there.
    let depth = if cfg!(debug_assertions) { 400 } else { 1000 };
    assert_eq!(
        run(&main, &format!(r#"<doc n="{depth}"/>"#)).unwrap(),
        format!("<out>{depth}</out>")
    );
}

/// §9.8: "Stylesheet functions … and named templates … may call other
/// functions and named templates without restriction" — mutual recursion.
#[test]
fn functions_may_call_each_other_and_templates() {
    let main = sheet(
        r#"<xsl:function name="f:even" as="xs:boolean"><xsl:param name="n" as="xs:integer"/>
             <xsl:sequence select="if ($n = 0) then true() else f:odd($n - 1)"/></xsl:function>
           <xsl:function name="f:odd" as="xs:boolean"><xsl:param name="n" as="xs:integer"/>
             <xsl:sequence select="if ($n = 0) then false() else f:even($n - 1)"/></xsl:function>
           <xsl:function name="f:wrap"><xsl:param name="s"/><xsl:call-template name="w">
             <xsl:with-param name="s" select="$s"/></xsl:call-template></xsl:function>
           <xsl:template name="w"><xsl:param name="s"/><w><xsl:value-of select="$s, f:even(string-length($s))"/></w></xsl:template>
           <xsl:template match="/"><out><xsl:value-of select="f:even(10), f:odd(7), f:even(3)"/><xsl:copy-of select="f:wrap('ab')"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out>true true false<w>ab true</w></out>"
    );
}

// ── function-available and use-when (§18.1.1, §3.12) ───────────────────────

/// §10.3: "If a stylesheet function has been defined with a particular
/// expanded-QName, then a call on function-available will return true when
/// called with an argument that is a lexical QName that expands to this same
/// expanded-QName." §18.1.1: "The two-argument version of the
/// function-available function returns true if and only if there is an
/// available function whose name matches the value of the $function-name
/// argument and whose arity matches the value of the $arity argument."
#[test]
fn function_available_sees_stylesheet_functions_and_their_arity() {
    let main = sheet(
        r#"<xsl:function name="f:two"><xsl:param name="a"/><xsl:param name="b"/><xsl:sequence select="1"/></xsl:function>
           <xsl:param name="name" select="'f:two'"/>
           <xsl:template match="/"><out><xsl:value-of select="
             function-available('f:two'), function-available('f:two', 2),
             function-available('f:two', 1), function-available('f:three'),
             function-available($name, 2), function-available($name, -1)"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out>true true false false true false</out>"
    );
}

/// G15-FUNCTION-AVAILABLE-ARITY (`function-available-1015`): the arity
/// argument is honoured for the processor's own functions too — `fn:true` has
/// arity 0 only, `fn:concat` any arity from 2.
#[test]
fn function_available_honours_the_arity_of_built_in_functions() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="
             function-available('true'), function-available('true', 0),
             function-available('true', 1), function-available('concat', 17),
             function-available('concat', 1), function-available('function-available', 2)"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out>true true false true false true</out>"
    );
}

/// §3.12, the static context of a `use-when` expression: "Note that stylesheet
/// functions are not included in the context, which means that the function
/// function-available will return false in respect of such functions" — and a
/// call to one does not bind (XPST0017). The arity argument is honoured there
/// too.
#[test]
fn use_when_does_not_see_stylesheet_functions() {
    let main = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="1"/></xsl:function>
           <xsl:template match="/"><out>
             <a use-when="function-available('f:g')" xsl:use-when="function-available('f:g')">seen</a>
             <b xsl:use-when="not(function-available('f:g'))">hidden</b>
             <c xsl:use-when="function-available('true', 0) and not(function-available('true', 1))">arity</c>
           </out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out><b>hidden</b><c>arity</c></out>"
    );
    let call = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="true()"/></xsl:function>
           <xsl:template match="/"><out xsl:use-when="f:g()"/></xsl:template>"#,
    );
    assert_code(&call, "XPST0017");
}

/// §3.8: with backwards-compatible behaviour "any function call for which no
/// implementation is available (unless it uses the standard function
/// namespace) is bound to a fallback error function" — a stylesheet function
/// *is* an implementation, so it binds first; an unknown name still gets the
/// fallback (XTDE1425 only when evaluated), and only in that mode.
#[test]
fn a_stylesheet_function_binds_before_the_backwards_compatible_fallback() {
    let main = sheet(
        r#"<xsl:function name="f:g"><xsl:sequence select="'g'"/></xsl:function>
           <xsl:template match="/"><out xsl:version="1.0"><xsl:value-of select="f:g()"/>
             <xsl:if test="function-available('f:missing')"><xsl:value-of select="f:missing()"/></xsl:if>
           </out></xsl:template>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), "<out>g</out>");
    let evaluated = sheet(
        r#"<xsl:template match="/"><out xsl:version="1.0"><xsl:value-of select="f:missing()"/></out></xsl:template>"#,
    );
    assert_code(&evaluated, "XTDE1425");
    let not_backwards = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="f:missing()"/></out></xsl:template>"#,
    );
    assert_code(&not_backwards, "XPST0017");
}

// ── Call sites ──────────────────────────────────────────────────────────────

/// §10.3: a function can be called "from any XPath expression used in the
/// stylesheet (including an XPath expression used within a predicate in a
/// pattern)".
#[test]
fn a_function_is_called_from_a_pattern_predicate() {
    let main = sheet(
        r#"<xsl:function name="f:keep" as="xs:boolean"><xsl:param name="n"/><xsl:sequence select="$n/@k = 'y'"/></xsl:function>
           <xsl:template match="/"><out><xsl:apply-templates select="doc/i"/></out></xsl:template>
           <xsl:template match="i[f:keep(.)]">[<xsl:value-of select="@id"/>]</xsl:template>
           <xsl:template match="i"/>"#,
    );
    assert_eq!(
        run(
            &main,
            r#"<doc><i id="1" k="y"/><i id="2"/><i id="3" k="y"/></doc>"#
        )
        .unwrap(),
        "<out>[1][3]</out>"
    );
}

/// An attribute value template (§5.6) is an XPath expression of the stylesheet
/// like any other.
#[test]
fn a_function_is_called_from_an_attribute_value_template() {
    let main = sheet(
        r#"<xsl:function name="f:sq"><xsl:param name="n"/><xsl:sequence select="$n * $n"/></xsl:function>
           <xsl:template match="/"><out a="{f:sq(3)}-{f:sq(4)}"/></xsl:template>"#,
    );
    assert_eq!(run(&main, "<doc/>").unwrap(), r#"<out a="9-16"/>"#);
}

/// A sort key (§13.1.1) is an XPath expression too — `function-0301`: "stylesheet
/// function invoked from within sort key".
#[test]
fn a_function_is_called_from_a_sort_key() {
    let main = sheet(
        r#"<xsl:function name="f:key"><xsl:param name="n"/><xsl:sequence select="0 - number($n/@v)"/></xsl:function>
           <xsl:template match="/"><out><xsl:for-each select="doc/i"><xsl:sort select="f:key(.)" data-type="number"/>
             <xsl:value-of select="@v"/></xsl:for-each></out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, r#"<doc><i v="2"/><i v="3"/><i v="1"/></doc>"#).unwrap(),
        "<out>321</out>"
    );
}

// ── Fixes for the independent review (`XSLT2_M2E_REVIEW.md`) ────────────────

/// Review finding 1. §3.2: "Reserved namespaces must not be used, however, in
/// the names of stylesheet-defined objects such as variables and stylesheet
/// functions", and "\[ERR XTSE0080\] It is a static error to use a reserved
/// namespace in the name of … a stylesheet function"; §10.3: "The prefix must
/// not refer to a reserved namespace". The exemption this processor makes for a
/// *template* named `xsl:initial-template` (the conventional entry point) does
/// not extend to a function of that name.
#[test]
fn xtse0080_a_function_named_xsl_initial_template() {
    assert_code(
        &sheet(
            r#"<xsl:function name="xsl:initial-template"><xsl:sequence select="'oops'"/></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="xsl:initial-template()"/></out></xsl:template>"#,
        ),
        "XTSE0080",
    );
    // The template exemption is unchanged.
    let template = sheet(
        r#"<xsl:template name="xsl:initial-template"><entry/></xsl:template>
           <xsl:template match="/"><out/></xsl:template>"#,
    );
    assert_eq!(run(&template, "<doc/>").unwrap(), "<out/>");
}

/// Review finding 2. §3.2: "The reserved namespaces are those listed below",
/// and the list includes "The XML namespace, defined in \[Namespaces in XML
/// 1.0\] as `http://www.w3.org/XML/1998/namespace`, is used for attributes such
/// as xml:lang, xml:space, and xml:id" and "The namespace
/// `http://www.w3.org/2000/xmlns/` is reserved for use as described in
/// \[Namespaces in XML 1.0\]". So XTSE0080 holds for a stylesheet function, a
/// named template and an attribute set named in either — while attributes in
/// the XML namespace stay legal, because they are not declared names: "Reserved
/// namespaces may be used without restriction to refer to the names of elements
/// and attributes in source documents and result documents."
#[test]
fn xtse0080_the_xml_and_xmlns_namespaces_are_reserved() {
    for declaration in [
        r#"<xsl:function name="xml:g"><xsl:sequence select="1"/></xsl:function>"#,
        r#"<xsl:template name="xml:t"/>"#,
        r#"<xsl:attribute-set name="xml:s"/>"#,
        // The parser accepts a prefix bound to the xmlns namespace, so the
        // name does expand into it.
        r#"<xsl:function name="r:g" xmlns:r="http://www.w3.org/2000/xmlns/"><xsl:sequence select="1"/></xsl:function>"#,
    ] {
        let main = sheet(&format!(
            r#"{declaration}<xsl:template match="/"><out/></xsl:template>"#
        ));
        assert_code(&main, "XTSE0080");
    }
    let attributes = sheet(
        r#"<xsl:template match="/"><out xml:lang="en"><xsl:attribute name="xml:space">preserve</xsl:attribute>
             <xsl:attribute name="xml:base" select="'http://example.com/'"/><e xml:id="i1"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&attributes, "<doc/>").unwrap(),
        r#"<out xml:lang="en" xml:space="preserve" xml:base="http://example.com/"><e xml:id="i1"/></out>"#
    );
}

/// Review finding 3. §2.9: "A static error must be signaled even if it occurs
/// in a part of the stylesheet that is never evaluated" — §10.1.1's "\[ERR
/// XTSE0680\] In the case of xsl:call-template, it is a static error to pass a
/// non-tunnel parameter named x to a template that does not have a template
/// parameter named x" in the body of a function that lost to one of higher
/// import precedence, as it already was for a template that lost.
#[test]
fn xtse0680_in_an_overridden_function_body() {
    let imported = sheet(
        r#"<xsl:function name="f:g"><xsl:call-template name="t">
             <xsl:with-param name="x" select="1"/></xsl:call-template></xsl:function>"#,
    );
    let main = sheet(
        r#"<xsl:import href="imp.xsl"/>
           <xsl:function name="f:g"><xsl:sequence select="'top'"/></xsl:function>
           <xsl:template name="t"/>
           <xsl:template match="/"><out><xsl:value-of select="f:g()"/></out></xsl:template>"#,
    );
    let error = run_modules(&[("file:///m2e/imp.xsl", &imported)], &main).unwrap_err();
    assert_eq!(error.code, "XTSE0680", "{error}");
}

/// Review finding 5. §18.1.1: `function-available($function-name as
/// xs:string, $arity as xs:integer) as xs:boolean`, and XPath 2.0 §3.1.5 converts
/// the argument: "Each item in the atomic sequence that is of type
/// xs:untypedAtomic is cast to the expected atomic type", numeric promotion only
/// widens (B.1), and "If, after the above conversions, the resulting value does
/// not match the expected type according to the rules for SequenceType Matching,
/// a type error is raised \[err:XPTY0004\]" — two items, an `xs:double`, a string,
/// an `xs:decimal` and the empty sequence are each XPTY0004, at run time and in
/// `use-when`.
#[test]
fn function_available_type_checks_its_arity_argument() {
    for arity in ["(0, 1)", "0e0", "'abc'", "0.5", "()"] {
        let at_run_time = sheet(&format!(
            r#"<xsl:template match="/"><out><xsl:value-of select="function-available('true', {arity})"/></out></xsl:template>"#
        ));
        assert_code(&at_run_time, "XPTY0004");
        let in_use_when = sheet(&format!(
            r#"<xsl:template match="/"><out><a xsl:use-when="function-available('true', {arity})"/></out></xsl:template>"#
        ));
        assert_code(&in_use_when, "XPTY0004");
    }
    // An untyped value is cast; a subtype of xs:integer is an xs:integer.
    let accepted = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="function-available('true', xs:untypedAtomic('0')),
             function-available('true', /doc/@n), function-available('true', xs:byte(0)),
             function-available('true', xs:untypedAtomic('1'))"/>
             <a xsl:use-when="function-available('true', xs:untypedAtomic('0'))"/></out></xsl:template>"#,
    );
    assert_eq!(
        run(&accepted, r#"<doc n=" 0 "/>"#).unwrap(),
        "<out>true true true false<a/></out>"
    );
    // An untyped value that is no xs:integer fails the cast (F&O FORG0001).
    let uncastable = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="function-available('true', /doc/@n)"/></out></xsl:template>"#,
    );
    let error = run(&uncastable, r#"<doc n="zero"/>"#).unwrap_err();
    assert_eq!(error.code, "FORG0001", "{error}");
    // XPath 1.0 compatibility mode adds one step for an expected type of one
    // item: "the value V is effectively replaced by V\[1\]" — and only
    // `xs:string` and `xs:double` get `fn:string` / `fn:number`, so `0e0` is
    // still not an xs:integer.
    let compat = |arity: &str| {
        sheet(&format!(
            r#"<xsl:template match="/"><out xsl:version="1.0"><xsl:value-of select="function-available('true', {arity})"/></out></xsl:template>"#
        ))
    };
    assert_eq!(run(&compat("(0, 1)"), "<doc/>").unwrap(), "<out>true</out>");
    assert_code(&compat("0e0"), "XPTY0004");
    assert_code(&compat("()"), "XPTY0004");
}

/// Review finding 6. XPath 2.0 §2.1.1: "The function signatures include the
/// signatures of constructor functions"; §3.10.4: "For every atomic type in the
/// in-scope schema types (except xs:NOTATION and xs:anyAtomicType, which are not
/// instantiable), a constructor function is implicitly defined", with the
/// signature `T($arg as xs:anyAtomicType?) as T?` (arity 1). For a basic
/// processor §21.1 adds: "The set of constructor functions available are limited
/// to those that construct values of the above atomic types" (§3.13's list).
/// §18.1.1: "A function is said to be available within an XPath expression if it
/// is present in the in-scope functions for that expression". §3.12 keeps "the
/// core functions defined in \[Functions and Operators\]" in a `use-when`
/// expression, where the constructors are defined.
#[test]
fn constructor_functions_are_available() {
    let main = sheet(
        r#"<xsl:template match="/"><out><xsl:value-of select="
             function-available('xs:string'), function-available('xs:string', 1),
             function-available('xs:integer', 1), function-available('xs:untypedAtomic', 1),
             function-available('xs:dayTimeDuration'), function-available('xs:QName', 1),
             function-available('xs:string', 0), function-available('xs:string', 2),
             function-available('xs:NOTATION'), function-available('xs:anyAtomicType', 1),
             function-available('xs:anySimpleType', 1), function-available('xs:untyped'),
             function-available('xs:anyType'), function-available('xs:long')"/>
             <a xsl:use-when="function-available('xs:string') and function-available('xs:date', 1)"/>
             <b xsl:use-when="function-available('xs:string', 2) or function-available('xs:NOTATION')"/>
           </out></xsl:template>"#,
    );
    assert_eq!(
        run(&main, "<doc/>").unwrap(),
        "<out>true true true true true true false false false false false false false false<a/></out>"
    );
}

// ── Review finding 4: the stack guard (Q33) ─────────────────────────────────

/// `n` levels of `open` … `close` around `inner`.
fn nest(n: usize, open: &str, inner: &str, close: &str) -> String {
    format!("{}{inner}{}", open.repeat(n), close.repeat(n))
}

/// Asserts that `result` is the depth error of `TransformOptions::
/// max_recursion_depth` / `stack_limit` — an error the caller gets back, where
/// without the guard the process aborts with a stack overflow.
fn assert_depth_error(result: Result<String, XsltError>, what: &str) {
    let error = match result {
        Ok(out) => panic!("{what}: expected the depth error, got {out:?}"),
        Err(e) => e,
    };
    assert_eq!(error.code, "XSLT2-INTERNAL", "{what}: {error}");
    assert!(
        error.message.contains("recursing without end"),
        "{what}: expected the depth error, got {error}"
    );
}

/// §9.8: "Implementations are required to signal circularity as a dynamic
/// error, but they are not required to detect non-terminating recursion" —
/// this processor does detect it, and the review found that a count of nested
/// calls alone did not make that a guarantee: a recursive call inside a heavy
/// body (an `xsl:variable` holding ten nested literal result elements, ten
/// nested XPath `if`/`for` levels, ten nested `xsl:if`/`xsl:for-each`/
/// `xsl:choose` triples) overflowed the default 256 MiB stack between 1 000 and
/// 4 500 calls, before the 5 000-call limit. The stack guard
/// (`TransformOptions::stack_limit`) ends each of them in the depth error.
#[test]
fn recursion_through_heavy_function_bodies_ends_in_the_depth_error() {
    let function = |body: &str| {
        sheet(&format!(
            r#"<xsl:function name="f:down" as="xs:integer"><xsl:param name="n" as="xs:integer"/>
                 <xsl:choose><xsl:when test="$n = 0"><xsl:sequence select="0"/></xsl:when><xsl:otherwise>
                 <xsl:variable name="v">{body}</xsl:variable>
                 <xsl:sequence select="xs:integer(string($v)) + 1"/></xsl:otherwise></xsl:choose></xsl:function>
               <xsl:template match="/"><out><xsl:value-of select="f:down(xs:integer(/doc/@n))"/></out></xsl:template>"#
        ))
    };
    let call = r#"<xsl:value-of select="f:down($n - 1)"/>"#;
    let elements = nest(10, "<e>", call, "</e>");
    let xpath = format!(
        r#"<xsl:value-of select="{}"/>"#,
        nest(
            10,
            "(if ($n ge 0) then (for $q in 1 return ",
            "f:down($n - 1)",
            ") else ())"
        )
    );
    let open =
        r#"<xsl:if test="$n ge 0"><xsl:for-each select="1"><xsl:choose><xsl:when test="true()">"#;
    let close = "</xsl:when></xsl:choose></xsl:for-each></xsl:if>";
    let instructions = nest(10, open, call, close);
    // One call, inside all three kinds of nesting.
    let all = nest(10, "<e>", &nest(10, open, &xpath, close), "</e>");
    for (what, body) in [
        ("nested literal result elements", elements.as_str()),
        ("nested XPath if/for", xpath.as_str()),
        (
            "nested xsl:if/xsl:for-each/xsl:choose",
            instructions.as_str(),
        ),
        ("all three", all.as_str()),
    ] {
        // A recursion that would terminate only after a million calls.
        assert_depth_error(run(&function(body), r#"<doc n="1000000"/>"#), what);
        // The guard does not stop a recursion of moderate depth.
        assert_eq!(
            run(&function(body), r#"<doc n="20"/>"#).unwrap(),
            "<out>20</out>",
            "{what}"
        );
    }
}

/// The same for templates: on the base the review compared against, a named
/// template whose recursive `xsl:call-template` sits inside an `xsl:variable`
/// overflowed the stack before the 40 000-invocation limit, and so did a
/// recursive `xsl:apply-templates` over a document 200 000 elements deep.
/// Built-in template rules (§6.6), which recurse into the children with no
/// template invocation to count, are guarded too.
#[test]
fn recursion_through_templates_ends_in_the_depth_error() {
    let template = |body: &str| {
        sheet(&format!(
            r#"<xsl:template name="down"><xsl:param name="n" as="xs:integer"/>
                 <xsl:variable name="v">{body}</xsl:variable>
                 <xsl:sequence select="xs:integer(string($v)) + 1"/></xsl:template>
               <xsl:template match="/"><out><xsl:call-template name="down">
                 <xsl:with-param name="n" select="0"/></xsl:call-template></out></xsl:template>"#
        ))
    };
    let call = r#"<xsl:call-template name="down"><xsl:with-param name="n" select="$n + 1"/></xsl:call-template>"#;
    for (what, body) in [
        ("a call in a variable", call.to_string()),
        (
            "a call in nested elements and instructions",
            nest(
                10,
                r#"<e><xsl:if test="$n ge 0"><xsl:for-each select="1">"#,
                call,
                "</xsl:for-each></xsl:if></e>",
            ),
        ),
    ] {
        // Unbounded: the parameter only ever grows.
        assert_depth_error(run(&template(&body), "<doc/>"), what);
    }

    let depth = 200_000;
    let deep = format!("{}x{}", "<a>".repeat(depth), "</a>".repeat(depth));
    let applied = sheet(
        r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>
           <xsl:template match="a"><xsl:apply-templates/></xsl:template>"#,
    );
    assert_depth_error(
        run(&applied, &deep),
        "xsl:apply-templates over a deep document",
    );
    let built_in =
        sheet(r#"<xsl:template match="/"><out><xsl:apply-templates/></out></xsl:template>"#);
    assert_depth_error(
        run(&built_in, &deep),
        "the built-in rules over a deep document",
    );
    // A document of moderate depth is processed as before.
    let shallow = format!("{}x{}", "<a>".repeat(100), "</a>".repeat(100));
    assert_eq!(run(&built_in, &shallow).unwrap(), "<out>x</out>");
    assert_eq!(run(&applied, &shallow).unwrap(), "<out>x</out>");
}

/// `TransformOptions::stack_limit`: explicit, and the conservative default on a
/// thread that `run_on_large_stack` did not start — whose stack size the run
/// cannot know (`OTHER_THREAD_STACK_LIMIT`, 1 MiB).
#[test]
fn the_stack_limit_is_configurable_and_conservative_on_other_threads() {
    let main = sheet(
        r#"<xsl:template name="down"><xsl:param name="n" as="xs:integer"/>
             <xsl:call-template name="down"><xsl:with-param name="n" select="$n + 1"/></xsl:call-template></xsl:template>
           <xsl:template match="/"><out><xsl:call-template name="down">
             <xsl:with-param name="n" select="0"/></xsl:call-template></out></xsl:template>"#,
    );
    // Explicit, on the large stack.
    let options = TransformOptions {
        stack_limit: Some(4 * 1024 * 1024),
        ..Default::default()
    };
    let error = run_full(&[], &main, "<doc/>", options).unwrap_err();
    assert_depth_error(Err(error.clone()), "an explicit limit");
    assert!(error.message.contains("more than the 4194304"), "{error}");

    // On a thread of our own (4 MiB, twice what the default limit assumes),
    // with the default options and with an explicit limit.
    let main_for_thread = main.clone();
    let (default_limit, explicit_limit) = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let run_with = |options: TransformOptions| -> Result<String, XsltError> {
                let functions = XsltFunctions::new();
                let names = NameTable::new();
                let compiled =
                    Compiler::new(&names, &functions).compile_str(&main_for_thread, "urn:t")?;
                let arena = Bump::new();
                let outcome = compiled
                    .transformation(&arena, options)
                    .transform_source("urn:in", "<doc/>")?;
                compiled.serialize_primary(&outcome).map(common::body)
            };
            (
                run_with(TransformOptions::default()),
                run_with(TransformOptions {
                    stack_limit: Some(256 * 1024),
                    ..Default::default()
                }),
            )
        })
        .unwrap()
        .join()
        .unwrap();
    let default_limit = default_limit.unwrap_err();
    assert_depth_error(Err(default_limit.clone()), "the default on another thread");
    assert!(
        default_limit.message.contains(&format!(
            "more than the {}",
            xslt2::OTHER_THREAD_STACK_LIMIT
        )),
        "{default_limit}"
    );
    let explicit_limit = explicit_limit.unwrap_err();
    assert!(
        explicit_limit.message.contains("more than the 262144"),
        "{explicit_limit}"
    );
}

/// Q33, review note: a direct call of the public `Transformation::matches`
/// evaluates a pattern's predicates — and the stylesheet functions they call —
/// outside a run, so the stack guard must be on there too. On an ordinary
/// 2 MiB thread, `r[f:g(1000)]` used to recurse past the end of the stack and
/// abort the process; it now ends in the depth error, as the same stylesheet
/// does through `transform_document`.
#[test]
fn a_direct_pattern_match_is_under_the_stack_guard() {
    let main = sheet(
        r#"<xsl:function name="f:g" as="xs:boolean">
             <xsl:param name="n" as="xs:integer"/>
             <xsl:sequence select="if ($n = 0) then true() else f:g($n - 1)"/>
           </xsl:function>
           <xsl:template match="r[f:g(1000)]">deep</xsl:template>"#,
    );
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || -> Result<bool, XsltError> {
            let functions = XsltFunctions::new();
            let names = NameTable::new();
            let compiled = Compiler::new(&names, &functions).compile_str(&main, "urn:t")?;
            let pattern = compiled
                .templates()
                .iter()
                .find_map(|rule| rule.pattern)
                .map(|id| compiled.pattern(id))
                .expect("the r[f:g(1000)] rule");
            let arena = Bump::new();
            let run = compiled.transformation(&arena, TransformOptions::default());
            let doc = run.load_document("urn:in", "<r/>")?;
            let mut node = run
                .store()
                .borrow()
                .get(doc)
                .expect("loaded")
                .doc
                .create_navigator();
            assert!(node.move_to_first_child(), "the r element");
            // A second call, after the first has left its scope, is guarded
            // just the same.
            let first = run.matches(pattern, &node);
            assert!(first.is_err(), "{first:?}");
            run.matches(pattern, &node)
        })
        .unwrap()
        .join()
        .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.code, "XSLT2-INTERNAL", "{error}");
    assert!(error.message.contains("recursing without end"), "{error}");
}

/// Review note: §3.2 — "\[ERR XTSE0080\] It is a static error to use a reserved
/// namespace in the name of a named template, a mode, an attribute set, a key,
/// a decimal-format, a variable or parameter, a stylesheet function, a named
/// output definition, or a character map." A function parameter is a
/// parameter, so `<xsl:param name="xsl:p"/>` in a function is XTSE0080 — and so
/// is every other variable, parameter, mode and named output that is declared
/// in a reserved namespace. (Keys, decimal-formats and character maps come
/// with M2f, M3d and M3a.) The `xml:` attributes of an instruction are not
/// declared names and stay valid.
#[test]
fn xtse0080_variables_parameters_modes_and_outputs_in_a_reserved_namespace() {
    let function_param = sheet(
        r#"<xsl:function name="f:id"><xsl:param name="xsl:p"/><xsl:sequence select="$xsl:p"/></xsl:function>
           <xsl:template match="/"><out><xsl:value-of select="f:id(1)"/></out></xsl:template>"#,
    );
    assert_code(&function_param, "XTSE0080");
    let cases = [
        (
            "a global variable",
            r#"<xsl:variable name="xml:v" select="1"/><xsl:template match="/"><out/></xsl:template>"#,
        ),
        (
            "a global parameter",
            r#"<xsl:param name="xsl:g"/><xsl:template match="/"><out/></xsl:template>"#,
        ),
        (
            "a template parameter",
            r#"<xsl:template match="/"><xsl:param name="fn:t"/><out/></xsl:template>"#,
        ),
        (
            "a local variable",
            r#"<xsl:template match="/"><xsl:variable name="xs:v" select="1"/><out/></xsl:template>"#,
        ),
        (
            "a mode of a template rule",
            r#"<xsl:template match="/" mode="xsl:m"><out/></xsl:template>"#,
        ),
        (
            "a mode of xsl:apply-templates",
            r#"<xsl:template match="/"><out><xsl:apply-templates mode="xsl:m"/></out></xsl:template>"#,
        ),
        (
            "a named output definition",
            r#"<xsl:output name="xsl:o"/><xsl:template match="/"><out/></xsl:template>"#,
        ),
    ];
    for (what, body) in cases {
        let main = format!(
            r#"<xsl:stylesheet version="2.0" xmlns:xsl="{NS}"
                 xmlns:xs="http://www.w3.org/2001/XMLSchema"
                 xmlns:fn="http://www.w3.org/2005/xpath-functions">{body}</xsl:stylesheet>"#
        );
        assert_eq!(error_of(&main).code, "XTSE0080", "{what}");
    }
    // Names outside the reserved namespaces, and `xml:` attributes, are fine.
    let fine = sheet(
        r#"<xsl:function name="f:id"><xsl:param name="f:p"/><xsl:sequence select="$f:p"/></xsl:function>
           <xsl:template match="/" mode="f:m"><out xml:lang="en"><xsl:value-of select="f:id(1)"/></out></xsl:template>
           <xsl:template match="/"><xsl:variable name="f:v" select="2"/><xsl:apply-templates select="." mode="f:m"/></xsl:template>"#,
    );
    assert_eq!(
        run(&fine, "<doc/>").unwrap(),
        r#"<out xml:lang="en">1</out>"#
    );
}
