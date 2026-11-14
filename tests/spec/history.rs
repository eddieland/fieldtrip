//! Slice 39: when `audit_log` is set, `history` reads it back, so a calling agent can see what was already tried in
//! this session or an earlier one. It shows only the configured endpoints, newest last, within the output limit.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply};

const QUERY: &str = "query Me { viewer { name } }";

/// Two endpoints, `store` and `admin`, both sending to `mock`, logging to `audit.jsonl`, with `settings` on `store`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "audit_log = \"audit.jsonl\"\n\n[endpoints.store]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n{settings}\n\
         [endpoints.admin]\nurl = \"{url}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        url = mock.url()
    )
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "viewer": { "name": "Ada" } } }))
}

/// A line as an earlier session would have written it.
fn earlier(endpoint: &str, operation: &str) -> String {
    json!({
        "at": "2026-11-12T09:00:00Z",
        "session": "0123456789abcdef",
        "endpoint": endpoint,
        "kind": "query",
        "operation": operation,
        "hash": "sha256:00",
        "text": format!("query {operation} {{ viewer {{ name }} }}"),
        "variables": {},
        "approval": "declined",
        "reason": "the user declined",
        "cost": 1,
    })
    .to_string()
}

/// Executes `operation` on `endpoint`.
fn execute(mcp: &mut Mcp, endpoint: &str, operation: &str) -> Reply {
    mcp.call("execute", &json!({ "endpoint": endpoint, "operation": operation }))
}

/// The JSON lines of a `history` result, parsed.
fn entries(reply: &Reply) -> Vec<Value> {
    reply
        .text
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect()
}

/// The session the result says is this one.
fn this_session(reply: &Reply) -> String {
    let (_, rest) = reply
        .text
        .split_once("This session is `")
        .unwrap_or_else(|| panic!("the result names this session: {}", reply.text));
    rest.split('`').next().unwrap_or_default().to_owned()
}

#[test]
fn it_is_offered_only_with_an_audit_log() {
    let mock = mock();
    let without = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    let mut mcp = Mcp::start(&without);
    assert!(!mcp.tools().contains(&"history".to_owned()), "{:?}", mcp.tools());
    assert!(
        !mcp.instructions().unwrap_or_default().contains("`history`"),
        "the instructions do not point to a tool that is not there"
    );

    let mut mcp = Mcp::start(&config(&mock, ""));
    let tools = mcp.tool_list();
    let history = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("history"))
        .unwrap_or_else(|| panic!("history is offered: {tools:#?}"));
    assert!(
        history["title"].as_str().is_some_and(|title| !title.is_empty()),
        "{history}"
    );
    assert!(
        history["description"].as_str().is_some_and(|text| text.len() > 200),
        "it says when to use it and what it returns: {history}"
    );
    for (argument, schema) in history["inputSchema"]["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        assert!(
            schema["description"].as_str().is_some_and(|text| text.len() > 20),
            "history.{argument} is described: {schema}"
        );
    }
    assert_eq!(
        history.pointer("/annotations/readOnlyHint"),
        Some(&json!(true)),
        "{history}"
    );
    assert_eq!(
        history.pointer("/annotations/openWorldHint"),
        Some(&json!(false)),
        "{history}"
    );
    assert!(
        mcp.instructions().unwrap_or_default().contains("`history`"),
        "the instructions point to it"
    );
}

#[test]
fn it_lists_this_session_and_earlier_ones_oldest_first() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, ""))
        .file("audit.jsonl", &format!("{}\n", earlier("store", "Before")))
        .human(Human::Declines)
        .start();
    let reply = execute(&mut mcp, "store", QUERY);
    assert!(reply.is_error, "{}", reply.text);
    let reply = execute(&mut mcp, "admin", QUERY);
    assert!(!reply.is_error, "{}", reply.text);

    let reply = mcp.call("history", &json!({}));

    assert!(!reply.is_error, "{}", reply.text);
    let entries = entries(&reply);
    assert_eq!(entries.len(), 3, "{}", reply.text);
    assert_eq!(entries[0]["operation"], "Before");
    assert_eq!(entries[1]["approval"], "declined");
    assert_eq!(entries[2]["approval"], "client");
    assert_eq!(entries[2]["endpoint"], "admin");
    let session = this_session(&reply);
    assert_eq!(entries[1]["session"], session.as_str(), "{}", reply.text);
    assert_eq!(entries[2]["session"], session.as_str());
    assert_ne!(entries[0]["session"], session.as_str());
}

#[test]
fn it_filters_by_endpoint_and_shows_only_configured_ones() {
    let mock = mock();
    let written = [
        earlier("store", "One"),
        earlier("elsewhere", "Hidden"),
        earlier("admin", "Two"),
        earlier("store", "Three"),
    ]
    .join("\n");
    let mut mcp = Launch::new(&config(&mock, ""))
        .file("audit.jsonl", &format!("{written}\n"))
        .start();

    let reply = mcp.call("history", &json!({}));
    let operations: Vec<Value> = entries(&reply).iter().map(|entry| entry["operation"].clone()).collect();
    assert_eq!(
        operations,
        [json!("One"), json!("Two"), json!("Three")],
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("elsewhere"), "{}", reply.text);

    let reply = mcp.call("history", &json!({ "endpoint": "store" }));
    let operations: Vec<Value> = entries(&reply).iter().map(|entry| entry["operation"].clone()).collect();
    assert_eq!(operations, [json!("One"), json!("Three")], "{}", reply.text);

    let reply = mcp.call("history", &json!({ "endpoint": "elsewhere" }));
    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("`admin`") && reply.text.contains("`store`"),
        "names the endpoints there are: {}",
        reply.text
    );
}

#[test]
fn limit_keeps_the_most_recent() {
    let mock = mock();
    let written = ["One", "Two", "Three"]
        .map(|operation| earlier("store", operation))
        .join("\n");
    let mut mcp = Launch::new(&config(&mock, ""))
        .file("audit.jsonl", &format!("{written}\nnot json\n"))
        .start();

    let reply = mcp.call("history", &json!({ "limit": 2 }));

    let operations: Vec<Value> = entries(&reply).iter().map(|entry| entry["operation"].clone()).collect();
    assert_eq!(operations, [json!("Two"), json!("Three")], "{}", reply.text);
    assert!(reply.text.contains("2 of 3"), "{}", reply.text);
    assert!(
        reply.text.contains("could not be read"),
        "a broken line is mentioned: {}",
        reply.text
    );
}

#[test]
fn an_empty_log_says_so() {
    let mock = mock();
    let mut mcp = Mcp::start(&config(&mock, ""));

    let reply = mcp.call("history", &json!({}));

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("no `execute` attempts"), "{}", reply.text);
    assert_eq!(this_session(&reply).len(), 16, "the session is named");
}

#[test]
fn a_long_history_is_cut_from_the_oldest_end() {
    let mock = mock();
    let written: Vec<String> = (0..40)
        .map(|index| {
            let mut line: Value = serde_json::from_str(&earlier("store", &format!("Op{index}"))).expect("json");
            line["variables"] = json!({ "note": "x".repeat(400) });
            line.to_string()
        })
        .collect();
    let mut mcp = Launch::new(&config(&mock, "max_output_chars = 3000\n"))
        .file("audit.jsonl", &format!("{}\n", written.join("\n")))
        .start();

    let reply = mcp.call("history", &json!({ "endpoint": "store", "limit": 40 }));

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.chars().count() <= 3000, "{}", reply.text.chars().count());
    let entries = entries(&reply);
    assert!(!entries.is_empty(), "{}", reply.text);
    assert_eq!(
        entries.last().map(|entry| entry["operation"].clone()),
        Some(json!("Op39")),
        "the newest is kept"
    );
    assert!(
        reply.text.contains("older"),
        "says older ones were left out: {}",
        reply.text
    );
}
