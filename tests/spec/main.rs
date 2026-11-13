//! The spec: one module per slice in `PLAN.md`.
//!
//! Every test starts the real `fieldtrip` binary and talks to it as an MCP client over stdio, so the tests see
//! exactly what a calling agent sees. `cargo test <module>` runs one slice.

mod support;

// Integration tests are always built with `cfg(test)`. Saying so keeps `clippy::tests_outside_test_module` satisfied.
#[cfg(test)]
mod approval_policy;
#[cfg(test)]
mod check_command;
#[cfg(test)]
mod cli_flags;
#[cfg(test)]
mod config_checks;
#[cfg(test)]
mod cost_argument;
#[cfg(test)]
mod cost_checks;
#[cfg(test)]
mod cost_map;
#[cfg(test)]
mod cost_patterns;
#[cfg(test)]
mod describe;
#[cfg(test)]
mod descriptions;
#[cfg(test)]
mod estimate_cost;
#[cfg(test)]
mod execute_approved;
#[cfg(test)]
mod execute_refused;
#[cfg(test)]
mod execute_save;
#[cfg(test)]
mod execute_select;
#[cfg(test)]
mod header_names;
#[cfg(test)]
mod headers_command;
#[cfg(test)]
mod headers_refresh;
#[cfg(test)]
mod headers_static;
#[cfg(test)]
mod introspect_file;
#[cfg(test)]
mod introspect_http;
#[cfg(test)]
mod large_schema;
#[cfg(test)]
mod limits;
#[cfg(test)]
mod list_endpoints;
#[cfg(test)]
mod mutations_denied;
#[cfg(test)]
mod pagination_hints;
#[cfg(test)]
mod protocol_2026;
#[cfg(test)]
mod redaction;
#[cfg(test)]
mod response_limits;
#[cfg(test)]
mod response_metadata;
#[cfg(test)]
mod response_summary;
#[cfg(test)]
mod search_schema;
#[cfg(test)]
mod templates;
#[cfg(test)]
mod tool_descriptions;
#[cfg(test)]
mod url_secrets;
