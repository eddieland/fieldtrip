//! Result files: whole responses written to disk, so a large one stays out of the calling agent's context without
//! being lost.
//!
//! fieldtrip picks the directory and the file name. The calling agent can only ask for a save, so it cannot write
//! anywhere else or overwrite anything. Each server writes into a directory of its own, named by its session, under
//! `fieldtrip/responses` in the user's cache directory. On Unix the directories and the files are readable by the
//! user alone.
//!
//! A file is deleted `result_ttl` after it was written, whichever session wrote it. Nothing runs in the background:
//! the old files are cleared out at startup, and before a file is written or looked for.

use std::fmt::Write as _;
use std::hash::{BuildHasher as _, RandomState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::config::show_duration;

/// Where this server writes responses.
#[derive(Debug)]
pub(crate) struct Results {
    /// This session's directory.
    directory: PathBuf,
    /// Counts the files written, so that no two IDs are made from the same input.
    written: AtomicU64,
    /// How long a file is kept after it was written.
    ttl: Duration,
}

/// A response that was written to a file.
#[derive(Debug)]
pub(crate) struct Stored {
    /// What `read_result` knows it by: the file's name without `.json`.
    pub(crate) id: String,
    /// The file.
    pub(crate) path: PathBuf,
}

/// The most of this session's results that are listed when one is not found.
const LISTED: usize = 10;

/// The directory every session's directory is in: `fieldtrip/responses` in the user's cache directory.
pub(crate) fn root() -> PathBuf {
    crate::cache::root().join("responses")
}

impl Results {
    /// The result files of the server whose session is `session`, each kept for `ttl`. Nothing is created until a
    /// response is written.
    pub(crate) fn new(session: &str, ttl: Duration) -> Self {
        Self {
            directory: root().join(session),
            written: AtomicU64::new(0),
            ttl,
        }
    }

    /// Deletes every result file written more than `result_ttl` ago, by any session, and each other session's
    /// directory that is left empty. A file that cannot be deleted is left for the next time.
    ///
    /// Only what fieldtrip writes is touched: `.json` files in the responses directory, where `save` once put
    /// them, and in the directories directly inside it. Links are not followed.
    pub(crate) fn sweep(&self) {
        let now = SystemTime::now();
        let expired = |path: &Path| {
            has_json_extension(path)
                && std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                    metadata.is_file()
                        && metadata
                            .modified()
                            .is_ok_and(|written| now.duration_since(written).is_ok_and(|age| age > self.ttl))
                })
        };
        let delete = |path: &Path| {
            if expired(path)
                && let Err(error) = std::fs::remove_file(path)
            {
                tracing::debug!("could not delete the old result file {}: {error}", path.display());
            }
        };
        let entries = |directory: &Path| {
            std::fs::read_dir(directory)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
        };
        for path in entries(&root()) {
            if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                delete(&path);
                continue;
            }
            entries(&path).for_each(|file| delete(&file));
            if path != self.directory {
                // This fails for a directory that still holds something, which is what keeps it.
                let _ignored = std::fs::remove_dir(&path);
            }
        }
    }

    /// Writes `body`, a response from the endpoint called `endpoint`, to a new file named after the endpoint and
    /// an ID made for this response.
    ///
    /// # Errors
    ///
    /// Returns a message when the directory or the file cannot be created or written.
    pub(crate) fn write(&self, endpoint: &str, body: &str) -> Result<Stored, String> {
        self.sweep();
        crate::cache::create_private_dir(&self.directory)?;
        let count = self.written.fetch_add(1, Ordering::Relaxed);
        // A fresh `RandomState` is keyed differently every time, so the ID cannot be guessed or repeat.
        let random = RandomState::new().hash_one(count) & 0xffff_ffff_ffff;
        let id = format!("{endpoint}-{random:012x}");
        let path = self.directory.join(format!("{id}.json"));
        crate::cache::write_new(&path, body)?;
        Ok(Stored { id, path })
    }

    /// Finds the result the calling agent calls `wanted`: an ID, or the path of a result file.
    ///
    /// Only a file under fieldtrip's responses directory is ever found, whatever `wanted` says, so the calling
    /// agent cannot use this to read anything else of the user's.
    ///
    /// # Errors
    ///
    /// Returns a message that says what can be read when `wanted` names no result file.
    pub(crate) fn find(&self, wanted: &str) -> Result<Stored, String> {
        self.sweep();
        let wanted = wanted.trim();
        if wanted.contains(['/', '\\']) || has_json_extension(Path::new(wanted)) {
            return find_path(Path::new(wanted), self.ttl);
        }
        let file = format!("{wanted}.json");
        let found = is_plain_name(wanted)
            .then(|| {
                // This session's own directory first, then those of earlier sessions.
                let others = std::fs::read_dir(root())
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path());
                std::iter::once(self.directory.clone())
                    .chain(others)
                    .map(|directory| directory.join(&file))
                    .find(|path| is_plain_file(path))
            })
            .flatten();
        found.map_or_else(
            || Err(self.unknown(wanted)),
            |path| {
                Ok(Stored {
                    id: wanted.to_owned(),
                    path,
                })
            },
        )
    }

    /// The error for an ID that names no result, which lists the newest results of this session.
    fn unknown(&self, wanted: &str) -> String {
        let mut own: Vec<(SystemTime, String)> = std::fs::read_dir(&self.directory)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let written = entry.metadata().and_then(|metadata| metadata.modified()).ok()?;
                let id = path.file_stem()?.to_str()?.to_owned();
                has_json_extension(&path).then_some((written, id))
            })
            .collect();
        own.sort_by(|left, right| right.cmp(left));
        let mut message = format!(
            "There is no result `{wanted}`. A result is a response that `execute` wrote to a file: pass the ID or \
             the path from its `Saved the whole response` line. A file is deleted {} after it was written \
             (`result_ttl`), and then the operation has to be run again.",
            show_duration(self.ttl)
        );
        if own.is_empty() {
            message.push_str(" This session has written none. Run the operation with `execute` first.");
        } else {
            let ids = own.iter().take(LISTED).map(|(_, id)| id.as_str());
            let _ = write!(
                message,
                " The results of this session, newest first: {}.",
                crate::suggest::backticked(ids)
            );
            if own.len() > LISTED {
                let _ = write!(message, " And {} older ones.", own.len() - LISTED);
            }
        }
        message
    }
}

/// Whether `name` can be a session or a result ID: letters, digits, `-`, `_` and `.`, and no leading dot, so it names
/// one file or directory and nothing above it.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ['-', '_', '.'].contains(&character))
}

/// The file of the result `id` that the session `session` wrote, when it is still there.
pub(crate) fn existing(session: &str, id: &str) -> Option<PathBuf> {
    if !is_plain_name(session) || !is_plain_name(id) {
        return None;
    }
    let path = root().join(session).join(format!("{id}.json"));
    is_plain_file(&path).then_some(path)
}

/// Whether `path` ends in `.json`.
fn has_json_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// Whether `path` is a file, and not a link to one.
fn is_plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Finds the result file at `path`, which has to be inside the responses directory once links and `..` are followed.
fn find_path(path: &Path, ttl: Duration) -> Result<Stored, String> {
    let root = root();
    let refused = || {
        format!(
            "`read_result` reads only the responses that `execute` wrote to files, which are under {}. {} is not \
             one of them. Pass the ID or the path from `execute`'s `Saved the whole response` line. A file is \
             deleted {} after it was written (`result_ttl`), and then the operation has to be run again.",
            root.display(),
            path.display(),
            show_duration(ttl)
        )
    };
    let real_root = std::fs::canonicalize(&root).map_err(|_error| refused())?;
    let real = std::fs::canonicalize(path).map_err(|_error| refused())?;
    let id = real.file_stem().and_then(|stem| stem.to_str()).map(str::to_owned);
    match id {
        Some(id) if real.starts_with(&real_root) && has_json_extension(&real) && is_plain_file(&real) => {
            Ok(Stored { id, path: real })
        }
        _ => Err(refused()),
    }
}
