//! Language presets used by the `guard` and `id` filters. A template picks one
//! with `lang:` in its front matter and can override any field there.

use serde::Serialize;

use crate::render::FrontValue;

#[derive(Debug, Clone, Serialize)]
pub struct GuardSyntax {
    /// Put before every call, e.g. `this.actions.` or `self.`.
    pub prefix: String,
    /// Put after every call name.
    pub suffix: String,
    pub not: String,
    pub and: String,
    pub or: String,
    /// Printed for a missing condition (unguarded or `else` branch).
    #[serde(rename = "true")]
    pub true_: String,
}

impl GuardSyntax {
    fn new(prefix: &str, suffix: &str, not: &str, and: &str, or: &str, true_: &str) -> GuardSyntax {
        GuardSyntax {
            prefix: prefix.into(),
            suffix: suffix.into(),
            not: not.into(),
            and: and.into(),
            or: or.into(),
            true_: true_.into(),
        }
    }

    fn c() -> GuardSyntax {
        GuardSyntax::new("", "()", "!", " && ", " || ", "true")
    }

    /// The field named `key`, to override it.
    pub fn field(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "prefix" => &mut self.prefix,
            "suffix" => &mut self.suffix,
            "not" => &mut self.not,
            "and" => &mut self.and,
            "or" => &mut self.or,
            "true" => &mut self.true_,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LanguageConfig {
    pub name: String,
    /// Reserved words: `id` appends `escape` to identifiers that collide with them.
    pub keywords: Vec<String>,
    pub escape: String,
    pub guard: GuardSyntax,
}

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else", "enum", "extern", "float",
    "for", "goto", "if", "inline", "int", "long", "register", "restrict", "return", "short", "signed", "sizeof",
    "static", "struct", "switch", "typedef", "union", "unsigned", "void", "volatile", "while", "bool", "true", "false",
    "NULL",
];

const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "bitand",
    "bitor",
    "catch",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "delete",
    "dynamic_cast",
    "explicit",
    "export",
    "friend",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "reinterpret_cast",
    "requires",
    "static_assert",
    "static_cast",
    "template",
    "this",
    "thread_local",
    "throw",
    "try",
    "typeid",
    "typename",
    "using",
    "virtual",
    "wchar_t",
    "xor",
    "xor_eq",
];

const TS_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "await",
    "any",
    "boolean",
    "constructor",
    "declare",
    "get",
    "module",
    "require",
    "number",
    "set",
    "string",
    "symbol",
    "type",
    "from",
    "of",
    "undefined",
];

const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "var",
    "record",
    "yield",
    "sealed",
    "permits",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal",
    "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case", "self",
];

fn preset(name: &str) -> Option<LanguageConfig> {
    let words = |lists: &[&[&str]]| lists.iter().flat_map(|l| l.iter().map(|w| w.to_string())).collect::<Vec<_>>();
    let (keywords, guard) = match name {
        "c" => (words(&[C_KEYWORDS]), GuardSyntax::c()),
        "cpp" => (words(&[C_KEYWORDS, CPP_KEYWORDS]), GuardSyntax::c()),
        "ts" => (words(&[TS_KEYWORDS]), GuardSyntax::c()),
        "java" => (words(&[JAVA_KEYWORDS]), GuardSyntax::c()),
        "python" => (words(&[PYTHON_KEYWORDS]), GuardSyntax::new("self.", "()", "not ", " and ", " or ", "True")),
        _ => return None,
    };
    Some(LanguageConfig { name: name.to_string(), keywords, escape: "_".to_string(), guard })
}

/// The preset named `lang` (or a neutral C-like one), with the front matter's overrides applied:
/// `keywords` (more reserved words), `escape` and `guard` (fields of the guard syntax).
pub fn language_config(config: &[(String, FrontValue)]) -> LanguageConfig {
    let get = |key: &str| config.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let name = match get("lang") {
        Some(FrontValue::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let mut lang = preset(&name).unwrap_or_else(|| LanguageConfig {
        name: name.clone(),
        keywords: Vec::new(),
        escape: "_".to_string(),
        guard: GuardSyntax::c(),
    });
    if let Some(FrontValue::List(extra)) = get("keywords") {
        lang.keywords.extend(extra.iter().map(FrontValue::to_text));
    }
    if let Some(FrontValue::Map(guard)) = get("guard") {
        for (k, v) in guard {
            if let (Some(field), FrontValue::Str(s)) = (lang.guard.field(k), v) {
                *field = s.clone();
            }
        }
    }
    if let Some(FrontValue::Str(s)) = get("escape") {
        lang.escape = s.clone();
    }
    lang
}
