//! Slice 57: `describe` of a type says what uses it. A definition shows what a type holds, not who holds it, and a
//! calling agent working back from a type it found needs the fields that return it and the arguments that take it.

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

/// Starts a server whose one endpoint, `own`, reads `sdl` as its schema.
fn own_schema(sdl: &str) -> Mcp {
    let config = "[endpoints.own]\nurl = \"http://localhost:9/graphql\"\nschema = \"own.graphql\"\n";
    Launch::new(config).file("own.graphql", sdl).start()
}

#[test]
fn an_object_lists_its_unions_and_the_fields_that_return_it() {
    let text = describe("User");

    assert!(
        text.contains(
            "}\n\
             # member of: SearchResult\n\
             # used by: Query.viewer, Query.user, Mutation.deactivateUser, Order.customer, Review.author, \
             UserConnection.nodes, UserPage.items\n"
        ),
        "the unions, then every field that returns it, root types first and the rest by type name:\n{text}"
    );
}

#[test]
fn an_enum_lists_the_arguments_and_input_fields_that_take_it() {
    assert!(
        describe("Role").contains("}\n# used by: Query.users(role:), User.role\n"),
        "an argument is written `Type.field(argument:)`"
    );
    assert!(
        describe("Currency").contains("}\n# used by: CreateOrderInput.currency, Money.currency\n"),
        "an input field and an output field"
    );
}

#[test]
fn an_input_object_lists_what_takes_it() {
    assert!(
        describe("ProductFilter").contains("}\n# used by: Query.catalog(filter:)\n"),
        "an argument of a root field"
    );
    assert!(
        describe("OrderItemInput").contains("}\n# used by: CreateOrderInput.items\n"),
        "an input field of another input object, as a list"
    );
}

#[test]
fn an_interface_and_a_union_list_the_fields_that_return_them() {
    let node = describe("Node");
    assert!(
        node.contains("}\n# implemented by: User, Product, Order\n# used by: Query.node\n"),
        "after the implementers:\n{node}"
    );
    assert!(
        describe("SearchResult").starts_with("union SearchResult = User | Product\n# used by: Query.search\n"),
        "a union"
    );
}

#[test]
fn a_type_nothing_uses_says_so() {
    let mut mcp = own_schema("type Query { ping: Boolean }\ntype Orphan { id: ID }\n");

    let reply = mcp.call("describe", &json!({ "endpoint": "own", "name": "Orphan" }));
    reply.succeeded();
    assert!(
        reply.text.contains("}\n# used by: no field, argument or input field\n"),
        "an unused type is not mistaken for a missing section:\n{}",
        reply.text
    );
}

#[test]
fn a_long_list_of_uses_is_counted_past_thirty() {
    let mut sdl = String::from("type Query {\n");
    for index in 0..40 {
        let _ = writeln!(sdl, "  thing{index:02}: Thing");
    }
    sdl.push_str("}\ntype Thing { id: ID }\n");
    let mut mcp = own_schema(&sdl);

    let reply = mcp.call("describe", &json!({ "endpoint": "own", "name": "Thing" }));
    reply.succeeded();
    let line = reply
        .text
        .lines()
        .find(|line| line.starts_with("# used by: "))
        .unwrap_or_default();
    assert_eq!(line.matches("Query.thing").count(), 30, "thirty are listed: {line}");
    assert!(
        line.contains("Query.thing29") && !line.contains("Query.thing30"),
        "in field order: {line}"
    );
    assert!(line.ends_with(", and 10 more"), "the rest are counted: {line}");
}

#[test]
fn root_types_built_in_types_and_fields_list_no_uses() {
    for name in ["Query", "Mutation", "ID", "String", "Query.user"] {
        let text = describe(name);
        assert!(!text.contains("# used by"), "{name} should list no uses:\n{text}");
    }
}
