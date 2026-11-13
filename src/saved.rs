//! `save`: writing a whole response to a file, so a large one stays out of the calling agent's context without
//! being lost.
//!
//! fieldtrip picks the directory and the file name. The calling agent only asks for a save, so it cannot write
//! anywhere else or overwrite anything. On Unix the directory and the files are readable by the user alone.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Counts the saves this process has made, so two in the same millisecond get different names.
static SAVES: AtomicU64 = AtomicU64::new(0);

/// The directory responses are saved in: `fieldtrip/responses` in the user's cache directory.
///
/// That is `$XDG_CACHE_HOME`, then `~/.cache`, then `%LOCALAPPDATA%` on Windows, then the system's temporary
/// directory.
fn directory() -> PathBuf {
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let cache = set("XDG_CACHE_HOME")
        .or_else(|| set("HOME").map(|home| home.join(".cache")))
        .or_else(|| set("LOCALAPPDATA"))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("fieldtrip").join("responses")
}

/// Writes `body`, a response from the endpoint called `endpoint`, to a new file, and returns its path.
///
/// # Errors
///
/// Returns a message when the directory or the file cannot be created or written.
pub(crate) fn save(endpoint: &str, body: &str) -> Result<PathBuf, String> {
    let directory = directory();
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let count = SAVES.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{endpoint}-{millis}-{}-{count}.json", std::process::id()));

    let mut options = std::fs::OpenOptions::new();
    // A new file every time: an existing one is never overwritten, nor a link followed.
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(body.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(path)
}
