//! Exact rewriting of the two XPath 3.0 constructs that the assertions of XSLT 2.0-applicable
//! cases use, so that the harness's XPath **2.0** engine can evaluate them.
//!
//! The catalog writes its `<assert>` expressions in XPath 3.0. The harness evaluates them with
//! the `xsd-schema` XPath 2.0 engine, which correctly rejects 3.0 syntax, so a handful of cases
//! could not be judged at all (`harnessError`). Two constructs occur, and each is rewritten only
//! in a form whose XPath 2.0 equivalent is exact:
//!
//! * **`Q{uri}local`** (XPath 3.0 §2.? `URIQualifiedName ::= BracedURILiteral NCName`,
//!   `BracedURILiteral ::= "Q" "{" [^{}]* "}"`) becomes `p:local` with a fresh prefix `p` bound to
//!   `uri` in the static context: both denote the expanded QName `{uri}local`. `Q{}local` becomes
//!   `local`, which in the harness's static context (no default element/type namespace, see
//!   `compare::eval_xpath`) is the same no-namespace name — except as a function name, where an
//!   unprefixed name means the default function namespace, so `Q{}name(` is refused. A URI with
//!   whitespace (3.0 whitespace-normalizes it) or the XMLNS namespace (3.0 XQST0070) is refused.
//! * **The simple map operator** `E1 ! E2` (XPath 3.0 §3.14: E2 is evaluated once per item of E1
//!   with that item as the context item, and the results are concatenated) is rewritten only for
//!   `F(…) ! string()` where `F(…)` is a whole function call standing as an argument (directly
//!   after `(` or `,`, or at the start) and `string()` is followed by `)`, `,` or the end. It
//!   becomes `(for $v in F(…) return string($v))`: F&O 1.0 `fn:string`: "The behavior of the
//!   function if the argument is omitted is exactly the same as if the context item had been
//!   passed as the argument", and XPath 2.0 §3.7 evaluates the `return` clause once per item, in
//!   order, concatenating the results.
//!
//! Anything else — any other `!`, any other `Q{` — makes [`translate`] return `None`, and the
//! caller reports the original compile error, so an assertion is either evaluated exactly or
//! not at all. String literals and comments are never touched. In XPath 2.0 a `Q{` or a `!` not
//! followed by `=` outside literals and comments is always a syntax error, so a rewrite can only
//! ever apply to an expression the 2.0 engine could not compile in the first place.

use crate::catalog::NsBindings;

/// An assertion rewritten into XPath 2.0, and the prefixes it binds on top of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    pub xpath: String,
    pub extra_ns: Vec<(String, String)>,
}

const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

/// XPath 2.0 reserved function names (XPath 2.0 A.3): an unprefixed call with one of these
/// names is a kind test or a keyword, never a function call.
const RESERVED_FUNCTION_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "document-node",
    "element",
    "empty-sequence",
    "if",
    "item",
    "node",
    "processing-instruction",
    "schema-attribute",
    "schema-element",
    "text",
    "typeswitch",
];

/// Rewrite the XPath 3.0 constructs of `xpath` into XPath 2.0, or `None` when there is nothing
/// to rewrite or something cannot be rewritten exactly.
pub fn translate(xpath: &str, ns: &NsBindings) -> Option<Translated> {
    let (after_names, extra_ns) = rewrite_eqnames(xpath, ns)?;
    let after_maps = rewrite_simple_maps(&after_names, xpath)?;
    if after_maps == xpath {
        return None;
    }
    Some(Translated {
        xpath: after_maps,
        extra_ns,
    })
}

// ───────────────────────── lexical helpers ─────────────────────────

/// For every character of `s`, whether it is *code* — outside string literals and comments.
///
/// A `BracedURILiteral` (XPath 3.0 `[100] BracedURILiteral ::= "Q" "{" [^{}]* "}"`) is code,
/// but opaque: its URI may contain quotes or `(:`, which must not start a literal or a comment
/// (`/Q{urn:it's}x = 'Q{urn:y}z'` would otherwise put the real literal out of step and get its
/// content rewritten). It runs to the first `}`, or to the end when there is none.
fn code_mask(s: &[char]) -> Vec<bool> {
    let mut mask = vec![true; s.len()];
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == 'Q' && i + 1 < s.len() && s[i + 1] == '{' {
            i = (i + 2..s.len())
                .find(|&j| s[j] == '}')
                .map_or(s.len(), |close| close + 1);
        } else if c == '"' || c == '\'' {
            // A literal runs to the next unpaired quote; a doubled quote is an escape.
            let start = i;
            i += 1;
            loop {
                if i >= s.len() {
                    break;
                }
                if s[i] == c {
                    if i + 1 < s.len() && s[i + 1] == c {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            let end = (i + 1).min(s.len());
            for m in &mut mask[start..end] {
                *m = false;
            }
            i = end;
        } else if c == '(' && i + 1 < s.len() && s[i + 1] == ':' {
            // Comments nest (XPath 2.0 §2.6).
            let start = i;
            let mut depth = 0usize;
            while i < s.len() {
                if s[i] == '(' && i + 1 < s.len() && s[i + 1] == ':' {
                    depth += 1;
                    i += 2;
                } else if s[i] == ':' && i + 1 < s.len() && s[i + 1] == ')' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            for m in &mut mask[start..i.min(s.len())] {
                *m = false;
            }
        } else {
            i += 1;
        }
    }
    mask
}

fn is_ncname_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ncname_char(c: char) -> bool {
    is_ncname_start(c)
        || c.is_ascii_digit()
        || c == '-'
        || c == '.'
        || c == '\u{B7}'
        || ('\u{300}'..='\u{36F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
        || (!c.is_ascii() && c.is_alphanumeric())
}

/// Every keyword of the XPath 2.0 grammar (A.1 EBNF): operators, the words of `for`, `some`,
/// `every`, `if`, `instance of`, `treat as`, `castable as`, `cast as`, and the axis names. An
/// EQName is never one of them in 3.0, but its local part spelled bare can be one in 2.0
/// (`/a Q{}div 2` is a 3.0 syntax error, `/a div 2` a valid 2.0 division), so `Q{}keyword` is
/// refused. Refusing a keyword that would have been a name test (`/Q{}text`) only costs a
/// `harnessError`.
const XPATH20_KEYWORDS: &[&str] = &[
    "ancestor",
    "ancestor-or-self",
    "and",
    "as",
    "attribute",
    "cast",
    "castable",
    "child",
    "descendant",
    "descendant-or-self",
    "div",
    "else",
    "eq",
    "every",
    "except",
    "following",
    "following-sibling",
    "for",
    "ge",
    "gt",
    "idiv",
    "if",
    "in",
    "instance",
    "intersect",
    "is",
    "le",
    "lt",
    "mod",
    "namespace",
    "ne",
    "of",
    "or",
    "parent",
    "preceding",
    "preceding-sibling",
    "return",
    "satisfies",
    "self",
    "some",
    "then",
    "to",
    "treat",
    "union",
];

/// XML whitespace — the four characters XPath and XML call whitespace.
fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

// ───────────────────────── Q{uri}local ─────────────────────────

/// Replace every `Q{uri}` before an NCName by a prefix bound to `uri`. `None` if a `Q{` cannot
/// be rewritten exactly.
fn rewrite_eqnames(xpath: &str, ns: &NsBindings) -> Option<(String, Vec<(String, String)>)> {
    let s: Vec<char> = xpath.chars().collect();
    let mask = code_mask(&s);
    let mut out = String::with_capacity(xpath.len());
    let mut extra: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let is_eqname = mask[i] && s[i] == 'Q' && i + 1 < s.len() && s[i + 1] == '{';
        if !is_eqname {
            out.push(s[i]);
            i += 1;
            continue;
        }
        // `abcQ{…}` is no EQName in 3.0 either.
        if i > 0 && mask[i - 1] && is_ncname_char(s[i - 1]) {
            return None;
        }
        // `p:Q{}x` and `*:Q{}x` are no 3.0 names, but `p:x` and `*:x` are 2.0 ones; only the
        // axis separator `::` may precede an EQName directly.
        if i > 0 && s[i - 1] == ':' && !(i > 1 && s[i - 2] == ':') {
            return None;
        }
        let close = (i + 2..s.len()).find(|&j| s[j] == '}')?;
        let uri: String = s[i + 2..close].iter().collect();
        if uri.contains('{') || uri.chars().any(is_xml_space) || uri == XMLNS_NS {
            return None;
        }
        let name_start = close + 1;
        if name_start >= s.len() || !is_ncname_start(s[name_start]) {
            // `Q{uri}*` and anything else that is not a URIQualifiedName.
            return None;
        }
        let mut name_end = name_start;
        while name_end < s.len() && is_ncname_char(s[name_end]) {
            name_end += 1;
        }
        let local: String = s[name_start..name_end].iter().collect();
        let mut next = name_end;
        while next < s.len() && is_xml_space(s[next]) {
            next += 1;
        }
        // Only where an EQName is a *name*: never an axis (`Q{}child::a` is a 3.0 syntax error,
        // `child::a` a 2.0 step), never the first part of a QName (`Q{}p:x` → `p:x`).
        if s.get(name_end) == Some(&':') || s.get(next) == Some(&':') {
            return None;
        }
        if uri.is_empty() {
            // `Q{}local` → `local`, except as a function name (followed by `(`), where a bare
            // name means the default function namespace, and except where the bare name is an
            // XPath 2.0 keyword.
            if s.get(next) == Some(&'(') || XPATH20_KEYWORDS.contains(&local.as_str()) {
                return None;
            }
        } else {
            let prefix = match extra.iter().find(|(_, u)| *u == uri) {
                Some((p, _)) => p.clone(),
                None => {
                    let p = fresh_prefix(xpath, ns, &extra);
                    extra.push((p.clone(), uri));
                    p
                }
            };
            out.push_str(&prefix);
            out.push(':');
        }
        i = name_start;
    }
    Some((out, extra))
}

/// A prefix bound nowhere in the assertion's scope and absent from its text.
fn fresh_prefix(xpath: &str, ns: &NsBindings, taken: &[(String, String)]) -> String {
    (1..)
        .map(|n| format!("eqname{n}"))
        .find(|p| {
            !xpath.contains(p.as_str())
                && !ns.iter().any(|(q, _)| q == p)
                && !taken.iter().any(|(q, _)| q == p)
                && !["xs", "fn", "xsl"].contains(&p.as_str())
        })
        .expect("an unbounded range always yields a fresh prefix")
}

// ───────────────────────── F(…) ! string() ─────────────────────────

/// Rewrite every simple map; `None` if one of them is not of the supported form.
fn rewrite_simple_maps(xpath: &str, original: &str) -> Option<String> {
    // The variable names introduced below must not occur in the assertion.
    if original.contains("xslts-map") {
        return None;
    }
    let mut current = xpath.to_string();
    for n in 1.. {
        let s: Vec<char> = current.chars().collect();
        let mask = code_mask(&s);
        let bang =
            (0..s.len()).find(|&i| mask[i] && s[i] == '!' && !(i + 1 < s.len() && s[i + 1] == '='));
        match bang {
            None => return Some(current),
            Some(bang) => current = rewrite_one_map(&s, &mask, bang, &format!("xslts-map-{n}"))?,
        }
    }
    unreachable!("the loop returns once no simple map is left")
}

/// Rewrite the simple map whose `!` is at `bang`, or `None` if it is not `F(…) ! string()` in an
/// argument position.
fn rewrite_one_map(s: &[char], mask: &[bool], bang: usize, var: &str) -> Option<String> {
    let skip_ws = |mut i: usize| {
        while i < s.len() && is_xml_space(s[i]) {
            i += 1;
        }
        i
    };
    // Right operand: exactly `string()`, then `)`, `,` or the end.
    let j = skip_ws(bang + 1);
    let word: String = s.get(j..j + 6)?.iter().collect();
    if word != "string" || !mask[j..j + 6].iter().all(|&m| m) {
        return None;
    }
    let k = skip_ws(j + 6);
    if s.get(k) != Some(&'(') || !mask[k] {
        return None;
    }
    let k = skip_ws(k + 1);
    if s.get(k) != Some(&')') || !mask[k] {
        return None;
    }
    let end_right = k + 1;
    let after = skip_ws(end_right);
    if after < s.len() && !(mask[after] && matches!(s[after], ')' | ',')) {
        return None;
    }

    // Left operand: a whole function call `QName(…)`.
    let mut close = bang;
    while close > 0 && is_xml_space(s[close - 1]) {
        close -= 1;
    }
    let close = close.checked_sub(1)?;
    if s[close] != ')' || !mask[close] {
        return None;
    }
    let mut depth = 0usize;
    let mut open = None;
    for q in (0..=close).rev() {
        if !mask[q] {
            continue;
        }
        match s[q] {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(q);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open?;
    let mut name_end = open;
    while name_end > 0 && is_xml_space(s[name_end - 1]) {
        name_end -= 1;
    }
    let mut name_start = name_end;
    while name_start > 0
        && mask[name_start - 1]
        && (is_ncname_char(s[name_start - 1]) || s[name_start - 1] == ':')
    {
        name_start -= 1;
    }
    let name: String = s[name_start..name_end].iter().collect();
    if !is_function_name(&name) {
        return None;
    }
    // The call must be the whole left operand: it starts the expression or an argument.
    let mut before = name_start;
    while before > 0 && is_xml_space(s[before - 1]) {
        before -= 1;
    }
    if before > 0 && !(mask[before - 1] && matches!(s[before - 1], '(' | ',')) {
        return None;
    }

    let head: String = s[..name_start].iter().collect();
    let call: String = s[name_start..=close].iter().collect();
    let tail: String = s[end_right..].iter().collect();
    Some(format!(
        "{head}(for ${var} in {call} return string(${var})){tail}"
    ))
}

/// A lexical QName that names a function: not a reserved name, which would be a kind test or a
/// keyword (XPath 2.0 A.3).
fn is_function_name(name: &str) -> bool {
    let ncname = |part: &str| {
        let mut chars = part.chars();
        matches!(chars.next(), Some(c) if is_ncname_start(c)) && chars.all(is_ncname_char)
    };
    match name.split_once(':') {
        Some((prefix, local)) => ncname(prefix) && ncname(local),
        None => ncname(name) && !RESERVED_FUNCTION_NAMES.contains(&name),
    }
}

/// Checks of the rewriting rules, run by `--self-test`. Returns the number of checks.
pub fn self_test() -> usize {
    let none: NsBindings = Vec::new();
    let t = |x: &str| translate(x, &none);
    let mut checks = 0;
    let mut check = |ok: bool, what: &str| {
        assert!(ok, "xpath30 self-test failed: {what}");
        checks += 1;
    };

    // Q{uri}local → p:local, one prefix per URI, nothing inside literals or comments.
    let r = t(r#"/Q{http://example.org/hal}out = ",,atr1""#).expect("EQName");
    check(
        r.xpath == r#"/eqname1:out = ",,atr1""#
            && r.extra_ns == vec![("eqname1".to_string(), "http://example.org/hal".to_string())],
        "element EQName",
    );
    let r = t("/Q{urn:a}x/@Q{urn:a}y = 'Q{urn:b}z' (: Q{urn:c}w :)").expect("shared URI");
    check(
        r.xpath == "/eqname1:x/@eqname1:y = 'Q{urn:b}z' (: Q{urn:c}w :)" && r.extra_ns.len() == 1,
        "one prefix per URI; literals and comments untouched",
    );
    check(
        t(r#"/root/Out/@Q{}Attr1 = "Whatsup""#).map(|r| r.xpath)
            == Some(r#"/root/Out/@Attr1 = "Whatsup""#.to_string()),
        "Q{}local becomes an unprefixed name",
    );
    // A fresh prefix avoids the assertion's own bindings and text.
    let bound = vec![("eqname1".to_string(), "urn:other".to_string())];
    check(
        translate("/Q{urn:a}x", &bound).map(|r| r.xpath) == Some("/eqname2:x".to_string()),
        "fresh prefix skips a bound one",
    );
    check(
        t("/Q{urn:a}x[@eqname1]").map(|r| r.xpath) == Some("/eqname2:x[@eqname1]".to_string()),
        "fresh prefix skips one that occurs in the text",
    );
    // Refusals: anything that is not exactly a URIQualifiedName we can express.
    for x in [
        "Q{}f(1)",                            // no-namespace function name
        "Q{} f (1)",                          // not even a URIQualifiedName (space)
        "/Q{urn:a}*",                         // wildcard form
        "/Q{ urn:a }x",                       // whitespace normalization
        "/Q{http://www.w3.org/2000/xmlns/}x", // XQST0070
        "/abcQ{urn:a}x",                      // not an EQName in 3.0 either
        "/Q{urn:a",                           // unterminated
    ] {
        check(t(x).is_none(), x);
    }
    // P2-2: a BracedURILiteral is opaque — a quote or `(:` inside it starts nothing, so a real
    // literal after it is left alone.
    let r = t("/Q{urn:it's}x = 'Q{urn:y}z'").expect("quote inside the braces");
    check(
        r.xpath == "/eqname1:x = 'Q{urn:y}z'"
            && r.extra_ns == vec![("eqname1".to_string(), "urn:it's".to_string())],
        "a quote inside Q{…} does not start a literal",
    );
    check(
        t(r#"/Q{urn:a"b}x = "Q{urn:y}z""#).map(|r| r.xpath)
            == Some(r#"/eqname1:x = "Q{urn:y}z""#.to_string()),
        "a double quote inside Q{…} does not start a literal",
    );
    check(
        t("/Q{urn:(:}x = 'Q{urn:y}z'").map(|r| r.xpath)
            == Some("/eqname1:x = 'Q{urn:y}z'".to_string()),
        "(: inside Q{…} does not start a comment",
    );
    // P2-2: never where the rewrite would turn invalid XPath 3.0 into valid XPath 2.0.
    for x in [
        "/a Q{}div 2", // operator: 3.0 error, 2.0 division
        "/a Q{}and true()",
        "1 Q{}to 3",
        "/Q{}child::a", // axis: 3.0 error, 2.0 step
        "/Q{urn:a}child::a",
        "/Q{}a :: b",
        "/Q{}p:x", // QName prefix: 3.0 error, 2.0 name
        "/p:Q{}x", // prefixed EQName: 3.0 error, 2.0 name
        "/*:Q{}x",
    ] {
        check(t(x).is_none(), x);
    }
    // … while an axis before an EQName is fine.
    check(
        t("/child::Q{}x").map(|r| r.xpath) == Some("/child::x".to_string()),
        "axis :: before an EQName",
    );
    // Nothing to do: plain XPath 2.0, `!=`, and 3.0 syntax only inside literals.
    for x in ["1 != 2", "'a!b' = \"Q{u}v\"", "(: ! :) true()"] {
        check(t(x).is_none(), x);
    }

    // F(…) ! string() in an argument position.
    check(
        t(r#"string-join(string-to-codepoints(/out/b[1])!string(), " ") = "10 32""#)
            .map(|r| r.xpath)
            == Some(
                r#"string-join((for $xslts-map-1 in string-to-codepoints(/out/b[1]) return string($xslts-map-1)), " ") = "10 32""#
                    .to_string(),
            ),
        "simple map over a function call",
    );
    check(
        t("count(f(g(x) ! string()) ! string())").map(|r| r.xpath)
            == Some(
                "count((for $xslts-map-2 in f((for $xslts-map-1 in g(x) return string($xslts-map-1))) return string($xslts-map-2)))"
                    .to_string(),
            ),
        "nested simple maps",
    );
    for x in [
        "count(a/f(x)!string())",  // left operand is a path, not the call alone
        "count(f(x)!string()/y)",  // right operand continues
        "count(f(x)!string()[1])", // right operand has a predicate
        "count(f(x)!name())",      // only string() is supported
        "count($x!string())",      // left operand is not a call
        "count(node()!string())",  // kind test, not a call
        "count(f(x)!string()!string())", // chained
        "count(-f(x)!string())",   // unary minus binds looser
        "count(f(x)!fn:string())", // only the unprefixed spelling
        "count(xslts-map-1(x)!string())", // reserved variable stem in the text
    ] {
        check(t(x).is_none(), x);
    }
    checks
}
