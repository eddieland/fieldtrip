//! fieldtrip's cache directory, and the copies of introspected schemas kept in it.
//!
//! A copy is SDL, kept per endpoint URL so a restart does not introspect again. The file is named by a hash of the
//! URL, because a URL can carry a key in its query, and only the shown URL, with query values hidden, is written
//! into it. On Unix the directories and files are readable by the user alone.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sha2::{Digest as _, Sha256};

/// Counts the files this process has written, so two temporary files never share a name.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// fieldtrip's directory in the user's cache directory.
///
/// That is `$XDG_CACHE_HOME`, then `~/.cache`, then `%LOCALAPPDATA%` on Windows, then the system's temporary
/// directory.
pub(crate) fn root() -> PathBuf {
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let cache = set("XDG_CACHE_HOME")
        .or_else(|| set("HOME").map(|home| home.join(".cache")))
        .or_else(|| set("LOCALAPPDATA"))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("fieldtrip")
}

/// Creates `directory` and its parents. On Unix the ones it creates are readable by the user alone.
///
/// # Errors
///
/// Returns a message naming the directory when it cannot be created.
pub(crate) fn create_private_dir(directory: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))
}

/// Creates a new file at `path` that only the user can read, and writes `contents` to it. An existing file is never
/// overwritten, nor a link followed.
///
/// # Errors
///
/// Returns a message naming the file when it cannot be created or written.
pub(crate) fn write_new(path: &Path, contents: &str) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// A schema found in the cache.
#[derive(Debug)]
pub(crate) struct Kept {
    /// The SDL.
    pub(crate) sdl: String,
    /// How long ago it was written.
    pub(crate) age: Duration,
}

/// The kept schema for the endpoint at `url`, however old it is. `None` when there is none or it cannot be read.
pub(crate) fn read_schema(url: &str) -> Option<Kept> {
    let path = schema_path(url);
    let sdl = std::fs::read_to_string(&path).ok()?;
    let written = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).ok()?;
    let age = SystemTime::now().duration_since(written).unwrap_or_default();
    Some(Kept { sdl, age })
}

/// Keeps `sdl` as the schema of the endpoint at `url`, replacing any copy kept before. `shown_url` is written into
/// the file, so a person can tell what it is.
///
/// # Errors
///
/// Returns a message when the directory or the file cannot be created or written.
pub(crate) fn write_schema(url: &str, shown_url: &str, sdl: &str) -> Result<(), String> {
    let path = schema_path(url);
    let directory = path.parent().unwrap_or(&path);
    create_private_dir(directory)?;
    let count = WRITES.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{}.{count}.tmp", std::process::id()));
    let mut contents = String::new();
    let _ = writeln!(contents, "# The schema of {shown_url}, kept by fieldtrip.");
    contents.push_str(sdl);
    write_new(&temporary, &contents)?;
    // A rename replaces the old copy in one step, so a reader sees either the old copy or the new one.
    std::fs::rename(&temporary, &path).map_err(|error| {
        let _ignored = std::fs::remove_file(&temporary);
        format!("could not write {}: {error}", path.display())
    })
}

/// Where the schema of the endpoint at `url` is kept: a file named by a hash of the URL, which may hold a key.
fn schema_path(url: &str) -> PathBuf {
    let digest = Sha256::digest(url.as_bytes());
    let mut name = String::with_capacity(40);
    for byte in digest.iter().take(16) {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".graphql");
    root().join("schemas").join(name)
}
