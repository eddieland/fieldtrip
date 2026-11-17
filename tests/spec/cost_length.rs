//! Slice 49: a rule can size a list by the length of a list argument, for batch lookups that return one item, or a
//! few, for each ID passed in.
//!
//! The fixture's `Query.productsByIds(ids: [ID!]!)` is such a lookup.

use serde_json::{Value, json};

use crate::support::{Launch, MockServer};

const BY_IDS: &str = "\"Query.productsByIds\" = { multiplier = { length = \"ids\" } }\n";

const CONFIG: &str = "[endpoints.store]\nurl = \"URL\"\nschema = \"schema.graphql\"\ncosts = \"store.costs.toml\"\n";

const WITH_VARIABLE: &str = "query($ids: [ID!]!) { productsByIds(ids: $ids) { title } }";

/// Calls `tool` on `store` under `map` with `arguments` added, and returns the text, failing on a tool error.
fn call(map: &str, tool: &str, mut arguments: Value) -> String {
    let mock = MockServer::unreachable();
    let mut mcp = Launch::new(&CONFIG.replace("URL", &mock.url()))
        .file("store.costs.toml", map)
        .start();
    arguments["endpoint"] = json!("store");
    let reply = mcp.call(tool, &arguments);
    assert!(!reply.is_error, "`{tool}` should succeed: {}", reply.text);
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
    reply.text
}

/// Estimates `operation` under `map` and returns the text.
fn estimate(map: &str, operation: &str, variables: &Value) -> String {
    call(
        map,
        "estimate_cost",
        json!({ "operation": operation, "variables": variables }),
    )
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
fn the_length_of_a_list_written_in_the_operation_sizes_the_result() {
    let text = estimate(
        BY_IDS,
        "{ productsByIds(ids: [\"1\", \"2\", \"3\"]) { title } }",
        &json!({}),
    );

    let expected = "\
Valid query for `store`.
Estimated cost: 3
Depth: 2
By top-level field:
  productsByIds: 3
From the cost map:
  - `Query.productsByIds`: multiplier from the length of `ids` (rule `Query.productsByIds`)
";
    assert_eq!(text, expected, "three products, and nothing assumed");
}

#[test]
fn without_the_rule_the_list_argument_means_nothing_to_the_estimate() {
    let text = assert_cost(
        "\"Query.viewer\" = { cost = 1 }\n",
        "{ productsByIds(ids: [\"1\", \"2\", \"3\"]) { title } }",
        &json!({}),
        10,
    );

    assert!(
        text.contains("`Query.productsByIds` takes no size argument, so 10 items were assumed."),
        "{text}"
    );
}

#[test]
fn the_length_of_a_variable_is_read_from_the_variables() {
    let ids: Vec<String> = (0..500).map(|id| id.to_string()).collect();

    assert_cost(BY_IDS, WITH_VARIABLE, &json!({ "ids": ids }), 500);
    assert_cost(BY_IDS, WITH_VARIABLE, &json!({ "ids": [] }), 0);
}

#[test]
fn a_variable_with_no_value_takes_its_default_or_the_assumed_size() {
    let defaulted = WITH_VARIABLE.replace("[ID!]!", "[ID!]! = [\"1\", \"2\"]");
    assert_cost(BY_IDS, &defaulted, &json!({}), 2);

    let text = assert_cost(BY_IDS, WITH_VARIABLE, &json!({}), 10);
    assert!(
        text.contains("`Query.productsByIds` is sized by `$ids`, which has no value, so 10 items were assumed."),
        "the assumption names the variable: {text}"
    );
}

#[test]
fn per_multiplies_the_length() {
    let map = "\"Query.productsByIds\" = { multiplier = { length = \"ids\", per = 3 } }\n";

    let text = assert_cost(map, WITH_VARIABLE, &json!({ "ids": ["1", "2", "3", "4"] }), 12);

    assert!(
        text.contains("  - `Query.productsByIds`: multiplier from the length of `ids`, times 3 (rule"),
        "the estimate says how the size was worked out: {text}"
    );
}

#[test]
fn a_single_value_counts_as_a_list_of_one() {
    // GraphQL accepts one value where a list is expected.
    assert_cost(BY_IDS, "{ productsByIds(ids: \"1\") { title } }", &json!({}), 1);
    assert_cost(BY_IDS, WITH_VARIABLE, &json!({ "ids": "1" }), 1);
}

#[test]
fn a_field_without_that_argument_keeps_the_built_in_sizes() {
    let map = "\"*.*\" = { multiplier = { length = \"ids\" } }\n";

    assert_cost(
        map,
        "{ viewer { orders(first: 2) { id } } productsByIds(ids: [\"1\", \"2\"]) { title } }",
        &json!({}),
        5,
    );
}

#[test]
fn introspect_reports_a_length_of_an_argument_that_is_missing_or_not_a_list() {
    let text = call(
        "\"Query.productsByIds\" = { multiplier = { length = \"idz\" } }\n\
         \"Query.user\" = { multiplier = { length = \"id\" } }\n\
         \"*.*\" = { multiplier = { length = \"ids\" } }\n",
        "introspect",
        json!({}),
    );

    assert!(
        text.contains("`Query.productsByIds` names the argument `idz`, which no field it matches has.\n"),
        "the misspelled argument is reported: {text}"
    );
    assert!(
        text.contains("`Query.user` takes the length of `id`, which is not a list on any field it matches.\n"),
        "an argument that cannot have a length is reported: {text}"
    );
    assert!(!text.contains("`*.*`"), "a pattern that fits one field is fine: {text}");
}

#[test]
fn a_length_that_is_not_valid_is_refused() {
    for value in [
        "{ length = 5 }",
        "{ length = \"not a name\" }",
        "{ per = 2 }",
        "{ length = \"ids\", per = -1 }",
        "{ length = \"ids\", per = \"three\" }",
        "{ length = \"ids\", by = 2 }",
        "{}",
    ] {
        let stderr = refused(&format!("\"Query.productsByIds\" = {{ multiplier = {value} }}\n"));

        assert!(
            stderr.contains("store.costs.toml") && stderr.contains("`Query.productsByIds`"),
            "names the file and the rule for `{value}`: {stderr}"
        );
        assert!(
            stderr.contains("`multiplier`") && stderr.contains("{ length = \"ids\" }"),
            "shows what a length looks like, for `{value}`: {stderr}"
        );
    }
}

#[test]
fn a_default_does_not_go_with_a_length() {
    let stderr = refused("\"Query.productsByIds\" = { multiplier = { length = \"ids\" }, default = 5 }\n");

    assert!(
        stderr.contains("`Query.productsByIds`") && stderr.contains("`default`"),
        "{stderr}"
    );
}
