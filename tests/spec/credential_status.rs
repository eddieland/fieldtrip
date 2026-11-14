//! Slice 36: `list_endpoints` says how each header command's last run went, without running it.
//!
//! An expired login is the most common reason a request fails. When the agent can see that the command behind a
//! header failed, it can tell the user to sign in again instead of reporting a bare 401.

use serde_json::json;

use crate::support::{FixtureApi, Mcp, MockServer};

/// A config whose `live` endpoint has a literal header and one from `token.sh` in `mode`.
fn config(mock: &MockServer, mode: &str) -> String {
    format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\nX-Client = \"fieldtrip\"\n\
         Authorization = {{ command = [\"sh\", \"token.sh\", \"{mode}\"], format = \"Bearer {{}}\" }}\n",
        mock.url()
    )
}

/// The line `list_endpoints` gives for the `Authorization` header.
fn status_line(mcp: &mut Mcp) -> String {
    let reply = mcp.call("list_endpoints", &json!({}));
    assert!(!reply.is_error, "list_endpoints should succeed: {}", reply.text);
    reply
        .text
        .lines()
        .find(|line| line.trim_start().starts_with("header Authorization:"))
        .unwrap_or_else(|| panic!("a status line for the command header: {}", reply.text))
        .trim()
        .to_owned()
}

#[test]
fn a_command_that_has_not_run_is_reported_as_not_run_yet() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, "count"));

    assert_eq!(status_line(&mut mcp), "header Authorization: command not run yet");
    assert!(
        !mcp.dir().join("token-runs").exists(),
        "listing the endpoints must not run the command"
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_command_that_worked_is_reported_as_ok() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, "count"));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));
    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    status_line(&mut mcp);
    let line = status_line(&mut mcp);

    assert!(
        line.starts_with("header Authorization: command ok, last run ") && line.ends_with(" ago"),
        "says the command worked and when: {line}"
    );
    assert_eq!(
        std::fs::read_to_string(mcp.dir().join("token-runs"))
            .unwrap_or_default()
            .trim(),
        "1",
        "listing twice ran nothing more"
    );
    assert!(!line.contains("t0ken"), "the value is never shown: {line}");
}

#[test]
fn a_command_that_failed_gives_its_exit_status_and_no_output() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, "fail"));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));
    assert!(reply.is_error, "a failed command is an error: {}", reply.text);
    let line = status_line(&mut mcp);

    assert!(
        line.starts_with("header Authorization: command failed, last run ") && line.contains(" ago: exit 3."),
        "says the command failed, when, and its exit status: {line}"
    );
    assert!(
        line.contains("sign"),
        "says the user probably has to sign in again: {line}"
    );
    assert!(!line.contains("sekrit"), "stdout is never shown: {line}");
    assert_eq!(mock.requests().len(), 0, "nothing was sent without the header");
}

#[test]
fn a_command_that_printed_nothing_says_so() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, "empty"));

    mcp.call("introspect", &json!({ "endpoint": "live" }));

    let line = status_line(&mut mcp);
    assert!(
        line.starts_with("header Authorization: command failed, last run ") && line.contains("printed nothing"),
        "{line}"
    );
}

#[test]
fn literal_and_env_headers_have_no_status_line() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock, "ok"));

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        !reply.text.contains("header X-Client:"),
        "only commands have a last run to report: {}",
        reply.text
    );
}

#[test]
fn a_shared_template_command_reports_the_same_run_for_every_endpoint() {
    let mock = MockServer::serving(FixtureApi::new());
    let config = format!(
        "[templates.acme.headers]\nAuthorization = {{ command = [\"sh\", \"token.sh\", \"count\"] }}\n\n\
         [endpoints.one]\ntemplate = \"acme\"\nurl = \"{url}\"\n\n\
         [endpoints.two]\ntemplate = \"acme\"\nurl = \"{url}\"\n",
        url = mock.url()
    );
    let mut mcp = Mcp::start(&config);

    mcp.call("introspect", &json!({ "endpoint": "one" }));
    let reply = mcp.call("list_endpoints", &json!({}));

    let ok = reply
        .text
        .lines()
        .filter(|line| line.trim().starts_with("header Authorization: command ok"))
        .count();
    assert_eq!(
        ok, 2,
        "both endpoints share the header and its last run: {}",
        reply.text
    );
}
