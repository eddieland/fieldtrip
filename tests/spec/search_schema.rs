//! Slice 4: `search_schema` finds things in the fixture schema by name and by description, best match first.

use serde_json::json;

use crate::support::Mcp;

const CONFIG: &str = r#"
[endpoints.store]
url    = "http://localhost:9/graphql"
schema = "schema.graphql"
"#;

/// Searches the fixture endpoint for `query` and returns the text, failing on a tool error.
fn search(query: &str) -> String {
    let mut mcp = Mcp::start(CONFIG);
    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": query }));
    assert!(!reply.is_error, "search for {query:?} should succeed: {}", reply.text);
    reply.text
}

#[test]
fn finds_a_type_and_the_field_that_returns_it() {
    let expected = "\
2 matches for \"review\" in `store`, best first:
type Review
Product.reviews(limit: Int): [Review!]!
";
    assert_eq!(
        search("review"),
        expected,
        "each match is a one-line signature that `describe` accepts"
    );
}

#[test]
fn an_exact_operation_name_comes_first() {
    let text = search("user");
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(
        lines.get(1).copied(),
        Some("Query.user(id: ID!): User  \"Look up one user by ID.\""),
        "the query named exactly `user` leads:\n{text}"
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("type User implements Node  \"A person with an account. Customers and staff are both users.\""),
        "the type named exactly `User` is next:\n{text}"
    );
    assert_eq!(
        lines.get(3).copied(),
        Some("Query.users(first: Int = 20, after: String, role: Role): UserConnection!  \"Users, newest first.\""),
        "then the query whose name starts with it:\n{text}"
    );
}

#[test]
fn finds_a_mutation_ahead_of_an_enum_value() {
    let expected = "\
2 matches for \"cancel\" in `store`, best first:
Mutation.cancelOrder(id: ID!, reason: String): Order  \"Cancel an order that has not shipped yet.\"
OrderStatus.CANCELLED
";
    assert_eq!(search("cancel"), expected, "operations outrank other kinds of match");
}

#[test]
fn finds_an_argument() {
    let expected = "\
1 match for \"reason\" in `store`, best first:
Mutation.cancelOrder(reason: String)
";
    assert_eq!(search("reason"), expected, "an argument is shown on its field");
}

#[test]
fn finds_an_enum_value() {
    let expected = "\
1 match for \"guest\" in `store`, best first:
Role.GUEST  \"Read-only access.\"
";
    assert_eq!(search("guest"), expected, "the search ignores case");
}

#[test]
fn finds_by_description() {
    let expected = "\
1 match for \"warehouse\" in `store`, best first:
CreateOrderInput.note: String  \"Free-text note for the warehouse.\"
";
    assert_eq!(
        search("warehouse"),
        expected,
        "a word that only appears in a description still matches"
    );
}

#[test]
fn every_word_must_match_and_one_may_name_the_parent_type() {
    let expected = "\
1 match for \"user email\" in `store`, best first:
User.email: String
";
    assert_eq!(
        search("user email"),
        expected,
        "`user` picks the type and `email` picks the field"
    );
}

#[test]
fn marks_deprecated_matches() {
    let text = search("nickname");

    assert!(
        text.contains("User.nickname: String @deprecated"),
        "a deprecated field says so:\n{text}"
    );
}

#[test]
fn a_broad_search_is_cut_off_with_a_count() {
    let text = search("e");
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines.len(), 22, "a header, twenty matches and a note:\n{text}");
    let note = lines.last().copied().unwrap_or_default();
    assert!(
        note.starts_with("Showing the best 20 of "),
        "the note says how many matched:\n{text}"
    );
    assert!(
        note.contains("were left out"),
        "the note says some were left out:\n{text}"
    );
}

#[test]
fn no_match_suggests_near_names() {
    let text = search("prodcut");

    assert!(
        text.starts_with("No matches for \"prodcut\" in `store`."),
        "says nothing matched:\n{text}"
    );
    assert!(text.contains("`Product`"), "suggests a near name:\n{text}");
}

#[test]
fn an_empty_query_is_an_error() {
    let mut mcp = Mcp::start(CONFIG);

    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "  " }));

    assert!(reply.is_error, "an empty search should be an error: {}", reply.text);
}
