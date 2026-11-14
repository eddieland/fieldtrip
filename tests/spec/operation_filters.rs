//! Slice 37: a library can hold hundreds of operations, so `list_operations` narrows it. `uses` keeps operations
//! that select a type or a chain of fields, written as in a cost map, `kind` keeps queries or mutations, and `name`
//! shows one in full. Past five matches each operation gets one line, and `search_schema` lists at most ten.

use serde_json::{Value, json};

use crate::support::{Launch, Mcp, MockServer};

/// Saved operations that touch different parts of the fixture schema, by file name.
const OPERATIONS: [(&str, &str); 6] = [
    (
        "user.graphql",
        "# A user's name and email.\nquery UserById($id: ID!) { user(id: $id) { name email } }",
    ),
    (
        "user_orders.graphql",
        "# A user's recent orders.\nquery UserOrders($id: ID!) { user(id: $id) { name orders(first: 5) { id status } } }",
    ),
    (
        "cancel.graphql",
        "# Cancel an order.\nmutation CancelOrder($id: ID!) { cancelOrder(id: $id) { id status } }",
    ),
    (
        "node_order.graphql",
        "# An order fetched as a node.\nquery NodeOrder($id: ID!) { node(id: $id) { ...OrderParts } }\n\
         fragment OrderParts on Order { id total { amountCents } }",
    ),
    (
        "products.graphql",
        "# Every product title.\nquery Products { products { title } }",
    ),
    (
        "stale.graphql",
        "# An order by ID, from before the API changed.\nquery StaleOrder($id: ID!) { order(id: $id) { id } }",
    ),
];

/// A server for the `store` endpoint with [`OPERATIONS`] saved, plus `extra` files.
fn start(extra: &[(String, String)]) -> Mcp {
    let mock = MockServer::unreachable();
    let mut launch = Launch::new(&format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\noperations = \"operations\"\n",
        mock.url()
    ));
    for (file, text) in OPERATIONS {
        launch = launch.file(&format!("operations/{file}"), text);
    }
    for (file, text) in extra {
        launch = launch.file(&format!("operations/{file}"), text);
    }
    launch.start()
}

/// Calls `list_operations` on `store` with `filters` added to the arguments.
fn list(mcp: &mut Mcp, filters: &Value) -> String {
    let mut arguments = json!({ "endpoint": "store" });
    if let (Some(arguments), Some(filters)) = (arguments.as_object_mut(), filters.as_object()) {
        arguments.extend(filters.clone());
    }
    let reply = mcp.call("list_operations", &arguments);
    assert!(!reply.is_error, "{}", reply.text);
    reply.text
}

/// The names of [`OPERATIONS`] that `text` lists, in order.
fn listed(text: &str) -> Vec<&'static str> {
    let names = [
        "CancelOrder",
        "NodeOrder",
        "Products",
        "StaleOrder",
        "UserById",
        "UserOrders",
    ];
    names
        .into_iter()
        .filter(|name| text.contains(&format!("{name} (")))
        .collect()
}

#[test]
fn uses_a_type_keeps_operations_that_select_it_through_fields_and_fragments() {
    let mut mcp = start(&[]);

    let text = list(&mut mcp, &json!({ "uses": ["Order"] }));

    assert_eq!(
        listed(&text),
        ["CancelOrder", "NodeOrder", "UserOrders"],
        "a field returning `Order`, a field on `Order`, or a fragment on it: {text}"
    );
    assert!(
        text.starts_with("3 of 6 saved operations for `store` match using `Order`."),
        "{text}"
    );
}

#[test]
fn uses_a_chain_of_fields_is_written_as_in_a_cost_map() {
    let mut mcp = start(&[]);

    assert_eq!(
        listed(&list(&mut mcp, &json!({ "uses": ["User.orders"] }))),
        ["UserOrders"]
    );
    assert_eq!(
        listed(&list(&mut mcp, &json!({ "uses": ["Query.node"] }))),
        ["NodeOrder"]
    );
    assert_eq!(
        listed(&list(&mut mcp, &json!({ "uses": ["Query.user.orders.status"] }))),
        ["UserOrders"],
        "an exact path from the root"
    );
    assert_eq!(
        listed(&list(&mut mcp, &json!({ "uses": ["*.status"] }))),
        ["CancelOrder", "UserOrders"],
        "`*` stands for any one name"
    );
    assert_eq!(
        listed(&list(&mut mcp, &json!({ "uses": ["Order", "Money"] }))),
        ["NodeOrder"],
        "every pattern must match"
    );
}

#[test]
fn kind_and_query_combine_with_uses() {
    let mut mcp = start(&[]);

    let text = list(&mut mcp, &json!({ "uses": ["Order"], "kind": "mutation" }));
    assert_eq!(listed(&text), ["CancelOrder"], "{text}");
    assert!(text.contains("using `Order`, mutations only"), "{text}");

    let text = list(&mut mcp, &json!({ "uses": ["Order"], "query": "node" }));
    assert_eq!(listed(&text), ["NodeOrder"], "{text}");
}

#[test]
fn a_broken_operation_never_matches_uses() {
    let mut mcp = start(&[]);

    let text = list(&mut mcp, &json!({ "uses": ["Order"], "query": "StaleOrder" }));

    assert!(text.starts_with("No saved operation"), "{text}");
    assert!(text.contains("broken never matches `uses`"), "{text}");
}

#[test]
fn a_pattern_with_an_unknown_name_suggests_the_nearest() {
    let mut mcp = start(&[]);

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "uses": ["Ordr"] }));
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Nearest types: `Order`"), "{}", reply.text);

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "uses": ["User.ordrs"] }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Nearest fields: `orders`"), "{}", reply.text);

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "uses": ["User..orders"] }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("is not a pattern"), "{}", reply.text);

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "kind": "subscription" }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("`query` or `mutation`"), "{}", reply.text);
}

#[test]
fn past_five_matches_each_operation_gets_one_line() {
    let mut mcp = start(&[]);

    let text = list(&mut mcp, &json!({}));

    assert!(text.contains("each is on one line"), "{text}");
    assert!(
        text.contains("\nUserById (query, cost 1, $id: ID!)  \"A user's name and email.\"\n"),
        "name, kind, cost, variables and description: {text}"
    );
    assert!(
        text.contains("\nStaleOrder (query, BROKEN: no longer valid, $id: ID!)"),
        "{text}"
    );
    assert!(!text.contains("email\n"), "no operation text: {text}");
}

#[test]
fn name_shows_one_operation_in_full() {
    let mut mcp = start(&[]);

    let text = list(&mut mcp, &json!({ "name": "NodeOrder" }));
    assert!(text.starts_with("Saved operation `NodeOrder` for `store`"), "{text}");
    assert!(
        text.contains("fragment OrderParts on Order {"),
        "the text, fragments included: {text}"
    );

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "NodeOrdr" }));
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Nearest names: `NodeOrder`"), "{}", reply.text);

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "name": "NodeOrder", "kind": "query" }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Give `name` alone"), "{}", reply.text);
}

#[test]
fn search_schema_lists_at_most_ten_saved_operations() {
    let extra: Vec<(String, String)> = (0..12)
        .map(|index| {
            (
                format!("viewer_{index}.graphql"),
                format!("# Viewer report {index}.\nquery ViewerReport{index} {{ viewer {{ name }} }}"),
            )
        })
        .collect();
    let mut mcp = start(&extra);

    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "viewer" }));

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(reply.text.matches("ViewerReport").count(), 10, "{}", reply.text);
    assert!(reply.text.contains("And 2 more."), "{}", reply.text);
    assert!(
        reply.text.contains("Query.viewer"),
        "the schema's matches follow: {}",
        reply.text
    );
}

#[test]
fn a_large_library_that_matches_nothing_does_not_list_every_name() {
    let extra: Vec<(String, String)> = (0..20)
        .map(|index| {
            (
                format!("viewer_{index}.graphql"),
                format!("query ViewerReport{index} {{ viewer {{ name }} }}"),
            )
        })
        .collect();
    let mut mcp = start(&extra);

    let text = list(&mut mcp, &json!({ "query": "refund" }));

    assert!(text.contains("It has 26: loosen a filter."), "{text}");
    assert!(!text.contains("ViewerReport"), "{text}");
}
