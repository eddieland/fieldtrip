//! Slice 34: `save` writes the whole response to a new file that fieldtrip names, in its cache directory, and the
//! result gives the path, the size and a short preview instead of the response.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply, Response};

/// Starts a server whose `store` endpoint answers from `mock` and asks `human`, with `settings`.
fn start(mock: &MockServer, human: Human, settings: &str) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    );
    Launch::new(&config)
        .human(human)
        .env("FIELDTRIP_SPEC_KEY", "env-s3cret-value")
        .start()
}

/// Executes the fixture query with `save`, and with `select` when one is given.
fn execute(mcp: &mut Mcp, select: Option<&str>) -> Reply {
    let mut arguments = json!({ "endpoint": "store", "operation": "{ products { id title } }", "save": true });
    if let Some(select) = select {
        arguments["select"] = json!(select);
    }
    mcp.call("execute", &arguments)
}

/// Where the server saves responses, inside the test's directory.
fn responses(mcp: &Mcp) -> PathBuf {
    mcp.dir().join("cache-home").join("fieldtrip").join("responses")
}

/// The path and size the result says the response was saved at.
fn saved(reply: &Reply) -> (PathBuf, usize) {
    let line = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("Saved the whole response, "))
        .unwrap_or_else(|| panic!("the result says where it saved the response: {}", reply.text));
    let (size, path) = line
        .split_once(" bytes, to ")
        .expect("the line gives a size and a path");
    (PathBuf::from(path), size.parse().expect("the size is a number"))
}

/// A response with `count` products whose titles are 1,000 characters long.
fn large(count: usize) -> String {
    let title = "t".repeat(1000);
    let products: Vec<Value> = (0..count)
        .map(|index| json!({ "id": format!("p-{index}"), "title": title }))
        .collect();
    json!({ "data": { "products": products } }).to_string()
}

#[test]
fn a_large_response_is_saved_whole_and_previewed() {
    let body = large(5000);
    let served = body.clone();
    let mock = MockServer::start(move |_| Response::new(200, served.clone()));
    let mut mcp = start(&mock, Human::Accepts, "");

    let reply = execute(&mut mcp, None);

    assert!(!reply.is_error, "{}", reply.text.get(..300).unwrap_or_default());
    let (path, size) = saved(&reply);
    assert_eq!(size, body.len(), "the size is the body's");
    assert_eq!(
        path.parent(),
        Some(responses(&mcp).as_path()),
        "it is in fieldtrip's cache directory"
    );
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("store-")),
        "the file is named after the endpoint: {}",
        path.display()
    );
    assert_eq!(path.extension().and_then(|extension| extension.to_str()), Some("json"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the saved file"),
        body,
        "the file holds the response as the server wrote it"
    );
    assert!(
        reply.text.chars().count() <= 4_000,
        "the result is only a preview: {}",
        reply.text.chars().count()
    );
    let note = reply.text.lines().last().unwrap_or_default();
    assert!(
        note.contains("shown of a saved response") && note.contains("The file has all of it"),
        "{note}"
    );
    let data = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap_or_default();
    let preview: Value = serde_json::from_str(data).expect("the preview is valid JSON");
    assert!(
        preview["products"]
            .as_array()
            .and_then(|list| list.last())
            .and_then(Value::as_str)
            .is_some_and(|marker| marker.ends_with("more items)")),
        "the preview is cut by structure: {preview}"
    );
}

#[test]
fn a_small_response_is_saved_and_shown_whole() {
    let body = json!({ "data": { "products": [{ "id": "p-1", "title": "Lamp" }] } }).to_string();
    let mock = MockServer::answering(&serde_json::from_str(&body).expect("JSON"));
    let mut mcp = start(&mock, Human::Accepts, "");

    let reply = execute(&mut mcp, None);

    let (path, _) = saved(&reply);
    assert_eq!(
        reply.text.lines().last(),
        Some(r#"data: {"products":[{"id":"p-1","title":"Lamp"}]}"#),
        "{}",
        reply.text
    );
    assert_eq!(std::fs::read_to_string(path).expect("read the saved file"), body);
}

#[test]
fn each_save_is_a_new_file() {
    let mock = MockServer::answering(&json!({ "data": { "products": [] } }));
    let mut mcp = start(&mock, Human::Accepts, "");

    let first = saved(&execute(&mut mcp, None)).0;
    let second = saved(&execute(&mut mcp, None)).0;

    assert_ne!(first, second, "a save never overwrites another");
    assert!(first.is_file() && second.is_file());
}

#[test]
fn a_saved_response_has_no_secrets() {
    let mock = MockServer::start(|request| {
        let echoed = request.header("x-api-key").unwrap_or_default();
        Response::new(
            200,
            json!({ "data": { "products": [] }, "extensions": { "key": echoed } }).to_string(),
        )
    });
    let mut mcp = start(
        &mock,
        Human::Accepts,
        "\n[endpoints.store.headers]\nX-Api-Key = { env = \"FIELDTRIP_SPEC_KEY\" }\n",
    );

    let reply = execute(&mut mcp, None);

    let (path, _) = saved(&reply);
    let contents = std::fs::read_to_string(path).expect("read the saved file");
    assert!(!contents.contains("s3cret"), "the header value was removed: {contents}");
    assert!(!reply.text.contains("s3cret"), "{}", reply.text);
}

#[test]
fn select_picks_from_the_saved_response() {
    let body = large(50);
    let served = body.clone();
    let mock = MockServer::start(move |_| Response::new(200, served.clone()));
    let mut mcp = start(&mock, Human::Accepts, "");

    let reply = execute(&mut mcp, Some("data.products[*].id"));

    let (path, _) = saved(&reply);
    assert_eq!(
        std::fs::read_to_string(path).expect("read"),
        body,
        "the file has everything"
    );
    let selected = reply
        .text
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("selected: "));
    let selected: Value = serde_json::from_str(selected.unwrap_or_default()).expect("JSON");
    assert_eq!(selected.as_array().map(Vec::len), Some(50), "{selected}");
}

#[test]
fn nothing_is_saved_when_nothing_is_sent_or_the_body_is_cut_off() {
    let mock = MockServer::start(|_| Response::new(200, large(100)));
    let mut declined = start(&mock, Human::Declines, "");
    let mut capped = start(&mock, Human::Accepts, "max_response_bytes = 65536\n");

    let refused = execute(&mut declined, None);
    let cut = execute(&mut capped, None);

    assert!(refused.is_error, "{}", refused.text);
    assert!(!responses(&declined).exists(), "a declined operation saves nothing");
    assert!(
        cut.text
            .contains("Nothing was saved, because only part of the body was read."),
        "{}",
        cut.text
    );
    assert!(!responses(&capped).exists(), "a cut-off body is not saved");
}

#[cfg(unix)]
#[test]
fn only_the_user_can_read_a_saved_response() {
    use std::os::unix::fs::PermissionsExt as _;

    let mock = MockServer::answering(&json!({ "data": { "products": [] } }));
    let mut mcp = start(&mock, Human::Accepts, "");

    let (path, _) = saved(&execute(&mut mcp, None));

    let mode = |path: &Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600, "the file is the user's alone");
    assert_eq!(mode(&responses(&mcp)), 0o700, "so is the directory");
}
