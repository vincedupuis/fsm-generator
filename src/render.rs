//! Renders a code model with a Jinja template (MiniJinja). One template
//! produces any number of files through `{% filter file("path") %}...{% endfilter %}`
//! blocks; text outside those blocks is ignored.
//!
//! A template may start with front matter (a YAML subset) between `---` lines:
//!
//! ```text
//! ---
//! lang: ts                   # language preset for `guard` and `id`
//! guard:
//!   prefix: "this.actions."  # overrides the preset
//! keywords: [signals]        # extra reserved words
//! options:                   # free values, available as {{ options.x }}
//!   namespace: fsm
//! ---
//! ```
//!
//! The filters and functions are built in on purpose: templates are data, so
//! generating code never runs code from a repository.

use std::sync::{Arc, Mutex};

use minijinja::syntax::SyntaxConfig;
use minijinja::value::{Kwargs, Rest, Serde, Value, ValueKind, merge_maps};
use minijinja::{AutoEscape, Environment, Error, ErrorKind};
use serde::{Serialize, Serializer};

use crate::codemodel::CodeModel;
use crate::languages::{LanguageConfig, language_config};
use crate::validation::Severity;

// ------------------------------------------------------------------ templates

pub struct Template {
    /// Parsed front matter.
    pub config: Vec<(String, FrontValue)>,
    pub lang: LanguageConfig,
    pub body: String,
    /// Line of the body's first line in the template file (for error messages).
    pub body_line: usize,
}

/// A template that can't be read or rendered. `line` is a line of the template file.
#[derive(Debug)]
pub struct TemplateError {
    pub message: String,
    pub line: Option<usize>,
}

impl TemplateError {
    fn new(message: impl Into<String>) -> TemplateError {
        TemplateError { message: message.into(), line: None }
    }
}

pub fn parse_template(text: &str) -> Result<Template, TemplateError> {
    let src = text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n").replace('\r', "\n");
    let (config, body, body_line) = match split_front_matter(&src) {
        Some((header, end)) => {
            let config = parse_front_matter(header).map_err(TemplateError::new)?;
            (config, &src[end..], src[..end].matches('\n').count() + 1)
        }
        None => (Vec::new(), src.as_str(), 1),
    };
    Ok(Template { lang: language_config(&config), config, body: body.to_string(), body_line })
}

/// The front matter (`/^---[ \t]*\n([\s\S]*?)\n---[ \t]*(?:\n|$)/`): what is between the `---` lines, and where the
/// body starts.
fn split_front_matter(src: &str) -> Option<(&str, usize)> {
    let rest = src.strip_prefix("---")?.trim_start_matches([' ', '\t']).strip_prefix('\n')?;
    let start = src.len() - rest.len();
    let mut from = start;
    while let Some(i) = src[from..].find("\n---") {
        let k = from + i;
        let after = src[k + 4..].trim_start_matches([' ', '\t']);
        if after.is_empty() {
            return Some((&src[start..k], src.len()));
        }
        if after.starts_with('\n') {
            return Some((&src[start..k], src.len() - after.len() + 1));
        }
        from = k + 1;
    }
    None
}

// ------------------------------------------------------------------ front matter

/// A front matter value. Maps keep the order of their keys.
#[derive(Debug, Clone)]
pub enum FrontValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<FrontValue>),
    Map(Vec<(String, FrontValue)>),
}

impl FrontValue {
    /// The value as JavaScript's `String()` prints it.
    pub fn to_text(&self) -> String {
        match self {
            FrontValue::Null => "null".to_string(),
            FrontValue::Bool(b) => b.to_string(),
            FrontValue::Num(n) => crate::codemodel::Num(*n).to_string(),
            FrontValue::Str(s) => s.clone(),
            FrontValue::List(l) => l.iter().map(FrontValue::to_text).collect::<Vec<_>>().join(","),
            FrontValue::Map(_) => "[object Object]".to_string(),
        }
    }
}

impl Serialize for FrontValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            FrontValue::Null => s.serialize_none(),
            FrontValue::Bool(b) => s.serialize_bool(*b),
            FrontValue::Num(n) => crate::codemodel::Num(*n).serialize(s),
            FrontValue::Str(v) => s.serialize_str(v),
            FrontValue::List(l) => l.serialize(s),
            FrontValue::Map(m) => {
                use serde::ser::SerializeMap;
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// A small YAML subset: `key: value` maps nested by indentation, `- item` lists, inline `[a, b]` lists, quoted or
/// plain scalars, numbers, booleans, null and `#` comments.
pub fn parse_front_matter(text: &str) -> Result<Vec<(String, FrontValue)>, String> {
    struct Line {
        indent: usize,
        text: String,
        no: usize,
    }
    let lines: Vec<Line> = text
        .split('\n')
        .enumerate()
        .map(|(i, raw)| (strip_comment(raw), i + 1))
        .filter(|(raw, _)| !raw.trim().is_empty())
        .map(|(raw, no)| Line { indent: raw.len() - raw.trim_start().len(), text: raw.trim().to_string(), no })
        .collect();

    fn block(lines: &[Line], i: &mut usize, indent: usize) -> Result<FrontValue, String> {
        if *i < lines.len() && lines[*i].text.starts_with("- ") {
            let mut list = Vec::new();
            while *i < lines.len() && lines[*i].indent == indent && lines[*i].text.starts_with("- ") {
                list.push(scalar(lines[*i].text[2..].trim())?);
                *i += 1;
            }
            return Ok(FrontValue::List(list));
        }
        let mut map: Vec<(String, FrontValue)> = Vec::new();
        while *i < lines.len() && lines[*i].indent == indent {
            let line = &lines[*i];
            let Some((key, value)) = key_value(&line.text) else {
                return Err(format!("Front matter line {}: expected 'key: value'.", line.no));
            };
            *i += 1;
            let v = match value {
                Some(v) if !v.is_empty() => scalar(v)?,
                _ if *i < lines.len() && lines[*i].indent > indent => block(lines, i, lines[*i].indent)?,
                _ => FrontValue::Null,
            };
            match map.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = v,
                None => map.push((key.to_string(), v)),
            }
        }
        if *i < lines.len() && lines[*i].indent > indent {
            return Err(format!("Front matter line {}: unexpected indentation.", lines[*i].no));
        }
        Ok(FrontValue::Map(map))
    }

    let mut i = 0;
    let result = if lines.is_empty() { FrontValue::Map(Vec::new()) } else { block(&lines, &mut i, lines[0].indent)? };
    if i < lines.len() {
        return Err(format!("Front matter line {}: unexpected indentation.", lines[i].no));
    }
    match result {
        FrontValue::Map(m) => Ok(m),
        _ => Err("Front matter must be a map of keys.".to_string()),
    }
}

/// `key: value` (`/^([A-Za-z_][\w-]*)\s*:(?:\s+(.*))?$/`): the key and the value, if any.
fn key_value(text: &str) -> Option<(&str, Option<&str>)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return None;
    }
    let n = 1 + bytes[1..].iter().take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-').count();
    let key = &text[..n];
    let rest = text[n..].trim_start().strip_prefix(':')?;
    if rest.is_empty() {
        return Some((key, None));
    }
    let value = rest.trim_start();
    if value.len() == rest.len() {
        return None; // no white space after the colon
    }
    Some((key, Some(value)))
}

fn strip_comment(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == '\\' && q == '"' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
                    return chars[..i].iter().collect::<String>().trim_end().to_string();
                }
            }
        }
        i += 1;
    }
    line.trim_end().to_string()
}

fn scalar(text: &str) -> Result<FrontValue, String> {
    let s = text.trim();
    if s.starts_with('[') && s.ends_with(']') && s.len() >= 2 {
        let inner = s[1..s.len() - 1].trim();
        if inner.is_empty() {
            return Ok(FrontValue::List(Vec::new()));
        }
        return split_list(inner).iter().map(|x| scalar(x)).collect::<Result<_, _>>().map(FrontValue::List);
    }
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return serde_json::from_str::<String>(s)
            .map(FrontValue::Str)
            .map_err(|_| format!("Front matter: invalid string {s}."));
    }
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        return Ok(FrontValue::Str(s[1..s.len() - 1].replace("''", "'")));
    }
    Ok(match s {
        "true" => FrontValue::Bool(true),
        "false" => FrontValue::Bool(false),
        "null" | "~" => FrontValue::Null,
        _ if is_number(s) => FrontValue::Num(s.parse().unwrap_or(0.0)),
        _ => FrontValue::Str(s.to_string()),
    })
}

/// `/^-?\d+(\.\d+)?$/`
fn is_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

fn split_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' && q == '"' {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                    continue;
                }
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == ',' {
                    out.push(cur.trim().to_string());
                    cur.clear();
                    continue;
                }
            }
        }
        cur.push(c);
    }
    out.push(cur.trim().to_string());
    out
}

// ------------------------------------------------------------------ rendering

pub struct OutputFile {
    pub path: String,
    pub content: String,
}

/// A warning or error a template reports with `warn()` or `error()`, about the element with xmi:id `id` ('' when
/// none).
pub struct TemplateIssue {
    pub severity: Severity,
    pub message: String,
    pub id: String,
}

#[derive(Default)]
struct Sink {
    files: Vec<OutputFile>,
    issues: Vec<TemplateIssue>,
}

/// A template ready to render code models, one after the other.
pub struct Renderer {
    env: Environment<'static>,
    name: String,
    body_line: usize,
    extra: Value,
    sink: Arc<Mutex<Sink>>,
}

impl Renderer {
    /// Compiles `template`; `name` is how errors name it.
    pub fn new(template: &Template, name: &str) -> Result<Renderer, TemplateError> {
        let sink: Arc<Mutex<Sink>> = Arc::default();
        let mut env = Environment::new();
        env.set_syntax(
            SyntaxConfig::builder().trim_blocks(true).lstrip_blocks(true).keep_trailing_newline(true).build().unwrap(),
        );
        env.set_auto_escape_callback(|_| AutoEscape::None);
        register(&mut env, &template.lang, &sink);
        env.add_template_owned(name.to_string(), template.body.clone())
            .map_err(|e| describe(&e, template.body_line))?;
        let options = match template.config.iter().find(|(k, _)| k == "options") {
            Some((_, v)) if !matches!(v, FrontValue::Null) => Value::from(Serde(v)),
            _ => Value::from(std::collections::BTreeMap::<String, Value>::new()),
        };
        let config = Value::from(Serde(&FrontValue::Map(template.config.clone())));
        let extra =
            minijinja::context! { lang => Value::from(Serde(&template.lang)), options => options, config => config };
        Ok(Renderer { env, name: name.to_string(), body_line: template.body_line, extra, sink })
    }

    /// Renders `model` and returns the files declared by the template's file blocks, and what the template reported
    /// with `warn()` and `error()`. No files when it reported errors.
    pub fn render(&self, model: &CodeModel) -> Result<(Vec<OutputFile>, Vec<TemplateIssue>), TemplateError> {
        *self.sink.lock().unwrap() = Sink::default();
        let ctx = merge_maps([self.extra.clone(), Value::from(Serde(model))]);
        let template = self.env.get_template(&self.name).map_err(|e| describe(&e, self.body_line))?;
        template.render(ctx).map_err(|e| describe(&e, self.body_line))?;
        let Sink { mut files, issues } = std::mem::take(&mut *self.sink.lock().unwrap());
        if issues.iter().any(|i| i.severity == Severity::Error) {
            return Ok((Vec::new(), issues));
        }
        if files.is_empty() {
            return Err(TemplateError::new(
                "The template declares no files: wrap the output in {% filter file(\"Name.ext\") %}...{% endfilter %} blocks.",
            ));
        }
        for f in &mut files {
            let content = f.content.replace("\r\n", "\n").replace('\r', "\n");
            f.content = content.trim_start_matches('\n').to_string();
        }
        Ok((files, issues))
    }
}

/// A MiniJinja error as a message, with the line of the template file.
fn describe(e: &Error, body_line: usize) -> TemplateError {
    let mut message = match e.detail() {
        Some(detail) => format!("{}: {detail}", e.kind()),
        None => e.kind().to_string(),
    };
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        message.push_str(&format!(" ({inner})"));
        source = inner.source();
    }
    TemplateError { message, line: e.line().map(|l| l + body_line - 1) }
}

// ------------------------------------------------------------------ filters and functions

/// A value as JavaScript's `String(x ?? '')` prints it.
fn text(v: &Value) -> String {
    match v.kind() {
        ValueKind::Undefined | ValueKind::None => String::new(),
        ValueKind::Bool => if v.is_true() { "true" } else { "false" }.to_string(),
        ValueKind::Seq => v.try_iter().map(|it| it.map(|x| text(&x)).collect::<Vec<_>>().join(",")).unwrap_or_default(),
        _ => v.to_string(),
    }
}

fn op_error(message: String) -> Error {
    Error::new(ErrorKind::InvalidOperation, message)
}

/// Words of an identifier: `blinkLed`, `blink_led` and `BlinkLED` all give blink, led.
pub fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut spaced = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            // aB → a B; ABc → A Bc
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase()))
            {
                spaced.push(' ');
            }
        }
        spaced.push(c);
    }
    spaced.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn cap(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

pub fn pascal(s: &str) -> String {
    words(s).iter().map(|w| cap(w)).collect()
}

pub fn camel(s: &str) -> String {
    let p = pascal(s);
    let mut c = p.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

pub fn snake(s: &str) -> String {
    words(s).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("_")
}

pub fn upper_snake(s: &str) -> String {
    words(s).iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join("_")
}

pub fn escape_id(name: &str, lang: &LanguageConfig) -> String {
    if lang.keywords.iter().any(|k| k == name) { format!("{name}{}", lang.escape) } else { name.to_string() }
}

/// Prints a condition tree (`{op: "call", name}`, `{op: "not", arg}`, `{op: "and" | "or", args}`) in the syntax of
/// `lang`, with `overrides` applied to its guard syntax; `guard.true` when there is none.
fn print_condition(c: &Value, lang: &LanguageConfig, overrides: &Kwargs) -> Result<String, Error> {
    let mut g = lang.guard.clone();
    for key in overrides.args() {
        let value: Value = overrides.get(key)?;
        if let (Some(field), Some(s)) = (g.field(key), value.as_str()) {
            *field = s.to_string();
        }
    }
    if c.is_undefined() || c.is_none() {
        return Ok(g.true_.clone());
    }
    // Precedence: or < and < not < call.
    fn rank(op: &str) -> u8 {
        match op {
            "or" => 1,
            "and" => 2,
            _ => 3,
        }
    }
    fn print(x: &Value, min: u8, g: &crate::languages::GuardSyntax, lang: &LanguageConfig) -> Result<String, Error> {
        let op = x.get_attr("op")?;
        let op = op.as_str().ok_or_else(|| op_error(format!("guard: {x} is not a condition")))?;
        let s = match op {
            "call" => format!("{}{}{}", g.prefix, escape_id(&text(&x.get_attr("name")?), lang), g.suffix),
            "not" => format!("{}{}", g.not, print(&x.get_attr("arg")?, 3, g, lang)?),
            "and" | "or" => {
                let sep = if op == "and" { &g.and } else { &g.or };
                let args = x.get_attr("args")?;
                let mut parts = Vec::new();
                for a in args.try_iter()? {
                    parts.push(print(&a, rank(op), g, lang)?);
                }
                parts.join(sep)
            }
            other => return Err(op_error(format!("guard: unknown condition operator '{other}'"))),
        };
        Ok(if rank(op) < min { format!("({s})") } else { s })
    }
    print(c, 0, &g, lang)
}

/// A path relative to the output folder, without `..`.
pub fn check_output_path(p: &str) -> Result<String, String> {
    let path = p.trim().replace('\\', "/");
    if path.is_empty() {
        return Err("A file block has an empty file name.".to_string());
    }
    let bytes = path.as_bytes();
    if path.starts_with('/') || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':') {
        return Err(format!("Generated file '{path}' must be a relative path."));
    }
    let parts: Vec<&str> = path.split('/').filter(|x| !x.is_empty() && *x != ".").collect();
    if parts.contains(&"..") {
        return Err(format!("Generated file '{path}' must stay inside the output folder (no '..')."));
    }
    Ok(parts.join("/"))
}

fn register(env: &mut Environment<'static>, lang: &LanguageConfig, sink: &Arc<Mutex<Sink>>) {
    // {% filter file("a/B.h") %}...{% endfilter %}: writes the block's output to a file.
    let files = sink.clone();
    env.add_filter("file", move |content: String, path: Value| -> Result<String, Error> {
        let path = check_output_path(&text(&path)).map_err(op_error)?;
        let mut sink = files.lock().unwrap();
        if sink.files.iter().any(|f| f.path == path) {
            return Err(op_error(format!("The template writes '{path}' twice.")));
        }
        sink.files.push(OutputFile { path, content });
        Ok(String::new())
    });

    let l = lang.clone();
    env.add_filter("guard", move |c: Value, overrides: Kwargs| print_condition(&c, &l, &overrides));
    let l = lang.clone();
    env.add_filter("id", move |s: Value| escape_id(&text(&s), &l));
    let l = lang.clone();
    env.add_filter("pascal", move |s: Value| escape_id(&pascal(&text(&s)), &l));
    let l = lang.clone();
    env.add_filter("camel", move |s: Value| escape_id(&camel(&text(&s)), &l));
    let l = lang.clone();
    env.add_filter("snake", move |s: Value| escape_id(&snake(&text(&s)), &l));
    let l = lang.clone();
    env.add_filter("upper_snake", move |s: Value| escape_id(&upper_snake(&text(&s)), &l));
    // A double-quoted string literal (valid in C, C++, Java, TypeScript and Python).
    env.add_filter("quote", |s: Value| serde_json::to_string(&text(&s)).unwrap_or_default());

    // {% do warn("message", id=id) %} reports a warning; error(...) an error, which stops the generation once the
    // template has been rendered (so that every error is reported). Several arguments are joined.
    for (name, severity) in [("warn", Severity::Warning), ("error", Severity::Error)] {
        let issues = sink.clone();
        env.add_function(name, move |args: Rest<Value>, kwargs: Kwargs| -> Result<String, Error> {
            let message: String = args.iter().map(text).collect();
            let id = if kwargs.has("id") { text(&kwargs.get::<Value>("id")?) } else { String::new() };
            kwargs.assert_all_used()?;
            let mut sink = issues.lock().unwrap();
            if !sink.issues.iter().any(|i| i.severity == severity && i.message == message && i.id == id) {
                sink.issues.push(TemplateIssue { severity, message, id });
            }
            Ok(String::new())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_conversions() {
        assert_eq!(words("blinkLed"), ["blink", "Led"]);
        assert_eq!(words("HTMLParser2Go"), ["HTML", "Parser2", "Go"]);
        assert_eq!(pascal("blink_led"), "BlinkLed");
        assert_eq!(camel("BlinkLED"), "blinkLED");
        assert_eq!(snake("BlinkLED"), "blink_led");
        assert_eq!(upper_snake("blinkLed"), "BLINK_LED");
    }

    #[test]
    fn front_matter() {
        let t = parse_template("---\n# c\nlang: cpp\nguard:\n  prefix: \"c.a.\"  # x\noptions:\n  list: [a, 'b''c', 3]\n  more:\n    - x\n---\nBODY\n").unwrap();
        assert_eq!(t.body, "BODY\n");
        assert_eq!(t.body_line, 11);
        assert_eq!(t.lang.guard.prefix, "c.a.");
        assert!(t.lang.keywords.iter().any(|k| k == "nullptr"));
        let options = &t.config.iter().find(|(k, _)| k == "options").unwrap().1;
        assert_eq!(
            serde_json::to_string(&Value::from(Serde(options))).unwrap(),
            r#"{"list":["a","b'c",3],"more":["x"]}"#
        );
        assert!(parse_front_matter("a:b").is_err());
        assert!(parse_front_matter("a: 1\n  b: 2").is_err());
        let no_front = parse_template("--- not front matter\n").unwrap();
        assert_eq!(no_front.body_line, 1);
    }

    #[test]
    fn output_paths() {
        assert_eq!(check_output_path(" a\\./b.h ").unwrap(), "a/b.h");
        assert!(check_output_path("../x").is_err());
        assert!(check_output_path("C:x").is_err());
        assert!(check_output_path("").is_err());
    }
}
