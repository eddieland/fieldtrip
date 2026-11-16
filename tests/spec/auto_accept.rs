//! Slice 39: a client known to accept approval prompts without showing them is never asked.
//!
//! OpenCode answers an elicitation that has no fields with an accept, without showing it to anyone. fieldtrip's prompt
//! has no fields, so asking OpenCode would approve every operation unseen. fieldtrip recognizes it by the name in
//! `clientInfo` and refuses instead, on both protocol revisions. The settings that do not depend on its prompt still
//! work.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, SdkClient};

const QUERY: &str = "query Viewer { viewer { name } }";
const MUTATION: &str = "mutation Delete { deleteUser(id: \"u-1\") }";

/// A config whose `store` endpoint has `settings`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    )
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "viewer": null, "deleteUser": true } }))
}

/// Starts a server with an OpenCode client that would accept every prompt.
fn opencode(mock: &MockServer, settings: &str) -> Mcp {
    Launch::new(&config(mock, settings))
        .client("opencode")
        .human(Human::Accepts)
        .start()
}

/// The arguments of `execute` for `operation` on `store`.
fn arguments(operation: &str) -> Value {
    json!({ "endpoint": "store", "operation": operation })
}

/// Checks that `text` is the refusal: it says nothing was sent, names the client, and says what the user can do.
fn assert_refused(text: &str) {
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(text.contains("OpenCode"), "names the client: {text}");
    assert!(
        text.contains("approval = \"client\""),
        "names the setting that fixes it: {text}"
    );
    assert!(
        text.contains("fieldtrip_execute"),
        "names the permission rule to add: {text}"
    );
}

#[test]
fn execute_refuses_instead_of_asking_opencode() {
    let mock = mock();
    let mut mcp = opencode(&mock, "");

    let reply = mcp.call("execute", &arguments(QUERY));

    assert!(reply.is_error, "{}", reply.text);
    assert_refused(&reply.text);
    assert!(mcp.prompts().is_empty(), "no prompt is sent: {:?}", mcp.prompts());
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn execute_refuses_instead_of_asking_opencode_on_2026_07_28() {
    let mock = mock();
    let launch = Launch::new(&config(&mock, "")).client("opencode").human(Human::Accepts);
    let client = SdkClient::start(&launch);

    let reply = client.call("execute", &arguments(QUERY));

    assert!(reply.is_error, "{}", reply.text);
    assert_refused(&reply.text);
    assert!(client.prompts().is_empty(), "no prompt is sent: {:?}", client.prompts());
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn the_name_is_matched_ignoring_case() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, ""))
        .client("OpenCode")
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments(QUERY));

    assert!(reply.is_error, "{}", reply.text);
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn approval_left_to_the_client_still_runs() {
    let mock = mock();
    let mut mcp = opencode(&mock, "approval = \"client\"\n");

    let reply = mcp.call("execute", &arguments(MUTATION));

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(
        mock.requests().len(),
        1,
        "sent, with OpenCode's own permission prompt in charge"
    );
}

#[test]
fn a_query_within_auto_approve_cost_still_runs_but_a_mutation_does_not() {
    let mock = mock();
    let mut mcp = opencode(&mock, "auto_approve_cost = 50\n");

    let query = mcp.call("execute", &arguments(QUERY));
    let mutation = mcp.call("execute", &arguments(MUTATION));

    assert!(!query.is_error, "no prompt was needed: {}", query.text);
    assert!(mutation.is_error, "{}", mutation.text);
    assert_refused(&mutation.text);
    assert!(mcp.prompts().is_empty(), "no prompt is sent: {:?}", mcp.prompts());
    assert_eq!(mock.requests().len(), 1, "only the query is sent");
}

#[test]
fn list_endpoints_says_execute_will_refuse() {
    let mock = mock();
    let config = format!(
        "{}\n[endpoints.dev]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        config(&mock, ""),
        mock.url()
    );
    let mut mcp = Launch::new(&config).client("opencode").human(Human::Accepts).start();

    let listed = mcp.call("list_endpoints", &json!({})).text;

    let approval = |endpoint: &str| {
        listed
            .lines()
            .skip_while(|line| *line != endpoint)
            .find(|line| line.trim_start().starts_with("approval:"))
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        approval("store").contains("OpenCode") && approval("store").contains("refuses"),
        "{listed}"
    );
    assert!(!approval("dev").contains("OpenCode"), "{listed}");
}

#[test]
fn a_warning_is_logged_when_opencode_connects() {
    let mock = mock();
    let mcp = opencode(&mock, "");

    let stderr = mcp.finish();

    assert!(
        stderr.contains("OpenCode") && stderr.contains("`store`"),
        "the log names the client and the endpoints that cannot run: {stderr}"
    );
}

#[test]
fn other_clients_are_asked_as_before() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call("execute", &arguments(QUERY));

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(mcp.prompts().len(), 1, "asked once");
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn the_refusal_is_recorded_in_the_audit_log() {
    let mock = mock();
    let config = format!("audit_log = \"audit.jsonl\"\n\n{}", config(&mock, ""));
    let mut mcp = Launch::new(&config).client("opencode").human(Human::Accepts).start();

    mcp.call("execute", &arguments(QUERY));

    let log = std::fs::read_to_string(mcp.dir().join("audit.jsonl")).expect("the audit log");
    let lines: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    let [entry] = lines.as_slice() else {
        panic!("one attempt, one line: {log}");
    };
    assert_eq!(entry["approval"], "unavailable", "{entry}");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("OpenCode")),
        "{entry}"
    );
    assert!(entry.get("status").is_none(), "nothing was sent: {entry}");
}
