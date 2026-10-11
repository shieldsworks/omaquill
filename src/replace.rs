//! Find and replace over [`RichText`].
//!
//! A match is a character range in [`RichText::plain_text`]. The replacement
//! takes the character style of the first character of that match. `render`
//! returns `None` when nothing would change, and that is the only signal
//! that skips `set_text` and `set_notes`.

use crate::project::{self, Item, Kind, Project};
use crate::rtf::{CharStyle, ParaStyle, Paragraph, RichText, Run};
use regex::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const MAX_PATTERN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    Any,
    Whole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Literal,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub case: Case,
    pub word: Word,
    pub mode: Mode,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            case: Case::Insensitive,
            word: Word::Any,
            mode: Mode::Literal,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QueryError {
    Empty,
    TooLong,
    Pattern(String),
    Replacement(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Empty => write!(f, "Enter text to find"),
            QueryError::TooLong => write!(f, "That search is longer than {MAX_PATTERN} characters"),
            QueryError::Pattern(m) | QueryError::Replacement(m) => write!(f, "{m}"),
        }
    }
}

/// A compiled search. `regex::Regex` stays inside this type.
#[derive(Debug, Clone)]
pub struct Query {
    needle: Regex,
    template: Template,
    word: Word,
}

#[derive(Debug, Clone)]
struct Template {
    pieces: Vec<Piece>,
}

#[derive(Debug, Clone)]
enum Piece {
    Lit(String),
    Group(usize),
}

/// Compiles `pattern`. An empty pattern, a pattern longer than
/// [`MAX_PATTERN`], a regex the crate rejects, or a replacement that names
/// a missing group comes back as `Err`. Literal mode stores the replacement
/// as typed, so `$1` is four characters.
pub fn query(pattern: &str, replacement: &str, opts: Options) -> Result<Query, QueryError> {
    if pattern.is_empty() {
        return Err(QueryError::Empty);
    }
    if pattern.chars().count() > MAX_PATTERN {
        return Err(QueryError::TooLong);
    }
    let source = if opts.mode == Mode::Literal {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let needle = regex::RegexBuilder::new(&source)
        .case_insensitive(opts.case == Case::Insensitive)
        .multi_line(true)
        .unicode(true)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|e| QueryError::Pattern(e.to_string()))?;
    let template = if opts.mode == Mode::Literal {
        Template {
            pieces: vec![Piece::Lit(replacement.to_string())],
        }
    } else {
        parse_template(replacement, &needle)?
    };
    Ok(Query {
        needle,
        template,
        word: opts.word,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor(usize);

impl Cursor {
    pub fn new(at: usize) -> Self {
        Cursor(at)
    }

    pub fn at(self) -> usize {
        self.0
    }
}

/// `end > start`, in Unicode scalar values of [`RichText::plain_text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharRange {
    start: usize,
    end: usize,
}

impl CharRange {
    pub fn try_bounds(start: usize, end: usize) -> Option<Self> {
        (end > start).then_some(CharRange { start, end })
    }

    pub fn start(self) -> usize {
        self.start
    }

    pub fn end(self) -> usize {
        self.end
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Caret(Cursor),
    Selection(CharRange),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    range: CharRange,
    matched: String,
}

impl Hit {
    pub fn range(&self) -> CharRange {
        self.range
    }

    pub fn matched(&self) -> &str {
        &self.matched
    }
}

#[derive(Debug)]
pub enum Step {
    None,
    Found(Hit),
    Replace(EditPlan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    range: CharRange,
    matched: String,
    replacement: String,
    char_style: CharStyle,
    para_style: ParaStyle,
}

impl Edit {
    pub fn range(&self) -> CharRange {
        self.range
    }

    pub fn matched(&self) -> &str {
        &self.matched
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn char_style(&self) -> &CharStyle {
        &self.char_style
    }

    pub fn para_style(&self) -> &ParaStyle {
        &self.para_style
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditPlan {
    edits: Vec<Edit>,
}

impl EditPlan {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }
}

pub fn find_next(text: &RichText, query: &Query, place: Place) -> Option<Hit> {
    let found = find_in(text, query);
    let hits = hits_of(&found);
    if hits.is_empty() {
        return None;
    }
    let from = match place {
        Place::Caret(c) => c.at(),
        Place::Selection(r) => r.end(),
    };
    hits.into_iter()
        .find(|h| h.range.start >= from)
        .or_else(|| hits_of(&found).into_iter().next())
}

pub fn step(text: &RichText, query: &Query, place: Place) -> Step {
    let found = find_in(text, query);
    if let Place::Selection(range) = place
        && let Some(hit) = found.iter().find(|f| f.range == range)
    {
        if hit.replacement != hit.matched {
            return Step::Replace(EditPlan {
                edits: vec![edit_of(hit)],
            });
        }
        return found
            .iter()
            .find(|f| f.range.start >= range.end())
            .or(found.first())
            .filter(|f| f.range != range)
            .map(hit_of)
            .map(Step::Found)
            .unwrap_or(Step::None);
    }
    let from = match place {
        Place::Caret(c) => c.at(),
        Place::Selection(r) => r.start(),
    };
    found
        .iter()
        .find(|f| f.range.start >= from)
        .or(found.first())
        .map(hit_of)
        .map(Step::Found)
        .unwrap_or(Step::None)
}

pub fn plan_all(text: &RichText, query: &Query) -> EditPlan {
    EditPlan {
        edits: edits_of(&find_in(text, query)),
    }
}

/// `None` when `plan` is empty. `Some` copies `text.lossy` onto the result.
pub fn render(text: &RichText, plan: &EditPlan) -> Option<RichText> {
    if plan.edits.is_empty() {
        return None;
    }
    let atoms = atoms_of(text);
    let total = atoms.last().map(Atom::end).unwrap_or(0);
    let mut paras = vec![Paragraph {
        style: text
            .paragraphs
            .first()
            .map(|p| p.style.clone())
            .unwrap_or_default(),
        runs: Vec::new(),
    }];
    let mut i = 0;
    let mut edit_ix = 0;
    while i < total {
        if let Some(edit) = plan.edits.get(edit_ix)
            && (edit.range.start < i || edit.range.end > total)
        {
            edit_ix += 1;
            continue;
        }
        if let Some(edit) = plan.edits.get(edit_ix)
            && edit.range.start == i
        {
            emit_replacement(&mut paras, edit);
            i = edit.range.end;
            edit_ix += 1;
            continue;
        }
        let limit = plan
            .edits
            .get(edit_ix)
            .map(|e| e.range.start)
            .unwrap_or(total);
        emit_source(&mut paras, &atoms, i, limit);
        if limit <= i {
            break;
        }
        i = limit;
    }
    Some(RichText {
        paragraphs: paras,
        lossy: text.lossy.clone(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Body,
    Notes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocId {
    uuid: String,
    field: Field,
}

impl DocId {
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    pub fn field(&self) -> Field {
        self.field
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchId(u32);

impl MatchId {
    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub before: String,
    pub matched: String,
    pub after: String,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    id: MatchId,
    edit: Edit,
    snippet: Snippet,
}

impl Row {
    pub fn id(&self) -> MatchId {
        self.id
    }

    pub fn edit(&self) -> &Edit {
        &self.edit
    }

    pub fn snippet(&self) -> &Snippet {
        &self.snippet
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    id: DocId,
    title: String,
    path_label: String,
    in_trash: bool,
    /// Features `rtf::write` drops if this field is rewritten.
    lossy: Vec<&'static str>,
    rows: Vec<Row>,
}

impl Group {
    pub fn id(&self) -> &DocId {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn path_label(&self) -> &str {
        &self.path_label
    }

    pub fn in_trash(&self) -> bool {
        self.in_trash
    }

    pub fn lossy(&self) -> &[&'static str] {
        &self.lossy
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    groups: Vec<Group>,
}

impl Preview {
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn ids(&self) -> Vec<MatchId> {
        self.groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.id))
            .collect()
    }
}

pub fn preview(project: &Project, query: &Query) -> Result<Preview, project::Error> {
    let mut groups = Vec::new();
    let mut next = 0u32;
    let items = project.items();
    let mut scan = Scan {
        project,
        query,
        groups: &mut groups,
        next: &mut next,
    };
    walk(&mut scan, &items, "", false)?;
    Ok(Preview { groups })
}

#[derive(Debug)]
pub struct Report {
    pub backup: PathBuf,
    pub written: Vec<DocId>,
    pub skipped: usize,
}

#[derive(Debug)]
pub enum ApplyOutcome {
    NothingToDo,
    Done(Report),
}

#[derive(Debug)]
pub enum ApplyError {
    Backup(project::Error),
    Read(project::Error),
    /// `written` already reached disk. `Project::save` was attempted.
    Write {
        written: Vec<DocId>,
        source: project::Error,
    },
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Backup(e) => write!(f, "Backup failed: {e}"),
            ApplyError::Read(e) => write!(f, "Replace failed: {e}"),
            ApplyError::Write { source, .. } => write!(f, "Replace failed: {source}"),
        }
    }
}

/// Re-reads each chosen field and writes it only when `render` returns
/// `Some`. An empty set returns [`ApplyOutcome::NothingToDo`] and does not
/// zip. A second call with the same preview skips every slice.
pub fn apply(
    project: &mut Project,
    backup_dir: &Path,
    keep: usize,
    preview: &Preview,
    chosen: &[MatchId],
) -> Result<ApplyOutcome, ApplyError> {
    let chosen: HashSet<MatchId> = chosen.iter().copied().collect();
    let mut pending = Vec::new();
    let mut skipped = 0usize;
    for group in &preview.groups {
        let current = read_field(project, group.id()).map_err(ApplyError::Read)?;
        let plain = current.plain_text();
        let mut edits = Vec::new();
        for row in group.rows() {
            if !chosen.contains(&row.id()) {
                continue;
            }
            match plain_slice(&plain, row.edit().range()) {
                Some(slice) if slice == row.edit().matched() => edits.push(row.edit().clone()),
                _ => skipped += 1,
            }
        }
        if let Some(next) = render(&current, &EditPlan { edits }) {
            pending.push((group.id().clone(), next));
        }
    }
    if pending.is_empty() {
        return Ok(ApplyOutcome::NothingToDo);
    }
    let backup = project
        .backup(backup_dir, keep.max(1))
        .map_err(ApplyError::Backup)?;
    let mut written = Vec::new();
    for (id, text) in pending {
        let result = match id.field() {
            Field::Body => project.set_text(id.uuid(), &text),
            Field::Notes => project.set_notes(id.uuid(), &text),
        };
        if let Err(source) = result {
            let _ = project.save();
            return Err(ApplyError::Write { written, source });
        }
        written.push(id);
    }
    if let Err(source) = project.save() {
        return Err(ApplyError::Write { written, source });
    }
    Ok(ApplyOutcome::Done(Report {
        backup,
        written,
        skipped,
    }))
}

struct Found {
    range: CharRange,
    matched: String,
    replacement: String,
    char_style: CharStyle,
    para_style: ParaStyle,
}

fn find_in(text: &RichText, query: &Query) -> Vec<Found> {
    let plain = text.plain_text();
    let chars: Vec<char> = plain.chars().collect();
    let mut byte_at_char = Vec::with_capacity(chars.len() + 1);
    let mut byte = 0usize;
    for ch in &chars {
        byte_at_char.push(byte);
        byte += ch.len_utf8();
    }
    byte_at_char.push(byte);
    let char_at_byte = |b: usize| {
        let index = byte_at_char.partition_point(|start| *start < b);
        byte_at_char
            .get(index)
            .filter(|start| **start == b)
            .map(|_| index)
            .expect("regex match lands on a character boundary")
    };
    let mut out = Vec::new();
    for caps in query.needle.captures_iter(&plain) {
        let whole = caps.get(0).expect("capture 0 is the whole match");
        if whole.range().is_empty() {
            continue;
        }
        let start = char_at_byte(whole.start());
        let end = char_at_byte(whole.end());
        if query.word == Word::Whole && !word_bounded(&chars, start, end) {
            continue;
        }
        let Some(range) = CharRange::try_bounds(start, end) else {
            continue;
        };
        let matched: String = chars[start..end].iter().collect();
        let replacement = expand(&query.template, &caps);
        let (char_style, para_style) = style_at(text, start);
        out.push(Found {
            range,
            matched,
            replacement,
            char_style,
            para_style,
        });
    }
    out
}

fn word_bounded(chars: &[char], start: usize, end: usize) -> bool {
    let before = start == 0 || !in_word(chars[start - 1]);
    let after = end >= chars.len() || !in_word(chars[end]);
    before && after
}

fn in_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '\'' | '\u{2019}' | '-' | '\u{2010}' | '\u{2011}')
}

fn expand(template: &Template, caps: &regex::Captures<'_>) -> String {
    let mut out = String::new();
    for piece in &template.pieces {
        match piece {
            Piece::Lit(s) => out.push_str(s),
            Piece::Group(i) => {
                if let Some(m) = caps.get(*i) {
                    out.push_str(m.as_str());
                }
            }
        }
    }
    out
}

fn parse_template(replacement: &str, needle: &Regex) -> Result<Template, QueryError> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            lit.push(chars[i]);
            i += 1;
            continue;
        }
        flush_lit(&mut pieces, &mut lit);
        i += 1;
        if i >= chars.len() {
            return Err(QueryError::Replacement("A $ has nothing after it".into()));
        }
        if chars[i] == '$' {
            lit.push('$');
            i += 1;
            continue;
        }
        let index = if chars[i] == '{' {
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '}' {
                i += 1;
            }
            if i >= chars.len() {
                return Err(QueryError::Replacement("A ${ has no closing }".into()));
            }
            let name: String = chars[start..i].iter().collect();
            i += 1;
            group_index(needle, &name)?
        } else if chars[i].is_ascii_digit() {
            let mut n = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                let digit = usize::try_from(
                    chars[i]
                        .to_digit(10)
                        .expect("the character was an ascii digit"),
                )
                .expect("a decimal digit fits in usize");
                n = n
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(digit))
                    .ok_or_else(|| QueryError::Replacement(format!("No group ${}", chars[i])))?;
                i += 1;
            }
            if n >= needle.captures_len() {
                return Err(QueryError::Replacement(format!("No group ${n}")));
            }
            n
        } else if is_name_start(chars[i]) {
            let start = i;
            i += 1;
            while i < chars.len() && is_name_cont(chars[i]) {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            group_index(needle, &name)?
        } else {
            return Err(QueryError::Replacement(format!(
                "A $ cannot be followed by {}",
                chars[i]
            )));
        };
        pieces.push(Piece::Group(index));
    }
    flush_lit(&mut pieces, &mut lit);
    Ok(Template { pieces })
}

fn flush_lit(pieces: &mut Vec<Piece>, lit: &mut String) {
    if !lit.is_empty() {
        pieces.push(Piece::Lit(std::mem::take(lit)));
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_cont(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn group_index(needle: &Regex, name: &str) -> Result<usize, QueryError> {
    if let Ok(n) = name.parse::<usize>() {
        if n < needle.captures_len() {
            return Ok(n);
        }
        return Err(QueryError::Replacement(format!("No group ${name}")));
    }
    needle
        .capture_names()
        .position(|n| n == Some(name))
        .ok_or_else(|| QueryError::Replacement(format!("No group ${name}")))
}

fn style_at(text: &RichText, index: usize) -> (CharStyle, ParaStyle) {
    let mut at = 0usize;
    let mut previous = CharStyle::default();
    for (pi, para) in text.paragraphs.iter().enumerate() {
        if pi > 0 {
            if at == index {
                let char_style = para
                    .runs
                    .first()
                    .map(|r| r.style.clone())
                    .unwrap_or(previous);
                return (char_style, para.style.clone());
            }
            at += 1;
        }
        for run in &para.runs {
            let n = run.text.chars().count();
            if index < at + n {
                return (run.style.clone(), para.style.clone());
            }
            at += n;
            if n > 0 {
                previous = run.style.clone();
            }
        }
    }
    (
        previous,
        text.paragraphs
            .last()
            .map(|p| p.style.clone())
            .unwrap_or_default(),
    )
}

enum Atom {
    Chars {
        end: usize,
        text: String,
        style: CharStyle,
    },
    Break {
        at: usize,
        next: ParaStyle,
    },
}

impl Atom {
    fn end(&self) -> usize {
        match self {
            Atom::Chars { end, .. } => *end,
            Atom::Break { at, .. } => *at + 1,
        }
    }
}

fn atoms_of(text: &RichText) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut at = 0usize;
    for (pi, para) in text.paragraphs.iter().enumerate() {
        if pi > 0 {
            atoms.push(Atom::Break {
                at,
                next: para.style.clone(),
            });
            at += 1;
        }
        for run in &para.runs {
            let n = run.text.chars().count();
            if n == 0 {
                continue;
            }
            atoms.push(Atom::Chars {
                end: at + n,
                text: run.text.clone(),
                style: run.style.clone(),
            });
            at += n;
        }
    }
    atoms
}

fn emit_source(paras: &mut Vec<Paragraph>, atoms: &[Atom], from: usize, to: usize) {
    for atom in atoms {
        match atom {
            Atom::Break { at, next } if *at >= from && *at < to => {
                paras.push(Paragraph {
                    style: next.clone(),
                    runs: Vec::new(),
                });
            }
            Atom::Chars { end, text, style } => {
                let start = end - text.chars().count();
                let lo = from.max(start);
                let hi = to.min(*end);
                if lo < hi {
                    let slice: String = text.chars().skip(lo - start).take(hi - lo).collect();
                    push_chars(paras, &slice, style);
                }
            }
            Atom::Break { .. } => {}
        }
    }
}

fn emit_replacement(paras: &mut Vec<Paragraph>, edit: &Edit) {
    let parts: Vec<&str> = edit.replacement.split('\n').collect();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            paras.push(Paragraph {
                style: edit.para_style.clone(),
                runs: Vec::new(),
            });
        }
        push_chars(paras, part, &edit.char_style);
    }
}

fn push_chars(paras: &mut Vec<Paragraph>, text: &str, style: &CharStyle) {
    if text.is_empty() {
        return;
    }
    let para = paras.last_mut().expect("render keeps one open paragraph");
    match para.runs.last_mut() {
        Some(run) if run.style == *style => run.text.push_str(text),
        _ => para.runs.push(Run {
            text: text.to_string(),
            style: style.clone(),
        }),
    }
}

fn hit_of(found: &Found) -> Hit {
    Hit {
        range: found.range,
        matched: found.matched.clone(),
    }
}

fn hits_of(found: &[Found]) -> Vec<Hit> {
    found.iter().map(hit_of).collect()
}

fn edit_of(found: &Found) -> Edit {
    Edit {
        range: found.range,
        matched: found.matched.clone(),
        replacement: found.replacement.clone(),
        char_style: found.char_style.clone(),
        para_style: found.para_style.clone(),
    }
}

fn edits_of(found: &[Found]) -> Vec<Edit> {
    found
        .iter()
        .filter(|f| f.replacement != f.matched)
        .map(edit_of)
        .collect()
}

fn plain_slice(plain: &str, range: CharRange) -> Option<String> {
    let slice: String = plain
        .chars()
        .skip(range.start())
        .take(range.len())
        .collect();
    (slice.chars().count() == range.len()).then_some(slice)
}

fn read_field(project: &Project, id: &DocId) -> Result<RichText, project::Error> {
    match id.field() {
        Field::Body => project.text(id.uuid()),
        Field::Notes => project.notes(id.uuid()),
    }
}

struct Scan<'a> {
    project: &'a Project,
    query: &'a Query,
    groups: &'a mut Vec<Group>,
    next: &'a mut u32,
}

fn walk(
    scan: &mut Scan<'_>,
    items: &[Item],
    path: &str,
    in_trash: bool,
) -> Result<(), project::Error> {
    for item in items {
        let here = in_trash || item.kind == Kind::Trash;
        let label = if path.is_empty() {
            item.title.clone()
        } else {
            format!("{path} / {}", item.title)
        };
        if item.kind.has_text() {
            let text = scan.project.text(&item.uuid)?;
            push_group(scan, item, path, here, Field::Body, &text);
        }
        let notes = scan.project.notes(&item.uuid)?;
        if !notes.plain_text().is_empty() {
            push_group(scan, item, path, here, Field::Notes, &notes);
        }
        walk(scan, &item.children, &label, here)?;
    }
    Ok(())
}

fn push_group(
    scan: &mut Scan<'_>,
    item: &Item,
    path: &str,
    in_trash: bool,
    field: Field,
    text: &RichText,
) {
    let plain = text.plain_text();
    let edits = edits_of(&find_in(text, scan.query));
    if edits.is_empty() {
        return;
    }
    let mut rows = Vec::new();
    for edit in edits {
        let Some(advanced) = scan.next.checked_add(1) else {
            break;
        };
        let id = MatchId(*scan.next);
        *scan.next = advanced;
        let snippet = snippet_of(&plain, edit.range, &edit.replacement);
        rows.push(Row { id, edit, snippet });
    }
    if rows.is_empty() {
        return;
    }
    scan.groups.push(Group {
        id: DocId {
            uuid: item.uuid.clone(),
            field,
        },
        title: item.title.clone(),
        path_label: path.to_string(),
        in_trash,
        lossy: text.lossy.clone(),
        rows,
    });
}

fn snippet_of(plain: &str, range: CharRange, replacement: &str) -> Snippet {
    let chars: Vec<char> = plain.chars().collect();
    let from = range.start().saturating_sub(40);
    let to = (range.end() + 40).min(chars.len());
    let take = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
    let mut before = take(from, range.start());
    let mut after = take(range.end(), to);
    if from > 0 {
        before.insert_str(0, "...");
    }
    if to < chars.len() {
        after.push_str("...");
    }
    Snippet {
        before,
        matched: take(range.start(), range.end()),
        after,
        replacement: replacement.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtf::{Align, CharStyle, ParaStyle, Paragraph, Run};

    fn literal(find: &str, replace: &str, word: Word, case: Case) -> Query {
        query(
            find,
            replace,
            Options {
                case,
                word,
                mode: Mode::Literal,
            },
        )
        .unwrap()
    }

    fn doc(runs: Vec<(&str, bool)>, lossy: Vec<&'static str>) -> RichText {
        RichText {
            paragraphs: vec![Paragraph {
                style: ParaStyle::default(),
                runs: runs
                    .into_iter()
                    .map(|(text, italic)| Run {
                        text: text.to_string(),
                        style: CharStyle {
                            italic,
                            ..CharStyle::default()
                        },
                    })
                    .collect(),
            }],
            lossy,
        }
    }

    #[test]
    fn styled_run_keeps_char_style() {
        let text = doc(
            vec![("before ", false), ("hello", true), (" after", false)],
            vec!["images"],
        );
        let plan = plan_all(&text, &literal("hello", "hi", Word::Any, Case::Sensitive));
        let next = render(&text, &plan).unwrap();
        assert_eq!(next.plain_text(), "before hi after");
        assert_eq!(next.lossy, ["images"]);
        let hi = next.paragraphs[0]
            .runs
            .iter()
            .find(|r| r.text == "hi")
            .unwrap();
        assert!(hi.style.italic);
        assert!(!next.paragraphs[0].runs[0].style.italic);
        let round = crate::rtf::parse(crate::rtf::write(&next).as_bytes());
        assert_eq!(round.plain_text(), "before hi after");
        assert!(
            round.paragraphs[0]
                .runs
                .iter()
                .any(|r| r.text == "hi" && r.style.italic)
        );
    }

    #[test]
    fn whole_word_does_not_match_inside_a_longer_word() {
        let text = RichText::from_plain("the catalog has a cat, and a well-known cat");
        let plan = plan_all(
            &text,
            &literal("cat", "dog", Word::Whole, Case::Insensitive),
        );
        let next = render(&text, &plan).unwrap();
        assert_eq!(
            next.plain_text(),
            "the catalog has a dog, and a well-known dog"
        );
        let well = plan_all(
            &text,
            &literal("well", "good", Word::Whole, Case::Sensitive),
        );
        assert!(well.is_empty());
        assert!(render(&text, &well).is_none());
    }

    #[test]
    fn match_case_is_exact() {
        let text = RichText::from_plain("Sea sea");
        let sensitive = plan_all(&text, &literal("Sea", "Ocean", Word::Any, Case::Sensitive));
        assert_eq!(render(&text, &sensitive).unwrap().plain_text(), "Ocean sea");
        let either = plan_all(
            &text,
            &literal("Sea", "Ocean", Word::Any, Case::Insensitive),
        );
        assert_eq!(render(&text, &either).unwrap().plain_text(), "Ocean Ocean");
    }

    #[test]
    fn regex_groups_reorder_the_match() {
        let text = RichText::from_plain("alpha beta");
        let q = query(
            r"(\w+) (\w+)",
            "$2 $1",
            Options {
                case: Case::Sensitive,
                word: Word::Any,
                mode: Mode::Regex,
            },
        )
        .unwrap();
        let next = render(&text, &plan_all(&text, &q)).unwrap();
        assert_eq!(next.plain_text(), "beta alpha");
    }

    #[test]
    fn missing_group_is_rejected() {
        let regex = Options {
            mode: Mode::Regex,
            ..Options::default()
        };
        assert!(matches!(
            query("a", "$1", regex),
            Err(QueryError::Replacement(_))
        ));
        assert!(query("a", "$0", regex).is_ok());
        let literal = query(
            "a",
            "$1",
            Options {
                mode: Mode::Literal,
                ..Options::default()
            },
        )
        .unwrap();
        let text = RichText::from_plain("a");
        let next = render(&text, &plan_all(&text, &literal)).unwrap();
        assert_eq!(next.plain_text(), "$1");
    }

    #[test]
    fn identity_replacement_is_not_an_edit() {
        let text = RichText::from_plain("cat");
        let plan = plan_all(&text, &literal("cat", "cat", Word::Any, Case::Sensitive));
        assert!(plan.is_empty());
        assert!(render(&text, &plan).is_none());
    }

    #[test]
    fn cross_run_match_takes_the_first_char_style() {
        let text = doc(vec![("alpha ", true), ("beta", false)], vec![]);
        let plan = plan_all(
            &text,
            &literal("alpha beta", "X", Word::Any, Case::Sensitive),
        );
        let next = render(&text, &plan).unwrap();
        assert_eq!(next.paragraphs[0].runs.len(), 1);
        assert_eq!(next.paragraphs[0].runs[0].text, "X");
        assert!(next.paragraphs[0].runs[0].style.italic);
    }

    #[test]
    fn paragraph_break_keeps_the_start_paragraph_style() {
        let mut center = ParaStyle::default();
        center.align = Align::Center;
        let mut left = ParaStyle::default();
        left.align = Align::Left;
        let italic = CharStyle {
            italic: true,
            ..CharStyle::default()
        };
        let text = RichText {
            paragraphs: vec![
                Paragraph {
                    style: center.clone(),
                    runs: vec![
                        Run {
                            text: "Hello ".into(),
                            style: CharStyle::default(),
                        },
                        Run {
                            text: "world".into(),
                            style: italic,
                        },
                    ],
                },
                Paragraph {
                    style: left,
                    runs: vec![Run {
                        text: "Next line".into(),
                        style: CharStyle::default(),
                    }],
                },
            ],
            lossy: vec![],
        };
        let plan = plan_all(
            &text,
            &literal("world\nNext", "there", Word::Any, Case::Sensitive),
        );
        let next = render(&text, &plan).unwrap();
        assert_eq!(next.paragraphs.len(), 1);
        assert_eq!(next.paragraphs[0].style.align, Align::Center);
        assert_eq!(next.plain_text(), "Hello there line");
        assert!(
            next.paragraphs[0]
                .runs
                .iter()
                .any(|r| r.text == "there" && r.style.italic)
        );
        assert!(
            next.paragraphs[0]
                .runs
                .iter()
                .any(|r| r.text == " line" && !r.style.italic)
        );
    }

    #[test]
    fn zero_width_match_is_dropped() {
        let text = RichText::from_plain("banana");
        let q = query(
            "(?=a)",
            "x",
            Options {
                mode: Mode::Regex,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(plan_all(&text, &q).is_empty());
    }

    #[test]
    fn newline_in_the_replacement_copies_the_start_paragraph_style() {
        let mut center = ParaStyle::default();
        center.align = Align::Center;
        let text = RichText {
            paragraphs: vec![Paragraph {
                style: center,
                runs: vec![Run {
                    text: "Hello world".into(),
                    style: CharStyle::default(),
                }],
            }],
            lossy: vec![],
        };
        let plan = plan_all(
            &text,
            &literal("world", "there\nfriend", Word::Any, Case::Sensitive),
        );
        let next = render(&text, &plan).unwrap();
        assert_eq!(next.paragraphs.len(), 2);
        assert_eq!(next.paragraphs[0].style.align, Align::Center);
        assert_eq!(next.paragraphs[1].style.align, Align::Center);
        assert_eq!(next.plain_text(), "Hello there\nfriend");
    }

    #[test]
    fn empty_pattern_is_rejected() {
        assert!(matches!(
            query("", "a", Options::default()),
            Err(QueryError::Empty)
        ));
    }
}
