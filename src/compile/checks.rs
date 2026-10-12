//! One table-driven static check over the whole stylesheet tree (§3.4).
//!
//! > \[ERR XTSE0010\] It is a static error if an XSLT-defined element is used
//! > in a context where it is not permitted, if a required attribute is
//! > omitted, or if the content of the element does not correspond to the
//! > content that is allowed for the element.
//!
//! Every XSLT 2.0 element is described once, in [`TABLE`], by exactly what the
//! spec's syntax summaries say about it (each element's own section, collected
//! in Appendix D "Element Syntax Summary"): which attributes it allows, which
//! it requires, which of them are attribute value templates, what its content
//! model is, and where it may appear. [`check`] walks every module of the
//! compilation once and validates the tree against that table **before** any
//! instruction is compiled, so a misplaced element is rejected even when it sits
//! in a subtree the compiler would never reach.
//!
//! # Adding a row
//!
//! One entry in [`TABLE`], transcribed from the element's syntax box:
//!
//! ```text
//! spec("for-each-group")                       // the local name
//!     .category(Category::Instruction)         // "Permitted parent elements"
//!     .required(&["select"])                   // attributes without a `?`
//!     .optional(&["group-by", "collation"])    // attributes with a `?`
//!     .avt(&["collation"])                     // attributes written `{ … }`
//!     .content(Content::GroupsThenSequence(&[SORT_ANY])),  // "Content:"
//! ```
//!
//! Nothing else has to change: the walk, the §3.9 forwards-compatible
//! relaxations, the §4.2 whitespace rules and the XTSE0090 / XTSE0010 /
//! XTSE0260 reporting are all driven from the row. An element this processor
//! does not implement yet still gets a row — the row is what makes it *legal*
//! where the spec allows it, so the compiler can go on reporting
//! `XSLT2-NOTIMPL` for it instead of a bogus static error. Later
//! sub-milestones therefore only ever *add* rows and never touch the walk.
//!
//! # What is deliberately not here
//!
//! Checks that need a name resolved against an import-precedence-wide table, or
//! a value compiled as an expression, stay where that information already is:
//! duplicate global variables (XTSE0630), `use-attribute-sets` resolution
//! (XTSE0710) and the `xsl:output` conflict (XTSE1560) are done once the
//! declarations exist, and `select`-versus-content exclusivity for the
//! node-construction instructions stays in their compilers. The rule of thumb
//! is that this pass answers "is this element allowed to be here, spelled like
//! this", and nothing that needs to look at another declaration.

use std::collections::HashSet;

use xsd_schema::navigator::{DomNavigator, DomNodeType};

use super::attributes::STANDARD_ATTRS;
use super::Builder;
use crate::collation::CollationRegistry;
use crate::error::{codes, XsltError};
use crate::model::{Body, Instruction, InstructionKind, SortKey, ValueSource, VarValue};
use crate::names::{resolve_qname, QName, XML_NS, XSLT_NS};
use crate::receiver::Nav;
use crate::stylesheet::{
    attr_local, attributes, in_scope_namespaces, is_stripped_stylesheet_text,
    move_to_document_element, static_context_of, text_run_value, Module, ModuleSet, StaticContext,
};
use crate::whitespace;

// ── The table ────────────────────────────────────────────────────────────

/// Where an XSLT element may appear ("Permitted parent elements").
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Category {
    /// `xsl:stylesheet` / `xsl:transform`: the module element, nowhere else.
    Root,
    /// A top-level declaration: a child of `xsl:stylesheet`/`xsl:transform`.
    Declaration,
    /// An instruction: allowed wherever a sequence constructor is (§2.6).
    Instruction,
    /// Both a declaration and an instruction (`xsl:variable`).
    Both,
    /// Only as a child of the specific elements whose content model names it
    /// (`xsl:when`, `xsl:sort`, `xsl:with-param`, …).
    Subordinate,
}

impl Category {
    /// Whether an element of this category may appear in a sequence
    /// constructor.
    fn in_sequence(self) -> bool {
        matches!(self, Category::Instruction | Category::Both)
    }

    /// Whether an element of this category may be a top-level declaration.
    fn at_top_level(self) -> bool {
        matches!(self, Category::Declaration | Category::Both)
    }
}

/// How many times one group of the content model may occur.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Occurs {
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
}

/// One group of an ordered content model: the XSLT local names it accepts and
/// how often it may occur. `(xsl:sort | xsl:with-param)*` is one group with two
/// names; `(xsl:when+, xsl:otherwise?)` is two groups.
pub(super) struct Group {
    names: &'static [&'static str],
    occurs: Occurs,
}

const PARAM_STAR: Group = Group {
    names: &["param"],
    occurs: Occurs::ZeroOrMore,
};
const SORT_STAR: Group = Group {
    names: &["sort"],
    occurs: Occurs::ZeroOrMore,
};
const SORT_PLUS: Group = Group {
    names: &["sort"],
    occurs: Occurs::OneOrMore,
};
const WITH_PARAM_STAR: Group = Group {
    names: &["with-param"],
    occurs: Occurs::ZeroOrMore,
};

/// What an element may contain ("Content:").
pub(super) enum Content {
    /// Nothing but comments and processing instructions (XTSE0260).
    Empty,
    /// `#PCDATA`: text, comments and processing instructions, no elements.
    Text,
    /// A sequence constructor (§5.7).
    Sequence,
    /// An ordered list of element groups and nothing else.
    Groups(&'static [Group]),
    /// An ordered list of element groups followed by a sequence constructor.
    GroupsThenSequence(&'static [Group]),
    /// `(xsl:import*, other-declarations)`: the top level of a module (§3.6).
    TopLevel,
    /// Content this processor does not model (`xsl:import-schema`'s inline
    /// `xs:schema`).
    Opaque,
}

/// Everything the spec's syntax summary says about one XSLT element.
pub(super) struct ElementSpec {
    /// The local name, in the XSLT namespace.
    pub local: &'static str,
    pub category: Category,
    /// Attributes in no namespace the element **must** have; a missing one is
    /// XTSE0010 (§3.4).
    pub required: &'static [&'static str],
    /// Attributes in no namespace the element may have, beyond the standard
    /// attributes of §3.5.
    pub optional: &'static [&'static str],
    /// Which of those attributes are attribute value templates (§5.6), written
    /// `{ … }` in the syntax summary. Recorded so that the table is the single
    /// description of an element: a fixed-value attribute written as an AVT is
    /// XTSE0020, and only an AVT may contain curly brackets.
    pub avt: &'static [&'static str],
    pub content: Content,
    /// The code for this element appearing where it may not, when the spec
    /// assigns one of its own rather than XTSE0010.
    pub misplaced: Option<&'static str>,
    /// §11.4.3's "mutually exclusive, and one of them must be present" rule
    /// over `select` and the sequence constructor, with its code.
    pub select_xor_content: Option<&'static str>,
}

/// A row with everything optional left empty; the table is written by
/// overriding the fields a given element actually has.
const fn row(local: &'static str, category: Category, content: Content) -> ElementSpec {
    ElementSpec {
        local,
        category,
        required: &[],
        optional: &[],
        avt: &[],
        content,
        misplaced: None,
        select_xor_content: None,
    }
}

/// Every XSLT 2.0 element, transcribed from the element syntax summaries
/// (Appendix D). The order is alphabetical, as the spec prints them.
///
/// The `version`, `exclude-result-prefixes`, `extension-element-prefixes`,
/// `xpath-default-namespace`, `default-collation` and `use-when` attributes are
/// not repeated in any row: §3.5 allows them on **every** XSLT element and
/// [`STANDARD_ATTRS`] holds them once.
static TABLE: &[ElementSpec] = &[
    ElementSpec {
        required: &["select", "regex"],
        optional: &["flags"],
        avt: &["regex", "flags"],
        ..row(
            "analyze-string",
            Category::Instruction,
            Content::Groups(&[
                Group {
                    names: &["matching-substring"],
                    occurs: Occurs::ZeroOrOne,
                },
                Group {
                    names: &["non-matching-substring"],
                    occurs: Occurs::ZeroOrOne,
                },
                Group {
                    names: &["fallback"],
                    occurs: Occurs::ZeroOrMore,
                },
            ]),
        )
    },
    row(
        "apply-imports",
        Category::Instruction,
        Content::Groups(&[WITH_PARAM_STAR]),
    ),
    ElementSpec {
        optional: &["select", "mode"],
        ..row(
            "apply-templates",
            Category::Instruction,
            Content::Groups(&[Group {
                names: &["sort", "with-param"],
                occurs: Occurs::ZeroOrMore,
            }]),
        )
    },
    ElementSpec {
        required: &["name"],
        optional: &["namespace", "select", "separator", "type", "validation"],
        avt: &["name", "namespace", "separator"],
        ..row("attribute", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["name"],
        optional: &["use-attribute-sets"],
        ..row(
            "attribute-set",
            Category::Declaration,
            Content::Groups(&[Group {
                names: &["attribute"],
                occurs: Occurs::ZeroOrMore,
            }]),
        )
    },
    ElementSpec {
        required: &["name"],
        ..row(
            "call-template",
            Category::Instruction,
            Content::Groups(&[WITH_PARAM_STAR]),
        )
    },
    ElementSpec {
        required: &["name"],
        optional: &["use-character-maps"],
        ..row(
            "character-map",
            Category::Declaration,
            Content::Groups(&[Group {
                names: &["output-character"],
                occurs: Occurs::ZeroOrMore,
            }]),
        )
    },
    row(
        "choose",
        Category::Instruction,
        Content::Groups(&[
            Group {
                names: &["when"],
                occurs: Occurs::OneOrMore,
            },
            Group {
                names: &["otherwise"],
                occurs: Occurs::ZeroOrOne,
            },
        ]),
    ),
    ElementSpec {
        optional: &["select"],
        ..row("comment", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        optional: &[
            "copy-namespaces",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        ..row("copy", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["select"],
        optional: &["copy-namespaces", "type", "validation"],
        ..row("copy-of", Category::Instruction, Content::Empty)
    },
    ElementSpec {
        optional: &[
            "name",
            "decimal-separator",
            "grouping-separator",
            "infinity",
            "minus-sign",
            "NaN",
            "percent",
            "per-mille",
            "zero-digit",
            "digit",
            "pattern-separator",
        ],
        ..row("decimal-format", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        optional: &["validation", "type"],
        ..row("document", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["name"],
        optional: &[
            "namespace",
            "inherit-namespaces",
            "use-attribute-sets",
            "type",
            "validation",
        ],
        avt: &["name", "namespace"],
        ..row("element", Category::Instruction, Content::Sequence)
    },
    row("fallback", Category::Instruction, Content::Sequence),
    ElementSpec {
        required: &["select"],
        ..row(
            "for-each",
            Category::Instruction,
            Content::GroupsThenSequence(&[SORT_STAR]),
        )
    },
    ElementSpec {
        required: &["select"],
        optional: &[
            "group-by",
            "group-adjacent",
            "group-starting-with",
            "group-ending-with",
            "collation",
        ],
        avt: &["collation"],
        ..row(
            "for-each-group",
            Category::Instruction,
            Content::GroupsThenSequence(&[SORT_STAR]),
        )
    },
    ElementSpec {
        required: &["name"],
        optional: &["as", "override"],
        ..row(
            "function",
            Category::Declaration,
            Content::GroupsThenSequence(&[PARAM_STAR]),
        )
    },
    ElementSpec {
        required: &["test"],
        ..row("if", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["href"],
        // §3.10.3: "It is a static error if the xsl:import element is not a
        // top-level element."
        misplaced: Some(codes::XTSE0190),
        ..row("import", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        optional: &["namespace", "schema-location"],
        ..row("import-schema", Category::Declaration, Content::Opaque)
    },
    ElementSpec {
        required: &["href"],
        // §3.10.2: "It is a static error if the xsl:include element is not a
        // top-level element."
        misplaced: Some(codes::XTSE0170),
        ..row("include", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        required: &["name", "match"],
        optional: &["use", "collation"],
        ..row("key", Category::Declaration, Content::Sequence)
    },
    row(
        "matching-substring",
        Category::Subordinate,
        Content::Sequence,
    ),
    ElementSpec {
        optional: &["select", "terminate"],
        avt: &["terminate"],
        ..row("message", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["name"],
        optional: &["select"],
        avt: &["name"],
        ..row("namespace", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["stylesheet-prefix", "result-prefix"],
        ..row("namespace-alias", Category::Declaration, Content::Empty)
    },
    row(
        "next-match",
        Category::Instruction,
        Content::Groups(&[Group {
            names: &["with-param", "fallback"],
            occurs: Occurs::ZeroOrMore,
        }]),
    ),
    row(
        "non-matching-substring",
        Category::Subordinate,
        Content::Sequence,
    ),
    ElementSpec {
        optional: &[
            "value",
            "select",
            "level",
            "count",
            "from",
            "format",
            "lang",
            "letter-value",
            "ordinal",
            "grouping-separator",
            "grouping-size",
        ],
        avt: &[
            "format",
            "lang",
            "letter-value",
            "ordinal",
            "grouping-separator",
            "grouping-size",
        ],
        ..row("number", Category::Instruction, Content::Empty)
    },
    row("otherwise", Category::Subordinate, Content::Sequence),
    ElementSpec {
        optional: &[
            "name",
            "method",
            "byte-order-mark",
            "cdata-section-elements",
            "doctype-public",
            "doctype-system",
            "encoding",
            "escape-uri-attributes",
            "include-content-type",
            "indent",
            "media-type",
            "normalization-form",
            "omit-xml-declaration",
            "standalone",
            "undeclare-prefixes",
            "use-character-maps",
            // §20 gives `xsl:output` its own `version`, which §3.9 explicitly
            // excludes from the standard-attribute rules; it is listed here so
            // that the row is complete even though STANDARD_ATTRS also has it.
            "version",
        ],
        ..row("output", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        required: &["character", "string"],
        ..row("output-character", Category::Subordinate, Content::Empty)
    },
    ElementSpec {
        required: &["name"],
        optional: &["select", "as", "required", "tunnel"],
        ..row("param", Category::Declaration, Content::Sequence)
    },
    ElementSpec {
        optional: &["select"],
        ..row(
            "perform-sort",
            Category::Instruction,
            Content::GroupsThenSequence(&[SORT_PLUS]),
        )
    },
    ElementSpec {
        required: &["elements"],
        ..row("preserve-space", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        required: &["name"],
        optional: &["select"],
        avt: &["name"],
        ..row(
            "processing-instruction",
            Category::Instruction,
            Content::Sequence,
        )
    },
    ElementSpec {
        optional: &[
            "format",
            "href",
            "validation",
            "type",
            "method",
            "byte-order-mark",
            "cdata-section-elements",
            "doctype-public",
            "doctype-system",
            "encoding",
            "escape-uri-attributes",
            "include-content-type",
            "indent",
            "media-type",
            "normalization-form",
            "omit-xml-declaration",
            "standalone",
            "undeclare-prefixes",
            "use-character-maps",
            "output-version",
        ],
        avt: &[
            "format",
            "href",
            "method",
            "byte-order-mark",
            "cdata-section-elements",
            "doctype-public",
            "doctype-system",
            "encoding",
            "escape-uri-attributes",
            "include-content-type",
            "indent",
            "media-type",
            "normalization-form",
            "omit-xml-declaration",
            "standalone",
            "undeclare-prefixes",
            "output-version",
        ],
        ..row("result-document", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        // §11.10 (XSLT 2.0): `select` is required and the content may be
        // nothing but `xsl:fallback`. XSLT 3.0 relaxed both.
        required: &["select"],
        ..row(
            "sequence",
            Category::Instruction,
            Content::Groups(&[Group {
                names: &["fallback"],
                occurs: Occurs::ZeroOrMore,
            }]),
        )
    },
    ElementSpec {
        optional: &[
            "select",
            "lang",
            "order",
            "collation",
            "stable",
            "case-order",
            "data-type",
        ],
        avt: &[
            "lang",
            "order",
            "collation",
            "stable",
            "case-order",
            "data-type",
        ],
        ..row("sort", Category::Subordinate, Content::Sequence)
    },
    ElementSpec {
        required: &["elements"],
        ..row("strip-space", Category::Declaration, Content::Empty)
    },
    ElementSpec {
        required: &["version"],
        optional: &["id", "default-validation", "input-type-annotations"],
        ..row("stylesheet", Category::Root, Content::TopLevel)
    },
    ElementSpec {
        optional: &["match", "name", "priority", "mode", "as"],
        ..row(
            "template",
            Category::Declaration,
            Content::GroupsThenSequence(&[PARAM_STAR]),
        )
    },
    ElementSpec {
        optional: &["disable-output-escaping"],
        ..row("text", Category::Instruction, Content::Text)
    },
    ElementSpec {
        required: &["version"],
        optional: &["id", "default-validation", "input-type-annotations"],
        ..row("transform", Category::Root, Content::TopLevel)
    },
    ElementSpec {
        optional: &["select", "separator", "disable-output-escaping"],
        avt: &["separator"],
        // §11.4.3: "These are mutually exclusive, and one of them must be
        // present. [ERR XTSE0870]"
        select_xor_content: Some(codes::XTSE0870),
        ..row("value-of", Category::Instruction, Content::Sequence)
    },
    ElementSpec {
        required: &["name"],
        optional: &["select", "as"],
        ..row("variable", Category::Both, Content::Sequence)
    },
    ElementSpec {
        required: &["test"],
        ..row("when", Category::Subordinate, Content::Sequence)
    },
    ElementSpec {
        required: &["name"],
        optional: &["select", "as", "tunnel"],
        ..row("with-param", Category::Subordinate, Content::Sequence)
    },
];

/// The row for an XSLT local name, or `None` when XSLT 2.0 has no such element.
pub(super) fn spec_of(local: &str) -> Option<&'static ElementSpec> {
    TABLE.iter().find(|spec| spec.local == local)
}

/// Whether `xsl:<local>` is an XSLT 2.0 instruction: an element whose syntax
/// summary says "Category: instruction" (`xsl:variable`'s says both
/// declaration and instruction). The basis of `element-available()`
/// (`functions::element_available`).
pub(crate) fn is_instruction(local: &str) -> bool {
    spec_of(local).is_some_and(|spec| spec.category.in_sequence())
}

/// The code for an attribute of `xsl:<element>` whose value is not one of the
/// permitted values for it.
///
/// > \[ERR XTSE0020\] It is a static error if an attribute (**other than an
/// > attribute written using curly brackets in a position where an attribute
/// > value template is permitted**) contains a value that is not one of the
/// > permitted values for that attribute.
///
/// The parenthesis is what the table's `avt` column is for: when the attribute
/// is an attribute value template only its *effective* value can be wrong, and
/// that is the dynamic XTDE0030 instead.
pub(super) fn bad_value_code(element: &str, attr: &str) -> &'static str {
    match spec_of(element) {
        Some(spec) if spec.avt.contains(&attr) => codes::XTDE0030,
        _ => codes::XTSE0020,
    }
}

/// The elements §4.2 strips whitespace text children of "regardless of any
/// `xml:space` attributes".
const ALWAYS_STRIP_WHITESPACE: &[&str] = &[
    "analyze-string",
    "apply-imports",
    "apply-templates",
    "attribute-set",
    "call-template",
    "character-map",
    "choose",
    "next-match",
    "stylesheet",
    "transform",
];

// ── xs:decimal lexical form (§3.6, §6.4) ─────────────────────────────────

/// Whether `text` is in the lexical space of `xs:decimal`.
///
/// > \[ERR XTSE0110\] The value of the `version` attribute must be a number:
/// > specifically, it must be a valid instance of the type `xs:decimal`.
///
/// `f64::from_str` is not that predicate: it also accepts `2.0e3`, `INF` and
/// `NaN`. XSD Part 2 §3.2.3: `decimal ::= (('+' | '-')? ([0-9]+ ('.' [0-9]*)? |
/// '.' [0-9]+))` — sign, digits, at most one period, no exponent.
pub(crate) fn is_xs_decimal(text: &str) -> bool {
    let text = whitespace::trim(text);
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (before, after) = match digits.split_once('.') {
        Some((b, a)) => (b, a),
        None => (digits, ""),
    };
    if after.contains('.') {
        return false;
    }
    if before.is_empty() && after.is_empty() {
        return false;
    }
    before.bytes().all(|b| b.is_ascii_digit()) && after.bytes().all(|b| b.is_ascii_digit())
}

// ── The pass ─────────────────────────────────────────────────────────────

/// Validates every module of the compilation against [`TABLE`].
///
/// The walk goes through [`Module::children`], so §3.12 `use-when` exclusion is
/// respected by construction: an excluded element and its whole subtree are
/// simply never seen (§3.12: "no static or dynamic errors will be reported in
/// respect of the element and its contents").
pub(super) fn check(
    modules: &ModuleSet<'_>,
    collations: &CollationRegistry,
) -> Result<(), XsltError> {
    for module in &modules.modules {
        let root = match module.root {
            Some(node) => module.doc.create_navigator_at(node),
            None => {
                let mut nav = module.doc.create_navigator();
                if !move_to_document_element(&mut nav) {
                    continue;
                }
                nav
            }
        };
        if module.is_excluded(&root) {
            continue;
        }
        Checker { module, collations }.element(&root)?;
    }
    Ok(())
}

struct Checker<'m, 't> {
    module: &'m Module<'t>,
    /// What `[xsl:]default-collation` may name (§3.6.1's XTSE0125).
    collations: &'m CollationRegistry,
}

impl<'t> Checker<'_, 't> {
    fn error(&self, code: &'static str, nav: &Nav<'t>, message: String) -> XsltError {
        XsltError::new(code, message).with_location(self.module.location(nav))
    }

    /// Checks one element that is already known to be allowed where it stands:
    /// its attributes, then its content.
    fn element(&self, nav: &Nav<'t>) -> Result<(), XsltError> {
        let sc = static_context_of(nav, self.module, self.collations)?;
        if nav.namespace_uri() != XSLT_NS {
            // A literal result element or an extension instruction. Its own
            // attributes belong to §11.1, which the literal-result-element
            // compiler owns; the one rule that is this pass's is §21.1's, which
            // applies wherever the attribute is written.
            self.basic_processor_attrs_of_lre(nav)?;
            if sc.is_extension_namespace(nav.namespace_uri()) {
                // §18.2.2: the instruction's own content is defined by the
                // extension, not by XSLT; only its xsl:fallback children are
                // ours, and their siblings are ignored.
                return self.fallback_children(nav);
            }
            return self.sequence(nav);
        }
        let Some(spec) = spec_of(nav.local_name()) else {
            // Reached only for a module root that is in the XSLT namespace but
            // is not xsl:stylesheet/xsl:transform; the loader rejects that with
            // XTSE0150 before this pass runs.
            return Ok(());
        };
        self.attributes(nav, spec, &sc)?;
        self.content(nav, spec, &sc)
    }

    // ── Attributes (§3.5, §3.9, §21.1) ───────────────────────────────────

    fn attributes(
        &self,
        nav: &Nav<'t>,
        spec: &'static ElementSpec,
        sc: &StaticContext,
    ) -> Result<(), XsltError> {
        let allows = |name: &str| {
            spec.required.contains(&name)
                || spec.optional.contains(&name)
                || STANDARD_ATTRS.contains(&name)
        };
        for (uri, local, _, value) in attributes(nav) {
            if uri == XSLT_NS {
                // §3.5: "The attribute must be in the XSLT namespace only if
                // its parent element is not in the XSLT namespace."
                return Err(self.error(
                    codes::XTSE0090,
                    nav,
                    format!(
                        "xsl:{} has an attribute xsl:{local} in the XSLT namespace",
                        spec.local
                    ),
                ));
            }
            if !uri.is_empty() {
                // §3.4: an attribute in a non-XSLT namespace is allowed on any
                // XSLT element and is ignored (xml:space, xml:base, extension
                // attributes).
                continue;
            }
            if let Some(error) = self.basic_processor_attr(nav, spec, &local, &value) {
                return Err(error);
            }
            if allows(&local) {
                continue;
            }
            // §3.9: "if an element has an attribute that XSLT 2.0 does not
            // allow the element to have, then the attribute must be ignored."
            if sc.compat.is_forwards() {
                continue;
            }
            return Err(self.error(
                codes::XTSE0090,
                nav,
                format!("xsl:{} has no attribute named {local:?}", spec.local),
            ));
        }
        for required in spec.required {
            if attr_local(nav, required).is_none() {
                return Err(self.error(
                    codes::XTSE0010,
                    nav,
                    format!(
                        "xsl:{} has no {required} attribute, which is required",
                        spec.local
                    ),
                ));
            }
        }
        // §3.6: "The value of the version attribute must be a number:
        // specifically, it must be a valid instance of the type xs:decimal."
        // `xsl:output/@version` is a serialization parameter, not this one
        // (§3.9).
        if spec.local != "output" {
            if let Some(version) = attr_local(nav, "version") {
                if !is_xs_decimal(&version) {
                    return Err(self.error(
                        codes::XTSE0110,
                        nav,
                        format!("the version attribute is {version:?}, which is not an xs:decimal"),
                    ));
                }
            }
        }
        // §6.4: "The value of the priority attribute must conform to the rules
        // for the xs:decimal type."
        if spec.local == "template" {
            if let Some(priority) = attr_local(nav, "priority") {
                if !is_xs_decimal(&priority) {
                    return Err(self.error(
                        codes::XTSE0530,
                        nav,
                        format!("the priority {priority:?} is not an xs:decimal"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// §21.1's two restrictions on a basic processor, on an XSLT element.
    ///
    /// > \[ERR XTSE1660\] A basic XSLT processor must signal a static error if
    /// > the stylesheet includes an `[xsl:]type` attribute, or an
    /// > `[xsl:]validation` or `default-validation` attribute with a value
    /// > other than `strip`.
    ///
    /// Only on an element whose own syntax admits the attribute: anywhere else
    /// the attribute is simply not one the element defines, which is XTSE0090.
    fn basic_processor_attr(
        &self,
        nav: &Nav<'t>,
        spec: &'static ElementSpec,
        local: &str,
        value: &str,
    ) -> Option<XsltError> {
        let admits = |name: &str| spec.required.contains(&name) || spec.optional.contains(&name);
        match local {
            "type" if admits("type") => Some(self.error(
                codes::XTSE1660,
                nav,
                format!(
                    "a basic XSLT processor cannot honour the type attribute of xsl:{}",
                    spec.local
                ),
            )),
            "validation" | "default-validation"
                if admits(local) && whitespace::trim(value) != "strip" =>
            {
                Some(self.error(
                    codes::XTSE1660,
                    nav,
                    format!(
                        "a basic XSLT processor supports only {local}=\"strip\", not {:?}",
                        whitespace::trim(value)
                    ),
                ))
            }
            _ => None,
        }
    }

    /// The same §21.1 rule for the `xsl:type` / `xsl:validation` spelling a
    /// literal result element uses (§11.1).
    fn basic_processor_attrs_of_lre(&self, nav: &Nav<'t>) -> Result<(), XsltError> {
        for (uri, local, _, value) in attributes(nav) {
            if uri != XSLT_NS {
                continue;
            }
            let offending = match local.as_str() {
                "type" => true,
                "validation" => whitespace::trim(&value) != "strip",
                _ => false,
            };
            if offending {
                return Err(self.error(
                    codes::XTSE1660,
                    nav,
                    format!(
                        "a basic XSLT processor cannot honour xsl:{local} on a literal result \
                         element"
                    ),
                ));
            }
        }
        Ok(())
    }

    // ── Content models ───────────────────────────────────────────────────

    fn content(
        &self,
        nav: &Nav<'t>,
        spec: &'static ElementSpec,
        sc: &StaticContext,
    ) -> Result<(), XsltError> {
        if let Some(code) = spec.select_xor_content {
            let has_select = attr_local(nav, "select").is_some();
            if has_select == self.has_content(nav) {
                return Err(self.error(
                    code,
                    nav,
                    format!(
                        "xsl:{} must have either a select attribute or content, and not both",
                        spec.local
                    ),
                ));
            }
        }
        match &spec.content {
            Content::Empty => self.empty(nav, spec),
            Content::Text => self.text_only(nav, spec),
            Content::Sequence => self.sequence(nav),
            Content::Groups(groups) => self.groups(nav, spec, groups, false),
            Content::GroupsThenSequence(groups) => self.groups(nav, spec, groups, true),
            Content::TopLevel => self.top_level(nav, sc),
            Content::Opaque => Ok(()),
        }
    }

    /// Whether the element has content that is neither stripped whitespace
    /// (§4.2) nor a comment or processing instruction.
    fn has_content(&self, nav: &Nav<'t>) -> bool {
        self.module.children(nav).any(|child| {
            child.node_type() == DomNodeType::Element || self.is_significant_text(nav, &child)
        })
    }

    fn is_significant_text(&self, parent: &Nav<'t>, child: &Nav<'t>) -> bool {
        is_significant_text(parent, child)
    }

    /// > \[ERR XTSE0260\] Within an XSLT element that is required to be empty,
    /// > any content other than comments or processing instructions, including
    /// > any whitespace text node preserved using the `xml:space="preserve"`
    /// > attribute, is a static error.
    fn empty(&self, nav: &Nav<'t>, spec: &'static ElementSpec) -> Result<(), XsltError> {
        for child in self.module.children(nav) {
            let offending = child.node_type() == DomNodeType::Element
                || (child.node_type().is_text_like() && !is_stripped_stylesheet_text(&child));
            if offending {
                return Err(self.error(
                    codes::XTSE0260,
                    &child,
                    format!("xsl:{} must be empty", spec.local),
                ));
            }
        }
        Ok(())
    }

    /// `#PCDATA`: `xsl:text` takes text, comments and processing instructions,
    /// and no elements at all — XSLT 2.0 dropped the nested instructions XSLT
    /// 1.0 tolerated.
    fn text_only(&self, nav: &Nav<'t>, spec: &'static ElementSpec) -> Result<(), XsltError> {
        for child in self.module.children(nav) {
            if child.node_type() == DomNodeType::Element {
                return Err(self.error(
                    codes::XTSE0010,
                    &child,
                    format!(
                        "xsl:{} may contain text only, not the element {}",
                        spec.local,
                        child.name()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// A sequence constructor (§5.7): instructions, literal result elements,
    /// extension instructions and text, in any order.
    fn sequence(&self, nav: &Nav<'t>) -> Result<(), XsltError> {
        for child in self.module.children(nav) {
            if child.node_type() != DomNodeType::Element {
                continue;
            }
            self.sequence_item(&child)?;
        }
        Ok(())
    }

    /// One element in a sequence-constructor position.
    fn sequence_item(&self, child: &Nav<'t>) -> Result<(), XsltError> {
        if child.namespace_uri() != XSLT_NS {
            return self.element(child);
        }
        match spec_of(child.local_name()) {
            Some(spec) if spec.category.in_sequence() => self.element(child),
            other => {
                // §3.9: an element in the XSLT namespace that XSLT 2.0 does not
                // allow in a sequence constructor falls back to its
                // `xsl:fallback` children, if it has any; "if the element has no
                // xsl:fallback children, then a static error is reported in the
                // same way as if forwards-compatible behavior were not enabled."
                let sc = static_context_of(child, self.module, self.collations)?;
                if sc.compat.is_forwards() && self.has_fallback(child) {
                    return self.fallback_children(child);
                }
                let code = other
                    .and_then(|spec| spec.misplaced)
                    .unwrap_or(codes::XTSE0010);
                let what = match other {
                    Some(_) => "is not allowed in a sequence constructor",
                    None => "is not an XSLT instruction",
                };
                Err(self.error(code, child, format!("xsl:{} {what}", child.local_name())))
            }
        }
    }

    fn has_fallback(&self, nav: &Nav<'t>) -> bool {
        self.module.children(nav).any(|child| {
            child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "fallback"
        })
    }

    /// Checks only the `xsl:fallback` children of an element whose own content
    /// this processor does not interpret (§3.9, §18.2.2: "Siblings of the
    /// `xsl:fallback` elements are ignored, even if they are valid XSLT 2.0
    /// instructions").
    fn fallback_children(&self, nav: &Nav<'t>) -> Result<(), XsltError> {
        for child in self.module.children(nav) {
            if child.node_type() == DomNodeType::Element
                && child.namespace_uri() == XSLT_NS
                && child.local_name() == "fallback"
            {
                self.element(&child)?;
            }
        }
        Ok(())
    }

    /// An ordered content model: the groups in order, then — when the model ends
    /// in `sequence-constructor` — everything else.
    fn groups(
        &self,
        nav: &Nav<'t>,
        spec: &'static ElementSpec,
        groups: &'static [Group],
        then_sequence: bool,
    ) -> Result<(), XsltError> {
        let mut current = 0usize;
        let mut counts = vec![0usize; groups.len()];
        let mut in_sequence = false;
        // The expanded names of the `xsl:param` / `xsl:with-param` children
        // placed in a group, for XTSE0580 / XTSE0670. Both rules compare
        // **expanded** QNames, so two different prefixes bound to the same
        // namespace collide and the same prefix rebound does not.
        let mut param_names: Vec<QName> = Vec::new();
        let mut with_param_names: Vec<QName> = Vec::new();
        for child in self.module.children(nav) {
            match child.node_type() {
                DomNodeType::Element => {
                    if in_sequence {
                        self.sequence_item(&child)?;
                        continue;
                    }
                    let placed = (child.namespace_uri() == XSLT_NS)
                        .then(|| {
                            groups[current..]
                                .iter()
                                .position(|group| group.names.contains(&child.local_name()))
                        })
                        .flatten();
                    match placed {
                        Some(offset) => {
                            current += offset;
                            counts[current] += 1;
                            match child.local_name() {
                                "param" => param_names.push(self.param_name(&child)?),
                                "with-param" => with_param_names.push(self.param_name(&child)?),
                                _ => {}
                            }
                            self.element(&child)?;
                        }
                        None if then_sequence => {
                            in_sequence = true;
                            self.sequence_item(&child)?;
                        }
                        None => {
                            return Err(self.error(
                                codes::XTSE0010,
                                &child,
                                format!(
                                    "{} is not allowed here; the content of xsl:{} is {}",
                                    child.name(),
                                    spec.local,
                                    describe(groups)
                                ),
                            ))
                        }
                    }
                }
                _ if self.is_significant_text(nav, &child) => {
                    if then_sequence {
                        in_sequence = true;
                    } else {
                        return Err(self.error(
                            codes::XTSE0010,
                            &child,
                            format!(
                                "xsl:{} must not have text content; its content is {}",
                                spec.local,
                                describe(groups)
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
        for (group, count) in groups.iter().zip(&counts) {
            let wrong = match group.occurs {
                Occurs::ZeroOrOne => *count > 1,
                Occurs::ZeroOrMore => false,
                Occurs::OneOrMore => *count == 0,
            };
            if wrong {
                return Err(self.error(
                    codes::XTSE0010,
                    nav,
                    format!(
                        "the content of xsl:{} is {}, but it has {count} xsl:{}",
                        spec.local,
                        describe(groups),
                        group.names.join(" / xsl:")
                    ),
                ));
            }
        }
        duplicate(&param_names)
            .map(|name| {
                Err(self.error(
                    codes::XTSE0580,
                    nav,
                    format!("two parameters of xsl:{} are named {name}", spec.local),
                ))
            })
            .unwrap_or(Ok(()))?;
        duplicate(&with_param_names)
            .map(|name| {
                Err(self.error(
                    codes::XTSE0670,
                    nav,
                    format!(
                        "xsl:{} has two xsl:with-param children named {name}",
                        spec.local
                    ),
                ))
            })
            .unwrap_or(Ok(()))
    }

    /// The expanded name of an `xsl:param` / `xsl:with-param`, resolved in its
    /// own in-scope namespaces (§5.1).
    fn param_name(&self, nav: &Nav<'t>) -> Result<QName, XsltError> {
        let Some(text) = attr_local(nav, "name") else {
            // A missing `name` is XTSE0010, raised by `attributes`; an
            // unnameable parameter cannot collide with anything.
            return Ok(QName::default());
        };
        let in_scope = in_scope_namespaces(nav);
        resolve_qname(
            &text,
            |prefix| {
                if prefix == crate::names::XML_PREFIX {
                    return Some(XML_NS.to_string());
                }
                in_scope
                    .iter()
                    .find(|(p, _)| p == prefix)
                    .map(|(_, u)| u.clone())
            },
            codes::XTSE0020,
        )
        .map_err(|e| e.or_location(&self.module.location(nav)))
    }

    /// `(xsl:import*, other-declarations)` — the children of the module element
    /// (§3.6).
    fn top_level(&self, nav: &Nav<'t>, sc: &StaticContext) -> Result<(), XsltError> {
        for child in self.module.children(nav) {
            match child.node_type() {
                DomNodeType::Element => {
                    if child.namespace_uri() != XSLT_NS {
                        // §3.6.2: a top-level element in a namespace other than
                        // the XSLT namespace is user data, and its content is
                        // not a sequence constructor — nothing in it is checked.
                        // An element in *no* namespace is XTSE0130, which the
                        // declaration walk reports.
                        continue;
                    }
                    match spec_of(child.local_name()) {
                        Some(spec) if spec.category.at_top_level() => self.element(&child)?,
                        Some(_) => {
                            return Err(self.error(
                                codes::XTSE0010,
                                &child,
                                format!(
                                    "xsl:{} is not a stylesheet declaration",
                                    child.local_name()
                                ),
                            ))
                        }
                        // §3.9: "if an element in the XSLT namespace appears as
                        // a child of the xsl:stylesheet element, and XSLT 2.0
                        // does not allow such elements to occur as children of
                        // the xsl:stylesheet element, then the element and its
                        // content must be ignored."
                        None if sc.compat.is_forwards() => {}
                        None => {
                            return Err(self.error(
                                codes::XTSE0010,
                                &child,
                                format!(
                                    "xsl:{} is not a stylesheet declaration",
                                    child.local_name()
                                ),
                            ))
                        }
                    }
                }
                // > [ERR XTSE0120] An xsl:stylesheet element must not have any
                // > text node children. (This rule applies after stripping of
                // > whitespace text nodes as described in §4.2.)
                _ if self.is_significant_text(nav, &child) => {
                    return Err(self.error(
                        codes::XTSE0120,
                        &child,
                        format!("xsl:{} must not have text node children", nav.local_name()),
                    ))
                }
                _ => {}
            }
        }
        Ok(())
    }
}

// ── Checks over the compiled tree ────────────────────────────────────────

/// The static half of XTSE0680 (§10.1.1), over the whole compiled stylesheet.
///
/// > \[ERR XTSE0680\] In the case of `xsl:call-template`, it is a static error
/// > to pass a non-tunnel parameter named `x` to a template that does not have
/// > a template parameter named `x`, unless backwards compatible behavior is
/// > enabled for the `xsl:call-template` instruction.
///
/// Two things make this different from the same test at call time: a tunnel
/// `xsl:param` of the callee does **not** satisfy a non-tunnel `xsl:with-param`
/// (§10.1.2 keeps the two sets of parameters apart), and a *static* error has to
/// be reported whether or not the call is ever reached.
pub(super) fn check_call_template_params(builder: &Builder<'_, '_, '_>) -> Result<(), XsltError> {
    let mut failure = None;
    for body in builder.every_body() {
        visit(body, &mut |instruction| {
            if failure.is_some() {
                return;
            }
            let InstructionKind::CallTemplate { name, params } = &instruction.kind else {
                return;
            };
            // §9.10 row 9: backwards-compatible behaviour drops the rule.
            if builder
                .static_contexts
                .get(instruction.ctx)
                .compat
                .is_backwards()
            {
                return;
            }
            let Some((id, _)) = builder.named.get(name) else {
                // XTSE0650 — "no template named x" — is the runtime's to report,
                // because §10.1 lets a stylesheet contain a call that is never
                // made to a template it does not declare.
                return;
            };
            let declared = &builder.templates[id.0 as usize].params;
            for param in params {
                if param.tunnel {
                    continue;
                }
                if !declared.iter().any(|p| !p.tunnel && p.name == param.name) {
                    failure = Some(
                        XsltError::new(
                            codes::XTSE0680,
                            format!(
                                "the template {name} has no non-tunnel parameter named {}",
                                param.name
                            ),
                        )
                        .with_location(param.loc.clone()),
                    );
                    return;
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

impl<'a, 'm, 't> Builder<'a, 'm, 't> {
    /// Every sequence constructor of the compiled stylesheet: template bodies,
    /// the `xsl:attribute` instructions of every attribute set, the content of
    /// every global variable or parameter, the body of every stylesheet
    /// function — the winners and, since §2.9 says "A static error must be
    /// signaled even if it occurs in a part of the stylesheet that is never
    /// evaluated", the declarations that lost to a higher import precedence —
    /// and the content of every `xsl:key` declaration, for the same reason.
    fn every_body(&self) -> Vec<&Body> {
        let mut out: Vec<&Body> = Vec::new();
        for rule in &self.templates {
            out.push(&rule.body);
            out.extend(rule.params.iter().filter_map(|p| body_of(&p.value)));
        }
        for decl in self
            .attribute_sets
            .values()
            .flat_map(|set| &set.declarations)
        {
            out.extend(decl.attributes.iter().filter_map(|i| match &i.kind {
                InstructionKind::Attribute {
                    value: ValueSource::Content(body),
                    ..
                } => Some(body),
                _ => None,
            }));
        }
        out.extend(self.globals.iter().filter_map(|g| body_of(&g.value)));
        // §10.3: a stylesheet function's body may call a template too.
        out.extend(self.functions_table.iter().flatten().map(|f| &f.body));
        out.extend(self.losing_function_bodies.iter());
        // §16.3.1: the sequence constructor of every `xsl:key` declaration,
        // of every module — whether or not `key()` ever asks for that key, and
        // whether or not any node ever matches that declaration.
        out.extend(
            self.keys
                .iter()
                .flat_map(|key| &key.definitions)
                .filter_map(|definition| match &definition.specifier {
                    ValueSource::Content(body) => Some(body),
                    ValueSource::Select(_) => None,
                }),
        );
        out
    }
}

/// The sequence constructor a variable-binding element's value is built from.
fn body_of(value: &VarValue) -> Option<&Body> {
    match value {
        VarValue::Tree { body, .. } | VarValue::Sequence(body) => Some(body),
        VarValue::Select(_) | VarValue::Empty => None,
    }
}

/// Calls `f` on every instruction of `body`, depth first.
pub(super) fn visit(body: &Body, f: &mut impl FnMut(&Instruction)) {
    for instruction in &body.0 {
        f(instruction);
        match &instruction.kind {
            InstructionKind::ForEach { body, sort, .. }
            | InstructionKind::ForEachGroup { body, sort, .. }
            | InstructionKind::PerformSort { body, sort, .. } => {
                visit_sort_keys(sort, f);
                visit(body, f);
            }
            InstructionKind::If { then: body, .. }
            | InstructionKind::LiteralElement { body, .. }
            | InstructionKind::Element { body, .. }
            | InstructionKind::Document { body }
            | InstructionKind::Copy { body, .. }
            | InstructionKind::Message { body, .. } => visit(body, f),
            InstructionKind::ResultDocument(rd) => visit(&rd.body, f),
            InstructionKind::Choose { whens, otherwise } => {
                for (_, body) in whens {
                    visit(body, f);
                }
                if let Some(body) = otherwise {
                    visit(body, f);
                }
            }
            InstructionKind::AnalyzeString {
                matching,
                non_matching,
                ..
            } => {
                for body in [matching, non_matching].into_iter().flatten() {
                    visit(body, f);
                }
            }
            InstructionKind::ValueOf { value, .. }
            | InstructionKind::Attribute { value, .. }
            | InstructionKind::Namespace { value, .. }
            | InstructionKind::Comment { value }
            | InstructionKind::ProcessingInstruction { value, .. } => {
                if let ValueSource::Content(body) = value {
                    visit(body, f);
                }
            }
            InstructionKind::Variable { value, .. } => {
                if let Some(body) = body_of(value) {
                    visit(body, f);
                }
            }
            InstructionKind::ApplyTemplates { params, sort, .. } => {
                visit_sort_keys(sort, f);
                for param in params {
                    if let Some(body) = body_of(&param.value) {
                        visit(body, f);
                    }
                }
            }
            InstructionKind::ApplyImports { params }
            | InstructionKind::NextMatch { params }
            | InstructionKind::CallTemplate { params, .. } => {
                for param in params {
                    if let Some(body) = body_of(&param.value) {
                        visit(body, f);
                    }
                }
            }
            InstructionKind::Text { .. }
            | InstructionKind::Sequence { .. }
            | InstructionKind::CopyOf { .. }
            | InstructionKind::Fallback
            | InstructionKind::NotImplemented { .. } => {}
        }
    }
}

/// Calls `f` on every instruction of the sequence constructors of a sort key
/// specification (§13.1).
fn visit_sort_keys(sort: &[SortKey], f: &mut impl FnMut(&Instruction)) {
    for key in sort {
        if let ValueSource::Content(body) = &key.value {
            visit(body, f);
        }
    }
}

/// Whether a child node is a text node §4.2 keeps — the whole of §4.2's
/// whitespace stripping, not only its `xml:space` bullet.
///
/// > Any whitespace text node that satisfies both the following conditions is
/// > removed from the tree: the parent of the text node is not an `xsl:text`
/// > element; the text node does not have an ancestor element that has an
/// > `xml:space` attribute with a value of `preserve` …
/// >
/// > Any whitespace text node whose parent is one of the following elements is
/// > removed from the tree, **regardless of any `xml:space` attributes**:
/// > `xsl:analyze-string`, `xsl:apply-imports`, `xsl:apply-templates`,
/// > `xsl:attribute-set`, `xsl:call-template`, `xsl:character-map`,
/// > `xsl:choose`, `xsl:next-match`, `xsl:stylesheet`, `xsl:transform`.
/// >
/// > Any whitespace text node whose following-sibling node is an `xsl:param` or
/// > `xsl:sort` element is removed from the tree, regardless of any `xml:space`
/// > attributes.
///
/// [`is_stripped_stylesheet_text`] implements the first of those three bullets.
/// The other two matter to every walk that decides what an element contains:
/// without them `xml:space="preserve"` makes legal whitespace inside
/// `xsl:choose`, or before an `xsl:param`, look like content.
pub(crate) fn is_significant_text(parent: &Nav<'_>, child: &Nav<'_>) -> bool {
    if !child.node_type().is_text_like() {
        return false;
    }
    // §4.2 decides on the merged text node, not on one piece of it.
    if !whitespace::is_all_xml_whitespace(&text_run_value(child)) {
        return true;
    }
    if is_stripped_stylesheet_text(child) {
        return false;
    }
    if parent.namespace_uri() == XSLT_NS && ALWAYS_STRIP_WHITESPACE.contains(&parent.local_name()) {
        return false;
    }
    !followed_by_param_or_sort(child)
}

/// Whether the next sibling element of a text node is `xsl:param` or `xsl:sort`
/// (§4.2, last bullet). Comments and processing instructions are removed by the
/// first bullet, so they are skipped here; so are the other text nodes of the
/// same run, which the second bullet merges with this one (and which are
/// whitespace too, or this test would not be reached).
fn followed_by_param_or_sort(child: &Nav<'_>) -> bool {
    let mut cursor = child.clone();
    while cursor.move_to_next_sibling() {
        match cursor.node_type() {
            DomNodeType::Comment | DomNodeType::ProcessingInstruction => continue,
            kind if kind.is_text_like() => continue,
            DomNodeType::Element => {
                return cursor.namespace_uri() == XSLT_NS
                    && matches!(cursor.local_name(), "param" | "sort")
            }
            _ => return false,
        }
    }
    false
}

/// The first name that occurs twice in `names`.
fn duplicate(names: &[QName]) -> Option<&QName> {
    let mut seen: HashSet<&QName> = HashSet::new();
    names.iter().find(|name| !seen.insert(name))
}

/// A content model as the diagnostic prints it, e.g. `(xsl:when+,
/// xsl:otherwise?)`.
fn describe(groups: &[Group]) -> String {
    let parts: Vec<String> = groups
        .iter()
        .map(|group| {
            let names = group
                .names
                .iter()
                .map(|name| format!("xsl:{name}"))
                .collect::<Vec<_>>()
                .join(" | ");
            let names = if group.names.len() > 1 {
                format!("({names})")
            } else {
                names
            };
            let occurs = match group.occurs {
                Occurs::ZeroOrOne => "?",
                Occurs::ZeroOrMore => "*",
                Occurs::OneOrMore => "+",
            };
            format!("{names}{occurs}")
        })
        .collect();
    format!("({})", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_names_a_distinct_element() {
        let mut seen = HashSet::new();
        for spec in TABLE {
            assert!(seen.insert(spec.local), "xsl:{} appears twice", spec.local);
        }
    }

    #[test]
    fn a_rows_avt_attributes_are_attributes_it_allows() {
        for spec in TABLE {
            for name in spec.avt {
                assert!(
                    spec.required.contains(name) || spec.optional.contains(name),
                    "xsl:{} lists {name} as an AVT but does not allow it",
                    spec.local
                );
            }
        }
    }

    #[test]
    fn every_group_name_is_an_element_of_the_table() {
        for spec in TABLE {
            let groups = match &spec.content {
                Content::Groups(groups) | Content::GroupsThenSequence(groups) => *groups,
                _ => continue,
            };
            for group in groups {
                for name in group.names {
                    assert!(
                        spec_of(name).is_some(),
                        "the content of xsl:{} names xsl:{name}, which has no row",
                        spec.local
                    );
                }
            }
        }
    }

    #[test]
    fn xs_decimal_has_no_exponent() {
        for ok in ["2.0", "0", "-1", "+3.5", ".5", "5.", "  2.0  ", "-0.0"] {
            assert!(is_xs_decimal(ok), "{ok:?} is an xs:decimal");
        }
        for bad in [
            "2.0e3", "1E2", "INF", "NaN", "", "-", ".", "1.2.3", "0x1", "1 2",
        ] {
            assert!(!is_xs_decimal(bad), "{bad:?} is not an xs:decimal");
        }
    }

    #[test]
    fn the_content_model_description_reads_like_the_spec() {
        assert_eq!(
            describe(&[
                Group {
                    names: &["when"],
                    occurs: Occurs::OneOrMore
                },
                Group {
                    names: &["otherwise"],
                    occurs: Occurs::ZeroOrOne
                },
            ]),
            "(xsl:when+, xsl:otherwise?)"
        );
        assert_eq!(
            describe(&[Group {
                names: &["sort", "with-param"],
                occurs: Occurs::ZeroOrMore
            }]),
            "((xsl:sort | xsl:with-param)*)"
        );
    }
}
