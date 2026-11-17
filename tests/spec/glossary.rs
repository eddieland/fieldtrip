//! Slice 53: a glossary maps the user's words to the schema's names. People describe a task in the words of the
//! product they use, so `search_schema` and `list_operations` look a word up in the glossary first and say which
//! term matched, and `introspect` reports the targets the schema no longer has.

use serde_json::json;

use crate::support::{Launch, Mcp, MockServer};

const CONFIG: &str = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n\
                      glossary = \"store.glossary.toml\"\n";

/// The user's words for things in the fixture schema.
const GLOSSARY: &str = r#"
[glossary]
"staff" = ["Role.ADMIN"]
"client" = ["User"]
"basket" = ["Order", "Order.items"]
"line item" = ["OrderItem"]
"note" = ["CreateOrderInput.note"]
"#;

/// Saved operations, by file name.
const OPERATIONS: [(&str, &str); 4] = [
    (
        "basket_report.graphql",
        "# What the basket team reads each morning.\nquery MorningReport { products { title } }",
    ),
    (
        "cancel.graphql",
        "# Cancel an order.\nmutation CancelOrder($id: ID!) { cancelOrder(id: $id) { id status } }",
    ),
    (
        "user.graphql",
        "# A user's name and email.\nquery UserById($id: ID!) { user(id: $id) { name email } }",
    ),
    (
        "user_orders.graphql",
        "# What a user bought.\nquery UserOrders($id: ID!) { user(id: $id) { orders(first: 5) { items { quantity } } } }",
    ),
];

/// Starts the server with `glossary` as the glossary file.
fn start(glossary: &str) -> Mcp {
    Launch::new(CONFIG).file("store.glossary.toml", glossary).start()
}

/// Starts the server with [`GLOSSARY`] and [`OPERATIONS`].
fn start_with_operations() -> Mcp {
    let mut launch =
        Launch::new(&format!("{CONFIG}operations = \"operations\"\n")).file("store.glossary.toml", GLOSSARY);
    for (file, text) in OPERATIONS {
        launch = launch.file(&format!("operations/{file}"), text);
    }
    launch.start()
}

/// Searches the `store` endpoint for `query`, failing on a tool error.
fn search(mcp: &mut Mcp, query: &str) -> String {
    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": query }));
    assert!(!reply.is_error, "search for {query:?} should succeed: {}", reply.text);
    reply.text
}

/// Lists the saved operations of `store` that match `query`, failing on a tool error.
fn list(mcp: &mut Mcp, query: &str) -> String {
    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": query }));
    assert!(!reply.is_error, "{}", reply.text);
    reply.text
}

/// Starts the server with `glossary`, expecting it to refuse, and returns what it wrote to stderr.
fn refused(glossary: &str) -> String {
    let stderr = Launch::new(CONFIG)
        .file("store.glossary.toml", glossary)
        .fails_to_start();
    assert!(
        stderr.contains("store.glossary.toml"),
        "the error names the glossary file: {stderr}"
    );
    assert!(stderr.contains("`store`"), "the error names the endpoint: {stderr}");
    stderr
}

/// Returns what `introspect` says about `glossary`: the lines from `Glossary:` on.
fn report(glossary: &str) -> String {
    let mock = MockServer::unreachable();
    let mut mcp = start(glossary);
    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));
    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    assert_eq!(mock.requests().len(), 0, "the glossary is checked with no request");
    let start = reply
        .text
        .find("Glossary:")
        .unwrap_or_else(|| panic!("no glossary report in:\n{}", reply.text));
    reply.text.get(start..).unwrap_or_default().to_owned()
}

#[test]
fn a_term_gives_its_target_first_and_says_which_term_matched() {
    let mut mcp = start(GLOSSARY);

    let expected = "\
From the user's glossary for `store`, their words for things in this schema:
staff → Role.ADMIN

1 other match for \"staff\" in `store`, best first:
type User implements Node  \"A person with an account. Customers and staff are both users.\"
";
    assert_eq!(
        search(&mut mcp, "staff"),
        expected,
        "the glossary's target leads, and the schema's own matches follow"
    );
}

#[test]
fn a_term_the_schema_never_uses_still_finds_its_target() {
    let mut mcp = start(GLOSSARY);

    let expected = "\
From the user's glossary for `store`, their words for things in this schema:
client → type User implements Node  \"A person with an account. Customers and staff are both users.\"
";
    assert_eq!(
        search(&mut mcp, "Client"),
        expected,
        "a term is matched ignoring case, and nothing says there were no matches"
    );
}

#[test]
fn a_term_with_several_targets_lists_each_and_none_is_repeated_below() {
    let mut mcp = start(&GLOSSARY.replace("\"note\"", "\"warehouse\""));

    let text = search(&mut mcp, "basket");
    assert!(
        text.contains("basket → type Order implements Node\nbasket → Order.items: [OrderItem!]!\n"),
        "each target has its own line, in the order the user wrote them:\n{text}"
    );

    let expected = "\
From the user's glossary for `store`, their words for things in this schema:
warehouse → CreateOrderInput.note: String  \"Free-text note for the warehouse.\"
";
    assert_eq!(
        search(&mut mcp, "warehouse"),
        expected,
        "a match the glossary already gave is not listed a second time"
    );
}

#[test]
fn a_term_of_several_words_matches_those_words_together() {
    let mut mcp = start(GLOSSARY);

    let text = search(&mut mcp, "line  ITEM");
    assert!(
        text.contains("line item → type OrderItem\n"),
        "the words match in a row, whatever the case and spacing:\n{text}"
    );

    let text = search(&mut mcp, "item");
    assert!(
        !text.contains("glossary"),
        "one of the words alone is not the term:\n{text}"
    );
}

#[test]
fn a_term_is_a_whole_word() {
    let mut mcp = start(GLOSSARY);

    let text = search(&mut mcp, "clients");

    assert!(
        !text.contains("glossary"),
        "`clients` is not the term `client`:\n{text}"
    );
}

#[test]
fn a_term_among_other_words_is_still_found() {
    let mut mcp = start(GLOSSARY);

    let text = search(&mut mcp, "client email");

    assert!(
        text.starts_with("From the user's glossary for `store`"),
        "the glossary comes first:\n{text}"
    );
    assert!(
        text.contains("client → type User implements Node"),
        "says what the term stands for:\n{text}"
    );
    assert!(
        text.contains("No matches for \"client email\" in `store`."),
        "the search for the words as written is still reported:\n{text}"
    );
}

#[test]
fn a_target_the_schema_lacks_is_marked_in_a_search() {
    let mut mcp = start("[glossary]\n\"draft\" = [\"Order.isDraft\", \"Order.status\"]\n");

    let text = search(&mut mcp, "draft");

    assert!(
        text.contains("draft → Order.isDraft  (not in this schema: the user's glossary is out of date. Tell them.)\n"),
        "the stale target is shown as stale:\n{text}"
    );
    assert!(
        text.contains("draft → Order.status: OrderStatus!\n"),
        "the target that exists is shown as usual:\n{text}"
    );
}

#[test]
fn a_search_with_no_glossary_term_reads_as_before() {
    let mut mcp = start(GLOSSARY);

    let expected = "\
2 matches for \"review\" in `store`, best first:
type Review
Product.reviews(limit: Int): [Review!]!
";
    assert_eq!(search(&mut mcp, "review"), expected);
}

#[test]
fn list_operations_finds_operations_that_use_a_terms_target() {
    let mut mcp = start_with_operations();

    let text = list(&mut mcp, "client");

    assert!(
        text.starts_with("2 of 4 saved operations for `store` match \"client\"."),
        "neither operation has the word, but both select fields of `User`:\n{text}"
    );
    assert!(
        text.contains("UserById (query)\n  glossary: client → User\n"),
        "says which term matched, and what it stands for:\n{text}"
    );
    assert!(
        text.contains("UserOrders (query)\n  glossary: client → User\n"),
        "on each operation:\n{text}"
    );
    assert!(
        !text.contains("CancelOrder ("),
        "an operation that uses no `User` is left out:\n{text}"
    );
}

#[test]
fn list_operations_puts_glossary_matches_before_matches_by_word() {
    let mut mcp = start_with_operations();

    let text = list(&mut mcp, "basket");

    assert!(
        text.starts_with("3 of 4 saved operations for `store` match \"basket\"."),
        "two use a target of `basket`, and one has the word in its description:\n{text}"
    );
    assert!(
        text.contains("CancelOrder (mutation)\n  glossary: basket → Order\n"),
        "`cancelOrder` returns an `Order`:\n{text}"
    );
    assert!(
        text.contains("UserOrders (query)\n  glossary: basket → Order, Order.items\n"),
        "an operation that uses both targets names both:\n{text}"
    );
    let report = text.find("MorningReport (").unwrap_or(0);
    let orders = text.find("UserOrders (").unwrap_or(usize::MAX);
    assert!(
        orders < report,
        "the operation that only has the word in its description comes last:\n{text}"
    );
    assert!(
        !text.contains("MorningReport (query)\n  glossary:"),
        "and it has no glossary line:\n{text}"
    );
}

#[test]
fn list_operations_combines_a_term_with_other_words_and_filters() {
    let mut mcp = start_with_operations();

    let text = list(&mut mcp, "bought client");
    assert!(
        text.starts_with("1 of 4 saved operations for `store` match \"bought client\"."),
        "the other word still has to be in the name or description:\n{text}"
    );
    assert!(
        text.contains("UserOrders (query)\n  glossary: client → User\n"),
        "{text}"
    );

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "query": "basket", "kind": "mutation" }),
    );
    assert!(
        reply
            .text
            .starts_with("1 of 4 saved operations for `store` match \"basket\", mutations only."),
        "`kind` narrows the glossary's matches too: {}",
        reply.text
    );
}

#[test]
fn search_schema_lists_saved_operations_found_through_the_glossary() {
    let mut mcp = start_with_operations();

    let text = search(&mut mcp, "client");

    assert!(
        text.starts_with("From the user's glossary for `store`"),
        "the glossary's targets still come first:\n{text}"
    );
    assert!(
        text.contains("UserById (query, cost 1, $id: ID!; glossary: client → User)"),
        "a saved operation that uses the target is offered, with the term:\n{text}"
    );
}

#[test]
fn introspect_says_when_every_target_is_in_the_schema() {
    let text = report(GLOSSARY);

    assert!(
        text.starts_with("Glossary: 5 terms from "),
        "says how many terms there are: {text}"
    );
    assert!(
        text.contains("store.glossary.toml"),
        "says where they came from: {text}"
    );
    assert!(
        text.ends_with("Every target is in this schema.\n"),
        "says all is well: {text}"
    );
}

#[test]
fn introspect_reports_targets_the_schema_does_not_have() {
    let text = report(
        "[glossary]\n\
         \"draft\" = [\"Order.isDraft\", \"Order.status\"]\n\
         \"PR\" = [\"PullRequest\"]\n\
         \"client\" = [\"User\"]\n",
    );

    assert!(
        text.contains(
            "These glossary targets are not in this schema: `PR` → `PullRequest`, `draft` → `Order.isDraft`. Tell the \
             user, who can fix the glossary.\n"
        ),
        "the removed field and the missing type are listed, and the good targets are not: {text}"
    );
}

#[test]
fn list_endpoints_and_check_show_the_glossary() {
    let mut mcp = start(GLOSSARY);
    let reply = mcp.call("list_endpoints", &json!({}));
    let path = mcp.dir().join("store.glossary.toml");
    assert!(
        reply
            .text
            .contains(&format!("  glossary: {} (5 terms)\n", path.display())),
        "list_endpoints names the file: {}",
        reply.text
    );
    drop(mcp);

    let checked = Launch::new(CONFIG)
        .file(
            "store.glossary.toml",
            "[glossary]\n\"draft\" = [\"Order.isDraft\"]\n\"client\" = [\"User\"]\n",
        )
        .check();
    assert!(
        checked.passed,
        "a stale target is a warning: {}{}",
        checked.stdout, checked.stderr
    );
    assert!(
        checked.stdout.contains(
            "store.glossary.toml (2 terms): warning: These glossary targets are not in this schema: `draft` → \
             `Order.isDraft`."
        ),
        "check reports the stale target: {}",
        checked.stdout
    );
    assert!(checked.stdout.contains("0 problems, 1 warning."), "{}", checked.stdout);
}

#[test]
fn an_endpoint_takes_the_glossary_from_its_template_unless_it_sets_one() {
    let config = "[templates.shop]\nglossary = \"shop.glossary.toml\"\nschema = \"schema.graphql\"\n\
                  [endpoints.dev]\ntemplate = \"shop\"\nurl = \"http://localhost:9/graphql\"\n\
                  [endpoints.prod]\ntemplate = \"shop\"\nurl = \"http://localhost:9/graphql\"\n\
                  glossary = \"prod.glossary.toml\"\n";
    let mut mcp = Launch::new(config)
        .file("shop.glossary.toml", "[glossary]\n\"client\" = [\"User\"]\n")
        .file("prod.glossary.toml", "[glossary]\n\"client\" = [\"Order\"]\n")
        .start();

    let dev = mcp.call("search_schema", &json!({ "endpoint": "dev", "query": "client" }));
    let prod = mcp.call("search_schema", &json!({ "endpoint": "prod", "query": "client" }));

    assert!(dev.text.contains("client → type User "), "the template's: {}", dev.text);
    assert!(prod.text.contains("client → type Order "), "its own: {}", prod.text);
}

#[test]
fn a_glossary_file_that_is_missing_or_not_toml_is_refused() {
    let stderr = Launch::new(CONFIG).fails_to_start();
    assert!(
        stderr.contains("store.glossary.toml"),
        "names the missing file: {stderr}"
    );

    let stderr = refused("[glossary]\n\"PR\" = [\"PullRequest\"]\n\"draft\" = [\n");
    assert!(stderr.contains("line 3"), "says where the problem is: {stderr}");
}

#[test]
fn a_glossary_with_anything_but_the_glossary_table_is_refused() {
    let stderr = refused("\"PR\" = [\"PullRequest\"]\n");

    assert!(
        stderr.contains("[glossary]"),
        "says the terms go under `[glossary]`: {stderr}"
    );
}

#[test]
fn a_target_that_is_not_a_type_or_a_member_of_one_is_refused() {
    for target in [
        "",
        "Pull Request",
        "PullRequest.",
        "Query.user.name",
        "User.*",
        "Query.user(id)",
    ] {
        let stderr = refused(&format!("[glossary]\n\"PR\" = [\"{target}\"]\n"));

        assert!(stderr.contains("`PR`"), "names the term for `{target}`: {stderr}");
        assert!(
            stderr.contains("`PullRequest.isDraft`"),
            "shows what a target looks like for `{target}`: {stderr}"
        );
    }
}

#[test]
fn a_term_with_no_words_or_no_targets_is_refused() {
    let stderr = refused("[glossary]\n\" \" = [\"User\"]\n");
    assert!(stderr.contains("no words"), "says what is wrong: {stderr}");

    let stderr = refused("[glossary]\n\"PR\" = []\n");
    assert!(stderr.contains("`PR`"), "names the term: {stderr}");
    assert!(stderr.contains("no targets"), "says what is wrong: {stderr}");
}

#[test]
fn two_terms_that_differ_only_in_case_are_refused() {
    let stderr = refused("[glossary]\n\"PR\" = [\"User\"]\n\"pr\" = [\"Order\"]\n");

    assert!(
        stderr.contains("`PR`") && stderr.contains("`pr`"),
        "names both: {stderr}"
    );
    assert!(stderr.contains("ignor"), "says terms ignore case: {stderr}");
}
