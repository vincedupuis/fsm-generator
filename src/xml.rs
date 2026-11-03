//! Minimal XML reader, enough for XMI documents: elements, attributes, text,
//! CDATA, comments, processing instructions and namespaces. Element and
//! attribute names are rewritten to canonical prefixes for known namespace
//! URIs, so documents using other prefixes read the same.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct XmlElement {
    /// Qualified name using the canonical prefix when the namespace is known, e.g. `uml:Model`.
    pub name: String,
    /// Namespace URI of the element ('' when none).
    pub ns: String,
    /// Local part of the name.
    pub local: String,
    attrs: Vec<(String, String)>,
    pub children: Vec<XmlElement>,
    pub text: String,
}

impl XmlElement {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn set_attr(&mut self, name: String, value: String) {
        match self.attrs.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = value,
            None => self.attrs.push((name, value)),
        }
    }
}

#[derive(Debug)]
pub struct XmlError(pub String);

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(message: impl AsRef<str>, line: usize) -> Result<T, XmlError> {
    Err(XmlError(format!("Line {line}: {}", message.as_ref())))
}

fn decode(s: &str, line: usize) -> Result<String, XmlError> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // &#x1F; &#31; &name;
        let len = if let Some(hex) = after.strip_prefix("#x") {
            let n = hex.bytes().take_while(u8::is_ascii_hexdigit).count();
            if n > 0 && hex[n..].starts_with(';') {
                Some((n + 2, u32::from_str_radix(&hex[..n], 16).ok()))
            } else {
                None
            }
        } else if let Some(dec) = after.strip_prefix('#') {
            let n = dec.bytes().take_while(u8::is_ascii_digit).count();
            if n > 0 && dec[n..].starts_with(';') { Some((n + 1, dec[..n].parse::<u32>().ok())) } else { None }
        } else {
            let n = after.bytes().take_while(u8::is_ascii_alphabetic).count();
            if n > 0 && after[n..].starts_with(';') { Some((n, None)) } else { None }
        };
        match len {
            None => {
                out.push('&');
                rest = after;
            }
            Some((n, code)) => {
                let entity = &after[..n];
                if entity.starts_with('#') {
                    match code.and_then(char::from_u32) {
                        Some(c) => out.push(c),
                        None => return err(format!("Invalid character reference &{entity};."), line),
                    }
                } else {
                    out.push(match entity {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "quot" => '"',
                        "apos" => '\'',
                        _ => return err(format!("Unknown entity &{entity};."), line),
                    });
                }
                rest = &after[n + 1..];
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

struct Raw {
    qname: String,
    attrs: Vec<(String, String)>,
    children: Vec<Raw>,
    text: String,
}

struct Parser<'a> {
    text: &'a str,
    i: usize,
    line: usize,
}

impl<'a> Parser<'a> {
    fn advance(&mut self, to: usize) {
        self.line += self.text.as_bytes()[self.i..to].iter().filter(|&&b| b == b'\n').count();
        self.i = to;
    }

    fn rest(&self) -> &'a str {
        &self.text[self.i..]
    }

    fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }

    fn expect(&mut self, s: &str) -> Result<(), XmlError> {
        if !self.starts_with(s) {
            return err(format!("Expected '{s}'."), self.line);
        }
        self.advance(self.i + s.len());
        Ok(())
    }

    fn skip_until(&mut self, end: &str, what: &str) -> Result<&'a str, XmlError> {
        match self.rest().find(end) {
            None => err(format!("Unterminated {what}."), self.line),
            Some(j) => {
                let content = &self.text[self.i..self.i + j];
                self.advance(self.i + j + end.len());
                Ok(content)
            }
        }
    }

    fn skip_space(&mut self) {
        let n: usize = self.rest().chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum();
        self.advance(self.i + n);
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let bytes = self.rest().as_bytes();
        if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
            return err("Expected a name.", self.line);
        }
        let n = 1 + bytes[1..]
            .iter()
            .take_while(|&&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b':'))
            .count();
        let name = self.rest()[..n].to_string();
        self.advance(self.i + n);
        Ok(name)
    }

    fn element(&mut self) -> Result<Raw, XmlError> {
        let start = self.line;
        self.expect("<")?;
        let qname = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_space();
            if self.starts_with("/>") {
                self.advance(self.i + 2);
                return Ok(Raw { qname, attrs, children: Vec::new(), text: String::new() });
            }
            if self.starts_with(">") {
                self.advance(self.i + 1);
                break;
            }
            let an = self.name()?;
            self.skip_space();
            self.expect("=")?;
            self.skip_space();
            let q = match self.rest().chars().next() {
                Some(c @ ('"' | '\'')) => c,
                _ => return err(format!("Attribute {an} must be quoted."), self.line),
            };
            self.advance(self.i + 1);
            let at = self.line;
            let raw = self.skip_until(&q.to_string(), &format!("attribute {an}"))?;
            if raw.contains('<') {
                return err(format!("'<' is not allowed in attribute {an}."), at);
            }
            attrs.push((an, decode(raw, at)?));
        }
        let mut children = Vec::new();
        let mut content = String::new();
        loop {
            if self.i >= self.text.len() {
                return err(format!("Element <{qname}> is not closed."), start);
            }
            if self.starts_with("</") {
                self.advance(self.i + 2);
                let close = self.name()?;
                if close != qname {
                    return err(format!("Expected </{qname}>, found </{close}>."), self.line);
                }
                self.skip_space();
                self.expect(">")?;
                return Ok(Raw { qname, attrs, children, text: content });
            }
            if self.starts_with("<!--") {
                self.advance(self.i + 4);
                self.skip_until("-->", "comment")?;
            } else if self.starts_with("<![CDATA[") {
                self.advance(self.i + 9);
                content.push_str(self.skip_until("]]>", "CDATA section")?);
            } else if self.starts_with("<?") {
                self.advance(self.i + 2);
                self.skip_until("?>", "processing instruction")?;
            } else if self.starts_with("<") {
                children.push(self.element()?);
            } else {
                let j = self.rest().find('<').map_or(self.text.len(), |j| self.i + j);
                let at = self.line;
                let chunk = &self.text[self.i..j];
                self.advance(j);
                content.push_str(&decode(chunk, at)?);
            }
        }
    }
}

/// Parses `text` and returns the document element.
/// `canonical` maps namespace URIs to the prefix used in the returned names.
pub fn parse_xml(text: &str, canonical: &HashMap<&str, &str>) -> Result<XmlElement, XmlError> {
    let mut p = Parser { text, i: 0, line: 1 };
    // Prolog
    loop {
        p.skip_space();
        if p.starts_with("<?") {
            p.advance(p.i + 2);
            p.skip_until("?>", "processing instruction")?;
        } else if p.starts_with("<!--") {
            p.advance(p.i + 4);
            p.skip_until("-->", "comment")?;
        } else if p.starts_with("<!DOCTYPE") {
            p.skip_until(">", "DOCTYPE")?;
        } else {
            break;
        }
    }
    if !p.starts_with("<") {
        return err("The document does not start with an element.", p.line);
    }
    let root = p.element()?;
    p.skip_space();
    while p.starts_with("<!--") {
        p.advance(p.i + 4);
        p.skip_until("-->", "comment")?;
        p.skip_space();
    }
    if p.i < text.len() {
        return err("Content after the document element.", p.line);
    }
    Ok(resolve(root, &HashMap::new(), canonical))
}

/// Namespace resolution.
fn resolve(raw: Raw, scope: &HashMap<String, String>, canonical: &HashMap<&str, &str>) -> XmlElement {
    let mut s = scope.clone();
    for (k, v) in &raw.attrs {
        if k == "xmlns" {
            s.insert(String::new(), v.clone());
        } else if let Some(prefix) = k.strip_prefix("xmlns:") {
            s.insert(prefix.to_string(), v.clone());
        }
    }
    // (name, ns, local)
    let qualify = |qname: &str, is_attr: bool| -> (String, String, String) {
        let (prefix, local) = match qname.find(':') {
            Some(c) => (&qname[..c], &qname[c + 1..]),
            None => ("", qname),
        };
        if is_attr && !qname.contains(':') {
            return (local.to_string(), String::new(), local.to_string());
        }
        let ns = s.get(prefix).cloned().unwrap_or_default();
        let p = canonical.get(ns.as_str()).copied().unwrap_or(prefix);
        let name = if p.is_empty() { local.to_string() } else { format!("{p}:{local}") };
        (name, ns, local.to_string())
    };
    let (name, ns, local) = qualify(&raw.qname, false);
    let mut el = XmlElement { name, ns, local, attrs: Vec::new(), children: Vec::new(), text: raw.text };
    for (k, v) in &raw.attrs {
        if k == "xmlns" || k.starts_with("xmlns:") {
            continue;
        }
        el.set_attr(qualify(k, true).0, v.clone());
    }
    // Qualified values (xmi:type="uml:State") follow the same prefix mapping.
    let typed: Vec<(String, String)> =
        el.attrs.iter().filter(|(k, _)| k.ends_with(":type")).map(|(k, v)| (k.clone(), qualify(v, false).0)).collect();
    for (k, v) in typed {
        el.set_attr(k, v);
    }
    el.children = raw.children.into_iter().map(|c| resolve(c, &s, canonical)).collect();
    el
}
