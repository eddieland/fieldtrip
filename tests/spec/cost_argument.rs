//! Slice 19: a rule can name the argument that carries a list's size, for APIs that do not call it `first`,
//! `last` or `limit`.
//!
//! The fixture has two such fields: `Product.related(take: Int = 4)`, a list, and `Query.catalog(pageSize: Int)`,
//! which returns a page with the list inside it.

use serde_json::{Value, json};

use crate::support::{Launch, MockServer};

const RELATED_BY_TAKE: &str = "\"Product.related\" = { multiplier = \"take\" }\n";

/// Estimates `operation` under `map` and returns the text, failing on a tool error.
fn estimate(map: &str, operation: &str, variables: &Value) -> String {
    let mock = MockServer::unreachable();
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\ncosts = \"store.costs.toml\"\n",
        mock.url()
    );
    let mut mcp = Launch::new(&config).file("store.costs.toml", map).start();
    let arguments = json!({ "endpoint": "store", "operation": operation, "variables": variables });
    let reply = mcp.call("estimate_cost", &arguments);
    assert!(
        !reply.is_error,
        "the estimate should succeed for {operation}: {}",
        reply.text
    );
    reply.text
}

/// Asserts that `operation` costs `cost` under `map`.
fn assert_cost(map: &str, operation: &str, cost: u64) {
    let text = estimate(map, operation, &json!({}));
    assert!(
        text.contains(&format!("Estimated cost: {cost}\n")),
        "expected cost {cost} for {operation} under the map\n{map}\ngot:\n{text}"
    );
}

#[test]
fn the_named_argument_sizes_the_list() {
    let text = estimate(
        RELATED_BY_TAKE,
        "{ product(id: \"1\") { related(take: 6) { title } } }",
        &json!({}),
    );

    let expected = "\
Valid query for `store`.
Estimated cost: 7
Depth: 3
By top-level field:
  product: 7
From the cost map:
  - `Product.related`: multiplier from `take` (rule `Product.related`)
";
    assert_eq!(text, expected, "six related products, and nothing assumed");
}

#[test]
fn without_the_rule_the_argument_means_nothing_to_the_estimate() {
    let text = estimate(
        "\"Query.viewer\" = { cost = 1 }\n",
        "{ product(id: \"1\") { related(take: 6) { title } } }",
        &json!({}),
    );

    assert!(
        text.contains("Estimated cost: 11\n"),
        "`take` is not a size argument the estimate knows:\n{text}"
    );
    assert!(
        text.contains("`Product.related` takes no size argument"),
        "so a size is assumed:\n{text}"
    );
}

#[test]
fn the_argument_is_followed_through_a_variable() {
    let operation = "query($n: Int) { product(id: \"1\") { related(take: $n) { title } } }";

    let given = estimate(RELATED_BY_TAKE, operation, &json!({ "n": 2 }));
    assert!(
        given.contains("Estimated cost: 3\n"),
        "the variable's value is used:\n{given}"
    );

    let defaulted = operation.replace("$n: Int", "$n: Int = 5");
    let defaulted = estimate(RELATED_BY_TAKE, &defaulted, &json!({}));
    assert!(
        defaulted.contains("Estimated cost: 6\n"),
        "the variable's default is used:\n{defaulted}"
    );
}

#[test]
fn the_default_in_the_schema_is_used_when_the_argument_is_left_out() {
    assert_cost(RELATED_BY_TAKE, "{ product(id: \"1\") { related { title } } }", 5);
}

#[test]
fn with_no_value_the_assumed_size_is_used_and_reported() {
    let operation = "query($n: Int) { product(id: \"1\") { related(take: $n) { title } } }";

    let text = estimate(RELATED_BY_TAKE, operation, &json!({}));

    assert!(
        text.contains("Estimated cost: 11\n"),
        "ten related products are assumed:\n{text}"
    );
    assert!(
        text.contains("`Product.related` is sized by `$n`, which has no value, so 10 items were assumed."),
        "the assumption names the variable:\n{text}"
    );
}

#[test]
fn an_explicit_null_is_reported_as_a_size_that_was_not_given() {
    let text = estimate(
        RELATED_BY_TAKE,
        "{ product(id: \"1\") { related(take: null) { title } } }",
        &json!({}),
    );

    assert!(
        text.contains("Estimated cost: 11\n"),
        "ten related products are assumed:\n{text}"
    );
    assert!(
        text.contains("`Product.related` was given no size argument, so 10 items were assumed."),
        "the wording knows that `take` is this field's size argument:\n{text}"
    );
}

#[test]
fn on_a_field_that_is_not_a_list_it_bounds_the_list_inside() {
    let map = "\"Query.catalog\" = { multiplier = \"pageSize\" }\n";

    assert_cost(map, "{ catalog(pageSize: 3) { total items { title } } }", 4);

    let no_size = estimate(map, "{ catalog { items { title } } }", &json!({}));
    assert!(
        no_size.contains("Estimated cost: 11\n"),
        "with no page size, ten items are assumed:\n{no_size}"
    );
    assert!(
        no_size.contains("`ProductPage.items` takes no size argument"),
        "and that is reported:\n{no_size}"
    );
}

#[test]
fn the_named_argument_replaces_the_built_in_ones_for_that_field() {
    let operation = "{ viewer { orders(first: 100, last: 2) { id } } }";

    assert_cost("\"Query.viewer\" = { cost = 1 }\n", operation, 101);
    assert_cost("\"User.orders\" = { multiplier = \"last\" }\n", operation, 3);
}

#[test]
fn a_field_without_that_argument_keeps_the_built_in_ones() {
    // The pattern matches every field, but only `related` has a `take`.
    let map = "\"*.*\" = { multiplier = \"take\" }\n";

    assert_cost(
        map,
        "{ viewer { orders(first: 2) { id } } product(id: \"1\") { related(take: 3) { title } } }",
        7,
    );
}

#[test]
fn a_number_from_a_more_specific_rule_wins_over_an_argument() {
    let map = "\"*.related\" = { multiplier = \"take\" }\n\"Product.related\" = { multiplier = 1 }\n";

    assert_cost(map, "{ product(id: \"1\") { related(take: 50) { title } } }", 2);
}
