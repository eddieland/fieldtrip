//! The spec: one module per slice in `PLAN.md`.
//!
//! Every test starts the real `fieldtrip` binary and talks to it as an MCP client over stdio, so the tests see
//! exactly what a calling agent sees. `cargo test <module>` runs one slice.

mod support;

// Integration tests are always built with `cfg(test)`. Saying so keeps `clippy::tests_outside_test_module` satisfied.
#[cfg(test)]
mod approval_policy;
#[cfg(test)]
mod describe;
#[cfg(test)]
mod estimate_cost;
#[cfg(test)]
mod execute_approved;
#[cfg(test)]
mod execute_refused;
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
mod list_endpoints;
#[cfg(test)]
mod redaction;
#[cfg(test)]
mod search_schema;
