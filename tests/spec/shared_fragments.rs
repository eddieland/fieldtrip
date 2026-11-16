//! Slice 44: `fragments` names a directory of shared fragments. A saved operation that spreads one it does not
//! define gets it appended, along with the fragments that one spreads, so validation, `estimate_cost`,
//! `list_operations`, `uses`, the prompt and the request all see the full text. A fragment in the operation's own
//! file wins. An operation the agent writes stays self-contained.

use serde_json::{Value, json};

use crate::support::{Human, Launch, MockServer};

/// A saved query that spreads two shared fragments, one of which spreads a third.
const USER_ORDERS: &str = "# A user and their recent orders.\n\
                           query UserOrders($id: ID!) {\n  user(id: $id) {\n    ...UserFields\n    \
                           orders(first: 5) { ...OrderFields }\n  }\n}\n";

/// A saved query that defines `UserFields` itself.
const LOCAL: &str = "query LocalUser($id: ID!) {\n  user(id: $id) { ...UserFields }\n}\n\n\
                     fragment UserFields on User { id }\n";

/// A saved query that spreads a fragment defined nowhere.
const MISSING: &str = "query Lost($id: ID!) {\n  user(id: $id) { ...Missing }\n}\n";

/// Shared fragments on users, two in one file. `UserFields` spreads `Contact`.
const USER_FRAGMENTS: &str = "# Shared by the user queries.\n\
                              fragment UserFields on User { name ...Contact }\n\n\
                              fragment Contact on User { email }\n";

/// A shared fragment on orders, which spreads `Contact` too.
const ORDER_FRAGMENTS: &str = "fragment OrderFields on Order { id status customer { ...Contact } }\n";

/// A shared fragment no operation uses.
const UNUSED: &str = "fragment ProductFields on Product { id title }\n";

/// The `store` endpoint with `settings`, sending operations to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\noperations = \"queries\"\n\
         fragments = \"queries/fragments\"\n{settings}",
        mock.url()
    )
}

/// A launch of `config` with the operations and fragments above.
fn launch(config: &str) -> Launch {
    Launch::new(config)
        .file("queries/user_orders.graphql", USER_ORDERS)
        .file("queries/local.graphql", LOCAL)
        .file("queries/lost.graphql", MISSING)
        .file("queries/fragments/user.graphql", USER_FRAGMENTS)
        .file("queries/fragments/order.gql", ORDER_FRAGMENTS)
        .file("queries/fragments/product.graphql", UNUSED)
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": { "name": "Ada", "email": null, "orders": [] } } }))
}

/// The `query` of the only request `mock` received.
fn sent(mock: &MockServer) -> String {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one request is sent");
    let body: Value = serde_json::from_str(&requests[0].body).expect("a JSON body");
    body["query"].as_str().expect("a query string").to_owned()
}

#[test]
fn execute_sends_and_shows_the_shared_fragments() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserOrders", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let query = sent(&mock);
    assert!(query.starts_with("query UserOrders($id: ID!) {"), "{query}");
    for fragment in [
        "fragment UserFields on User {",
        "fragment OrderFields on Order {",
        "fragment Contact on User {",
    ] {
        assert_eq!(
            query.matches(fragment).count(),
            1,
            "each fragment is sent once: {query}"
        );
    }
    assert!(
        !query.contains("ProductFields"),
        "unused fragments are left out: {query}"
    );
    assert!(!query.contains("Shared by"), "comments are dropped: {query}");

    let prompts = mcp.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked");
    assert!(
        prompts[0].contains(&query),
        "the prompt shows exactly what is sent: {}",
        prompts[0]
    );
}

#[test]
fn list_operations_and_estimate_cost_see_the_full_text() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "UserOrders" }));
    assert!(!reply.is_error, "{}", reply.text);
    assert!(!reply.text.contains("BROKEN"), "{}", reply.text);
    assert!(
        reply.text.contains("description: A user and their recent orders."),
        "{}",
        reply.text
    );
    assert!(reply.text.contains("fragment Contact on User"), "{}", reply.text);

    let reply = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation_name": "UserOrders", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Valid query `UserOrders`"), "{}", reply.text);

    let reply = mcp.call(
        "list_operations",
        &json!({ "endpoint": "store", "uses": ["Order.customer"] }),
    );
    assert!(
        reply.text.contains("UserOrders"),
        "`uses` follows shared fragments: {}",
        reply.text
    );
}

#[test]
fn a_fragment_in_the_operations_own_file_wins() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "LocalUser", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let query = sent(&mock);
    assert!(query.contains("fragment UserFields on User {\n  id\n}"), "{query}");
    assert_eq!(query.matches("fragment UserFields").count(), 1, "{query}");
    assert!(!query.contains("Contact"), "the shared one is not used: {query}");
}

#[test]
fn a_missing_fragment_marks_the_operation_broken() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let listed = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "Lost" }));
    assert!(listed.text.contains("BROKEN"), "{}", listed.text);
    assert!(listed.text.contains("Missing"), "names the fragment: {}", listed.text);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "Lost", "variables": { "id": "u-1" } }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Missing"), "{}", reply.text);
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn an_operation_the_agent_writes_stays_self_contained() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": "query Mine { viewer { ...UserFields } }",
        }),
    );

    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("UserFields"), "{}", reply.text);
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_template_can_hold_the_fragments() {
    let mock = mock();
    let config = format!(
        "[templates.shop]\nschema = \"schema.graphql\"\nfragments = \"queries/fragments\"\n\n\
         [endpoints.store]\ntemplate = \"shop\"\nurl = \"{}\"\noperations = \"queries\"\n",
        mock.url()
    );
    let mut mcp = launch(&config).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserOrders", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "{}", reply.text);
    assert!(sent(&mock).contains("fragment Contact on User"));
}

#[test]
fn check_counts_the_fragments_and_warns_about_unused_ones() {
    let checked = launch(&config(&MockServer::unreachable(), "")).check();

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    let fragments = checked.dir.path().join("queries").join("fragments");
    let product = fragments.join("product.graphql");
    assert!(
        checked.stdout.contains(&format!(
            "  fragments: {} (4 fragments): warning: `ProductFields` in {} is used by no operation\n",
            fragments.display(),
            product.display()
        )),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.contains("`Lost`") && checked.stdout.contains("Missing"),
        "the missing fragment is a warning on its operation: {}",
        checked.stdout
    );
    assert!(checked.stdout.contains("0 problems, 2 warnings."), "{}", checked.stdout);
}

#[test]
fn check_reports_fragments_that_are_all_used_as_ok() {
    let config = config(&MockServer::unreachable(), "");
    let checked = Launch::new(&config)
        .file("queries/user_orders.graphql", USER_ORDERS)
        .file("queries/fragments/user.graphql", USER_FRAGMENTS)
        .file("queries/fragments/order.gql", ORDER_FRAGMENTS)
        .check();

    assert!(checked.passed, "{}", checked.stdout);
    let fragments = checked.dir.path().join("queries").join("fragments");
    assert!(
        checked
            .stdout
            .contains(&format!("  fragments: {} (3 fragments): ok\n", fragments.display())),
        "{}",
        checked.stdout
    );
}

#[test]
fn bad_fragments_stop_the_server() {
    let mock = MockServer::unreachable();
    let operations = |launch: Launch| launch.file("queries/user_orders.graphql", USER_ORDERS);
    let cases = [
        (
            operations(Launch::new(&config(&mock, ""))),
            vec!["`fragments` names", "is not a directory"],
        ),
        (
            operations(Launch::new(&config(&mock, "")))
                .file("queries/fragments/user.graphql", USER_FRAGMENTS)
                .file("queries/fragments/again.graphql", "fragment Contact on User { name }\n"),
            vec!["`Contact`", "again.graphql", "user.graphql"],
        ),
        (
            operations(Launch::new(&config(&mock, "")))
                .file("queries/fragments/query.graphql", "query Q { viewer { name } }\n"),
            vec!["query.graphql", "only fragment definitions"],
        ),
        (
            operations(Launch::new(&config(&mock, ""))).file("queries/fragments/bad.graphql", "fragment Bad on {"),
            vec!["bad.graphql", "is not valid GraphQL"],
        ),
        (
            Launch::new(&format!(
                "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\nfragments = \"queries/fragments\"\n",
                mock.url()
            ))
            .file("queries/fragments/user.graphql", USER_FRAGMENTS),
            vec!["`fragments`", "`operations`"],
        ),
    ];
    for (launch, expected) in cases {
        let stderr = launch.fails_to_start();
        assert!(stderr.contains("endpoint `store`"), "{stderr}");
        for expected in expected {
            assert!(stderr.contains(expected), "expected {expected:?} in: {stderr}");
        }
    }
}
