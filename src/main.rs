#![cfg_attr(not(test), deny(clippy::unwrap_used))]
//! omaquill: a free writing studio for authors and writers on Linux and Omarchy.
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
use omaquill::export;
use omaquill::project::{Item, Kind, Project};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
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
        // The window's PDF compiles run here, a process each.
        Some(export::RENDER_PDF) => cli(export::serve_render(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )),
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
    let mut cache = HashMap::new();
    let survey = compile::survey(&p, &mut cache).map_err(|e| e.to_string())?;
    let opts = Options::for_project(&p);
    let sections = compile::gather_cached(&p, &opts, &mut cache).map_err(|e| e.to_string())?;
    let docs = survey.docs;
    let words = survey.words;
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
    if survey.lossy.is_empty() {
        println!("  every document reads fully");
    } else {
        println!("  shown as plain text (kept unless edited):");
        for l in survey.lossy {
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
    if write_output(out, &bytes)? == Wrote::File {
        println!("{}", out.display());
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Wrote {
    /// Standard output got the bytes, so the path is not printed after them.
    Stdout,
    /// A file, device or FIFO got them.
    File,
}

/// Writes the compiled `bytes` to `out`, the command's OUT argument.
///
/// - `-`, `/dev/stdout`, `/dev/fd/1` and `/proc/self/fd/1` are this
///   process's standard output. The bytes go to that open stream, which
///   also keeps `>>` appending.
/// - A FIFO, device or socket at `out` gets a plain write, opened with
///   `O_NOFOLLOW` and without creating anything.
/// - A symlink at a name for a stream ([`names_a_stream`]) is followed for
///   a plain write and is never replaced, as root too. When it leads to a
///   regular file, nothing is written. A symlink anywhere else, including
///   world-writable `/dev/shm` and `/dev/mqueue`, is replaced like any
///   other file.
/// - Anything else goes through [`omaquill::fsdir::write_atomic`]: a new
///   file is renamed into place, and a symlink at `out` is replaced, not
///   followed.
fn write_output(out: &Path, bytes: &[u8]) -> Result<Wrote, String> {
    use std::io::Write;
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let shown = out.display();
    let stdout_names = ["-", "/dev/stdout", "/dev/fd/1", "/proc/self/fd/1"];
    if stdout_names.iter().any(|n| out == Path::new(n)) {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(bytes)
            .and_then(|()| stdout.flush())
            .map_err(|e| format!("couldn't write to standard output: {e}"))?;
        return Ok(Wrote::Stdout);
    }
    let meta = std::fs::symlink_metadata(out).ok();
    let kind = meta.as_ref().map(|m| m.file_type());
    let dev_link = names_a_stream(out) && kind.is_some_and(|k| k.is_symlink());
    let special = kind
        .is_some_and(|k| k.is_fifo() || k.is_char_device() || k.is_block_device() || k.is_socket());
    if dev_link || special {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true);
        if !dev_link {
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts
            .open(out)
            .map_err(|e| format!("couldn't open {shown} to write: {e}"))?;
        let now = file
            .metadata()
            .map_err(|e| format!("couldn't check {shown}: {e}"))?;
        if now.is_file() {
            // A link in /dev to a regular file, or a name swapped since the
            // check. A plain write there would leave the old tail.
            return Err(format!(
                "{shown} is a regular file now; give its own path to replace it"
            ));
        }
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .map_err(|e| format!("couldn't write {shown}: {e}"))?;
        return Ok(Wrote::File);
    }
    omaquill::fsdir::write_atomic(out, bytes)
        .map_err(|e| format!("couldn't write {shown}: {e}"))?;
    Ok(Wrote::File)
}

/// Whether `out` is one of the system's names for an open stream:
/// `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/fd/*`, `/dev/tty*` or
/// `/proc/self/fd/*`, with no `.` or `..` in it. Only root writes these
/// names, so a symlink at one leads where the system put it.
fn names_a_stream(out: &Path) -> bool {
    if !out
        .components()
        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return false;
    }
    let in_dir = |dir: &str| out.parent() == Some(Path::new(dir));
    let name = out.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ["/dev/stdin", "/dev/stdout", "/dev/stderr"]
        .iter()
        .any(|n| out == Path::new(n))
        || in_dir("/dev/fd")
        || in_dir("/proc/self/fd")
        || (in_dir("/dev") && name.starts_with("tty"))
}
