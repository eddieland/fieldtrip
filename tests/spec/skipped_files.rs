//! Slice 46: a bad file in `operations` or `fragments` is left out and reported, and the rest load. Every file that
//! shares an operation name is left out, as is every operation that spreads a fragment defined twice. A missing
//! directory still stops the server.

use serde_json::{Value, json};

use crate::support::{Human, Launch, MockServer, store_config};

/// A good saved query, which spreads a good shared fragment.
const USER: &str = "query UserById($id: ID!) {\n  user(id: $id) { name ...Contact }\n}\n";

/// A query saved twice under one name, the second time by copying the file.
const VIEWER: &str = "query Viewer { viewer { name } }\n";

/// A saved query that spreads a fragment two files define.
const CLASH: &str = "query UsesClash { viewer { ...Clashing } }\n";

/// The `store` endpoint, sending operations to `mock`.
fn config(mock: &MockServer) -> String {
    store_config(
        mock,
        "operations = \"operations\"\n\
         fragments = \"fragments\"\n",
    )
}

/// A launch of `config` with one good operation and one bad file of each kind.
fn launch(config: &str) -> Launch {
    Launch::new(config)
        .file("operations/user.graphql", USER)
        .file("operations/viewer.graphql", VIEWER)
        .file("operations/viewer_copy.graphql", VIEWER)
        .file("operations/anonymous.graphql", "{ viewer { name } }\n")
        .file("operations/unfinished.graphql", "query Unfinished {")
        .file("operations/clash.graphql", CLASH)
        .file("fragments/contact.graphql", "fragment Contact on User { email }\n")
        .file("fragments/a.graphql", "fragment Clashing on User { name }\n")
        .file("fragments/b.graphql", "fragment Clashing on User { id }\n")
        .file("fragments/query.graphql", "query Stray { viewer { name } }\n")
}

/// A mock that answers any operation with some data.
fn mock() -> MockServer {
    MockServer::answering(&json!({ "data": { "user": { "name": "Ada", "email": null } } }))
}

#[test]
fn the_good_operations_load_and_the_bad_files_are_listed() {
    let mut mcp = launch(&config(&MockServer::unreachable())).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store" }));

    reply.succeeded();
    let text = &reply.text;
    assert!(text.starts_with("1 saved operation for `store`"), "{text}");
    assert!(text.contains("UserById (query)"), "{text}");
    assert!(!text.contains("BROKEN"), "{text}");
    let left_out = text.split("Left out").nth(1).expect("a section of files left out");
    for expected in [
        "the operation name `Viewer` is used in both ",
        "viewer.graphql",
        "viewer_copy.graphql",
        "anonymous.graphql has no name",
        "unfinished.graphql is not valid GraphQL",
        "clash.graphql spreads the shared fragment `Clashing`",
        "the fragment name `Clashing` is defined in both ",
        "query.graphql holds a definition that is not a fragment",
    ] {
        assert!(left_out.contains(expected), "expected {expected:?} in: {left_out}");
    }
}

#[test]
fn a_filtered_list_points_to_the_files_left_out() {
    let mut mcp = launch(&config(&MockServer::unreachable())).start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store", "query": "user" }));

    reply.succeeded();
    assert!(reply.text.contains("UserById"), "{}", reply.text);
    assert!(
        reply
            .text
            .contains("5 operation files and 3 shared fragment files were left out"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("anonymous.graphql"), "{}", reply.text);
}

#[test]
fn a_name_two_files_share_runs_neither() {
    let mock = mock();
    let mut mcp = launch(&config(&mock)).human(Human::Accepts).start();

    let reply = mcp.call("execute", &json!({ "endpoint": "store", "operation_name": "Viewer" }));
    reply.failed();
    assert!(reply.text.contains("viewer.graphql"), "{}", reply.text);
    assert!(reply.text.contains("viewer_copy.graphql"), "{}", reply.text);

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UsesClash" }),
    );
    reply.failed();
    assert!(reply.text.contains("`Clashing`"), "{}", reply.text);

    assert_eq!(mcp.prompts().len(), 0, "the human is not asked");
    assert_eq!(mock.requests().len(), 0, "nothing is sent");

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation_name": "UserById", "variables": { "id": "u-1" } }),
    );
    assert!(!reply.is_error, "the good operation still runs: {}", reply.text);
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(&requests[0].body).expect("a JSON body");
    assert!(
        body["query"]
            .as_str()
            .is_some_and(|query| query.contains("fragment Contact on User")),
        "{body}"
    );
}

#[test]
fn list_endpoints_counts_the_files_left_out() {
    let mut mcp = launch(&config(&MockServer::unreachable())).start();

    let reply = mcp.call("list_endpoints", &json!({}));
    assert!(
        reply.text.contains("saved operations: 1 in ")
            && reply.text.contains(", 5 files left out (see `list_operations`)"),
        "{}",
        reply.text
    );
}

#[test]
fn check_warns_about_each_file_left_out() {
    let checked = launch(&config(&MockServer::unreachable())).check();

    assert!(
        checked.passed,
        "a file left out is a warning: {}{}",
        checked.stdout, checked.stderr
    );
    let operations = checked.dir.path().join("operations");
    let fragments = checked.dir.path().join("fragments");
    let stdout = &checked.stdout;
    assert!(
        stdout.contains(&format!(
            "  operations: {} (1 operation): warning: the saved operation {} has no name",
            operations.display(),
            operations.join("anonymous.graphql").display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "  fragments: {} (1 fragment): warning: the shared fragment file {} holds a definition",
            fragments.display(),
            fragments.join("query.graphql").display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("0 problems, 6 warnings."), "{stdout}");
}

#[test]
fn startup_logs_each_file_left_out() {
    let mcp = launch(&config(&MockServer::unreachable())).start();

    let stderr = mcp.finish();
    for expected in [
        "viewer_copy.graphql",
        "anonymous.graphql",
        "unfinished.graphql",
        "query.graphql",
    ] {
        assert!(stderr.contains(expected), "expected {expected:?} in: {stderr}");
    }
    assert!(stderr.contains("endpoint `store`"), "{stderr}");
}

#[test]
fn a_library_of_only_bad_files_still_lists_them() {
    let config = store_config(&MockServer::unreachable(), "operations = \"operations\"\n");
    let mut mcp = Launch::new(&config)
        .file("operations/anonymous.graphql", "{ viewer { name } }\n")
        .start();

    let reply = mcp.call("list_operations", &json!({ "endpoint": "store" }));
    reply.succeeded();
    assert!(
        reply.text.starts_with("0 saved operations for `store`"),
        "{}",
        reply.text
    );
    assert!(reply.text.contains("anonymous.graphql has no name"), "{}", reply.text);
}
