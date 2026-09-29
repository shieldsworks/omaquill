//! Compile: the manuscript out as one file (DOCX, EPUB, Markdown, RTF or
//! plain text).
//!
//! Structure follows Scrivener's novel convention: every item directly in
//! the Draft folder is a chapter, and the text documents inside it (at any
//! depth) are its scenes, in binder order. [`gather`] turns the binder into
//! that list of [`Section`]s once, and each format writes it out.
//!
//! Every format is written by hand. DOCX and EPUB are zips of XML, stored
//! uncompressed by [`crate::zip`], which every reader accepts.

use crate::project::{Item, Kind, Project, Result};
use crate::rtf::{
    self, Align, CharStyle, LineSpacing, ParaStyle, Paragraph, RichText, Run, Script,
};
use crate::xml::escape;
use crate::zip::ZipWriter;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Docx,
    Epub,
    Markdown,
    PlainText,
    Rtf,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Docx,
        Format::Epub,
        Format::Markdown,
        Format::PlainText,
        Format::Rtf,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Docx => "docx",
            Format::Epub => "epub",
            Format::Markdown => "md",
            Format::PlainText => "txt",
            Format::Rtf => "rtf",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Docx => "Word (DOCX)",
            Format::Epub => "EPUB",
            Format::Markdown => "Markdown",
            Format::PlainText => "Plain text",
            Format::Rtf => "RTF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Headings {
    /// "Chapter One", "Chapter Two" ...
    Numbered,
    /// The chapter's binder title.
    Titles,
    None,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub title: String,
    pub author: String,
    /// Standard manuscript format: Times New Roman 12 pt, double spaced, 1"
    /// margins, 0.5" first-line indents, title page with name/word count,
    /// running header "Surname / TITLE / page". Otherwise the text keeps its
    /// own formatting.
    pub manuscript: bool,
    pub headings: Headings,
    /// Goes between scenes within a chapter.
    pub scene_separator: String,
}

impl Options {
    pub fn for_project(p: &Project) -> Options {
        Options {
            title: p.name(),
            author: String::new(),
            manuscript: true,
            headings: Headings::Numbered,
            scene_separator: "#".to_string(),
        }
    }
}

/// One chapter: its heading (per [`Headings`]) and its non-empty scenes.
#[derive(Debug, Clone)]
pub struct Section {
    pub heading: Option<String>,
    pub scenes: Vec<RichText>,
}

// ------------------------------------------------------------- gathering

/// What gets compiled, in order: walks the Draft folder, skipping items
/// with `include_in_compile == false` (and their children).
pub fn gather(project: &Project, opts: &Options) -> Result<Vec<Section>> {
    let Some(draft) = project.root_folder(Kind::Draft) else {
        return Ok(Vec::new());
    };
    let mut sections = Vec::new();
    for item in draft.children.iter().filter(|i| i.include_in_compile) {
        let mut scenes = Vec::new();
        collect_scenes(project, item, &mut scenes)?;
        let heading = match opts.headings {
            Headings::Numbered => Some(format!("Chapter {}", number_words(sections.len() + 1))),
            Headings::Titles => Some(item.title.clone()),
            Headings::None => None,
        };
        sections.push(Section { heading, scenes });
    }
    Ok(sections)
}

/// A folder's own text first, then its descendants depth-first: nested
/// folders flatten into the chapter they sit in.
fn collect_scenes(project: &Project, item: &Item, scenes: &mut Vec<RichText>) -> Result<()> {
    if item.kind.has_text() {
        let mut text = project.text(&item.uuid)?;
        // Blank lines at either end would show up as gaps around separators.
        while text.paragraphs.last().is_some_and(is_blank) {
            text.paragraphs.pop();
        }
        let lead = text.paragraphs.iter().take_while(|p| is_blank(p)).count();
        text.paragraphs.drain(..lead);
        if !text.paragraphs.is_empty() {
            scenes.push(text);
        }
    }
    for child in item.children.iter().filter(|c| c.include_in_compile) {
        collect_scenes(project, child, scenes)?;
    }
    Ok(())
}

fn is_blank(p: &Paragraph) -> bool {
    p.runs.iter().all(|r| r.text.trim().is_empty())
}

/// 1 → "One", 42 → "Forty-Two"; past ninety-nine, digits.
pub fn number_words(n: usize) -> String {
    const ONES: [&str; 20] = [
        "Zero",
        "One",
        "Two",
        "Three",
        "Four",
        "Five",
        "Six",
        "Seven",
        "Eight",
        "Nine",
        "Ten",
        "Eleven",
        "Twelve",
        "Thirteen",
        "Fourteen",
        "Fifteen",
        "Sixteen",
        "Seventeen",
        "Eighteen",
        "Nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety",
    ];
    match n {
        0..=19 => ONES[n].to_string(),
        20..=99 if n.is_multiple_of(10) => TENS[n / 10].to_string(),
        20..=99 => format!("{}-{}", TENS[n / 10], ONES[n % 10]),
        _ => n.to_string(),
    }
}

pub fn word_count(sections: &[Section]) -> usize {
    sections
        .iter()
        .flat_map(|s| &s.scenes)
        .map(|t| rtf::word_count(&t.plain_text()))
        .sum()
}

// ------------------------------------------------------------- compiling

pub fn compile(project: &Project, opts: &Options, format: Format) -> Result<Vec<u8>> {
    let sections = gather(project, opts)?;
    Ok(match format {
        Format::Docx => docx(&sections, opts)?,
        Format::Epub => epub(&sections, opts)?,
        Format::Markdown => markdown(&sections, opts).into_bytes(),
        Format::PlainText => plain_text(&sections, opts).into_bytes(),
        Format::Rtf => rtf_file(&sections, opts).into_bytes(),
    })
}

/// Paragraph formatting of manuscript body text.
fn manuscript_para() -> ParaStyle {
    ParaStyle {
        first_indent: 720,
        line_spacing: LineSpacing::Multiple(2.0),
        ..ParaStyle::default()
    }
}

const MANUSCRIPT_FONT: &str = "Times New Roman";

/// A scene reset to manuscript format: one font and size, double spaced,
/// ragged right with half-inch indents. Emphasis (bold, italic, underline,
/// strikethrough, super/subscript) and centered lines survive; color doesn't.
/// Blank spacer paragraphs go: double spacing and indents already set
/// paragraphs apart.
fn manuscript_text(text: &RichText) -> RichText {
    let paragraphs = text
        .paragraphs
        .iter()
        .filter(|p| !is_blank(p))
        .map(|p| {
            let mut style = manuscript_para();
            if matches!(p.style.align, Align::Center | Align::Right) {
                style.align = p.style.align;
                style.first_indent = 0;
            }
            let runs = p
                .runs
                .iter()
                .map(|r| Run {
                    text: r.text.clone(),
                    style: CharStyle {
                        font: Some(MANUSCRIPT_FONT.to_string()),
                        size: 24,
                        color: None,
                        highlight: None,
                        ..r.style.clone()
                    },
                })
                .collect();
            Paragraph { style, runs }
        })
        .collect();
    RichText {
        paragraphs,
        lossy: Vec::new(),
    }
}

/// The scene as it will be written: manuscript-formatted or untouched.
fn scene_text(text: &RichText, opts: &Options) -> RichText {
    if opts.manuscript {
        manuscript_text(text)
    } else {
        text.clone()
    }
}

/// "about 12,300 words": manuscripts give the length to the nearest hundred.
fn about_words(n: usize) -> String {
    let rounded = if n == 0 {
        0
    } else {
        ((n + 50) / 100).max(1) * 100
    };
    let digits = rounded.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("about {grouped} words")
}

fn surname(author: &str) -> &str {
    author.split_whitespace().last().unwrap_or("")
}

// ------------------------------------------------------------------ DOCX

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn docx(sections: &[Section], opts: &Options) -> Result<Vec<u8>> {
    let mut zip = ZipWriter::new(Vec::new());
    let header_type = if opts.manuscript {
        "<Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>"
    } else {
        ""
    };
    let content_types = format!(
        "{XML_DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>\
{header_type}\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
</Types>"
    );
    zip.add("[Content_Types].xml", content_types.as_bytes())?;

    let rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>"
    );
    zip.add("_rels/.rels", rels.as_bytes())?;

    let now = utc_now();
    let core = format!(
        "{XML_DECL}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>{}</dc:creator>\
<dcterms:created xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:created>\
<dcterms:modified xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:modified>\
</cp:coreProperties>",
        escape(&opts.title),
        escape(&opts.author)
    );
    zip.add("docProps/core.xml", core.as_bytes())?;

    let header_rel = if opts.manuscript {
        "<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/header\" Target=\"header1.xml\"/>"
    } else {
        ""
    };
    let doc_rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings\" Target=\"settings.xml\"/>\
{header_rel}</Relationships>"
    );
    zip.add("word/_rels/document.xml.rels", doc_rels.as_bytes())?;

    let settings = format!(
        "{XML_DECL}<w:settings xmlns:w=\"{W_NS}\"><w:defaultTabStop w:val=\"720\"/>\
<w:compat><w:compatSetting w:name=\"compatibilityMode\" w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/></w:compat>\
</w:settings>"
    );
    zip.add("word/settings.xml", settings.as_bytes())?;
    zip.add("word/styles.xml", docx_styles(opts).as_bytes())?;
    if opts.manuscript {
        zip.add("word/header1.xml", docx_header(opts).as_bytes())?;
    }
    zip.add(
        "word/document.xml",
        docx_document(sections, opts).as_bytes(),
    )?;
    Ok(zip.finish()?)
}

/// Normal carries only the font, so every paragraph's own formatting (or
/// the manuscript's, applied to the model) is what shows. Heading 1 makes
/// chapters show up in Word's navigation pane.
fn docx_styles(opts: &Options) -> String {
    // A third of the way down the 9" text block.
    let heading = if opts.manuscript {
        "<w:pPr><w:keepNext/><w:spacing w:before=\"4320\" w:after=\"480\" w:line=\"480\" w:lineRule=\"auto\"/>\
<w:jc w:val=\"center\"/><w:outlineLvl w:val=\"0\"/></w:pPr>"
    } else {
        "<w:pPr><w:keepNext/><w:spacing w:before=\"1440\" w:after=\"480\"/><w:jc w:val=\"center\"/><w:outlineLvl w:val=\"0\"/></w:pPr>\
<w:rPr><w:b/><w:bCs/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/></w:rPr>"
    };
    format!(
        "{XML_DECL}<w:styles xmlns:w=\"{W_NS}\">\
<w:docDefaults><w:rPrDefault><w:rPr>\
<w:rFonts w:ascii=\"{MANUSCRIPT_FONT}\" w:hAnsi=\"{MANUSCRIPT_FONT}\" w:eastAsia=\"{MANUSCRIPT_FONT}\" w:cs=\"{MANUSCRIPT_FONT}\"/>\
<w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/><w:lang w:val=\"en-US\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr/></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/>\
<w:next w:val=\"Normal\"/><w:qFormat/>{heading}</w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:jc w:val=\"center\"/></w:pPr></w:style>\
</w:styles>"
    )
}

/// "Surname / TITLE / 7", right-aligned. The title page has none
/// (`w:titlePg` with no first-page header).
fn docx_header(opts: &Options) -> String {
    let mut label = String::new();
    for part in [surname(&opts.author).to_string(), opts.title.to_uppercase()] {
        if !part.is_empty() {
            label.push_str(&part);
            label.push_str(" / ");
        }
    }
    format!(
        "{XML_DECL}<w:hdr xmlns:w=\"{W_NS}\"><w:p><w:pPr><w:jc w:val=\"right\"/></w:pPr>\
<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r>\
<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
</w:p></w:hdr>",
        escape(&label)
    )
}

fn docx_document(sections: &[Section], opts: &Options) -> String {
    let mut body = String::new();
    // Every chapter starts a page, except a first chapter with nothing before it.
    let mut page_break = false;
    if opts.manuscript {
        docx_title_page(&mut body, sections, opts);
        page_break = true;
    }
    let separator = centered_style(opts);
    for section in sections {
        if let Some(heading) = &section.heading {
            let ppr = docx_ppr(&ParaStyle::default(), Some("Heading1"), page_break);
            body.push_str("<w:p>");
            body.push_str(&ppr);
            docx_run(&mut body, heading, "");
            body.push_str("</w:p>");
            page_break = false;
        }
        for (i, scene) in section.scenes.iter().enumerate() {
            if i > 0 {
                body.push_str("<w:p>");
                body.push_str(&docx_ppr(&separator, None, false));
                docx_run(
                    &mut body,
                    &opts.scene_separator,
                    &docx_rpr(&body_char(opts, false)),
                );
                body.push_str("</w:p>");
            }
            for p in &scene_text(scene, opts).paragraphs {
                body.push_str("<w:p>");
                body.push_str(&docx_ppr(&p.style, None, page_break));
                page_break = false;
                for r in &p.runs {
                    docx_run(&mut body, &r.text, &docx_rpr(&r.style));
                }
                body.push_str("</w:p>");
            }
        }
        // A heading-less, empty chapter still leaves its page.
        if page_break {
            body.push_str("<w:p>");
            body.push_str(&docx_ppr(&ParaStyle::default(), None, true));
            body.push_str("</w:p>");
        }
        page_break = true;
    }

    let mut sect = String::from("<w:sectPr>");
    if opts.manuscript {
        sect.push_str("<w:headerReference w:type=\"default\" r:id=\"rId3\"/>");
    }
    // US Letter, 1" margins.
    sect.push_str(
        "<w:pgSz w:w=\"12240\" w:h=\"15840\"/>\
<w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>",
    );
    if opts.manuscript {
        sect.push_str("<w:titlePg/>");
    }
    sect.push_str("</w:sectPr>");
    format!(
        "{XML_DECL}<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\"><w:body>{body}{sect}</w:body></w:document>"
    )
}

/// Name top left, word count top right, title and byline centered halfway
/// down, single spaced; the first chapter's page break follows.
fn docx_title_page(body: &mut String, sections: &[Section], opts: &Options) {
    let rpr = docx_rpr(&body_char(opts, false));
    body.push_str(
        "<w:p><w:pPr><w:tabs><w:tab w:val=\"right\" w:pos=\"9360\"/></w:tabs>\
<w:spacing w:before=\"0\" w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>",
    );
    docx_run(
        body,
        &format!("{}\t{}", opts.author, about_words(word_count(sections))),
        &rpr,
    );
    body.push_str("</w:p>");
    body.push_str(
        "<w:p><w:pPr><w:pStyle w:val=\"Title\"/><w:spacing w:before=\"5760\" w:after=\"0\" w:line=\"480\" w:lineRule=\"auto\"/>\
<w:jc w:val=\"center\"/></w:pPr>",
    );
    docx_run(body, &opts.title, &rpr);
    body.push_str("</w:p>");
    if !opts.author.is_empty() {
        body.push_str(
            "<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"480\" w:lineRule=\"auto\"/>\
<w:jc w:val=\"center\"/></w:pPr>",
        );
        docx_run(body, &format!("by {}", opts.author), &rpr);
        body.push_str("</w:p>");
    }
}

/// A centered line omaquill adds itself (separators, headings in RTF),
/// spaced like the body around it.
fn centered_style(opts: &Options) -> ParaStyle {
    let base = if opts.manuscript {
        manuscript_para()
    } else {
        ParaStyle::default()
    };
    ParaStyle {
        align: Align::Center,
        first_indent: 0,
        ..base
    }
}

/// The character style of text omaquill adds itself (separators, the title
/// page): the manuscript font, or the document default.
fn body_char(opts: &Options, bold: bool) -> CharStyle {
    CharStyle {
        font: opts.manuscript.then(|| MANUSCRIPT_FONT.to_string()),
        bold,
        ..CharStyle::default()
    }
}

/// `w:pPr`, children in the order the schema requires.
fn docx_ppr(s: &ParaStyle, style_id: Option<&str>, page_break: bool) -> String {
    let mut out = String::from("<w:pPr>");
    if let Some(id) = style_id {
        let _ = write!(out, "<w:pStyle w:val=\"{id}\"/>");
    }
    if page_break {
        out.push_str("<w:pageBreakBefore/>");
    }
    let (line, rule) = match s.line_spacing {
        LineSpacing::Single => (None, "auto"),
        // w:line counts 240ths of a line for "auto".
        LineSpacing::Multiple(m) => (Some((m * 240.0).round() as i32), "auto"),
        LineSpacing::AtLeast(t) => (Some(t), "atLeast"),
        LineSpacing::Exactly(t) => (Some(t), "exact"),
    };
    if s.space_before != 0 || s.space_after != 0 || line.is_some() {
        let _ = write!(
            out,
            "<w:spacing w:before=\"{}\" w:after=\"{}\"",
            s.space_before, s.space_after
        );
        if let Some(line) = line {
            let _ = write!(out, " w:line=\"{line}\" w:lineRule=\"{rule}\"");
        }
        out.push_str("/>");
    }
    if s.left_indent != 0 || s.right_indent != 0 || s.first_indent != 0 {
        let _ = write!(
            out,
            "<w:ind w:left=\"{}\" w:right=\"{}\"",
            s.left_indent, s.right_indent
        );
        if s.first_indent < 0 {
            let _ = write!(out, " w:hanging=\"{}\"", -s.first_indent);
        } else {
            let _ = write!(out, " w:firstLine=\"{}\"", s.first_indent);
        }
        out.push_str("/>");
    }
    let jc = match s.align {
        Align::Natural => None,
        Align::Left => Some("left"),
        Align::Center => Some("center"),
        Align::Right => Some("right"),
        Align::Justify => Some("both"),
    };
    if let Some(jc) = jc {
        let _ = write!(out, "<w:jc w:val=\"{jc}\"/>");
    }
    out.push_str("</w:pPr>");
    out
}

/// `w:rPr`, children in schema order. Highlights take any color, so they
/// go in as shading rather than Word's fixed highlight palette.
fn docx_rpr(s: &CharStyle) -> String {
    let mut out = String::from("<w:rPr>");
    if let Some(font) = &s.font {
        let f = escape(font);
        let _ = write!(
            out,
            "<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:eastAsia=\"{f}\" w:cs=\"{f}\"/>"
        );
    }
    if s.bold {
        out.push_str("<w:b/><w:bCs/>");
    }
    if s.italic {
        out.push_str("<w:i/><w:iCs/>");
    }
    if s.strike {
        out.push_str("<w:strike/>");
    }
    if let Some((r, g, b)) = s.color {
        let _ = write!(out, "<w:color w:val=\"{r:02X}{g:02X}{b:02X}\"/>");
    }
    let _ = write!(out, "<w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/>", s.size);
    if s.underline {
        out.push_str("<w:u w:val=\"single\"/>");
    }
    if let Some((r, g, b)) = s.highlight {
        let _ = write!(
            out,
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{r:02X}{g:02X}{b:02X}\"/>"
        );
    }
    match s.script {
        Script::Normal => {}
        Script::Super => out.push_str("<w:vertAlign w:val=\"superscript\"/>"),
        Script::Sub => out.push_str("<w:vertAlign w:val=\"subscript\"/>"),
    }
    out.push_str("</w:rPr>");
    out
}

/// One run; tabs and soft line breaks become their own elements.
fn docx_run(out: &mut String, text: &str, rpr: &str) {
    out.push_str("<w:r>");
    out.push_str(rpr);
    let mut piece = String::new();
    let flush = |out: &mut String, piece: &mut String| {
        if !piece.is_empty() {
            let _ = write!(out, "<w:t xml:space=\"preserve\">{}</w:t>", escape(piece));
            piece.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\t' => {
                flush(out, &mut piece);
                out.push_str("<w:tab/>");
            }
            '\u{2028}' | '\n' => {
                flush(out, &mut piece);
                out.push_str("<w:br/>");
            }
            c => piece.push(c),
        }
    }
    flush(out, &mut piece);
    out.push_str("</w:r>");
}

// ------------------------------------------------------------------ EPUB

const XHTML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"en\" lang=\"en\">\n";

const EPUB_CSS: &str = "body { margin: 0 5%; }
h1 { text-align: center; font-weight: normal; margin: 3em 0 2em; }
p { margin: 0; text-indent: 1.5em; }
p.first, p.center, p.right, p.sep { text-indent: 0; }
p.center, p.sep { text-align: center; }
p.right { text-align: right; }
p.sep { margin: 1em 0; }
.titlepage { text-align: center; margin-top: 30%; }
.titlepage h1 { font-size: 2em; margin: 0 0 1em; }
.titlepage p { text-indent: 0; }
";

fn epub(sections: &[Section], opts: &Options) -> Result<Vec<u8>> {
    let title = if opts.title.trim().is_empty() {
        "Untitled"
    } else {
        &opts.title
    };
    let mut zip = ZipWriter::new(Vec::new());
    // The spec wants this first and stored, so readers can sniff it.
    zip.add("mimetype", b"application/epub+zip")?;
    zip.add(
        "META-INF/container.xml",
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\
<rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles>\
</container>",
    )?;
    zip.add("OEBPS/style.css", EPUB_CSS.as_bytes())?;

    let chapter_label = |i: usize, s: &Section| {
        s.heading
            .clone()
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| format!("Chapter {}", number_words(i + 1)))
    };

    let mut title_page = String::from(XHTML_HEAD);
    let _ = write!(
        title_page,
        "<head><meta charset=\"utf-8\"/><title>{t}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
<body><section class=\"titlepage\" epub:type=\"titlepage\"><h1>{t}</h1>",
        t = escape(title)
    );
    if !opts.author.is_empty() {
        let _ = write!(title_page, "<p>by {}</p>", escape(&opts.author));
    }
    title_page.push_str("</section></body>\n</html>\n");
    zip.add("OEBPS/title.xhtml", title_page.as_bytes())?;

    let mut nav = String::from(XHTML_HEAD);
    nav.push_str(
        "<head><meta charset=\"utf-8\"/><title>Contents</title></head>\n<body><nav epub:type=\"toc\" id=\"toc\">\
<h1>Contents</h1><ol><li><a href=\"title.xhtml\">Title Page</a></li>",
    );
    let mut manifest = String::new();
    let mut spine = String::from("<itemref idref=\"title\"/>");
    for (i, section) in sections.iter().enumerate() {
        let n = i + 1;
        let label = escape(&chapter_label(i, section));
        let _ = write!(nav, "<li><a href=\"chapter-{n}.xhtml\">{label}</a></li>");
        let _ = write!(
            manifest,
            "<item id=\"ch{n}\" href=\"chapter-{n}.xhtml\" media-type=\"application/xhtml+xml\"/>"
        );
        let _ = write!(spine, "<itemref idref=\"ch{n}\"/>");
        let page = epub_chapter(section, &label, opts);
        zip.add(&format!("OEBPS/chapter-{n}.xhtml"), page.as_bytes())?;
    }
    nav.push_str("</ol></nav></body>\n</html>\n");
    zip.add("OEBPS/nav.xhtml", nav.as_bytes())?;

    let creator = if opts.author.is_empty() {
        String::new()
    } else {
        format!("<dc:creator>{}</dc:creator>", escape(&opts.author))
    };
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\" xml:lang=\"en\">\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
<dc:identifier id=\"bookid\">urn:uuid:{uuid}</dc:identifier><dc:title>{title}</dc:title>{creator}\
<dc:language>en</dc:language><meta property=\"dcterms:modified\">{modified}</meta></metadata>\
<manifest><item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\
<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\
<item id=\"title\" href=\"title.xhtml\" media-type=\"application/xhtml+xml\"/>{manifest}</manifest>\
<spine>{spine}</spine></package>",
        uuid = book_uuid(opts),
        title = escape(title),
        modified = utc_now(),
    );
    zip.add("OEBPS/content.opf", opf.as_bytes())?;
    Ok(zip.finish()?)
}

/// Stable across recompiles of the same book, so a reader treats a new
/// compile as an update rather than a second copy.
fn book_uuid(opts: &Options) -> String {
    let mut b =
        crate::sha1::digest(format!("omaquill\0{}\0{}", opts.title, opts.author).as_bytes());
    b[6] = (b[6] & 0x0F) | 0x50;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b[..16].iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// A chapter page. Ebooks leave fonts, sizes and indents to the reader, so
/// only emphasis, color and alignment carry over.
fn epub_chapter(section: &Section, label: &str, opts: &Options) -> String {
    let mut out = String::from(XHTML_HEAD);
    let _ = write!(
        out,
        "<head><meta charset=\"utf-8\"/><title>{label}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
<body><section epub:type=\"chapter\">\n"
    );
    if let Some(h) = &section.heading {
        let _ = writeln!(out, "<h1>{}</h1>", escape(h));
    }
    for (i, scene) in section.scenes.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(
                out,
                "<p class=\"sep\">{}</p>",
                escape(&opts.scene_separator)
            );
        }
        let mut first = true;
        for p in scene.paragraphs.iter().filter(|p| !is_blank(p)) {
            let class = match p.style.align {
                Align::Center => Some("center"),
                Align::Right => Some("right"),
                _ if first => Some("first"),
                _ => None,
            };
            first = false;
            match class {
                Some(c) => {
                    let _ = write!(out, "<p class=\"{c}\">");
                }
                None => out.push_str("<p>"),
            }
            for r in &p.runs {
                html_run(&mut out, r);
            }
            out.push_str("</p>\n");
        }
    }
    out.push_str("</section></body>\n</html>\n");
    out
}

fn html_run(out: &mut String, r: &Run) {
    let s = &r.style;
    let color = s.color.filter(|&c| c != (0, 0, 0));
    let mut close = Vec::new();
    if let Some((red, g, b)) = color {
        let _ = write!(out, "<span style=\"color: #{red:02x}{g:02x}{b:02x}\">");
        close.push("</span>");
    }
    for (on, open, end) in [
        (s.bold, "<strong>", "</strong>"),
        (s.italic, "<em>", "</em>"),
        (s.underline, "<u>", "</u>"),
        (s.strike, "<s>", "</s>"),
        (s.script == Script::Super, "<sup>", "</sup>"),
        (s.script == Script::Sub, "<sub>", "</sub>"),
    ] {
        if on {
            out.push_str(open);
            close.push(end);
        }
    }
    let text = escape(&r.text.replace('\t', " "));
    out.push_str(&text.replace(['\u{2028}', '\n'], "<br/>"));
    for end in close.iter().rev() {
        out.push_str(end);
    }
}

// -------------------------------------------------------------- Markdown

fn markdown(sections: &[Section], opts: &Options) -> String {
    let mut out = String::new();
    // Front matter, as Pandoc and most static-site tools read it.
    let yaml = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let _ = writeln!(out, "---\ntitle: \"{}\"", yaml(&opts.title));
    if !opts.author.is_empty() {
        let _ = writeln!(out, "author: \"{}\"", yaml(&opts.author));
    }
    out.push_str("---\n");
    for section in sections {
        if let Some(h) = &section.heading {
            let _ = write!(out, "\n# {}\n", md_escape(h, true));
        }
        for (i, scene) in section.scenes.iter().enumerate() {
            if i > 0 {
                let _ = write!(out, "\n{}\n", md_escape(&opts.scene_separator, true));
            }
            for p in scene.paragraphs.iter().filter(|p| !is_blank(p)) {
                let _ = write!(out, "\n{}\n", md_paragraph(p));
            }
        }
    }
    out
}

/// Emphasis as a stack of open markers: markers close innermost first, and
/// whitespace stays outside them, since `* word*` isn't emphasis.
fn md_paragraph(p: &Paragraph) -> String {
    let mut out = String::new();
    let mut open: Vec<&str> = Vec::new();
    let close_from = |out: &mut String, open: &mut Vec<&str>, from: usize| {
        let trimmed = out.trim_end().len();
        let ws = out.split_off(trimmed);
        for m in open.drain(from..).rev() {
            out.push_str(m);
        }
        out.push_str(&ws);
    };
    for r in &p.runs {
        let text = md_escape(&r.text.replace('\t', " "), out.trim().is_empty())
            .replace(['\u{2028}', '\n'], "\\\n");
        if text.trim().is_empty() {
            out.push_str(&text);
            continue;
        }
        let want: Vec<&str> = [
            ("**", r.style.bold),
            ("*", r.style.italic),
            ("~~", r.style.strike),
        ]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(m, _)| m)
        .collect();
        if let Some(pos) = open.iter().position(|m| !want.contains(m)) {
            close_from(&mut out, &mut open, pos);
        }
        let body = text.trim_start();
        out.push_str(&text[..text.len() - body.len()]);
        for m in want {
            if !open.contains(&m) {
                out.push_str(m);
                open.push(m);
            }
        }
        out.push_str(body);
    }
    close_from(&mut out, &mut open, 0);
    // Leading spaces would make an indented code block.
    out.trim().to_string()
}

/// Backslash-escapes what Markdown would read as syntax. `line_start`
/// also covers what only means something at the start of a line: `#`
/// headings, `>` quotes, `-`/`+` list items and `1.` numbered ones.
fn md_escape(s: &str, line_start: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let s_trim = if line_start { s.trim_start() } else { s };
    if line_start {
        let first = s_trim.chars().next();
        if matches!(first, Some('#' | '>' | '-' | '+' | '=')) {
            out.push('\\');
        } else if let Some(digits) = s_trim.find(|c: char| !c.is_ascii_digit())
            && digits > 0
            && matches!(s_trim[digits..].chars().next(), Some('.' | ')'))
        {
            out.push_str(&s_trim[..digits]);
            out.push('\\');
            out.push_str(&md_escape(&s_trim[digits..], false));
            return out;
        }
    }
    for c in s_trim.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '~' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------ plain text

fn plain_text(sections: &[Section], opts: &Options) -> String {
    let mut out = String::new();
    out.push_str(&opts.title);
    out.push('\n');
    if !opts.author.is_empty() {
        let _ = writeln!(out, "by {}", opts.author);
    }
    for section in sections {
        out.push_str("\n\n");
        if let Some(h) = &section.heading {
            let _ = writeln!(out, "{h}\n");
        }
        let mut first = true;
        for (i, scene) in section.scenes.iter().enumerate() {
            if i > 0 {
                let _ = writeln!(out, "\n{}", opts.scene_separator);
            }
            for p in scene.paragraphs.iter().filter(|p| !is_blank(p)) {
                if !first {
                    out.push('\n');
                }
                first = false;
                let _ = writeln!(out, "{}", p.text().replace('\u{2028}', "\n").trim_end());
            }
        }
    }
    out
}

// ------------------------------------------------------------------- RTF

/// One RichText for the whole book, written by [`rtf::write`]. RTF from
/// here has no page breaks: chapters are set apart by a blank line and a
/// centered, bold heading.
fn rtf_file(sections: &[Section], opts: &Options) -> String {
    let mut doc = RichText::default();
    let centered = |text: &str, bold: bool| Paragraph {
        style: centered_style(opts),
        runs: vec![Run {
            text: text.to_string(),
            style: body_char(opts, bold),
        }],
    };
    doc.paragraphs.push(centered(&opts.title, true));
    if !opts.author.is_empty() {
        doc.paragraphs
            .push(centered(&format!("by {}", opts.author), false));
    }
    for section in sections {
        doc.paragraphs.push(centered("", false));
        if let Some(h) = &section.heading {
            doc.paragraphs.push(centered(h, true));
            doc.paragraphs.push(centered("", false));
        }
        for (i, scene) in section.scenes.iter().enumerate() {
            if i > 0 {
                doc.paragraphs.push(centered(&opts.scene_separator, false));
            }
            doc.paragraphs.extend(scene_text(scene, opts).paragraphs);
        }
    }
    rtf::write(&doc)
}

// ------------------------------------------------------------------ time

/// Now in UTC as W3CDTF (`2026-09-28T19:05:02Z`), which both DOCX core
/// properties and EPUB's `dcterms:modified` want.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_in_words() {
        assert_eq!(number_words(1), "One");
        assert_eq!(number_words(13), "Thirteen");
        assert_eq!(number_words(20), "Twenty");
        assert_eq!(number_words(42), "Forty-Two");
        assert_eq!(number_words(99), "Ninety-Nine");
        assert_eq!(number_words(100), "100");
    }

    #[test]
    fn word_counts_round_to_hundreds() {
        assert_eq!(about_words(0), "about 0 words");
        assert_eq!(about_words(12), "about 100 words");
        assert_eq!(about_words(149), "about 100 words");
        assert_eq!(about_words(150), "about 200 words");
        assert_eq!(about_words(81_234), "about 81,200 words");
    }

    #[test]
    fn markdown_emphasis_keeps_spaces_outside() {
        let run = |text: &str, bold: bool, italic: bool| Run {
            text: text.to_string(),
            style: CharStyle {
                bold,
                italic,
                ..CharStyle::default()
            },
        };
        let p = Paragraph {
            style: ParaStyle::default(),
            runs: vec![
                run("She said ", false, false),
                run("never ", false, true),
                run("again", true, true),
                run(" and left.", false, false),
            ],
        };
        assert_eq!(md_paragraph(&p), "She said *never **again*** and left.");
    }

    #[test]
    fn markdown_escapes_syntax() {
        assert_eq!(md_escape("# not a heading", true), "\\# not a heading");
        assert_eq!(md_escape("1984. A year", true), "1984\\. A year");
        assert_eq!(md_escape("a *b* <c> [d]", false), "a \\*b\\* \\<c> \\[d\\]");
    }

    #[test]
    fn utc_time_is_w3cdtf() {
        let t = utc_now();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }
}
