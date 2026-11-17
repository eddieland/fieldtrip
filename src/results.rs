//! Result files: whole responses written to disk, so a large one stays out of the calling agent's context without
//! being lost.
//!
//! fieldtrip picks the directory and the file name. The calling agent can only ask for a save, so it cannot write
//! anywhere else or overwrite anything. Each server writes into a directory of its own, named by its session, under
//! `fieldtrip/responses` in the user's cache directory. On Unix the directories and the files are readable by the
//! user alone.

use std::fmt::Write as _;
use std::hash::{BuildHasher as _, RandomState};
use std::path::{Path, PathBuf};
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
    /// What `read_result` knows it by: the file's name without `.json`.
    pub(crate) id: String,
    /// The file.
    pub(crate) path: PathBuf,
}

/// The most of this session's results that are listed when one is not found.
const LISTED: usize = 10;

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
        let wanted = wanted.trim();
        let looks_like_a_path = wanted.contains(['/', '\\']) || has_json_extension(Path::new(wanted));
        if looks_like_a_path {
            return find_path(Path::new(wanted));
        }
        let is_a_name = !wanted.is_empty()
            && !wanted.starts_with('.')
            && wanted
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ['-', '_', '.'].contains(&character));
        let file = format!("{wanted}.json");
        let found = is_a_name
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
        let mut own: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(&self.directory)
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
             the path from its `Saved the whole response` line."
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
fn find_path(path: &Path) -> Result<Stored, String> {
    let root = root();
    let refused = || {
        format!(
            "`read_result` reads only the responses that `execute` wrote to files, which are under {}. {} is not \
             one of them. Pass the ID or the path from `execute`'s `Saved the whole response` line.",
            root.display(),
            path.display()
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
