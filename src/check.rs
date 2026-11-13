//! `fieldtrip check`: the configuration as each endpoint resolves it, for the user to read.
//!
//! It answers what an MCP client hides: whether the config loads, what an endpoint takes from its template,
//! whether every header can be produced, and whether a cost map fits the schema. It never prints a header value,
//! and it sends nothing to any endpoint.

use std::fmt::Write as _;
use std::path::Path;

use crate::approval;
use crate::config::{Config, Endpoint};
use crate::schema::Loaded;

/// What the check found.
#[derive(Debug)]
pub(crate) struct Report {
    /// The report, to print as it is.
    pub(crate) text: String,
    /// How many things are wrong. Any problem makes the check fail.
    pub(crate) problems: usize,
}

/// Checks every endpoint in `config`.
pub(crate) async fn check(config: &Config) -> Report {
    let mut text = config.path.as_ref().map_or_else(
        || {
            "Config: from --url
"
            .to_owned()
        },
        |path| {
            format!(
                "Config: {}
",
                path.display()
            )
        },
    );
    let mut problems = 0;
    let mut warnings = 0;
    if config.endpoints.is_empty() {
        text.push_str("\nNo endpoints are configured.\n");
        problems += 1;
    }
    for (name, endpoint) in &config.endpoints {
        let mut out = Out::default();
        endpoint_report(&mut out, name, endpoint).await;
        text.push_str(&out.text);
        problems += out.problems;
        warnings += out.warnings;
    }
    let plural = |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let _ = writeln!(
        text,
        "\n{}, {}.",
        plural(problems, "problem"),
        plural(warnings, "warning")
    );
    Report { text, problems }
}

/// The report for one endpoint, as it is being written.
#[derive(Debug, Default)]
struct Out {
    text: String,
    problems: usize,
    warnings: usize,
}

impl Out {
    /// Writes `label: value`.
    fn line(&mut self, label: &str, value: impl std::fmt::Display) {
        let _ = writeln!(self.text, "  {label}: {value}");
    }

    /// Writes `label: what: PROBLEM: problem`, and counts it.
    fn problem(&mut self, label: &str, what: &str, problem: &str) {
        let _ = writeln!(self.text, "  {label}: {what}: PROBLEM: {problem}");
        self.problems += 1;
    }

    /// Writes `label: what: warning: warning`, and counts it.
    fn warning(&mut self, label: &str, what: &str, warning: &str) {
        let _ = writeln!(self.text, "  {label}: {what}: warning: {warning}");
        self.warnings += 1;
    }
}

/// Writes what one endpoint resolves to, checking its schema file, cost map and headers.
async fn endpoint_report(out: &mut Out, name: &str, endpoint: &Endpoint) {
    let _ = writeln!(out.text, "\n{name}");
    if let Some(description) = &endpoint.description {
        out.line("description", description);
    }
    if let Some(template) = &endpoint.template {
        out.line("template", template);
    }
    out.line("url", &endpoint.shown_url);
    out.line("approval", approval::policy(endpoint));
    out.line("timeout", format!("{}s", endpoint.timeout.as_secs()));
    out.line("assumed list size", endpoint.assumed_list_size);

    let schema = match &endpoint.schema {
        None => {
            out.line(
                "schema",
                "introspection, not checked: check sends nothing to the endpoint",
            );
            None
        }
        Some(path) => {
            let what = format!("file {}", path.display());
            match read_schema(path) {
                Ok(loaded) => {
                    out.line("schema", format!("{what}: ok"));
                    Some(loaded)
                }
                Err(problem) => {
                    out.problem("schema", &what, &problem);
                    None
                }
            }
        }
    };

    if let Some(source) = &endpoint.costs.source {
        let rules = endpoint.costs.len();
        let what = format!(
            "{} ({rules} rule{})",
            source.display(),
            if rules == 1 { "" } else { "s" }
        );
        match &schema {
            None => out.line("cost map", format!("{what}: not checked against a schema")),
            Some(loaded) => {
                let findings = loaded.cost_map_findings(&endpoint.costs);
                if findings.is_empty() {
                    out.line("cost map", format!("{what}: ok"));
                }
                // A map shared by several instances of a service can have rules that only some of them match,
                // so these are warnings, not problems.
                for finding in findings {
                    out.warning("cost map", &what, &finding);
                }
            }
        }
    }

    for header in &endpoint.headers {
        let label = format!("header {}", header.name);
        let what = header.source();
        match header.resolve(false).await {
            Ok(_resolved) => out.line(&label, format!("{what}: ok")),
            Err(problem) => out.problem(&label, &what, &problem),
        }
    }
}

/// Reads and validates a schema file.
fn read_schema(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("could not read it: {error}"))?;
    Loaded::from_sdl(&text, "it")
}
