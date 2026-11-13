//! Slice 31: `execute` reads up to `max_response_bytes`, 8 MiB by default, and a tool result about an endpoint carries
//! up to `max_output_chars`, 40,000 by default. Both are set on an endpoint or a template, and `check` shows them.

use std::fmt::Write as _;

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer, Response};

/// A GraphQL response whose body is a little over `kilobytes` long.
fn large_response(kilobytes: usize) -> String {
    let title = "x".repeat(1000);
    let products: Vec<_> = (0..kilobytes).map(|_| json!({ "title": title })).collect();
    json!({ "data": { "products": products } }).to_string()
}

/// Starts a server whose `store` endpoint answers from `mock`, reads the fixture schema, never prompts, and has
/// `settings`.
fn start(mock: &MockServer, settings: &str) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n{settings}",
        mock.url()
    );
    Launch::new(&config).human(Human::Absent).start()
}

/// Executes a small query on `endpoint`.
fn execute(mcp: &mut Mcp, endpoint: &str) -> crate::support::Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": endpoint, "operation": "{ products { title } }" }),
    )
}

#[test]
fn a_response_of_several_megabytes_is_read_by_default() {
    let body = large_response(5000);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut mcp = start(&mock, "");

    let reply = execute(&mut mcp, "store");

    assert!(!reply.is_error, "5 MB is under the default 8 MiB: {}", reply.text.len());
    assert!(
        !reply.text.contains("`max_response_bytes`"),
        "the whole body was read: {}",
        reply.text.get(..300).unwrap_or_default()
    );
}

#[test]
fn a_response_over_the_read_limit_says_which_limit_and_what_was_lost() {
    let body = large_response(100);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut mcp = start(&mock, "max_response_bytes = 65536\n");

    let reply = execute(&mut mcp, "store");

    assert!(!reply.is_error, "a 200 is still a result: {}", reply.text);
    for phrase in [
        "Sent query to `store`.",
        "HTTP 200 · ",
        "larger than 64 KiB",
        "`max_response_bytes`",
        "`data` and `errors` are both lost",
        "`first`",
    ] {
        assert!(reply.text.contains(phrase), "says {phrase:?}: {}", reply.text);
    }
}

#[test]
fn an_error_status_over_the_read_limit_is_an_error() {
    let body = large_response(100);
    let mock = MockServer::start(move |_| Response::new(500, body.clone()));
    let mut mcp = start(&mock, "max_response_bytes = 65536\n");

    let reply = execute(&mut mcp, "store");

    assert!(reply.is_error, "HTTP 500 is an error: {}", reply.text);
    assert!(reply.text.contains("`max_response_bytes`"), "{}", reply.text);
}

#[test]
fn the_output_limit_applies_to_execute_and_describe() {
    let body = large_response(10);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut schema = String::from("type Query {\n  products: [Product!]!\n");
    for index in 0..400 {
        let _ = writeln!(schema, "  \"The field with number {index}.\"\n  field{index}: String");
    }
    schema.push_str("}\ntype Product { title: String! }\n");
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"big.graphql\"\napproval = \"client\"\nmax_output_chars = 2000\n",
        mock.url()
    );
    let mut mcp = Launch::new(&config).file("big.graphql", &schema).start();

    let executed = execute(&mut mcp, "store");
    let described = mcp.call("describe", &json!({ "endpoint": "store", "name": "Query" }));

    for reply in [&executed, &described] {
        assert!(!reply.is_error, "{}", reply.text);
        assert!(reply.text.contains("2000"), "the note names the limit: {}", reply.text);
        assert!(
            reply.text.chars().count() < 2_600,
            "the result is about the size of the limit: {}",
            reply.text.chars().count()
        );
    }
}

#[test]
fn a_template_sets_both_limits_and_check_shows_them() {
    let body = large_response(100);
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let config = format!(
        "[templates.api]\nschema = \"schema.graphql\"\nmax_response_bytes = 65536\nmax_output_chars = 5000\n\n\
         [endpoints.shared]\ntemplate = \"api\"\nurl = \"{url}\"\napproval = \"client\"\n\n\
         [endpoints.own]\ntemplate = \"api\"\nurl = \"{url}\"\napproval = \"client\"\n\
         max_response_bytes = 1048576\nmax_output_chars = 3000\n",
        url = mock.url()
    );

    let mut mcp = Launch::new(&config).start();
    let shared = execute(&mut mcp, "shared");
    let own = execute(&mut mcp, "own");
    let checked = Launch::new(&config).check();

    assert!(
        shared.text.contains("larger than 64 KiB"),
        "the template's read limit applies: {}",
        shared.text
    );
    assert!(
        !own.text.contains("`max_response_bytes`"),
        "the endpoint's read limit wins: {}",
        own.text
    );
    assert!(
        own.text.contains("at 3000 characters"),
        "the endpoint's output limit wins"
    );
    assert!(checked.passed, "{}", checked.stdout);
    for (endpoint, bytes, chars) in [("shared", "64 KiB", "5000"), ("own", "1 MiB", "3000")] {
        let section = checked
            .stdout
            .split(&format!("\n{endpoint}\n"))
            .nth(1)
            .unwrap_or_default();
        assert!(
            section.contains(&format!("  max response bytes: {bytes}\n  max output chars: {chars}\n")),
            "check shows the limits for `{endpoint}`: {}",
            checked.stdout
        );
    }
}

#[test]
fn limits_that_cannot_work_are_refused_at_startup() {
    for (setting, named) in [
        ("max_response_bytes = 0", "`max_response_bytes`"),
        ("max_output_chars = 999", "`max_output_chars` must be at least 1000"),
    ] {
        let config = format!("[endpoints.store]\nurl = \"http://localhost:9/graphql\"\n{setting}\n");

        let stderr = Launch::new(&config).fails_to_start();

        assert!(stderr.contains(named), "{setting} is refused: {stderr}");
    }
}
