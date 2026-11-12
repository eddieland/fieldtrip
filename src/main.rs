//! The `fieldtrip` binary: an MCP server on stdio.

use std::process::ExitCode;

fn main() -> ExitCode {
    fieldtrip::run(std::env::args_os())
}
