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
    /// Asks the processor whether it formats every number of a request with the numbering
    /// sequence of the request's token — the `combinations_for_numbering` dependency
    /// ([`numbering_supported`]). The driver passes `engine::numbering_probe`, which compiles and
    /// runs a probe stylesheet through the public API: what the processor supports is never a
    /// second table here.
    pub numbering: fn(&NumberingRequest) -> Result<(), String>,
}

/// One `combinations_for_numbering` question for the processor: does
/// `<xsl:number value="n" format="{token}" letter-value="{letter_value}"/>` format every `n` of
/// `numbers` with the sequence of `token`, rather than fall back to the token `1` (XSLT 2.0
/// §12.3: "If an implementation does not support a numbering sequence represented by the given
/// token, it must use a format token of 1", and "Numbers that fall outside this range must be
/// formatted using the format token 1")?
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NumberingRequest {
    pub token: char,
    /// `alphabetic` or `traditional`, from a value like `GREEK SMALL LETTER ALPHA (TRADITIONAL)`.
    pub letter_value: Option<String>,
    pub numbers: Vec<u64>,
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

/// Cases excluded with a documented reason: the expected result is wrong for a conformant XSLT
/// **2.0** processor, or pins a choice the 2.0 specifications leave to the implementation, or —
/// one entry, `regex-syntax-xslt20-0987`, by project decision — pins a dated (Unicode 2.0)
/// character class that the regex backend does not implement. Every entry cites the text that
/// makes it so (XSLT 2.0 = `xslt20.html`, XPath 2.0 = `xpath2.html`; the M2j reports have the
/// full quotations and the evidence). Other processors' results are never the reason. `--all`
/// runs them anyway.
pub const EXCLUDED: &[(&str, &str)] = &[
    // ── XPath 3.0 syntax evaluated by the stylesheet ──
    ("match-248", X_SIMPLE_MAP),
    ("match-254", X_SIMPLE_MAP),
    ("function-0302", "XPath 2.0 has no `||` operator (A.1 EBNF); in a version=\"2.0\" stylesheet XSLT 2.0 §5.3 makes an attribute that does not match the XPath production Expr a static error"),
    ("for-each-group-084", "version=\"3.0\" stylesheet whose xsl:for-each-group body evaluates <xsl:comment select=\"'DEBUG',last(),position(),current-group()!string(.)\"/> for every group: the XPath 3.0 simple-map operator `!`, which the XPath 2.0 grammar does not have (A.1 EBNF [26] RelativePathExpr ::= StepExpr ((\"/\" | \"//\") StepExpr)*; `!` occurs only in GeneralComp's \"!=\"; XPST0003: \"It is a static error if an expression is not a valid instance of the grammar defined in A.1 EBNF\"). Forwards-compatible behaviour lifts only the static error — §5.3: \"Except where forwards-compatible behavior is enabled (see 3.9 Forwards-Compatible Processing), it is a static error if the value of such an attribute … does not match the XPath production Expr\" — and §3.9 relaxes only XSLT elements and attributes, so the expression still has no XPath 2.0 meaning when it is evaluated, and the expected successful result cannot arise"),
    ("doe-0409", "version=\"3.0\" stylesheet that evaluates <xsl:value-of select=\"'&lt;a&gt;' || . || '&lt;/a&gt;'\"/> for every item of every group: XPath 2.0 has no `||` operator (A.1 EBNF [10] ComparisonExpr ::= RangeExpr ((ValueComp | GeneralComp | NodeComp) RangeExpr)?, [11] RangeExpr ::= AdditiveExpr (\"to\" AdditiveExpr)?; no production contains \"||\"). Forwards-compatible behaviour lifts only the static error (§5.3: \"Except where forwards-compatible behavior is enabled …\"; §3.9 relaxes only XSLT elements and attributes), so the evaluated expression still has no XPath 2.0 meaning; the entry template is also named xsl:initial-template, in a reserved namespace (§3.2 [ERR XTSE0080] \"It is a static error to use a reserved namespace in the name of a named template\")"),
    // ── XSLT 3.0 instructions and declarations in a 2.0 stylesheet ──
    ("collection-005", "xsl:mode is not among the declarations XSLT 2.0 §3.6 permits in xsl:stylesheet; in a version=\"2.0\" stylesheet that is XTSE0010 (\"an XSLT-defined element is used in a context where it is not permitted\"), and the expected result also needs 3.0's on-no-match"),
    ("system-property-022", "xsl:evaluate (an XSLT 3.0 instruction, absent from XSLT 2.0) in a sequence constructor of this version=\"2.0\" stylesheet: XTSE0010 (§2.2), and the §18.2.3 note: \"an unrecognized XSLT element appearing within a sequence constructor is a static error unless (a) forwards-compatible behavior is enabled, and (b) the instruction has an xsl:fallback child\" — neither holds; the expected XTDE3160 is an XSLT 3.0 code"),
    ("number-1004", X_UNKNOWN_INSTRUCTION_20),
    ("collations-0128", X_UNKNOWN_INSTRUCTION_20),
    ("result-document-1006", "current-output-uri() is not an XSLT 2.0 function (§16 / F&O 1.0); calling it is XPST0017 (XPath 2.0 §3.1.5), not the XTRE1495 / XTDE1490 the case accepts"),
    ("sequence-0132", "XSLT 2.0 §11.10: xsl:sequence is <xsl:sequence select = expression> <!-- Content: xsl:fallback* -->: a sequence constructor child and a missing select are XTSE0010 (the stylesheet's own <?error XTSE0010?> says so); XTTE0570 is the XSLT 3.0 reading"),
    ("namespace-0912", "version=\"3.0\": under §3.9 the top-level xsl:mode \"must be ignored\", so §6.6's built-in rules apply, $var receives the w:s element, and as=\"element(p)\" raises XTTE0570 (§9.1); the expected tree needs 3.0's on-no-match=\"shallow-copy\""),
    ("variable-4802", "version=\"3.0\" expand-text=\"yes\": §3.9 ignores the unknown attribute (and the xsl:mode declaration), so <out>{.}</out> is literal text; the assertion needs XSLT 3.0 text value templates"),
    // ── XSLT 3.0 attributes and names in a 2.0 stylesheet ──
    ("function-1902", "expand-text on xsl:message is not an XSLT 2.0 attribute: XTSE0090 (§3.3: \"an attribute whose namespace is … null … other than attributes defined for the element in this document\") in this version=\"2.0\" stylesheet"),
    ("output-0724", X_OUTPUT_30),
    ("output-0725", X_OUTPUT_30),
    ("output-0726", X_OUTPUT_30),
    ("result-document-0286", X_EQNAME_OUTPUT),
    ("result-document-0287", X_EQNAME_OUTPUT),
    ("strip-space-025", "§4.4: the elements attribute is \"a whitespace-separated list of NameTests\" (XPath 2.0); Q{}test1 is an XPath 3.0 EQName, so stripping test1 is not a 2.0 behaviour"),
    // ── expected results that §4.2 or §5.7.3 rule out ──
    ("as-1303", X_STYLESHEET_WHITESPACE),
    ("as-1304", X_STYLESHEET_WHITESPACE),
    ("attribute-set-1508", X_STYLESHEET_WHITESPACE),
    ("attribute-set-1509", X_STYLESHEET_WHITESPACE),
    ("attribute-0004", "the expected assert-xml pins the invented prefix p_1: §11.3 \"this prefix may subsequently be added, changed, or removed during the namespace fixup process\", and §5.7.3 \"the actual namespace nodes that are added to the tree by the namespace fixup process are implementation-dependent\""),
    // ── expected results that §4.4 and §6.6 rule out (source whitespace that the stylesheet copies) ──
    ("for-each-group-085", "the source has whitespace-only text nodes before each <t> child of <c>; the template for /c is <xsl:apply-templates> with the default select (child::node()), so they reach the built-in rule, and §6.6: \"The built-in template rule for text and attribute nodes returns a text node containing the string value of the context node\"; the stylesheet has no xsl:strip-space, and §4.4 keeps them: an element name is whitespace-preserving \"if and only if there is no match or the best match is an xsl:preserve-space element\". The expected assert-xml has no whitespace, and the catalog compares assert-xml after canonicalization \"byte-for-byte\" (or by fn:deep-equal), in which whitespace text nodes count"),
    // ── implementation-dependent cases, and expectations that are not XSLT 2.0's (M2e review) ──
    ("param-0301", "§9.8 uses this very stylesheet as its example of an implementation-dependent case: \"the function declares a local variable $b, but it returns a result that does not require the variable to be evaluated. It is implementation-dependent whether the value is actually evaluated, and it is therefore implementation-dependent whether the circularity is signaled as an error\"; this processor evaluates $b and signals XTDE0640 (\"In general, a circularity in a stylesheet is a non-recoverable dynamic error\"), which the case does not accept"),
    ("copy-4307", X_COPY_WITHOUT_FOCUS),
    ("copy-4309", X_COPY_WITHOUT_FOCUS),
    // ── M3e residue: an expectation that is XSLT 3.0's ──
    ("next-match-039", "the template rule matches one|one with two alternatives of the same default priority (0.5) and calls xsl:next-match; the case expects the rule to run twice. XSLT 2.0 §6.7 rules that out: xsl:next-match \"considers all template rules with lower import precedence than the current template rule, plus the template rules that are at the same import precedence that have lower priority than the current template rule\" (and earlier same-priority rules only after recovering from XTRE0540, which §6.4 says does not arise here: \"it is not an error if a node matches more than one of the alternatives\"), and its note: \"where the same node matches more than one alternative, and the alternatives have different priority, it is possible for an xsl:next-match instruction to cause the current template rule to be invoked recursively. This situation does not occur when the alternatives have the same priority.\" The stylesheet's own comment says the second alternative should be matched first, which is the XSLT 3.0 ordering"),
    ("format-number-070", "the catalog names <initial-template name=\"main\"/>, and the stylesheet declares no template named main, so §2.3 applies: \"[ERR XTDE0040] It is a non-recoverable dynamic error if the invocation of the stylesheet specifies a template name that does not match the expanded-QName of a named template defined in the stylesheet.\" (The catalog limits the case to XSLT20, yet the stylesheet says version=\"3.0\")"),
    // ── Unicode-version-dependent regex classes ──
    ("regex-syntax-xslt20-0984", "expects U+2308/U+2309 in \\w = [#x0000-#x10FFFF]-[\\p{P}\\p{Z}\\p{C}] (XSD 1.0 2E §F.1.1); in Unicode 16.0 they are Ps/Pe, i.e. \\p{P} (verified with Python unicodedata 16.0.0), so the case pins an older Unicode (that they were Sm until Unicode 6.3: INFERRED, not checked against the historical data). The Unicode version is implementation-defined, the latest recommended (F&O 1.0 §1.1; XSLT 2.0 [Unicode] reference)"),
    ("regex-syntax-xslt20-0985", "expects U+1369..U+1371 in \\d = \\p{Nd} (XSD 1.0 2E §F.1.1); in Unicode 16.0 they are No (verified with Python unicodedata 16.0.0), so the case pins an older Unicode (that they were Nd until Unicode 4.1: INFERRED, not checked against the historical data). The Unicode version is implementation-defined, the latest recommended (F&O 1.0 §1.1; XSLT 2.0 [Unicode] reference)"),
    ("regex-syntax-xslt20-0987", "Unicode-vintage character class, excluded by project decision (2026-10-03, cluster L-REGEX-UNICODE-VINTAGE with 0984/0985). The case expects U+0346 outside \\c, which pins \\c to the NameChar of XML 1.0 Second Edition: F&O 1.0 2E §7.6.1 \"The regular expression syntax and semantics are identical to those defined in [XML Schema Part 2: Datatypes Second Edition]\", whose Appendix F reads \"\\c the set of name characters, those ·match·ed by NameChar\" with a dated link to XML 1.0 2E (WD-xml-2e-20000814); XML 1.0 2E (REC-xml-20001006): NameChar ::= Letter | Digit | '.' | '-' | '_' | ':' | CombiningChar | Extender, CombiningChar ::= [#x0300-#x0345] | …, classes that \"can be derived from the Unicode 2.0 character database\" (all read at w3.org; that the dated 2000-08-14 draft has the same production: INFERRED). U+0346 lies outside it. The regex backend (regexml) uses the XML 1.0 Fifth Edition name characters, [4a] NameChar ::= … [#x0300-#x036F] …, where U+0346 is a name character. Under that normative chain the expectation is the strict XPath 2.0 one, so this is a deliberate exclusion of a dated character class, not a wrong expectation; the strict alternative (\\c and \\i per XML 1.0 2E in the XPath 2.0 regex dialect) is a regex-dialect item outside M2h, kept excluded by decision"),
    // ── misc/regex-classes: general categories checked against Unicode 3.1 / 5.2 / 6.0 data (user decision 2026-10-10) ──
    ("regex-classes-001", X_REGEX_CLASSES_UNICODE_NONCHAR), // C
    ("regex-classes-003", X_REGEX_CLASSES_UNICODE), // Cf
    ("regex-classes-004", X_REGEX_CLASSES_UNICODE_NONCHAR), // Cn
    ("regex-classes-006", X_REGEX_CLASSES_UNICODE), // L
    ("regex-classes-007", X_REGEX_CLASSES_UNICODE), // Ll
    ("regex-classes-008", X_REGEX_CLASSES_UNICODE), // Lm
    ("regex-classes-009", X_REGEX_CLASSES_UNICODE), // Lo
    ("regex-classes-011", X_REGEX_CLASSES_UNICODE), // Lu
    ("regex-classes-012", X_REGEX_CLASSES_UNICODE), // M
    ("regex-classes-013", X_REGEX_CLASSES_UNICODE), // Mc
    ("regex-classes-014", X_REGEX_CLASSES_UNICODE), // Me
    ("regex-classes-015", X_REGEX_CLASSES_UNICODE), // Mn
    ("regex-classes-016", X_REGEX_CLASSES_UNICODE), // N
    ("regex-classes-017", X_REGEX_CLASSES_UNICODE), // Nd
    ("regex-classes-018", X_REGEX_CLASSES_UNICODE), // Nl
    ("regex-classes-019", X_REGEX_CLASSES_UNICODE), // No
    ("regex-classes-020", X_REGEX_CLASSES_UNICODE), // P
    ("regex-classes-022", X_REGEX_CLASSES_UNICODE), // Pd
    ("regex-classes-023", X_REGEX_CLASSES_UNICODE), // Pe
    ("regex-classes-024", X_REGEX_CLASSES_UNICODE_NONCHAR), // C
    ("regex-classes-026", X_REGEX_CLASSES_UNICODE), // Po
    ("regex-classes-027", X_REGEX_CLASSES_UNICODE), // Ps
    ("regex-classes-028", X_REGEX_CLASSES_UNICODE), // S
    ("regex-classes-029", X_REGEX_CLASSES_UNICODE), // Sc
    ("regex-classes-030", X_REGEX_CLASSES_UNICODE), // Sk
    ("regex-classes-031", X_REGEX_CLASSES_UNICODE), // Sm
    ("regex-classes-032", X_REGEX_CLASSES_UNICODE), // So
    ("regex-classes-033", X_REGEX_CLASSES_UNICODE), // Z
    ("regex-classes-036", X_REGEX_CLASSES_UNICODE), // Zs
    // ── decl/character-map: XSLT 3.0 serialization in a stylesheet labelled XSLT20+ ──
    ("character-map-026", "a version=\"3.0\" stylesheet labelled XSLT20+: xsl:output method=\"adaptive\" is XTSE1570 in XSLT 2.0 (§20: an unprefixed method \"must be one of xml, html, xhtml, or text\"), the template is named xsl:initial-template (XTSE0080: \"It is a static error to use a reserved namespace in the name of a named template\"), and map { … } is an XPath 3.1 map constructor, absent from the XPath 2.0 grammar; the expected adaptive serialization cannot arise from an XSLT 2.0 processor"),
    // ── misc/unicode-90: the sixteen version="3.0" cases (the other 1,444 are 2.0) ──
    ("unicode90-001", X_UNICODE90),
    ("unicode90-002", X_UNICODE90),
    ("unicode90-003", X_UNICODE90),
    ("unicode90-004", X_UNICODE90),
    ("unicode90-005", X_UNICODE90),
    ("unicode90-006", X_UNICODE90),
    ("unicode90-007", X_UNICODE90),
    ("unicode90-008", X_UNICODE90),
    ("unicode90-100", X_UNICODE90),
    ("unicode90-101", X_UNICODE90),
    ("unicode90-102", X_UNICODE90),
    ("unicode90-103", X_UNICODE90),
    ("unicode90-104", X_UNICODE90),
    ("unicode90-105", X_UNICODE90),
    ("unicode90-106", X_UNICODE90),
    ("unicode90-107", X_UNICODE90),
];

const X_UNKNOWN_INSTRUCTION_20: &str = "an XSLT 3.0 instruction (xsl:iterate / xsl:on-completion, xsl:evaluate; absent from XSLT 2.0) in a sequence constructor of this version=\"2.0\" stylesheet, without xsl:fallback: XTSE0010 (§2.2: \"A static error is signaled if an XSLT-defined element is used in a context where it is not permitted\"), and the §18.2.3 note: \"an unrecognized XSLT element appearing within a sequence constructor is a static error unless (a) forwards-compatible behavior is enabled, and (b) the instruction has an xsl:fallback child\" — neither holds, so the expected result or dynamic error cannot arise";
const X_SIMPLE_MAP: &str = "the stylesheet evaluates the XPath 3.0 simple-map operator `!`, which XPath 2.0 does not have (A.1 EBNF [21] ValueExpr ::= PathExpr; `!` occurs only in `!=`); XSLT 2.0 §5.3: an attribute that does not match Expr is a static error, and with forwards-compatible behaviour the expression is still evaluated by the entry template";
const X_OUTPUT_30: &str = "xsl:output html-version / suppress-indentation are not XSLT 2.0 attributes (XTSE0090, §3.3, in this version=\"2.0\" stylesheet), and the template name xsl:initial-template is in a reserved namespace: XTSE0080 (§3.2)";
const X_EQNAME_OUTPUT: &str = "<xsl:output name=\"Q{…}format2\"> in a version=\"2.0\" stylesheet: §5.1 \"A QName is always written in the form (NCName \":\")? NCName\", so the EQName is XTSE0020 (§2.2: not one of the permitted values)";
const X_STYLESHEET_WHITESPACE: &str = "the expected assert-xml keeps whitespace-only text that can only come from the stylesheet, which §4.2 removes (\"Any whitespace text node that satisfies both the following conditions is removed from the tree\"); assert-xml compares canonical forms byte for byte (catalog schema), so no conformant processor produces it";
const X_COPY_WITHOUT_FOCUS: &str = "xsl:copy in the body of a stylesheet function (copy903err.xsl), where §10.3 says \"Within the body of a stylesheet function, the focus is initially undefined; this means that any attempt to reference the context item, context position, or context size is a non-recoverable dynamic error. [XPDY0002]\" — and xsl:copy copies the context item (§11.9.1: \"The xsl:copy instruction provides a way of copying the context item\"). The expected XTTE0945 is an XSLT 3.0 code: it occurs nowhere in the XSLT 2.0 Recommendation, Appendix E (Summary of Error Conditions) included";
const X_UNICODE90: &str = "version=\"3.0\" stylesheet (unicode90-001.xsl / -002.xsl): the entry point is a template named xsl:initial-template (XTSE0080 under XSLT 2.0), the regex arrives as a static parameter (3.0), and the select uses the XPath 3.0 simple-map operator `!` (absent from the XPath 2.0 grammar)";

/// The text both `regex-classes` justifications share (a macro, so that the second can extend
/// it with `concat!`).
macro_rules! regex_classes_unicode {
    () => {
        "misc/regex-classes: reclass.xsl matches \\p{X}+ and \\P{X}+ over all 1,112,033 legal code points (9 to 10, 13, 32 to 55295, 57344 to 65533, 65536 to 1114111) and returns <true/> only when the ranges it finds deep-equal one of its reference files, which exist for Unicode 3.1, 5.2 and 6.0 — its own comment: \"Alternative results are acceptable depending on which version of Unicode the processor supports\"; the set declares <unicode-version value=\"6.0\" satisfied=\"true\"/>. The regex backend (regexml on ICU4X 1.5) implements Unicode 15.1 (established at M2h, XSLT2_M2H_REPORT.md §6 and its review: for the 14 classes compared code point by code point, the output equals Python's Unicode 16.0 data except for the characters added in 16.0 and U+1171E, recategorized in 16.0), and every code point at which the result differs from the 6.0 reference is a character assigned or recategorized after Unicode 6.0, so it matches none of the three references. The Unicode version is implementation-defined, the latest recommended (F&O 1.0 §1.1; XSLT 2.0 [Unicode] reference). The 84 block classes (IsBasicLatin …) and the 7 general categories whose result still equals a reference (Cc, Co, Lt, Pc, Pi, Zl, Zp) still run"
    };
}
const X_REGEX_CLASSES_UNICODE: &str = regex_classes_unicode!();
const X_REGEX_CLASSES_UNICODE_NONCHAR: &str = concat!(
    regex_classes_unicode!(),
    ". For C and Cn (cases 001 and 024 test C, 004 Cn) one more difference has nothing to do with later assignments: the 6.0 reference leaves the 64 noncharacters among the legal code points (U+FDD0 to U+FDEF, and U+nFFFE / U+nFFFF for n = 1 to 16; U+FFFE and U+FFFF are not in the tested set) out of the class, while the 3.1 and 5.2 references include them, as the data of every Unicode version does (noncharacters are General_Category Cn: so in the 3.1 and 5.2 references, and in Python unicodedata 16.0.0 and 3.2.0). No version's data reproduces the 6.0 reference for these classes, and data newer than 5.2 cannot reproduce the 3.1 or 5.2 references"
);

/// Cases whose expected result is a document node with several top-level nodes although the
/// catalog leaves `<output well-formed>` at its default, "yes" ("Determines whether the output
/// will be well-formed XML. The default is yes.", `admin/catalog-schema.xsd`). The harness
/// rebuilds the document node of a non-well-formed result only where the case allows it
/// (`compare::CompareContext::entity_results`), so each entry is a correction of the catalog,
/// with the case's own words as the evidence.
pub const UNDECLARED_NOT_WELL_FORMED: &[(&str, &str)] = &[(
    "xsl-document-0501",
    "the description says the case verifies \"that creation of multiple elements at the top \
     level (directly under the document node) is not an error\", and its six assertions \
     /foo[1] = \"one\" … /foo[6] = \"six\" hold only over a document node with six element \
     children, which no well-formed XML document has (XML 1.0 §2.1: \"There is exactly one \
     element, called the root, or document element\")",
)];

/// Whether the case may produce a result that is not a well-formed document: it says so with
/// `<output well-formed="no"/>`, or it is on [`UNDECLARED_NOT_WELL_FORMED`].
pub fn result_may_be_entity(case: &TestCase) -> bool {
    case.test.output.as_ref().and_then(|o| o.well_formed) == Some(false)
        || UNDECLARED_NOT_WELL_FORMED
            .iter()
            .any(|(name, _)| *name == case.name)
}

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
        Some(EnvRef::Ref(r)) => set
            .environments
            .get(r)
            .map(|e| e.has_package)
            .unwrap_or(false),
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
        if d.kind == "combinations_for_numbering" {
            let range = d.range.as_deref().unwrap_or("unbounded");
            match (numbering_supported(d, policy), d.satisfied) {
                (Ok(()), false) => {
                    return Applicability::Unsatisfied(format!(
                        "{}={} ({range}) is supported but the test needs its absence",
                        d.kind, d.value
                    ))
                }
                (Err(why), true) => {
                    return Applicability::Unsatisfied(format!(
                        "{}={} ({range}) not supported: {why}",
                        d.kind, d.value
                    ))
                }
                _ => continue,
            }
        }
        match dependency_supported(d, policy) {
            Some(true) if !d.satisfied => {
                return Applicability::Unsatisfied(format!(
                    "{}={} is supported but the test needs its absence",
                    d.kind, d.value
                ));
            }
            Some(false) if d.satisfied => {
                return Applicability::Unsatisfied(format!("{}={} not supported", d.kind, d.value));
            }
            None => {
                return Applicability::Unsatisfied(format!(
                    "unknown dependency {}={}",
                    d.kind, d.value
                ))
            }
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
            "serialization"
            | "backwards_compatibility"
            | "namespace_axis"
            | "disabling_output_escaping"
            | "built_in_derived_types" => true,
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
        "unparsed_text_encoding" => {
            v.eq_ignore_ascii_case("UTF-8") || v.eq_ignore_ascii_case("UTF-16")
        }
        // Q23: English-only xsl:number words/ordinals in v1.
        "default_language_for_numbering" | "languages_for_numbering" => v == "en",
        // Q24: Gregorian ("AD") and ISO calendars only.
        "supported_calendars_in_date_formatting_functions"
        | "default_calendar_in_date_formatting_functions" => v == "AD" || v == "ISO",
        // XTRE0540 policy: recover by default, signal in --strict mode.
        "on-multiple-match" => match v {
            "error" => policy.strict_recoverable,
            "recover" => !policy.strict_recoverable,
            _ => false,
        },
        // xs:decimal is rust_decimal (28 significant digits).
        "maximum_number_of_decimal_digits" => v.parse::<u32>().map(|n| n <= 28).unwrap_or(false),
        // `combinations_for_numbering` is asked of the processor: `numbering_supported`.
        // CLDR ordinals, streaming, packages, extension functions, non-codepoint collations, HTML
        // versions: not supported now.
        "ordinal_scheme_name"
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

/// The characters the catalog's `combinations_for_numbering` values name: the enumeration of
/// `admin/catalog-schema.xsd` ("The current list uses the Unicode names of the digit one values of
/// non-digit digits in the Number, Other category"; the Greek pair adds a letter-value in
/// parentheses). The harness has no Unicode name database, so this transcribes the names — the
/// catalog's vocabulary, not what the processor supports, which `Policy::numbering` asks the
/// processor. A value missing here is reported as not supported, naming the value.
pub const NUMBERING_NAMES: &[(&str, char)] = &[
    ("CIRCLED DIGIT ONE", '\u{2460}'),
    ("DOUBLE CIRCLED DIGIT ONE", '\u{24F5}'),
    ("DIGIT ONE FULL STOP", '\u{2488}'),
    ("PARENTHESIZED DIGIT ONE", '\u{2474}'),
    ("SUBSCRIPT ONE", '\u{2081}'),
    ("SUPERSCRIPT ONE", '\u{B9}'),
    ("TIBETAN DIGIT HALF ONE", '\u{F2A}'),
    ("ETHIOPIC DIGIT ONE", '\u{1369}'),
    ("DINGBAT NEGATIVE CIRCLED DIGIT ONE", '\u{2776}'),
    ("DINGBAT CIRCLED SANS-SERIF DIGIT ONE", '\u{2780}'),
    ("DINGBAT NEGATIVE CIRCLED SANS-SERIF DIGIT ONE", '\u{278A}'),
    ("PARENTHESIZED IDEOGRAPH ONE", '\u{3220}'),
    ("CIRCLED IDEOGRAPH ONE", '\u{3280}'),
    ("AEGEAN NUMBER ONE", '\u{10107}'),
    ("COPTIC EPACT DIGIT ONE", '\u{102E1}'),
    ("OLD ITALIC NUMERAL ONE", '\u{10320}'),
    ("RUMI DIGIT ONE", '\u{10E60}'),
    ("BRAHMI NUMBER ONE", '\u{11052}'),
    ("SINHALA ARCHAIC DIGIT ONE", '\u{111E1}'),
    ("MENDE KIKAKUI DIGIT ONE", '\u{1E8C7}'),
    ("COUNTING ROD UNIT DIGIT ONE", '\u{1D360}'),
    ("DIGIT ONE COMMA", '\u{1F102}'),
    ("GREEK SMALL LETTER ALPHA", '\u{3B1}'),
];

/// The request a `combinations_for_numbering` dependency makes of the processor (its value and
/// its `range`), or why it cannot be made.
pub fn numbering_request(d: &Dependency) -> Result<NumberingRequest, String> {
    let (name, letter_value) = match d.value.strip_suffix(')').and_then(|v| v.split_once(" (")) {
        Some((name, qualifier)) => (name, Some(qualifier.to_ascii_lowercase())),
        None => (d.value.as_str(), None),
    };
    let token = NUMBERING_NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, token)| token)
        .ok_or_else(|| format!("the harness knows no character named {name:?}"))?;
    Ok(NumberingRequest {
        token,
        letter_value,
        numbers: range_numbers(d.range.as_deref())?,
    })
}

/// The numbers a `range` asks to be formatted. `"a to b"`: each of them (the schema's ranges run
/// to 1000 at most; a longer one is sampled — its ends and a thousand steps between). An
/// `unbounded` range (also the schema's default, the attribute absent) cannot be checked by any
/// finite probe: 1 to 1000 (the least upper bound §12.3 allows for the sequences it describes:
/// "any upper bound imposed by the implementation must not be less than 1000") and 2^32 - 1 —
/// enough to reject every sequence with a natural end.
pub fn range_numbers(range: Option<&str>) -> Result<Vec<u64>, String> {
    let words: Vec<&str> = xslt2::whitespace::tokens(range.unwrap_or("unbounded")).collect();
    let (low, high) = match words.as_slice() {
        ["unbounded"] => return Ok((1..=1000).chain([u64::from(u32::MAX)]).collect()),
        [low, "to", high] => match (low.parse::<u64>(), high.parse::<u64>()) {
            (Ok(low), Ok(high)) if low <= high => (low, high),
            _ => {
                return Err(format!(
                    "the range {range:?} has no bounds a to b with a <= b"
                ))
            }
        },
        _ => {
            return Err(format!(
                "the range {range:?} is not \"a to b\" or \"unbounded\""
            ))
        }
    };
    let step = ((high - low) / 1000).max(1);
    let mut numbers: Vec<u64> = (low..=high).step_by(step as usize).collect();
    if numbers.last() != Some(&high) {
        numbers.push(high);
    }
    Ok(numbers)
}

/// Whether the processor formats the numbers of a `combinations_for_numbering` dependency with its
/// sequence (`Err`: why not, or why the question cannot be asked).
pub fn numbering_supported(d: &Dependency, policy: &Policy) -> Result<(), String> {
    (policy.numbering)(&numbering_request(d)?)
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
