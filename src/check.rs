//! `fieldtrip check`: the configuration as each endpoint resolves it, for the user to read.
//!
//! It answers what an MCP client hides: whether the config loads, what an endpoint takes from its template,
//! whether every header can be produced, and whether a cost map, a glossary and the saved operations fit the schema. It never
//! prints a header value. It sends nothing to any endpoint unless the user passes `--introspect`: for a schema that
//! comes from introspection it reads the copy in fieldtrip's cache.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::{Config, Endpoint, Mutations, show_bytes, show_duration};
use crate::schema::{Loaded, introspection};
use crate::{approval, cache, http, results};

/// What the check found.
#[derive(Debug)]
pub(crate) struct Report {
    /// The report, to print as it is.
    pub(crate) text: String,
    /// How many things are wrong. Any problem makes the check fail.
    pub(crate) problems: usize,
}

/// Checks every endpoint in `config`. With `http`, which the user asks for with `--introspect`, an endpoint that has
/// no schema file is introspected. Without it nothing is sent.
pub(crate) async fn check(config: &Config, http: Option<&http::Client>) -> Report {
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
    if let Some(log) = &config.audit_log {
        let _ = writeln!(text, "Audit log: {}", log.display());
    }
    let _ = writeln!(
        text,
        "Result files: {}, deleted after {}",
        results::root().display(),
        show_duration(config.result_ttl)
    );
    let mut problems = 0;
    let mut warnings = 0;
    if config.endpoints.is_empty() {
        text.push_str("\nNo endpoints are configured.\n");
        problems += 1;
    }
    for (name, endpoint) in &config.endpoints {
        let mut out = Out::default();
        endpoint_report(&mut out, name, endpoint, http).await;
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

/// The schema a cost map and the saved operations are checked against.
struct Against {
    schema: Loaded,
    /// Where the schema came from when it is not the endpoint's file, to end a line with: `: checked against ...`.
    note: String,
}

/// Finds the schema of an endpoint that has no schema file: by introspection when `http` allows it, else in the cache.
async fn introspected_schema(out: &mut Out, endpoint: &Endpoint, http: Option<&http::Client>) -> Option<Against> {
    let caching = !endpoint.schema_ttl.is_zero();
    let Some(http) = http else {
        // A copy of any age is better than no check at all, as long as the report says how old it is. One that
        // does not parse, written by an older fieldtrip perhaps, is as good as none.
        let kept = caching.then(|| cache::read_schema(&endpoint.url)).flatten();
        let kept = kept.and_then(|kept| Some((Loaded::from_sdl(&kept.sdl, "it").ok()?, kept.age)));
        let Some((schema, age)) = kept else {
            out.line(
                "schema",
                "introspection, not checked: check sends nothing to the endpoint",
            );
            return None;
        };
        let age = show_duration(age);
        out.line(
            "schema",
            format!("introspection, cached {age} ago: check sends nothing to the endpoint"),
        );
        return Some(Against {
            schema,
            note: format!(": checked against schema cached {age} ago"),
        });
    };
    let what = "introspection, fetched for --introspect";
    let fetched = match introspection::fetch(http, endpoint).await {
        Ok(sdl) => Loaded::from_sdl(&sdl, "the introspection result").map(|schema| (schema, sdl)),
        Err(problem) => Err(problem),
    };
    match fetched {
        Ok((schema, sdl)) => {
            out.line("schema", format!("{what}: ok"));
            if caching && let Err(error) = cache::write_schema(&endpoint.url, &endpoint.shown_url, &sdl) {
                tracing::warn!(
                    "the schema of {} could not be kept in the cache: {error}",
                    endpoint.shown_url
                );
            }
            Some(Against {
                schema,
                note: ": checked against schema introspected now".to_owned(),
            })
        }
        Err(problem) => {
            out.problem("schema", what, &problem);
            None
        }
    }
}

/// Writes what one endpoint resolves to, checking its schema, cost map and headers.
async fn endpoint_report(out: &mut Out, name: &str, endpoint: &Endpoint, http: Option<&http::Client>) {
    let _ = writeln!(out.text, "\n{name}");
    if let Some(description) = &endpoint.description {
        out.line("description", description);
    }
    if let Some(template) = &endpoint.template {
        out.line("template", template);
    }
    out.line("url", &endpoint.shown_url);
    out.line("approval", approval::policy(endpoint));
    if endpoint.mutations == Mutations::Deny {
        out.line("mutations", "denied");
    }
    if !endpoint.audit_variables {
        out.line("audit variables", "not recorded");
    }
    out.line("timeout", format!("{}s", endpoint.timeout.as_secs()));
    out.line("assumed list size", endpoint.assumed_list_size);
    if !endpoint.response_headers.is_empty() {
        out.line("response headers shown", endpoint.response_headers.join(", "));
    }
    out.line("max response bytes", show_bytes(endpoint.max_response_bytes));
    out.line("max output chars", endpoint.max_output_chars);
    out.line("max inline chars", endpoint.max_inline_chars);

    let schema = match &endpoint.schema {
        None => {
            out.line("max schema bytes", show_bytes(endpoint.max_schema_bytes));
            if endpoint.schema_ttl.is_zero() {
                out.line("schema ttl", "0, not cached");
            } else {
                out.line("schema ttl", show_duration(endpoint.schema_ttl));
            }
            introspected_schema(out, endpoint, http).await
        }
        Some(path) => {
            let what = format!("file {}", path.display());
            match read_schema(path) {
                Ok(schema) => {
                    out.line("schema", format!("{what}: ok"));
                    Some(Against {
                        schema,
                        note: String::new(),
                    })
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
            Some(against) => {
                let what = format!("{what}{}", against.note);
                let findings = against.schema.cost_map_findings(&endpoint.costs);
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

    if let Some(source) = &endpoint.glossary.source {
        let terms = endpoint.glossary.len();
        let what = format!(
            "{} ({terms} term{})",
            source.display(),
            if terms == 1 { "" } else { "s" }
        );
        match &schema {
            None => out.line("glossary", format!("{what}: not checked against a schema")),
            Some(against) => {
                let what = format!("{what}{}", against.note);
                // A glossary shared by several instances of a service can name a field only some of them have,
                // so this is a warning, as an unmatched cost rule is.
                match against.schema.glossary_finding(&endpoint.glossary) {
                    None => out.line("glossary", format!("{what}: ok")),
                    Some(finding) => out.warning("glossary", &what, &finding),
                }
            }
        }
    }

    operations_report(out, name, endpoint, schema.as_ref());

    for header in &endpoint.headers {
        let label = format!("header {}", header.name);
        let what = header.source();
        match header.resolve(false, &[]).await {
            Ok(_resolved) => out.line(&label, format!("{what}: ok")),
            Err(problem) => out.problem(&label, &what, &problem),
        }
    }
}

/// Writes whether each of the endpoint's saved operations still validates against its schema, when both are there.
fn operations_report(out: &mut Out, name: &str, endpoint: &Endpoint, schema: Option<&Against>) {
    if let Some(source) = &endpoint.operations.source {
        let count = endpoint.operations.operations.len();
        let what = format!(
            "{} ({count} operation{})",
            source.display(),
            if count == 1 { "" } else { "s" }
        );
        // A file left out is a warning too: the rest of the library loads without it.
        for left_out in &endpoint.operations.rejected {
            out.warning("operations", &what, &left_out.problem);
        }
        match schema {
            None => out.line("operations", format!("{what}: not checked against a schema")),
            Some(against) => {
                let loaded = &against.schema;
                let what = format!("{what}{}", against.note);
                let mut broken = endpoint.operations.rejected.len();
                // An operation broken by a change to the schema is a warning, as an unmatched cost rule is: the
                // endpoint still works, and only that operation is refused.
                for saved in &endpoint.operations.operations {
                    if let Err(error) = saved.plan(loaded, name, endpoint) {
                        let problems = error.problems().join("; ");
                        let warning = format!(
                            "`{}` in {}{} no longer validates: {problems}",
                            saved.name,
                            saved.file.display(),
                            saved.with_fragments()
                        );
                        out.warning("operations", &what, &warning);
                        broken += 1;
                    }
                }
                if broken == 0 {
                    out.line("operations", format!("{what}: ok"));
                }
            }
        }
    }
    if let Some(shelf) = &endpoint.operations.fragments {
        let count = shelf.fragments.len();
        let what = format!(
            "{} ({count} fragment{})",
            shelf.source.display(),
            if count == 1 { "" } else { "s" }
        );
        for left_out in &shelf.rejected {
            out.warning("fragments", &what, &left_out.problem);
        }
        let mut unused = shelf.rejected.len();
        // A fragment no operation spreads does no harm, but it may be one an operation misspells.
        for shared in shelf.fragments.iter().filter(|shared| !shared.used) {
            let warning = format!("`{}` in {} is used by no operation", shared.name, shared.file.display());
            out.warning("fragments", &what, &warning);
            unused += 1;
        }
        if unused == 0 {
            out.line("fragments", format!("{what}: ok"));
        }
    }
    let directives = &endpoint.operations.client_directives;
    if !directives.is_empty() {
        let what: Vec<String> = directives.iter().map(|name| format!("@{name}")).collect();
        let what = what.join(", ");
        let unused = endpoint.operations.unused_client_directives();
        // A listed directive nothing uses does no harm, but it may be a misspelling of one that is used.
        for name in &unused {
            let warning = format!("`@{name}` is used by no saved operation or shared fragment");
            out.warning("client directives", &what, &warning);
        }
        if unused.is_empty() {
            out.line("client directives", format!("{what}: ok"));
        }
    }
}

/// Reads and validates a schema file.
fn read_schema(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("could not read it: {error}"))?;
    Loaded::from_sdl(&text, "it")
}
