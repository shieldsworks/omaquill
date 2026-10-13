//! A small XML tree that writes back exactly what it read.
//!
//! Scrivener's `.scrivx` holds far more than omaquill understands (section
//! types, compile settings, bookmarks, targets). Everything is kept as it
//! came in: text nodes keep their whitespace, attributes keep their order and
//! their escaped spelling, so an untouched file serializes byte for byte.

use std::fmt::Write as _;

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
            let b = self.find_char('[');
            let g = self.find_char('>');
            match (b, g) {
                (Some(b), Some(g)) if b < g => {
                    let start = self.pos;
                    self.take_until("]")?;
                    self.take_until(">")?;
                    &self.src[start..self.pos]
                }
                _ => self.take_until(">")?,
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
}
