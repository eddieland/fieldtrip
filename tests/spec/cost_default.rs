//! Slice 48: a rule that sizes a list by an argument can say what the size is when the argument is left out, for
//! APIs that apply a page size on the server without declaring it as a default in the schema.
//!
//! The fixture's `User.orders(first: Int, last: Int)` declares no default, `Product.related(take: Int = 4)` declares
//! one, and `Query.catalog(pageSize: Int)` returns a page with the list inside it.

use serde_json::{Value, json};

use crate::support::{Launch, MockServer};

const ORDERS: &str = "\"User.orders\" = { multiplier = \"first\", default = 100 }\n";

const CONFIG: &str = "[endpoints.store]\nurl = \"URL\"\nschema = \"schema.graphql\"\ncosts = \"store.costs.toml\"\n";

/// Estimates `operation` under `map` and returns the text, failing on a tool error.
fn estimate(map: &str, operation: &str, variables: &Value) -> String {
    let mock = MockServer::unreachable();
    let mut mcp = Launch::new(&CONFIG.replace("URL", &mock.url()))
        .file("store.costs.toml", map)
        .start();
    let arguments = json!({ "endpoint": "store", "operation": operation, "variables": variables });
    let reply = mcp.call("estimate_cost", &arguments);
    assert!(
        !reply.is_error,
        "the estimate should succeed for {operation}: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "estimating sends nothing");
    reply.text
}

/// Asserts that `operation` costs `cost` under `map`, and returns the estimate.
fn assert_cost(map: &str, operation: &str, variables: &Value, cost: u64) -> String {
    let text = estimate(map, operation, variables);
    assert!(
        text.contains(&format!("Estimated cost: {cost}\n")),
        "expected cost {cost} for {operation} under the map\n{map}\ngot:\n{text}"
    );
    text
}

/// Starts the server with `map` as the cost map, expecting it to refuse, and returns what it wrote to stderr.
fn refused(map: &str) -> String {
    Launch::new(&CONFIG.replace("URL", "http://localhost:9/graphql"))
        .file("store.costs.toml", map)
        .fails_to_start()
}

#[test]
fn the_default_sizes_the_list_when_the_argument_is_left_out() {
    let text = estimate(ORDERS, "{ viewer { orders { id } } }", &json!({}));

    let expected = "\
Valid query for `store`.
Estimated cost: 101
Depth: 3
By top-level field:
  viewer: 101
From the cost map:
  - `User.orders`: multiplier 100, the default when `first` is left out (rule `User.orders`)
";
    assert_eq!(text, expected, "a hundred orders, and nothing assumed");
}

#[test]
fn a_size_the_operation_gives_wins_over_the_default() {
    let text = assert_cost(ORDERS, "{ viewer { orders(first: 5) { id } } }", &json!({}), 6);

    assert!(
        text.contains("  - `User.orders`: multiplier from `first` (rule `User.orders`)\n"),
        "the estimate says the argument was used: {text}"
    );
    assert!(!text.contains("the default"), "and not the default: {text}");
}

#[test]
fn an_explicit_null_takes_the_default() {
    assert_cost(ORDERS, "{ viewer { orders(first: null) { id } } }", &json!({}), 101);
}

#[test]
fn a_variable_is_followed_and_one_with_no_value_takes_the_default() {
    let operation = "query($n: Int) { viewer { orders(first: $n) { id } } }";

    assert_cost(ORDERS, operation, &json!({ "n": 2 }), 3);

    let text = assert_cost(ORDERS, operation, &json!({}), 101);
    assert!(
        !text.contains("Assumptions:"),
        "the server applies its page size to a variable left out, so nothing is assumed: {text}"
    );

    assert_cost(ORDERS, operation, &json!({ "n": null }), 101);
    assert_cost(ORDERS, &operation.replace("$n: Int", "$n: Int = 7"), &json!({}), 8);
}

#[test]
fn a_default_the_schema_declares_wins_over_the_rule() {
    let map = "\"Product.related\" = { multiplier = \"take\", default = 50 }\n";

    let text = assert_cost(map, "{ product(id: \"1\") { related { title } } }", &json!({}), 5);

    assert!(
        text.contains("multiplier from `take`"),
        "the schema says what the server does with no `take`: {text}"
    );
}

#[test]
fn on_a_field_that_is_not_a_list_the_default_bounds_the_list_inside() {
    let map = "\"Query.catalog\" = { multiplier = \"pageSize\", default = 100 }\n";

    let text = assert_cost(map, "{ catalog { total items { title } } }", &json!({}), 101);
    assert!(
        text.contains("`Query.catalog`: multiplier 100, the default when `pageSize` is left out"),
        "{text}"
    );

    assert_cost(map, "{ catalog(pageSize: 3) { items { title } } }", &json!({}), 4);
}

#[test]
fn one_rule_gives_a_default_to_every_field_that_takes_the_argument() {
    let map = "\"*.*\" = { multiplier = \"first\", default = 100 }\n\
               \"Query.users\" = { multiplier = \"first\", default = 1000 }\n";

    // `orders` takes `first` and is given none. `products` has no such argument, so the rule leaves it alone.
    let text = assert_cost(map, "{ viewer { orders { id } } products { title } }", &json!({}), 111);
    assert!(
        text.contains("`Query.products` takes no size argument, so 10 items were assumed."),
        "a field without the argument keeps the built-in guess: {text}"
    );
}

#[test]
fn a_default_without_an_argument_to_stand_in_for_is_refused() {
    for rule in ["{ multiplier = 5, default = 1 }", "{ cost = 1, default = 1 }"] {
        let stderr = refused(&format!("\"Query.products\" = {rule}\n"));

        assert!(
            stderr.contains("store.costs.toml") && stderr.contains("`Query.products`"),
            "names the file and the rule for `{rule}`: {stderr}"
        );
        assert!(
            stderr.contains("`default`") && stderr.contains("names an argument"),
            "says when `default` applies, for `{rule}`: {stderr}"
        );
    }
}

#[test]
fn a_default_of_the_wrong_kind_is_refused() {
    for value in ["-1", "1.5", "\"ten\"", "true"] {
        let stderr = refused(&format!(
            "\"User.orders\" = {{ multiplier = \"first\", default = {value} }}\n"
        ));

        assert!(
            stderr.contains("`User.orders`"),
            "names the rule for `{value}`: {stderr}"
        );
        assert!(
            stderr.contains("`default`") && stderr.contains("whole number"),
            "says what a default is, for `{value}`: {stderr}"
        );
    }
}
