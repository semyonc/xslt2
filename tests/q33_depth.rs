//! Q33: how deep a recursive named template can go before the host stack runs
//! out, M1 having no tail-call elimination.
//!
//! The measurement, on this machine with the default 256 MiB worker stack:
//! **50 000 levels succeed, 55 000 overflow** — roughly 5 KiB of stack per
//! template call. A 1 GiB stack reaches 100 000. `TransformOptions::
//! max_recursion_depth` defaults to 40 000 so that a runaway recursion ends in
//! an error a caller can catch rather than in a process abort it cannot.
//!
//! The test itself runs a modest depth so that `cargo test` stays quick; set
//! `Q33_DEPTH` and `Q33_STACK` to repeat the measurement.
use bumpalo::Bump;
use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, TransformOptions, XsltFunctions};

fn run(depth: u64) -> Result<String, xslt2::XsltError> {
    let s = format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
             xmlns:xs="http://www.w3.org/2001/XMLSchema"
             exclude-result-prefixes="xs">
             <xsl:template match="/"><out><xsl:call-template name="down">
               <xsl:with-param name="n" select="{depth}"/></xsl:call-template></out></xsl:template>
             <xsl:template name="down"><xsl:param name="n" as="xs:integer"/>
               <xsl:choose>
                 <xsl:when test="$n > 0"><xsl:call-template name="down">
                   <xsl:with-param name="n" select="$n - 1"/></xsl:call-template></xsl:when>
                 <xsl:otherwise><bottom/></xsl:otherwise>
               </xsl:choose>
             </xsl:template>
           </xsl:stylesheet>"#
    );
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions).compile_str(&s, "urn:q33")?;
    let arena = Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run.transform_source("urn:in", "<doc/>")?;
    compiled.serialize_primary(&outcome)
}

#[test]
fn measure() {
    let target: u64 = std::env::var("Q33_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        // A debug build's frames are several times larger, so the default
        // depth is chosen to fit both profiles; `Q33_DEPTH` repeats the real
        // measurement in `--release`.
        .unwrap_or(if cfg!(debug_assertions) { 2_000 } else { 10_000 });
    let stack: usize = std::env::var("Q33_STACK").ok().and_then(|v| v.parse().ok())
        .unwrap_or(xslt2::DEFAULT_STACK_SIZE);
    let out = xslt2::run_on_large_stack(stack, move || run(target));
    let serialized = out.expect("a recursion of this depth fits on the stack");
    assert_eq!(serialized, "<out><bottom/></out>");
    println!("Q33: depth {target} on a {stack}-byte stack: ok");
}
