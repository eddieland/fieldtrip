//! Slice 17: an endpoint's cost map replaces the built-in cost or multiplier of the fields its rules name.
//!
//! The rules here are paths with no wildcards: `Query.viewer.orders` is a path from the root, and `User.orders`
//! is the field `orders` wherever it is selected on a `User`.

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer};

/// Starts a server whose `store` endpoint reads the fixture schema and uses `map` as its cost map.
fn start(map: &str) -> Mcp {
    start_with(&MockServer::unreachable(), map, "", Human::Absent)
}

/// Starts a server on `mock` with `map` as the cost map, `settings` on the endpoint, and a human who answers.
fn start_with(mock: &MockServer, map: &str, settings: &str, human: Human) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\ncosts = \"store.costs.toml\"\n{settings}",
        mock.url()
    );
    Launch::new(&config).file("store.costs.toml", map).human(human).start()
}

/// Estimates `operation` under `map` and returns the text, failing on a tool error.
fn estimate(map: &str, operation: &str) -> String {
    let mut mcp = start(map);
    let reply = mcp.call("estimate_cost", &json!({ "endpoint": "store", "operation": operation }));
    assert!(
        !reply.is_error,
        "the estimate should succeed for {operation}: {}",
        reply.text
    );
    reply.text
}

/// Asserts that `operation` costs `cost` under `map`.
fn assert_cost(map: &str, operation: &str, cost: u64) {
    let text = estimate(map, operation);
    assert!(
        text.contains(&format!("Estimated cost: {cost}\n")),
        "expected cost {cost} for {operation} under the map\n{map}\ngot:\n{text}"
    );
}

#[test]
fn the_estimate_uses_a_rule_and_lists_it() {
    let text = estimate(
        "\"Query.products\" = { multiplier = 500 }\n",
        "{ products { title } viewer { name } }",
    );

    let expected = "\
Valid query for `store`.
Estimated cost: 501
Depth: 2
By top-level field:
  products: 500
  viewer: 1
From the cost map:
  - `Query.products`: multiplier 500 (rule `Query.products`)
";
    assert_eq!(
        text, expected,
        "the rule replaces the assumed size, so there is no assumption left to report"
    );
}

#[test]
fn cost_replaces_what_the_field_itself_costs() {
    // A scalar is free by default.
    assert_cost("\"User.email\" = { cost = 3 }\n", "{ viewer { name email } }", 4);
    // An object costs 1 by default.
    assert_cost("\"Query.viewer\" = { cost = 7 }\n", "{ viewer { name } }", 7);
    // In a list, the cost is per item.
    assert_cost(
        "\"Query.search\" = { cost = 25 }\n",
        "{ search(text: \"x\", limit: 2) { __typename } }",
        50,
    );
    // A cost of zero makes an object free.
    assert_cost("\"Query.viewer\" = { cost = 0 }\n", "{ viewer { name } }", 0);
}

#[test]
fn multiplier_replaces_the_size_of_a_list() {
    // In place of the assumed size.
    assert_cost(
        "\"Query.products\" = { multiplier = 500 }\n",
        "{ products { title } }",
        500,
    );
    // In place of a size argument: the owner of the map says users have about three orders, whatever is asked for.
    assert_cost(
        "\"User.orders\" = { multiplier = 3 }\n",
        "{ viewer { orders(first: 100) { id } } }",
        4,
    );
    // A multiplier of zero makes the list free.
    assert_cost(
        "\"User.orders\" = { multiplier = 0 }\n",
        "{ viewer { orders { id } } }",
        1,
    );
}

#[test]
fn cost_and_multiplier_combine() {
    assert_cost(
        "\"Query.products\" = { cost = 2, multiplier = 50 }\n",
        "{ products { title } }",
        100,
    );
    assert_cost(
        "\"Product.tags\" = { cost = 1, multiplier = 4 }\n",
        "{ product(id: \"1\") { tags } }",
        5,
    );
}

#[test]
fn a_multiplier_on_a_connection_bounds_the_list_inside_it() {
    assert_cost(
        "\"Query.users\" = { multiplier = 7 }\n",
        "{ users(first: 100) { nodes { id } } }",
        8,
    );
}

#[test]
fn a_path_from_the_root_matches_only_there() {
    let map = "\"Query.viewer.orders\" = { multiplier = 2 }\n";

    assert_cost(map, "{ viewer { orders { id } } }", 3);
    assert_cost(map, "{ user(id: \"1\") { orders { id } } }", 11);
}

#[test]
fn a_path_from_a_type_matches_wherever_that_type_is_selected() {
    let map = "\"User.orders\" = { multiplier = 2 }\n";

    assert_cost(map, "{ viewer { orders { id } } }", 3);
    assert_cost(map, "{ user(id: \"1\") { orders { id } } }", 3);
    assert_cost(
        map,
        "{ product(id: \"1\") { reviews(limit: 1) { author { orders { id } } } } }",
        5,
    );
}

#[test]
fn a_longer_path_wins_over_a_shorter_one() {
    let map = "\"User.orders\" = { multiplier = 5 }\n\"Query.viewer.orders\" = { multiplier = 2 }\n";

    assert_cost(map, "{ viewer { orders { id } } }", 3);
    assert_cost(map, "{ user(id: \"1\") { orders { id } } }", 6);
}

#[test]
fn a_rule_on_a_mutation_replaces_the_ten() {
    assert_cost(
        "\"Mutation.deleteUser\" = { cost = 100 }\n",
        "mutation { deleteUser(id: \"1\") }",
        100,
    );
    assert_cost(
        "\"Mutation.cancelOrder\" = { cost = 40 }\n",
        "mutation { cancelOrder(id: \"1\") { id customer { name } } }",
        41,
    );
}

#[test]
fn fragments_and_aliases_do_not_change_what_a_rule_matches() {
    let map = "\"User.orders\" = { multiplier = 2 }\n\"Query.viewer\" = { cost = 5 }\n";

    assert_cost(map, "{ me: viewer { mine: orders { id } } }", 7);
    assert_cost(
        map,
        "{ viewer { ...Orders } } fragment Orders on User { orders { id } }",
        7,
    );
    assert_cost(
        map,
        "{ search(text: \"a\", limit: 1) { ... on User { orders { id } } } }",
        3,
    );
}

#[test]
fn a_field_no_rule_names_keeps_the_built_in_cost() {
    let text = estimate(
        "\"Query.products\" = { multiplier = 500 }\n",
        "{ viewer { orders { id } } }",
    );

    assert!(
        text.contains("Estimated cost: 11\n"),
        "the built-in rules still apply:\n{text}"
    );
    assert!(
        text.contains("`User.orders` was given no size argument"),
        "and so do the assumptions:\n{text}"
    );
    assert!(
        !text.contains("From the cost map"),
        "no rule was used, so none is listed:\n{text}"
    );
}

#[test]
fn a_query_the_map_makes_cheap_can_pass_the_approval_threshold() {
    let mock = MockServer::answering(&json!({ "data": { "products": [] } }));
    let map = "\"Query.products\" = { multiplier = 2 }\n";
    let mut mcp = start_with(&mock, map, "auto_approve_cost = 5\n", Human::Declines);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { title } }" }),
    );

    assert!(
        !reply.is_error,
        "the query costs 2 under the map, which is within the threshold: {}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 0, "so nobody is asked");
    assert_eq!(mock.requests().len(), 1, "and it is sent");
}

#[test]
fn list_endpoints_shows_the_map() {
    let mut mcp = start("\"Query.products\" = { multiplier = 500 }\n\"User.orders\" = { multiplier = 3 }\n");
    let map_path = mcp.dir().join("store.costs.toml");

    let text = mcp.call("list_endpoints", &json!({})).text;

    assert!(
        text.contains(&format!("  cost map: {} (2 rules)\n", map_path.display())),
        "the endpoint shows where its cost map is and how many rules it has:\n{text}"
    );
}

#[test]
fn a_map_file_that_is_missing_is_refused_at_startup() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\ncosts = \"missing.costs.toml\"\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("missing.costs.toml"), "names the file: {stderr}");
}
