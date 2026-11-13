//! Slice 18: `*` in a cost map pattern stands for any one name, and the most specific rule wins.

use serde_json::json;

use crate::support::{Launch, MockServer};

/// Estimates `operation` under `map` and returns the text, failing on a tool error.
fn estimate(map: &str, operation: &str) -> String {
    let mock = MockServer::unreachable();
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\ncosts = \"store.costs.toml\"\n",
        mock.url()
    );
    let mut mcp = Launch::new(&config).file("store.costs.toml", map).start();
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

const VIEWER_ORDERS: &str = "{ viewer { orders { id } } }";
const USER_ORDERS: &str = "{ user(id: \"1\") { orders { id } } }";
const AUTHOR_ORDERS: &str = "{ product(id: \"1\") { reviews(limit: 1) { author { orders { id } } } } }";

#[test]
fn a_star_stands_for_any_type() {
    let map = "\"*.orders\" = { multiplier = 2 }\n";

    assert_cost(map, VIEWER_ORDERS, 3);
    assert_cost(map, AUTHOR_ORDERS, 5);
}

#[test]
fn a_star_stands_for_any_field() {
    let map = "\"Query.*\" = { cost = 5 }\n";

    assert_cost(map, "{ viewer { name } product(id: \"1\") { title } }", 10);
    // `orders` is not a field of `Query`, so it keeps the built-in cost.
    assert_cost(map, "{ viewer { orders(first: 2) { id } } }", 7);
}

#[test]
fn a_star_stands_for_exactly_one_name() {
    let map = "\"Query.*.orders\" = { multiplier = 2 }\n";

    assert_cost(map, VIEWER_ORDERS, 3);
    assert_cost(map, USER_ORDERS, 3);
    // Three fields lie between the root and `orders` here, so the pattern does not reach it.
    assert_cost(map, AUTHOR_ORDERS, 13);
}

#[test]
fn stars_alone_match_every_field() {
    assert_cost(
        "\"*.*\" = { cost = 0 }\n",
        "{ viewer { name orders(first: 3) { id customer { name } } } }",
        0,
    );
}

#[test]
fn a_name_wins_over_a_star() {
    let map = "\"*.orders\" = { multiplier = 9 }\n\"User.orders\" = { multiplier = 5 }\n";

    assert_cost(map, VIEWER_ORDERS, 6);
}

#[test]
fn a_longer_pattern_wins_even_with_a_star_in_it() {
    let map = "\"User.orders\" = { multiplier = 5 }\n\"Query.*.orders\" = { multiplier = 4 }\n";

    assert_cost(map, VIEWER_ORDERS, 5);
    assert_cost(map, AUTHOR_ORDERS, 8);
}

#[test]
fn a_name_nearer_the_field_wins_a_tie() {
    // Both patterns have two parts and one star. The one that names the field itself is the more specific.
    let map = "\"*.orders\" = { multiplier = 3 }\n\"User.*\" = { multiplier = 7 }\n";

    assert_cost(map, VIEWER_ORDERS, 4);
}

#[test]
fn the_most_specific_of_three_wins() {
    let map = "\"*.orders\" = { multiplier = 9 }\n\
               \"User.orders\" = { multiplier = 5 }\n\
               \"Query.viewer.orders\" = { multiplier = 2 }\n";

    assert_cost(map, VIEWER_ORDERS, 3);
    assert_cost(map, USER_ORDERS, 6);
}

#[test]
fn cost_and_multiplier_are_each_taken_from_the_most_specific_rule_that_sets_them() {
    let map = "\"*.orders\" = { multiplier = 2 }\n\"User.orders\" = { cost = 4 }\n";

    let text = estimate(map, VIEWER_ORDERS);

    let expected = "\
Valid query for `store`.
Estimated cost: 9
Depth: 3
By top-level field:
  viewer: 9
From the cost map:
  - `User.orders`: cost 4 (rule `User.orders`)
  - `User.orders`: multiplier 2 (rule `*.orders`)
";
    assert_eq!(
        text, expected,
        "the cost comes from one rule and the multiplier from another: 1 + 2 x 4"
    );
}
