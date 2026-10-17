//! The gathered manuscript and its options as bytes, for the PDF renderer
//! process (see [`crate::export`]). Both ends are the same build of
//! omaquill, so the format has no versions. [`RichText::lossy`] stays
//! behind: rendering does not read it.

use crate::compile::{Headings, Options, Section};
use crate::rtf::{Align, CharStyle, LineSpacing, ParaStyle, Paragraph, Rgb, RichText, Run, Script};
use std::sync::Arc;

const MAGIC: &[u8] = b"omaquill snapshot\n";

pub fn encode(sections: &[Section], opts: &Options) -> Vec<u8> {
    let mut w = Writer(MAGIC.to_vec());
    let Options {
        title,
        author,
        manuscript,
        headings,
        scene_separator,
    } = opts;
    w.str(title);
    w.str(author);
    w.bool(*manuscript);
    w.u8(match headings {
        Headings::Numbered => 0,
        Headings::Titles => 1,
        Headings::None => 2,
    });
    w.str(scene_separator);
    w.len(sections.len());
    for Section { heading, scenes } in sections {
        w.opt_str(heading.as_deref());
        w.len(scenes.len());
        for RichText {
            paragraphs,
            lossy: _,
        } in scenes
        {
            w.len(paragraphs.len());
            for Paragraph { style, runs } in paragraphs {
                w.para(style);
                w.len(runs.len());
                for Run { text, style } in runs {
                    w.str(text);
                    w.chars(style);
                }
            }
        }
    }
    w.0
}

pub fn decode(bytes: &[u8]) -> Result<(Vec<Section>, Options), String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("the manuscript did not come from omaquill".to_string());
    }
    let title = r.str()?;
    let author = r.str()?;
    let manuscript = r.bool()?;
    let headings = match r.u8()? {
        0 => Headings::Numbered,
        1 => Headings::Titles,
        2 => Headings::None,
        _ => return Err(r.bad()),
    };
    let scene_separator = r.str()?;
    let opts = Options {
        title,
        author,
        manuscript,
        headings,
        scene_separator,
    };
    let mut sections = Vec::new();
    for _ in 0..r.len()? {
        let heading = r.opt_str()?;
        let mut scenes = Vec::new();
        for _ in 0..r.len()? {
            let mut paragraphs = Vec::new();
            for _ in 0..r.len()? {
                let style = r.para()?;
                let mut runs = Vec::new();
                for _ in 0..r.len()? {
                    let text = r.str()?;
                    let style = r.chars()?;
                    runs.push(Run { text, style });
                }
                paragraphs.push(Paragraph { style, runs });
            }
            scenes.push(RichText {
                paragraphs,
                lossy: Vec::new(),
            });
        }
        sections.push(Section { heading, scenes });
    }
    if r.at != bytes.len() {
        return Err(r.bad());
    }
    Ok((sections, opts))
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    /// Counts and byte lengths go as 64 bits, the widest a `usize` is.
    fn len(&mut self, n: usize) {
        self.0.extend_from_slice(&(n as u64).to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }

    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }

    fn rgb(&mut self, c: Option<Rgb>) {
        match c {
            Some((r, g, b)) => self.0.extend_from_slice(&[1, r, g, b]),
            None => self.u8(0),
        }
    }

    fn para(&mut self, style: &ParaStyle) {
        let ParaStyle {
            align,
            first_indent,
            left_indent,
            right_indent,
            space_before,
            space_after,
            line_spacing,
            tabs,
        } = style;
        self.u8(match align {
            Align::Natural => 0,
            Align::Left => 1,
            Align::Center => 2,
            Align::Right => 3,
            Align::Justify => 4,
        });
        for v in [
            first_indent,
            left_indent,
            right_indent,
            space_before,
            space_after,
        ] {
            self.i32(*v);
        }
        match line_spacing {
            LineSpacing::Single => self.u8(0),
            LineSpacing::Multiple(m) => {
                self.u8(1);
                self.u32(m.to_bits());
            }
            LineSpacing::AtLeast(t) => {
                self.u8(2);
                self.i32(*t);
            }
            LineSpacing::Exactly(t) => {
                self.u8(3);
                self.i32(*t);
            }
        }
        self.len(tabs.len());
        for (at, kind) in tabs {
            self.i32(*at);
            self.u32(u32::from(*kind));
        }
    }

    fn chars(&mut self, style: &CharStyle) {
        let CharStyle {
            font,
            size,
            bold,
            italic,
            underline,
            strike,
            color,
            highlight,
            script,
            link,
        } = style;
        self.opt_str(font.as_deref());
        self.u16(*size);
        for v in [bold, italic, underline, strike] {
            self.bool(*v);
        }
        self.rgb(*color);
        self.rgb(*highlight);
        self.u8(match script {
            Script::Normal => 0,
            Script::Super => 1,
            Script::Sub => 2,
        });
        self.opt_str(link.as_deref());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn bad(&self) -> String {
        format!("the manuscript is damaged at byte {}", self.at)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| self.bad())?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.array::<1>()?[0])
    }

    fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.bad()),
        }
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// A count or byte length. Every item takes at least one byte, so a
    /// count past the bytes left is damage, not a reason to allocate.
    fn len(&mut self) -> Result<usize, String> {
        let n = u64::from_le_bytes(self.array()?);
        match usize::try_from(n) {
            Ok(n) if n <= self.bytes.len() - self.at => Ok(n),
            _ => Err(self.bad()),
        }
    }

    fn str(&mut self) -> Result<String, String> {
        let n = self.len()?;
        let at = self.at;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| format!("the manuscript is damaged at byte {at}"))
    }

    fn opt_str(&mut self) -> Result<Option<String>, String> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.str().map(Some),
            _ => Err(self.bad()),
        }
    }

    fn rgb(&mut self) -> Result<Option<Rgb>, String> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let [r, g, b] = self.array()?;
                Ok(Some((r, g, b)))
            }
            _ => Err(self.bad()),
        }
    }

    fn para(&mut self) -> Result<ParaStyle, String> {
        let align = match self.u8()? {
            0 => Align::Natural,
            1 => Align::Left,
            2 => Align::Center,
            3 => Align::Right,
            4 => Align::Justify,
            _ => return Err(self.bad()),
        };
        let first_indent = self.i32()?;
        let left_indent = self.i32()?;
        let right_indent = self.i32()?;
        let space_before = self.i32()?;
        let space_after = self.i32()?;
        let line_spacing = match self.u8()? {
            0 => LineSpacing::Single,
            1 => LineSpacing::Multiple(f32::from_bits(self.u32()?)),
            2 => LineSpacing::AtLeast(self.i32()?),
            3 => LineSpacing::Exactly(self.i32()?),
            _ => return Err(self.bad()),
        };
        let mut tabs = Vec::new();
        for _ in 0..self.len()? {
            let at = self.i32()?;
            let kind = char::from_u32(self.u32()?).ok_or_else(|| self.bad())?;
            tabs.push((at, kind));
        }
        Ok(ParaStyle {
            align,
            first_indent,
            left_indent,
            right_indent,
            space_before,
            space_after,
            line_spacing,
            tabs,
        })
    }

    fn chars(&mut self) -> Result<CharStyle, String> {
        let font = self.opt_str()?;
        let size = self.u16()?;
        let bold = self.bool()?;
        let italic = self.bool()?;
        let underline = self.bool()?;
        let strike = self.bool()?;
        let color = self.rgb()?;
        let highlight = self.rgb()?;
        let script = match self.u8()? {
            0 => Script::Normal,
            1 => Script::Super,
            2 => Script::Sub,
            _ => return Err(self.bad()),
        };
        let link = self.opt_str()?.map(Arc::from);
        Ok(CharStyle {
            font,
            size,
            bold,
            italic,
            underline,
            strike,
            color,
            highlight,
            script,
            link,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<Section>, Options) {
        let plain = CharStyle::default();
        let marked = CharStyle {
            font: Some("DejaVu Serif".to_string()),
            size: 31,
            bold: true,
            italic: true,
            underline: true,
            strike: true,
            color: Some((1, 2, 3)),
            highlight: Some((250, 251, 252)),
            script: Script::Super,
            link: Some(Arc::from("scrivlnk://4D9C3A5E")),
        };
        let para = |align, line_spacing, runs| Paragraph {
            style: ParaStyle {
                align,
                first_indent: -360,
                left_indent: 720,
                right_indent: 1,
                space_before: i32::MIN,
                space_after: i32::MAX,
                line_spacing,
                tabs: vec![(720, 'l'), (4320, 'r'), (2000, 'c'), (3000, 'd')],
            },
            runs,
        };
        let run = |text: &str, style: &CharStyle| Run {
            text: text.to_string(),
            style: style.clone(),
        };
        let scene = RichText {
            paragraphs: vec![
                para(Align::Natural, LineSpacing::Single, vec![]),
                para(
                    Align::Left,
                    LineSpacing::Multiple(1.5),
                    vec![run("plain ", &plain), run("marked", &marked)],
                ),
                para(
                    Align::Center,
                    LineSpacing::AtLeast(300),
                    vec![run(
                        "ภาษาไทย 日本語 \u{ad}\u{301} 😀",
                        &CharStyle {
                            script: Script::Sub,
                            ..plain.clone()
                        },
                    )],
                ),
                para(
                    Align::Right,
                    LineSpacing::Exactly(-1),
                    vec![run("", &plain)],
                ),
                para(Align::Justify, LineSpacing::Multiple(f32::NAN), vec![]),
            ],
            lossy: Vec::new(),
        };
        let sections = vec![
            Section {
                heading: Some("Chapter One".to_string()),
                scenes: vec![scene.clone(), RichText::default()],
            },
            Section {
                heading: None,
                scenes: vec![],
            },
        ];
        let opts = Options {
            title: "The Lighthouse".to_string(),
            author: "Ada Q. Writer".to_string(),
            manuscript: true,
            headings: Headings::Titles,
            scene_separator: "#".to_string(),
        };
        (sections, opts)
    }

    #[test]
    fn a_snapshot_reads_back_as_it_was_written() {
        let (sections, opts) = sample();
        for headings in [Headings::Numbered, Headings::Titles, Headings::None] {
            let opts = Options {
                headings,
                ..opts.clone()
            };
            let bytes = encode(&sections, &opts);
            let (back, back_opts) = decode(&bytes).unwrap();
            assert_eq!(format!("{back_opts:?}"), format!("{opts:?}"));
            // NaN is not equal to itself; its bits are what must survive.
            assert_eq!(format!("{back:?}"), format!("{sections:?}"));
            assert_eq!(encode(&back, &back_opts), bytes);
        }
    }

    #[test]
    fn a_snapshot_cut_short_or_damaged_is_refused() {
        let (sections, opts) = sample();
        let bytes = encode(&sections, &opts);
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err(), "cut at {n}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer).is_err());
        // A damaged byte anywhere: decoding answers, and never panics or
        // allocates for a count the bytes cannot hold.
        for at in MAGIC.len()..bytes.len() {
            for v in [0, 1, 5, 0x7f, 0xff] {
                let mut damaged = bytes.clone();
                damaged[at] = v;
                let _ = decode(&damaged);
            }
        }
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&huge).is_err());
    }
}
