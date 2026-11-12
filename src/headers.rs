//! The header plugs: where the value of each configured header comes from.
//!
//! There are three: a literal, an environment variable, and a command. Values are secrets. They are resolved when
//! a request is made and never shown to the calling agent, the approval prompt or the log.

use std::fmt;

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
    Command,
}

impl Header {
    /// Reads one entry of an endpoint's `headers` table.
    ///
    /// # Errors
    ///
    /// Returns a message when the entry is not one of the three plugs. The message never quotes a value.
    pub(crate) fn from_config(name: String, value: toml::Value) -> Result<Self, String> {
        let wire_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_error| format!("`{name}` is not a valid HTTP header name"))?;
        let source = match value {
            toml::Value::String(literal) => Source::Literal(Secret(literal)),
            toml::Value::Table(table) => source_from_table(&name, table)?,
            _ => return Err(expected_a_plug(&name)),
        };
        Ok(Self {
            name,
            wire_name,
            source,
        })
    }

    /// Produces the header to send.
    ///
    /// # Errors
    ///
    /// Returns a message that names the header and what it is waiting for, never a value.
    pub(crate) fn resolve(&self) -> Result<(HeaderName, HeaderValue), String> {
        let name = &self.name;
        let value =
            match &self.source {
                Source::Literal(Secret(literal)) => literal.clone(),
                Source::Env { variable, format } => {
                    let value = std::env::var(variable).ok().filter(|value| !value.is_empty()).ok_or_else(|| {
                    format!(
                        "The header `{name}` is read from the environment variable `{variable}`, which is not set. \
                         The user has to set it in the environment fieldtrip starts in."
                    )
                })?;
                    apply(format.as_ref(), &value)
                }
                Source::Command => {
                    return Err(format!(
                        "The header `{name}` uses a command, which is not available yet."
                    ));
                }
            };
        let mut value = HeaderValue::from_str(&value).map_err(|_error| {
            format!("The value of the header `{name}` has characters an HTTP header cannot carry.")
        })?;
        value.set_sensitive(true);
        Ok((self.wire_name.clone(), value))
    }
}

/// Reads the table form of a header: `{ env = "NAME" }` or `{ command = [...] }`, with an optional `format`.
fn source_from_table(name: &str, mut table: toml::Table) -> Result<Source, String> {
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
    let env = table.remove("env");
    let command = table.remove("command");
    if let Some(unknown) = table.keys().next() {
        return Err(format!("header `{name}` has an unknown key `{unknown}`. {PLUGS}"));
    }
    match (env, command) {
        (Some(toml::Value::String(variable)), None) => Ok(Source::Env { variable, format }),
        (None, Some(toml::Value::Array(_))) => Ok(Source::Command),
        (Some(_), Some(_)) => Err(format!(
            "header `{name}` sets both `env` and `command`. Use one of them."
        )),
        _ => Err(expected_a_plug(name)),
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
