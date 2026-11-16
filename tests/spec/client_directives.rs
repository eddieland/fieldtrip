//! Slice 47: `client_directives` names directives that only the user's code generator reads. They are removed from
//! saved operations and shared fragments as they load, wherever they are used, so what is validated, costed, shown in
//! the prompt and sent has none of them. A directive that is not listed and not in the schema is still an error, an
//! operation the agent writes is validated as written, and `fieldtrip check` warns about a listed directive that
//! nothing uses.

use serde_json::{Value, json};

use crate::support::{Human, Launch, MockServer};

/// A saved query that uses listed directives on the operation, a variable, a field, a fragment spread, an inline
/// fragment and a fragment of its own, alongside `@include`, which the server must still see.
const GET_ORDERS: &str = "# A user's orders, a page at a time.\n\
                          query GetOrders($id: ID!, $full: Boolean! = false @defaultValue(value: \"no\")) \
                          @paginated {\n  user(id: $id) {\n    name @immutable\n    \
                          orders(first: 5) { ...OrderFields @baseClass(name: \"OrderBase\") }\n    \
                          ... on User @unordered { email @include(if: $full) }\n    \
                          ...Shared\n  }\n}\n\n\
                          fragment OrderFields on Order @baseClass(name: \"Base\") { id status }\n";

/// A saved mutation with a listed directive on the operation.
const SAVE_ORDER: &str = "mutation SaveOrder($input: CreateOrderInput!) @idempotent {\n  \
                          createOrder(input: $input) { id }\n}\n";

/// A saved query with a misspelled directive that is not listed.
const TYPO: &str = "query Typo($id: ID!) @paginatd {\n  user(id: $id) { id }\n}\n";

/// A shared fragment that uses a listed directive.
const SHARED: &str = "fragment Shared on User @immutable { role }\n";

/// The directives the generator reads, one of which nothing uses.
const LISTED: &str = "client_directives = [\"paginated\", \"idempotent\", \"baseClass\", \"defaultValue\", \"unordered\", \
     \"immutable\", \"retry\"]\n";

/// Every listed directive, as it would appear in a document.
const USES: [&str; 7] = [
    "@paginated",
    "@idempotent",
    "@baseClass",
    "@defaultValue",
    "@unordered",
    "@immutable",
    "@retry",
];

/// The `store` endpoint with `settings`, sending operations to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\noperations = \"queries\"\n\
         fragments = \"queries/fragments\"\n{settings}",
        mock.url()
    )
}

/// A launch of `config` with the operations and fragment above.
fn launch(config: &str) -> Launch {
    Launch::new(config)
        .file("queries/get_orders.graphql", GET_ORDERS)
        .file("queries/save_order.graphql", SAVE_ORDER)
        .file("queries/typo.graphql", TYPO)
        .file("queries/fragments/shared.graphql", SHARED)
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": { "name": "Ada", "orders": [], "role": "ADMIN" } } }))
}

/// The `query` of the only request `mock` received.
fn sent(mock: &MockServer) -> String {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one request is sent");
    let body: Value = serde_json::from_str(&requests[0].body).expect("a JSON body");
    body["query"].as_str().expect("a query string").to_owned()
}

#[test]
fn listed_directives_are_removed_before_the_prompt_and_the_request() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, LISTED)).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "GetOrders", "variables": { "id": "u-1" } }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let query = sent(&mock);
    assert!(
        query.starts_with("query GetOrders($id: ID!, $full: Boolean! = false) {"),
        "{query}"
    );
    for listed in USES {
        assert!(!query.contains(listed), "{listed} is removed: {query}");
    }
    for kept in [
        "...OrderFields",
        "... on User {",
        "fragment OrderFields on Order {",
        "fragment Shared on User {",
        "email @include(if: $full)",
    ] {
        assert!(query.contains(kept), "{kept:?} is kept: {query}");
    }

    let prompts = mcp.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked");
    assert!(
        prompts[0].contains(&query),
        "the prompt shows exactly what is sent: {}",
        prompts[0]
    );
}

#[test]
fn a_mutation_loses_its_listed_directive_too() {
    let mock = MockServer::answering(&json!({ "data": { "createOrder": { "id": "o-1" } } }));
    let mut mcp = launch(&config(&mock, LISTED)).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation_name": "SaveOrder",
            "variables": { "input": { "items": [] } },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let query = sent(&mock);
    assert!(
        query.starts_with("mutation SaveOrder($input: CreateOrderInput!) {"),
        "{query}"
    );
}

#[test]
fn list_operations_and_estimate_cost_see_the_text_without_them() {
    let mut mcp = launch(&config(&MockServer::unreachable(), LISTED)).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "GetOrders" }));
    assert!(!reply.is_error, "{}", reply.text);
    assert!(!reply.text.contains("BROKEN"), "{}", reply.text);
    assert!(
        reply.text.contains("description: A user's orders, a page at a time."),
        "the description is kept: {}",
        reply.text
    );
    assert!(
        reply.text.contains("$full: Boolean! = false") && !reply.text.contains("@defaultValue"),
        "{}",
        reply.text
    );
    for listed in USES {
        assert!(!reply.text.contains(listed), "{listed} is removed: {}", reply.text);
    }

    let reply = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation_name": "GetOrders", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("Valid query `GetOrders`"), "{}", reply.text);
}

#[test]
fn a_directive_that_is_not_listed_is_still_an_error() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, LISTED)).human(Human::Accepts).start();

    let listed = mcp.call("list_operations", &json!({ "endpoint": "store", "name": "Typo" }));
    assert!(listed.text.contains("BROKEN"), "{}", listed.text);
    assert!(
        listed.text.contains("@paginatd"),
        "names the directive: {}",
        listed.text
    );

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "Typo", "variables": { "id": "u-1" } }),
    );
    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("@paginatd"), "{}", reply.text);
    assert!(
        reply.text.contains("`client_directives`"),
        "says how a generator's directive is listed: {}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn without_the_setting_the_directives_break_the_operations() {
    let mut mcp = launch(&config(&MockServer::unreachable(), "")).start();

    for name in ["GetOrders", "SaveOrder"] {
        let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "name": name }));
        assert!(reply.text.contains("BROKEN"), "{}", reply.text);
    }
}

#[test]
fn an_operation_the_agent_writes_is_validated_as_written() {
    let mock = mock();
    let mut mcp = launch(&config(&mock, LISTED)).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query Mine @paginated { viewer { id } }" }),
    );

    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("@paginated"), "{}", reply.text);
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_template_can_hold_the_list() {
    let mock = mock();
    let config = format!(
        "[templates.shop]\nschema = \"schema.graphql\"\n{LISTED}\n\
         [endpoints.store]\ntemplate = \"shop\"\nurl = \"{}\"\noperations = \"queries\"\n\
         fragments = \"queries/fragments\"\n",
        mock.url()
    );
    let mut mcp = launch(&config).human(Human::Accepts).start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "GetOrders", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "{}", reply.text);
    assert!(!sent(&mock).contains("@paginated"));
}

#[test]
fn check_warns_about_a_listed_directive_nothing_uses() {
    let checked = launch(&config(&MockServer::unreachable(), LISTED)).check();

    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    assert!(
        checked.stdout.contains(
            "  client directives: @baseClass, @defaultValue, @idempotent, @immutable, @paginated, @retry, \
             @unordered: warning: `@retry` is used by no saved operation or shared fragment\n"
        ),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.contains("`Typo`") && checked.stdout.contains("@paginatd"),
        "the unlisted directive is a warning on its operation: {}",
        checked.stdout
    );
    assert!(checked.stdout.contains("0 problems, 2 warnings."), "{}", checked.stdout);
}

#[test]
fn check_reports_directives_that_are_all_used_as_ok() {
    let config = config(
        &MockServer::unreachable(),
        "client_directives = [\"paginated\", \"idempotent\", \"baseClass\", \"defaultValue\", \"unordered\", \
         \"immutable\"]\n",
    );
    let checked = Launch::new(&config)
        .file("queries/get_orders.graphql", GET_ORDERS)
        .file("queries/save_order.graphql", SAVE_ORDER)
        .file("queries/fragments/shared.graphql", SHARED)
        .check();

    assert!(checked.passed, "{}", checked.stdout);
    assert!(
        checked.stdout.contains(
            "  client directives: @baseClass, @defaultValue, @idempotent, @immutable, @paginated, @unordered: ok\n"
        ),
        "{}",
        checked.stdout
    );
    assert!(checked.stdout.contains("0 problems, 0 warnings."), "{}", checked.stdout);
}

#[test]
fn a_bad_list_stops_the_server() {
    let mock = MockServer::unreachable();
    let cases = [
        (
            "client_directives = [\"@paginated\"]\n",
            vec!["`client_directives`", "`@paginated`", "without the `@`"],
        ),
        (
            "client_directives = [\"page size\"]\n",
            vec!["`client_directives`", "`page size`", "not a directive name"],
        ),
        (
            "client_directives = [\"include\"]\n",
            vec!["`client_directives`", "`@include`", "the server"],
        ),
    ];
    for (settings, expected) in cases {
        let stderr = launch(&config(&mock, settings)).fails_to_start();
        assert!(stderr.contains("endpoint `store`"), "{stderr}");
        for expected in expected {
            assert!(stderr.contains(expected), "expected {expected:?} in: {stderr}");
        }
    }

    let stderr = Launch::new(&format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{LISTED}",
        mock.url()
    ))
    .fails_to_start();
    assert!(stderr.contains("endpoint `store`"), "{stderr}");
    assert!(
        stderr.contains("`client_directives`") && stderr.contains("`operations`"),
        "{stderr}"
    );
}
