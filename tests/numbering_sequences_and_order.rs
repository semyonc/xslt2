//! `xsl:number` (XSLT 2.0 §12): the numbering sequences of
//! consecutive Unicode code points (§12.3, "Any other format token indicates a
//! numbering sequence in which that token represents the number 1 (one)"), and
//! the numbers of nodes evaluated in any order, over many names, and with
//! `count` / `from` patterns that read local variables (§12.2), which the
//! instruction counts with one walk per tree or parent, an index for nodes
//! asked for out of order, and a memo keyed by the local variables' values.
//!
//! Every test goes through the public API — compile, transform,
//! `serialize_primary` — and cites the rule it checks; the costs are guarded by
//! the crate's own `runtime::number::tests`.

mod common;

use xsd_schema::namespace::NameTable;
use xslt2::{Compiler, TransformOptions, XsltFunctions};

const NS: &str = "http://www.w3.org/1999/XSL/Transform";

fn sheet(body: &str) -> String {
    format!(
        "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"{NS}\" \
         xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:f=\"urn:f\" \
         exclude-result-prefixes=\"xs f\">{body}</xsl:stylesheet>"
    )
}

/// `content` inside `<out>` as the template matching `/`.
fn root(content: &str) -> String {
    sheet(&format!(
        r#"<xsl:template match="/"><out>{content}</out></xsl:template>"#
    ))
}

fn out_over(text: &str, source: &str) -> String {
    let names = NameTable::new();
    let functions = XsltFunctions::new();
    let compiled = Compiler::new(&names, &functions)
        .compile_str(text, "urn:numbering:sheet.xsl")
        .unwrap_or_else(|e| panic!("static error {}: {}", e.code, e.message));
    let arena = bumpalo::Bump::new();
    let run = compiled.transformation(&arena, TransformOptions::default());
    let outcome = run
        .transform_source("urn:numbering:in.xml", source)
        .unwrap_or_else(|e| panic!("dynamic error {}: {}", e.code, e.message));
    let text = compiled
        .serialize_primary(&outcome)
        .unwrap_or_else(|e| panic!("serialization error {}: {}", e.code, e.message));
    common::body(text)
}

/// The text of the one `<out>` element of `text` run over `<doc/>`.
fn inner(text: &str) -> String {
    let all = out_over(text, "<doc/>");
    all.strip_prefix("<out>")
        .and_then(|s| s.strip_suffix("</out>"))
        .map(str::to_string)
        .unwrap_or_else(|| if all == "<out/>" { String::new() } else { all })
}

/// A sequence as the Unicode character database names its characters: the
/// token (the character for one), the character for zero if the repertoire
/// has one, and the runs of consecutive code points `(first number, first code
/// point, last number)`.
type Sequence = (char, Option<char>, &'static [(u32, u32, u32)]);

/// The 17 sequences.
const SEQUENCES: [Sequence; 17] = [
    // CIRCLED DIGIT ONE … CIRCLED NUMBER TWENTY, CIRCLED NUMBER TWENTY ONE …
    // THIRTY FIVE, CIRCLED NUMBER THIRTY SIX … FIFTY; CIRCLED DIGIT ZERO
    (
        '\u{2460}',
        Some('\u{24EA}'),
        &[(1, 0x2460, 20), (21, 0x3251, 35), (36, 0x32B1, 50)],
    ),
    // DIGIT ONE FULL STOP … NUMBER TWENTY FULL STOP; DIGIT ZERO FULL STOP
    ('\u{2488}', Some('\u{1F100}'), &[(1, 0x2488, 20)]),
    // DINGBAT NEGATIVE CIRCLED DIGIT ONE … NUMBER TEN, NEGATIVE CIRCLED NUMBER
    // ELEVEN … TWENTY; NEGATIVE CIRCLED DIGIT ZERO
    (
        '\u{2776}',
        Some('\u{24FF}'),
        &[(1, 0x2776, 10), (11, 0x24EB, 20)],
    ),
    // DINGBAT CIRCLED SANS-SERIF DIGIT ONE … NUMBER TEN; … DIGIT ZERO
    ('\u{2780}', Some('\u{1F10B}'), &[(1, 0x2780, 10)]),
    // DINGBAT NEGATIVE CIRCLED SANS-SERIF DIGIT ONE … NUMBER TEN; … DIGIT ZERO
    ('\u{278A}', Some('\u{1F10C}'), &[(1, 0x278A, 10)]),
    // PARENTHESIZED DIGIT ONE … PARENTHESIZED NUMBER TWENTY
    ('\u{2474}', None, &[(1, 0x2474, 20)]),
    // DOUBLE CIRCLED DIGIT ONE … DOUBLE CIRCLED NUMBER TEN
    ('\u{24F5}', None, &[(1, 0x24F5, 10)]),
    // PARENTHESIZED IDEOGRAPH ONE … TEN
    ('\u{3220}', None, &[(1, 0x3220, 10)]),
    // CIRCLED IDEOGRAPH ONE … TEN
    ('\u{3280}', None, &[(1, 0x3280, 10)]),
    // AEGEAN NUMBER ONE … TEN
    ('\u{10107}', None, &[(1, 0x10107, 10)]),
    // COPTIC EPACT DIGIT ONE … COPTIC EPACT NUMBER TEN
    ('\u{102E1}', None, &[(1, 0x102E1, 10)]),
    // RUMI DIGIT ONE … RUMI NUMBER TEN
    ('\u{10E60}', None, &[(1, 0x10E60, 10)]),
    // BRAHMI NUMBER ONE … TEN
    ('\u{11052}', None, &[(1, 0x11052, 10)]),
    // SINHALA ARCHAIC DIGIT ONE … SINHALA ARCHAIC NUMBER TEN
    ('\u{111E1}', None, &[(1, 0x111E1, 10)]),
    // DIGIT ONE COMMA … DIGIT NINE COMMA; DIGIT ZERO COMMA
    ('\u{1F102}', Some('\u{1F101}'), &[(1, 0x1F102, 9)]),
    // COUNTING ROD UNIT DIGIT ONE … NINE
    ('\u{1D360}', None, &[(1, 0x1D360, 9)]),
    // MENDE KIKAKUI DIGIT ONE … NINE
    ('\u{1E8C7}', None, &[(1, 0x1E8C7, 9)]),
];

/// What `<xsl:number value="n" format="{token}"/>` should give.
fn expected(sequence: &Sequence, n: u32) -> String {
    let (_, zero, runs) = sequence;
    if n == 0 {
        return zero.map_or_else(|| "0".to_string(), String::from);
    }
    runs.iter()
        .find(|(first, _, last)| (*first..=*last).contains(&n))
        .map_or_else(
            || n.to_string(),
            |(first, code, _)| char::from_u32(code + n - first).unwrap().to_string(),
        )
}

/// §12.3: "Any other format token indicates a numbering sequence in which that
/// token represents the number 1 (one) … It is implementation-defined which
/// numbering sequences, additional to those listed above, are supported";
/// "the formatting token &#x2460; (circled digit one) has a range of 1 to 20
/// imposed by the Unicode character repertoire" (here 0 to 50: the repertoire
/// has ⓪ and ㉑–㊿ elsewhere); "Numbers that fall outside this range must be
/// formatted using the format token 1". The W3C catalog's `number-5000.xsl`
/// shape — `<xsl:number value="." format=" ①"/>` for `$from to $to` — for each
/// sequence from 0 to five past the end of its range.
#[test]
fn every_sequence_from_zero_to_past_its_end() {
    for sequence in &SEQUENCES {
        let (token, _, runs) = sequence;
        let last = runs[runs.len() - 1].2;
        let got = inner(&root(&format!(
            r#"<xsl:for-each select="0 to {}"><xsl:number value="." format=" &#x{:X};"/></xsl:for-each>"#,
            last + 5,
            *token as u32
        )));
        let want: String = (0..=last + 5)
            .map(|n| format!(" {}", expected(sequence, n)))
            .collect();
        assert_eq!(got, want, "U+{:04X}", *token as u32);
    }
}

/// The catalog's own expected strings for three of its cases, as
/// `assert-string-value` compares them (whitespace normalized):
/// `number-5013` (circled digits 0 to 55), `number-0821` (digit full stop 0 to
/// 25, the alternative with 🄀) and `number-5099` (digit comma 0 to 14).
#[test]
fn three_catalog_cases() {
    let run = |token: &str, to: u32| {
        inner(&root(&format!(
            r#"<xsl:for-each select="0 to {to}"><xsl:number value="." format=" {token}"/></xsl:for-each>"#
        )))
        .trim_start_matches(' ')
        .to_string()
    };
    assert_eq!(
        run("&#x2460;", 55),
        "⓪ ① ② ③ ④ ⑤ ⑥ ⑦ ⑧ ⑨ ⑩ ⑪ ⑫ ⑬ ⑭ ⑮ ⑯ ⑰ ⑱ ⑲ ⑳ ㉑ ㉒ ㉓ ㉔ ㉕ ㉖ ㉗ ㉘ ㉙ ㉚ ㉛ ㉜ ㉝ ㉞ ㉟ \
         ㊱ ㊲ ㊳ ㊴ ㊵ ㊶ ㊷ ㊸ ㊹ ㊺ ㊻ ㊼ ㊽ ㊾ ㊿ 51 52 53 54 55"
    );
    assert_eq!(
        run("&#x2488;", 25),
        "🄀 ⒈ ⒉ ⒊ ⒋ ⒌ ⒍ ⒎ ⒏ ⒐ ⒑ ⒒ ⒓ ⒔ ⒕ ⒖ ⒗ ⒘ ⒙ ⒚ ⒛ 21 22 23 24 25"
    );
    assert_eq!(run("🄂", 14), "🄁 🄂 🄃 🄄 🄅 🄆 🄇 🄈 🄉 🄊 10 11 12 13 14");
}

/// §12.3: outside its range a sequence is the token `1` — decimal, grouped,
/// with the English ordinal suffix; inside it, "If ordinal numbering is not
/// supported for the combination of the format token, the language, and the
/// actual value of the ordinal attribute, the request is ignored and cardinal
/// numbers are generated instead", and grouping applies to "decimal numbering
/// sequences" only.
#[test]
fn outside_its_range_a_sequence_is_the_token_1() {
    let number = |attributes: &str| inner(&root(&format!("<xsl:number {attributes}/>")));
    assert_eq!(
        number(r#"value="2, 51" format="&#x2460;" ordinal="yes""#),
        "②.51st"
    );
    assert_eq!(
        number(r#"value="0, 3" format="(&#x2474;)" ordinal="yes""#),
        "(0th.⑶)"
    );
    assert_eq!(
        number(r#"value="12, 12345" format="&#x2460;" grouping-separator="," grouping-size="3""#),
        "⑫.12,345"
    );
    // An integer beyond `u64` (2^70, exact as a double) is beyond every
    // sequence.
    assert_eq!(
        number(r#"value="1180591620717411303424" format="&#x2776;""#),
        "1180591620717411303424"
    );
}

/// §12.3: the sequences in every position of a `format` string, with
/// separators, and the format as an attribute value template; numbers of
/// nodes (§12.2) take them like any other token.
#[test]
fn sequences_with_separators_levels_and_templates() {
    let source = "<doc><s><p/><p/></s><s><p/><p/><p/></s></doc>";
    assert_eq!(
        out_over(
            &root(
                r#"<xsl:for-each select="//p"><n><xsl:number level="multiple" count="s|p" format="&#x2460;-&#x3220;"/></n></xsl:for-each>"#
            ),
            source
        ),
        "<out><n>①-㈠</n><n>①-㈡</n><n>②-㈠</n><n>②-㈡</n><n>②-㈢</n></out>"
    );
    assert_eq!(
        out_over(
            &root(
                r#"<xsl:variable name="f" select="'[&#x278A;]'"/><xsl:for-each select="//p"><xsl:number level="any" format="{$f}"/></xsl:for-each>"#
            ),
            source
        ),
        "<out>[➊][➋][➌][➍][➎]</out>"
    );
}

/// The two Greek cases of the catalog are not supported: `α`,
/// with either `letter-value`, is the token `1` — §12.3: "If an
/// implementation does not support a numbering sequence represented by the
/// given token, it must use a format token of 1."
#[test]
fn greek_alpha_is_the_token_1() {
    for letter_value in [
        "",
        r#"letter-value="alphabetic""#,
        r#"letter-value="traditional""#,
    ] {
        assert_eq!(
            inner(&root(&format!(
                r#"<xsl:number value="1, 2, 25" format="&#x3B1;" {letter_value}/>"#
            ))),
            "1.2.25",
            "{letter_value}"
        );
    }
}

/// Numbers every node of `source` selected by `select` (for each of the given
/// `xsl:number` attribute sets) in three orders — document order, reverse
/// document order, a pseudo-random `xsl:sort` order — and returns, per
/// order, the sorted `generate-id()=number` pairs: §12.2 gives a node the
/// same number whatever the order of evaluation.
fn numbers_in_three_orders(source: &str, select: &str, attributes: &str) -> Vec<Vec<String>> {
    let orders = [
        (select.to_string(), String::new()),
        (format!("reverse({select})"), String::new()),
        (
            select.to_string(),
            r#"<xsl:sort select="(position() * 7919) mod 10007" data-type="number"/>"#.to_string(),
        ),
    ];
    orders
        .iter()
        .map(|(select, sort)| {
            let text = root(&format!(
                r#"<xsl:for-each select="{select}">{sort}<n><xsl:value-of select="generate-id()"/>=<xsl:number {attributes}/></n></xsl:for-each>"#
            ));
            let result = out_over(&text, source);
            let mut rows: Vec<String> = result
                .split("<n>")
                .skip(1)
                .map(|r| r.split("</n>").next().unwrap_or("").to_string())
                .collect();
            rows.sort();
            rows
        })
        .collect()
}

/// §12.2, without `count`: "it defaults to the pattern that matches any node
/// with the same node kind as the selected node and, if the selected node has
/// an expanded-QName, with the same expanded-QName as the selected node" —
/// over 300 names in random order (one walk now counts them all), nested, in
/// every order of evaluation, at every level; spot-checked against the
/// spec's `$A` expressions.
#[test]
fn the_implied_pattern_over_many_names_in_any_order() {
    let mut seed: u64 = 7;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let mut source = String::from("<doc>");
    for _ in 0..400 {
        let (a, b) = (next() % 300, next() % 300);
        source.push_str(&format!("<n{a}><n{b}/>t<n{a}/></n{a}>"));
    }
    source.push_str("</doc>");
    for level in ["single", "multiple", "any"] {
        let orders = numbers_in_three_orders(&source, "//*", &format!(r#"level="{level}""#));
        assert_eq!(orders[0], orders[1], "level={level}: reverse");
        assert_eq!(orders[0], orders[2], "level={level}: sorted");
        assert_eq!(orders[0].len(), 1201);
    }
    let any = root(
        r#"<xsl:for-each select="//*"><xsl:variable name="S" select="."/><r><xsl:number level="any"/>|<xsl:value-of select="count((preceding::* | ancestor-or-self::*)[node-name(.) eq node-name($S)])"/></r></xsl:for-each>"#,
    );
    let result = out_over(&any, &source);
    for row in result.split("<r>").skip(1) {
        let row = row.split("</r>").next().unwrap();
        let (ours, spec) = row.split_once('|').unwrap();
        assert_eq!(ours, spec, "{row}");
    }
}

/// §12.2 with `count` and `from`, every level, in reverse and in a sorted
/// order: the numbers of document order (the walks look nodes they have
/// passed up in an index).
#[test]
fn count_and_from_in_any_order() {
    let source = r#"<doc><ch><s><p/><p><p/></p></s><s><p/></s></ch><ch><p/><s><p/><p/><!--c--><p/></s></ch><p/></doc>"#;
    for attributes in [
        r#"level="any" count="p" from="ch""#,
        r#"level="any" count="node()""#,
        r#"level="single" count="p|s""#,
        r#"level="single" count="p" from="s""#,
        r#"level="multiple" count="ch|s|p""#,
        r#"level="multiple" count="node()" from="ch""#,
    ] {
        let orders = numbers_in_three_orders(source, "//node() | //@*", attributes);
        assert_eq!(orders[0], orders[1], "{attributes}: reverse");
        assert_eq!(orders[0], orders[2], "{attributes}: sorted");
    }
}

/// §12.2: `matches-count` and `matches-from` are the patterns themselves, so
/// a pattern that reads a local variable matches according to the
/// variable's value in each evaluation (§9.7: a local variable is visible to
/// "the following siblings of the variable-binding element and their
/// descendants"). What one evaluation counted is reused only while the
/// variables keep their values: here they change, change back, and alternate
/// between `-0` and `0` (which `string()` tells apart), and a node-valued one
/// is the document element of whichever tree the node is in. Every number is
/// checked against a stylesheet function in the pattern, which no evaluation
/// shares.
#[test]
fn a_local_variable_in_a_pattern_is_read_in_each_evaluation() {
    let source =
        r#"<doc><e k="0"/><e k="1"/><g><e k="0"/><e k="2"/></g><e k="1"/><e k="0"/></doc>"#;
    let numbers = |counts: [&str; 3]| {
        let [any, single, multiple] = counts;
        out_over(
            &sheet(&format!(
                r#"<xsl:function name="f:same" as="xs:boolean"><xsl:param name="a"/><xsl:param name="b"/><xsl:sequence select="$a = $b"/></xsl:function>
                   <xsl:variable name="copy"><xsl:copy-of select="/"/></xsl:variable>
                   <xsl:template match="/"><out>
                     <xsl:for-each select="(//e, //e, reverse(//e), $copy//e, //e)">
                       <xsl:variable name="k" select="string((position() - 1) mod 3)"/>
                       <xsl:variable name="d" select="if (position() mod 2 = 0) then -0.0e0 else 0.0e0"/>
                       <xsl:variable name="top" select="/*"/>
                       <n><xsl:number level="any" count="{any}"/>,<xsl:number count="{single}"/>,<xsl:number level="multiple" count="{multiple}"/></n>
                     </xsl:for-each></out>
                   </xsl:template>"#
            )),
            source,
        )
    };
    for (local, function) in [
        (
            ["e[@k = $k]", "e[@k = $k]|g", "*[@k = $k]|g"],
            [
                "e[f:same(@k, $k)]",
                "e[f:same(@k, $k)]|g",
                "*[f:same(@k, $k)]|g",
            ],
        ),
        (
            [
                "e[string($d) = '0']",
                "e[string($d) = '-0']",
                "*[string($d) = '0']",
            ],
            [
                "e[f:same(string($d), '0')]",
                "e[f:same(string($d), '-0')]",
                "*[f:same(string($d), '0')]",
            ],
        ),
        (
            ["e[not(.. is $top)]", "*[. is $top]|e", "*[not(. is $top)]"],
            [
                "e[not(f:same(generate-id(..), generate-id($top)))]",
                "*[f:same(generate-id(.), generate-id($top))]|e",
                "*[not(f:same(generate-id(.), generate-id($top)))]",
            ],
        ),
    ] {
        let ours = numbers(local);
        assert_eq!(ours, numbers(function), "{local:?}");
        assert!(ours.contains(','), "{ours}");
    }
    // The values the run sees: `-0` is not `0` to `string()`.
    let negative = numbers(["e[string($d) = '-0']", "e", "e"]);
    assert!(negative.starts_with("<out><n>,"), "{negative}");
}

/// A local variable holding a sequence (twenty integers, then four) keys the
/// memo like a single value; one of a type the memo does not compare exactly
/// (`xs:decimal`, whose equality is lossy) makes every evaluation count
/// afresh. Either way the pattern sees the value of its own evaluation.
#[test]
fn a_long_or_decimal_local_variable_in_a_pattern() {
    let source = r#"<doc><e k="1"/><e k="5"/><e k="1"/><e k="20"/><e k="5"/></doc>"#;
    let text = root(
        r#"<xsl:for-each select="//e, //e">
             <xsl:variable name="ks" select="if (position() le 5) then (1 to 20) else (1 to 4)"/>
             <xsl:variable name="dk" select="if (position() le 5) then 5.0 else 1.0"/>
             <n><xsl:number level="any" count="e[@k = $ks]"/>,<xsl:number level="any" count="e[@k = $dk]"/></n>
           </xsl:for-each>"#,
    );
    assert_eq!(
        out_over(&text, source),
        "<out><n>1,</n><n>2,1</n><n>3,1</n><n>4,1</n><n>5,2</n>\
         <n>1,1</n><n>1,1</n><n>2,2</n><n>2,2</n><n>2,2</n></out>"
    );
}

/// Runs `numbers` — `xsl:number` instructions between `<n>` and `</n>`, in
/// the body of an `xsl:for-each` over `select` whose local variables are
/// `locals` — over `source`, with the patterns written twice: as given
/// (`{0}`, `{1}` … replaced from `patterns`) and as in `functions`, which
/// call the stylesheet function `f:same($a, $b)` (`$a = $b`) and so make
/// every evaluation count afresh (no memo). Returns both results.
fn with_and_without_memo(
    source: &str,
    globals: &str,
    select: &str,
    locals: &str,
    numbers: &str,
    patterns: &[&str],
    functions: &[&str],
) -> (String, String) {
    let run = |patterns: &[&str]| {
        let mut numbers = numbers.to_string();
        for (i, pattern) in patterns.iter().enumerate() {
            numbers = numbers.replace(&format!("{{{i}}}"), pattern);
        }
        out_over(
            &sheet(&format!(
                r#"<xsl:function name="f:same" as="xs:boolean"><xsl:param name="a"/><xsl:param name="b"/><xsl:sequence select="$a = $b"/></xsl:function>
                   {globals}
                   <xsl:template match="/"><out><xsl:variable name="orig" select="/*"/>
                     <xsl:for-each select="{select}">{locals}<n>{numbers}</n></xsl:for-each>
                   </out></xsl:template>"#
            )),
            source,
        )
    };
    (run(patterns), run(functions))
}

/// §12.2's `matches-from` is the `from` pattern itself, so a
/// local variable read by `from` is read in each evaluation, like one read by
/// `count`: the walks an evaluation made are kept for the next only while
/// the variables of **both** patterns keep their values. Here `$v` changes
/// every third evaluation, in document order, in reverse and in document
/// order again; `level="any"` restarts its count at each `from` match on the
/// walk, so a walk kept across a change of `$v` would restart at the wrong
/// sections.
#[test]
fn a_local_variable_in_a_from_pattern_is_read_in_each_evaluation() {
    let source = r#"<doc><sec k="1"><x/><x/><sec k="2"><x/></sec><x/></sec><sec k="2"><x/><x/><x/></sec><x/><sec k="1"><x/><sec k="2"><x/><x/></sec></sec><x/></doc>"#;
    let (memo, fresh) = with_and_without_memo(
        source,
        "",
        "(//x, reverse(//x), //x)",
        r#"<xsl:variable name="i" select="position()"/><xsl:variable name="v" select="('1', '2')[($i idiv 3) mod 2 + 1]"/>"#,
        r#"<xsl:number level="any" count="x" from="{0}"/>,<xsl:number level="any" from="{0}"/>,<xsl:number level="any" count="x | sec" from="{1}"/>"#,
        &["sec[@k = $v]", "*[@k = $v]"],
        &["sec[f:same(@k, $v)]", "*[f:same(@k, $v)]"],
    );
    assert_eq!(memo, fresh);
    // `$v` is 1, 1, 2, 2, 2, 1 …: the first `x` of the second `sec k="2"`
    // is 1 counted from that section, the next 6 counted from the first one.
    assert!(
        memo.starts_with(
            "<out><n>1,1,2</n><n>2,2,3</n><n>1,1,2</n><n>2,2,3</n><n>1,1,2</n><n>6,6,9</n>"
        ),
        "{memo}"
    );
}

/// The argument of a `key()` anchor (§5.5.2: `key(StringLiteral,
/// KeyValue)`, a literal or a variable reference) is read in each evaluation
/// too, so a local variable there keys the memo like one in a predicate. The
/// same patterns without the anchor, through a stylesheet function, are the
/// reference; `$v` changes every other evaluation.
#[test]
fn a_local_variable_in_a_key_anchor_is_read_in_each_evaluation() {
    let source = r#"<doc><x k="1"><y/></x><x k="2"><y/><y/><z/></x><x k="1"><y/><z/><y/></x><g><x k="2"><y/></x><x k="3"/><x k="1"><y/></x></g><x k="2"/></doc>"#;
    let (memo, fresh) = with_and_without_memo(
        source,
        r#"<xsl:key name="k" match="x" use="@k"/>"#,
        "(//x | //y | //z, reverse(//x | //y | //z))",
        r#"<xsl:variable name="i" select="position()"/><xsl:variable name="v" select="('1', '2', '3')[($i idiv 2) mod 3 + 1]"/>"#,
        r#"<xsl:number level="any" count="{0}"/>,<xsl:number count="{1}"/>,<xsl:number level="multiple" count="{0} | g"/>"#,
        &["key('k', $v)", "key('k', $v)//y | z"],
        &["x[f:same(@k, $v)]", "y[ancestor::x[f:same(@k, $v)]] | z"],
    );
    assert_eq!(memo, fresh);
    // `$v` is 1, 2, 2, 3, 3, 1 … over `x k="1"`, `y`, `x k="2"`, `y`, `y`,
    // `z`: an `x` counts only under its own key value.
    assert!(
        memo.starts_with("<out><n>1,,1</n><n>,,</n><n>1,,1</n><n>,,</n><n>,,</n><n>1,1,</n>"),
        "{memo}"
    );
}

/// A node-valued local variable keys the memo by the node's
/// identity, which includes its tree: here `$top` alternates between the
/// document element of a copy of the source and that of the source itself —
/// the same position in two documents — while only nodes of the copy are
/// numbered, so a memo that compared positions without the document would
/// keep counts made with the other value.
#[test]
fn a_node_valued_local_from_another_tree_is_told_apart() {
    let source = r#"<doc><e/><g><e/><e/></g><e/><g><e/></g><e/><e/></doc>"#;
    let (memo, fresh) = with_and_without_memo(
        source,
        r#"<xsl:variable name="copy"><xsl:copy-of select="/"/></xsl:variable>"#,
        "($copy//e, reverse($copy//e), $copy//e)",
        r#"<xsl:variable name="top" select="if ((position() idiv 2) mod 2 = 0) then $copy/* else $orig"/>"#,
        r#"<xsl:number level="any" count="{0}"/>,<xsl:number level="any" count="e" from="{1}"/>"#,
        &["e[not(.. is $top)]", "g[not(.. is $top)]"],
        &[
            "e[not(f:same(generate-id(..), generate-id($top)))]",
            "g[not(f:same(generate-id(..), generate-id($top)))]",
        ],
    );
    assert_eq!(memo, fresh);
    // `$top` is the copy's document element, the source's twice, the copy's
    // again: the children of the copy's document element are counted only
    // while `$top` is the source's.
    assert!(
        memo.starts_with("<out><n>,1</n><n>2,1</n><n>3,2</n><n>2,4</n>"),
        "{memo}"
    );
}
