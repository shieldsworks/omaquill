//! A small XML tree that writes back exactly what it read.
//!
//! Scrivener's `.scrivx` holds far more than omaquill understands (section
//! types, compile settings, bookmarks, targets). Everything is kept as it
//! came in: text nodes keep their whitespace, attributes keep their order and
//! their escaped spelling, so an untouched file serializes byte for byte.

use std::fmt::Write as _;

#[cfg(test)]
thread_local! {
    static INSERT_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn note_insert_walk(n: u64) {
    #[cfg(test)]
    INSERT_WALKS.with(|visits| visits.set(visits.get().saturating_add(n)));
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(test)]
pub fn reset_insert_walks() {
    INSERT_WALKS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub fn insert_walks() -> u64 {
    INSERT_WALKS.with(|visits| visits.get())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Element),
    /// Character data, still escaped as it appeared in the file.
    Text(String),
    /// Anything else (`<?xml ...?>`, comments, CDATA, doctype), kept raw.
    Raw(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub name: String,
    /// Attribute values are stored escaped, exactly as written.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    /// `<Foo/>` rather than `<Foo></Foo>`.
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Nodes before the root element (declaration, comments, whitespace).
    pub prolog: Vec<Node>,
    pub root: Element,
    /// Whatever follows the root element, usually a newline.
    pub epilog: String,
    /// The file began with a byte-order mark.
    pub bom: bool,
}

#[derive(Debug)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "XML error at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for Error {}

/// Deepest element nesting accepted. A Scrivener binder nests a few dozen
/// levels at most; a hostile file nested far deeper would overflow the
/// stack of the code that walks the tree.
const MAX_DEPTH: usize = 512;

fn open_element(stack: &[Element]) -> &Element {
    stack
        .last()
        .expect("parser stack is non-empty until the root element returns")
}

fn open_element_mut(stack: &mut [Element]) -> &mut Element {
    stack
        .last_mut()
        .expect("parser stack is non-empty until the root element returns")
}

struct BangDecl {
    opens_subset: bool,
    gt: Option<usize>,
    examined: usize,
}

fn bang_decl(rest: &str) -> BangDecl {
    let Some(g) = rest.find('>') else {
        return BangDecl {
            opens_subset: false,
            gt: None,
            examined: rest.len(),
        };
    };
    let bracket = rest[..g].find('[');
    let bracket_walk = match bracket {
        Some(i) => i + 1,
        None => g,
    };
    BangDecl {
        opens_subset: bracket.is_some(),
        gt: Some(g),
        examined: g + 1 + bracket_walk,
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// Bytes walked by searches from the current position.
    scanned: u64,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, message: impl Into<String>) -> Result<T, Error> {
        Err(Error {
            offset: self.pos,
            message: message.into(),
        })
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn note(&mut self, n: usize) {
        self.scanned += u64::try_from(n).expect("a byte length fits in u64");
    }

    /// How far a search that stopped at `at` walked. `at == rest.len()`
    /// means the needle was absent and the whole suffix was walked.
    fn note_boundary(&mut self, at: usize) {
        let extra = if at < self.rest().len() {
            self.rest()[at..]
                .chars()
                .next()
                .expect("index from a str search is a char boundary")
                .len_utf8()
        } else {
            0
        };
        let examined = if extra == 0 {
            self.rest().len()
        } else {
            at + extra
        };
        self.note(examined);
    }

    fn find_char(&mut self, needle: char) -> Option<usize> {
        let rest_len = self.rest().len();
        let found = self.rest().find(needle);
        let read = match found {
            Some(i) => i + needle.len_utf8(),
            None => rest_len,
        };
        self.note(read);
        found
    }

    fn find_str(&mut self, needle: &str) -> Option<usize> {
        let rest_len = self.rest().len();
        let found = self.rest().find(needle);
        let read = match found {
            Some(i) => i + needle.len(),
            None => rest_len,
        };
        self.note(read);
        found
    }

    fn take_until(&mut self, end: &str) -> Result<&'a str, Error> {
        match self.find_str(end) {
            Some(i) => {
                let s = &self.src[self.pos..self.pos + i + end.len()];
                self.pos += i + end.len();
                Ok(s)
            }
            None => self.err(format!("unterminated, expected {end:?}")),
        }
    }

    /// Byte length of the leading whitespace. Counts the bytes the scan walks.
    fn leading_ws(&mut self) -> usize {
        let n = self.rest().len() - self.rest().trim_start().len();
        self.note_boundary(n);
        n
    }

    fn skip_ws(&mut self) {
        self.pos += self.leading_ws();
    }

    fn name(&mut self) -> Result<String, Error> {
        let n = self
            .rest()
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '='))
            .unwrap_or(self.rest().len());
        self.note_boundary(n);
        if n == 0 {
            return self.err("expected a name");
        }
        let name = self.rest()[..n].to_string();
        self.pos += n;
        Ok(name)
    }

    /// Parses `<name attrs>` or `<name attrs/>`; `self.pos` is on the `<`.
    fn start_tag(&mut self) -> Result<Element, Error> {
        self.pos += 1;
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.pos += 2;
                return Ok(Element {
                    name,
                    attrs,
                    children: Vec::new(),
                    empty: true,
                });
            }
            if rest.starts_with('>') {
                self.pos += 1;
                return Ok(Element {
                    name,
                    attrs,
                    children: Vec::new(),
                    empty: false,
                });
            }
            if rest.is_empty() {
                return self.err("unterminated start tag");
            }
            let key = self.name()?;
            self.skip_ws();
            if !self.rest().starts_with('=') {
                return self.err(format!("attribute {key} has no value"));
            }
            self.pos += 1;
            self.skip_ws();
            let quote = match self.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return self.err("attribute value is not quoted"),
            };
            self.pos += 1;
            let end = match self.find_char(quote) {
                Some(i) => i,
                None => return self.err("unterminated attribute value"),
            };
            let mut value = self.rest()[..end].to_string();
            if quote == '\'' {
                // Written back in double quotes, so escape any inside.
                value = value.replace('"', "&quot;");
            }
            attrs.push((key, value));
            self.pos += end + 1;
        }
    }

    fn misc(&mut self) -> Result<Option<Node>, Error> {
        let raw = if self.rest().starts_with("<?") {
            self.take_until("?>")?
        } else if self.rest().starts_with("<!--") {
            self.take_until("-->")?
        } else if self.rest().starts_with("<![CDATA[") {
            self.take_until("]]>")?
        } else if self.rest().starts_with("<!") {
            // A DOCTYPE's internal subset `[...]` can hold '>'.
            let bang = bang_decl(self.rest());
            self.note(bang.examined);
            if bang.opens_subset {
                let start = self.pos;
                self.take_until("]")?;
                self.take_until(">")?;
                &self.src[start..self.pos]
            } else if let Some(g) = bang.gt {
                let end = self.pos + g + 1;
                let raw = &self.src[self.pos..end];
                self.pos = end;
                raw
            } else {
                return self.err("unterminated, expected \">\"");
            }
        } else {
            return Ok(None);
        };
        Ok(Some(Node::Raw(raw.to_string())))
    }

    fn element(&mut self) -> Result<Element, Error> {
        let first = self.start_tag()?;
        if first.empty {
            return Ok(first);
        }
        let mut stack = vec![first];
        loop {
            if self.rest().is_empty() {
                let name = open_element(&stack).name.clone();
                return self.err(format!("<{name}> is never closed"));
            }
            if !self.rest().starts_with('<') {
                let (n, examined) = match self.rest().find('<') {
                    Some(i) => (i, i + 1),
                    None => {
                        let n = self.rest().len();
                        (n, n)
                    }
                };
                self.note(examined);
                let text = self.rest()[..n].to_string();
                open_element_mut(&mut stack).children.push(Node::Text(text));
                self.pos += n;
                continue;
            }
            if let Some(node) = self.misc()? {
                open_element_mut(&mut stack).children.push(node);
                continue;
            }
            if self.rest().starts_with("</") {
                self.pos += 2;
                let name = self.name()?;
                self.skip_ws();
                if !self.rest().starts_with('>') {
                    return self.err("bad end tag");
                }
                self.pos += 1;
                let done = stack
                    .pop()
                    .expect("parser stack is non-empty until the root element returns");
                if done.name != name {
                    return self.err(format!("</{name}> closes <{}>", done.name));
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(done)),
                    None => return Ok(done),
                }
                continue;
            }
            let el = self.start_tag()?;
            if el.empty {
                open_element_mut(&mut stack)
                    .children
                    .push(Node::Element(el));
            } else if stack.len() >= MAX_DEPTH {
                return self.err("elements nested too deeply");
            } else {
                stack.push(el);
            }
        }
    }
}

pub fn parse(src: &str) -> Result<Document, Error> {
    parse_scanned(src).0
}

fn parse_scanned(src: &str) -> (Result<Document, Error>, u64) {
    let bom = src.starts_with('\u{feff}');
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser {
        src,
        pos: 0,
        scanned: 0,
    };
    let result = p.finish(bom);
    (result, p.scanned)
}

impl<'a> Parser<'a> {
    fn finish(&mut self, bom: bool) -> Result<Document, Error> {
        let mut prolog = Vec::new();
        loop {
            let ws = self.leading_ws();
            if ws > 0 {
                prolog.push(Node::Text(self.rest()[..ws].to_string()));
                self.pos += ws;
            }
            match self.misc()? {
                Some(node) => prolog.push(node),
                None => break,
            }
        }
        if !self.rest().starts_with('<') {
            return self.err("no root element");
        }
        let root = self.element()?;
        let epilog = self.rest().to_string();
        // Only whitespace, comments and processing instructions may follow.
        loop {
            self.skip_ws();
            if self.rest().is_empty() {
                break;
            }
            if self.rest().starts_with("<!--") || self.rest().starts_with("<?") {
                self.misc()?;
            } else {
                return self.err("content after the root element");
            }
        }
        Ok(Document {
            prolog,
            root,
            epilog,
            bom,
        })
    }
}

impl Document {
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        for node in &self.prolog {
            write_node(&mut out, node);
        }
        write_element(&mut out, &self.root);
        out.push_str(&self.epilog);
        out
    }
}

fn write_node(out: &mut String, node: &Node) {
    match node {
        Node::Element(el) => write_element(out, el),
        Node::Text(s) | Node::Raw(s) => out.push_str(s),
    }
}

fn write_element(out: &mut String, el: &Element) {
    out.push('<');
    out.push_str(&el.name);
    for (k, v) in &el.attrs {
        let _ = write!(out, " {k}=\"{v}\"");
    }
    if el.empty && el.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &el.children {
        write_node(out, child);
    }
    out.push_str("</");
    out.push_str(&el.name);
    out.push('>');
}

/// Where the line break at `nl` (a '\n') starts: back one for "\r\n".
fn line_start(ws: &str, nl: usize) -> usize {
    if nl > 0 && ws.as_bytes()[nl - 1] == b'\r' {
        nl - 1
    } else {
        nl
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML 1.0 forbids most control characters outright.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    unescape_scanned(s).0
}

fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let num = if let Some(hex) = entity.strip_prefix("#x") {
                u32::from_str_radix(hex, 16).ok()
            } else if let Some(dec) = entity.strip_prefix('#') {
                dec.parse().ok()
            } else {
                None
            };
            num.and_then(char::from_u32)
        }
    }
}

fn count_bytes(n: usize, scanned: &mut u64) {
    *scanned += u64::try_from(n).expect("a byte length fits in u64");
}

fn semicolon_from(
    bytes: &[u8],
    from: usize,
    cached: &mut Option<(usize, usize)>,
    scanned: &mut u64,
) -> Option<usize> {
    if let Some((start, at)) = *cached
        && from >= start
        && from <= at
    {
        return (at < bytes.len()).then_some(at);
    }
    let (at, read) = match bytes[from..].iter().position(|&b| b == b';') {
        Some(rel) => (from + rel, rel + 1),
        None => (bytes.len(), bytes.len() - from),
    };
    count_bytes(read, scanned);
    *cached = Some((from, at));
    (at < bytes.len()).then_some(at)
}

fn unescape_scanned(s: &str) -> (String, u64) {
    let bytes = s.as_bytes();
    if !bytes.contains(&b'&') {
        return (
            s.to_string(),
            u64::try_from(bytes.len()).expect("a byte length fits in u64"),
        );
    }
    let mut out = String::with_capacity(s.len());
    let mut scanned = 0u64;
    let mut i = 0usize;
    let mut cached: Option<(usize, usize)> = None;
    while i < bytes.len() {
        let copied = match bytes[i..].iter().position(|&b| b == b'&') {
            Some(rel) => i + rel,
            None => bytes.len(),
        };
        if copied > i {
            count_bytes(copied - i, &mut scanned);
            out.push_str(&s[i..copied]);
        }
        if copied == bytes.len() {
            break;
        }
        let Some(end) = semicolon_from(bytes, copied, &mut cached, &mut scanned) else {
            out.push_str(&s[copied..]);
            break;
        };
        if let Some(ch) = entity_char(&s[copied + 1..end]) {
            out.push(ch);
            i = end + 1;
        } else {
            out.push('&');
            i = copied + 1;
        }
    }
    (out, scanned)
}

impl Element {
    pub fn new(name: &str) -> Element {
        Element {
            name: name.to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
            empty: false,
        }
    }

    pub fn attr(&self, key: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| unescape(v))
    }

    pub fn set_attr(&mut self, key: &str, value: &str) {
        let value = escape(value);
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.attrs.push((key.to_string(), value)),
        }
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn elements_mut(&mut self) -> impl Iterator<Item = &mut Element> {
        self.children.iter_mut().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    pub fn child_mut(&mut self, name: &str) -> Option<&mut Element> {
        self.elements_mut().find(|e| e.name == name)
    }

    /// The unescaped text content, CDATA included.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for node in &self.children {
            match node {
                Node::Text(s) => out.push_str(&unescape(s)),
                Node::Raw(s) => {
                    if let Some(body) = s
                        .strip_prefix("<![CDATA[")
                        .and_then(|b| b.strip_suffix("]]>"))
                    {
                        out.push_str(body);
                    }
                }
                Node::Element(e) => out.push_str(&e.text()),
            }
        }
        out
    }

    pub fn child_text(&self, name: &str) -> Option<String> {
        self.child(name).map(Element::text)
    }

    pub fn set_text(&mut self, text: &str) {
        self.children = vec![Node::Text(escape(text))];
        self.empty = false;
    }

    /// The indentation this element's children use, guessed from the
    /// whitespace already there. Scrivener indents with four spaces.
    fn child_indent(&self, own: &str) -> String {
        for (i, node) in self.children.iter().enumerate() {
            if let (Node::Text(ws), Some(Node::Element(_))) = (node, self.children.get(i + 1))
                && let Some(nl) = ws.rfind('\n')
            {
                return ws[line_start(ws, nl)..].to_string();
            }
        }
        format!("{own}    ")
    }

    /// Where this element's closing tag sits: the whitespace before it.
    fn closing_indent(&self, own: &str) -> String {
        if let Some(Node::Text(ws)) = self.children.last()
            && ws.trim().is_empty()
            && let Some(nl) = ws.rfind('\n')
        {
            ws[line_start(ws, nl)..].to_string()
        } else {
            own.to_string()
        }
    }

    /// Inserts `el` as the `index`th child element, indented to match its
    /// siblings. `own` is this element's own indentation, `"\n    "` style.
    pub fn insert_element(&mut self, index: usize, el: Element, own: &str) {
        let indent = self.child_indent(own);
        let close = self.closing_indent(own);
        let has_elements = self.elements().next().is_some();
        if !has_elements {
            // Only whitespace (or nothing) inside: lay it out fresh,
            // keeping any comments.
            let mut fresh = Vec::new();
            for n in std::mem::take(&mut self.children) {
                note_insert_walk(1);
                if let Node::Raw(_) = n {
                    fresh.push(Node::Text(indent.clone()));
                    fresh.push(n);
                }
            }
            fresh.push(Node::Text(indent));
            fresh.push(Node::Element(el));
            fresh.push(Node::Text(close));
            self.children = fresh;
            self.empty = false;
            return;
        }
        // Position in `children` of the index-th element.
        let mut seen = 0;
        let mut at = None;
        for (i, node) in self.children.iter().enumerate() {
            note_insert_walk(1);
            if let Node::Element(_) = node {
                if seen == index {
                    at = Some(i);
                    break;
                }
                seen += 1;
            }
        }
        match at {
            Some(i) => {
                // Before an element: [ws] NEW [ws] OLD. The whitespace before
                // `i` stays in front of NEW; add fresh whitespace after it.
                self.children.insert(i, Node::Text(indent));
                self.children.insert(i, Node::Element(el));
            }
            None => {
                // After the last element, before the closing whitespace.
                let last = self
                    .children
                    .iter()
                    .rposition(|n| matches!(n, Node::Element(_)))
                    .expect("this element has a child element");
                self.children.insert(last + 1, Node::Element(el));
                self.children.insert(last + 1, Node::Text(indent));
            }
        }
    }

    /// Appends every element after the children already here. The existing
    /// children are examined once. Inserting one child at a time walks the
    /// list again for each element.
    pub fn append_elements(&mut self, entries: Vec<Element>, own: &str) {
        if entries.is_empty() {
            return;
        }
        let indent = self.child_indent(own);
        let close = self.closing_indent(own);
        for _ in &self.children {
            note_insert_walk(1);
        }
        let has_elements = self.elements().next().is_some();
        if !has_elements {
            let mut fresh = Vec::new();
            for n in std::mem::take(&mut self.children) {
                if let Node::Raw(_) = n {
                    fresh.push(Node::Text(indent.clone()));
                    fresh.push(n);
                }
            }
            for entry in entries {
                note_insert_walk(1);
                fresh.push(Node::Text(indent.clone()));
                fresh.push(Node::Element(entry));
            }
            fresh.push(Node::Text(close));
            self.children = fresh;
            self.empty = false;
            return;
        }
        let closing = match self.children.last() {
            Some(Node::Text(ws)) if ws.trim().is_empty() => self.children.pop(),
            _ => None,
        };
        for entry in entries {
            note_insert_walk(1);
            self.children.push(Node::Text(indent.clone()));
            self.children.push(Node::Element(entry));
        }
        self.children.push(closing.unwrap_or(Node::Text(close)));
    }

    /// Removes the `index`th child element with the whitespace before it.
    pub fn remove_element(&mut self, index: usize) -> Option<Element> {
        let mut seen = 0;
        let pos = self.children.iter().position(|n| {
            if let Node::Element(_) = n {
                seen += 1;
                seen == index + 1
            } else {
                false
            }
        })?;
        let Node::Element(el) = self.children.remove(pos) else {
            unreachable!()
        };
        if pos > 0 && matches!(&self.children[pos - 1], Node::Text(t) if t.trim().is_empty()) {
            self.children.remove(pos - 1);
        }
        if self.elements().next().is_none() {
            // Nothing left but whitespace: collapse to `<Children></Children>`.
            // Keep comments and the like; drop only the whitespace.
            self.children
                .retain(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()));
        }
        Some(el)
    }

    /// Sets (or creates) `<name>text</name>` among the children.
    pub fn set_child_text(&mut self, name: &str, text: &str, own: &str) {
        if let Some(c) = self.child_mut(name) {
            c.set_text(text);
            return;
        }
        let mut el = Element::new(name);
        el.set_text(text);
        let n = self.elements().count();
        self.insert_element(n, el, own);
    }

    pub fn remove_child(&mut self, name: &str) -> Option<Element> {
        let i = self.elements().position(|e| e.name == name)?;
        self.remove_element(i)
    }
}

/// Lays out an element (built in code, or moved to a new depth) the way
/// Scrivener does: one child
/// per line, four spaces per level. `indent` is the element's own
/// indentation, as `"\n        "`.
pub fn pretty(el: &mut Element, indent: &str) {
    let has_elements = el.elements().next().is_some();
    if !has_elements {
        return;
    }
    let inner = format!("{indent}    ");
    let mut children = Vec::new();
    for node in std::mem::take(&mut el.children) {
        match node {
            Node::Element(mut child) => {
                pretty(&mut child, &inner);
                children.push(Node::Text(inner.clone()));
                children.push(Node::Element(child));
            }
            Node::Raw(raw) => {
                children.push(Node::Text(inner.clone()));
                children.push(Node::Raw(raw));
            }
            // Whitespace between elements is what's being replaced.
            Node::Text(_) => {}
        }
    }
    children.push(Node::Text(indent.to_string()));
    el.children = children;
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Root A="1" B="x &amp; y">
    <Item UUID="1">
        <Title>Fish &amp; Chips</Title>
        <Empty/>
    </Item>
    <Style><![CDATA[{\rtf1 <$Scr_H::1>}]]></Style>
    <!-- note -->
</Root>
"#;

    #[test]
    fn round_trips_byte_for_byte() {
        let doc = parse(SAMPLE).unwrap();
        assert_eq!(doc.to_xml(), SAMPLE);
    }

    #[test]
    fn reads_text_and_attributes() {
        let doc = parse(SAMPLE).unwrap();
        assert_eq!(doc.root.attr("B").as_deref(), Some("x & y"));
        let item = doc.root.child("Item").unwrap();
        assert_eq!(item.child_text("Title").as_deref(), Some("Fish & Chips"));
        assert_eq!(
            doc.root.child("Style").unwrap().text(),
            r"{\rtf1 <$Scr_H::1>}"
        );
    }

    #[test]
    fn inserts_with_matching_indentation() {
        let mut doc = parse(SAMPLE).unwrap();
        let item = doc.root.child_mut("Item").unwrap();
        item.set_child_text("Synopsis", "a <b>", "\n    ");
        let xml = doc.to_xml();
        assert!(
            xml.contains("        <Empty/>\n        <Synopsis>a &lt;b&gt;</Synopsis>\n    </Item>")
        );
        let item = doc.root.child_mut("Item").unwrap();
        item.remove_child("Synopsis");
        assert_eq!(doc.to_xml(), SAMPLE);
    }

    #[test]
    fn append_elements_walks_the_list_once() {
        let mut parent = Element::new("Docs");
        parent.append_elements(vec![Element::new("Existing")], "\n    ");
        let n = 80usize;
        let entries: Vec<Element> = (0..n).map(|_| Element::new("Document")).collect();
        reset_insert_walks();
        parent.append_elements(entries, "\n    ");
        let walks = insert_walks();
        let bound = u64::try_from(n).unwrap().saturating_mul(4);
        assert!(
            walks >= u64::try_from(n).unwrap(),
            "appending {n} elements recorded {walks} steps"
        );
        assert!(
            walks <= bound,
            "appending {n} elements walked {walks} element-list steps"
        );
        assert_eq!(parent.elements().count(), n + 1);
    }

    #[test]
    fn inserts_into_empty_parent() {
        let mut doc = parse("<A>\n    <Children></Children>\n</A>").unwrap();
        let children = doc.root.child_mut("Children").unwrap();
        children.insert_element(0, Element::new("X"), "\n    ");
        assert_eq!(
            doc.to_xml(),
            "<A>\n    <Children>\n        <X></X>\n    </Children>\n</A>"
        );
        let children = doc.root.child_mut("Children").unwrap();
        children.insert_element(0, Element::new("Y"), "\n    ");
        assert_eq!(
            doc.to_xml(),
            "<A>\n    <Children>\n        <Y></Y>\n        <X></X>\n    </Children>\n</A>"
        );
        let children = doc.root.child_mut("Children").unwrap();
        children.remove_element(0);
        children.remove_element(0);
        assert_eq!(doc.to_xml(), "<A>\n    <Children></Children>\n</A>");
    }

    #[test]
    fn unescapes_numeric_entities() {
        assert_eq!(unescape("&#233;t&#xE9; &bogus; &"), "été &bogus; &");
    }

    #[test]
    fn decodes_named_decimal_and_hex_entities() {
        let cases = [
            ("Fish &amp; Chips", "Fish & Chips"),
            ("&lt;&gt;&quot;&apos;", "<>\"'"),
            ("&#233;", "é"),
            ("&#00065;", "A"),
            ("&#xE9;", "é"),
            ("&#xe9;", "é"),
            ("&#x4A;", "J"),
            ("&#x000041;", "A"),
            ("&#X4A;", "&#X4A;"),
            ("&#x10FFFF;", "\u{10FFFF}"),
            ("&#x110000;", "&#x110000;"),
            ("&#xD800;", "&#xD800;"),
            ("&#x0;", "\0"),
            ("&#0;", "\0"),
            ("&#4294967296;", "&#4294967296;"),
            ("&#4294967295;", "&#4294967295;"),
            ("&;", "&;"),
            ("&#x;", "&#x;"),
            ("&#;", "&#;"),
            ("&bogus;", "&bogus;"),
            ("&amp", "&amp"),
            ("&", "&"),
            ("&&amp;", "&&"),
            ("&&&&&amp;", "&&&&&"),
            ("&bogus&amp;", "&bogus&"),
            ("&amp&amp;", "&amp&"),
            ("&#38;amp;", "&amp;"),
            ("&#x26;lt;", "&lt;"),
            ("&#233;t&#xE9; &bogus; &", "été &bogus; &"),
            ("plain", "plain"),
            ("a &amp; b &lt; c", "a & b < c"),
        ];
        for (input, expected) in cases {
            assert_eq!(unescape(input), expected, "{input:?}");
        }
    }

    #[test]
    fn ampersand_run_before_one_semicolon_reads_linearly() {
        let title = format!("{};", "&".repeat(200_000));
        let (text, bytes_read) = unescape_scanned(&title);
        assert_eq!(text, title);
        let len = u64::try_from(title.len()).unwrap();
        assert!(bytes_read >= len, "read {bytes_read} of {len}");
        let limit = len * 3;
        assert!(
            bytes_read <= limit,
            "read {bytes_read} bytes of a {len}-byte title, limit {limit}"
        );

        let xml = format!("<BinderItem Type=\"Text\"><Title>{title}</Title></BinderItem>");
        let doc = parse(&xml).unwrap();
        assert_eq!(
            doc.root.child_text("Title").as_deref(),
            Some(title.as_str())
        );
    }

    #[test]
    fn keeps_bom_doctype_and_trailing_comments() {
        let src = "\u{feff}<?xml version=\"1.0\"?>\n<!DOCTYPE a [<!ENTITY x \"y\">]>\n<a/>\n<!-- end -->\n";
        let doc = parse(src).unwrap();
        assert_eq!(doc.to_xml(), src);
    }

    #[test]
    fn single_quoted_attributes_stay_valid() {
        let doc = parse("<a b='say \"hi\"'/>").unwrap();
        assert_eq!(doc.root.attr("b").as_deref(), Some("say \"hi\""));
        assert!(parse(&doc.to_xml()).is_ok());
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let mut doc = parse("<A>\r\n    <B>\r\n        <x/>\r\n    </B>\r\n</A>").unwrap();
        let b = doc.root.child_mut("B").unwrap();
        b.insert_element(1, Element::new("y"), "\r\n    ");
        let xml = doc.to_xml();
        assert!(!xml.replace("\r\n", "").contains('\n'), "{xml:?}");
    }

    #[test]
    fn emptying_keeps_comments() {
        let mut doc = parse("<A>\n    <!-- keep -->\n    <x/>\n</A>").unwrap();
        doc.root.remove_element(0);
        assert!(doc.to_xml().contains("<!-- keep -->"));
        doc.root.insert_element(0, Element::new("y"), "\n");
        assert!(doc.to_xml().contains("<!-- keep -->"));
        assert!(doc.to_xml().contains("<y></y>"));
    }

    #[test]
    fn rejects_absurd_nesting() {
        let deep = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
        assert!(parse(&deep).is_err());
        let fine = format!("{}{}", "<a>".repeat(100), "</a>".repeat(100));
        assert!(parse(&fine).is_ok());
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse("<a><b></a></b>").is_err());
    }

    #[test]
    fn unclosed_element_names_the_open_tag() {
        let err = parse("<chapter><scene>text").unwrap_err();
        assert_eq!(err.message, "<scene> is never closed");
    }

    fn bang_declarations(n: usize) -> (u64, u64) {
        let mut xml = "<!x>".repeat(n);
        xml.push_str("<r/>");
        let (doc, scanned) = parse_scanned(&xml);
        assert_eq!(doc.unwrap().to_xml(), xml);
        (scanned, u64::try_from(xml.len()).unwrap())
    }

    /// Many short `<!x>` declarations and no `[` anywhere. Each declaration
    /// used to search the whole remaining suffix for that bracket.
    #[test]
    fn bang_declarations_without_a_bracket_scan_linearly() {
        let (small, _) = bang_declarations(1_000);
        let (large, large_len) = bang_declarations(10_000);
        let growth = large / small.max(1);
        assert!(
            large < small.saturating_mul(30),
            "10x declarations walked {large} bytes after {small} ({growth}x), limit 30x"
        );
        let limit = large_len * 8;
        assert!(
            large <= limit,
            "walked {large} bytes of a {large_len}-byte file, limit {limit}"
        );
    }

    fn assert_linear_parse(label: &str, small: &str, large: &str) {
        let cost = |xml: &str| {
            let (doc, scanned) = parse_scanned(xml);
            assert_eq!(doc.unwrap().to_xml(), xml, "{label}");
            (scanned, u64::try_from(xml.len()).unwrap())
        };
        let (small_n, _) = cost(small);
        let (large_n, large_len) = cost(large);
        let growth = large_n / small_n.max(1);
        assert!(
            large_n < small_n.saturating_mul(30),
            "{label}: 10x input walked {large_n} bytes after {small_n} ({growth}x), limit 30x"
        );
        let limit = large_len * 8;
        assert!(
            large_n <= limit,
            "{label}: walked {large_n} bytes of a {large_len}-byte file, limit {limit}"
        );
    }

    fn repeat_before_root(token: &str, n: usize) -> String {
        let mut xml = token.repeat(n);
        xml.push_str("<r/>");
        xml
    }

    #[test]
    fn prolog_constructs_scan_linearly() {
        assert_linear_parse(
            "doctype",
            &repeat_before_root("<!DOCTYPE a>", 1_000),
            &repeat_before_root("<!DOCTYPE a>", 10_000),
        );
        assert_linear_parse(
            "doctype subset",
            &repeat_before_root("<!DOCTYPE a [x]>", 1_000),
            &repeat_before_root("<!DOCTYPE a [x]>", 10_000),
        );
        assert_linear_parse(
            "comment",
            &repeat_before_root("<!--x-->", 1_000),
            &repeat_before_root("<!--x-->", 10_000),
        );
        assert_linear_parse(
            "cdata",
            &repeat_before_root("<![CDATA[x]]>", 1_000),
            &repeat_before_root("<![CDATA[x]]>", 10_000),
        );
        assert_linear_parse(
            "pi",
            &repeat_before_root("<?x?>", 1_000),
            &repeat_before_root("<?x?>", 10_000),
        );
        let elements = |n: usize| {
            let mut xml = String::from("<r>");
            xml.push_str(&"<a/>".repeat(n));
            xml.push_str("</r>");
            xml
        };
        assert_linear_parse("empty elements", &elements(1_000), &elements(10_000));
        assert_linear_parse(
            "text",
            &format!("<r>{}</r>", "x".repeat(1_000)),
            &format!("<r>{}</r>", "x".repeat(10_000)),
        );
        assert_linear_parse(
            "entities",
            &format!("<r>{}</r>", "&amp;".repeat(1_000)),
            &format!("<r>{}</r>", "&amp;".repeat(10_000)),
        );
    }

    #[test]
    fn scrivx_shapes_round_trip_byte_for_byte() {
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Lighthouse.scriv/Lighthouse.scrivx"
        ))
        .unwrap();
        assert_eq!(parse(&fixture).unwrap().to_xml(), fixture);

        let shapes = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE ScrivenerProject [<!ENTITY e \"quoted &amp; text\">]>\n\
<!-- keep -->\n\
<?omaquill note?>\n\
<ScrivenerProject>\n\
    <Title>Fish &amp; Chips</Title>\n\
    <Style><![CDATA[{\\rtf1 hi}]]></Style>\n\
    <Note>a &lt; b &gt; c &quot;d&quot; &apos;e&apos;</Note>\n\
</ScrivenerProject>\n";
        let doc = parse(shapes).unwrap();
        assert_eq!(doc.to_xml(), shapes);
        assert_eq!(
            parse("<!DOCTYPE").unwrap_err().message,
            "unterminated, expected \">\""
        );
        assert_eq!(
            doc.root.child_text("Title").as_deref(),
            Some("Fish & Chips")
        );
        assert_eq!(
            doc.root.child_text("Note").as_deref(),
            Some("a < b > c \"d\" 'e'")
        );
        assert_eq!(doc.root.child_text("Style").as_deref(), Some("{\\rtf1 hi}"));
    }

    fn legacy_bracket_before_gt(rest: &str) -> bool {
        matches!(
            (rest.find('['), rest.find('>')),
            (Some(b), Some(g)) if b < g
        )
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn bang_decl_matches_the_old_bracket_search() {
        let mut state = 0xA5A5_1234_F00D_7788u64;
        for _ in 0..5_000 {
            let len = (xorshift(&mut state) as usize) % 48;
            let mut body = String::new();
            for _ in 0..len {
                let b = (xorshift(&mut state) % 95) as u8 + 32;
                body.push(b as char);
            }
            if xorshift(&mut state).is_multiple_of(3) {
                body.push('[');
            }
            if xorshift(&mut state).is_multiple_of(3) {
                body.push('>');
            }
            if xorshift(&mut state).is_multiple_of(5) {
                body.push('<');
            }
            let src = format!("<!{body}");
            assert_eq!(
                bang_decl(&src).opens_subset,
                legacy_bracket_before_gt(&src),
                "{src:?}"
            );
            let doc_src = format!("{src}<r/>");
            assert_eq!(
                bang_decl(&doc_src).opens_subset,
                legacy_bracket_before_gt(&doc_src),
                "{doc_src:?}"
            );
            if let Ok(doc) = parse(&doc_src) {
                assert_eq!(doc.to_xml(), doc_src, "{doc_src:?}");
            }
        }

        let mut state = 0x0F1E_2D3C_4B5A_6978u64;
        for _ in 0..2_000 {
            let mut src = String::from("<?xml version=\"1.0\"?>");
            let pieces = (xorshift(&mut state) % 6) as usize;
            for _ in 0..pieces {
                let piece = match xorshift(&mut state) % 6 {
                    0 => "<!--x-->",
                    1 => "<![CDATA[z]]>",
                    2 => "<?pi?>",
                    3 => "<!x>",
                    4 => "<!DOCTYPE a [b]>",
                    _ => "<!DOCTYPE a>",
                };
                src.push_str(piece);
            }
            src.push_str("<r>");
            let texts = (xorshift(&mut state) % 8) as usize;
            for _ in 0..texts {
                let piece = match xorshift(&mut state) % 4 {
                    0 => "Fish &amp; Chips",
                    1 => "a&lt;b",
                    2 => "x",
                    _ => "<e/>",
                };
                src.push_str(piece);
            }
            src.push_str("</r>");
            assert_eq!(parse(&src).unwrap().to_xml(), src);
        }
    }

    #[test]
    #[ignore = "one million tokens of each construct"]
    fn million_token_constructs_stay_linear() {
        let n = 1_000_000;
        let cases = [
            ("bang", {
                let mut xml = "<!x>".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("doctype", {
                let mut xml = "<!DOCTYPE a>".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("doctype subset", {
                let mut xml = "<!DOCTYPE a [x]>".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("comment", {
                let mut xml = "<!--x-->".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("cdata", {
                let mut xml = "<![CDATA[x]]>".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("pi", {
                let mut xml = "<?x?>".repeat(n);
                xml.push_str("<r/>");
                xml
            }),
            ("elements", format!("<r>{}</r>", "<a/>".repeat(n))),
            ("text", format!("<r>{}</r>", "x".repeat(n))),
            ("entities", format!("<r>{}</r>", "&amp;".repeat(n))),
        ];
        for (label, xml) in cases {
            let len = u64::try_from(xml.len()).unwrap();
            let started = std::time::Instant::now();
            let (doc, scanned) = parse_scanned(&xml);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(doc.unwrap().to_xml(), xml, "{label}");
            let limit = len * 8;
            assert!(
                scanned <= limit,
                "{label} walked {scanned} of {len}, limit {limit}"
            );
            eprintln!("{label} len={len} scanned={scanned} ms={ms:.1}");
        }
    }
}
