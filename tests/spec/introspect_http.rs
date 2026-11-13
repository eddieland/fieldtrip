//! Slice 5: with no schema file, the schema comes from an introspection query to the endpoint.

use serde_json::json;

use crate::support::{FixtureApi, Mcp, MockServer, Response};

/// Names whose `describe` output must not depend on where the schema came from.
const DESCRIBED: [&str; 10] = [
    "User",
    "Node",
    "SearchResult",
    "OrderStatus",
    "CreateOrderInput",
    "OrderItemInput",
    "DateTime",
    "Query.users",
    "Mutation.createOrder",
    "Mutation.deleteUser",
];

/// Searches whose results must not depend on where the schema came from.
const SEARCHED: [&str; 5] = ["user", "order", "cancel", "warehouse", "e"];

/// A config with two endpoints on `mock`: `live` is introspected, `file` reads the fixture schema.
fn config(mock: &MockServer) -> String {
    format!(
        "[endpoints.live]\nurl = \"{url}\"\n\n[endpoints.file]\nurl = \"{url}\"\nschema = \"schema.graphql\"\n",
        url = mock.url()
    )
}

/// Calls `tool` on `endpoint` and returns the text, failing on a tool error.
fn ok(mcp: &mut Mcp, tool: &str, endpoint: &str, extra: (&str, &str)) -> String {
    let reply = mcp.call(tool, &json!({ "endpoint": endpoint, extra.0: extra.1 }));
    assert!(
        !reply.is_error,
        "{tool} {extra:?} on `{endpoint}` should succeed: {}",
        reply.text
    );
    reply.text
}

/// Asserts that `describe` and `search_schema` answer the same for the introspected and the file schema.
fn assert_same_as_file(mcp: &mut Mcp) {
    for name in DESCRIBED {
        let live = ok(mcp, "describe", "live", ("name", name));
        let file = ok(mcp, "describe", "file", ("name", name));
        assert_eq!(
            live, file,
            "describe {name} differs between introspection and the schema file"
        );
    }
    for query in SEARCHED {
        let live = ok(mcp, "search_schema", "live", ("query", query));
        let file = ok(mcp, "search_schema", "file", ("query", query));
        assert_eq!(
            live.replace("`live`", "`file`"),
            file,
            "search {query:?} differs between the two sources"
        );
    }
}

#[test]
fn loads_the_schema_from_the_endpoint() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock));

    let live = mcp.call("introspect", &json!({ "endpoint": "live" }));
    let file = mcp.call("introspect", &json!({ "endpoint": "file" }));

    assert!(!live.is_error, "introspect should succeed: {}", live.text);
    let (live_source, live_overview) = live.text.split_once('\n').expect("a source line and an overview");
    let (_, file_overview) = file.text.split_once('\n').expect("a source line and an overview");
    assert_eq!(
        live_source,
        format!("Schema for `live`, loaded by introspection from {}.", mock.url()),
        "the first line says where the schema came from"
    );
    assert_eq!(
        live_overview, file_overview,
        "the overview is the same as for the schema file"
    );
}

#[test]
fn every_tool_answers_as_it_does_for_the_schema_file() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock));

    assert_same_as_file(&mut mcp);
}

#[test]
fn asks_once_and_keeps_the_schema() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock));

    ok(&mut mcp, "describe", "live", ("name", "User"));
    ok(&mut mcp, "search_schema", "live", ("query", "order"));
    mcp.call("introspect", &json!({ "endpoint": "live" }));

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one introspection request serves every later call");
    let request = requests.first().expect("one request");
    assert_eq!(request.method, "POST", "GraphQL over HTTP uses POST");
    assert!(
        request.body.contains("__schema"),
        "the request is an introspection query: {}",
        request.body
    );
}

#[test]
fn refresh_asks_the_endpoint_again() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Mcp::start(&config(&mock));

    mcp.call("introspect", &json!({ "endpoint": "live" }));
    mcp.call("introspect", &json!({ "endpoint": "live", "refresh": true }));

    assert_eq!(mock.requests().len(), 2, "refresh sends a second introspection request");
}

#[test]
fn falls_back_for_a_server_that_rejects_newer_introspection_fields() {
    let mock = MockServer::serving(FixtureApi::new().legacy());
    let mut mcp = Mcp::start(&config(&mock));

    assert_same_as_file(&mut mcp);

    assert_eq!(
        mock.requests().len(),
        2,
        "the full query is rejected, then the older query succeeds"
    );
}

#[test]
fn introspection_turned_off_points_at_a_schema_file() {
    let mock = MockServer::serving(FixtureApi::new().without_introspection());
    let mut mcp = Mcp::start(&config(&mock));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(
        reply.is_error,
        "a refused introspection should be an error: {}",
        reply.text
    );
    assert!(
        reply.text.contains("introspection is not allowed"),
        "passes on the server's reason: {}",
        reply.text
    );
    assert!(
        reply.text.contains("`schema`"),
        "says a schema file is the way out: {}",
        reply.text
    );
}

#[test]
fn a_response_that_is_not_graphql_is_an_error_with_the_status() {
    let mock = MockServer::start(|_| Response::new(502, "<html>Bad Gateway</html>"));
    let mut mcp = Mcp::start(&config(&mock));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a gateway error should be an error: {}", reply.text);
    assert!(reply.text.contains("HTTP 502"), "names the status: {}", reply.text);
}

#[test]
fn an_endpoint_that_is_down_is_an_error_that_names_it() {
    // Port 9 is the discard service, which nothing listens on here.
    let mut mcp = Mcp::start("[endpoints.live]\nurl = \"http://127.0.0.1:9/graphql\"\n");

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(
        reply.is_error,
        "a refused connection should be an error: {}",
        reply.text
    );
    assert!(
        reply.text.contains("http://127.0.0.1:9/graphql"),
        "names the endpoint: {}",
        reply.text
    );
}
