//! Slice 59: the prompt for a query has one optional field, with which the human can approve that query, whatever its
//! variables, until the server restarts.
//!
//! Only an explicit choice of the session counts. An accept that fills nothing in approves the one request it was
//! asked about, as before, so a client that accepts without showing the prompt can never grant more. A mutation's
//! prompt has no field, and a mutation prompts every time.

use rmcp::model::{CallToolResponse, InputRequest, InputResponses};
use serde_json::{Value, json};

use crate::support::{FOR_SESSION, Human, Launch, Mcp, MockServer, Reply, Retry, SdkClient, sdk_reply};

const QUERY: &str = "query UserName($id: ID!) { user(id: $id) { name } }";
const OTHER_QUERY: &str = "query UserEmail($id: ID!) { user(id: $id) { email } }";
const MUTATION: &str = "mutation Remove($id: ID!) { deleteUser(id: $id) }";

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": null, "deleteUser": true } }))
}

/// A config with the endpoints `store` and `other`, both sending to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    let url = mock.url();
    format!(
        "{settings}[endpoints.store]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n\n\
         [endpoints.other]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n"
    )
}

/// Starts a server on the older protocol revisions, whose human answers as `human` does.
fn start(mock: &MockServer, human: Human) -> Mcp {
    Launch::new(&config(mock, "")).human(human).start()
}

/// The arguments of `execute` for `operation` on `endpoint`, for the user with `id`.
fn arguments(endpoint: &str, operation: &str, id: &str) -> Value {
    json!({ "endpoint": endpoint, "operation": operation, "variables": { "id": id } })
}

/// Executes `operation` on `store` for the user with `id`.
fn execute(mcp: &mut Mcp, operation: &str, id: &str) -> Reply {
    mcp.call("execute", &arguments("store", operation, id))
}

#[test]
fn a_query_prompt_offers_the_session_and_a_mutation_prompt_has_no_field() {
    let mock = mock();
    let mut mcp = start(&mock, Human::Accepts);

    execute(&mut mcp, QUERY, "u-1");
    execute(&mut mcp, MUTATION, "u-1");

    let forms = mcp.forms();
    assert_eq!(forms.len(), 2, "both were asked about");
    let field = forms[0]
        .pointer("/properties/approve")
        .expect("a query's prompt has one field");
    assert_eq!(field["type"], "string");
    assert_eq!(
        field["enum"],
        json!(["this request only", FOR_SESSION]),
        "the choice is between this request and the session"
    );
    assert_eq!(field["default"], "this request only", "one request is the default");
    assert!(
        forms[0].get("required").is_none_or(|required| required == &json!([])),
        "the field is optional: {}",
        forms[0]
    );
    assert_eq!(
        forms[1]["properties"],
        json!({}),
        "a mutation's prompt is a plain accept or decline"
    );
    assert!(
        mcp.prompts()[0].ends_with("Accept to send it. Decline to stop."),
        "the prompt's text is as before: {}",
        mcp.prompts()[0]
    );
}

#[test]
fn a_query_approved_for_the_session_runs_again_with_other_variables_and_no_prompt() {
    let mock = mock();
    let mut mcp = start(&mock, Human::AcceptsForSession);

    let first = execute(&mut mcp, QUERY, "u-1");
    let second = execute(&mut mcp, QUERY, "u-2");
    // The same query written differently has the same canonical text.
    let third = execute(&mut mcp, "query UserName($id:ID!){user(id:$id){name}} # again", "u-3");

    assert!(!first.is_error, "{}", first.text);
    assert!(
        first.text.starts_with(
            "Sent query `UserName` to `store`. Estimated cost: 1. The user approved this query for the rest of the \
             session: until fieldtrip restarts it runs without a prompt, whatever its variables.\nHTTP 200 · "
        ),
        "the result says what was approved: {}",
        first.text
    );
    for later in [&second, &third] {
        assert!(!later.is_error, "{}", later.text);
        assert!(
            later.text.starts_with(
                "Sent query `UserName` to `store`. Estimated cost: 1. The user was not asked: they approved this \
                 query, with any variables, for the rest of the session.\nHTTP 200 · "
            ),
            "the result says why nobody was asked: {}",
            later.text
        );
    }
    assert_eq!(mcp.prompts().len(), 1, "the human was asked once");
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "all three were sent");
    assert!(requests[2].body.contains("\"u-3\""), "with their own variables");
}

#[test]
fn an_accept_that_chooses_nothing_approves_one_request() {
    let mock = mock();
    let mut mcp = start(&mock, Human::Accepts);

    let first = execute(&mut mcp, QUERY, "u-1");
    let second = execute(&mut mcp, QUERY, "u-1");

    assert!(!first.is_error && !second.is_error, "both run");
    assert!(
        !first.text.contains("session"),
        "nothing was approved beyond the request: {}",
        first.text
    );
    assert_eq!(mcp.prompts().len(), 2, "the human is asked each time");
}

#[test]
fn the_approval_covers_one_query_on_one_endpoint() {
    let mock = mock();
    let mut mcp = start(&mock, Human::AcceptsForSession);

    execute(&mut mcp, QUERY, "u-1");
    assert_eq!(mcp.prompts().len(), 1);

    execute(&mut mcp, OTHER_QUERY, "u-1");
    assert_eq!(mcp.prompts().len(), 2, "another query is asked about");

    mcp.call("execute", &arguments("other", QUERY, "u-1"));
    assert_eq!(
        mcp.prompts().len(),
        3,
        "the same query on another endpoint is asked about"
    );

    execute(&mut mcp, QUERY, "u-9");
    assert_eq!(mcp.prompts().len(), 3, "the approved query still runs unasked");
}

#[test]
fn a_mutation_prompts_every_time_whatever_the_answer_holds() {
    let mock = mock();
    let mut mcp = start(&mock, Human::AcceptsForSession);

    let first = execute(&mut mcp, MUTATION, "u-1");
    let second = execute(&mut mcp, MUTATION, "u-1");

    assert!(!first.is_error && !second.is_error, "both run");
    assert!(
        !first.text.contains("session"),
        "a mutation is never approved for the session: {}",
        first.text
    );
    assert_eq!(mcp.prompts().len(), 2, "the human is asked each time");
}

#[test]
fn the_approval_ends_with_the_server() {
    let mock = mock();
    let mut first = start(&mock, Human::AcceptsForSession);
    execute(&mut first, QUERY, "u-1");
    drop(first);

    let mut second = start(&mock, Human::Declines);
    let reply = execute(&mut second, QUERY, "u-1");

    assert!(reply.is_error, "a new server asks again: {}", reply.text);
    assert_eq!(mock.requests().len(), 1, "only the first server sent it");
}

#[test]
fn the_audit_log_says_which_runs_the_session_approval_let_through() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "audit_log = \"audit.jsonl\"\n\n"))
        .human(Human::AcceptsForSession)
        .start();

    execute(&mut mcp, QUERY, "u-1");
    execute(&mut mcp, QUERY, "u-2");

    let log = std::fs::read_to_string(mcp.dir().join("audit.jsonl")).expect("read the audit log");
    let lines: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert_eq!(lines[0]["approval"], "accepted");
    assert_eq!(
        lines[0]["reason"],
        "the user approved this query for the rest of the session"
    );
    assert_eq!(lines[1]["approval"], "session");
    assert_eq!(lines[1]["variables"], json!({ "id": "u-2" }));
    assert_eq!(
        lines[1]["hash"], lines[0]["hash"],
        "the approval is tied to the text's hash"
    );
}

/// Starts a server with a 2026-07-28 client whose human answers as `human` does.
fn start_sdk(mock: &MockServer, human: Human) -> SdkClient {
    SdkClient::start(&Launch::new(&config(mock, "")).human(human))
}

/// Sends one round of `execute` on `store` and returns what the server answered.
fn round(client: &SdkClient, operation: &str, id: &str, retry: Option<Retry>) -> CallToolResponse {
    client.call_once("execute", &arguments("store", operation, id), retry)
}

/// The form and the state of the approval a first round asks for.
fn asked(response: CallToolResponse) -> (Value, Option<String>) {
    let CallToolResponse::InputRequired(required) = response else {
        panic!("the first round should ask for input: {response:?}");
    };
    let requests = required.input_requests.expect("the server asks for something");
    let Some(InputRequest::Elicitation(request)) = requests.get("approval") else {
        panic!("the request is an elicitation under the key `approval`: {requests:?}");
    };
    let request = serde_json::to_value(request).expect("serialize the request");
    let form = request
        .pointer("/params/requestedSchema")
        .cloned()
        .unwrap_or(Value::Null);
    (form, required.request_state)
}

/// What a client sends back when the human answers with `action` and chooses the session.
fn for_session(action: &str, state: Option<String>) -> Retry {
    let answer = json!({ "action": action, "content": { "approve": FOR_SESSION } });
    Retry {
        responses: InputResponses::from([("approval".to_owned(), answer)]),
        state,
    }
}

#[test]
fn on_revision_2026_the_session_is_offered_and_approved_by_the_retry() {
    let mock = mock();
    let client = start_sdk(&mock, Human::AcceptsForSession);

    let first = client.call("execute", &arguments("store", QUERY, "u-1"));
    let second = client.call("execute", &arguments("store", QUERY, "u-2"));
    let mutation = client.call("execute", &arguments("store", MUTATION, "u-1"));
    let again = client.call("execute", &arguments("store", MUTATION, "u-1"));

    assert!(
        first
            .text
            .contains("The user approved this query for the rest of the session"),
        "{}",
        first.text
    );
    assert!(
        second.text.contains("The user was not asked: they approved this query"),
        "{}",
        second.text
    );
    assert!(!mutation.is_error && !again.is_error, "the mutations run");
    assert_eq!(
        client.prompts().len(),
        3,
        "the query was asked about once and the mutation both times"
    );
    assert_eq!(mock.requests().len(), 4);
}

#[test]
fn on_revision_2026_the_form_has_the_field_for_a_query_only() {
    let mock = mock();
    let client = start_sdk(&mock, Human::Accepts);

    let (query_form, _) = asked(round(&client, QUERY, "u-1", None));
    let (mutation_form, _) = asked(round(&client, MUTATION, "u-1", None));

    assert_eq!(
        query_form.pointer("/properties/approve/enum"),
        Some(&json!(["this request only", FOR_SESSION]))
    );
    assert_eq!(mutation_form["properties"], json!({}));
    assert_eq!(mock.requests().len(), 0, "nothing is sent while nobody has answered");
}

#[test]
fn on_revision_2026_a_decline_that_chooses_the_session_approves_nothing() {
    let mock = mock();
    let client = start_sdk(&mock, Human::Accepts);
    let (_, state) = asked(round(&client, QUERY, "u-1", None));

    let CallToolResponse::Complete(declined) = round(&client, QUERY, "u-1", Some(for_session("decline", state))) else {
        panic!("a retry finishes the call");
    };
    assert!(sdk_reply(&declined).is_error, "a decline sends nothing");

    let next = round(&client, QUERY, "u-1", None);
    assert!(
        matches!(next, CallToolResponse::InputRequired(_)),
        "the query is asked about again: {next:?}"
    );
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn on_revision_2026_a_session_approval_needs_the_ticket_of_its_prompt() {
    let mock = mock();
    let client = start_sdk(&mock, Human::Accepts);
    let (_, state) = asked(round(&client, QUERY, "u-1", None));

    // The human saw the query for `u-1`. The retry claims a session approval for another query.
    let CallToolResponse::Complete(forged) = round(&client, OTHER_QUERY, "u-1", Some(for_session("accept", state)))
    else {
        panic!("a retry finishes the call");
    };
    assert!(sdk_reply(&forged).is_error, "the approval was not for this query");

    let next = round(&client, OTHER_QUERY, "u-1", None);
    assert!(
        matches!(next, CallToolResponse::InputRequired(_)),
        "nothing was approved for the session: {next:?}"
    );
    assert_eq!(mock.requests().len(), 0);
}
