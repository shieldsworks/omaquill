//! Compiles away from the window's thread. The window gathers the
//! manuscript on its own thread, the one that owns the project, and hands
//! the owned [`Job`] to [`run`] on a worker. Nothing here touches GTK or the
//! project.
//!
//! A PDF is set by Pango and written by Cairo, whose FreeType faces every
//! thread of a process shares. Two PDFs rendered at once in one process set
//! lines with each other's spacing and, with fonts for many scripts,
//! crash; so does one PDF rendered while GTK lays out text on the window's
//! thread. So a PDF renders in a process of its own: omaquill run with
//! [`RENDER_PDF`].

use crate::compile::{self, Format, Options, Section};
use crate::project;
use crate::snapshot;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::Duration;

/// Everything one compile needs: the gathered manuscript, the options, the
/// format and the file it replaces.
pub struct Job {
    pub sections: Vec<Section>,
    pub opts: Options,
    pub format: Format,
    pub path: PathBuf,
}

const RUNNING: u8 = 0;
const STOPPED: u8 = 1;
const COMMITTED: u8 = 2;

/// Shared between the window and one running [`Job`]. Whichever of
/// [`Stop::stop`] and the job's rename comes first wins, so a stopped job
/// never replaces its target.
#[derive(Clone, Default)]
pub struct Stop(Arc<AtomicU8>);

impl Stop {
    /// Asks the job to stop, and ends its PDF renderer if one is running.
    /// True when its target stays as it was; false when the job already
    /// replaced it.
    pub fn stop(&self) -> bool {
        match self
            .0
            .compare_exchange(RUNNING, STOPPED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => true,
            Err(now) => now == STOPPED,
        }
    }

    fn stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst) == STOPPED
    }

    fn commit(&self) -> bool {
        self.0
            .compare_exchange(RUNNING, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    /// The output replaced the target.
    Written,
    /// [`Stop::stop`] came first. The target is as it was.
    Stopped,
}

/// Renders `job` and writes it through a temporary file in the target's
/// folder, renamed over the target only if `stop` has not been called. A
/// PDF renders in a `renderer` process: omaquill's own program. Errors come
/// back as the text the window shows.
pub fn run(job: Job, stop: &Stop, renderer: &Path) -> Result<Done, String> {
    let bytes = match job.format {
        Format::Pdf => match render_apart(&job, stop, renderer)? {
            Some(bytes) => bytes,
            None => return Ok(Done::Stopped),
        },
        format => compile::render(&job.sections, &job.opts, format).map_err(|e| e.to_string())?,
    };
    if stop.stopped() {
        return Ok(Done::Stopped);
    }
    match project::write_atomic_if(&job.path, &bytes, || stop.commit()) {
        Ok(true) => Ok(Done::Written),
        Ok(false) => Ok(Done::Stopped),
        Err(e) => Err(e.to_string()),
    }
}

/// The argument that makes omaquill a PDF renderer: [`serve_render`] reads a
/// gathered manuscript on standard input and writes its PDF on standard
/// output.
pub const RENDER_PDF: &str = "render-pdf";

/// Runs one PDF renderer's work, in the process [`run`] started.
pub fn serve_render(input: &mut impl Read, output: &mut impl Write) -> Result<(), String> {
    let mut bytes = Vec::new();
    input
        .read_to_end(&mut bytes)
        .map_err(|e| format!("couldn't read the manuscript: {e}"))?;
    let (sections, opts) = snapshot::decode(&bytes)?;
    let pdf = compile::render(&sections, &opts, Format::Pdf).map_err(|e| e.to_string())?;
    output
        .write_all(&pdf)
        .and_then(|()| output.flush())
        .map_err(|e| format!("couldn't hand back the PDF: {e}"))
}

/// The PDF of `job`, from a `renderer` process. `None` when `stop` came
/// first: the renderer is ended then and its work dropped.
fn render_apart(job: &Job, stop: &Stop, renderer: &Path) -> Result<Option<Vec<u8>>, String> {
    if stop.stopped() {
        return Ok(None);
    }
    // The manuscript goes in a file, not a pipe: omaquill dies of SIGPIPE
    // (see main), and a write to a renderer that has ended would raise it.
    let input = staged(&snapshot::encode(&job.sections, &job.opts))
        .map_err(|e| format!("couldn't hand the manuscript to the PDF renderer: {e}"))?;
    let mut child = Command::new(renderer)
        .arg(RENDER_PDF)
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start the PDF renderer: {e}"))?;
    let pdf = drain(child.stdout.take());
    let why = drain(child.stderr.take());
    let status = loop {
        if stop.stopped() {
            end(&mut child);
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                end(&mut child);
                return Err(format!("lost the PDF renderer: {e}"));
            }
        }
    };
    let pdf = pdf
        .join()
        .map_err(|_| "lost the PDF renderer's output".to_string())?;
    let why = why.join().ok().and_then(Result::ok).unwrap_or_default();
    if !status.success() {
        let why = String::from_utf8_lossy(&why);
        let last = why.lines().rev().map(str::trim).find(|l| !l.is_empty());
        return Err(match last {
            Some(line) => line.strip_prefix("omaquill: ").unwrap_or(line).to_string(),
            None => format!("the PDF renderer stopped ({status})"),
        });
    }
    pdf.map(Some)
        .map_err(|e| format!("couldn't read the PDF renderer's output: {e}"))
}

/// Ends `child` and reaps it.
fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Reads all of `pipe` on a thread of its own, so the renderer never waits
/// on a full pipe.
fn drain(
    pipe: Option<impl Read + Send + 'static>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes)?;
        }
        Ok(bytes)
    })
}

/// `bytes` in a file only this process can open, already unlinked, read
/// from its start.
fn staged(bytes: &[u8]) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        ".omaquill-snapshot-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
