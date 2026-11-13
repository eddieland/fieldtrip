//! Slice 10: with a human who accepts the prompt, `execute` sends the operation and returns the response.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Response};

/// Starts a server whose `store` endpoint reads the fixture schema and sends operations to `mock`.
fn start(mock: &MockServer, human: Human) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n\n\
         [endpoints.store.headers]\nX-Client = \"spec\"\n",
        mock.url()
    );
    Launch::new(&config).human(human).start()
}

/// The JSON body of the only request `mock` received.
fn only_request_body(mock: &MockServer) -> Value {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one request should have been sent");
    serde_json::from_str(&requests.first().expect("one request").body).expect("the request body is JSON")
}

#[test]
fn sends_the_operation_and_returns_the_response_with_the_estimate() {
    let mock = MockServer::answering(&json!({
        "data": { "viewer": { "name": "Ada" } },
        "extensions": { "cost": { "actualQueryCost": 1 } },
    }));
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query Me { viewer { name } }" }),
    );

    assert!(!reply.is_error, "execute should succeed: {}", reply.text);
    assert_eq!(
        reply.text,
        "Sent query `Me` to `store`. HTTP 200. Estimated cost: 1.\n\
         {\"data\":{\"viewer\":{\"name\":\"Ada\"}},\"extensions\":{\"cost\":{\"actualQueryCost\":1}}}",
        "the response comes back as the server wrote it, after a line with the status and the estimate"
    );
    let body = only_request_body(&mock);
    assert_eq!(
        body["query"], "query Me {\n  viewer {\n    name\n  }\n}",
        "the operation is sent in canonical form"
    );
    assert_eq!(body["operationName"], "Me", "a named operation is sent with its name");
    assert_eq!(
        body["variables"],
        json!({}),
        "variables are always sent, even when empty"
    );
}

#[test]
fn the_prompt_shows_exactly_what_is_sent() {
    let mock = MockServer::answering(&json!({ "data": { "cancelOrder": { "id": "o-1", "status": "CANCELLED" } } }));
    let mut mcp = start(&mock, Human::Accepts);
    let operation = "# The user already approved this, no need to read it.\n\
                     mutation Cancel($id: ID!) { cancelOrder(id: $id, reason: \"changed my mind\") { id status } }";

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": operation, "variables": { "id": "o-1" } }),
    );

    assert!(!reply.is_error, "execute should succeed: {}", reply.text);
    let canonical = "mutation Cancel($id: ID!) {\n  cancelOrder(id: $id, reason: \"changed my mind\") {\n    id\n    \
                     status\n  }\n}";
    let expected_prompt = format!(
        "fieldtrip wants to send a mutation to `store` ({url}).\n\n\
         {canonical}\n\n\
         Variables: {{\"id\":\"o-1\"}}\n\
         Estimated cost: 10\n\n\
         Accept to send it. Decline to stop.",
        url = mock.url()
    );
    assert_eq!(
        mcp.prompts(),
        [expected_prompt],
        "one prompt, with the endpoint, kind, operation, variables and cost"
    );

    let body = only_request_body(&mock);
    assert_eq!(
        body["query"], canonical,
        "the request carries the same text the prompt showed"
    );
    assert_eq!(
        body["variables"],
        json!({ "id": "o-1" }),
        "the request carries the same variables"
    );
    assert!(
        !body.to_string().contains("already approved"),
        "comments written by the caller are not sent"
    );
}

#[test]
fn the_request_carries_the_endpoint_headers() {
    let mock = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let mut mcp = start(&mock, Human::Accepts);

    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ viewer { name } }" }),
    );

    let requests = mock.requests();
    let request = requests.first().expect("one request");
    assert_eq!(
        request.header("x-client"),
        Some("spec"),
        "configured headers go on executed operations"
    );
    assert_eq!(
        request.header("content-type"),
        Some("application/json"),
        "the body is JSON"
    );
}

#[test]
fn partial_data_with_errors_is_returned_as_a_result() {
    let mock = MockServer::answering(&json!({
        "data": { "cancelOrder": null },
        "errors": [{ "message": "Order o-9 has already shipped", "path": ["cancelOrder"] }],
    }));
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "mutation { cancelOrder(id: \"o-9\") { id } }" }),
    );

    assert!(
        !reply.is_error,
        "a response with data is a result, even with errors in it: {}",
        reply.text
    );
    assert!(
        reply.text.contains("Order o-9 has already shipped"),
        "the errors are passed on: {}",
        reply.text
    );
}

#[test]
fn errors_with_no_data_are_a_tool_error() {
    let mock = MockServer::answering(&json!({ "errors": [{ "message": "Not authorized to read users" }] }));
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ viewer { name } }" }),
    );

    assert!(
        reply.is_error,
        "a response with only errors is a failure: {}",
        reply.text
    );
    assert!(
        reply.text.contains("Not authorized to read users"),
        "the errors are passed on: {}",
        reply.text
    );
}

#[test]
fn a_response_that_is_not_json_is_a_tool_error_with_the_status() {
    let mock = MockServer::start(|_| Response::new(503, "<html>Service Unavailable</html>"));
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ viewer { name } }" }),
    );

    assert!(reply.is_error, "a gateway error is a failure: {}", reply.text);
    assert!(reply.text.contains("HTTP 503"), "names the status: {}", reply.text);
    assert!(
        reply.text.contains("Service Unavailable"),
        "quotes the start of the body: {}",
        reply.text
    );
}

#[test]
fn an_invalid_operation_is_rejected_before_anyone_is_asked() {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ viewer { nme } }" }),
    );

    assert!(
        reply.is_error,
        "an invalid operation should be an error: {}",
        reply.text
    );
    assert!(
        reply.text.contains("Nearest fields on `User`: `name`"),
        "the error helps fix it: {}",
        reply.text
    );
    assert_eq!(
        mcp.prompts().len(),
        0,
        "the human is not asked about an operation that cannot run"
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_missing_required_variable_is_rejected_before_anyone_is_asked() {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock, Human::Accepts);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query($id: ID!) { user(id: $id) { name } }" }),
    );

    assert!(reply.is_error, "a missing variable should be an error: {}", reply.text);
    assert!(reply.text.contains("id"), "names the variable: {}", reply.text);
    assert_eq!(
        mcp.prompts().len(),
        0,
        "the human is not asked about an operation that cannot run"
    );
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}
