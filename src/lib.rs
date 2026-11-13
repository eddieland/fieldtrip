//! GraphQL introspection and execution over MCP.
//!
//! Agents use `fieldtrip` to discover the real operations a GraphQL API exposes, plan and estimate the cost of
//! running them, and execute them only with the user's approval.

mod approval;
mod cli;
mod config;
mod costs;
mod headers;
mod http;
mod plan;
mod schema;
mod server;
mod suggest;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser as _;
use rmcp::ServiceExt as _;

use crate::cli::Args;
use crate::config::Config;
use crate::server::Fieldtrip;

/// Everything that can stop the server from starting or make it exit early.
#[derive(Debug, thiserror::Error)]
enum Error {
    /// The configuration could not be loaded.
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    /// The async runtime could not be created.
    #[error("could not start the async runtime: {0}")]
    Runtime(#[source] std::io::Error),
    /// The HTTP client could not be set up.
    #[error("{0}")]
    Http(String),
    /// The MCP session failed.
    #[error("the MCP session failed: {0}")]
    Session(String),
}

/// Parses `args`, loads the configuration and serves MCP on stdio until the client disconnects.
///
/// The first item of `args` is the program name, as in [`std::env::args_os`]. Diagnostics go to stderr, because
/// stdout carries the protocol.
pub fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            // `--help` and `--version` arrive here too, and clap routes them to stdout. That is fine: no MCP
            // session has started yet. Nothing useful can be done if the write itself fails.
            let _ignored = error.print();
            return if error.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };

    init_logging();

    match start(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Sends logs to stderr. `FIELDTRIP_LOG` sets the level for fieldtrip and the MCP SDK, and defaults to `warn`.
///
/// Every other crate stays at warnings whatever the setting, so the HTTP stack never gets to log a request, and
/// the headers on it, at a verbose level.
fn init_logging() {
    use tracing::Level;
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let level = std::env::var("FIELDTRIP_LOG")
        .ok()
        .and_then(|level| level.parse::<Level>().ok())
        .unwrap_or(Level::WARN);
    let targets = Targets::new()
        .with_default(level.min(Level::WARN))
        .with_target(env!("CARGO_CRATE_NAME"), level)
        .with_target("rmcp", level);
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_filter(targets);
    tracing_subscriber::registry().with(stderr).init();
}

/// Loads the configuration and blocks on the MCP session.
fn start(args: &Args) -> Result<(), Error> {
    let config = Config::load(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    runtime.block_on(serve(config))
}

/// Serves MCP on stdio until the client disconnects.
async fn serve(config: Config) -> Result<(), Error> {
    let service = Fieldtrip::new(config, http::Client::new().map_err(Error::Http)?)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| Error::Session(error.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|error| Error::Session(error.to_string()))?;
    Ok(())
}
