//! Slice 55: `read_result` returns part of a response that `execute` wrote to a file, by a `select` path, and sends
//! nothing to the endpoint. It reads only the files fieldtrip wrote.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply};

/// A response with 50 issues whose bodies are 400 characters long, and one error.
fn issues() -> Value {
    let nodes: Vec<Value> = (0..50)
        .map(|number| json!({ "number": number, "body": "b".repeat(400) }))
        .collect();
    json!({
        "data": { "repository": { "issues": { "nodes": nodes } } },
        "errors": [{ "message": "issue 50 could not be loaded", "path": ["repository", "issues", "nodes", 50] }],
    })
}

/// The config of a `store` endpoint that answers from `mock` and never prompts.
fn config(mock: &MockServer) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        mock.url()
    )
}

/// Starts a server, executes one query whose response goes to a file, and returns the file and its result ID.
fn executed(mock: &MockServer) -> (Mcp, PathBuf, String) {
    let mut mcp = Launch::new(&config(mock)).human(Human::Absent).start();
    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { id title } }" }),
    );
    let path = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("Saved the whole response, "))
        .and_then(|line| line.split_once(" bytes, to "))
        .map_or_else(
            || panic!("the response went to a file: {}", reply.text),
            |(_, path)| PathBuf::from(path),
        );
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("the file has a name")
        .to_owned();
    assert!(
        reply.text.contains(&format!("`read_result` with `result: \"{id}\"`")),
        "`execute` says how to read the rest: {}",
        reply.text
    );
    (mcp, path, id)
}

/// Calls `read_result`.
fn read(mcp: &mut Mcp, result: &str, select: Option<&str>) -> Reply {
    let mut arguments = json!({ "result": result });
    if let Some(select) = select {
        arguments["select"] = json!(select);
    }
    mcp.call("read_result", &arguments)
}

/// The JSON on the line of `reply` that starts with `name: `.
fn part(reply: &Reply, name: &str) -> Value {
    let line = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
        .unwrap_or_else(|| panic!("there is a `{name}` line: {}", reply.text));
    serde_json::from_str(line).unwrap_or_else(|error| panic!("valid JSON ({error}): {line}"))
}

#[test]
fn a_path_into_a_saved_result_comes_back_without_a_request() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, _path, id) = executed(&mock);

    let reply = read(&mut mcp, &id, Some("data.repository.issues.nodes[*].number"));

    assert!(!reply.is_error, "{}", reply.text);
    let numbers: Vec<Value> = (0..50).map(Value::from).collect();
    assert_eq!(part(&reply, "selected"), json!(numbers));
    assert!(reply.text.contains("Nothing was sent"), "{}", reply.text);
    assert_eq!(mock.requests().len(), 1, "only `execute` reached the endpoint");
}

#[test]
fn the_path_execute_printed_names_the_result_too() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, path, _id) = executed(&mock);

    let reply = read(
        &mut mcp,
        &path.display().to_string(),
        Some("data.repository.issues.nodes[7].number"),
    );

    assert_eq!(part(&reply, "selected"), json!(7), "{}", reply.text);
}

#[test]
fn errors_come_whole_with_any_selection() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, _path, id) = executed(&mock);

    let reply = read(&mut mcp, &id, Some("data.repository.issues.nodes[0].number"));

    assert_eq!(part(&reply, "errors"), issues()["errors"]);
    let lines: Vec<&str> = reply.text.lines().collect();
    let errors = lines.iter().position(|line| line.starts_with("errors: "));
    let selected = lines.iter().position(|line| line.starts_with("selected: "));
    assert!(errors < selected, "errors come first: {lines:#?}");
}

#[test]
fn without_a_path_it_gives_the_preview_and_the_shape_again() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, _path, id) = executed(&mock);

    let reply = read(&mut mcp, &id, None);

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.chars().count() < 5_000, "{}", reply.text.chars().count());
    assert!(
        reply
            .text
            .contains("\ndata.repository.issues.nodes[50].body: string (400 chars)"),
        "{}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn a_selection_too_long_for_a_result_is_cut_and_says_so() {
    let nodes: Vec<Value> = (0..400).map(|_| json!({ "body": "b".repeat(400) })).collect();
    let mock = MockServer::answering(&json!({ "data": { "nodes": nodes } }));
    let (mut mcp, _path, id) = executed(&mock);

    let reply = read(&mut mcp, &id, Some("data.nodes[*].body"));

    assert!(reply.text.chars().count() <= 40_000, "{}", reply.text.chars().count());
    let selected = part(&reply, "selected");
    let list = selected.as_array().expect("a list");
    assert!(list.len() > 50, "far more than a preview holds: {}", list.len());
    assert!(
        list.last()
            .and_then(Value::as_str)
            .is_some_and(|marker| marker.ends_with("more items)")),
        "the cut is marked"
    );
    let note = reply.text.lines().last().unwrap_or_default();
    assert!(
        note.contains("`max_output_chars`") && note.contains("`select`"),
        "{note}"
    );
}

#[test]
fn a_path_that_misses_says_what_is_there() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, _path, id) = executed(&mock);

    let missed = read(&mut mcp, &id, Some("data.repository.pullRequests"));
    let invalid = read(&mut mcp, &id, Some("data..nodes"));

    assert!(
        missed
            .text
            .contains("`data.repository` has no `pullRequests`. It has `issues`."),
        "{}",
        missed.text
    );
    assert!(
        invalid.is_error && invalid.text.contains("is not a path"),
        "{}",
        invalid.text
    );
}

#[test]
fn only_files_fieldtrip_wrote_can_be_read() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, path, id) = executed(&mock);
    let outside = mcp.dir().join("private.json");
    std::fs::write(&outside, r#"{"data":{"password":"hunter2-hunter2"}}"#).expect("write a file");
    let climbing = path
        .parent()
        .expect("a directory")
        .join("../../../..")
        .join("private.json");

    for wanted in [
        outside.display().to_string(),
        climbing.display().to_string(),
        "../../../../private".to_owned(),
        "private.json".to_owned(),
    ] {
        let reply = read(&mut mcp, &wanted, Some("data"));

        assert!(reply.is_error, "`{wanted}` is refused: {}", reply.text);
        assert!(!reply.text.contains("hunter2"), "{}", reply.text);
    }
    let unknown = read(&mut mcp, "store-000000000000", Some("data"));
    assert!(unknown.is_error, "{}", unknown.text);
    assert!(
        unknown.text.contains(&format!("`{id}`")),
        "an unknown ID is answered with the results there are: {}",
        unknown.text
    );
}

#[test]
fn a_result_from_an_earlier_session_can_be_read_by_its_id() {
    let mock = MockServer::answering(&issues());
    let (earlier, _path, id) = executed(&mock);
    let cache = earlier.dir().join("cache-home");
    let mut later = Launch::new(&config(&mock))
        .env("XDG_CACHE_HOME", &cache.display().to_string())
        .start();

    let reply = read(&mut later, &id, Some("data.repository.issues.nodes[49].number"));

    assert_eq!(part(&reply, "selected"), json!(49), "{}", reply.text);
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn a_small_response_saved_on_request_can_be_read() {
    let mock = MockServer::answering(&json!({ "data": { "products": [{ "id": "p-1" }] } }));
    let mut mcp = Launch::new(&config(&mock)).start();
    let saved = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { id } }", "save": true }),
    );
    let id = saved
        .text
        .split_once("`result: \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or_else(
            || panic!("the result has an ID: {}", saved.text),
            |(id, _)| id.to_owned(),
        );

    let reply = read(&mut mcp, &id, Some("data.products[0].id"));

    assert_eq!(part(&reply, "selected"), json!("p-1"), "{}", reply.text);
}

#[test]
fn a_file_that_is_not_json_says_so() {
    let mock = MockServer::answering(&issues());
    let (mut mcp, path, id) = executed(&mock);
    std::fs::write(&path, "<html>Bad gateway</html>").expect("replace the file");

    let reply = read(&mut mcp, &id, Some("data"));

    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("not JSON") && reply.text.contains("Bad gateway"),
        "{}",
        reply.text
    );
}
