//! Command-line flags.

use std::path::PathBuf;

/// GraphQL introspection and execution over MCP, served on stdio.
#[derive(Debug, clap::Parser)]
#[command(version, about)]
pub(crate) struct Args {
    /// Path to the config file. Defaults to `~/.config/fieldtrip/config.toml`.
    #[arg(long, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,

    /// Serve one endpoint at this URL, with no headers, instead of reading a config file.
    #[arg(long, value_name = "URL", conflicts_with = "config")]
    pub(crate) url: Option<String>,

    /// With `--url`: read the schema from this SDL file instead of introspecting the endpoint.
    #[arg(long, value_name = "PATH", requires = "url")]
    pub(crate) schema: Option<PathBuf>,
}
