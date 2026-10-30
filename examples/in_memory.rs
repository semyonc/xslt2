//! A stylesheet with an included module and a lookup document, all held in
//! memory, run over a source given as a string, with a parameter.
//!
//! ```text
//! cargo run --example in_memory
//! ```

use std::sync::Arc;

use xslt2::{
    Compiler, MemoryResolver, NameTable, QName, TransformOptions, XPathValue, XsltError,
    XsltFunctions,
};

const MAIN: &str = r#"<xsl:stylesheet version="2.0"
    xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:include href="format.xsl"/>
  <xsl:output method="text"/>
  <xsl:param name="currency" select="'EUR'"/>
  <xsl:variable name="rates" select="document('rates.xml')/rates"/>
  <xsl:template match="/order">
    <xsl:variable name="rate" select="number($rates/rate[@currency = $currency])"/>
    <xsl:for-each select="item">
      <xsl:value-of select="@name"/>
      <xsl:text>: </xsl:text>
      <xsl:call-template name="money">
        <xsl:with-param name="amount" select="@price * $rate"/>
      </xsl:call-template>
      <xsl:text>&#10;</xsl:text>
    </xsl:for-each>
  </xsl:template>
</xsl:stylesheet>"#;

const FORMAT: &str = r#"<xsl:stylesheet version="2.0"
    xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:template name="money">
    <xsl:param name="amount"/>
    <xsl:value-of select="format-number($amount, '#,##0.00'), $currency"/>
  </xsl:template>
</xsl:stylesheet>"#;

const RATES: &str =
    r#"<rates><rate currency="EUR">1</rate><rate currency="CHF">0.94</rate></rates>"#;

const ORDER: &str =
    r#"<order><item name="Tea" price="4.5"/><item name="Cups" price="1200"/></order>"#;

fn main() -> Result<(), XsltError> {
    // Every resource the stylesheet reads, by absolute URI: the module, the
    // module it includes, and the document `document()` reads. Relative
    // references resolve against the module's URI.
    let resolver = Arc::new(
        MemoryResolver::new()
            .with("http://example.com/shop/main.xsl", MAIN)
            .with("http://example.com/shop/format.xsl", FORMAT)
            .with("http://example.com/shop/rates.xml", RATES),
    );

    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let sheet = Compiler::new(&names, &functions)
        .with_resolver(resolver)
        .compile_uri("http://example.com/shop/main.xsl")?;

    // Every run reads `document()` through the resolver the stylesheet was
    // compiled with.
    for currency in ["EUR", "CHF"] {
        let arena = xslt2::bumpalo::Bump::new();
        let outcome = sheet
            .transformation(&arena, TransformOptions::default())
            .with_param(QName::local("currency"), XPathValue::from(currency))
            .transform_source("urn:example:order", ORDER)?;
        print!("{}", sheet.serialize_primary(&outcome)?);
    }
    Ok(())
}
