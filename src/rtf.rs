//! RTF as Scrivener writes it (Cocoa's dialect), read into [`RichText`]
//! and written back out.
//!
//! The reader understands the formatting a manuscript uses: fonts, sizes,
//! bold, italic, underline, strikethrough, color, highlight, super/subscript,
//! alignment, indents, spacing and line height. Lists come through as their
//! marker text (Cocoa writes it in `\listtext`), and table cells become
//! tab-separated paragraphs. Anything dropped on the way in is named in
//! [`RichText::lossy`], so the app can warn before rewriting such a file.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RichText {
    pub paragraphs: Vec<Paragraph>,
    /// Features the file had that won't survive a save, e.g. "images".
    pub lossy: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Paragraph {
    pub style: ParaStyle,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: CharStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Natural,
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LineSpacing {
    #[default]
    Single,
    /// A multiple of single spacing (`\slN\slmult1`, N/240).
    Multiple(f32),
    /// At least this many twips.
    AtLeast(i32),
    /// Exactly this many twips.
    Exactly(i32),
}

/// Paragraph formatting. Lengths are in twips (1/20 of a point).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParaStyle {
    pub align: Align,
    pub first_indent: i32,
    pub left_indent: i32,
    pub right_indent: i32,
    pub space_before: i32,
    pub space_after: i32,
    pub line_spacing: LineSpacing,
    /// Tab stops as (position, kind): 'l' left, 'r' right, 'c' center,
    /// 'd' decimal. Empty: the reader's default stops.
    pub tabs: Vec<(i32, char)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Script {
    #[default]
    Normal,
    Super,
    Sub,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CharStyle {
    /// Family name as a person would write it: "Palatino", "Times New Roman".
    pub font: Option<String>,
    /// Size in half-points, as RTF counts it (24 = 12 pt).
    pub size: u16,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<Rgb>,
    pub highlight: Option<Rgb>,
    pub script: Script,
    /// The target when this text is a link: a web address, a link to
    /// another document (`scrivlnk://`), or the anchor of a Scrivener
    /// comment or footnote (`scrivcmt://`), whose text lives in the
    /// document's `content.comments`.
    pub link: Option<String>,
}

pub type Rgb = (u8, u8, u8);

impl Default for CharStyle {
    fn default() -> Self {
        CharStyle {
            font: None,
            size: 24,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: None,
            highlight: None,
            script: Script::Normal,
            link: None,
        }
    }
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    fn push(&mut self, text: &str, style: &CharStyle) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(run) if run.style == *style => run.text.push_str(text),
            _ => self.runs.push(Run {
                text: text.to_string(),
                style: style.clone(),
            }),
        }
    }
}

impl RichText {
    pub fn from_plain(text: &str) -> RichText {
        RichText {
            paragraphs: text
                .split('\n')
                .map(|line| {
                    let mut p = Paragraph::default();
                    p.push(line, &CharStyle::default());
                    p
                })
                .collect(),
            lossy: Vec::new(),
        }
    }

    /// The text with paragraphs joined by newlines.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for (i, p) in self.paragraphs.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for r in &p.runs {
                out.push_str(&r.text);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.paragraphs
            .iter()
            .all(|p| p.runs.iter().all(|r| r.text.is_empty()))
    }
}

/// Words the way a writer counts them: runs of letters or digits, with
/// apostrophes and hyphens inside a word ("don't", "well-known") not
/// splitting it.
pub fn word_count(text: &str) -> usize {
    text.split(|c: char| c.is_whitespace() || c == '\u{2014}' || c == '\u{2013}')
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

// ---------------------------------------------------------------- reading

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    Text,
    Skip,
    FontTable,
    ColorTable,
    /// `\listtext`: the bullet or number Cocoa writes before list items.
    ListText,
    /// A field's instruction, e.g. `HYPERLINK "https://..."`.
    FldInst,
}

#[derive(Clone)]
struct State {
    ch: CharStyle,
    para: ParaStyle,
    dest: Dest,
    /// Characters to skip after `\uN`.
    uc: usize,
    /// `\sl` seen; `\slmult` decides what it means.
    sl: i32,
    /// Bold/italic that came from a face name ("Palatino-Italic") rather
    /// than `\b`/`\i`, so switching back to a plain face turns it off.
    face_bold: bool,
    face_italic: bool,
    /// `\tqr` and friends: the kind of the next `\tx`.
    tab_kind: char,
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    stack: Vec<State>,
    st: State,
    fonts: Vec<(i32, String)>,
    font_name: String,
    colors: Vec<Option<Rgb>>,
    color: (u8, u8, u8, bool),
    codepage: u32,
    out: RichText,
    current: Paragraph,
    pending: String,
    /// Chars still to skip after a `\u`.
    skip: usize,
    /// A high surrogate waiting for its partner.
    high: Option<u16>,
    final_para: Option<ParaStyle>,
    fldinst: String,
    /// The link the next `\fldrslt` carries.
    field_link: Option<String>,
}

pub fn parse(src: &[u8]) -> RichText {
    let mut r = Reader {
        src,
        pos: 0,
        stack: Vec::new(),
        st: State {
            ch: CharStyle::default(),
            para: ParaStyle::default(),
            dest: Dest::Text,
            uc: 1,
            sl: 0,
            face_bold: false,
            face_italic: false,
            tab_kind: 'l',
        },
        fonts: Vec::new(),
        font_name: String::new(),
        colors: Vec::new(),
        color: (0, 0, 0, false),
        codepage: 1252,
        out: RichText::default(),
        current: Paragraph::default(),
        pending: String::new(),
        skip: 0,
        high: None,
        final_para: None,
        fldinst: String::new(),
        field_link: None,
    };
    r.run();
    r.out
}

impl Reader<'_> {
    fn lossy(&mut self, what: &'static str) {
        if !self.out.lossy.contains(&what) {
            self.out.lossy.push(what);
        }
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            let text = std::mem::take(&mut self.pending);
            self.current.push(&text, &self.st.ch);
        }
    }

    fn char(&mut self, c: char) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        match self.st.dest {
            Dest::Text | Dest::ListText => self.pending.push(c),
            Dest::FontTable => {
                if c == ';' {
                    if let Some(entry) = self.fonts.last_mut() {
                        entry.1 = std::mem::take(&mut self.font_name).trim().to_string();
                    }
                } else {
                    self.font_name.push(c);
                }
            }
            Dest::ColorTable => {
                if c == ';' {
                    let (r, g, b, set) = self.color;
                    self.colors.push(set.then_some((r, g, b)));
                    self.color = (0, 0, 0, false);
                }
            }
            Dest::FldInst => self.fldinst.push(c),
            Dest::Skip => {}
        }
    }

    fn end_paragraph(&mut self) {
        self.flush();
        self.current.style = self.st.para.clone();
        let p = std::mem::take(&mut self.current);
        self.out.paragraphs.push(p);
    }

    /// Sets the char style's font from a `\fN`.
    fn set_font(&mut self, n: i32) {
        let name = self
            .fonts
            .iter()
            .find(|(i, _)| *i == n)
            .map(|(_, name)| name.clone());
        let Some(name) = name else { return };
        let (family, bold, italic) = font_family(&name);
        self.st.ch.font = Some(family);
        // Cocoa names the face ("Helvetica-Bold") and usually also writes
        // \b, but not always. A trait that only came from the previous
        // face ends with it.
        if bold {
            self.st.ch.bold = true;
        } else if self.st.face_bold {
            self.st.ch.bold = false;
        }
        if italic {
            self.st.ch.italic = true;
        } else if self.st.face_italic {
            self.st.ch.italic = false;
        }
        self.st.face_bold = bold;
        self.st.face_italic = italic;
    }

    fn run(&mut self) {
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            self.pos += 1;
            match b {
                b'{' => {
                    self.flush();
                    self.stack.push(self.st.clone());
                }
                b'}' => {
                    self.flush();
                    let leaving = self.st.dest;
                    if self.stack.len() == 1 {
                        // Closing the document: the last paragraph keeps
                        // the style in force here, not the one outside.
                        self.final_para = Some(self.st.para.clone());
                    }
                    if let Some(prev) = self.stack.pop() {
                        self.st = prev;
                    }
                    if leaving == Dest::FontTable {
                        self.font_name.clear();
                    }
                    if leaving == Dest::FldInst && self.st.dest != Dest::FldInst {
                        self.field_link = hyperlink(&std::mem::take(&mut self.fldinst));
                    }
                    self.skip = 0;
                }
                b'\\' => self.control(),
                b'\r' | b'\n' => {}
                b if b >= 0x80 => {
                    // Raw 8-bit bytes: a stray UTF-8 sequence or a codepage byte.
                    let start = self.pos - 1;
                    let len = match b {
                        0xC0..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        0xF0..=0xF7 => 4,
                        _ => 1,
                    };
                    let end = (start + len).min(self.src.len());
                    match std::str::from_utf8(&self.src[start..end]) {
                        Ok(s) if len > 1 => {
                            self.pos = end;
                            for c in s.chars() {
                                self.char(c);
                            }
                        }
                        _ => {
                            let c = decode_byte(b, self.codepage);
                            self.char(c);
                        }
                    }
                }
                _ => self.char(b as char),
            }
        }
        self.flush();
        // The last paragraph has no \par after it.
        self.current.style = self
            .final_para
            .take()
            .unwrap_or_else(|| self.st.para.clone());
        let p = std::mem::take(&mut self.current);
        self.out.paragraphs.push(p);
    }

    fn control(&mut self) {
        let Some(&c) = self.src.get(self.pos) else {
            return;
        };
        if !c.is_ascii_alphabetic() {
            self.pos += 1;
            match c {
                b'\'' => {
                    let hex = self.src.get(self.pos..self.pos + 2).unwrap_or(b"");
                    let byte = std::str::from_utf8(hex)
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    self.pos += hex.len();
                    if let Some(byte) = byte {
                        let ch = decode_byte(byte, self.codepage);
                        self.char(ch);
                    }
                }
                b'\n' | b'\r' => {
                    // Cocoa's paragraph break is a backslash at line end.
                    if self.st.dest == Dest::Text {
                        self.end_paragraph();
                    }
                }
                b'*' => {
                    self.flush();
                    self.st.dest = Dest::Skip;
                }
                b'~' => self.char('\u{a0}'),
                b'_' => self.char('\u{2011}'),
                b'-' => {} // optional hyphen
                b'\\' | b'{' | b'}' => self.char(c as char),
                b'\t' => self.char('\t'),
                _ => {}
            }
            return;
        }
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let word = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        let num_start = self.pos;
        if self.src.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let param: Option<i32> = std::str::from_utf8(&self.src[num_start..self.pos])
            .ok()
            .and_then(|s| s.parse().ok());
        if self.src.get(self.pos) == Some(&b' ') {
            self.pos += 1;
        }
        self.word(word, param);
    }

    fn word(&mut self, word: &str, param: Option<i32>) {
        let on = param != Some(0);
        let n = param.unwrap_or(0);
        if word == "bin" {
            // Raw bytes follow; never read them as RTF.
            self.pos = (self.pos + n.max(0) as usize).min(self.src.len());
            return;
        }
        // A control word inside a \u fallback counts as one skipped char.
        if self.skip > 0 && !matches!(word, "u") {
            self.skip -= 1;
            return;
        }
        match self.st.dest {
            Dest::FontTable => {
                match word {
                    "f" => {
                        self.fonts.push((n, String::new()));
                        self.font_name.clear();
                    }
                    "uc" => self.st.uc = n.max(0) as usize,
                    "u" => {
                        if let Some(c) = param
                            .and_then(unicode_unit)
                            .and_then(|u| char::from_u32(u as u32))
                        {
                            self.font_name.push(c);
                        }
                        self.skip = self.st.uc;
                    }
                    _ => {}
                }
                return;
            }
            Dest::FldInst => return,
            Dest::ColorTable => {
                match word {
                    "red" => (self.color.0, self.color.3) = (n.clamp(0, 255) as u8, true),
                    "green" => (self.color.1, self.color.3) = (n.clamp(0, 255) as u8, true),
                    "blue" => (self.color.2, self.color.3) = (n.clamp(0, 255) as u8, true),
                    _ => {}
                }
                return;
            }
            Dest::Skip => {
                // Images and notes hide in \* groups too (\shppict).
                match word {
                    "pict" | "NeXTGraphic" | "object" | "shp" => self.lossy("images"),
                    "annotation" => self.lossy("comments"),
                    "listtable" | "list" => self.lossy("lists"),
                    "fldinst" => {
                        self.fldinst.clear();
                        self.st.dest = Dest::FldInst;
                    }
                    _ => {}
                }
                return;
            }
            Dest::Text | Dest::ListText => {}
        }
        match word {
            // Header and destinations.
            "ansicpg" => self.codepage = n as u32,
            "mac" => self.codepage = 10000,
            "fonttbl" => self.st.dest = Dest::FontTable,
            "colortbl" => self.st.dest = Dest::ColorTable,
            "stylesheet" | "info" | "header" | "headerl" | "headerr" | "headerf" | "footer"
            | "footerl" | "footerr" | "footerf" | "listtable" | "listoverridetable" | "revtbl"
            | "xmlnstbl" | "themedata" | "colorschememapping" | "latentstyles" | "datastore" => {
                self.flush();
                self.st.dest = Dest::Skip;
            }
            "pict" | "NeXTGraphic" | "object" | "shp" | "nonshppict" => {
                self.flush();
                self.lossy("images");
                self.st.dest = Dest::Skip;
            }
            "footnote" => {
                self.flush();
                self.lossy("footnotes");
                self.st.dest = Dest::Skip;
            }
            "annotation" | "atnid" | "atnauthor" => {
                self.flush();
                self.lossy("comments");
                self.st.dest = Dest::Skip;
            }
            "listtext" => {
                self.flush();
                self.st.dest = Dest::ListText;
            }
            "trowd" => self.lossy("tables"),
            "ls" => self.lossy("lists"),
            "fldinst" => {
                self.flush();
                self.fldinst.clear();
                self.st.dest = Dest::FldInst;
            }
            "fldrslt" => {
                self.flush();
                self.st.ch.link = self.field_link.take();
            }
            "Scrv" | "Scrvannot" => self.lossy("inline annotations"),
            // Unicode.
            "uc" => self.st.uc = n.max(0) as usize,
            "u" => {
                self.skip = self.st.uc;
                // No or out-of-range parameter: nothing to insert.
                let Some(unit) = param.and_then(unicode_unit) else {
                    return;
                };
                if (0xD800..0xDC00).contains(&unit) {
                    self.high = Some(unit);
                } else if (0xDC00..0xE000).contains(&unit) {
                    if let Some(high) = self.high.take() {
                        let c = 0x10000 + (((high as u32) - 0xD800) << 10) + (unit as u32 - 0xDC00);
                        if let Some(c) = char::from_u32(c) {
                            self.pending_char(c);
                        }
                    }
                } else if let Some(c) = char::from_u32(unit as u32) {
                    self.high = None;
                    self.pending_char(c);
                }
            }
            // Special characters.
            "par" => {
                if self.st.dest == Dest::Text {
                    self.end_paragraph();
                }
            }
            "line" => self.char('\u{2028}'),
            "tab" => self.char('\t'),
            "cell" => self.char('\t'),
            "row" => {
                // Drop the tab the last \cell left, then end the row.
                self.flush();
                if let Some(run) = self.current.runs.last_mut()
                    && run.text.ends_with('\t')
                {
                    run.text.pop();
                }
                self.current.runs.retain(|r| !r.text.is_empty());
                self.end_paragraph();
            }
            "emdash" => self.char('\u{2014}'),
            "endash" => self.char('\u{2013}'),
            "emspace" => self.char('\u{2003}'),
            "enspace" => self.char('\u{2002}'),
            "bullet" => self.char('\u{2022}'),
            "lquote" => self.char('\u{2018}'),
            "rquote" => self.char('\u{2019}'),
            "ldblquote" => self.char('\u{201c}'),
            "rdblquote" => self.char('\u{201d}'),
            "page" | "sect" => {
                if self.st.dest == Dest::Text {
                    self.end_paragraph();
                }
            }
            // Character formatting.
            "plain" => {
                self.flush();
                self.st.ch = CharStyle::default();
            }
            "f" => {
                self.flush();
                self.set_font(n);
            }
            "fs" => {
                self.flush();
                self.st.ch.size = n.clamp(2, 3000) as u16;
            }
            "fsmilli" => {
                self.flush();
                self.st.ch.size = (n / 500).clamp(2, 3000) as u16;
            }
            "b" => {
                self.flush();
                self.st.ch.bold = on;
                self.st.face_bold = false;
            }
            "i" => {
                self.flush();
                self.st.ch.italic = on;
                self.st.face_italic = false;
            }
            "ul" | "uld" | "uldb" | "ulw" | "ulth" => {
                self.flush();
                self.st.ch.underline = on;
            }
            "ulnone" => {
                self.flush();
                self.st.ch.underline = false;
            }
            "strike" | "striked" => {
                self.flush();
                self.st.ch.strike = on;
            }
            "cf" => {
                self.flush();
                self.st.ch.color = self.colors.get(n as usize).copied().flatten();
            }
            "cb" | "highlight" | "chcbpat" => {
                self.flush();
                self.st.ch.highlight = self.colors.get(n as usize).copied().flatten();
            }
            "super" => {
                self.flush();
                self.st.ch.script = Script::Super;
            }
            "sub" => {
                self.flush();
                self.st.ch.script = Script::Sub;
            }
            "nosupersub" => {
                self.flush();
                self.st.ch.script = Script::Normal;
            }
            "up" => {
                self.flush();
                self.st.ch.script = if n > 0 { Script::Super } else { Script::Normal };
            }
            "dn" => {
                self.flush();
                self.st.ch.script = if n > 0 { Script::Sub } else { Script::Normal };
            }
            // Paragraph formatting.
            "pard" => {
                self.flush();
                self.st.para = ParaStyle::default();
                self.st.sl = 0;
            }
            "tqr" => self.st.tab_kind = 'r',
            "tqc" => self.st.tab_kind = 'c',
            "tqdec" => self.st.tab_kind = 'd',
            "tx" => {
                self.st.para.tabs.push((n, self.st.tab_kind));
                self.st.tab_kind = 'l';
            }
            "ql" => self.st.para.align = Align::Left,
            "qc" => self.st.para.align = Align::Center,
            "qr" => self.st.para.align = Align::Right,
            "qj" => self.st.para.align = Align::Justify,
            "qnatural" => self.st.para.align = Align::Natural,
            "fi" => self.st.para.first_indent = n,
            "li" => self.st.para.left_indent = n,
            "ri" => self.st.para.right_indent = n,
            "sb" => self.st.para.space_before = n,
            "sa" => self.st.para.space_after = n,
            "sl" => {
                self.st.sl = n;
                self.st.para.line_spacing = match n {
                    0 => LineSpacing::Single,
                    n if n < 0 => LineSpacing::Exactly(-n),
                    n => LineSpacing::AtLeast(n),
                };
            }
            "slmult" if n == 1 && self.st.sl > 0 => {
                self.st.para.line_spacing = LineSpacing::Multiple(self.st.sl as f32 / 240.0);
            }
            _ => {}
        }
    }

    fn pending_char(&mut self, c: char) {
        match self.st.dest {
            Dest::Text | Dest::ListText => self.pending.push(c),
            Dest::FontTable => self.font_name.push(c),
            _ => {}
        }
    }
}

/// A `\uN` parameter as a UTF-16 unit: RTF writes units above 32767 as
/// negative numbers. Anything outside -32768..=65535 is invalid.
fn unicode_unit(n: i32) -> Option<u16> {
    match n {
        -32768..=-1 => Some((n + 65536) as u16),
        0..=65535 => Some(n as u16),
        _ => None,
    }
    .filter(|&u| u != 0)
}

/// The target of a `HYPERLINK "..."` field instruction.
fn hyperlink(inst: &str) -> Option<String> {
    let rest = inst.trim().strip_prefix("HYPERLINK")?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string()).filter(|u| !u.is_empty())
}

/// Windows-1252 (and Mac Roman for `\mac` files) to Unicode.
fn decode_byte(b: u8, codepage: u32) -> char {
    const CP1252: [u16; 32] = [
        0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0xFFFD, 0x017D, 0xFFFD, 0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178,
    ];
    const MAC_ROMAN: [u16; 128] = [
        0xC4, 0xC5, 0xC7, 0xC9, 0xD1, 0xD6, 0xDC, 0xE1, 0xE0, 0xE2, 0xE4, 0xE3, 0xE5, 0xE7, 0xE9,
        0xE8, 0xEA, 0xEB, 0xED, 0xEC, 0xEE, 0xEF, 0xF1, 0xF3, 0xF2, 0xF4, 0xF6, 0xF5, 0xFA, 0xF9,
        0xFB, 0xFC, 0x2020, 0xB0, 0xA2, 0xA3, 0xA7, 0x2022, 0xB6, 0xDF, 0xAE, 0xA9, 0x2122, 0xB4,
        0xA8, 0x2260, 0xC6, 0xD8, 0x221E, 0xB1, 0x2264, 0x2265, 0xA5, 0xB5, 0x2202, 0x2211, 0x220F,
        0x3C0, 0x222B, 0xAA, 0xBA, 0x3A9, 0xE6, 0xF8, 0xBF, 0xA1, 0xAC, 0x221A, 0x192, 0x2248,
        0x2206, 0xAB, 0xBB, 0x2026, 0xA0, 0xC0, 0xC3, 0xD5, 0x152, 0x153, 0x2013, 0x2014, 0x201C,
        0x201D, 0x2018, 0x2019, 0xF7, 0x25CA, 0xFF, 0x178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01,
        0xFB02, 0x2021, 0xB7, 0x201A, 0x201E, 0x2030, 0xC2, 0xCA, 0xC1, 0xCB, 0xC8, 0xCD, 0xCE,
        0xCF, 0xCC, 0xD3, 0xD4, 0xF8FF, 0xD2, 0xDA, 0xDB, 0xD9, 0x131, 0x2C6, 0x2DC, 0xAF, 0x2D8,
        0x2D9, 0x2DA, 0xB8, 0x2DD, 0x2DB, 0x2C7,
    ];
    let code = match (codepage, b) {
        (_, 0..=0x7F) => b as u32,
        (10000, _) => MAC_ROMAN[(b - 0x80) as usize] as u32,
        (_, 0x80..=0x9F) => CP1252[(b - 0x80) as usize] as u32,
        _ => b as u32,
    };
    char::from_u32(code).unwrap_or('\u{fffd}')
}

/// Turns a font name from the font table into (family, is bold, is italic).
/// Cocoa writes PostScript names: "Palatino-Roman", "Helvetica-BoldOblique",
/// "TimesNewRomanPSMT".
pub fn font_family(name: &str) -> (String, bool, bool) {
    let known = [
        ("TimesNewRomanPS", "Times New Roman"),
        ("CourierNewPS", "Courier New"),
        ("ArialMT", "Arial"),
        ("Arial", "Arial"),
        ("HelveticaNeue", "Helvetica Neue"),
        ("GillSans", "Gill Sans"),
        ("AmericanTypewriter", "American Typewriter"),
    ];
    let (base, face) = match name.split_once('-') {
        Some((b, f)) => (b, f),
        None => (name, ""),
    };
    let lower_face = face.to_ascii_lowercase();
    let mut bold =
        lower_face.contains("bold") || lower_face.contains("black") || lower_face.contains("heavy");
    let mut italic = lower_face.contains("italic") || lower_face.contains("oblique");
    // TimesNewRomanPS-BoldMT, TimesNewRomanPSMT
    if let Some((_, family)) = known.iter().find(|(prefix, _)| base.starts_with(prefix)) {
        return (family.to_string(), bold, italic);
    }
    if base.contains(' ') {
        // Already a family name ("Times New Roman").
        let lower = base.to_ascii_lowercase();
        bold |= lower.ends_with(" bold");
        italic |= lower.ends_with(" italic");
        return (base.to_string(), bold, italic);
    }
    // CamelCase to words: "CenturySchoolbook" -> "Century Schoolbook".
    let mut family = String::new();
    let chars: Vec<char> = base.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let prev = if i > 0 { Some(chars[i - 1]) } else { None };
        if i > 0
            && c.is_uppercase()
            && (prev.is_some_and(|p| p.is_lowercase())
                || (next_lower && prev.is_some_and(|p| p.is_uppercase())))
        {
            family.push(' ');
        }
        family.push(c);
    }
    (family, bold, italic)
}

// ---------------------------------------------------------------- writing

/// Writes `text` as Cocoa-flavored RTF, the way Scrivener lays it out.
pub fn write(text: &RichText) -> String {
    let mut fonts: Vec<String> = Vec::new();
    let mut colors: Vec<Rgb> = Vec::new();
    for p in &text.paragraphs {
        for r in &p.runs {
            let font = r.style.font.clone().unwrap_or_else(default_font);
            if !fonts.contains(&font) {
                fonts.push(font);
            }
            for c in [r.style.color, r.style.highlight].into_iter().flatten() {
                if !colors.contains(&c) {
                    colors.push(c);
                }
            }
        }
    }
    if fonts.is_empty() {
        fonts.push(default_font());
    }
    // Color 1 is white in every file Scrivener writes; keep that.
    let mut table: Vec<Rgb> = vec![(255, 255, 255)];
    for c in colors {
        if !table.contains(&c) {
            table.push(c);
        }
    }

    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg1252\\cocoartf2822\n\\cocoatextscaling0\\cocoaplatform0{\\fonttbl",
    );
    for (i, f) in fonts.iter().enumerate() {
        // ';' ends a font-table entry, so it can't be part of a name.
        let name: String = f.chars().filter(|&c| c != ';').collect();
        let _ = write!(out, "\\f{i}\\fnil\\fcharset0 {};", rtf_escape_plain(&name));
    }
    out.push_str("}\n{\\colortbl;");
    for (r, g, b) in &table {
        let _ = write!(out, "\\red{r}\\green{g}\\blue{b};");
    }
    out.push_str("}\n{\\*\\expandedcolortbl;");
    for _ in &table {
        out.push(';');
    }
    out.push_str("}\n");

    let color_index = |c: Option<Rgb>| -> usize {
        c.and_then(|c| table.iter().position(|t| *t == c))
            .map_or(0, |i| i + 1)
    };
    let font_index = |f: &Option<String>| -> usize {
        let f = f.clone().unwrap_or_else(default_font);
        fonts.iter().position(|x| *x == f).unwrap_or(0)
    };

    let mut last_para: Option<&ParaStyle> = None;
    let mut ch = CharStyle {
        font: Some(String::new()), // forces \f on the first run
        size: 0,
        ..CharStyle::default()
    };
    let mut cf = usize::MAX;
    // An open `\field`: its link, and the formatting outside it, which
    // comes back when its groups close.
    let mut field: Option<(String, CharStyle, usize)> = None;
    fn close_field(
        out: &mut String,
        field: &mut Option<(String, CharStyle, usize)>,
        ch: &mut CharStyle,
        cf: &mut usize,
    ) {
        if let Some((_, outer, f)) = field.take() {
            out.push_str("}}");
            *ch = outer;
            *cf = f;
        }
    }
    for (pi, p) in text.paragraphs.iter().enumerate() {
        if last_para != Some(&p.style) {
            write_para(&mut out, &p.style);
            out.push_str("\n\n");
            last_para = Some(&p.style);
        }
        for r in &p.runs {
            let s = &r.style;
            if field.as_ref().map(|f| &f.0) != s.link.as_ref() {
                close_field(&mut out, &mut field, &mut ch, &mut cf);
                if let Some(url) = &s.link {
                    let url: String = url
                        .chars()
                        .filter(|c| !matches!(c, '"' | '{' | '}' | '\\'))
                        .collect();
                    let _ = write!(
                        out,
                        "{{\\field{{\\*\\fldinst{{HYPERLINK \"{url}\"}}}}{{\\fldrslt "
                    );
                    field = Some((s.link.clone().unwrap(), ch.clone(), cf));
                }
            }
            let mut codes = String::new();
            if font_index(&s.font) != font_index(&ch.font) || ch.font.as_deref() == Some("") {
                let _ = write!(codes, "\\f{}", font_index(&s.font));
            }
            if s.size != ch.size {
                let _ = write!(codes, "\\fs{}", s.size);
            }
            if s.bold != ch.bold {
                codes.push_str(if s.bold { "\\b" } else { "\\b0" });
            }
            if s.italic != ch.italic {
                codes.push_str(if s.italic { "\\i" } else { "\\i0" });
            }
            if s.underline != ch.underline {
                codes.push_str(if s.underline {
                    "\\ul\\ulc0"
                } else {
                    "\\ulnone"
                });
            }
            if s.strike != ch.strike {
                codes.push_str(if s.strike {
                    "\\strike\\strikec0"
                } else {
                    "\\strike0"
                });
            }
            if s.script != ch.script {
                codes.push_str(match s.script {
                    Script::Super => "\\super",
                    Script::Sub => "\\sub",
                    Script::Normal => "\\nosupersub",
                });
            }
            let want_cf = color_index(s.color);
            if want_cf != cf {
                let _ = write!(codes, "\\cf{want_cf}");
                cf = want_cf;
            }
            if !codes.is_empty() {
                out.push_str(&codes);
                out.push(' ');
            }
            // Highlight goes in a group of its own: Cocoa reads `\cb0` as
            // a black highlight, not as none, so it can't be switched off.
            match color_index(s.highlight) {
                0 => rtf_escape_into(&mut out, &r.text),
                cb => {
                    let _ = write!(out, "{{\\cb{cb} ");
                    rtf_escape_into(&mut out, &r.text);
                    out.push('}');
                }
            }
            ch = s.clone();
        }
        close_field(&mut out, &mut field, &mut ch, &mut cf);
        if pi + 1 < text.paragraphs.len() {
            out.push_str("\\\n");
        }
    }
    out.push('}');
    out
}

fn default_font() -> String {
    "Palatino".to_string()
}

fn write_para(out: &mut String, s: &ParaStyle) {
    out.push_str("\\pard");
    for (at, kind) in &s.tabs {
        out.push_str(match kind {
            'r' => "\\tqr",
            'c' => "\\tqc",
            'd' => "\\tqdec",
            _ => "",
        });
        let _ = write!(out, "\\tx{at}");
    }
    if s.left_indent != 0 {
        let _ = write!(out, "\\li{}", s.left_indent);
    }
    if s.first_indent != 0 {
        let _ = write!(out, "\\fi{}", s.first_indent);
    }
    if s.right_indent != 0 {
        let _ = write!(out, "\\ri{}", s.right_indent);
    }
    match s.line_spacing {
        LineSpacing::Single => {}
        LineSpacing::Multiple(m) => {
            let _ = write!(out, "\\sl{}\\slmult1", (m * 240.0).round() as i32);
        }
        LineSpacing::AtLeast(t) => {
            let _ = write!(out, "\\sl{t}\\slmult0");
        }
        LineSpacing::Exactly(t) => {
            let _ = write!(out, "\\sl-{t}\\slmult0");
        }
    }
    if s.space_before != 0 {
        let _ = write!(out, "\\sb{}", s.space_before);
    }
    if s.space_after != 0 {
        let _ = write!(out, "\\sa{}", s.space_after);
    }
    out.push_str("\\pardirnatural");
    out.push_str(match s.align {
        Align::Natural => "",
        Align::Left => "\\ql",
        Align::Center => "\\qc",
        Align::Right => "\\qr",
        Align::Justify => "\\qj",
    });
    out.push_str("\\partightenfactor0");
}

fn rtf_escape_plain(s: &str) -> String {
    let mut out = String::new();
    rtf_escape_into(&mut out, s);
    out
}

fn rtf_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\t' => out.push('\t'),
            '\u{2028}' | '\n' => out.push_str("\\line "),
            '\u{a0}' => out.push_str("\\~"),
            c if (c as u32) < 0x20 => {}
            c if c.is_ascii() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\uc0\\u{} ", *u as i16);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COCOA: &str = r"{\rtf1\ansi\ansicpg1252\cocoartf2638
\cocoatextscaling0\cocoaplatform0{\fonttbl\f0\froman\fcharset0 Palatino-Roman;\f1\froman\fcharset0 Palatino-Bold;}
{\colortbl;\red255\green255\blue255;\red200\green0\blue0;}
{\*\expandedcolortbl;;\csgenericrgb\c78431\c0\c0;}
\pard\tx560\tx1120\sl264\slmult1\fi720\pardirnatural\qj\partightenfactor0

\f0\fs24 \cf0 She said \'93hello\'94 and caf\'e9 \uc0\u8212  then {\i left}.\
\
\pard\pardirnatural\qc\partightenfactor0

\f1\b\fs36 \cf2 Chapter\uc0\u55357 \u56832 }";

    #[test]
    fn reads_cocoa_rtf() {
        let t = parse(COCOA.as_bytes());
        assert_eq!(
            t.plain_text(),
            "She said \u{201c}hello\u{201d} and caf\u{e9} \u{2014} then left.\n\nChapter\u{1f600}"
        );
        let p0 = &t.paragraphs[0];
        assert_eq!(p0.style.align, Align::Justify);
        assert_eq!(p0.style.first_indent, 720);
        assert_eq!(p0.style.line_spacing, LineSpacing::Multiple(1.1));
        assert_eq!(p0.runs[0].style.font.as_deref(), Some("Palatino"));
        assert!(!p0.runs[0].style.italic);
        assert_eq!(p0.runs[1].text, "left");
        assert!(p0.runs[1].style.italic);
        let last = t.paragraphs.last().unwrap();
        assert_eq!(last.style.align, Align::Center);
        let run = &last.runs[0];
        assert!(run.style.bold);
        assert_eq!(run.style.size, 36);
        assert_eq!(run.style.color, Some((200, 0, 0)));
        assert!(t.lossy.is_empty());
    }

    #[test]
    fn write_then_read_is_stable() {
        let t = parse(COCOA.as_bytes());
        let rtf = write(&t);
        let again = parse(rtf.as_bytes());
        assert_eq!(again.paragraphs, t.paragraphs, "{rtf}");
        assert_eq!(write(&again), rtf);
    }

    #[test]
    fn escapes_braces_and_backslashes() {
        let t = RichText::from_plain("a {b} \\c\nnext");
        let again = parse(write(&t).as_bytes());
        assert_eq!(again.plain_text(), "a {b} \\c\nnext");
    }

    #[test]
    fn empty_document() {
        let t = parse(b"");
        assert_eq!(t.plain_text(), "");
        let t = RichText::from_plain("");
        assert_eq!(parse(write(&t).as_bytes()).plain_text(), "");
    }

    #[test]
    fn list_markers_and_tables_become_text() {
        let src = r"{\rtf1\ansi{\*\listtable{\list{\listlevel{\*\levelmarker \{disc\}}}}}
\pard\ls1\ilvl0\cf0 {\listtext	\uc0\u8226 	}One\
{\listtext	\uc0\u8226 	}Two\
\itap1\trowd \cellx4320\cellx8640
\pard\intbl\itap1 A\cell B\cell \lastrow\row
\pard After}";
        let t = parse(src.as_bytes());
        assert_eq!(
            t.plain_text(),
            "\t\u{2022}\tOne\n\t\u{2022}\tTwo\nA\tB\nAfter"
        );
        assert_eq!(t.lossy, vec!["lists", "tables"]);
    }

    #[test]
    fn flags_images_and_footnotes() {
        let t = parse(br"{\rtf1 a{\*\shppict{\pict\pngblip 89504e47}}b{\footnote note}c}");
        assert_eq!(t.plain_text(), "abc");
        assert_eq!(t.lossy, vec!["images", "footnotes"]);
    }

    #[test]
    fn fallback_chars_after_unicode_are_skipped() {
        let t = parse(br"{\rtf1\uc1 x\u233\'e9y\uc2\u8212--z}");
        assert_eq!(t.plain_text(), "x\u{e9}y\u{2014}z");
    }

    #[test]
    fn links_and_comment_anchors_survive() {
        let src = br#"{\rtf1{\fonttbl\f0 Palatino-Roman;}\f0 see {\field{\*\fldinst{HYPERLINK "scrivcmt://ABCD"}}{\fldrslt \i the note}} and {\field{\*\fldinst{HYPERLINK "https://x.org/a?b=c"}}{\fldrslt site}} end}"#;
        let t = parse(src);
        assert_eq!(t.plain_text(), "see the note and site end");
        let note = t.paragraphs[0]
            .runs
            .iter()
            .find(|r| r.text == "the note")
            .unwrap();
        assert_eq!(note.style.link.as_deref(), Some("scrivcmt://ABCD"));
        assert!(note.style.italic);
        let end = t.paragraphs[0].runs.last().unwrap();
        assert_eq!(end.style.link, None);
        assert!(
            !end.style.italic,
            "formatting inside the field ends with it"
        );
        let again = parse(write(&t).as_bytes());
        assert_eq!(again.paragraphs, t.paragraphs);
    }

    #[test]
    fn highlight_is_grouped_never_cb0() {
        let mut t = RichText::from_plain("a b c");
        let mut runs = Vec::new();
        for (text, hl) in [("a ", None), ("b", Some((255, 230, 120))), (" c", None)] {
            let mut r = t.paragraphs[0].runs[0].clone();
            r.text = text.into();
            r.style.highlight = hl;
            r.style.font = Some("Palatino".into());
            runs.push(r);
        }
        t.paragraphs[0].runs = runs;
        let rtf = write(&t);
        // macOS takes \cb0 as a black highlight.
        assert!(!rtf.contains("\\cb0"), "{rtf}");
        assert!(rtf.contains("{\\cb2 b}"), "{rtf}");
        assert_eq!(parse(rtf.as_bytes()).paragraphs, t.paragraphs);
    }

    #[test]
    fn tab_stops_survive() {
        let t = parse(br"{\rtf1{\fonttbl\f0 Palatino-Roman;}\pard\tx720\tqr\tx8640\f0 a\tab b}");
        assert_eq!(t.paragraphs[0].style.tabs, vec![(720, 'l'), (8640, 'r')]);
        let again = parse(write(&t).as_bytes());
        assert_eq!(again.paragraphs, t.paragraphs);
    }

    #[test]
    fn face_traits_end_with_the_face() {
        let t = parse(br"{\rtf1{\fonttbl\f0 Palatino-Roman;\f1 Palatino-Italic;}\f1 a\f0  b}");
        let runs = &t.paragraphs[0].runs;
        assert!(runs[0].style.italic);
        assert!(!runs[1].style.italic);
        // An explicit \i outlives the face.
        let t = parse(br"{\rtf1{\fonttbl\f0 Palatino-Roman;\f1 Palatino-Italic;}\f1\i a\f0  b}");
        assert!(t.paragraphs[0].runs.iter().all(|r| r.style.italic));
    }

    #[test]
    fn odd_font_names_round_trip() {
        let mut t = RichText::from_plain("x");
        t.paragraphs[0].runs[0].style.font = Some("Garamond Pr\u{e9}mier".into());
        let again = parse(write(&t).as_bytes());
        assert_eq!(
            again.paragraphs[0].runs[0].style.font.as_deref(),
            Some("Garamond Pr\u{e9}mier")
        );
        t.paragraphs[0].runs[0].style.font = Some("A;B".into());
        let again = parse(write(&t).as_bytes());
        assert_eq!(again.plain_text(), "x");
        assert_eq!(
            again.paragraphs[0].runs[0].style.font.as_deref(),
            Some("AB")
        );
    }

    #[test]
    fn bad_unicode_and_binary_are_ignored() {
        // No NUL or garbage for an out-of-range \u; \bin's bytes (here "}{}")
        // aren't read as RTF.
        let t = parse(br"{\rtf1 a\uc0\u99999999 b{\*\blob\bin3 }{}}c}");
        assert_eq!(t.plain_text(), "abc");
    }

    #[test]
    fn font_names() {
        assert_eq!(
            font_family("Palatino-Roman"),
            ("Palatino".into(), false, false)
        );
        assert_eq!(
            font_family("Helvetica-BoldOblique"),
            ("Helvetica".into(), true, true)
        );
        assert_eq!(
            font_family("TimesNewRomanPSMT"),
            ("Times New Roman".into(), false, false)
        );
        assert_eq!(
            font_family("TimesNewRomanPS-ItalicMT"),
            ("Times New Roman".into(), false, true)
        );
        assert_eq!(
            font_family("CenturySchoolbook"),
            ("Century Schoolbook".into(), false, false)
        );
        assert_eq!(
            font_family("LucidaGrande"),
            ("Lucida Grande".into(), false, false)
        );
        assert_eq!(font_family("Courier"), ("Courier".into(), false, false));
    }

    #[test]
    fn counts_words() {
        assert_eq!(
            word_count("Don't stop\u{2014}well-known, 3 pm \u{2014} ok."),
            6
        );
        assert_eq!(word_count("  "), 0);
    }
}
