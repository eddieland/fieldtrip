//! Slice 52: a validation error for a name that does not exist lists the names that do.
//!
//! The fixture has `Query.catalog(pageSize: Int, filter: ProductFilter)`. `ProductFilter.title` is a `StringFilter`
//! with four fields, and `ProductFilter.madeIn` is a `Country`, an enum with 17 values, more than an error lists.

use serde_json::json;

use crate::support::{Mcp, MockServer, store_config};

/// Estimates `operation` on the fixture endpoint and returns the error text, failing if the tool succeeded.
fn rejection(operation: &str) -> String {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&store_config(&mock, ""));
    let reply = mcp.call("estimate_cost", &json!({ "endpoint": "store", "operation": operation }));
    assert!(
        reply.is_error,
        "the estimate should fail for {operation}: {}",
        reply.text
    );
    reply.text
}

#[test]
fn an_unknown_input_field_lists_the_fields_with_their_types() {
    let error = rejection("{\n  catalog(filter: { title: { contains: \"mug\" } }) { total }\n}");

    assert!(
        error.contains(
            "- 2:40 field `contains` does not exist on input `StringFilter`. \
             Its fields are: eq: String, ne: String, anyOf: [String!], noneOf: [String!]\n"
        ),
        "lists all four fields: {error}"
    );
}

#[test]
fn an_unknown_argument_lists_the_arguments_with_their_types() {
    let error = rejection("{ users(limit: 3) { totalCount } }");

    assert!(
        error.contains(
            "the argument `limit` is not supported by `Query.users`. \
             Its arguments are: first: Int = 20, after: String, role: Role\n"
        ),
        "lists the arguments, with a default where there is one: {error}"
    );
}

#[test]
fn an_unknown_argument_on_a_field_without_any_says_so() {
    let error = rejection("{ products(first: 3) { title } }");

    assert!(
        error.contains("the argument `first` is not supported by `Query.products`. It takes no arguments.\n"),
        "says the field takes no arguments: {error}"
    );
}

#[test]
fn an_unknown_enum_value_lists_the_values() {
    let error = rejection("{ users(role: OWNER) { totalCount } }");

    assert!(
        error.contains("value `OWNER` does not exist on enum `Role`. Its values are: ADMIN, CUSTOMER, GUEST\n"),
        "lists every value: {error}"
    );
}

#[test]
fn a_long_list_shows_the_nearest_names_first_and_counts_the_rest() {
    let error = rejection("{ catalog(filter: { madeIn: SWEEDEN }) { total } }");

    assert!(
        error.contains(
            "value `SWEEDEN` does not exist on enum `Country`. Its 17 values, nearest first: \
             SWEDEN, ARGENTINA, AUSTRALIA, BRAZIL, CANADA, CHINA, FRANCE, GERMANY, INDIA, ITALY, JAPAN, KOREA, \
             and 5 more\n"
        ),
        "puts the nearest values first, then schema order, then the count left out: {error}"
    );
}

#[test]
fn an_unknown_output_field_still_gets_the_nearest_names() {
    let error = rejection("{ viewer { nme } }");

    assert!(
        error.contains("Nearest fields on `User`: `name`"),
        "suggests the nearest field: {error}"
    );
}
