//! Slice 1: the server starts with `--config` and lists the endpoints.

use serde_json::json;

use crate::support::Mcp;

const CONFIG: &str = r#"
[endpoints.acme]
url    = "https://api.acme.example/graphql"
schema = "schema.graphql"

[endpoints.acme.headers]
X-Client      = "sekrit-literal-value"
Authorization = { command = ["token-helper"], format = "Bearer {}" }

[endpoints.public]
url = "http://localhost:9/graphql"
"#;

#[test]
fn the_tool_is_offered() {
    let mut mcp = Mcp::start(CONFIG);

    assert!(
        mcp.tools().contains(&"list_endpoints".to_owned()),
        "list_endpoints should be in tools/list"
    );
}

#[test]
fn lists_each_endpoint_with_its_url_and_schema_source() {
    let mut mcp = Mcp::start(CONFIG);
    let schema_path = mcp.dir().join("schema.graphql");

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(!reply.is_error, "list_endpoints should succeed: {}", reply.text);
    let expected = format!(
        "acme\n  \
           url: https://api.acme.example/graphql\n  \
           schema: file {}\n  \
           approval: the user is asked before every operation\n  \
           headers: Authorization, X-Client\n  \
           header Authorization: command not run yet\n\
         public\n  \
           url: http://localhost:9/graphql\n  \
           schema: introspection\n  \
           approval: the user is asked before every operation\n",
        schema_path.display()
    );
    assert_eq!(
        reply.text, expected,
        "endpoints are listed in name order, with a relative schema path resolved"
    );
}

#[test]
fn shows_header_names_but_never_values() {
    let mut mcp = Mcp::start(CONFIG);

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        reply.text.contains("X-Client"),
        "header names are shown: {}",
        reply.text
    );
    assert!(
        !reply.text.contains("sekrit-literal-value"),
        "a literal header value leaked: {}",
        reply.text
    );
    assert!(
        !reply.text.contains("token-helper"),
        "a header command leaked: {}",
        reply.text
    );
    assert!(!reply.text.contains("Bearer"), "a header format leaked: {}", reply.text);
}

#[test]
fn says_so_when_nothing_is_configured() {
    let mut mcp = Mcp::start("");

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        reply.text.contains("No endpoints are configured"),
        "got: {}",
        reply.text
    );
}
