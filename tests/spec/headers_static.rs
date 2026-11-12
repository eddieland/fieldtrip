//! Slice 7: the literal and `env` plugs put their headers on every request to the endpoint.

use serde_json::json;

use crate::support::{FixtureApi, Launch, MockServer};

/// A config whose `live` endpoint is introspected through `mock` and carries `headers`.
fn config(mock: &MockServer, headers: &str) -> String {
    format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\n{headers}",
        mock.url()
    )
}

const HEADERS: &str = r#"
X-Client      = "fieldtrip-spec"
X-Api-Key     = { env = "FIELDTRIP_SPEC_KEY" }
Authorization = { env = "FIELDTRIP_SPEC_TOKEN", format = "Bearer {}" }
"#;

#[test]
fn the_request_carries_literal_and_env_headers() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&config(&mock, HEADERS))
        .env("FIELDTRIP_SPEC_KEY", "k3y-from-env")
        .env("FIELDTRIP_SPEC_TOKEN", "t0ken-from-env")
        .start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    let requests = mock.requests();
    let request = requests.first().expect("the introspection request");
    assert_eq!(
        request.header("x-client"),
        Some("fieldtrip-spec"),
        "a literal is sent as written"
    );
    assert_eq!(
        request.header("x-api-key"),
        Some("k3y-from-env"),
        "an env plug sends the variable's value"
    );
    assert_eq!(
        request.header("authorization"),
        Some("Bearer t0ken-from-env"),
        "`format` wraps the value"
    );
}

#[test]
fn a_missing_variable_is_an_error_that_names_the_header_and_sends_nothing() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&config(&mock, HEADERS))
        .env("FIELDTRIP_SPEC_KEY", "k3y-from-env")
        .start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a missing variable should be an error: {}", reply.text);
    assert!(
        reply.text.contains("`Authorization`"),
        "names the header: {}",
        reply.text
    );
    assert!(
        reply.text.contains("`FIELDTRIP_SPEC_TOKEN`"),
        "names the variable: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "no request goes out with a header missing");
}

#[test]
fn the_variable_is_read_when_the_request_is_made() {
    let mock = MockServer::serving(FixtureApi::new());
    // The server starts without the token. Starting must not fail: the variable is only needed at request time.
    let mut mcp = Launch::new(&config(&mock, HEADERS))
        .env("FIELDTRIP_SPEC_KEY", "k3y-from-env")
        .start();

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        !reply.is_error,
        "tools that send nothing work without the variable: {}",
        reply.text
    );
}

#[test]
fn a_format_without_a_placeholder_is_refused_at_startup() {
    let mock = MockServer::unreachable();
    let headers = "Authorization = { env = \"FIELDTRIP_SPEC_TOKEN\", format = \"Bearer sekrit-in-format\" }\n";

    let stderr = Launch::new(&config(&mock, headers)).fails_to_start();

    assert!(stderr.contains("`Authorization`"), "names the header: {stderr}");
    assert!(stderr.contains("{}"), "says the placeholder is missing: {stderr}");
    assert!(
        !stderr.contains("sekrit-in-format"),
        "does not print the format, which may hold a secret: {stderr}"
    );
}

#[test]
fn a_header_that_is_not_one_of_the_plugs_is_refused_at_startup() {
    let mock = MockServer::unreachable();

    let both = Launch::new(&config(&mock, "X-Key = { env = \"A\", command = [\"b\"] }\n")).fails_to_start();
    assert!(both.contains("`X-Key`"), "names the header: {both}");

    let neither = Launch::new(&config(&mock, "X-Key = { file = \"token.txt\" }\n")).fails_to_start();
    assert!(neither.contains("`X-Key`"), "names the header: {neither}");
    assert!(
        neither.contains("env") && neither.contains("command"),
        "says what the plugs are: {neither}"
    );

    let number = Launch::new(&config(&mock, "X-Key = 42\n")).fails_to_start();
    assert!(number.contains("`X-Key`"), "names the header: {number}");
}
