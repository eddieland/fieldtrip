//! Slice 35: when a connection in the response says `hasNextPage: true`, the result says so, with its path and
//! cursor, so the calling agent pages through it instead of asking for a bigger page.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply};

/// Starts a server whose `store` endpoint answers every request with `body`, and never prompts.
fn start(body: &Value) -> (Mcp, MockServer) {
    let mock = MockServer::answering(body);
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        mock.url()
    );
    (Launch::new(&config).human(Human::Absent).start(), mock)
}

/// Executes a query on `store`, with `select` when one is given.
fn execute(mcp: &mut Mcp, select: Option<&str>) -> Reply {
    let mut arguments = json!({ "endpoint": "store", "operation": "{ products { id } }" });
    if let Some(select) = select {
        arguments["select"] = json!(select);
    }
    mcp.call("execute", &arguments)
}

/// The lines of a result that point out more pages.
fn hints(reply: &Reply) -> Vec<&str> {
    reply
        .text
        .lines()
        .filter(|line| line.starts_with("More pages: "))
        .collect()
}

/// A connection with two nodes, which has more pages when `more` is set.
fn connection(more: bool, cursor: &str) -> Value {
    json!({
        "nodes": [{ "number": 1 }, { "number": 2 }],
        "pageInfo": { "hasNextPage": more, "endCursor": cursor },
    })
}

#[test]
fn a_connection_with_more_pages_is_pointed_out_with_its_cursor() {
    let body = json!({ "data": { "repository": { "pullRequests": connection(true, "Y3Vyc29yOjI=") } } });
    let (mut mcp, _mock) = start(&body);

    let reply = execute(&mut mcp, None);

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(
        hints(&reply),
        [
            "More pages: `data.repository.pullRequests` has `hasNextPage: true`. Its `endCursor` is \"Y3Vyc29yOjI=\". \
             To get the rest, pass the cursor as `after` with the same page size, rather than raising `first`."
        ]
    );
    assert_eq!(
        reply.text.lines().last(),
        Some(format!("data: {}", body["data"]).as_str()),
        "the data is still there as written, after the hint"
    );
}

#[test]
fn a_last_page_is_not_pointed_out() {
    let body = json!({ "data": { "repository": { "pullRequests": connection(false, "Y3Vyc29yOjI=") } } });
    let (mut mcp, _mock) = start(&body);

    let reply = execute(&mut mcp, None);

    assert!(hints(&reply).is_empty(), "{}", reply.text);
}

#[test]
fn a_connection_inside_a_list_is_counted_across_its_items() {
    let repositories: Vec<Value> = (0..5)
        .map(|index| json!({ "name": format!("r-{index}"), "issues": connection(index % 2 == 0, "c") }))
        .collect();
    let body = json!({ "data": { "viewer": { "repositories": { "nodes": repositories } } } });
    let (mut mcp, _mock) = start(&body);

    let reply = execute(&mut mcp, None);

    assert_eq!(
        hints(&reply),
        [
            "More pages: `data.viewer.repositories.nodes[*].issues` has `hasNextPage: true` in 3 of its 5 \
             appearances. Page through each with `after` and its `endCursor`, rather than raising `first`."
        ]
    );
}

#[test]
fn many_connections_are_pointed_out_up_to_a_limit() {
    let mut data = serde_json::Map::new();
    for index in 0..8 {
        data.insert(format!("list{index}"), connection(true, "c"));
    }
    let (mut mcp, _mock) = start(&json!({ "data": data }));

    let reply = execute(&mut mcp, None);

    let hints = hints(&reply);
    assert_eq!(hints.len(), 6, "{hints:#?}");
    assert_eq!(
        hints.last().copied(),
        Some("More pages: 3 more connections have `hasNextPage: true`.")
    );
}

#[test]
fn hints_come_with_a_selection_and_with_a_summary() {
    let title = "t".repeat(1000);
    let nodes: Vec<Value> = (0..100)
        .map(|number| json!({ "number": number, "title": title }))
        .collect();
    let body = json!({
        "data": { "repository": { "pullRequests": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": true, "endCursor": "abc" },
        } } },
    });
    let (mut mcp, _mock) = start(&body);

    let summarized = execute(&mut mcp, None);
    let selected = execute(&mut mcp, Some("data.repository.pullRequests.nodes[*].number"));

    for reply in [&summarized, &selected] {
        let lines: Vec<&str> = reply.text.lines().collect();
        assert!(
            lines
                .get(2)
                .is_some_and(|line| line.starts_with("More pages: `data.repository.pullRequests`")),
            "the hint comes right after the status line: {lines:#?}"
        );
    }
    assert!(summarized.text.contains("this is a summary"), "{}", summarized.text);
}
