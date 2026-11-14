//! `audit_log`: one JSON line per `execute` attempt, appended to a file the user names.
//!
//! The log answers what an MCP client's transcript may not: which operations ran, against which endpoint, who or
//! what approved them, and what came back. Attempts that were refused, declined or failed are recorded too, with the
//! reason. An operation that is not valid is not an attempt, since nothing could have been sent.
//!
//! `history` reads the log back, so a calling agent can see what was already tried, in this session or an earlier one.
//!
//! Header values never reach the log. What the calling agent wrote, the operation and its variables, has the values
//! of the endpoint's `env` and `command` plugs removed, as a response body does, and an endpoint with
//! `audit_variables = false` has its variables left out altogether.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::hash::{BuildHasher as _, RandomState};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::config::Endpoint;
use crate::http::Reply;
use crate::plan::{Kind, Variables};

/// Where attempts are recorded, if anywhere.
#[derive(Debug)]
pub(crate) struct Audit {
    /// The log file. `None` when `audit_log` is not set, and nothing is recorded.
    path: Option<PathBuf>,
    /// Tells this server's lines from those of earlier ones that wrote to the same file.
    session: String,
    /// Held while a line is written, so lines from concurrent calls never interleave.
    writing: Mutex<()>,
}

/// What an attempt was: the operation, where it was going, and its estimated cost.
#[derive(Debug)]
pub(crate) struct Attempt<'call> {
    /// The endpoint's name.
    pub(crate) endpoint: &'call str,
    /// Whether it is a query or a mutation.
    pub(crate) kind: Kind,
    /// The operation's name, when it has one.
    pub(crate) operation: Option<&'call str>,
    /// The operation's text: canonical when it was validated, as the calling agent wrote it when it was not.
    pub(crate) text: &'call str,
    /// The variables given with it.
    pub(crate) variables: &'call Variables,
    /// The estimated cost, when it was costed.
    pub(crate) cost: Option<u64>,
}

/// Who let an attempt through, or what stopped it.
#[derive(Debug)]
pub(crate) struct Verdict {
    /// One word: `accepted`, `auto`, `client`, `declined`, `cancelled`, `unavailable`, `failed` or `denied`.
    pub(crate) approval: &'static str,
    /// Why, in a few words. `None` for a plain accept.
    pub(crate) reason: Option<String>,
}

/// What came of an attempt.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Outcome<'reply> {
    /// Nothing was sent.
    NotSent,
    /// The endpoint answered.
    Answered(&'reply Reply),
    /// The request was made, or was about to be, and no answer came. The message says why.
    Failed(&'reply str),
}

/// The most of the end of the log that `history` reads.
const HISTORY_BYTES: u64 = 16 * 1024 * 1024;

/// The lines `history` found.
#[derive(Debug, Default)]
pub(crate) struct History {
    /// The lines that were kept, as written, oldest first.
    pub(crate) lines: Vec<String>,
    /// How many lines are not a JSON object with an `endpoint`, and were skipped.
    pub(crate) unreadable: usize,
    /// Whether the log is longer than what was read, so older lines were not looked at.
    pub(crate) partial: bool,
}

/// The only part of a response the log reads: how many `errors` it has.
#[derive(Deserialize)]
struct Errors {
    errors: Option<Vec<IgnoredAny>>,
}

impl Audit {
    /// Prepares to record attempts in the file at `path`, creating it and its folders if needed. With no `path`,
    /// nothing is recorded and no file is touched.
    ///
    /// The file is opened now so that a log that cannot be written stops the server before any attempt is made.
    /// On Unix, new folders are readable by the user alone, and so is a new file.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file and the setting when it cannot be created or opened for appending.
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self, String> {
        if let Some(path) = &path {
            create(path).map_err(|error| {
                format!(
                    "could not open the audit log {}, which `audit_log` names in the config: {error}",
                    path.display()
                )
            })?;
        }
        let session = format!("{:016x}", RandomState::new().hash_one(std::process::id()));
        Ok(Self {
            path,
            session,
            writing: Mutex::new(()),
        })
    }

    /// The log file, when there is one.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The ID this server writes in the `session` of each line.
    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    /// Reads the lines of the log whose `endpoint` passes `keep`, oldest first. Only the last 16 MiB are read, and
    /// a last line that is still being written is left out.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file when it cannot be read. A log that is not there has no lines.
    pub(crate) fn history(&self, keep: impl Fn(&str) -> bool) -> Result<History, String> {
        let Some(path) = &self.path else {
            return Ok(History::default());
        };
        let unreadable = |error: std::io::Error| format!("could not read the audit log {}: {error}", path.display());
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(History::default()),
            Err(error) => return Err(unreadable(error)),
        };
        let length = file.metadata().map_err(unreadable)?.len();
        let start = length.saturating_sub(HISTORY_BYTES);
        file.seek(SeekFrom::Start(start)).map_err(unreadable)?;
        let mut bytes = Vec::new();
        file.take(HISTORY_BYTES).read_to_end(&mut bytes).map_err(unreadable)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.split_inclusive('\n').filter(|line| line.ends_with('\n'));
        if start > 0 {
            // The read began partway through a line.
            lines.next();
        }
        let mut history = History {
            partial: start > 0,
            ..History::default()
        };
        for line in lines.map(str::trim).filter(|line| !line.is_empty()) {
            let endpoint = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|entry| entry.get("endpoint").and_then(Value::as_str).map(str::to_owned));
            match endpoint {
                Some(endpoint) if keep(&endpoint) => history.lines.push(line.to_owned()),
                Some(_) => {}
                None => history.unreadable += 1,
            }
        }
        Ok(history)
    }

    /// Appends one line for `attempt` on `endpoint`. A line that cannot be written is reported in the log on
    /// stderr: by then the attempt has happened, and failing the tool call would not undo it.
    pub(crate) async fn record(
        &self,
        endpoint: &Endpoint,
        attempt: &Attempt<'_>,
        verdict: &Verdict,
        outcome: Outcome<'_>,
    ) {
        let Some(path) = &self.path else { return };
        let mut line = Value::Object(self.entry(endpoint, attempt, verdict, outcome)).to_string();
        // The calling agent may have pasted a credential into the operation or its variables.
        for header in &endpoint.headers {
            if let Some(secret) = header.known_secret().await {
                line = secret.scrub(&line).unwrap_or(line);
            }
        }
        line.push('\n');
        let written = {
            let _guard = self.writing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            append(path, &line)
        };
        if let Err(error) = written {
            tracing::error!("could not write to the audit log {}: {error}", path.display());
        }
    }

    /// The fields of one line, in the order they are written.
    fn entry(
        &self,
        endpoint: &Endpoint,
        attempt: &Attempt<'_>,
        verdict: &Verdict,
        outcome: Outcome<'_>,
    ) -> Map<String, Value> {
        let mut entry = Map::new();
        entry.insert("at".to_owned(), json!(timestamp(SystemTime::now())));
        entry.insert("session".to_owned(), json!(self.session));
        entry.insert("endpoint".to_owned(), json!(attempt.endpoint));
        entry.insert("kind".to_owned(), json!(attempt.kind.keyword()));
        entry.insert("operation".to_owned(), json!(attempt.operation));
        entry.insert("hash".to_owned(), json!(hash(attempt.text)));
        entry.insert("text".to_owned(), json!(attempt.text));
        if endpoint.audit_variables {
            entry.insert("variables".to_owned(), Value::Object(attempt.variables.clone()));
        }
        entry.insert("approval".to_owned(), json!(verdict.approval));
        if let Some(reason) = &verdict.reason {
            entry.insert("reason".to_owned(), json!(reason));
        }
        if let Some(cost) = attempt.cost {
            entry.insert("cost".to_owned(), json!(cost));
        }
        match outcome {
            Outcome::NotSent => {}
            Outcome::Answered(reply) => {
                entry.insert("status".to_owned(), json!(reply.status));
                entry.insert("ms".to_owned(), json!(reply.elapsed.as_millis()));
                let errors = (!reply.cut_short)
                    .then(|| serde_json::from_str::<Errors>(&reply.body).ok())
                    .flatten()
                    .map(|parsed| parsed.errors.map_or(0, |errors| errors.len()));
                entry.insert("errors".to_owned(), json!(errors));
                if !reply.headers.is_empty() {
                    let headers: Map<String, Value> = reply
                        .headers
                        .iter()
                        .map(|(name, value)| (name.clone(), json!(value)))
                        .collect();
                    entry.insert("headers".to_owned(), Value::Object(headers));
                }
            }
            Outcome::Failed(error) => {
                entry.insert("error".to_owned(), json!(error));
            }
        }
        entry
    }
}

/// The name of the first mutation in `text`, for a mutation that was refused before it was validated. `text` may
/// not parse, in which case what did parse is used.
pub(crate) fn mutation_name(text: &str) -> Option<String> {
    use apollo_compiler::ast::{Definition, Document, OperationType};

    let document = Document::parse(text, "operation.graphql").unwrap_or_else(|invalid| invalid.partial);
    document.definitions.iter().find_map(|definition| match definition {
        Definition::OperationDefinition(operation) if operation.operation_type == OperationType::Mutation => {
            operation.name.as_ref().map(ToString::to_string)
        }
        _ => None,
    })
}

/// `sha256:` and the SHA-256 of `text`, in lowercase hex.
fn hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in &digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `time` in RFC 3339, in UTC, to the second: `2026-11-13T14:02:11Z`.
fn timestamp(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let (year, month, day) = civil(seconds / 86_400);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        time % 3_600 / 60,
        time % 60
    )
}

/// The year, month and day that is `days` days after 1970-01-01, in the proleptic Gregorian calendar.
///
/// This is Howard Hinnant's `civil_from_days`, for days on or after the epoch.
const fn civil(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// How the log is opened: for appending, created if missing, and on Unix readable by the user alone when created.
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// Creates the log at `path` and the folders it is in, if they are missing.
fn create(path: &Path) -> std::io::Result<()> {
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(folder)?;
    }
    options().open(path).map(drop)
}

/// Appends `line` to the log at `path`. The file is opened each time, so a log rotated away is started afresh.
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = options().open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}
