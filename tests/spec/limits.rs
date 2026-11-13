//! Slice 14: redirects are refused, large results are cut off with a note, and a silent endpoint times out.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer, Response};

/// Starts a server whose `store` endpoint reads the fixture schema, has `settings`, and never prompts.
fn start(mock: &MockServer, settings: &str) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n{settings}",
        mock.url()
    );
    Launch::new(&config).human(Human::Absent).start()
}

/// Executes a small query on the `store` endpoint.
fn execute(mcp: &mut Mcp) -> crate::support::Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { title } }" }),
    )
}

/// A GraphQL response with data in it, whose body is a little over `kilobytes` long.
fn large_response(kilobytes: usize) -> String {
    let title = "x".repeat(1000);
    let products: Vec<_> = (0..kilobytes).map(|_| json!({ "title": title })).collect();
    json!({ "data": { "products": products } }).to_string()
}

#[test]
fn a_redirect_is_refused_and_its_target_receives_nothing() {
    let target = MockServer::answering(&json!({ "data": { "products": [] } }));
    let location = target.url();
    let mock = MockServer::start(move |_| Response {
        status: 307,
        headers: vec![("location".to_owned(), location.clone())],
        body: String::new(),
    });
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n\n\
         [endpoints.store.headers]\nAuthorization = \"Bearer must-not-travel\"\n",
        mock.url()
    );
    let mut mcp = Mcp::start(&config);

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "a redirect is an error: {}", reply.text);
    assert!(reply.text.contains("HTTP 307"), "names the status: {}", reply.text);
    assert!(
        reply.text.contains(&target.url()),
        "says where the redirect pointed: {}",
        reply.text
    );
    assert!(
        reply.text.contains("does not follow redirects"),
        "says why: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 1, "the configured endpoint was asked once");
    assert_eq!(
        target.requests().len(),
        0,
        "the redirect target must receive nothing, least of all the headers"
    );
}

#[test]
fn a_large_response_is_summarized_with_a_note() {
    let body = large_response(60);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut mcp = start(&mock, "");

    let reply = execute(&mut mcp);

    assert!(
        !reply.is_error,
        "a long response with data is still a result: {}",
        reply.text.len()
    );
    let mut lines = reply.text.lines();
    assert!(
        lines.next().unwrap_or_default().starts_with("Sent query to `store`."),
        "the summary line comes first"
    );
    assert!(
        lines.next().unwrap_or_default().starts_with("HTTP 200 · "),
        "the status line comes next"
    );
    let note = reply.text.lines().last().unwrap_or_default();
    assert!(
        note.starts_with("[The result would have ") && note.contains("this is a summary"),
        "the last line is the note: {note}"
    );
    assert!(note.contains("Ask for less"), "the note says what to do: {note}");
    assert!(
        reply.text.chars().count() <= 40_000,
        "the result fits the cap: {}",
        reply.text.len()
    );
}

#[test]
fn a_response_too_large_to_read_is_cut_off_without_failing() {
    let body = large_response(3000);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut mcp = start(&mock, "max_response_bytes = 1048576\n");

    let reply = execute(&mut mcp);

    assert!(
        !reply.is_error,
        "a 200 that is too long to read in full is still a result"
    );
    assert!(
        reply
            .text
            .contains("[The response is larger than 1 MiB, the `max_response_bytes` limit"),
        "the note names the limit: {}",
        reply.text
    );
    assert!(reply.text.chars().count() < 2_000, "only a short excerpt is quoted");

    let after = mcp.call("list_endpoints", &json!({}));
    assert!(!after.is_error, "the server keeps working afterwards: {}", after.text);
}

#[test]
fn a_huge_type_is_cut_off_with_a_note() {
    let mock = MockServer::unreachable();
    let config = format!("[endpoints.big]\nurl = \"{}\"\nschema = \"big.graphql\"\n", mock.url());
    let mut schema = String::from("type Query {\n");
    for index in 0..4000 {
        let _ = writeln!(schema, "  \"The field with number {index}.\"\n  field{index}: String");
    }
    schema.push_str("}\n");
    // The schema file has to be there when the server starts, which checks that it is.
    let mut mcp = Launch::new(&config).file("big.graphql", &schema).start();

    let reply = mcp.call("describe", &json!({ "endpoint": "big", "name": "Query" }));

    assert!(!reply.is_error, "describe should succeed: {}", reply.text.len());
    let note = reply.text.lines().last().unwrap_or_default();
    assert!(
        note.starts_with("[fieldtrip cut this off at 40000 characters."),
        "the last line is the note: {note}"
    );
    assert!(
        note.contains("`Type.field`"),
        "the note says how to ask for less: {note}"
    );
}

#[test]
fn an_endpoint_that_does_not_answer_times_out() {
    let mock = MockServer::start(|_| {
        std::thread::sleep(Duration::from_secs(8));
        Response::new(200, "{\"data\":{\"products\":[]}}")
    });
    let mut mcp = start(&mock, "timeout = 1\n");
    let started = Instant::now();

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "a timeout is an error: {}", reply.text);
    assert!(
        reply.text.contains("did not answer within 1 seconds"),
        "says how long it waited: {}",
        reply.text
    );
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "it gave up after the timeout, not when the answer came"
    );
}

#[test]
fn a_timeout_of_zero_is_refused_at_startup() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\ntimeout = 0\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("`timeout`"), "names the setting: {stderr}");
}
