//! Result files: whole responses written to disk, so a large one stays out of the calling agent's context without
//! being lost.
//!
//! fieldtrip picks the directory and the file name. The calling agent can only ask for a save, so it cannot write
//! anywhere else or overwrite anything. Each server writes into a directory of its own, named by its session, under
//! `fieldtrip/responses` in the user's cache directory. On Unix the directories and the files are readable by the
//! user alone.

use std::hash::{BuildHasher as _, RandomState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where this server writes responses.
#[derive(Debug)]
pub(crate) struct Results {
    /// This session's directory.
    directory: PathBuf,
    /// Counts the files written, so that no two IDs are made from the same input.
    written: AtomicU64,
}

/// A response that was written to a file.
#[derive(Debug)]
pub(crate) struct Stored {
    /// The file.
    pub(crate) path: PathBuf,
}

/// The directory every session's directory is in: `fieldtrip/responses` in the user's cache directory.
fn root() -> PathBuf {
    crate::cache::root().join("responses")
}

impl Results {
    /// The result files of the server whose session is `session`. Nothing is created until a response is written.
    pub(crate) fn new(session: &str) -> Self {
        Self {
            directory: root().join(session),
            written: AtomicU64::new(0),
        }
    }

    /// Writes `body`, a response from the endpoint called `endpoint`, to a new file named after the endpoint and
    /// an ID made for this response.
    ///
    /// # Errors
    ///
    /// Returns a message when the directory or the file cannot be created or written.
    pub(crate) fn write(&self, endpoint: &str, body: &str) -> Result<Stored, String> {
        crate::cache::create_private_dir(&self.directory)?;
        let count = self.written.fetch_add(1, Ordering::Relaxed);
        // A fresh `RandomState` is keyed differently every time, so the ID cannot be guessed or repeat.
        let random = RandomState::new().hash_one(count) & 0xffff_ffff_ffff;
        let path = self.directory.join(format!("{endpoint}-{random:012x}.json"));
        crate::cache::write_new(&path, body)?;
        Ok(Stored { path })
    }
}
