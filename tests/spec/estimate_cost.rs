//! Slice 6: `estimate_cost` validates an operation against the schema and costs it, without sending it.

use serde_json::{Value, json};

use crate::support::{Mcp, MockServer, Reply};

/// Starts a server whose `store` endpoint reads the fixture schema, with `extra` lines added to the endpoint.
fn start(mock: &MockServer, extra: &str) -> Mcp {
    Mcp::start(&format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{extra}",
        mock.url()
    ))
}

/// Estimates `operation` on the fixture endpoint.
fn estimate_with(operation: &str, variables: &Value) -> Reply {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock, "");
    mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation": operation, "variables": variables }),
    )
}

/// Estimates `operation` and returns the text, failing on a tool error.
fn estimate(operation: &str) -> String {
    let reply = estimate_with(operation, &json!({}));
    assert!(
        !reply.is_error,
        "the estimate should succeed for {operation}: {}",
        reply.text
    );
    reply.text
}

/// Estimates `operation` and returns the error text, failing if the tool succeeded.
fn rejection(operation: &str) -> String {
    let reply = estimate_with(operation, &json!({}));
    assert!(
        reply.is_error,
        "the estimate should fail for {operation}: {}",
        reply.text
    );
    reply.text
}

/// Asserts that `operation` costs `cost`.
fn assert_cost(operation: &str, cost: u64) {
    let text = estimate(operation);
    assert!(
        text.contains(&format!("Estimated cost: {cost}\n")),
        "expected cost {cost} for {operation}, got:\n{text}"
    );
}

#[test]
fn reports_cost_depth_breakdown_and_assumptions() {
    let text = estimate(
        "query Recent { viewer { name orders(first: 3) { items { quantity product { title } } } } products { title } }",
    );

    let expected = "\
Valid query `Recent` for `store`.
Estimated cost: 74
Depth: 5
By top-level field:
  viewer: 64
  products: 10
Assumptions:
  - `Order.items` takes no size argument, so 10 items were assumed.
  - `Query.products` takes no size argument, so 10 items were assumed.
";
    assert_eq!(
        text, expected,
        "viewer is 1 + 3 x (1 + 10 x (1 + 1)), and products is 10 x 1"
    );
}

#[test]
fn scalars_are_free_and_an_object_costs_one() {
    assert_cost("{ viewer { name email role } }", 1);
    assert_cost("{ viewer { name } product(id: \"1\") { title priceCents } }", 2);
    assert_cost("{ product(id: \"1\") { tags } }", 1);
    assert_cost("{ viewer { __typename } }", 1);
}

#[test]
fn a_list_multiplies_by_its_size_argument() {
    assert_cost("{ search(text: \"x\", limit: 5) { __typename } }", 5);
    assert_cost("{ viewer { orders(first: 4) { id } } }", 5);
    assert_cost("{ viewer { orders(last: 4) { id customer { name } } } }", 9);
}

#[test]
fn a_size_argument_on_a_connection_bounds_the_list_inside_it() {
    let text = estimate("{ users(first: 5) { totalCount nodes { name } } }");

    assert!(
        text.contains("Estimated cost: 6\n"),
        "the connection is 1 and its five nodes are 1 each:\n{text}"
    );
    assert!(
        !text.contains("Assumptions"),
        "`first` on the connection sizes `nodes`, so nothing is assumed:\n{text}"
    );
}

#[test]
fn a_default_in_the_schema_is_used_when_the_argument_is_left_out() {
    let text = estimate("{ users { nodes { id } } }");

    assert!(
        text.contains("Estimated cost: 21\n"),
        "`first` defaults to 20 in the schema:\n{text}"
    );
    assert!(
        !text.contains("Assumptions"),
        "a schema default is a fact, not an assumption:\n{text}"
    );
}

#[test]
fn a_size_given_through_a_variable_is_resolved() {
    let operation = "query($n: Int!) { users(first: $n) { nodes { id } } }";

    let given = estimate_with(operation, &json!({ "n": 3 }));
    assert!(
        given.text.contains("Estimated cost: 4\n"),
        "the variable's value is used: {}",
        given.text
    );

    let defaulted = estimate("query($n: Int = 2) { users(first: $n) { nodes { id } } }");
    assert!(
        defaulted.contains("Estimated cost: 3\n"),
        "the variable's default is used:\n{defaulted}"
    );

    let missing = estimate(operation);
    assert!(
        missing.contains("Estimated cost: 11\n"),
        "with no value, the assumed size is used:\n{missing}"
    );
    assert!(
        missing.contains("`UserConnection.nodes` is sized by `$n`, which has no value, so 10 items were assumed."),
        "the assumption names the variable:\n{missing}"
    );
}

#[test]
fn an_unbounded_list_uses_the_assumed_size_from_the_config() {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock, "assumed_list_size = 3\n");

    let reply = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation": "{ products { title } }" }),
    );

    assert!(
        reply.text.contains("Estimated cost: 3\n"),
        "three products are assumed: {}",
        reply.text
    );
    assert!(
        reply.text.contains("so 3 items were assumed"),
        "the assumption gives the size: {}",
        reply.text
    );
}

#[test]
fn a_list_called_without_its_size_argument_says_so() {
    let text = estimate("{ viewer { orders { id } } }");

    assert!(
        text.contains("`User.orders` was given no size argument, so 10 items were assumed."),
        "the wording tells the caller a size argument exists:\n{text}"
    );
}

#[test]
fn each_top_level_mutation_field_costs_ten() {
    assert_cost("mutation { deleteUser(id: \"1\") }", 10);
    assert_cost("mutation { cancelOrder(id: \"1\") { id status } }", 10);
    assert_cost("mutation { cancelOrder(id: \"1\") { id customer { name } } }", 11);
    assert_cost("mutation { a: deleteUser(id: \"1\") b: deleteUser(id: \"2\") }", 20);

    let text = estimate("mutation Cancel { cancelOrder(id: \"1\") { id } }");
    assert!(
        text.starts_with("Valid mutation `Cancel` for `store`.\n"),
        "the kind is reported:\n{text}"
    );
}

#[test]
fn fragments_are_followed() {
    assert_cost(
        "query { viewer { ...U } } fragment U on User { name orders(first: 2) { id } }",
        3,
    );
    assert_cost(
        "{ search(text: \"a\", limit: 2) { ... on User { name } ... on Product { reviews(limit: 3) { rating } } } }",
        8,
    );
    assert_cost("{ ...Q } fragment Q on Query { viewer { name } }", 1);
}

#[test]
fn the_breakdown_uses_aliases() {
    let text = estimate("{ me: viewer { name } all: products { title } }");

    assert!(
        text.contains("  me: 1\n  all: 10\n"),
        "each top-level field is listed by its response key:\n{text}"
    );
}

#[test]
fn an_invalid_operation_returns_the_errors_with_positions_and_near_names() {
    let error = rejection("{\n  viewer { nme }\n}");

    assert!(
        error.starts_with("The operation is not valid for `store`:\n"),
        "says the operation is invalid: {error}"
    );
    assert!(error.contains("- 2:12 "), "gives the line and column: {error}");
    assert!(error.contains("`nme`"), "names the unknown field: {error}");
    assert!(
        error.contains("Nearest fields on `User`: `name`"),
        "suggests the nearest field: {error}"
    );
}

#[test]
fn a_syntax_error_is_reported() {
    let error = rejection("{ viewer {");

    assert!(
        error.starts_with("The operation is not valid for `store`:\n"),
        "says the operation is invalid: {error}"
    );
}

#[test]
fn subscriptions_are_rejected() {
    let error = rejection("subscription { orderUpdated(id: \"1\") { id } }");

    assert!(error.contains("Subscriptions are not supported"), "says why: {error}");
}

#[test]
fn more_than_one_operation_is_rejected() {
    let error = rejection("query A { viewer { name } } query B { products { title } }");

    assert!(error.contains("2 operations (A, B)"), "names the operations: {error}");
    assert!(error.contains("exactly one operation"), "says what to do: {error}");
}

#[test]
fn nothing_is_sent_to_the_endpoint() {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock, "");

    for operation in ["{ viewer { name } }", "mutation { deleteUser(id: \"1\") }", "{ nope }"] {
        mcp.call("estimate_cost", &json!({ "endpoint": "store", "operation": operation }));
    }

    assert_eq!(mock.requests().len(), 0, "estimating never contacts the endpoint");
}
