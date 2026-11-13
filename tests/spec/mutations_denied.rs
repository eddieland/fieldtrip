//! Slice 29: `mutations = "deny"` makes an endpoint read-only. `execute` refuses every mutation before it asks
//! anyone or sends anything, while queries, `estimate_cost` and `introspect` keep working. A template can set it,
//! and an endpoint that names the template can only make it stricter.

use rmcp::model::CallToolResponse;
use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, SdkClient, sdk_reply};

const QUERY: &str = "{ viewer { name } }";
const MUTATION: &str = "mutation Remove { deleteUser(id: \"u-1\") }";

/// The `store` endpoint with `settings`, sending operations to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    )
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "viewer": null, "deleteUser": true } }))
}

/// The arguments of `execute` or `estimate_cost` for `operation` on `endpoint`.
fn arguments(endpoint: &str, operation: &str) -> Value {
    json!({ "endpoint": endpoint, "operation": operation })
}

/// Asserts that `text` is the refusal for a denied mutation on `endpoint`.
fn assert_refused(text: &str, endpoint: &str) {
    assert!(
        text.contains(&format!("Nothing was sent to `{endpoint}`")),
        "says nothing was sent: {text}"
    );
    assert!(
        text.contains("`mutations = \"deny\"`"),
        "names the setting that refused it: {text}"
    );
    assert!(
        text.contains("`estimate_cost`"),
        "says what the calling agent can still do: {text}"
    );
}

#[test]
fn a_mutation_is_refused_with_no_prompt_and_nothing_sent() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments("store", MUTATION));

    assert!(reply.is_error, "the mutation is refused: {}", reply.text);
    assert_refused(&reply.text, "store");
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn approval_left_to_the_client_does_not_open_writes() {
    let mock = mock();
    let mut mcp = Mcp::start(&config(&mock, "approval = \"client\"\nmutations = \"deny\"\n"));

    let reply = mcp.call("execute", &arguments("store", MUTATION));

    assert!(reply.is_error, "the mutation is refused: {}", reply.text);
    assert_refused(&reply.text, "store");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn a_mutation_is_refused_before_it_is_validated() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments("store", "mutation { noSuchField }"));

    assert!(reply.is_error, "the mutation is refused: {}", reply.text);
    assert_refused(&reply.text, "store");
    assert!(
        !reply.text.contains("noSuchField"),
        "the refusal comes first, so it does not read as a fixable validation error: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn a_document_with_a_mutation_among_other_operations_is_refused() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();
    let document = format!("query Read {QUERY}\n{MUTATION}");

    let reply = mcp.call("execute", &arguments("store", &document));

    assert!(reply.is_error, "the document is refused: {}", reply.text);
    assert_refused(&reply.text, "store");
    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn queries_are_unaffected() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments("store", QUERY));

    assert!(!reply.is_error, "the query runs: {}", reply.text);
    assert_eq!(mcp.prompts().len(), 1, "the query is approved as usual");
    assert_eq!(mock.requests().len(), 1, "the query is sent");
}

#[test]
fn a_mutation_is_refused_on_revision_2026_07_28_without_asking_for_input() {
    let mock = mock();
    let client = SdkClient::start(&Launch::new(&config(&mock, "mutations = \"deny\"\n")).human(Human::Accepts));

    let response = client.call_once("execute", &arguments("store", MUTATION), None);

    let CallToolResponse::Complete(result) = response else {
        panic!("a denied mutation finishes the call instead of asking for input: {response:?}");
    };
    let reply = sdk_reply(&result);
    assert!(reply.is_error, "the mutation is refused: {}", reply.text);
    assert_refused(&reply.text, "store");
    assert_eq!(client.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");

    let query = client.call("execute", &arguments("store", QUERY));
    assert!(!query.is_error, "a query still runs: {}", query.text);
    assert_eq!(mock.requests().len(), 1, "the query is sent");
}

#[test]
fn estimate_cost_still_works_and_says_execute_will_refuse() {
    let mock = mock();
    let mut mcp = Mcp::start(&config(&mock, "mutations = \"deny\"\n"));

    let reply = mcp.call("estimate_cost", &arguments("store", MUTATION));

    assert!(!reply.is_error, "the mutation is estimated: {}", reply.text);
    assert!(
        reply
            .text
            .starts_with("Valid mutation `Remove` for `store`.\nEstimated cost: 10\n"),
        "the estimate is as usual: {}",
        reply.text
    );
    assert!(
        reply.text.contains("`execute` will refuse it") && reply.text.contains("`mutations = \"deny\"`"),
        "says that it cannot be executed here, and why: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "estimating sends nothing");

    let query = mcp.call("estimate_cost", &arguments("store", QUERY));
    assert!(
        !query.text.contains("refuse"),
        "a query carries no such note: {}",
        query.text
    );
}

#[test]
fn introspect_lists_mutations_and_marks_them_unavailable() {
    let mock = MockServer::unreachable();
    let mut mcp = Mcp::start(&config(&mock, "mutations = \"deny\"\n"));

    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));

    assert!(!reply.is_error, "introspect works: {}", reply.text);
    assert!(
        reply.text.contains(
            "Mutation (4 fields): createOrder, cancelOrder, deleteUser (deprecated), deactivateUser\n\
             Mutations are denied on this endpoint by `mutations = \"deny\"`: they can be described and estimated, \
             but `execute` refuses them.\n"
        ),
        "the mutation names are listed and marked unavailable: {}",
        reply.text
    );
}

#[test]
fn list_endpoints_shows_which_endpoints_deny_mutations() {
    let mock = mock();
    let config = format!(
        "[endpoints.read]\nurl = \"{url}\"\nschema = \"schema.graphql\"\nmutations = \"deny\"\n\n\
         [endpoints.write]\nurl = \"{url}\"\nschema = \"schema.graphql\"\nmutations = \"prompt\"\n",
        url = mock.url()
    );
    let mut mcp = Mcp::start(&config);

    let text = mcp.call("list_endpoints", &json!({})).text;

    let (read, write) = text.split_once("write\n").expect("both endpoints are listed");
    assert!(
        read.contains("  mutations: denied\n"),
        "the read-only endpoint:\n{text}"
    );
    assert!(!write.contains("mutations"), "the default needs no line:\n{text}");
}

#[test]
fn fieldtrip_check_shows_mutations_denied() {
    let mock = MockServer::unreachable();
    let checked = Launch::new(&config(&mock, "mutations = \"deny\"\n")).check();

    assert!(
        checked.passed,
        "the config is fine: {}{}",
        checked.stdout, checked.stderr
    );
    assert!(
        checked
            .stdout
            .contains("  approval: the user is asked before every operation\n  mutations: denied\n  timeout: 30s\n"),
        "the setting is shown next to approval:\n{}",
        checked.stdout
    );
}

/// The template `store` with `template`, and the endpoint `store-prod` naming it with `endpoint`.
fn templated(mock: &MockServer, template: &str, endpoint: &str) -> String {
    format!(
        "[templates.store]\n{template}\n\
         [endpoints.store-prod]\ntemplate = \"store\"\nurl = \"{}\"\nschema = \"schema.graphql\"\n{endpoint}",
        mock.url()
    )
}

#[test]
fn an_endpoint_takes_deny_from_its_template() {
    let mock = mock();
    let mut mcp = Launch::new(&templated(&mock, "mutations = \"deny\"\n", "approval = \"client\"\n"))
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments("store-prod", MUTATION));

    assert!(reply.is_error, "the template's setting applies: {}", reply.text);
    assert_refused(&reply.text, "store-prod");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn an_endpoint_cannot_loosen_its_templates_deny() {
    let mock = mock();

    let stderr = Launch::new(&templated(&mock, "mutations = \"deny\"\n", "mutations = \"prompt\"\n")).fails_to_start();

    assert!(stderr.contains("endpoint `store-prod`"), "names the endpoint: {stderr}");
    assert!(stderr.contains("template `store`"), "names the template: {stderr}");
    assert!(
        stderr.contains("`mutations = \"prompt\"`") && stderr.contains("`mutations = \"deny\"`"),
        "names both settings: {stderr}"
    );
}

#[test]
fn an_endpoint_can_deny_what_its_template_allows() {
    let mock = mock();
    let mut mcp = Launch::new(&templated(&mock, "mutations = \"prompt\"\n", "mutations = \"deny\"\n"))
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments("store-prod", MUTATION));

    assert!(
        reply.is_error,
        "the endpoint's stricter setting applies: {}",
        reply.text
    );
    assert_refused(&reply.text, "store-prod");
    assert_eq!(mock.requests().len(), 0, "nothing reaches the endpoint");
}

#[test]
fn an_unknown_setting_is_refused_at_startup() {
    let mock = mock();

    let stderr = Launch::new(&config(&mock, "mutations = \"never\"\n")).fails_to_start();

    assert!(stderr.contains("`mutations`"), "names the setting: {stderr}");
    assert!(
        stderr.contains("\"deny\"") && stderr.contains("\"prompt\""),
        "says what it can be: {stderr}"
    );
}
