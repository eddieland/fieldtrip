//! `save`: writing a whole response to a file, so a large one stays out of the calling agent's context without
//! being lost.
//!
//! fieldtrip picks the directory and the file name. The calling agent only asks for a save, so it cannot write
//! anywhere else or overwrite anything. On Unix the directory and the files are readable by the user alone.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Counts the saves this process has made, so two in the same millisecond get different names.
static SAVES: AtomicU64 = AtomicU64::new(0);

/// The directory responses are saved in: `fieldtrip/responses` in the user's cache directory.
fn directory() -> PathBuf {
    crate::cache::root().join("responses")
}

/// Writes `body`, a response from the endpoint called `endpoint`, to a new file, and returns its path.
///
/// # Errors
///
/// Returns a message when the directory or the file cannot be created or written.
pub(crate) fn save(endpoint: &str, body: &str) -> Result<PathBuf, String> {
    let directory = directory();
    crate::cache::create_private_dir(&directory)?;

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let count = SAVES.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{endpoint}-{millis}-{}-{count}.json", std::process::id()));

    crate::cache::write_new(&path, body)?;
    Ok(path)
}
