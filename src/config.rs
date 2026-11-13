//! The config file: which endpoints exist, where each schema comes from, and which headers each request carries.
//!
//! An endpoint can name a template, which holds the settings that several endpoints share, such as the dev,
//! staging and production instances of one service. There is one level: a template cannot name another. The URL,
//! the description and the approval settings are never taken from a template, so an endpoint that leaves them out
//! falls back to the defaults, which ask before every operation.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::HeaderName;
use serde::Deserialize;

use crate::cli::Args;
use crate::costs::CostMap;
use crate::headers::Header;

/// Why the configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// Neither `--config` nor a home directory to find the default file in.
    #[error("no --config given and no home directory to look for the default config file in")]
    NoDefaultPath,
    /// No flags, and no config file at the default location.
    #[error(
        "there is no config file at {path}. Create one, pass --config <path>, or pass --url <url> to serve a single \
         endpoint without a config file"
    )]
    NothingConfigured {
        /// The default location that was looked at.
        path: PathBuf,
    },
    /// `--url` or `--schema` has a value the server cannot use.
    #[error("the command line is not usable: {0}")]
    Flags(String),
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
    /// An endpoint or a template has a value the server cannot use.
    #[error("{what} in {path}: {problem}")]
    Invalid {
        /// The file the endpoint or template is defined in.
        path: PathBuf,
        /// `` endpoint `name` `` or `` template `name` ``.
        what: String,
        /// What is wrong with it.
        problem: String,
    },
}

/// The loaded configuration.
#[derive(Debug)]
pub(crate) struct Config {
    /// Where the configuration came from: the config file, or `None` for `--url`.
    pub(crate) path: Option<PathBuf>,
    /// Endpoints by name, in name order.
    pub(crate) endpoints: BTreeMap<String, Endpoint>,
}

/// One GraphQL endpoint.
#[derive(Debug)]
pub(crate) struct Endpoint {
    /// Where operations are sent, exactly as configured. It can carry a key in its query, so it is never shown:
    /// [`Endpoint::shown_url`] is.
    pub(crate) url: String,
    /// The URL with the value of every query parameter replaced, for showing in output, prompts and errors.
    pub(crate) shown_url: String,
    /// What the user says the endpoint is for, on one line.
    pub(crate) description: Option<String>,
    /// The template the endpoint takes its shared settings from.
    pub(crate) template: Option<String>,
    /// The SDL file to read the schema from, as an absolute path. `None` means introspection.
    pub(crate) schema: Option<PathBuf>,
    /// The headers sent with every request to this endpoint, in name order. A header that comes from a template is
    /// shared with the template's other endpoints, and so is a command's cached output.
    pub(crate) headers: Vec<Arc<Header>>,
    /// How many items to assume for a list whose size nothing bounds, when estimating cost.
    pub(crate) assumed_list_size: u64,
    /// The rules that replace the built-in costs for this endpoint. Empty when it has no cost map.
    pub(crate) costs: Arc<CostMap>,
    /// How long a request to this endpoint may take.
    pub(crate) timeout: Duration,
    /// The response headers `execute` shows, as written in the config. Only the user chooses them, because some
    /// carry values the calling agent should not see.
    pub(crate) response_headers: Vec<String>,
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

/// The name of the endpoint that `--url` defines.
const FLAG_ENDPOINT: &str = "default";

/// The `timeout` of an endpoint that does not set one.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The `assumed_list_size` of an endpoint that does not set one.
const DEFAULT_ASSUMED_LIST_SIZE: u64 = 10;

/// The headers fieldtrip sets itself. Configuring one would send it twice, or point the request elsewhere.
const RESERVED_HEADERS: [&str; 6] = [
    "accept",
    "connection",
    "content-length",
    "content-type",
    "host",
    "transfer-encoding",
];

/// What replaces the value of each query parameter in a URL that is shown.
const HIDDEN: &str = "<redacted>";

/// The file as written, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    endpoints: BTreeMap<String, RawEndpoint>,
    #[serde(default)]
    templates: BTreeMap<String, RawTemplate>,
}

/// One `[endpoints.NAME]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEndpoint {
    url: String,
    description: Option<String>,
    template: Option<String>,
    approval: Option<String>,
    auto_approve_cost: Option<u64>,
    schema: Option<PathBuf>,
    #[serde(default)]
    headers: BTreeMap<String, toml::Value>,
    assumed_list_size: Option<u64>,
    costs: Option<PathBuf>,
    timeout: Option<u64>,
    response_headers: Option<Vec<String>>,
}

/// One `[templates.NAME]` table as written.
///
/// The settings a template cannot hold are read too, so that the error can say why instead of only that the key
/// is unknown.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplate {
    url: Option<toml::Value>,
    description: Option<toml::Value>,
    template: Option<toml::Value>,
    approval: Option<toml::Value>,
    auto_approve_cost: Option<toml::Value>,
    schema: Option<PathBuf>,
    #[serde(default)]
    headers: BTreeMap<String, toml::Value>,
    assumed_list_size: Option<u64>,
    costs: Option<PathBuf>,
    timeout: Option<u64>,
    response_headers: Option<Vec<String>>,
}

/// The settings an endpoint can take from a template. serde cannot flatten them into a table that denies
/// unknown keys, so both tables list them and hand them over in this.
#[derive(Default)]
struct RawShared {
    schema: Option<PathBuf>,
    headers: BTreeMap<String, toml::Value>,
    assumed_list_size: Option<u64>,
    costs: Option<PathBuf>,
    timeout: Option<u64>,
    response_headers: Option<Vec<String>>,
}

/// Moves the shared settings out of a raw endpoint or template.
macro_rules! shared {
    ($raw:expr) => {
        RawShared {
            schema: $raw.schema,
            headers: $raw.headers,
            assumed_list_size: $raw.assumed_list_size,
            costs: $raw.costs,
            timeout: $raw.timeout,
            response_headers: $raw.response_headers,
        }
    };
}

/// A template, checked and ready to be applied to the endpoints that name it.
#[derive(Debug, Default)]
struct Template {
    schema: Option<PathBuf>,
    headers: Vec<Arc<Header>>,
    assumed_list_size: Option<u64>,
    costs: Option<Arc<CostMap>>,
    timeout: Option<Duration>,
    response_headers: Option<Vec<String>>,
}

impl Config {
    /// Loads the config file named by `--config` or the default one, or builds a configuration from `--url`.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the file is missing, is not valid TOML, or describes an unusable endpoint.
    pub(crate) fn load(args: &Args) -> Result<Self, ConfigError> {
        if let Some(url) = &args.url {
            return Self::from_flags(url, args.schema.as_ref());
        }
        let (path, is_default) = match &args.config {
            Some(path) => (path.clone(), false),
            None => (default_path().ok_or(ConfigError::NoDefaultPath)?, true),
        };
        // Paths in the file are relative to its directory, and header commands run there. A relative `--config`
        // has an empty directory, which no command can start in, so it is made absolute first.
        let path = std::path::absolute(&path).unwrap_or(path);
        let text = std::fs::read_to_string(&path).map_err(|source| {
            if is_default && source.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NothingConfigured { path: path.clone() }
            } else {
                ConfigError::Read {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        Self::parse(&text, &path)
    }

    /// Builds the configuration for `--url`, with an optional `--schema`: one endpoint called `default`.
    fn from_flags(url: &str, schema: Option<&PathBuf>) -> Result<Self, ConfigError> {
        let raw = RawEndpoint {
            url: url.to_owned(),
            schema: schema.cloned(),
            ..RawEndpoint::default()
        };
        // A relative `--schema` is relative to where the server was started, as any command-line path is.
        let base = std::env::current_dir().unwrap_or_else(|_error| PathBuf::from("."));
        let endpoint = Endpoint::from_raw(raw, &base, &BTreeMap::new()).map_err(ConfigError::Flags)?;
        Ok(Self {
            path: None,
            endpoints: BTreeMap::from([(FLAG_ENDPOINT.to_owned(), endpoint)]),
        })
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
        let invalid = |what: String| {
            move |problem| ConfigError::Invalid {
                path: path.to_path_buf(),
                what,
                problem,
            }
        };
        let templates = raw
            .templates
            .into_iter()
            .map(|(name, template)| {
                let template = Template::from_raw(template, base).map_err(invalid(format!("template `{name}`")))?;
                Ok((name, template))
            })
            .collect::<Result<BTreeMap<_, _>, ConfigError>>()?;
        let endpoints = raw
            .endpoints
            .into_iter()
            .map(|(name, endpoint)| {
                let endpoint = check_endpoint_name(&name)
                    .and_then(|()| Endpoint::from_raw(endpoint, base, &templates))
                    .map_err(invalid(format!("endpoint `{name}`")))?;
                Ok((name, endpoint))
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            endpoints,
        })
    }
}

impl Template {
    /// Checks one template. Paths in it are resolved against `base`, the config file's directory.
    fn from_raw(raw: RawTemplate, base: &Path) -> Result<Self, String> {
        let never_shared = [
            ("url", raw.url.is_some(), "each endpoint names its own"),
            (
                "description",
                raw.description.is_some(),
                "each endpoint says what it is for",
            ),
            (
                "template",
                raw.template.is_some(),
                "a template cannot name another template",
            ),
            (
                "approval",
                raw.approval.is_some(),
                "each endpoint sets its own, so one that leaves it out still asks",
            ),
            (
                "auto_approve_cost",
                raw.auto_approve_cost.is_some(),
                "each endpoint sets its own, so one that leaves it out still asks",
            ),
        ];
        if let Some((key, _, why)) = never_shared.into_iter().find(|(_, present, _)| *present) {
            return Err(format!("`{key}` cannot be set in a template: {why}"));
        }
        let shared = shared!(raw);
        Ok(Self {
            schema: schema_path(shared.schema.as_deref(), base)?,
            headers: headers(shared.headers, base)?,
            assumed_list_size: assumed_list_size(shared.assumed_list_size)?,
            costs: shared
                .costs
                .map(|costs| CostMap::load(&resolve(&costs, base)).map(Arc::new))
                .transpose()?,
            timeout: timeout(shared.timeout)?,
            response_headers: response_headers(shared.response_headers)?,
        })
    }
}

impl Endpoint {
    /// Validates one endpoint and applies its template. A relative path is resolved against `base`, the config
    /// file's directory.
    fn from_raw(raw: RawEndpoint, base: &Path, templates: &BTreeMap<String, Template>) -> Result<Self, String> {
        let shown_url = check_url(&raw.url)?;
        let template = match &raw.template {
            None => &Template::default(),
            Some(name) => templates.get(name).ok_or_else(|| {
                let known: Vec<String> = templates.keys().map(|name| format!("`{name}`")).collect();
                if known.is_empty() {
                    format!("`template` names `{name}`, but the config file defines no templates")
                } else {
                    format!(
                        "`template` names `{name}`, which is not defined. Templates: {}",
                        known.join(", ")
                    )
                }
            })?,
        };
        let approval = match raw.approval.as_deref() {
            None | Some("elicit") => Approval::Elicit,
            Some("client") => Approval::Client,
            Some(other) => return Err(format!("`approval` must be \"elicit\" or \"client\", got \"{other}\"")),
        };
        if approval == Approval::Client && raw.auto_approve_cost.is_some() {
            return Err(
                "`auto_approve_cost` has no effect with `approval = \"client\"`, which sends every operation without \
                 asking. Remove one of them."
                    .to_owned(),
            );
        }
        let shared = shared!(raw);

        // The endpoint's own headers win over the template's, compared ignoring case as HTTP does.
        let mut headers = headers(shared.headers, base)?;
        let own: Vec<String> = headers.iter().map(|header| header.wire_name().to_owned()).collect();
        headers.extend(
            template
                .headers
                .iter()
                .filter(|header| !own.iter().any(|name| name == header.wire_name()))
                .cloned(),
        );
        headers.sort_by(|left, right| left.name.cmp(&right.name));

        let costs = match &shared.costs {
            Some(path) => Arc::new(CostMap::load(&resolve(path, base))?),
            None => template.costs.clone().unwrap_or_default(),
        };
        Ok(Self {
            shown_url,
            description: raw.description.as_deref().map(one_line).filter(|text| !text.is_empty()),
            template: raw.template,
            schema: schema_path(shared.schema.as_deref(), base)?.or_else(|| template.schema.clone()),
            headers,
            assumed_list_size: assumed_list_size(shared.assumed_list_size)?
                .or(template.assumed_list_size)
                .unwrap_or(DEFAULT_ASSUMED_LIST_SIZE),
            costs,
            timeout: timeout(shared.timeout)?.or(template.timeout).unwrap_or(DEFAULT_TIMEOUT),
            response_headers: response_headers(shared.response_headers)?
                .or_else(|| template.response_headers.clone())
                .unwrap_or_default(),
            approval,
            auto_approve_cost: raw.auto_approve_cost,
            url: raw.url,
        })
    }
}

/// Checks that `url` is an HTTP URL with a host and no credentials, and returns it as it may be shown.
fn check_url(url: &str) -> Result<String, String> {
    let not_http = || format!("`url` must start with http:// or https://, got `{}`", shown(url));
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(not_http());
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|error| format!("`url` is not a usable URL ({error}): `{}`", shown(url)))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(not_http());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(format!("`url` has no host: `{}`", shown(url)));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "`url` has a user name or password in it. Send credentials in a header instead, such as \
             `Authorization = { env = \"NAME\", format = \"Basic {}\" }`, where they are kept out of every tool \
             result."
                .to_owned(),
        );
    }
    Ok(shown(url))
}

/// `url` with the value of every query parameter replaced by [`HIDDEN`], and any fragment dropped. Keys stay, so
/// the user can tell which parameters are set.
fn shown(url: &str) -> String {
    let without_fragment = url.split_once('#').map_or(url, |(before, _)| before);
    let Some((address, query)) = without_fragment.split_once('?') else {
        return without_fragment.to_owned();
    };
    let mut out = format!("{address}?");
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            out.push('&');
        }
        let key = pair.split_once('=').map_or(pair, |(key, _)| key);
        let _ = write!(out, "{key}={HIDDEN}");
    }
    out
}

/// Checks an endpoint's name. The calling agent types it into every tool call, so it is kept plain.
fn check_endpoint_name(name: &str) -> Result<(), String> {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'));
    if plain {
        Ok(())
    } else {
        Err("an endpoint name may only have letters, digits, `-`, `_` and `.`, such as `acme-prod`".to_owned())
    }
}

/// Reads a `headers` table. Two names that differ only in case are an error, since HTTP treats them as one.
fn headers(table: BTreeMap<String, toml::Value>, base: &Path) -> Result<Vec<Arc<Header>>, String> {
    let mut headers: Vec<Arc<Header>> = Vec::with_capacity(table.len());
    for (name, value) in table {
        let header = Header::from_config(name, value, base)?;
        if RESERVED_HEADERS.contains(&header.wire_name()) {
            return Err(format!(
                "header `{}` is set by fieldtrip itself and cannot be configured",
                header.name
            ));
        }
        if let Some(earlier) = headers.iter().find(|earlier| earlier.wire_name() == header.wire_name()) {
            return Err(format!(
                "headers `{}` and `{}` are the same header, because header names ignore case. Keep one of them.",
                earlier.name, header.name
            ));
        }
        headers.push(Arc::new(header));
    }
    Ok(headers)
}

/// Resolves a configured `schema` path, and checks that the file is there.
fn schema_path(schema: Option<&Path>, base: &Path) -> Result<Option<PathBuf>, String> {
    let Some(schema) = schema else { return Ok(None) };
    let path = resolve(schema, base);
    if path.is_file() {
        Ok(Some(path))
    } else {
        Err(format!(
            "`schema` names {}, which is not a file. Export the schema there, or remove `schema` to load it by \
             introspection.",
            path.display()
        ))
    }
}

/// Checks `assumed_list_size`.
fn assumed_list_size(size: Option<u64>) -> Result<Option<u64>, String> {
    if size == Some(0) {
        return Err(
            "`assumed_list_size` must be at least 1. A list of unknown size assumed empty would make any query \
             look free."
                .to_owned(),
        );
    }
    Ok(size)
}

/// Checks `response_headers`: each must be a header name, and no two may differ only in case.
fn response_headers(names: Option<Vec<String>>) -> Result<Option<Vec<String>>, String> {
    let Some(names) = names else { return Ok(None) };
    let mut seen: Vec<HeaderName> = Vec::with_capacity(names.len());
    for name in &names {
        let parsed = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_error| format!("`response_headers` has `{name}`, which is not a header name"))?;
        if seen.contains(&parsed) {
            return Err(format!(
                "`response_headers` names `{name}` twice, because header names ignore case. Keep one of them."
            ));
        }
        seen.push(parsed);
    }
    Ok(Some(names))
}

/// Checks `timeout`.
fn timeout(seconds: Option<u64>) -> Result<Option<Duration>, String> {
    if seconds == Some(0) {
        return Err("`timeout` is in seconds and must be at least 1".to_owned());
    }
    Ok(seconds.map(Duration::from_secs))
}

/// Resolves a configured path: `~/` is the home directory, and a relative path is relative to `base`.
fn resolve(path: &Path, base: &Path) -> PathBuf {
    let mut components = path.components();
    if components.next() == Some(Component::Normal("~".as_ref()))
        && let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
    {
        return PathBuf::from(home).join(components.as_path());
    }
    base.join(path)
}

/// `text` on one line, with runs of whitespace collapsed.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
