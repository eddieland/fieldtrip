//! Slice 16: on protocol revision 2026-07-28, approval travels as an "input required" result and its retry.
//!
//! These tests use the MCP SDK's own client. On this revision a server cannot send a request to the client, so
//! `execute` answers the first call by asking for input, and runs only when the call comes back with an accept
//! for that same operation.

use rmcp::model::{CallToolResponse, InputRequest, InputResponses};
use serde_json::{Value, json};

use crate::support::{Human, Launch, MockServer, Retry, SdkClient};

const QUERY: &str = "{ viewer { name } }";
const MUTATION: &str = "mutation { deleteUser(id: \"u-1\") }";

/// Starts a server whose `store` endpoint has `settings`, with a 2026-07-28 client that answers as `human` does.
fn start(mock: &MockServer, settings: &str, human: Human) -> SdkClient {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    );
    SdkClient::start(&Launch::new(&config).human(human))
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "viewer": null, "deleteUser": true } }))
}

/// The arguments of `execute` for `operation` on the `store` endpoint.
fn arguments(operation: &str) -> Value {
    json!({ "endpoint": "store", "operation": operation })
}

/// The answers a client sends when the human responds with `action`.
fn answer(action: &str) -> InputResponses {
    InputResponses::from([(
        "approval".to_owned(),
        serde_json::from_value(json!({ "action": action })).expect("json"),
    )])
}

/// Sends the first round of `execute` and returns the prompt and the state the server handed out.
fn first_round(client: &SdkClient, operation: &str) -> (String, Option<String>) {
    let CallToolResponse::InputRequired(required) = client.call_once("execute", &arguments(operation), None) else {
        panic!("the first round of `execute` should ask for input");
    };
    let requests = required.input_requests.expect("the server asks for something");
    assert_eq!(requests.len(), 1, "one thing is asked for: the approval");
    let Some(InputRequest::Elicitation(request)) = requests.get("approval") else {
        panic!("the request is an elicitation under the key `approval`: {requests:?}");
    };
    let request = serde_json::to_value(request).expect("serialize the request");
    let prompt = request
        .pointer("/params/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    (prompt, required.request_state)
}

/// Sends a retry of `execute` for `operation` and returns the text and error flag of what came back.
fn retry(client: &SdkClient, operation: &str, action: &str, state: Option<String>) -> crate::support::Reply {
    let retry = Retry {
        responses: answer(action),
        state,
    };
    match client.call_once("execute", &arguments(operation), Some(retry)) {
        CallToolResponse::Complete(result) => crate::support::sdk_reply(&result),
        other => panic!("a retry should finish the call, got {other:?}"),
    }
}

#[test]
fn the_first_round_asks_for_approval_and_sends_nothing() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);

    let (prompt, state) = first_round(&client, MUTATION);

    let expected = format!(
        "fieldtrip wants to send a mutation to `store` ({}).\n\n\
         mutation {{\n  deleteUser(id: \"u-1\")\n}}\n\n\
         Variables: none\nEstimated cost: 10\n\n\
         Accept to send it. Decline to stop.",
        mock.url()
    );
    assert_eq!(prompt, expected, "the prompt is the same one the older revisions show");
    assert!(
        state.is_some(),
        "the server hands out state to tie the answer to this operation"
    );
    assert_eq!(
        mock.requests().len(),
        0,
        "nothing is sent while the human has not answered"
    );
}

#[test]
fn an_accepted_approval_sends_the_operation() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);

    let reply = client.call("execute", &arguments(MUTATION));

    assert!(!reply.is_error, "the mutation runs: {}", reply.text);
    assert!(
        reply
            .text
            .starts_with("Sent mutation to `store`. Estimated cost: 10.\nHTTP 200 · "),
        "{}",
        reply.text
    );
    assert_eq!(client.prompts().len(), 1, "the human was asked once");
    assert_eq!(mock.requests().len(), 1, "the operation was sent once");
}

#[test]
fn anything_but_an_accept_sends_nothing() {
    for (human, expected) in [
        (Human::Declines, "The user declined"),
        (Human::Cancels, "dismissed the approval prompt"),
        (Human::Absent, "does not support elicitation"),
    ] {
        let mock = mock();
        let client = start(&mock, "", human);

        for operation in [QUERY, MUTATION] {
            let reply = client.call("execute", &arguments(operation));
            assert!(
                reply.is_error,
                "{human:?}: `{operation}` must not be reported as done: {}",
                reply.text
            );
            assert!(
                reply.text.contains(expected),
                "{human:?}: says what happened: {}",
                reply.text
            );
            assert!(
                reply.text.contains("Nothing was sent"),
                "{human:?}: says nothing was sent: {}",
                reply.text
            );
        }
        assert_eq!(mock.requests().len(), 0, "{human:?}: the endpoint must receive nothing");
    }
}

#[test]
fn an_approval_does_not_carry_over_to_another_operation() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);
    let (_, state) = first_round(&client, QUERY);

    // The human saw the query. The retry asks to run the mutation with that approval.
    let reply = retry(&client, MUTATION, "accept", state);

    assert!(
        reply.is_error,
        "an approval for another operation is refused: {}",
        reply.text
    );
    assert!(
        reply.text.contains("Nothing was sent"),
        "says nothing was sent: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "the mutation is not sent");
}

#[test]
fn an_approval_is_used_up_by_one_send() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);
    let (_, state) = first_round(&client, MUTATION);

    let first = retry(&client, MUTATION, "accept", state.clone());
    let second = retry(&client, MUTATION, "accept", state);

    assert!(!first.is_error, "the approved mutation runs: {}", first.text);
    assert!(second.is_error, "replaying the approval is refused: {}", second.text);
    assert_eq!(mock.requests().len(), 1, "the mutation is sent once");
}

#[test]
fn an_accept_that_no_prompt_asked_for_is_refused() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);

    let without_state = retry(&client, MUTATION, "accept", None);
    let made_up_state = retry(
        &client,
        MUTATION,
        "accept",
        Some("00000000000000000000000000000000".to_owned()),
    );

    assert!(
        without_state.is_error,
        "an accept with no state is refused: {}",
        without_state.text
    );
    assert!(
        made_up_state.is_error,
        "an accept with made-up state is refused: {}",
        made_up_state.text
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_declined_retry_sends_nothing() {
    let mock = mock();
    let client = start(&mock, "", Human::Accepts);
    let (_, state) = first_round(&client, MUTATION);

    let reply = retry(&client, MUTATION, "decline", state);

    assert!(reply.is_error, "a decline is not a result: {}", reply.text);
    assert!(
        reply.text.contains("The user declined"),
        "says the human declined: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn the_approval_policy_applies_on_this_revision_too() {
    let mock = mock();
    let client = start(&mock, "auto_approve_cost = 5\n", Human::Declines);

    let query = client.call("execute", &arguments(QUERY));
    assert!(!query.is_error, "a cheap query runs without asking: {}", query.text);
    assert_eq!(client.prompts().len(), 0, "the query does not prompt");

    let mutation = client.call("execute", &arguments(MUTATION));
    assert!(mutation.is_error, "the human declined the mutation: {}", mutation.text);
    assert_eq!(client.prompts().len(), 1, "the mutation prompts");
    assert_eq!(mock.requests().len(), 1, "only the query was sent");
}

#[test]
fn the_other_tools_work_on_this_revision() {
    let mock = MockServer::unreachable();
    let client = start(&mock, "", Human::Absent);

    let described = client.call("describe", &json!({ "endpoint": "store", "name": "Money" }));
    let estimated = client.call("estimate_cost", &arguments(QUERY));

    assert_eq!(
        described.text, "type Money {\n  amountCents: Int!\n  currency: Currency!\n}\n",
        "describe"
    );
    assert!(
        estimated.text.contains("Estimated cost: 1\n"),
        "estimate_cost: {}",
        estimated.text
    );
}
