//! Grammar of the text a state machine may contain, with the same rules and
//! messages as fsm-editor-vscode/media/expressions.js.
//!
//! The state machine holds no variables: behaviors and conditions are calls to
//! functions without arguments; the parentheses only mark the call.
//!
//! ```text
//! actions   := call (';' call)*                      entry, exit, do, effect
//! condition := or                                     guard, invariant, pre/postcondition
//! or        := and ('||' and)*
//! and       := unary ('&&' unary)*
//! unary     := '!' unary | call | '(' or ')'
//! guard     := 'else' | condition                     'else' only on choice/junction branches
//! trigger   := event | 'after(' number unit ')'       unit: ms, s, m, h
//! event     := name                                   also used for deferrable events
//! call      := name '()'
//! name      := [A-Za-z_][A-Za-z0-9_]*
//! ```

use serde::Serialize;

use crate::model::{js_space, js_trim};

/// A condition tree: `{ op: "call", name }`, `{ op: "not", arg }`, `{ op: "and" | "or", args }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Condition {
    Call { name: String },
    Not { arg: Box<Condition> },
    And { args: Vec<Condition> },
    Or { args: Vec<Condition> },
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Name(String),
    AndAnd,
    OrOr,
    Open,
    Close,
    Bang,
    Semi,
}

impl Token {
    /// How the token shows in messages (`tok.v ?? tok.t`).
    fn text(&self) -> &str {
        match self {
            Token::Name(n) => n,
            Token::AndAnd => "&&",
            Token::OrOr => "||",
            Token::Open => "(",
            Token::Close => ")",
            Token::Bang => "!",
            Token::Semi => ";",
        }
    }

    /// The token's type (`tok.t`).
    fn kind(&self) -> &str {
        match self {
            Token::Name(_) => "name",
            other => other.text(),
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if js_space(c) {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            tokens.push(Token::Name(chars[i..j].iter().collect()));
            i = j;
        } else if (c == '&' || c == '|') && chars.get(i + 1) == Some(&c) {
            tokens.push(if c == '&' { Token::AndAnd } else { Token::OrOr });
            i += 2;
        } else if let Some(t) = match c {
            '(' => Some(Token::Open),
            ')' => Some(Token::Close),
            '!' => Some(Token::Bang),
            ';' => Some(Token::Semi),
            _ => None,
        } {
            tokens.push(t);
            i += 1;
        } else if c == '.' {
            return Err("Dotted names are not allowed: use a single function name like doThing().".to_string());
        } else if "<>=+-*/%&|".contains(c) || c.is_ascii_digit() {
            return Err(format!(
                "'{c}' is not allowed: the state machine has no variables, so use function calls like isReady() combined with !, && and ||."
            ));
        } else {
            return Err(format!("Unexpected character '{c}'."));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    i: usize,
}

impl Parser {
    fn new(text: &str) -> Result<Parser, String> {
        Ok(Parser { tokens: tokenize(text)?, i: 0 })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.i)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.i).cloned();
        self.i += 1;
        t
    }

    fn call_name(&mut self) -> Result<String, String> {
        let name = match self.next() {
            Some(Token::Name(n)) => n,
            Some(t) => return Err(format!("Expected a function call, found '{}'.", t.kind())),
            None => return Err("Expected a function call.".to_string()),
        };
        if self.next() != Some(Token::Open) {
            return Err(format!("'{name}' must be a function call: write {name}()."));
        }
        if self.next() != Some(Token::Close) {
            return Err(format!("Functions take no arguments: write {name}()."));
        }
        Ok(name)
    }

    fn or(&mut self) -> Result<Condition, String> {
        let mut args = vec![self.and()?];
        while self.peek() == Some(&Token::OrOr) {
            self.next();
            args.push(self.and()?);
        }
        Ok(if args.len() == 1 { args.pop().unwrap() } else { Condition::Or { args } })
    }

    fn and(&mut self) -> Result<Condition, String> {
        let mut args = vec![self.unary()?];
        while self.peek() == Some(&Token::AndAnd) {
            self.next();
            args.push(self.unary()?);
        }
        Ok(if args.len() == 1 { args.pop().unwrap() } else { Condition::And { args } })
    }

    fn unary(&mut self) -> Result<Condition, String> {
        match self.peek() {
            Some(Token::Bang) => {
                self.next();
                Ok(Condition::Not { arg: Box::new(self.unary()?) })
            }
            Some(Token::Open) => {
                self.next();
                let inner = self.or()?;
                if self.next() != Some(Token::Close) {
                    return Err("Missing closing parenthesis.".to_string());
                }
                Ok(inner)
            }
            _ => Ok(Condition::Call { name: self.call_name()? }),
        }
    }

    fn end(&self) -> Result<(), String> {
        match self.peek() {
            Some(t) => Err(format!("Unexpected '{}'.", t.text())),
            None => Ok(()),
        }
    }
}

/// A condition built from calls, !, && and ||. Empty text is allowed (no condition).
pub fn parse_condition(text: &str) -> Result<Option<Condition>, String> {
    let s = js_trim(text);
    if s.is_empty() {
        return Ok(None);
    }
    let mut p = Parser::new(s)?;
    let c = p.or()?;
    p.end()?;
    Ok(Some(c))
}

pub fn check_condition(text: &str) -> Result<(), String> {
    parse_condition(text).map(|_| ())
}

/// Transition guard: a condition, or `else` when `allow_else` is set.
pub fn check_guard(text: &str, allow_else: bool) -> Result<(), String> {
    let s = js_trim(text);
    if s == "else" {
        return if allow_else {
            Ok(())
        } else {
            Err("[else] is only allowed on transitions leaving a choice or junction.".to_string())
        };
    }
    check_condition(s)
}

/// Names of the functions a behavior calls, in order: calls separated by ';'. Empty text is allowed (no behavior).
pub fn parse_actions(text: &str) -> Result<Vec<String>, String> {
    let mut s = js_trim(text);
    // A trailing ';' (and the white space after it) is allowed.
    if let Some(semi) = s.rfind(';')
        && s[semi + 1..].chars().all(js_space)
    {
        s = &s[..semi];
    }
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut p = Parser::new(s)?;
    let mut calls = vec![p.call_name()?];
    while p.peek() == Some(&Token::Semi) {
        p.next();
        calls.push(p.call_name()?);
    }
    match p.peek() {
        None => Ok(calls),
        Some(Token::AndAnd | Token::OrOr | Token::Bang) => {
            Err("Actions cannot use !, && or ||: separate several calls with ;".to_string())
        }
        Some(t) => Err(format!("Unexpected '{}': separate several calls with ;", t.text())),
    }
}

pub fn check_actions(text: &str) -> Result<(), String> {
    parse_actions(text).map(|_| ())
}

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `s` starts with `word` followed by a word boundary (`/^word\b/`).
fn starts_with_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word).is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// `s` without its first `(` and what follows, nor the white space before it.
fn before_paren(s: &str) -> &str {
    match s.find('(') {
        Some(i) => s[..i].trim_end_matches(js_space),
        None => s,
    }
}

/// Event name (used for triggers and deferrable events).
pub fn check_event(text: &str) -> Result<(), String> {
    let s = js_trim(text);
    if s.is_empty() {
        return Err("Empty event name.".to_string());
    }
    if starts_with_word(s, "after") {
        return Err("A time event cannot be deferred; use a named event.".to_string());
    }
    if s.contains('(') {
        return Err(format!("Events are plain names without (): write {}.", before_paren(s)));
    }
    if !is_name(s) {
        return Err(format!("'{s}' is not a valid event name (letters, digits and _ only)."));
    }
    Ok(())
}

/// A plain name (letters, digits and _), e.g. a stereotype.
pub fn check_name(text: &str) -> Result<(), String> {
    let s = js_trim(text);
    if s.is_empty() || is_name(s) {
        return Ok(());
    }
    Err(format!("'{s}' is not a valid name: use letters, digits and _ only, not starting with a digit."))
}

/// `after(<number><unit>)`: the number's text and the unit.
fn parse_time(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("after")?.trim_start_matches(js_space);
    let rest = rest.strip_prefix('(')?.trim_start_matches(js_space);
    let int = rest.bytes().take_while(u8::is_ascii_digit).count();
    if int == 0 {
        return None;
    }
    let mut n = int;
    if rest[n..].starts_with('.') {
        let frac = rest[n + 1..].bytes().take_while(u8::is_ascii_digit).count();
        if frac > 0 {
            n += 1 + frac;
        }
    }
    let number = &rest[..n];
    let rest = rest[n..].trim_start_matches(js_space);
    let unit_len = if rest.starts_with("ms") {
        2
    } else if rest.starts_with(['s', 'm', 'h']) {
        1
    } else {
        return None;
    };
    let unit = &rest[..unit_len];
    let rest = rest[unit_len..].trim_start_matches(js_space);
    if rest == ")" { Some((number, unit)) } else { None }
}

/// One trigger: an event name, or after(<number><ms|s|m|h>).
pub fn check_trigger(text: &str) -> Result<(), String> {
    let s = js_trim(text);
    if starts_with_word(s, "after") {
        let Some((number, _)) = parse_time(s) else {
            return Err(
                "Time triggers are written after(<number><unit>) with unit ms, s, m or h, e.g. after(500ms) or after(2s).".to_string()
            );
        };
        if number.parse::<f64>().unwrap_or(0.0) <= 0.0 {
            return Err("The time of an after() trigger must be greater than zero.".to_string());
        }
        return Ok(());
    }
    if starts_with_word(s, "when") {
        return Err("Change events (when) are not supported: use a named event or after(...).".to_string());
    }
    if s.is_empty() {
        return Err("Empty trigger.".to_string());
    }
    if s.contains('(') {
        return Err(format!(
            "Events are plain names without (): write {}. The only exception is after(...).",
            before_paren(s)
        ));
    }
    if !is_name(s) {
        return Err(format!("'{s}' is not a valid event name (letters, digits and _ only)."));
    }
    Ok(())
}

/// Parses an after() trigger into milliseconds.
pub fn time_trigger_ms(trigger: &str) -> Option<f64> {
    let (number, unit) = parse_time(js_trim(trigger))?;
    let factor = match unit {
        "ms" => 1.0,
        "s" => 1000.0,
        "m" => 60000.0,
        _ => 3600000.0,
    };
    Some(number.parse::<f64>().ok()? * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions() {
        assert_eq!(parse_condition("").unwrap(), None);
        let c = parse_condition("a() && !(b() || c())").unwrap().unwrap();
        assert_eq!(
            c,
            Condition::And {
                args: vec![
                    Condition::Call { name: "a".into() },
                    Condition::Not {
                        arg: Box::new(Condition::Or {
                            args: vec![Condition::Call { name: "b".into() }, Condition::Call { name: "c".into() }]
                        })
                    }
                ]
            }
        );
        assert_eq!(parse_condition("a").unwrap_err(), "'a' must be a function call: write a().");
        assert_eq!(parse_condition("a(x)").unwrap_err(), "Functions take no arguments: write a().");
        assert_eq!(
            parse_condition("x.y()").unwrap_err(),
            "Dotted names are not allowed: use a single function name like doThing()."
        );
        assert_eq!(parse_condition("a() b()").unwrap_err(), "Unexpected 'b'.");
        assert_eq!(parse_condition("(a()").unwrap_err(), "Missing closing parenthesis.");
    }

    #[test]
    fn actions() {
        assert_eq!(parse_actions(" a(); b() ; ").unwrap(), vec!["a", "b"]);
        assert_eq!(
            parse_actions("a() && b()").unwrap_err(),
            "Actions cannot use !, && or ||: separate several calls with ;"
        );
        assert_eq!(parse_actions("a() b()").unwrap_err(), "Unexpected 'b': separate several calls with ;");
    }

    #[test]
    fn triggers() {
        assert_eq!(time_trigger_ms("after(5m)"), Some(300000.0));
        assert_eq!(time_trigger_ms(" after ( 1.5 s ) "), Some(1500.0));
        assert_eq!(time_trigger_ms("after(5)"), None);
        assert_eq!(time_trigger_ms("afterwards"), None);
        assert!(check_trigger("after(0s)").is_err());
        assert!(check_trigger("go").is_ok());
        assert_eq!(
            check_trigger("go(1)").unwrap_err(),
            "Events are plain names without (): write go. The only exception is after(...)."
        );
        assert!(check_trigger("when(x)").is_err());
    }
}
