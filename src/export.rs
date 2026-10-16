//! Compiles away from the window's thread. The window gathers the
//! manuscript on its own thread, the one that owns the project, and hands
//! the owned [`Job`] to [`run`] on a worker. Nothing here touches GTK or the
//! project.

use crate::compile::{self, Format, Options, Section};
use crate::project;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
    /// Asks the job to stop. True when its target stays as it was; false
    /// when the job already replaced it.
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
/// folder, renamed over the target only if `stop` has not been called.
/// Errors come back as the text the window shows.
pub fn run(job: Job, stop: &Stop) -> Result<Done, String> {
    let bytes = compile::render(&job.sections, &job.opts, job.format).map_err(|e| e.to_string())?;
    if stop.stopped() {
        return Ok(Done::Stopped);
    }
    match project::write_atomic_if(&job.path, &bytes, || stop.commit()) {
        Ok(true) => Ok(Done::Written),
        Ok(false) => Ok(Done::Stopped),
        Err(e) => Err(e.to_string()),
    }
}
