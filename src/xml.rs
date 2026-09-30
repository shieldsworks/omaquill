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

struct Parser<'a> {
    src: &'a str,
    pos: usize,
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

    fn take_until(&mut self, end: &str) -> Result<&'a str, Error> {
        match self.rest().find(end) {
            Some(i) => {
                let s = &self.src[self.pos..self.pos + i + end.len()];
                self.pos += i + end.len();
                Ok(s)
            }
            None => self.err(format!("unterminated, expected {end:?}")),
        }
    }

    fn skip_ws(&mut self) {
        let n = self.rest().len() - self.rest().trim_start().len();
        self.pos += n;
    }

    fn name(&mut self) -> Result<String, Error> {
        let rest = self.rest();
        let n = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '='))
            .unwrap_or(rest.len());
        if n == 0 {
            return self.err("expected a name");
        }
        self.pos += n;
        Ok(rest[..n].to_string())
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
            let end = match self.rest().find(quote) {
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
        let rest = self.rest();
        let raw = if rest.starts_with("<?") {
            self.take_until("?>")?
        } else if rest.starts_with("<!--") {
            self.take_until("-->")?
        } else if rest.starts_with("<![CDATA[") {
            self.take_until("]]>")?
        } else if rest.starts_with("<!") {
            // A DOCTYPE's internal subset `[...]` can hold '>'.
            match (rest.find('['), rest.find('>')) {
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
        let mut stack = vec![self.start_tag()?];
        if stack[0].empty {
            return Ok(stack.pop().unwrap());
        }
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                return self.err(format!("<{}> is never closed", stack.last().unwrap().name));
            }
            if !rest.starts_with('<') {
                let n = rest.find('<').unwrap_or(rest.len());
                stack
                    .last_mut()
                    .unwrap()
                    .children
                    .push(Node::Text(rest[..n].to_string()));
                self.pos += n;
                continue;
            }
            if let Some(node) = self.misc()? {
                stack.last_mut().unwrap().children.push(node);
                continue;
            }
            if rest.starts_with("</") {
                self.pos += 2;
                let name = self.name()?;
                self.skip_ws();
                if !self.rest().starts_with('>') {
                    return self.err("bad end tag");
                }
                self.pos += 1;
                let done = stack.pop().unwrap();
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
                stack.last_mut().unwrap().children.push(Node::Element(el));
            } else if stack.len() >= MAX_DEPTH {
                // Walking a tree this deep later would overflow the stack.
                return self.err("elements nested too deeply");
            } else {
                stack.push(el);
            }
        }
    }
}

pub fn parse(src: &str) -> Result<Document, Error> {
    let bom = src.starts_with('\u{feff}');
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser { src, pos: 0 };
    let mut prolog = Vec::new();
    loop {
        let rest = p.rest();
        let ws = rest.len() - rest.trim_start().len();
        if ws > 0 {
            prolog.push(Node::Text(rest[..ws].to_string()));
            p.pos += ws;
        }
        match p.misc()? {
            Some(node) => prolog.push(node),
            None => break,
        }
    }
    if !p.rest().starts_with('<') {
        return p.err("no root element");
    }
    let root = p.element()?;
    let epilog = p.rest().to_string();
    // Only whitespace, comments and processing instructions may follow.
    loop {
        p.skip_ws();
        if p.rest().is_empty() {
            break;
        }
        if p.rest().starts_with("<!--") || p.rest().starts_with("<?") {
            p.misc()?;
        } else {
            return p.err("content after the root element");
        }
    }
    Ok(Document {
        prolog,
        root,
        epilog,
        bom,
    })
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
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let c = match entity {
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
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
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
        match self.children.last() {
            Some(Node::Text(ws)) if ws.trim().is_empty() && ws.contains('\n') => {
                ws[line_start(ws, ws.rfind('\n').unwrap())..].to_string()
            }
            _ => own.to_string(),
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
                    .unwrap();
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
}
