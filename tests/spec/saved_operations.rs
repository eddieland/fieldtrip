//! Slice 36: an endpoint or a template names a directory of saved operations. `list_operations` lists them with
//! their description, variables and cost, `search_schema` finds them, and `execute` and `estimate_cost` take one by
//! `operation_name`, sending its canonical text after the usual approval. One that no longer validates against the
//! schema is listed as broken, is never sent, and is a warning in `fieldtrip check` and in the startup log.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, SdkClient};

/// A saved query that fits the fixture schema, with a two-line description.
const VALID: &str = "# Look up a user's name and email by ID.\n\
                     # Used by the support runbook.\n\
                     query UserById($id: ID!) {\n  user(id: $id) { name email } # inline comment\n}\n";

/// A saved query that the schema no longer has a field for.
const STALE: &str = "# Find an order by ID.\nquery OrderById($id: ID!) {\n  order(id: $id) { id status }\n}\n";

/// A saved mutation.
const MUTATION: &str =
    "# Cancel an order.\nmutation CancelOrder($id: ID!) {\n  cancelOrder(id: $id) { id status }\n}\n";

/// The canonical text of [`VALID`]: what the human is shown and the server receives.
const CANONICAL: &str = "query UserById($id: ID!) {\n  user(id: $id) {\n    name\n    email\n  }\n}";

/// The `store` endpoint with `settings` and the operations in `operations/`, sending operations to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\noperations = \"operations\"\n{settings}",
        mock.url()
    )
}

/// A launch of `config` with the valid, the stale and the mutation operations saved.
fn launch(config: &str) -> Launch {
    Launch::new(config)
        .file("operations/user.graphql", VALID)
        .file("operations/order.graphql", STALE)
        .file("operations/cancel.gql", MUTATION)
        .file("operations/notes.txt", "not an operation")
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": { "name": "Ada", "email": null } } }))
}

#[test]
fn list_operations_shows_each_with_the_broken_one_marked() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store" }));

    assert!(!reply.is_error, "{}", reply.text);
    let text = &reply.text;
    assert!(text.starts_with("3 saved operations for `store`"), "{text}");
    assert!(text.contains("UserById (query)"), "{text}");
    assert!(
        text.contains("description: Look up a user's name and email by ID. Used by the support runbook."),
        "the leading comment is the description, on one line: {text}"
    );
    assert!(text.contains("variables: $id: ID!"), "{text}");
    assert!(text.contains("estimated cost: 1"), "{text}");
    assert!(text.contains("    email\n"), "the canonical text is shown: {text}");
    assert!(
        !text.contains("inline comment"),
        "comments are not part of the text: {text}"
    );

    let stale = text
        .split("OrderById (query)")
        .nth(1)
        .expect("the stale operation is listed");
    assert!(stale.contains("description: Find an order by ID."), "{text}");
    assert!(stale.contains("BROKEN"), "the stale operation is marked: {text}");
    assert!(stale.contains("`order`"), "says what no longer fits: {text}");
    assert!(stale.contains("order.graphql"), "names the file to fix: {text}");

    assert!(
        text.contains("CancelOrder (mutation)"),
        "a .gql file is read too: {text}"
    );
    assert!(!text.contains("notes"), "other files are ignored: {text}");
    assert_eq!(mock.requests().len(), 0, "listing sends nothing");
}

#[test]
fn a_query_narrows_the_list() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "ORDER" }));
    assert!(reply.text.starts_with("2 of 3 saved operations"), "{}", reply.text);
    assert!(reply.text.contains("OrderById"), "{}", reply.text);
    assert!(reply.text.contains("CancelOrder"), "{}", reply.text);
    assert!(!reply.text.contains("UserById"), "{}", reply.text);

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "runbook" }));
    assert!(
        reply.text.starts_with("1 of 3"),
        "descriptions match too: {}",
        reply.text
    );

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "refund" }));
    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("No saved operation"), "{}", reply.text);
    assert!(
        reply.text.contains("`UserById`"),
        "lists the names there are: {}",
        reply.text
    );
}

#[test]
fn execute_by_name_sends_the_canonical_text_after_approval() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserById", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Sent query `UserById`"), "{}", reply.text);
    let prompts = mcp.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked as for any query");
    assert!(
        prompts[0].contains(CANONICAL),
        "the prompt shows the full text: {}",
        prompts[0]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0].body).expect("a JSON body");
    assert_eq!(body["query"], CANONICAL, "the canonical text is sent, without comments");
    assert_eq!(body["operationName"], "UserById");
    assert_eq!(body["variables"], json!({ "id": "u-1" }));
}

#[test]
fn execute_by_name_works_on_revision_2026_07_28() {
    let mock = mock();
    let client = SdkClient::start(&launch(&config(&mock, "")).human(Human::Accepts));

    let reply = client.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserById", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let prompts = client.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked");
    assert!(prompts[0].contains(CANONICAL), "{}", prompts[0]);
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn a_broken_operation_is_never_sent() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "OrderById", "variables": { "id": "o-1" } }),
    );

    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("saved operation `OrderById`"), "{}", reply.text);
    assert!(reply.text.contains("no longer validates"), "{}", reply.text);
    assert!(reply.text.contains("order.graphql"), "names the file: {}", reply.text);
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn estimate_cost_takes_a_name_too() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let reply = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation_name": "UserById", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Valid query `UserById`"), "{}", reply.text);
}

#[test]
fn a_name_and_an_operation_are_exclusive_and_one_is_needed() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "")).human(Human::Accepts).start();

    let both = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserById", "operation": "{ viewer { name } }" }),
    );
    assert!(both.is_error, "{}", both.text);
    assert!(both.text.contains("not both"), "{}", both.text);

    let neither = mcp.call("execute", &json!({ "endpoint": "store" }));
    assert!(neither.is_error, "{}", neither.text);
    assert!(neither.text.contains("`operation_name`"), "{}", neither.text);

    let unknown = mcp.call("execute", &json!({ "endpoint": "store", "operation_name": "UserByID" }));
    assert!(unknown.is_error, "{}", unknown.text);
    assert!(unknown.text.contains("Nearest names: `UserById`"), "{}", unknown.text);

    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_denied_mutation_is_refused_by_name_too() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();

    let listed = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "cancel" }));
    assert!(listed.text.contains("`execute` refuses it here"), "{}", listed.text);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "CancelOrder", "variables": { "id": "o-1" } }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("`mutations = \"deny\"`"), "{}", reply.text);
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn search_schema_finds_saved_operations_by_name_and_description() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "user" }));
    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.starts_with("Saved operations that match"), "{}", reply.text);
    assert!(reply.text.contains("UserById (query)"), "{}", reply.text);
    assert!(
        reply.text.contains("Query.user("),
        "schema matches follow: {}",
        reply.text
    );

    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "order" }));
    assert!(reply.text.contains("OrderById (query) BROKEN"), "{}", reply.text);

    let reply = mcp.call("search_schema", &json!({ "endpoint": "store", "query": "runbook" }));
    assert!(reply.text.contains("UserById"), "descriptions match: {}", reply.text);
}

#[test]
fn list_endpoints_points_to_the_saved_operations() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let reply = mcp.call("list_endpoints", &json!({}));
    assert!(reply.text.contains("saved operations: 3 in "), "{}", reply.text);
    assert!(reply.text.contains("`list_operations`"), "{}", reply.text);
}

#[test]
fn an_endpoint_without_a_library_says_how_to_add_one() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    ));

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store" }));
    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("`operations`"), "{}", reply.text);
}

#[test]
fn a_template_shares_its_library() {
    let mock = MockServer::unreachable();
    let config = format!(
        "[templates.shop]\nschema = \"schema.graphql\"\noperations = \"operations\"\n\n\
         [endpoints.store]\ntemplate = \"shop\"\nurl = \"{}\"\n",
        mock.url()
    );
    let mut mcp = launch(&config).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "UserById" }));
    assert!(reply.text.contains("UserById (query)"), "{}", reply.text);
}

#[test]
fn check_warns_about_the_stale_operation() {
    let checked = launch(&config(&MockServer::unreachable(), "")).check();

    assert!(
        checked.passed,
        "a broken operation is a warning: {}{}",
        checked.stdout, checked.stderr
    );
    let operations = checked.dir.path().join("operations");
    let order = operations.join("order.graphql");
    assert!(
        checked.stdout.contains(&format!(
            "  operations: {} (3 operations): warning: `OrderById` in {} no longer validates: ",
            operations.display(),
            order.display()
        )),
        "{}",
        checked.stdout
    );
    assert!(!checked.stdout.contains("`UserById`"), "{}", checked.stdout);
    assert!(checked.stdout.contains("0 problems, 1 warning."), "{}", checked.stdout);
}

#[test]
fn check_reports_a_good_library_as_ok() {
    let config = config(&MockServer::unreachable(), "");
    let checked = Launch::new(&config).file("operations/user.graphql", VALID).check();

    assert!(checked.passed, "{}", checked.stdout);
    let operations = checked.dir.path().join("operations");
    assert!(
        checked
            .stdout
            .contains(&format!("  operations: {} (1 operation): ok\n", operations.display())),
        "{}",
        checked.stdout
    );
}

#[test]
fn startup_logs_the_stale_operation() {
    let mcp = launch(&config(&MockServer::unreachable(), "")).start();

    let stderr = mcp.finish();
    assert!(
        stderr.contains("the saved operation `OrderById` for endpoint `store`"),
        "{stderr}"
    );
    assert!(stderr.contains("no longer validates"), "{stderr}");
    assert!(!stderr.contains("UserById"), "{stderr}");
}

#[test]
fn a_library_that_cannot_be_read_stops_the_server() {
    let mock = MockServer::unreachable();
    let cases = [
        (Launch::new(&config(&mock, "")), "is not a directory"),
        (
            Launch::new(&config(&mock, "")).file("operations/anonymous.graphql", "{ viewer { name } }"),
            "has no name",
        ),
        (
            Launch::new(&config(&mock, ""))
                .file("operations/a.graphql", VALID)
                .file("operations/b.graphql", VALID),
            "is used in both",
        ),
        (
            Launch::new(&config(&mock, "")).file(
                "operations/two.graphql",
                "query A { viewer { name } }\nquery B { viewer { name } }",
            ),
            "more than one operation",
        ),
        (
            Launch::new(&config(&mock, "")).file("operations/bad.graphql", "query Broken {"),
            "is not valid GraphQL",
        ),
        (
            Launch::new(&config(&mock, "")).file(
                "operations/live.graphql",
                "subscription Live { orderUpdated(id: \"1\") { id } }",
            ),
            "subscription",
        ),
    ];
    for (launch, expected) in cases {
        let stderr = launch.fails_to_start();
        assert!(stderr.contains("endpoint `store`"), "{stderr}");
        assert!(stderr.contains(expected), "expected {expected:?} in: {stderr}");
    }
}
