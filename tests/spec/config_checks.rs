//! Slice 21: a config that cannot work stops the server at startup, with a message that says what to change.

use serde_json::json;

use crate::support::{Launch, Mcp};

/// Starts the server with `endpoint` as the body of `[endpoints.store]`, expecting it to refuse.
fn refused(endpoint: &str) -> String {
    Launch::new(&format!("[endpoints.store]\n{endpoint}")).fails_to_start()
}

#[test]
fn a_url_without_a_host_is_refused() {
    let stderr = refused("url = \"https://\"\n");

    assert!(stderr.contains("endpoint `store`"), "names the endpoint: {stderr}");
    assert!(stderr.contains("`url`"), "names the setting: {stderr}");
}

#[test]
fn a_schema_file_that_is_not_there_is_refused() {
    let stderr = refused("url = \"http://localhost:9/graphql\"\nschema = \"missing.graphql\"\n");

    assert!(stderr.contains("missing.graphql"), "names the file: {stderr}");
    assert!(
        stderr.contains("introspection"),
        "says what else the user can do: {stderr}"
    );
}

#[test]
fn an_assumed_list_size_of_zero_is_refused() {
    let stderr = refused("url = \"http://localhost:9/graphql\"\nassumed_list_size = 0\n");

    assert!(
        stderr.contains("`assumed_list_size` must be at least 1"),
        "a list assumed empty would make any query free: {stderr}"
    );
}

#[test]
fn auto_approve_cost_with_client_approval_is_refused() {
    let stderr = refused("url = \"http://localhost:9/graphql\"\napproval = \"client\"\nauto_approve_cost = 5\n");

    assert!(
        stderr.contains("`auto_approve_cost` has no effect"),
        "the two settings contradict each other: {stderr}"
    );
}

#[test]
fn an_endpoint_name_the_agent_could_not_type_is_refused() {
    for name in ["\"\"", "\"a b\"", "\"a\\nurl: https://evil.example\""] {
        let stderr =
            Launch::new(&format!("[endpoints.{name}]\nurl = \"http://localhost:9/graphql\"\n")).fails_to_start();

        assert!(
            stderr.contains("an endpoint name may only have letters, digits"),
            "{name}: says what a name may hold: {stderr}"
        );
    }
}

#[test]
fn a_name_with_dashes_dots_and_underscores_is_accepted() {
    let mut mcp = Mcp::start("[endpoints.\"acme-prod_v2.eu\"]\nurl = \"http://localhost:9/graphql\"\n");

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(reply.text.starts_with("acme-prod_v2.eu\n"), "got: {}", reply.text);
}

#[test]
fn a_path_that_starts_with_a_tilde_is_in_the_home_directory() {
    let home = tempfile::tempdir().expect("create a home directory");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema.graphql"),
        home.path().join("store.graphql"),
    )
    .expect("put a schema in the home directory");
    let mut mcp =
        Launch::new("[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"~/store.graphql\"\n")
            .env("HOME", &home.path().display().to_string())
            .start();

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        reply.text.contains(&format!(
            "schema: file {}\n",
            home.path().join("store.graphql").display()
        )),
        "`~/` is the home directory: {}",
        reply.text
    );
}
