//! Slice 33: `select` picks part of a response after it arrives. `errors` still comes first and in full. A path that
//! is not valid stops the call before anything is sent, and a path never changes the request or the prompt.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply};

/// A response with a viewer, three products and one error, written with `errors` last.
fn response() -> Value {
    json!({
        "data": {
            "viewer": { "name": "Ada" },
            "products": [
                { "id": "p-0", "title": "Lamp" },
                { "id": "p-1", "title": "Desk" },
                { "id": "p-2", "title": null },
            ],
        },
        "errors": [{ "message": "Title unavailable", "path": ["products", 2, "title"] }],
    })
}

/// Starts a server whose `store` endpoint answers with `body` and asks a human who accepts.
fn start(mock: &MockServer) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    Launch::new(&config).human(Human::Accepts).start()
}

/// Executes the fixture query with `select`.
fn execute(mcp: &mut Mcp, select: &str) -> Reply {
    mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": "query Shop { viewer { name } products { id title } }",
            "select": select,
        }),
    )
}

/// The lines of a result after the summary and status lines, apart from its parts, and the parts as one object.
fn lines_and_json(reply: &Reply) -> (Vec<&str>, Value) {
    let mut lines = Vec::new();
    let mut parts = serde_json::Map::new();
    for line in reply.text.lines().skip(2) {
        let part = line
            .split_once(": ")
            .filter(|(name, _)| ["errors", "selected"].contains(name));
        match part {
            Some((name, json)) => {
                let value = serde_json::from_str(json).unwrap_or_else(|error| panic!("valid JSON ({error}): {json}"));
                parts.insert(name.to_owned(), value);
            }
            None => lines.push(line),
        }
    }
    (lines, Value::Object(parts))
}

#[test]
fn a_path_picks_one_value_and_errors_come_first() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    let reply = execute(&mut mcp, "data.viewer.name");

    assert!(!reply.is_error, "{}", reply.text);
    let (lines, json) = lines_and_json(&reply);
    assert_eq!(lines, ["Selected `data.viewer.name`."]);
    assert_eq!(json, json!({ "errors": response()["errors"], "selected": "Ada" }));
    let order: Vec<&str> = reply
        .text
        .lines()
        .skip(3)
        .filter_map(|line| line.split(':').next())
        .collect();
    assert_eq!(order, ["errors", "selected"], "errors come first: {}", reply.text);
}

#[test]
fn a_wildcard_gives_a_list_of_every_match() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    let ids = execute(&mut mcp, "data.products[*].id");
    let one = execute(&mut mcp, "data.products[1].title");

    let (lines, json) = lines_and_json(&ids);
    assert_eq!(lines, ["Selected `data.products[*].id`: 3 matches, as a list."]);
    assert_eq!(json["selected"], json!(["p-0", "p-1", "p-2"]));
    let (_, json) = lines_and_json(&one);
    assert_eq!(json["selected"], "Desk");
}

#[test]
fn a_path_into_errors_does_not_repeat_them() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    let reply = execute(&mut mcp, "errors[*].message");

    let (_, json) = lines_and_json(&reply);
    assert_eq!(json, json!({ "selected": ["Title unavailable"] }));
}

#[test]
fn a_path_that_misses_says_what_is_there() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    let misspelled = execute(&mut mcp, "data.viewer.nmae");
    let too_far = execute(&mut mcp, "data.products[7].id");

    assert!(
        !misspelled.is_error,
        "the operation ran and has data, so it is a result: {}",
        misspelled.text
    );
    let (lines, json) = lines_and_json(&misspelled);
    assert_eq!(
        lines,
        ["`select` matched nothing: `data.viewer` has no `nmae`. It has `name`."]
    );
    assert_eq!(
        json,
        json!({ "errors": response()["errors"] }),
        "errors are still shown"
    );
    let (lines, _) = lines_and_json(&too_far);
    assert_eq!(
        lines,
        ["`select` matched nothing: `data.products` has 3 items, so there is no `[7]`."]
    );
}

#[test]
fn a_path_that_is_not_valid_sends_nothing_and_asks_nothing() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    for path in [
        "",
        "data..viewer",
        "data.products[x]",
        "data.products[*",
        "data.$name",
        "[0].data",
    ] {
        let reply = execute(&mut mcp, path);

        assert!(reply.is_error, "{path:?} is not a path: {}", reply.text);
        assert!(
            reply.text.contains("`select` is not a path") && reply.text.contains("Nothing was sent"),
            "{path:?}: {}",
            reply.text
        );
    }
    assert_eq!(mock.requests().len(), 0, "nothing reached the endpoint");
    assert!(mcp.prompts().is_empty(), "the user was not asked");
}

#[test]
fn select_changes_neither_the_request_nor_the_prompt() {
    let mock = MockServer::answering(&response());
    let mut mcp = start(&mock);

    let _plain = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query Shop { viewer { name } products { id title } }" }),
    );
    let _selected = execute(&mut mcp, "data.viewer.name");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests.first().map(|request| &request.body),
        requests.get(1).map(|request| &request.body),
        "the same request is sent"
    );
    let prompts = mcp.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts.first(), prompts.get(1), "the user sees the same prompt");
}

#[test]
fn a_large_selection_is_summarized() {
    let title = "t".repeat(200);
    let products: Vec<Value> = (0..1000)
        .map(|index| json!({ "id": format!("p-{index}"), "title": title }))
        .collect();
    let mock = MockServer::answering(&json!({ "data": { "products": products } }));
    let mut mcp = start(&mock);

    let reply = execute(&mut mcp, "data.products[*].title");

    assert!(reply.text.chars().count() <= 40_000, "{}", reply.text.chars().count());
    let (lines, json) = lines_and_json(&reply);
    assert_eq!(
        lines.first().copied(),
        Some("Selected `data.products[*].title`: 1000 matches, as a list.")
    );
    assert!(
        lines.last().is_some_and(|note| note.contains("this is a summary")),
        "{lines:?}"
    );
    let selected = json["selected"].as_array().expect("a list");
    assert!(
        selected
            .last()
            .and_then(Value::as_str)
            .is_some_and(|marker| marker.ends_with("more items)")),
        "the list says how many were dropped"
    );
}
