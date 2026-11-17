//! Slice 50: `fieldtrip check` checks the cost map and saved operations of an introspected endpoint against the
//! copy of its schema in fieldtrip's cache, saying how old the copy is, and still sends nothing. `--introspect`
//! lets it fetch the schema instead.

use std::time::{Duration, SystemTime};

use serde_json::json;
use tempfile::TempDir;

use crate::support::{Checked, FixtureApi, Launch, MockServer, Response};

const COSTS: &str = "\"Query.products\" = { multiplier = 50 }\n";
const TYPO: &str = "\"Query.products\" = { multiplier = 50 }\n\"Product.nope\" = { cost = 1 }\n";
const VIEWER: &str = "# Who is signed in.\nquery Viewer { viewer { name } }\n";
const BROKEN: &str = "query Gone { viewer { shoeSize } }\n";

/// A mock that serves the fixture schema.
fn api() -> MockServer {
    let api = FixtureApi::new();
    MockServer::start(move |request| api.respond(request))
}

/// A config with one introspected endpoint, `live`, at `url`, with a cost map, saved operations and `extra` lines.
fn config(url: &str, extra: &str) -> String {
    format!("[endpoints.live]\nurl = \"{url}\"\ncosts = \"costs.toml\"\noperations = \"operations/\"\n{extra}")
}

/// A launch of `config` with `costs` as the cost map, `Viewer` as a saved operation, and `cache` as the cache.
fn launch(config: &str, costs: &str, cache: &TempDir) -> Launch {
    Launch::new(config)
        .file("costs.toml", costs)
        .file("operations/viewer.graphql", VIEWER)
        .env("XDG_CACHE_HOME", &cache.path().display().to_string())
}

/// Starts a server that introspects `live` once, so the cache holds its schema, then makes that copy `age` old.
fn warm(config: &str, cache: &TempDir, age: Duration) {
    let mut mcp = launch(config, COSTS, cache).start();
    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));
    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    drop(mcp);
    let schemas = cache.path().join("fieldtrip").join("schemas");
    let files: Vec<_> = std::fs::read_dir(schemas)
        .expect("the schema directory")
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    let [file] = files.as_slice() else {
        panic!("one kept schema, got {files:?}");
    };
    std::fs::File::options()
        .write(true)
        .open(file)
        .and_then(|file| file.set_modified(SystemTime::now() - age))
        .expect("set the modification time");
}

/// Asserts that the report has `line`, indented as the report's lines are.
fn assert_line(checked: &Checked, line: &str) {
    assert!(
        checked.stdout.contains(&format!("  {line}\n")),
        "expected the line `{line}` in:\n{}{}",
        checked.stdout,
        checked.stderr
    );
}

#[test]
fn a_cached_schema_is_checked_against_and_its_age_is_given() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache, Duration::from_mins(97));
    let sent = mock.requests().len();

    let checked = launch(&config, COSTS, &cache).check();

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    let dir = checked.dir.path();
    assert_line(
        &checked,
        "schema: introspection, cached 97 min ago: check sends nothing to the endpoint",
    );
    assert_line(
        &checked,
        &format!(
            "cost map: {} (1 rule): checked against schema cached 97 min ago: ok",
            dir.join("costs.toml").display()
        ),
    );
    assert_line(
        &checked,
        &format!(
            "operations: {} (1 operation): checked against schema cached 97 min ago: ok",
            dir.join("operations/").display()
        ),
    );
    assert!(
        checked.stdout.ends_with("0 problems, 0 warnings.\n"),
        "{}",
        checked.stdout
    );
    assert_eq!(mock.requests().len(), sent, "check sends nothing to the endpoint");
}

#[test]
fn a_rule_or_an_operation_that_does_not_fit_the_cached_schema_is_a_warning() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache, Duration::from_mins(5));

    let checked = launch(&config, TYPO, &cache)
        .file("operations/gone.graphql", BROKEN)
        .check();

    assert!(checked.passed, "a warning does not fail the check: {}", checked.stdout);
    assert!(
        checked.stdout.contains(
            "(2 rules): checked against schema cached 5 min ago: warning: These rules match nothing in this schema: \
             `Product.nope`.\n"
        ),
        "{}",
        checked.stdout
    );
    assert!(
        checked
            .stdout
            .contains("(2 operations): checked against schema cached 5 min ago: warning: `Gone` in ")
            && checked.stdout.contains("no longer validates"),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.ends_with("0 problems, 2 warnings.\n"),
        "{}",
        checked.stdout
    );
}

#[test]
fn a_copy_older_than_the_ttl_is_still_checked_against() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache, Duration::from_hours(72));
    let sent = mock.requests().len();

    let checked = launch(&config, TYPO, &cache).check();

    assert!(
        checked
            .stdout
            .contains("checked against schema cached 3 days ago: warning:"),
        "the age tells the user how far to trust it: {}",
        checked.stdout
    );
    assert_eq!(mock.requests().len(), sent, "an old copy is no reason to send anything");
}

#[test]
fn with_no_cached_copy_nothing_is_checked() {
    let mock = MockServer::unreachable();
    let cache = tempfile::tempdir().expect("a cache directory");

    let checked = launch(&config(&mock.url(), ""), TYPO, &cache).check();

    assert!(checked.passed, "{}", checked.stdout);
    assert_line(
        &checked,
        "schema: introspection, not checked: check sends nothing to the endpoint",
    );
    assert!(
        checked.stdout.contains("(2 rules): not checked against a schema\n")
            && checked.stdout.contains("(1 operation): not checked against a schema\n"),
        "{}",
        checked.stdout
    );
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn an_endpoint_that_keeps_no_schema_reads_none() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    warm(&config(&mock.url(), ""), &cache, Duration::from_mins(5));

    let checked = launch(&config(&mock.url(), "schema_ttl = 0\n"), TYPO, &cache).check();

    assert_line(&checked, "schema ttl: 0, not cached");
    assert!(
        checked.stdout.contains("(2 rules): not checked against a schema\n"),
        "{}",
        checked.stdout
    );
}

#[test]
fn introspect_fetches_the_schema_and_keeps_it() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");

    let checked = launch(&config, TYPO, &cache).check_with(&["--introspect"]);

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    assert_eq!(mock.requests().len(), 1, "the user asked for a request");
    assert_line(&checked, "schema: introspection, fetched for --introspect: ok");
    assert!(
        checked
            .stdout
            .contains("(2 rules): checked against schema introspected now: warning: These rules match nothing"),
        "{}",
        checked.stdout
    );
    assert!(
        checked
            .stdout
            .contains("(1 operation): checked against schema introspected now: ok\n"),
        "{}",
        checked.stdout
    );

    let again = launch(&config, TYPO, &cache).check();
    assert!(
        again.stdout.contains("checked against schema cached 0 s ago"),
        "the fetched schema was kept: {}",
        again.stdout
    );
    assert_eq!(mock.requests().len(), 1, "and a plain check sends nothing");
}

#[test]
fn introspect_ignores_a_cached_copy() {
    let mock = api();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache, Duration::from_mins(5));

    let checked = launch(&config, COSTS, &cache).check_with(&["--introspect"]);

    assert_eq!(mock.requests().len(), 2, "a fresh copy was asked for");
    assert!(checked.stdout.contains("introspected now: ok"), "{}", checked.stdout);
}

#[test]
fn introspect_that_fails_is_a_problem() {
    let mock = MockServer::start(|_request| Response::new(503, "Service Unavailable"));
    let cache = tempfile::tempdir().expect("a cache directory");

    let checked = launch(&config(&mock.url(), ""), COSTS, &cache).check_with(&["--introspect"]);

    assert!(!checked.passed, "the user asked for the schema: {}", checked.stdout);
    assert!(
        checked
            .stdout
            .contains("  schema: introspection, fetched for --introspect: PROBLEM: "),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.contains("(1 rule): not checked against a schema\n"),
        "{}",
        checked.stdout
    );
}

#[test]
fn introspect_sends_nothing_to_an_endpoint_with_a_schema_file() {
    let mock = MockServer::unreachable();
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );

    let checked = Launch::new(&config).check_with(&["--introspect"]);

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    assert!(checked.stdout.contains("schema.graphql: ok\n"), "{}", checked.stdout);
    assert_eq!(mock.requests().len(), 0, "a schema file means no introspection, ever");
}
