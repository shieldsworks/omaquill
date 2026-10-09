#![cfg_attr(not(test), deny(clippy::unwrap_used))]
//! omaquill: a writing studio for Omarchy that opens Scrivener projects.
//!
//! ```text
//! omaquill [PROJECT.scriv]                  open the app
//! omaquill check PROJECT.scriv              read everything, report, change nothing
//! omaquill compile PROJECT.scriv OUT.docx   compile from the terminal
//!     [--format docx|pdf|epub|md|txt|rtf] [--author NAME] [--title TITLE]
//!     [--plain | --book] (no manuscript format: the text's own formatting,
//!     or for PDF a 6x9" paperback)
//! ```

mod ui;

use omaquill::compile::{self, Format, Options};
use omaquill::project::{Item, Kind, Project};
use omaquill::rtf;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    // `omaquill check ... | head` should stop quietly, not panic.
    // SAFETY: restoring the default signal disposition before any threads.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V") => {
            println!("omaquill {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", USAGE);
            ExitCode::SUCCESS
        }
        Some("check") => cli(check(&args[2..])),
        Some("compile") => cli(compile_cli(&args[2..])),
        _ => {
            let code = ui::run();
            if code == gtk::glib::ExitCode::SUCCESS {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

const USAGE: &str = "\
usage: omaquill [PROJECT.scriv]
       omaquill check PROJECT.scriv
       omaquill compile PROJECT.scriv OUT [--format docx|pdf|epub|md|txt|rtf]
                [--author NAME] [--title TITLE] [--plain | --book]
";

fn cli(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omaquill: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open(path: Option<&String>) -> Result<Project, String> {
    let path = path.ok_or_else(|| USAGE.trim_end().to_string())?;
    let mut p = PathBuf::from(path);
    if p.extension().is_some_and(|e| e == "scrivx") {
        p.pop();
    }
    Project::open(&p).map_err(|e| e.to_string())
}

/// Reads every document and reports what omaquill sees. Writes nothing.
fn check(args: &[String]) -> Result<(), String> {
    let p = open(args.first())?;
    let mut docs = 0;
    let mut words = 0;
    let mut lossy = Vec::new();
    fn walk(
        p: &Project,
        items: &[Item],
        docs: &mut usize,
        words: &mut usize,
        lossy: &mut Vec<String>,
    ) -> Result<(), String> {
        for i in items {
            if i.kind.has_text() {
                let t = p.text(&i.uuid).map_err(|e| format!("{}: {e}", i.title))?;
                // Writing and reading back must give the same document.
                let again = rtf::parse(rtf::write(&t).as_bytes());
                if again.paragraphs != t.paragraphs {
                    return Err(format!(
                        "{} ({}) doesn't survive a rewrite",
                        i.title, i.uuid
                    ));
                }
                *docs += 1;
                *words += rtf::word_count(&t.plain_text());
                if !t.lossy.is_empty() {
                    lossy.push(format!("  {} has {}", i.title, t.lossy.join(", ")));
                }
            }
            walk(p, &i.children, docs, words, lossy)?;
        }
        Ok(())
    }
    walk(&p, &p.items(), &mut docs, &mut words, &mut lossy)?;
    let opts = Options::for_project(&p);
    let sections = compile::gather(&p, &opts).map_err(|e| e.to_string())?;
    println!("{}", p.name());
    println!("  {docs} documents, {words} words in all");
    println!(
        "  manuscript: {} chapters, {} words compile",
        sections.len(),
        compile::word_count(&sections)
    );
    if let Some(draft) = p.root_folder(Kind::Draft) {
        println!(
            "  binder: {} items in {}",
            count(&draft.children),
            draft.title
        );
    }
    if lossy.is_empty() {
        println!("  every document reads fully");
    } else {
        println!("  shown as plain text (kept unless edited):");
        for l in lossy {
            println!("{l}");
        }
    }
    Ok(())
}

fn count(items: &[Item]) -> usize {
    items.iter().map(|i| 1 + count(&i.children)).sum()
}

fn compile_cli(args: &[String]) -> Result<(), String> {
    let p = open(args.first())?;
    let out = args.get(1).ok_or_else(|| USAGE.trim_end().to_string())?;
    let out = Path::new(out);
    let mut opts = Options::for_project(&p);
    let mut format = None;
    let mut i = 2;
    while i < args.len() {
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or(format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--format" => {
                let v = value()?;
                format = Some(
                    Format::ALL
                        .into_iter()
                        .find(|f| f.extension() == v || (v == "markdown" && *f == Format::Markdown))
                        .ok_or(format!("unknown format {v}"))?,
                );
                i += 1;
            }
            "--author" => {
                opts.author = value()?;
                i += 1;
            }
            "--title" => {
                opts.title = value()?;
                i += 1;
            }
            // Keep the text's own formatting; for PDF, the paperback layout.
            "--plain" | "--book" => opts.manuscript = false,
            other => return Err(format!("unknown option {other}")),
        }
        i += 1;
    }
    let format = match format {
        Some(f) => f,
        None => {
            let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
            Format::ALL
                .into_iter()
                .find(|f| f.extension() == ext)
                .ok_or(
                    "give --format, or an output name ending .docx .pdf .epub .md .txt or .rtf",
                )?
        }
    };
    let bytes = compile::compile(&p, &opts, format).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}
