//! The config file: which endpoints exist, where each schema comes from, and which headers each request carries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::cli::Args;
use crate::headers::Header;

/// Why the configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// Neither `--config` nor a home directory to find the default file in.
    #[error("no --config given and no home directory to look for the default config file in")]
    NoDefaultPath,
    /// The file could not be read.
    #[error("could not read the config file {path}: {source}")]
    Read {
        /// The file that was being read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file is not valid TOML, or does not have the expected shape.
    #[error("the config file {path} is not valid{location}: {message}")]
    Parse {
        /// The file that was being parsed.
        path: PathBuf,
        /// ` at line N`, or empty when the parser gave no position.
        location: String,
        /// The parser's message, without the source snippet, which could contain a header value.
        message: String,
    },
    /// An endpoint has a value the server cannot use.
    #[error("endpoint `{endpoint}` in {path}: {problem}")]
    Endpoint {
        /// The file the endpoint is defined in.
        path: PathBuf,
        /// The endpoint's name.
        endpoint: String,
        /// What is wrong with it.
        problem: String,
    },
}

/// The loaded configuration.
#[derive(Debug)]
pub(crate) struct Config {
    /// Endpoints by name, in name order.
    pub(crate) endpoints: BTreeMap<String, Endpoint>,
}

/// One GraphQL endpoint.
#[derive(Debug)]
pub(crate) struct Endpoint {
    /// Where operations are sent.
    pub(crate) url: String,
    /// The SDL file to read the schema from, as an absolute path. `None` means introspection.
    pub(crate) schema: Option<PathBuf>,
    /// The headers sent with every request to this endpoint, in name order.
    pub(crate) headers: Vec<Header>,
    /// How many items to assume for a list whose size nothing bounds, when estimating cost.
    pub(crate) assumed_list_size: u64,
    /// How long a request to this endpoint may take.
    pub(crate) timeout: Duration,
    /// Who approves an operation before it is sent.
    pub(crate) approval: Approval,
    /// Queries with an estimated cost at or below this are sent without asking. Mutations always ask.
    pub(crate) auto_approve_cost: Option<u64>,
}

/// Who approves an operation before it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Approval {
    /// fieldtrip asks the user through MCP elicitation.
    Elicit,
    /// The MCP client's own tool approval is relied on, and fieldtrip does not ask.
    Client,
}

/// The `timeout` of an endpoint that does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The `assumed_list_size` of an endpoint that does not set one.
const DEFAULT_ASSUMED_LIST_SIZE: u64 = 10;

/// The file as written, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    endpoints: BTreeMap<String, RawEndpoint>,
}

/// One `[endpoints.NAME]` table as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEndpoint {
    url: String,
    schema: Option<PathBuf>,
    #[serde(default)]
    headers: BTreeMap<String, toml::Value>,
    assumed_list_size: Option<u64>,
    approval: Option<String>,
    auto_approve_cost: Option<u64>,
    timeout: Option<u64>,
}

impl Config {
    /// Loads the config file named by `--config`, or the default one.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the file is missing, is not valid TOML, or describes an unusable endpoint.
    pub(crate) fn load(args: &Args) -> Result<Self, ConfigError> {
        let path = match &args.config {
            Some(path) => path.clone(),
            None => default_path().ok_or(ConfigError::NoDefaultPath)?,
        };
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, &path)
    }

    /// Parses and validates the text of the config file at `path`.
    fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|error| ConfigError::Parse {
            path: path.to_path_buf(),
            location: error
                .span()
                .map_or_else(String::new, |span| format!(" at line {}", line_of(text, span.start))),
            message: error.message().to_owned(),
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let endpoints = raw
            .endpoints
            .into_iter()
            .map(|(name, endpoint)| {
                let endpoint = Endpoint::from_raw(endpoint, base).map_err(|problem| ConfigError::Endpoint {
                    path: path.to_path_buf(),
                    endpoint: name.clone(),
                    problem,
                })?;
                Ok((name, endpoint))
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self { endpoints })
    }
}

impl Endpoint {
    /// Validates one endpoint. A relative `schema` path is resolved against `base`, the config file's directory.
    fn from_raw(raw: RawEndpoint, base: &Path) -> Result<Self, String> {
        if !(raw.url.starts_with("http://") || raw.url.starts_with("https://")) {
            return Err(format!("`url` must start with http:// or https://, got `{}`", raw.url));
        }
        let headers = raw
            .headers
            .into_iter()
            .map(|(name, value)| Header::from_config(name, value, base))
            .collect::<Result<_, String>>()?;
        let approval = match raw.approval.as_deref() {
            None | Some("elicit") => Approval::Elicit,
            Some("client") => Approval::Client,
            Some(other) => return Err(format!("`approval` must be \"elicit\" or \"client\", got \"{other}\"")),
        };
        if raw.timeout == Some(0) {
            return Err("`timeout` is in seconds and must be at least 1".to_owned());
        }
        Ok(Self {
            timeout: raw.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs),
            approval,
            auto_approve_cost: raw.auto_approve_cost,
            url: raw.url,
            schema: raw.schema.map(|schema| base.join(schema)),
            headers,
            assumed_list_size: raw.assumed_list_size.unwrap_or(DEFAULT_ASSUMED_LIST_SIZE),
        })
    }
}

/// `$XDG_CONFIG_HOME/fieldtrip/config.toml`, falling back to `~/.config/fieldtrip/config.toml`.
fn default_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(config_home.join("fieldtrip").join("config.toml"))
}

/// The 1-based line number of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> usize {
    text.bytes().take(offset).filter(|byte| *byte == b'\n').count() + 1
}
