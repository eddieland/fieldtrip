//! Slice 26: `fieldtrip check` loads the config, resolves every header, checks schema files and cost maps, and
//! prints what each endpoint resolves to, without a value and without sending anything.

use crate::support::{Launch, MockServer};

const CONFIG: &str = r#"
[templates.store]
costs = "store.costs.toml"

[templates.store.headers]
Authorization = { command = ["sh", "token.sh", "echo", "cmd-s3cret-value"], format = "Bearer {}" }

[endpoints.store]
template    = "store"
description = "The store."
url         = "URL?key=q-s3cret-value"
schema      = "schema.graphql"

[endpoints.store.headers]
X-Api-Key = { env = "FIELDTRIP_SPEC_KEY" }
X-Client  = "literal-s3cret-value"
"#;

/// The config with the mock's URL in it.
fn config(mock: &MockServer) -> String {
    CONFIG.replace("URL", &mock.url())
}

const COSTS: &str = "\"Query.products\" = { multiplier = 50 }\n\"Product.nope\" = { cost = 1 }\n";

#[test]
fn a_good_config_passes_and_shows_what_each_endpoint_resolves_to() {
    let mock = MockServer::unreachable();
    let checked = Launch::new(&config(&mock))
        .file("store.costs.toml", COSTS)
        .env("FIELDTRIP_SPEC_KEY", "env-s3cret-value")
        .check();

    assert!(
        checked.passed,
        "a warning does not fail the check: {}{}",
        checked.stdout, checked.stderr
    );
    let dir = checked.dir.path();
    let expected = format!(
        "Config: {config}\n\
         \n\
         store\n  \
           description: The store.\n  \
           template: store\n  \
           url: {url}?key=<redacted>\n  \
           approval: the user is asked before every operation\n  \
           timeout: 30s\n  \
           assumed list size: 10\n  \
           max response bytes: 8 MiB\n  \
           max output chars: 40000\n  \
           schema: file {schema}: ok\n  \
           cost map: {costs} (2 rules): warning: These rules match nothing in this schema: `Product.nope`.\n  \
           header Authorization: command `sh`, formatted, reused for 300s: ok\n  \
           header X-Api-Key: env `FIELDTRIP_SPEC_KEY`: ok\n  \
           header X-Client: literal: ok\n\
         \n\
         0 problems, 1 warning.\n",
        config = dir.join("fieldtrip.toml").display(),
        url = mock.url(),
        schema = dir.join("schema.graphql").display(),
        costs = dir.join("store.costs.toml").display(),
    );
    assert_eq!(checked.stdout, expected);
    assert_eq!(mock.requests().len(), 0, "check sends nothing to the endpoint");
    for output in [&checked.stdout, &checked.stderr] {
        assert!(!output.contains("s3cret"), "a value leaked: {output}");
    }
}

#[test]
fn a_header_that_cannot_be_produced_fails_the_check() {
    let mock = MockServer::unreachable();
    let checked = Launch::new(&config(&mock)).file("store.costs.toml", COSTS).check();

    assert!(!checked.passed, "a missing variable is a problem: {}", checked.stdout);
    assert!(
        checked.stdout.contains(
            "  header X-Api-Key: env `FIELDTRIP_SPEC_KEY`: PROBLEM: The header `X-Api-Key` is read from the \
             environment variable `FIELDTRIP_SPEC_KEY`, which is not set."
        ),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.ends_with("1 problem, 1 warning.\n"),
        "{}",
        checked.stdout
    );
    assert!(!checked.stdout.contains("s3cret"), "a value leaked: {}", checked.stdout);
}

#[test]
fn a_schema_file_that_is_not_valid_fails_the_check() {
    let checked = Launch::new("[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"bad.graphql\"\n")
        .file("bad.graphql", "type Query { nope: Missing }\n")
        .check();

    assert!(!checked.passed, "{}", checked.stdout);
    assert!(checked.stdout.contains("bad.graphql: PROBLEM:"), "{}", checked.stdout);
}

#[test]
fn an_introspected_endpoint_is_not_contacted() {
    let mock = MockServer::unreachable();
    let checked = Launch::new(&format!("[endpoints.live]\nurl = \"{}\"\n", mock.url())).check();

    assert!(checked.passed, "{}", checked.stdout);
    assert!(
        checked
            .stdout
            .contains("  schema: introspection, not checked: check sends nothing to the endpoint\n"),
        "{}",
        checked.stdout
    );
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn a_config_that_does_not_load_fails_with_the_reason_on_stderr() {
    let checked = Launch::new("[endpoints.store]\nurl = \"https://\"\n").check();

    assert!(!checked.passed);
    assert!(checked.stdout.is_empty(), "no report: {}", checked.stdout);
    assert!(
        checked.stderr.contains("`url` is not a usable URL (empty host)"),
        "{}",
        checked.stderr
    );
}

#[test]
fn check_works_with_url_flags() {
    let checked = Launch::flags(&["--url", "http://localhost:9/graphql", "--schema", "schema.graphql"]).check();

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    assert!(
        checked.stdout.starts_with("Config: from --url\n\ndefault\n"),
        "{}",
        checked.stdout
    );
}

#[test]
fn a_relative_config_path_is_relative_to_the_working_directory() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n\n\
                  [endpoints.store.headers]\nX-Token = { command = [\"sh\", \"token.sh\", \"ok\"] }\n";
    let checked = Launch::flags(&["--config", "fieldtrip.toml"])
        .file("fieldtrip.toml", config)
        .check();

    assert!(
        checked.passed,
        "the command runs in the config file's directory: {}",
        checked.stdout
    );
    let dir = checked.dir.path();
    assert!(
        checked
            .stdout
            .contains(&format!("schema: file {}: ok", dir.join("schema.graphql").display())),
        "paths are shown absolute: {}",
        checked.stdout
    );
}
