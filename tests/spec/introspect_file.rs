//! Slice 3: `introspect` returns an overview of a schema read from a file, without touching the network.

use serde_json::json;

use crate::support::{Mcp, MockServer};

/// A config whose only endpoint reads the fixture schema from a file and points at `mock`.
fn config(mock: &MockServer) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    )
}

#[test]
fn returns_the_overview_of_the_fixture() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&config(&mock));
    let schema_path = mcp.dir().join("schema.graphql");

    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));

    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    let expected = format!(
        "\
Schema for `store`, read from the file {}.
\"A small store API. This is the fixture schema for fieldtrip's tests.\"

20 types: 12 objects, 1 interface, 1 union, 3 enums, 2 input objects, 1 scalar

Query (8 fields): viewer, user, users, product, products, search, catalog, node
Mutation (4 fields): createOrder, cancelOrder, deleteUser (deprecated), deactivateUser
Subscription (1 field): orderUpdated
Subscriptions can be described but not executed.
",
        schema_path.display()
    );
    assert_eq!(
        reply.text, expected,
        "the overview names the source, the counts and the root fields"
    );
}

#[test]
fn makes_no_http_request() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&config(&mock));

    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));

    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    assert_eq!(
        mock.requests().len(),
        0,
        "a schema file means the endpoint is never asked for its schema"
    );
}

#[test]
fn keeps_the_loaded_schema_until_asked_to_refresh() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&config(&mock));
    let schema_path = mcp.dir().join("schema.graphql");
    let first = mcp.call("introspect", &json!({ "endpoint": "store" }));
    assert!(
        first.text.contains("Query (8 fields)"),
        "the fixture has eight queries: {}",
        first.text
    );

    let original = std::fs::read_to_string(&schema_path).expect("read the fixture copy");
    let changed = original.replace("  node(id: ID!): Node\n}", "  node(id: ID!): Node\n  ping: String\n}");
    assert_ne!(original, changed, "the edit should have changed the schema text");
    std::fs::write(&schema_path, changed).expect("rewrite the fixture copy");

    let cached = mcp.call("introspect", &json!({ "endpoint": "store" }));
    assert!(
        cached.text.contains("Query (8 fields)"),
        "without refresh the loaded schema is kept: {}",
        cached.text
    );

    let refreshed = mcp.call("introspect", &json!({ "endpoint": "store", "refresh": true }));
    assert!(
        refreshed.text.contains("Query (9 fields)"),
        "refresh reads the file again: {}",
        refreshed.text
    );
    assert!(
        refreshed.text.contains("ping"),
        "the new field is listed: {}",
        refreshed.text
    );
}

#[test]
fn an_invalid_schema_file_is_an_error_that_names_the_file() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&config(&mock));
    let schema_path = mcp.dir().join("schema.graphql");
    std::fs::write(&schema_path, "type Query { broken: Missing }").expect("rewrite the fixture copy");

    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));

    assert!(reply.is_error, "an invalid schema should be an error: {}", reply.text);
    assert!(
        reply.text.contains("schema.graphql"),
        "the error names the file: {}",
        reply.text
    );
    assert!(
        reply.text.contains("Missing"),
        "the error passes on the diagnostic: {}",
        reply.text
    );
}
