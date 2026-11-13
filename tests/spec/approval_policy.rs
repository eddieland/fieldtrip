//! Slice 12: which operations prompt, as set per endpoint by `auto_approve_cost` and `approval`.
//!
//! Most tests use a human who declines. If a prompt is shown, nothing is sent, so a request reaching the mock
//! proves that no prompt was involved.

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer};

/// A query that costs 1.
const CHEAP_QUERY: &str = "{ viewer { name } }";
/// A query that costs 5.
const QUERY_COSTING_5: &str = "{ search(text: \"x\", limit: 5) { __typename } }";
/// A query that costs 10.
const QUERY_COSTING_10: &str = "{ products { title } }";
/// A mutation that costs 10.
const MUTATION_COSTING_10: &str = "mutation { deleteUser(id: \"u-1\") }";

/// Starts a server whose `store` endpoint has `settings` and sends operations to `mock`.
fn start(mock: &MockServer, settings: &str, human: Human) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    );
    Launch::new(&config).human(human).start()
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "ok": true } }))
}

#[test]
fn a_query_within_the_threshold_runs_with_no_prompt() {
    let mock = mock();
    let mut mcp = start(&mock, "auto_approve_cost = 5\n", Human::Declines);

    let cheap = mcp.call("execute", &json!({ "endpoint": "store", "operation": CHEAP_QUERY }));
    let at_the_limit = mcp.call("execute", &json!({ "endpoint": "store", "operation": QUERY_COSTING_5 }));

    assert!(!cheap.is_error, "a query costing 1 runs: {}", cheap.text);
    assert!(
        !at_the_limit.is_error,
        "a query costing exactly the threshold runs: {}",
        at_the_limit.text
    );
    assert_eq!(mcp.prompts().len(), 0, "neither query prompts");
    assert_eq!(mock.requests().len(), 2, "both queries are sent");
    assert!(
        cheap.text.starts_with(
            "Sent query to `store`. Estimated cost: 1. The user was not asked: the cost is within \
             `auto_approve_cost` (5).\nHTTP 200 · "
        ),
        "the result says why nobody was asked: {}",
        cheap.text
    );
}

#[test]
fn a_query_above_the_threshold_prompts() {
    let mock = mock();
    let mut mcp = start(&mock, "auto_approve_cost = 5\n", Human::Declines);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": QUERY_COSTING_10 }),
    );

    assert!(reply.is_error, "the human declined: {}", reply.text);
    assert_eq!(
        mcp.prompts().len(),
        1,
        "a query costing more than the threshold prompts"
    );
    assert_eq!(mock.requests().len(), 0, "and a decline sends nothing");
}

#[test]
fn a_mutation_prompts_even_when_a_query_of_the_same_cost_does_not() {
    let mock = mock();
    let mut mcp = start(&mock, "auto_approve_cost = 50\n", Human::Declines);

    let query = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": QUERY_COSTING_10 }),
    );
    assert!(
        !query.is_error,
        "a query costing 10 is within a threshold of 50: {}",
        query.text
    );
    assert_eq!(mcp.prompts().len(), 0, "the query does not prompt");

    let mutation = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": MUTATION_COSTING_10 }),
    );
    assert!(mutation.is_error, "the human declined the mutation: {}", mutation.text);
    assert_eq!(mcp.prompts().len(), 1, "a mutation costing 10 prompts all the same");
    assert_eq!(mock.requests().len(), 1, "only the query was sent");
}

#[test]
fn without_a_threshold_every_query_prompts() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Declines);

    mcp.call("execute", &json!({ "endpoint": "store", "operation": CHEAP_QUERY }));

    assert_eq!(mcp.prompts().len(), 1, "the default is to ask");
    assert_eq!(mock.requests().len(), 0, "and a decline sends nothing");
}

#[test]
fn approval_left_to_the_client_runs_with_no_prompt() {
    let mock = mock();
    // This client cannot show prompts at all, which is the case the setting exists for.
    let mut mcp = start(&mock, "approval = \"client\"\n", Human::Absent);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": MUTATION_COSTING_10 }),
    );

    assert!(!reply.is_error, "the mutation runs: {}", reply.text);
    assert_eq!(mock.requests().len(), 1, "the mutation is sent");
    assert!(
        reply.text.starts_with(
            "Sent mutation to `store`. Estimated cost: 10. The user was not asked: approval is left to the MCP \
             client for this endpoint.\nHTTP 200 · "
        ),
        "the result says why nobody was asked: {}",
        reply.text
    );
}

#[test]
fn approval_left_to_the_client_does_not_prompt_a_client_that_could() {
    let mock = mock();
    let mut mcp = start(&mock, "approval = \"client\"\n", Human::Declines);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": MUTATION_COSTING_10 }),
    );

    assert!(!reply.is_error, "the mutation runs: {}", reply.text);
    assert_eq!(
        mcp.prompts().len(),
        0,
        "fieldtrip's own prompt is off for this endpoint"
    );
}

#[test]
fn the_policy_is_per_endpoint() {
    let mock = mock();
    let config = format!(
        "[endpoints.open]\nurl = \"{url}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n\n\
         [endpoints.guarded]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n",
        url = mock.url()
    );
    let mut mcp = Launch::new(&config).human(Human::Declines).start();

    mcp.call(
        "execute",
        &json!({ "endpoint": "open", "operation": MUTATION_COSTING_10 }),
    );
    mcp.call(
        "execute",
        &json!({ "endpoint": "guarded", "operation": MUTATION_COSTING_10 }),
    );

    assert_eq!(mcp.prompts().len(), 1, "only the guarded endpoint prompts");
    assert_eq!(mock.requests().len(), 1, "only the open endpoint's mutation is sent");
}

#[test]
fn list_endpoints_shows_the_policy() {
    let mock = mock();
    let config = format!(
        "[endpoints.a]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n\n\
         [endpoints.b]\nurl = \"{url}\"\nschema = \"schema.graphql\"\nauto_approve_cost = 50\n\n\
         [endpoints.c]\nurl = \"{url}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        url = mock.url()
    );
    let mut mcp = Mcp::start(&config);

    let text = mcp.call("list_endpoints", &json!({})).text;

    assert!(
        text.contains("  approval: the user is asked before every operation\n"),
        "the default policy:\n{text}"
    );
    assert!(
        text.contains("  approval: the user is asked, except for queries with an estimated cost of 50 or less\n"),
        "a threshold:\n{text}"
    );
    assert!(
        text.contains("  approval: left to the MCP client\n"),
        "client approval:\n{text}"
    );
}

#[test]
fn an_unknown_approval_setting_is_refused_at_startup() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\napproval = \"never\"\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("`approval`"), "names the setting: {stderr}");
    assert!(
        stderr.contains("elicit") && stderr.contains("client"),
        "says what it can be: {stderr}"
    );
}
