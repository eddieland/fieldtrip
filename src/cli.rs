//! Command-line flags.

use std::path::PathBuf;

/// GraphQL introspection and execution over MCP, served on stdio.
#[derive(Debug, clap::Parser)]
#[command(version, about)]
pub(crate) struct Args {
    /// Path to the config file. Defaults to `~/.config/fieldtrip/config.toml`.
    #[arg(long, value_name = "PATH", global = true)]
    pub(crate) config: Option<PathBuf>,

    /// Serve one endpoint at this URL, with no headers, instead of reading a config file.
    #[arg(long, value_name = "URL", conflicts_with = "config", global = true)]
    pub(crate) url: Option<String>,

    /// With `--url`: read the schema from this SDL file instead of introspecting the endpoint.
    #[arg(long, value_name = "PATH", requires = "url", global = true)]
    pub(crate) schema: Option<PathBuf>,

    /// What to do instead of serving MCP.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// What to do instead of serving MCP.
#[derive(Debug, Clone, Copy, clap::Subcommand)]
pub(crate) enum Command {
    /// Check the configuration and print what each endpoint resolves to, then exit.
    ///
    /// Every header is resolved, so `env` variables are read and commands run, but no value is printed. Schema
    /// files are read, and cost maps are checked against them. Nothing is sent to any endpoint. The exit status is
    /// 0 when nothing is wrong.
    Check,
}
