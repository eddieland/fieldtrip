//! Slice 11: unless the human accepts, `execute` sends nothing.
//!
//! Every test here asserts on the mock's request count, not only on what the tool said.

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer};

/// A query and a mutation. Neither may be sent without an accept.
const OPERATIONS: [&str; 2] = ["{ viewer { name } }", "mutation { deleteUser(id: \"u-1\") }"];

/// Starts a server whose `store` endpoint reads the fixture schema and would send operations to `mock`.
fn start(mock: &MockServer, human: Human) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    Launch::new(&config).human(human).start()
}

/// Runs both operations with a client that answers as `human` does, and returns what the tool said for each.
///
/// Fails unless both calls are tool errors and the mock received nothing.
fn refused(human: Human, expected_prompts: usize) -> Vec<String> {
    let mock = MockServer::answering(&json!({ "data": { "viewer": null, "deleteUser": true } }));
    let mut mcp = start(&mock, human);

    let replies: Vec<String> = OPERATIONS
        .iter()
        .map(|operation| {
            let reply = mcp.call("execute", &json!({ "endpoint": "store", "operation": operation }));
            assert!(
                reply.is_error,
                "{human:?}: `{operation}` must not be reported as done: {}",
                reply.text
            );
            reply.text
        })
        .collect();

    assert_eq!(mock.requests().len(), 0, "{human:?}: the endpoint must receive nothing");
    assert_eq!(
        mcp.prompts().len(),
        expected_prompts,
        "{human:?}: the number of prompts shown"
    );
    replies
}

#[test]
fn a_decline_sends_nothing() {
    for text in refused(Human::Declines, 2) {
        assert!(text.contains("The user declined"), "says the human declined: {text}");
        assert!(text.contains("Nothing was sent"), "says nothing was sent: {text}");
        assert!(
            text.contains("Do not send this operation again"),
            "tells the caller not to retry: {text}"
        );
    }
}

#[test]
fn a_dismissed_prompt_sends_nothing() {
    for text in refused(Human::Cancels, 2) {
        assert!(
            text.contains("dismissed the approval prompt"),
            "says the prompt was dismissed: {text}"
        );
        assert!(text.contains("Nothing was sent"), "says nothing was sent: {text}");
    }
}

#[test]
fn a_client_that_cannot_prompt_sends_nothing() {
    for text in refused(Human::Absent, 0) {
        assert!(text.contains("Nothing was sent"), "says nothing was sent: {text}");
        assert!(
            text.contains("does not support elicitation"),
            "says why nobody could be asked: {text}"
        );
        assert!(
            text.contains("approval = \"client\""),
            "names the setting that opts out: {text}"
        );
    }
}

#[test]
fn a_prompt_that_fails_sends_nothing() {
    for text in refused(Human::Broken, 2) {
        assert!(text.contains("Nothing was sent"), "says nothing was sent: {text}");
        assert!(
            text.contains("The approval prompt failed"),
            "says the prompt failed: {text}"
        );
    }
}

#[test]
fn a_refusal_is_not_remembered() {
    let mock = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let mut mcp = start(&mock, Human::Declines);
    let arguments = json!({ "endpoint": "store", "operation": "{ viewer { name } }" });

    mcp.call("execute", &arguments);
    mcp.call("execute", &arguments);

    assert_eq!(
        mcp.prompts().len(),
        2,
        "the same operation asks again, because each send needs its own answer"
    );
    assert_eq!(mock.requests().len(), 0, "and still nothing is sent");
}

#[test]
fn an_approval_argument_from_the_caller_changes_nothing() {
    let mock = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let mut mcp = start(&mock, Human::Declines);

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": "{ viewer { name } }",
            "approved": true,
            "confirmed": true,
            "approval": "client",
        }),
    );

    assert!(
        reply.is_error,
        "extra arguments must not stand in for the human: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}
