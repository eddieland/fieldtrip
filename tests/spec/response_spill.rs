//! Slice 54: a response longer than `max_inline_chars` is written whole to a file, and the result gives the path,
//! `errors` in full, a short preview and the shape of the data instead of the body.

use std::path::PathBuf;

use serde_json::{Value, json};

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

/// Executes a query on the `store` endpoint, with `select` when one is given.
fn execute(mcp: &mut Mcp, select: Option<&str>) -> Reply {
    let mut arguments = json!({ "endpoint": "store", "operation": "{ products { id title } }" });
    if let Some(select) = select {
        arguments["select"] = json!(select);
    }
    mcp.call("execute", &arguments)
}

/// `count` products, each with a title of `title_length` characters.
fn products(count: usize, title_length: usize) -> Value {
    let title = "t".repeat(title_length);
    (0..count)
        .map(|index| json!({ "id": format!("p-{index}"), "title": title }))
        .collect()
}

/// Every file the server has written under its responses directory.
fn files(mcp: &Mcp) -> Vec<PathBuf> {
    let root = mcp.dir().join("cache-home").join("fieldtrip").join("responses");
    let Ok(sessions) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    sessions
        .filter_map(Result::ok)
        .filter_map(|session| std::fs::read_dir(session.path()).ok())
        .flat_map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()))
        .collect()
}

/// The path the result says the response was saved at.
fn saved(reply: &Reply) -> PathBuf {
    reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("Saved the whole response, "))
        .and_then(|line| line.split_once(" bytes, to "))
        .map_or_else(
            || panic!("the result says where the response is: {}", reply.text),
            |(_, path)| PathBuf::from(path),
        )
}

/// The lines of the result that give the shape of the response.
fn shape(reply: &Reply) -> Vec<&str> {
    reply
        .text
        .lines()
        .skip_while(|line| !line.starts_with("Shape of the whole response"))
        .skip(1)
        .collect()
}

#[test]
fn a_response_over_the_limit_gives_a_path_a_preview_and_the_shape() {
    let body = json!({ "data": { "products": products(200, 100) } }).to_string();
    assert!(body.len() > 10_000 && body.len() < 40_000, "{}", body.len());
    let (mut mcp, _mock) = start(body.clone(), "");

    let reply = execute(&mut mcp, None);

    assert!(!reply.is_error, "{}", reply.text);
    let path = saved(&reply);
    assert_eq!(
        files(&mcp),
        std::slice::from_ref(&path),
        "one file, in a directory for this session"
    );
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("store-")),
        "{}",
        path.display()
    );
    assert_eq!(path.extension().and_then(|extension| extension.to_str()), Some("json"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the file"),
        body,
        "the file holds the response as the server wrote it"
    );
    assert!(
        reply.text.contains("`max_inline_chars`"),
        "the result says why it was saved: {}",
        reply.text
    );
    assert!(
        reply.text.chars().count() < 6_000,
        "never the full body: {}",
        reply.text.chars().count()
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
        "the preview is cut and says so: {preview}"
    );
    assert_eq!(
        shape(&reply),
        [
            "data.products[200].id: string",
            "data.products[200].title: string (100 chars)"
        ],
        "{}",
        reply.text
    );
}

#[test]
fn a_response_under_the_limit_is_returned_inline() {
    let body = json!({ "data": { "products": products(50, 100) } }).to_string();
    assert!(body.len() < 10_000, "{}", body.len());
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, None);

    assert_eq!(
        reply.text.lines().last(),
        Some(format!("data: {}", json!({ "products": products(50, 100) })).as_str()),
        "the data is whole"
    );
    assert!(!reply.text.contains("Saved"), "{}", reply.text);
    assert!(files(&mcp).is_empty(), "nothing is written");
}

#[test]
fn one_long_string_spills_and_the_shape_says_how_long_it_is() {
    let body = json!({ "data": { "product": { "id": "p-1", "description": "d".repeat(31_204) } } }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, None);

    assert!(reply.text.contains("more characters)"), "the string is cut and marked");
    assert!(reply.text.chars().count() < 6_000, "{}", reply.text.chars().count());
    assert_eq!(
        shape(&reply),
        [
            "data.product.id: string",
            "data.product.description: string (31,204 chars)"
        ],
        "{}",
        reply.text
    );
}

#[test]
fn errors_are_whole_when_the_data_spills() {
    let errors: Vec<Value> = (0..20)
        .map(|index| json!({ "message": "m".repeat(400), "path": ["products", index, "title"] }))
        .collect();
    let body = json!({ "data": { "products": products(500, 100) }, "errors": errors }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, None);

    let shown = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("errors: "))
        .unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<Value>(shown).expect("JSON"),
        json!(errors),
        "every error is there, though they are longer than the preview"
    );
    let data = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap_or_default();
    assert!(data.contains("more items)"), "the data is still only a preview: {data}");
    assert!(
        shape(&reply).contains(&"errors[20].message: string (400 chars)"),
        "the shape covers the whole response: {:?}",
        shape(&reply)
    );
}

#[test]
fn the_shape_merges_list_items_and_says_what_varies() {
    let rows: Vec<Value> = (0..300)
        .map(|index| {
            json!({
                "id": index,
                "note": if index % 2 == 0 { Value::Null } else { json!("n".repeat(index)) },
                "tags": vec!["t"; index % 4],
                "price": { "amount": 9.5, "taxed": true },
            })
        })
        .collect();
    let body = json!({ "data": { "products": rows, "empty": [], "none": {} } }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, None);

    assert_eq!(
        shape(&reply),
        [
            "data.products[300].id: number",
            "data.products[300].note: null | string (up to 299 chars)",
            "data.products[300].tags[0-3]: string",
            "data.products[300].price.amount: number",
            "data.products[300].price.taxed: boolean",
            "data.empty[0]: empty list",
            "data.none: empty object",
        ],
        "{}",
        reply.text
    );
}

#[test]
fn a_shape_with_many_paths_is_cut_and_says_so() {
    let wide: serde_json::Map<String, Value> = (0..500)
        .map(|index| (format!("field{index}"), json!("v".repeat(40))))
        .collect();
    let body = json!({ "data": wide }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, None);

    let shape = shape(&reply);
    assert_eq!(shape.len(), 41, "forty paths and a note: {}", shape.len());
    assert_eq!(shape.last(), Some(&"… and 460 more paths."), "{shape:?}");
}

#[test]
fn a_selection_from_a_spilled_response_is_shown_and_the_file_has_the_rest() {
    let body = json!({ "data": { "products": products(200, 100) } }).to_string();
    let (mut mcp, _mock) = start(body.clone(), "");

    let reply = execute(&mut mcp, Some("data.products[*].id"));

    assert_eq!(std::fs::read_to_string(saved(&reply)).expect("read"), body);
    let selected = reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("selected: "))
        .unwrap_or_default();
    let selected: Value = serde_json::from_str(selected).expect("JSON");
    assert_eq!(selected.as_array().map(Vec::len), Some(200), "the selection is whole");
}

#[test]
fn a_selection_that_is_too_long_itself_is_cut_to_the_limit() {
    let body = json!({ "data": { "products": products(500, 100) } }).to_string();
    let (mut mcp, _mock) = start(body, "");

    let reply = execute(&mut mcp, Some("data.products[*].title"));

    assert!(
        reply.text.chars().count() <= 10_000,
        "the result fits `max_inline_chars`: {}",
        reply.text.chars().count()
    );
    assert!(reply.text.contains("more items)"), "the selection is cut and marked");
    let note = reply
        .text
        .lines()
        .find(|line| line.starts_with("[The result would have"))
        .unwrap_or_default();
    assert!(
        note.contains("`max_inline_chars`") && note.contains("The file has all of it"),
        "{note}"
    );
}

#[test]
fn the_limit_is_set_on_an_endpoint_or_its_template() {
    let body = json!({ "data": { "products": products(30, 100) } }).to_string();
    assert!(body.len() > 2_000 && body.len() < 10_000, "{}", body.len());
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let config = format!(
        "[templates.api]\nschema = \"schema.graphql\"\nmax_inline_chars = 2000\n\n\
         [endpoints.small]\nurl = \"{url}\"\ntemplate = \"api\"\napproval = \"client\"\n\n\
         [endpoints.roomy]\nurl = \"{url}\"\ntemplate = \"api\"\napproval = \"client\"\nmax_inline_chars = 20000\n",
        url = mock.url()
    );
    let mut mcp = Launch::new(&config).start();
    let query = |endpoint: &str| json!({ "endpoint": endpoint, "operation": "{ products { id title } }" });

    let small = mcp.call("execute", &query("small"));
    let roomy = mcp.call("execute", &query("roomy"));

    assert!(small.text.contains("Saved the whole response"), "{}", small.text);
    assert!(!roomy.text.contains("Saved"), "{}", roomy.text);
    assert_eq!(
        files(&mcp).len(),
        1,
        "only the endpoint with the small limit wrote a file"
    );
}

#[test]
fn a_limit_above_the_output_limit_cannot_show_more_than_a_result_holds() {
    let body = json!({ "data": { "products": products(50, 100) } }).to_string();
    let (mut mcp, _mock) = start(body, "max_output_chars = 3000\nmax_inline_chars = 20000\n");

    let reply = execute(&mut mcp, None);

    assert!(reply.text.contains("Saved the whole response"), "{}", reply.text);
    assert!(reply.text.chars().count() <= 3_000, "{}", reply.text.chars().count());
}

#[test]
fn a_limit_that_is_too_small_stops_the_server() {
    let stderr = Launch::new(
        "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\nmax_inline_chars = 999\n",
    )
    .fails_to_start();

    assert!(stderr.contains("`max_inline_chars` must be at least 1000"), "{stderr}");
}

#[test]
fn check_shows_the_limit() {
    let checked = Launch::new(
        "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\nmax_inline_chars = 2500\n\n\
         [endpoints.plain]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n",
    )
    .check();

    assert!(checked.stdout.contains("max inline chars: 2500"), "{}", checked.stdout);
    assert!(checked.stdout.contains("max inline chars: 10000"), "{}", checked.stdout);
}
