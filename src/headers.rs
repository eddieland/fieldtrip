//! The header plugs: where the value of each configured header comes from.
//!
//! There are three: a literal, an environment variable, and a command. Values are secrets. They are resolved when
//! a request is made and never shown to the calling agent, the approval prompt or the log.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderName, HeaderValue};

/// A string that must not be printed.
#[derive(Clone)]
pub(crate) struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// One configured header.
#[derive(Debug)]
pub(crate) struct Header {
    /// The header's name as written in the config.
    pub(crate) name: String,
    /// The validated header name to send.
    wire_name: HeaderName,
    /// Where the value comes from.
    source: Source,
}

/// A header that is ready to send.
#[derive(Debug)]
pub(crate) struct Resolved {
    /// The header name.
    pub(crate) name: HeaderName,
    /// The header value, marked sensitive so the HTTP client does not print it.
    pub(crate) value: HeaderValue,
    /// The value an `env` or `command` plug produced, before any `format`. A server may echo it back, so it is
    /// removed from responses. A literal is not treated this way: it is usually not a secret, and removing a
    /// word like `en` from a response would only damage it.
    pub(crate) secret: Option<Secret>,
}

impl Secret {
    /// Replaces every occurrence of this secret in `text`. Short values are left alone: they are not much of a
    /// secret, and removing a few common characters would damage the text.
    pub(crate) fn scrub(&self, text: &str) -> Option<String> {
        (self.0.chars().count() >= MIN_SCRUBBED && text.contains(&self.0)).then(|| text.replace(&self.0, REDACTED))
    }
}

/// What a scrubbed secret is replaced with.
const REDACTED: &str = "<redacted>";
/// The shortest secret that is scrubbed from responses.
const MIN_SCRUBBED: usize = 8;

/// Where a header's value comes from.
#[derive(Debug)]
enum Source {
    /// The string written in the config.
    Literal(Secret),
    /// An environment variable, read when the request is made.
    Env {
        /// The variable's name.
        variable: String,
        /// The format the value is placed into, when one is configured.
        format: Option<Secret>,
    },
    /// The output of a command.
    Command {
        /// The program and its arguments. There is no shell in between.
        argv: Vec<Secret>,
        /// The directory the command runs in: the config file's.
        directory: PathBuf,
        /// The format the output is placed into, when one is configured.
        format: Option<Secret>,
        /// How long an output is reused before the command runs again.
        ttl: Duration,
        /// The last output and when it was produced. The lock is held while the command runs, so that
        /// concurrent requests share one run.
        cached: tokio::sync::Mutex<Option<(Instant, Secret)>>,
        /// How the last run went and when it ended, for `list_endpoints`. It is apart from `cached`, so it can be
        /// read while the command runs.
        last_run: Mutex<Option<(Instant, Outcome)>>,
    },
}

/// How a run of a header command went. It never holds the output.
#[derive(Debug, Clone)]
enum Outcome {
    /// It printed a value.
    Ok,
    /// It failed, for the short reason given, such as `exit 3`.
    Failed(String),
}

/// A header command that did not produce a value.
#[derive(Debug)]
struct Failure {
    /// A few words for `list_endpoints`, such as `exit 3`.
    short: String,
    /// The full message, which names the header and the program and quotes stderr.
    message: String,
}

impl Failure {
    /// A failure with a short reason and a full message.
    fn new(short: impl Into<String>, message: String) -> Self {
        Self {
            short: short.into(),
            message,
        }
    }
}

/// How long a command's output is reused when the config sets no `ttl`.
const DEFAULT_TTL: Duration = Duration::from_secs(300);
/// How long a header command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How much of a failed command's stderr to pass on.
const MAX_STDERR: usize = 600;

impl Header {
    /// Reads one entry of an endpoint's `headers` table. `directory` is the config file's, where commands run.
    ///
    /// # Errors
    ///
    /// Returns a message when the entry is not one of the three plugs. The message never quotes a value.
    pub(crate) fn from_config(name: String, value: toml::Value, directory: &Path) -> Result<Self, String> {
        let wire_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_error| format!("`{name}` is not a valid HTTP header name"))?;
        let source = match value {
            toml::Value::String(literal) => Source::Literal(Secret(literal)),
            toml::Value::Table(table) => source_from_table(&name, table, directory)?,
            _ => return Err(expected_a_plug(&name)),
        };
        Ok(Self {
            name,
            wire_name,
            source,
        })
    }

    /// The header's name as it is sent: lowercase, so two names that differ only in case compare equal.
    pub(crate) fn wire_name(&self) -> &str {
        self.wire_name.as_str()
    }

    /// Where the value comes from, for `fieldtrip check`: the plug, the variable or the program, never a value,
    /// an argument or the format.
    pub(crate) fn source(&self) -> String {
        match &self.source {
            Source::Literal(_) => "literal".to_owned(),
            Source::Env { variable, format } => {
                let formatted = if format.is_some() { ", formatted" } else { "" };
                format!("env `{variable}`{formatted}")
            }
            Source::Command { argv, format, ttl, .. } => {
                let program = argv.first().map_or("", |Secret(program)| program.as_str());
                let formatted = if format.is_some() { ", formatted" } else { "" };
                format!("command `{program}`{formatted}, reused for {}s", ttl.as_secs())
            }
        }
    }

    /// Whether the header comes from a command, whose output can go stale and be produced again.
    pub(crate) const fn is_refreshable(&self) -> bool {
        matches!(self.source, Source::Command { .. })
    }

    /// The value fieldtrip would remove from a response for this header, found without running anything: an `env`
    /// variable's current value, or a command's last output, however old. A literal has none, as in
    /// [`Resolved::secret`], and neither has a command that has not run yet.
    pub(crate) async fn known_secret(&self) -> Option<Secret> {
        match &self.source {
            Source::Literal(_) => None,
            Source::Env { variable, .. } => std::env::var(variable)
                .ok()
                .filter(|value| !value.is_empty())
                .map(Secret),
            Source::Command { cached, .. } => cached.lock().await.as_ref().map(|(_, secret)| secret.clone()),
        }
    }

    /// How the last run of the header's command went, as in `command failed, last run 4 s ago: exit 3.`, without
    /// running it. `None` for a header that does not come from a command.
    pub(crate) fn status(&self) -> Option<String> {
        let Source::Command { last_run, .. } = &self.source else {
            return None;
        };
        let last_run = last_run.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Some(match last_run {
            None => "command not run yet".to_owned(),
            Some((at, Outcome::Ok)) => format!("command ok, last run {}", ago(at)),
            Some((at, Outcome::Failed(reason))) => format!(
                "command failed, last run {}: {reason}. The user has to fix it, usually by signing in again.",
                ago(at)
            ),
        })
    }

    /// Produces the header to send. With `fresh`, a command runs again even if its last output is still in its
    /// `ttl`.
    ///
    /// # Errors
    ///
    /// Returns a message that names the header and what it is waiting for, never a value.
    pub(crate) async fn resolve(&self, fresh: bool) -> Result<Resolved, String> {
        let name = &self.name;
        let (value, secret) =
            match &self.source {
                Source::Literal(Secret(literal)) => (literal.clone(), None),
                Source::Env { variable, format } => {
                    let value = std::env::var(variable).ok().filter(|value| !value.is_empty()).ok_or_else(|| {
                    format!(
                        "The header `{name}` is read from the environment variable `{variable}`, which is not set. \
                         The user has to set it in the environment fieldtrip starts in."
                    )
                })?;
                    (apply(format.as_ref(), &value), Some(Secret(value)))
                }
                Source::Command {
                    argv,
                    directory,
                    format,
                    ttl,
                    cached,
                    last_run,
                } => {
                    let mut cached = cached.lock().await;
                    let reusable = cached.as_ref().filter(|(at, _)| !fresh && at.elapsed() < *ttl);
                    let output = if let Some((_, Secret(output))) = reusable {
                        output.clone()
                    } else {
                        let ran = run(name, argv, directory).await;
                        let outcome = match &ran {
                            Ok(_) => Outcome::Ok,
                            Err(failure) => Outcome::Failed(failure.short.clone()),
                        };
                        *last_run.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), outcome));
                        let output = ran.map_err(|failure| failure.message)?;
                        *cached = Some((Instant::now(), Secret(output.clone())));
                        output
                    };
                    drop(cached);
                    (apply(format.as_ref(), &output), Some(Secret(output)))
                }
            };
        let mut value = HeaderValue::from_str(&value).map_err(|_error| {
            format!("The value of the header `{name}` has characters an HTTP header cannot carry.")
        })?;
        value.set_sensitive(true);
        Ok(Resolved {
            name: self.wire_name.clone(),
            value,
            secret,
        })
    }
}

/// Reads the table form of a header: `{ env = "NAME" }` or `{ command = [...] }`, with an optional `format`.
fn source_from_table(name: &str, mut table: toml::Table, directory: &Path) -> Result<Source, String> {
    let format = match table.remove("format") {
        None => None,
        Some(toml::Value::String(format)) if format.contains("{}") => Some(Secret(format)),
        Some(toml::Value::String(_)) => {
            return Err(format!(
                "the `format` of header `{name}` has no `{{}}` placeholder for the value to go in"
            ));
        }
        Some(_) => return Err(format!("the `format` of header `{name}` must be a string")),
    };
    let ttl = match table.remove("ttl") {
        None => None,
        Some(toml::Value::Integer(seconds)) => {
            Some(Duration::from_secs(u64::try_from(seconds).map_err(|_error| {
                format!("the `ttl` of header `{name}` cannot be negative")
            })?))
        }
        Some(_) => {
            return Err(format!(
                "the `ttl` of header `{name}` must be a whole number of seconds"
            ));
        }
    };
    let env = table.remove("env");
    let command = table.remove("command");
    if let Some(unknown) = table.keys().next() {
        return Err(format!("header `{name}` has an unknown key `{unknown}`. {PLUGS}"));
    }
    match (env, command) {
        (Some(toml::Value::String(_)), None) if ttl.is_some() => {
            Err(format!("header `{name}` sets `ttl`, which only applies to a `command`"))
        }
        (Some(toml::Value::String(variable)), None) => Ok(Source::Env { variable, format }),
        (None, Some(command)) => {
            let argv = argv(name, command)?;
            Ok(Source::Command {
                argv,
                directory: directory.to_path_buf(),
                format,
                ttl: ttl.unwrap_or(DEFAULT_TTL),
                cached: tokio::sync::Mutex::new(None),
                last_run: Mutex::new(None),
            })
        }
        (Some(_), Some(_)) => Err(format!(
            "header `{name}` sets both `env` and `command`. Use one of them."
        )),
        _ => Err(expected_a_plug(name)),
    }
}

/// Reads `command`: a non-empty list of strings.
fn argv(name: &str, command: toml::Value) -> Result<Vec<Secret>, String> {
    let needs_a_list = || {
        format!(
            "the `command` of header `{name}` must be a list of strings, the program first and then its arguments. \
             It is run directly, not through a shell."
        )
    };
    let toml::Value::Array(items) = command else {
        return Err(needs_a_list());
    };
    let argv: Vec<Secret> = items
        .into_iter()
        .map(|item| {
            if let toml::Value::String(item) = item {
                Ok(Secret(item))
            } else {
                Err(needs_a_list())
            }
        })
        .collect::<Result<_, String>>()?;
    if argv.is_empty() { Err(needs_a_list()) } else { Ok(argv) }
}

/// Runs a header command and returns its trimmed stdout.
///
/// Errors name the header, the program, the exit status and stderr. They never include stdout or the arguments,
/// either of which can hold a secret.
async fn run(name: &str, argv: &[Secret], directory: &Path) -> Result<String, Failure> {
    let Some((Secret(program), arguments)) = argv.split_first() else {
        return Err(Failure::new(
            "empty command",
            format!("The header `{name}` has an empty command."),
        ));
    };
    let child = tokio::process::Command::new(program)
        .args(arguments.iter().map(|Secret(argument)| argument))
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            Failure::new(
                "could not be started",
                format!("The command for the header `{name}` could not be started: `{program}`: {error}"),
            )
        })?;
    let output = tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_elapsed| {
            Failure::new(
                format!("did not finish within {} s", COMMAND_TIMEOUT.as_secs()),
                format!(
                    "The command for the header `{name}` (`{program}`) did not finish within {} seconds.",
                    COMMAND_TIMEOUT.as_secs()
                ),
            )
        })?
        .map_err(|error| {
            Failure::new(
                "could not be waited for",
                format!("The command for the header `{name}` (`{program}`) could not be waited for: {error}"),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr: String = stderr.trim().chars().take(MAX_STDERR).collect();
        let detail = if stderr.is_empty() {
            "It wrote nothing to stderr.".to_owned()
        } else {
            format!("Its stderr: {stderr}")
        };
        let short = output
            .status
            .code()
            .map_or_else(|| "stopped by a signal".to_owned(), |code| format!("exit {code}"));
        return Err(Failure::new(
            short,
            format!(
                "The command for the header `{name}` (`{program}`) failed with {}. {detail}\nThe user has to fix \
                 this. It usually means signing in again.",
                output.status
            ),
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_error| {
        Failure::new(
            "printed something that is not text",
            format!("The command for the header `{name}` (`{program}`) printed something that is not text."),
        )
    })?;
    let value = stdout.trim();
    if value.is_empty() {
        return Err(Failure::new(
            "printed nothing",
            format!(
                "The command for the header `{name}` (`{program}`) succeeded but printed nothing. It has to print \
                 the header's value on stdout."
            ),
        ));
    }
    Ok(value.to_owned())
}

/// How long ago `at` was, roughly: `4 s ago`, `12 min ago` or `3 h ago`.
fn ago(at: Instant) -> String {
    let seconds = at.elapsed().as_secs();
    match seconds {
        0..120 => format!("{seconds} s ago"),
        120..7_200 => format!("{} min ago", seconds / 60),
        _ => format!("{} h ago", seconds / 3_600),
    }
}

/// The error for a header that is not one of the plugs.
fn expected_a_plug(name: &str) -> String {
    format!("header `{name}` is not one of the header plugs. {PLUGS}")
}

/// One sentence naming the three plugs.
const PLUGS: &str = "A header is a string, `{ env = \"NAME\" }` or `{ command = [\"program\", \"argument\"] }`, and \
     the last two take an optional `format`.";

/// Places `value` into `format`, or returns it as it is when there is no format.
fn apply(format: Option<&Secret>, value: &str) -> String {
    format.map_or_else(|| value.to_owned(), |Secret(format)| format.replace("{}", value))
}
