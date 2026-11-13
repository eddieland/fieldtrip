//! Slice 32: when the parts after `errors` are too long for a result, they are summarized by structure. `errors`
//! stays whole, each part is still valid JSON, and each cut list or string says how much it dropped.

use serde_json::{Map, Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply, Response};

/// Starts a server whose `store` endpoint answers every request with `body`, and never prompts.
fn start(body: String, settings: &str) -> (Mcp, MockServer) {
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n{settings}",
        mock.url()
    );
    (Launch::new(&config).human(Human::Absent).start(), mock)
}

/// Executes a query on the `store` endpoint.
fn execute(mcp: &mut Mcp) -> Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { id title } }" }),
    )
}

/// `count` products, each with a title of `title_length` characters.
fn products(count: usize, title_length: usize) -> Vec<Value> {
    let title = "t".repeat(title_length);
    (0..count)
        .map(|index| json!({ "id": format!("p-{index}"), "title": title }))
        .collect()
}

/// The parts of a summarized result, each parsed, in order, and the note on the last line.
fn parts(reply: &Reply) -> (Vec<(String, Value)>, String) {
    let lines: Vec<&str> = reply.text.lines().skip(2).collect();
    let (note, parts) = lines.split_last().expect("there is a note");
    let parts = parts
        .iter()
        .map(|line| {
            let (name, json) = line.split_once(": ").unwrap_or_else(|| panic!("a part: {line}"));
            let value = serde_json::from_str(json).unwrap_or_else(|error| panic!("valid JSON ({error}): {json}"));
            (name.to_owned(), value)
        })
        .collect();
    (parts, (*note).to_owned())
}

/// The part called `name`.
fn part<'a>(parts: &'a [(String, Value)], name: &str) -> &'a Value {
    parts
        .iter()
        .find_map(|(part, value)| (part == name).then_some(value))
        .unwrap_or_else(|| panic!("there is a `{name}` part"))
}

/// The number in a marker such as `…(+412 more items)`.
fn dropped(marker: &Value, noun: &str) -> usize {
    let text = marker.as_str().unwrap_or_default();
    text.strip_prefix("…(+")
        .and_then(|rest| rest.strip_suffix(&format!(" more {noun})")))
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{marker} is a marker for {noun}"))
}

#[test]
fn a_large_data_is_summarized_after_whole_errors() {
    let errors = json!([{
        "message": "Product p-4999 could not be loaded",
        "path": ["products", 4999, "title"],
        "extensions": { "code": "INTERNAL_SERVER_ERROR" },
    }]);
    // Many servers write `errors` after `data`. Here `data` alone is about 5 MB.
    let body = format!(
        "{{\"data\":{},\"errors\":{errors},\"extensions\":{{\"cost\":5000}}}}",
        json!({ "products": products(5000, 1000) })
    );
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "a response with data is a result: {}", reply.text);
    assert!(
        reply.text.chars().count() <= 40_000,
        "the result fits the cap: {}",
        reply.text.chars().count()
    );
    let (parts, note) = parts(&reply);
    let names: Vec<&str> = parts.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["errors", "data", "extensions"], "errors come first");
    assert_eq!(part(&parts, "errors"), &errors, "errors are whole");
    assert_eq!(
        part(&parts, "extensions"),
        &json!({ "cost": 5000 }),
        "small parts are whole"
    );
    for phrase in ["`max_output_chars`", "valid JSON", "`errors` is whole", "`first`"] {
        assert!(note.contains(phrase), "the note says {phrase:?}: {note}");
    }
    let list = part(&parts, "data")["products"].as_array().expect("the list is kept");
    let (marker, kept) = list.split_last().expect("the list has a marker");
    assert!(kept.len() >= 5, "as many items as fit are kept: {}", kept.len());
    assert_eq!(
        kept.len() + dropped(marker, "items"),
        5000,
        "the marker says how many were dropped"
    );
    assert_eq!(kept.first(), Some(&json!({ "id": "p-0", "title": "t".repeat(1000) })));
    assert!(
        note.contains(&format!("lists to their first {} items", kept.len())),
        "the note says how many items each list kept: {note}"
    );
}

#[test]
fn a_body_just_over_the_cap_is_summarized() {
    let body = json!({ "data": { "products": products(400, 80) } }).to_string();
    assert!(body.len() > 40_000 && body.len() < 45_000, "{}", body.len());
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "{}", reply.text);
    let (parts, note) = parts(&reply);
    assert!(note.contains("this is a summary"), "{note}");
    let list = part(&parts, "data")["products"].as_array().expect("the list is kept");
    let (marker, kept) = list.split_last().expect("the list has a marker");
    assert_eq!(kept.len() + dropped(marker, "items"), 400);
    assert!(kept.len() > 300, "most of it still fits: {}", kept.len());
}

#[test]
fn a_long_string_is_cut_and_says_how_much() {
    let body = json!({ "data": { "product": { "id": "p-1", "description": "d".repeat(100_000) } } }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp);

    let (parts, note) = parts(&reply);
    let product = &part(&parts, "data")["product"];
    let description = product["description"].as_str().unwrap_or_default();
    let (kept, marker) = description.split_once('…').expect("the string has a marker");
    assert_eq!(
        dropped(&json!(format!("…{marker}")), "characters") + kept.len(),
        100_000
    );
    assert!(
        note.contains(&format!("strings to {} characters", kept.len())),
        "{note}"
    );
    assert_eq!(product["id"], "p-1", "short values are untouched");
}

#[test]
fn whitespace_alone_is_not_a_reason_to_drop_anything() {
    let data = json!({ "products": products(200, 100) });
    let body = serde_json::to_string_pretty(&json!({ "data": data }))
        .expect("pretty JSON")
        .replace("  ", "        ");
    assert!(body.len() > 40_000, "{}", body.len());
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp);

    let (parts, note) = parts(&reply);
    assert!(note.contains("Nothing was dropped"), "{note}");
    assert_eq!(part(&parts, "data"), &data, "the whole response is there");
}

#[test]
fn a_smaller_output_limit_still_gives_a_summary_that_fits() {
    let body = json!({
        "data": { "products": products(50, 200) },
        "errors": [{ "message": "partial" }],
    })
    .to_string();
    let (mut mcp, _mock) = start(body, "max_output_chars = 1500\n");

    let reply = execute(&mut mcp);

    assert!(
        reply.text.chars().count() <= 1_500,
        "the result fits the limit: {}",
        reply.text.chars().count()
    );
    let (parts, _) = parts(&reply);
    assert_eq!(part(&parts, "errors"), &json!([{ "message": "partial" }]));
}

#[test]
fn parts_after_data_are_summarized_too() {
    let mut extensions = Map::new();
    extensions.insert("trace".to_owned(), json!(products(500, 100)));
    let body = json!({ "data": { "products": [] }, "extensions": extensions }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp);

    let (parts, _) = parts(&reply);
    assert_eq!(part(&parts, "data"), &json!({ "products": [] }));
    let trace = part(&parts, "extensions")["trace"].as_array().expect("a list");
    assert!(
        trace
            .last()
            .and_then(Value::as_str)
            .is_some_and(|marker| marker.ends_with("more items)")),
        "{trace:?}"
    );
}
