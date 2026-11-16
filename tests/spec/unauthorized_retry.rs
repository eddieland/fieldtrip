//! Slice 37: when a 401 makes the header commands run again and the retry is refused too, the result says that both
//! happened and names the program, so the user knows to sign in again.

use serde_json::json;

use crate::support::{FixtureApi, Human, Launch, Mcp, MockServer, Response};

/// A config whose `live` endpoint takes `Authorization` from `token.sh` in `mode`. The user's MCP client approves.
fn config(mock: &MockServer, mode: &str) -> String {
    format!(
        "[endpoints.live]\nurl = \"{}\"\napproval = \"client\"\n\n[endpoints.live.headers]\n\
         Authorization = {{ command = [\"sh\", \"token.sh\", \"{mode}\"], format = \"Bearer {{}}\" }}\n",
        mock.url()
    )
}

/// A mock that refuses every request with a 401.
fn refusing() -> MockServer {
    MockServer::start(|_| Response::new(401, "{\"errors\":[{\"message\":\"token expired\"}]}"))
}

const QUERY: &str = "query Viewer { viewer { id } }";

#[test]
fn introspect_says_the_fresh_credential_was_refused_too_and_names_the_program() {
    let mock = refusing();
    let mut mcp = Mcp::start(&config(&mock, "count"));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a refused credential is an error: {}", reply.text);
    assert!(
        reply.text.contains("HTTP 401 twice"),
        "says both requests were refused: {}",
        reply.text
    );
    assert!(
        reply.text.contains("`Authorization` (`sh`)"),
        "names the header and the program that ran again: {}",
        reply.text
    );
    assert!(
        reply.text.contains("sign in again"),
        "says what the user has to do: {}",
        reply.text
    );
    assert!(
        reply.text.contains("403"),
        "tells it apart from a missing permission: {}",
        reply.text
    );
    assert!(!reply.text.contains("t0ken"), "no token is shown: {}", reply.text);
    assert_eq!(mock.requests().len(), 2, "one retry, then it stops");
}

#[test]
fn execute_reports_the_retry_with_the_response() {
    let mock = refusing();
    let mut mcp = Mcp::start(&with_schema(&mock, "count"));

    let reply = mcp.call("execute", &json!({ "endpoint": "live", "operation": QUERY }));

    assert!(
        reply.text.contains("HTTP 401 twice"),
        "says both requests were refused: {}",
        reply.text
    );
    assert!(
        reply.text.contains("`Authorization` (`sh`)"),
        "names the program: {}",
        reply.text
    );
    assert!(
        reply.text.contains("token expired"),
        "still shows what the server said: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 2, "one retry, then it stops");
}

/// Like [`config`], with the schema read from the fixture file so that nothing is introspected.
fn with_schema(mock: &MockServer, mode: &str) -> String {
    config(mock, mode).replace(
        "approval = \"client\"\n",
        "approval = \"client\"\nschema = \"schema.graphql\"\n",
    )
}

#[test]
fn a_retry_that_works_needs_no_note() {
    let api = FixtureApi::new();
    let mock = MockServer::start(move |request| {
        if request.header("authorization") == Some("Bearer t0ken-1") {
            Response::new(401, "{\"errors\":[{\"message\":\"token expired\"}]}")
        } else {
            api.respond(request)
        }
    });
    let mut mcp = Mcp::start(&with_schema(&mock, "count"));

    let reply = mcp.call("execute", &json!({ "endpoint": "live", "operation": QUERY }));

    assert!(!reply.is_error, "the retry worked: {}", reply.text);
    assert!(!reply.text.contains("401"), "nothing to report: {}", reply.text);
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn a_command_that_fails_when_run_again_says_the_401_came_first() {
    let mock = refusing();
    let mut mcp = Mcp::start(&with_schema(&mock, "once"));

    let reply = mcp.call("execute", &json!({ "endpoint": "live", "operation": QUERY }));

    assert!(reply.is_error, "no credential, no result: {}", reply.text);
    assert!(reply.text.contains("HTTP 401"), "says what started it: {}", reply.text);
    assert!(
        reply.text.contains("session expired: run acme login"),
        "passes on the command's stderr: {}",
        reply.text
    );
    assert!(reply.text.contains("`sh`"), "names the program: {}", reply.text);
    assert_eq!(mock.requests().len(), 1, "the retry is not sent without a credential");
}

#[test]
fn list_endpoints_shows_the_refused_credential() {
    let mock = refusing();
    let mut mcp = Mcp::start(&config(&mock, "count"));

    mcp.call("introspect", &json!({ "endpoint": "live" }));
    let reply = mcp.call("list_endpoints", &json!({}));

    let line = reply
        .text
        .lines()
        .find(|line| line.trim_start().starts_with("header Authorization:"))
        .unwrap_or_else(|| panic!("a status line: {}", reply.text));
    assert!(
        line.contains("refused with HTTP 401") && line.contains("sign in again"),
        "the command ran, but what it printed was refused: {line}"
    );
}

#[test]
fn a_401_with_no_command_to_run_again_is_reported_as_before() {
    let mock = refusing();
    let config = format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\nX-Api-Key = \"fixed\"\n",
        mock.url()
    );
    let mut mcp = Launch::new(&config).human(Human::Absent).start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "{}", reply.text);
    assert!(!reply.text.contains("twice"), "there was no retry: {}", reply.text);
    assert_eq!(mock.requests().len(), 1);
}
