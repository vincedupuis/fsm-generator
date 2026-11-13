//! Paths: resolving and displaying them, decoding hrefs, expanding glob
//! patterns (PowerShell and cmd don't expand them, so the program does).

use std::path::{Component, Path, PathBuf};

/// `p` with `/` separators.
pub fn slash(p: &str) -> String {
    p.replace('\\', "/")
}

/// `p` resolved against `base` (when relative) and normalized, without touching the file system (like Node's
/// `path.resolve`).
pub fn resolve(base: &Path, p: &str) -> PathBuf {
    let joined = base.join(p);
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), None | Some(Component::RootDir | Component::Prefix(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// An absolute, normalized path from what the user typed.
pub fn absolute(p: &str) -> PathBuf {
    resolve(&cwd(), p)
}

/// How to show a path: relative to the current folder when inside it, `/`-separated.
pub fn display(p: &Path) -> String {
    match p.strip_prefix(cwd()) {
        Ok(rel) if !rel.as_os_str().is_empty() => slash(&rel.to_string_lossy()),
        _ => slash(&p.to_string_lossy()),
    }
}

/// The byte of the `%XX` escape at `i`, if there is one.
fn escape_at(bytes: &[u8], i: usize) -> Option<u8> {
    if bytes.get(i) != Some(&b'%') {
        return None;
    }
    let h = (*bytes.get(i + 1)? as char).to_digit(16)?;
    let l = (*bytes.get(i + 2)? as char).to_digit(16)?;
    Some((h * 16 + l) as u8)
}

/// `decodeURI`: decodes `%XX` escapes (UTF-8), except those of reserved characters (`;/:@&=+$,`; unlike
/// `decodeURI`, `%23` and `%3F` give `#` and `?`, which hrefs encode in file names). Malformed escapes are kept as
/// they are.
pub fn decode_uri(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(b) = escape_at(bytes, i) {
            let len = match b {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 0,
            };
            let mut run = vec![b];
            let mut j = i + 3;
            while run.len() < len {
                let Some(c) = escape_at(bytes, j) else { break };
                run.push(c);
                j += 3;
            }
            if let Ok(text) = std::str::from_utf8(&run) {
                if len == 1 && ";/:@&=+$,".contains(text) {
                    out.extend_from_slice(&bytes[i..j]);
                } else {
                    out.extend_from_slice(text.as_bytes());
                }
                i = j;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ globs

#[derive(Debug)]
enum Token {
    Char(char),
    /// `?`: one character but `/`.
    One,
    /// `*`: any characters but `/`.
    Star,
    /// `**` not followed by `/`: any characters.
    Any,
    /// `**/`: any number of folders.
    Folders,
    /// `[...]`, `[!...]`.
    Class {
        negated: bool,
        items: Vec<(char, char)>,
    },
}

fn tokenize(glob: &str) -> Vec<Token> {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            i += 1;
            if chars.get(i + 1) == Some(&'/') {
                i += 1;
                out.push(Token::Folders);
            } else {
                out.push(Token::Any);
            }
        } else if c == '*' {
            out.push(Token::Star);
        } else if c == '?' {
            out.push(Token::One);
        } else if c == '[' {
            match chars[i + 1..].iter().position(|&x| x == ']') {
                None => out.push(Token::Char('[')),
                Some(len) => {
                    let mut body = &chars[i + 1..i + 1 + len];
                    let negated = body.first() == Some(&'!');
                    if negated {
                        body = &body[1..];
                    }
                    let mut items = Vec::new();
                    let mut k = 0;
                    while k < body.len() {
                        if k + 2 < body.len() && body[k + 1] == '-' {
                            items.push((body[k], body[k + 2]));
                            k += 3;
                        } else {
                            items.push((body[k], body[k]));
                            k += 1;
                        }
                    }
                    out.push(Token::Class { negated, items });
                    i += len + 1;
                }
            }
        } else {
            out.push(Token::Char(c));
        }
        i += 1;
    }
    out
}

fn matches(tokens: &[Token], path: &[char]) -> bool {
    let Some((first, rest)) = tokens.split_first() else { return path.is_empty() };
    match first {
        Token::Char(c) => path.first() == Some(c) && matches(rest, &path[1..]),
        Token::One => path.first().is_some_and(|&c| c != '/') && matches(rest, &path[1..]),
        Token::Class { negated, items } => {
            path.first().is_some_and(|&c| {
                let inside = items.iter().any(|&(a, b)| a <= c && c <= b);
                inside != *negated
            }) && matches(rest, &path[1..])
        }
        Token::Star => {
            for n in 0..=path.len() {
                if matches(rest, &path[n..]) {
                    return true;
                }
                if n < path.len() && path[n] == '/' {
                    break;
                }
            }
            false
        }
        Token::Any => (0..=path.len()).any(|n| matches(rest, &path[n..])),
        Token::Folders => {
            // (?:.*/)? : nothing, or anything ending with a '/'
            matches(rest, path) || (0..path.len()).any(|n| path[n] == '/' && matches(rest, &path[n + 1..]))
        }
    }
}

fn has_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// The files matching `pattern` (`*`, `?`, `[...]`, `**`), sorted; the path itself when it has no wildcards.
pub fn expand_glob(pattern: &str) -> Vec<PathBuf> {
    let p = slash(pattern);
    if !has_magic(&p) {
        return vec![absolute(&p)];
    }
    let parts: Vec<&str> = p.split('/').collect();
    let first = parts.iter().position(|s| has_magic(s)).unwrap();
    let base = parts[..first].join("/");
    let base = if !base.is_empty() {
        base
    } else if p.starts_with('/') {
        "/".to_string()
    } else {
        ".".to_string()
    };
    let rest = parts[first..].join("/");
    let tokens = tokenize(&rest);
    let mut out = Vec::new();
    fn walk(dir: &Path, rel: &str, tokens: &[Token], out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            let full = dir.join(&name);
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if std::fs::metadata(&full).is_ok_and(|m| m.is_dir()) {
                walk(&full, &r, tokens, out);
            } else if matches(tokens, &r.chars().collect::<Vec<_>>()) {
                out.push(full);
            }
        }
    }
    walk(&absolute(&base), "", &tokens, &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(glob: &str, path: &str) -> bool {
        matches(&tokenize(glob), &path.chars().collect::<Vec<_>>())
    }

    #[test]
    fn globs() {
        assert!(m("*.fsm", "a.fsm"));
        assert!(!m("*.fsm", "x/a.fsm"));
        assert!(m("**/*.fsm", "a.fsm"));
        assert!(m("**/*.fsm", "x/y/a.fsm"));
        assert!(m("x/**", "x/y/a.fsm"));
        assert!(m("[a-c]?.fsm", "bz.fsm"));
        assert!(!m("[!a-c]?.fsm", "bz.fsm"));
    }

    #[test]
    fn uris() {
        assert_eq!(decode_uri("My%20Machine.fsm"), "My Machine.fsm");
        assert_eq!(decode_uri("a%23b%2Fc%3F"), "a#b%2Fc?");
        assert_eq!(decode_uri("caf%C3%A9%zz"), "café%zz");
    }

    #[test]
    fn resolving() {
        assert_eq!(resolve(Path::new("/a/b"), "../c/./d.fsm"), PathBuf::from("/a/c/d.fsm"));
        assert_eq!(resolve(Path::new("/a"), "/x/../y"), PathBuf::from("/y"));
    }
}
