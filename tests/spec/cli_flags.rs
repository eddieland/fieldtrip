//! Slice 15: the server can start from `--url` and `--schema`, with no config file.

use serde_json::json;

use crate::support::{FixtureApi, Launch, MockServer};

#[test]
fn url_alone_serves_one_introspected_endpoint() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::flags(&["--url", &mock.url()]).start();

    let endpoints = mcp.call("list_endpoints", &json!({}));
    assert_eq!(
        endpoints.text,
        format!(
            "default\n  url: {}\n  schema: introspection, cached for 1 day\n  approval: the user is asked before every operation\n",
            mock.url()
        ),
        "the endpoint is called `default`, has no headers, and keeps the default policy"
    );

    let described = mcp.call("describe", &json!({ "endpoint": "default", "name": "Money" }));
    assert_eq!(
        described.text, "type Money {\n  amountCents: Int!\n  currency: Currency!\n}\n",
        "the schema is loaded by introspection"
    );
    assert_eq!(mock.requests().len(), 1, "one introspection request");
}

#[test]
fn schema_reads_a_file_and_asks_the_endpoint_for_nothing() {
    let mock = MockServer::unreachable();
    // The fixtures are copied into the directory the server starts in, so the relative path resolves there.
    let mut mcp = Launch::flags(&["--url", &mock.url(), "--schema", "schema.graphql"]).start();

    let endpoints = mcp.call("list_endpoints", &json!({}));
    let schema_path = mcp.dir().join("schema.graphql");
    assert!(
        endpoints
            .text
            .contains(&format!("  schema: file {}\n", schema_path.display())),
        "a relative --schema is resolved against the starting directory: {}",
        endpoints.text
    );

    let described = mcp.call("describe", &json!({ "endpoint": "default", "name": "Money" }));
    assert!(!described.is_error, "the schema file is used: {}", described.text);
    assert_eq!(mock.requests().len(), 0, "nothing is sent to the endpoint");
}

#[test]
fn url_and_config_together_are_refused() {
    let stderr = Launch::flags(&["--url", "http://localhost:9/graphql", "--config", "fieldtrip.toml"]).fails_to_start();

    assert!(
        stderr.contains("--url") && stderr.contains("--config"),
        "names both flags: {stderr}"
    );
}

#[test]
fn schema_without_url_is_refused() {
    let stderr = Launch::flags(&["--schema", "schema.graphql"]).fails_to_start();

    assert!(stderr.contains("--url"), "says --url is needed: {stderr}");
}

#[test]
fn a_url_that_is_not_http_is_refused() {
    let stderr = Launch::flags(&["--url", "ftp://example.com/graphql"]).fails_to_start();

    assert!(
        stderr.contains("http://") && stderr.contains("https://"),
        "says what a URL must be: {stderr}"
    );
}

#[test]
fn no_flags_reads_the_config_file_at_the_default_location() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\n";
    let mut mcp = Launch::new(config).at_the_default_location().start();

    let endpoints = mcp.call("list_endpoints", &json!({}));

    assert!(
        endpoints.text.starts_with("store\n"),
        "the endpoint from the default config file: {}",
        endpoints.text
    );
}

#[test]
fn no_flags_and_no_config_file_says_what_to_do() {
    let stderr = Launch::flags(&[]).fails_to_start();

    assert!(stderr.contains("config.toml"), "names the file it looked for: {stderr}");
    assert!(
        stderr.contains("--config") && stderr.contains("--url"),
        "names the two ways out: {stderr}"
    );
}
