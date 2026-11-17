//! Slice 58: `describe` of a type gives the shortest paths to it from `Query`. On a large schema a search finds a type
//! but not how to reach it, and the calling agent needs a selection that starts at a root field.

use std::fmt::Write as _;

use serde_json::json;

use crate::support::{Launch, Mcp};

const CONFIG: &str = r#"
[endpoints.store]
url    = "http://localhost:9/graphql"
schema = "schema.graphql"
"#;

/// Calls `describe` for `name` on the fixture endpoint and returns the text, failing on a tool error.
fn describe(name: &str) -> String {
    let mut mcp = Mcp::start(CONFIG);
    let reply = mcp.call("describe", &json!({ "endpoint": "store", "name": name }));
    reply.succeeded();
    reply.text
}

/// Calls `describe` for `name` on an endpoint whose schema is `sdl`, failing on a tool error.
fn describe_in(sdl: &str, name: &str) -> String {
    let config = "[endpoints.own]\nurl = \"http://localhost:9/graphql\"\nschema = \"own.graphql\"\n";
    let mut mcp = Launch::new(config).file("own.graphql", sdl).start();
    let reply = mcp.call("describe", &json!({ "endpoint": "own", "name": name }));
    reply.succeeded();
    reply.text
}

/// The lines from `# paths from` or `# no path from` to the end.
fn paths(text: &str) -> &str {
    text.find("# paths from")
        .or_else(|| text.find("# no path from"))
        .and_then(|start| text.get(start..))
        .unwrap_or_default()
}

#[test]
fn the_shortest_paths_come_first_one_per_field_of_query() {
    let text = describe("User");

    assert_eq!(
        paths(&text),
        "# paths from Query:\n\
         #   Query.viewer\n\
         #   Query.user\n\
         #   Query.search{... on User}\n\
         #   Query.node{... on User}\n\
         #   Query.users.nodes\n\
         #   and 5 more, from other fields of Query\n",
        "shortest first, in the order of the fields of Query, through unions and interfaces too:\n{text}"
    );
    assert!(
        text.contains("UserPage.items\n# paths from Query:\n"),
        "the paths come after the uses:\n{text}"
    );
}

#[test]
fn a_path_goes_through_an_interface_or_a_union_where_that_is_shorter() {
    assert_eq!(
        paths(&describe("Money")),
        "# paths from Query:\n\
         #   Query.node{... on Order}.total\n\
         #   Query.viewer.orders.total\n\
         #   Query.user.orders.total\n\
         #   Query.search{... on User}.orders.total\n\
         #   Query.users.nodes.orders.total\n\
         #   and 5 more, from other fields of Query\n",
        "a possible type is selected with `... on Type`, which is not a field"
    );
    assert_eq!(
        paths(&describe("Review")),
        "# paths from Query:\n\
         #   Query.product.reviews\n\
         #   Query.products.reviews\n\
         #   Query.search{... on Product}.reviews\n\
         #   Query.productsByIds.reviews\n\
         #   Query.node{... on Product}.reviews\n\
         #   and 5 more, from other fields of Query\n",
        "in the middle of a path as well as at its end"
    );
}

#[test]
fn a_type_with_few_paths_lists_them_all() {
    assert_eq!(
        paths(&describe("Node")),
        "# paths from Query:\n#   Query.node\n",
        "an interface"
    );
    assert_eq!(
        paths(&describe("SearchResult")),
        "# paths from Query:\n#   Query.search\n",
        "a union"
    );
    assert!(
        paths(&describe("Role")).starts_with("# paths from Query:\n#   Query.viewer.role\n"),
        "an enum is reached like any other type"
    );
}

#[test]
fn a_type_that_cannot_be_selected_from_query_says_so() {
    assert_eq!(
        paths(&describe("Country")),
        "# no path from Query\n",
        "an enum only taken as input"
    );

    let text = describe_in(
        "type Query { ping: Boolean }\ntype Mutation { pay: Receipt }\ntype Receipt { id: ID }\n",
        "Receipt",
    );
    assert!(
        text.ends_with("# used by: Mutation.pay\n# no path from Query\n"),
        "a type only a mutation returns:\n{text}"
    );
}

#[test]
fn a_deep_type_is_found_and_cycles_are_not_followed() {
    let mut sdl = String::from(
        "type Query { me: User }\n\
         type User { friends: [User!]! query: Query! start: Level0 }\n",
    );
    for level in 0..12 {
        let _ = writeln!(sdl, "type Level{level} {{ next: Level{} }}", level + 1);
    }
    sdl.push_str("type Level12 { prize: Prize }\ntype Prize { id: ID }\n");

    let text = describe_in(&sdl, "Prize");
    let expected = format!("# paths from Query:\n#   Query.me.start{}.prize\n", ".next".repeat(12));
    assert_eq!(paths(&text), expected, "one path, as short as it can be:\n{text}");
}

#[test]
fn input_objects_root_types_built_in_types_and_fields_have_no_paths() {
    for name in ["CreateOrderInput", "Query", "Mutation", "String", "Query.user"] {
        let text = describe(name);
        assert!(!text.contains("path from"), "{name} should have no paths:\n{text}");
    }
}
