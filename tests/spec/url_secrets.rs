//! Slice 23: a key in the URL's query never reaches the calling agent, and credentials in the URL are refused.

use serde_json::json;

use crate::support::{FixtureApi, Human, Launch, MockServer, Response};

const KEY: &str = "q-k3y-in-the-url";

/// A config for an endpoint at `mock` that takes `KEY` in its query string.
fn config(mock: &MockServer, extra: &str) -> String {
    format!("[endpoints.store]\nurl = \"{}?api_key={KEY}&v=2\"\n{extra}", mock.url())
}

#[test]
fn the_key_is_sent_but_never_shown() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&config(&mock, "")).human(Human::Accepts).start();

    let mut seen = Vec::new();
    for (tool, arguments) in [
        ("list_endpoints", json!({})),
        ("introspect", json!({ "endpoint": "store" })),
        (
            "execute",
            json!({ "endpoint": "store", "operation": "{ viewer { name } }" }),
        ),
    ] {
        let reply = mcp.call(tool, &arguments);
        assert!(!reply.is_error, "{tool} should succeed: {}", reply.text);
        seen.push(reply.text);
    }
    seen.extend(mcp.prompts().iter().cloned());
    seen.push(mcp.finish());

    for text in &seen {
        assert!(!text.contains(KEY), "the key leaked: {text}");
    }
    assert!(
        seen.first()
            .is_some_and(|listed| listed.contains("?api_key=<redacted>&v=<redacted>")),
        "the parameter names are shown, so the user can tell the key is there: {seen:?}"
    );
    let requests = mock.requests();
    assert!(
        requests.iter().all(|request| request.target.contains(KEY)),
        "the request goes to the URL as configured: {:?}",
        requests.iter().map(|request| &request.target).collect::<Vec<_>>()
    );
}

#[test]
fn errors_about_the_endpoint_do_not_show_the_key() {
    let mock = MockServer::start(|_request| Response::new(500, "not graphql"));
    let mut mcp = Launch::new(&config(&mock, "")).start();
    // Nothing listens on port 1, so the request fails before any response, in the HTTP client.
    let mut down = Launch::new(&format!(
        "[endpoints.store]\nurl = \"http://127.0.0.1:1/graphql?api_key={KEY}\"\n"
    ))
    .start();

    for reply in [
        mcp.call("introspect", &json!({ "endpoint": "store" })),
        down.call("introspect", &json!({ "endpoint": "store" })),
    ] {
        assert!(reply.is_error, "should fail: {}", reply.text);
        assert!(!reply.text.contains(KEY), "the key leaked: {}", reply.text);
    }
}

#[test]
fn a_user_name_or_password_in_the_url_is_refused() {
    let stderr =
        Launch::new("[endpoints.store]\nurl = \"https://me:hunter2-password@api.example/graphql\"\n").fails_to_start();

    assert!(stderr.contains("user name or password"), "says what is wrong: {stderr}");
    assert!(stderr.contains("header"), "says where credentials go: {stderr}");
    assert!(!stderr.contains("hunter2-password"), "the password leaked: {stderr}");
}
