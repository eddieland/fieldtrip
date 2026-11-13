//! Slice 9: a command's output is reused for its `ttl`, and a 401 makes the command run again.
//!
//! `token.sh count` prints `t0ken-1`, then `t0ken-2`, and so on, so the token on a request says how many times
//! the command has run.

use serde_json::json;

use crate::support::{FixtureApi, Mcp, MockServer, Response};

/// A config whose `live` endpoint sends a counted token, with `extra` added to the header's table.
fn config(mock: &MockServer, extra: &str) -> String {
    format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\n\
         Authorization = {{ command = [\"sh\", \"token.sh\", \"count\"], format = \"Bearer {{}}\"{extra} }}\n",
        mock.url()
    )
}

/// The `Authorization` header of every request the mock received.
fn tokens(mock: &MockServer) -> Vec<String> {
    mock.requests()
        .iter()
        .map(|request| request.header("authorization").unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn the_command_runs_once_within_its_ttl() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, ""));

    mcp.call("introspect", &json!({ "endpoint": "live" }));
    mcp.call("introspect", &json!({ "endpoint": "live", "refresh": true }));

    assert_eq!(
        tokens(&mock),
        ["Bearer t0ken-1", "Bearer t0ken-1"],
        "both requests reuse the first output"
    );
}

#[test]
fn a_ttl_of_zero_runs_the_command_for_every_request() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, ", ttl = 0"));

    mcp.call("introspect", &json!({ "endpoint": "live" }));
    mcp.call("introspect", &json!({ "endpoint": "live", "refresh": true }));

    assert_eq!(tokens(&mock), ["Bearer t0ken-1", "Bearer t0ken-2"], "nothing is reused");
}

#[test]
fn a_401_runs_the_command_again_and_retries_once() {
    let api = FixtureApi::new();
    let mock = MockServer::start(move |request| {
        if request.header("authorization") == Some("Bearer t0ken-1") {
            Response::new(401, "{\"errors\":[{\"message\":\"token expired\"}]}")
        } else {
            api.respond(request)
        }
    });
    let mut mcp = Mcp::start(&config(&mock, ""));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(!reply.is_error, "the retry should have succeeded: {}", reply.text);
    assert_eq!(
        tokens(&mock),
        ["Bearer t0ken-1", "Bearer t0ken-2"],
        "the second request carries a fresh token"
    );
}

#[test]
fn a_second_401_is_an_error_and_is_not_retried_again() {
    let mock = MockServer::start(|_| Response::new(401, "{\"errors\":[{\"message\":\"no\"}]}"));
    let mut mcp = Mcp::start(&config(&mock, ""));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(
        reply.is_error,
        "a refused credential should be an error: {}",
        reply.text
    );
    assert!(reply.text.contains("HTTP 401"), "names the status: {}", reply.text);
    assert!(!reply.text.contains("t0ken"), "the token is not shown: {}", reply.text);
    assert_eq!(
        tokens(&mock),
        ["Bearer t0ken-1", "Bearer t0ken-2"],
        "one retry with a fresh token, then it stops"
    );
}

#[test]
fn a_401_is_not_retried_when_no_header_comes_from_a_command() {
    let mock = MockServer::start(|_| Response::new(401, "{\"errors\":[{\"message\":\"no\"}]}"));
    let config = format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\nX-Api-Key = \"fixed\"\n",
        mock.url()
    );
    let mut mcp = Mcp::start(&config);

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(
        reply.is_error,
        "a refused credential should be an error: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 1, "sending the same header again cannot help");
}
