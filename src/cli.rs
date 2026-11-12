//! Command-line flags.

use std::path::PathBuf;

/// GraphQL introspection and execution over MCP, served on stdio.
#[derive(Debug, clap::Parser)]
#[command(version, about)]
pub(crate) struct Args {
    /// Path to the config file. Defaults to `~/.config/fieldtrip/config.toml`.
    #[arg(long, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
}
