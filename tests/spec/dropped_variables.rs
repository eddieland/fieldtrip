//! Slice 51: a variable that only a removed client directive used is dropped from the saved operation with it, so
//! the operation still validates. `list_operations` shows the variables that are left, and a dropped one passed to
//! `execute` or `estimate_cost` is left out with a note instead of being sent. A variable that nothing used before the
//! directives were removed still breaks the operation, and one that anything else uses stays.

use serde_json::{Value, json};

use crate::support::{Human, Launch, MockServer, SdkClient};

/// A saved query whose `$precision` is used only by a listed directive, and whose `$full` is used by one too, but
/// also by `@include`, which stays.
const GET_USER: &str = "query GetUser($id: ID!, $precision: Int = 6, $full: Boolean! = false) {\n  \
                        user(id: $id) {\n    name @round(digits: $precision)\n    \
                        email @include(if: $full) @mask(when: $full)\n  }\n}\n";

/// A saved query with a variable nothing uses, listed directives or not, next to one that a directive used.
const UNUSED: &str = "query Unused($id: ID!, $precision: Int, $never: Int) {\n  \
                      user(id: $id) { name @round(digits: $precision) }\n}\n";

/// A saved query whose variables are used only inside shared fragments: `$style` by a listed directive there, deep
/// in a list, and `$count` by a field argument.
const VIA_FRAGMENT: &str = "query ViaFragment($id: ID!, $style: String, $count: Int) {\n  \
                            user(id: $id) { ...Rounded }\n}\n";

/// The shared fragment `ViaFragment` spreads, which spreads another.
const ROUNDED: &str = "fragment Rounded on User {\n  name @round(styles: [{ name: $style }])\n  ...Orders\n}\n";

/// A shared fragment that uses `$count` in an argument and in a listed directive.
const ORDERS: &str = "fragment Orders on User { orders(first: $count) @round(digits: $count) { id } }\n";

/// The `store` endpoint, sending operations to `mock`, with the directives above listed.
fn launch(mock: &MockServer) -> Launch {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\noperations = \"queries\"\n\
         fragments = \"queries/fragments\"\nclient_directives = [\"round\", \"mask\"]\n",
        mock.url()
    );
    Launch::new(&config)
        .file("queries/get_user.graphql", GET_USER)
        .file("queries/unused.graphql", UNUSED)
        .file("queries/via_fragment.graphql", VIA_FRAGMENT)
        .file("queries/fragments/rounded.graphql", ROUNDED)
        .file("queries/fragments/orders.graphql", ORDERS)
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": { "name": "Ada" } } }))
}

/// The body of the only request `mock` received.
fn sent(mock: &MockServer) -> Value {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one request is sent");
    serde_json::from_str(&requests[0].body).expect("a JSON body")
}

#[test]
fn a_variable_only_a_removed_directive_used_is_dropped() {
    let mock = mock();
    let mut mcp = launch(&mock).human(Human::Accepts).start();

    let listed = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "GetUser" }));
    assert!(!listed.is_error, "{}", listed.text);
    assert!(!listed.text.contains("BROKEN"), "{}", listed.text);
    assert!(
        listed.text.contains("  variables: $id: ID!, $full: Boolean! = false\n"),
        "{}",
        listed.text
    );
    assert!(!listed.text.contains("precision"), "{}", listed.text);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "GetUser", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "{}", reply.text);
    assert!(!reply.text.contains("precision"), "nothing to note: {}", reply.text);
    let body = sent(&mock);
    let query = body["query"].as_str().expect("a query string");
    assert!(
        query.starts_with("query GetUser($id: ID!, $full: Boolean! = false) {"),
        "{query}"
    );
    assert!(query.contains("email @include(if: $full)"), "{query}");
    assert!(!query.contains("precision") && !query.contains("@mask"), "{query}");
    assert_eq!(body["variables"], json!({ "id": "u-1" }));
}

#[test]
fn the_one_line_list_shows_the_variables_that_are_left() {
    let mut mcp = launch(&MockServer::unreachable()).start();

    let found = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "GetUser" }));

    assert!(!found.is_error, "{}", found.text);
    assert!(
        found.text.contains("$id: ID!, $full: Boolean! = false)"),
        "{}",
        found.text
    );
    assert!(!found.text.contains("precision"), "{}", found.text);
}

#[test]
fn a_dropped_variable_passed_to_execute_is_noted_and_not_sent() {
    let mock = mock();
    let mut mcp = launch(&mock).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation_name": "GetUser",
            "variables": { "id": "u-1", "precision": 2, "full": true },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    for expected in ["`$precision`", "`@round`", "is not sent"] {
        assert!(
            reply.text.contains(expected),
            "expected {expected:?} in: {}",
            reply.text
        );
    }
    assert_eq!(sent(&mock)["variables"], json!({ "id": "u-1", "full": true }));
    let prompts = mcp.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked");
    assert!(
        !prompts[0].contains("precision"),
        "the prompt shows what is sent: {}",
        prompts[0]
    );
}

#[test]
fn a_dropped_variable_is_left_out_on_revision_2026_too() {
    let mock = mock();
    let client = SdkClient::start(&launch(&mock).human(Human::Accepts));

    let reply = client.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation_name": "GetUser",
            "variables": { "id": "u-1", "precision": 2 },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("`$precision`"), "{}", reply.text);
    assert_eq!(sent(&mock)["variables"], json!({ "id": "u-1" }));
    let prompts = client.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked once");
    assert!(!prompts[0].contains("precision"), "{}", prompts[0]);
}

#[test]
fn estimate_cost_notes_a_dropped_variable() {
    let mut mcp = launch(&MockServer::unreachable()).start();

    let reply = mcp.call(
        "estimate_cost",
        &json!({
            "endpoint": "store",
            "operation_name": "GetUser",
            "variables": { "id": "u-1", "precision": 2 },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Valid query `GetUser`"), "{}", reply.text);
    assert!(
        reply.text.contains("`$precision`") && reply.text.contains("`@round`"),
        "{}",
        reply.text
    );
}

#[test]
fn a_variable_that_was_already_unused_still_breaks_the_operation() {
    let mock = mock();
    let mut mcp = launch(&mock).human(Human::Accepts).start();

    let listed = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "Unused" }));
    assert!(listed.text.contains("BROKEN"), "{}", listed.text);
    assert!(listed.text.contains("$never"), "names the variable: {}", listed.text);
    assert!(
        !listed.text.contains("precision"),
        "the dropped one is not a problem: {}",
        listed.text
    );

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "Unused", "variables": { "id": "u-1" } }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("$never"), "{}", reply.text);
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn shared_fragments_count_on_both_sides() {
    let mock = mock();
    let mut mcp = launch(&mock).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation_name": "ViaFragment",
            "variables": { "id": "u-1", "style": "short", "count": 3 },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("`$style`"), "{}", reply.text);
    assert!(!reply.text.contains("`$count`"), "{}", reply.text);
    let body = sent(&mock);
    let query = body["query"].as_str().expect("a query string");
    assert!(
        query.starts_with("query ViaFragment($id: ID!, $count: Int) {"),
        "{query}"
    );
    assert!(query.contains("orders(first: $count) {"), "{query}");
    assert_eq!(body["variables"], json!({ "id": "u-1", "count": 3 }));
}

#[test]
fn a_variable_the_operation_never_declared_is_not_excused() {
    let mock = mock();
    let mut mcp = launch(&mock).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": "query Mine($id: ID!) { user(id: $id) { name } }",
            "variables": { "id": "u-1", "precision": 2 },
        }),
    );

    assert!(!reply.text.contains("is not sent"), "{}", reply.text);
    if !reply.is_error {
        assert_eq!(sent(&mock)["variables"], json!({ "id": "u-1", "precision": 2 }));
    }
}
