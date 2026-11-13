//! Slice 28: `execute` returns `errors` in full before `data`, with the status, the time taken and the response
//! headers the user chose. A response with only `errors` is a result. A failed request says how long it waited.

use std::time::Duration;

use serde_json::json;

use crate::support::{Human, Launch, Mcp, MockServer, Response};

/// A value the `env` plug sends, which a server could echo back in a header.
const SECRET: &str = "env-sekrit-0005";

/// A mock that answers every request with status 200, `body`, and the response headers `headers`.
fn mock_with_headers(body: &serde_json::Value, headers: &[(&str, &str)]) -> MockServer {
    let body = body.to_string();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    MockServer::start(move |_| Response {
        status: 200,
        headers: headers.clone(),
        body: body.clone(),
    })
}

/// Starts a server whose `store` endpoint reads the fixture schema, has `settings`, and never prompts.
fn start(mock: &MockServer, settings: &str) -> Mcp {
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n{settings}",
        mock.url()
    );
    Launch::new(&config)
        .env("FIELDTRIP_SPEC_SECRET", SECRET)
        .human(Human::Absent)
        .start()
}

/// Executes a small query on the `store` endpoint.
fn execute(mcp: &mut Mcp) -> crate::support::Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query Me { viewer { name } }" }),
    )
}

/// The second line of a result: the status, the time taken and the chosen headers.
fn status_line(text: &str) -> &str {
    text.lines().nth(1).unwrap_or_default()
}

/// `line` without its `HTTP <status> · <n> ms` start, or `None` when it does not start that way.
fn after_timing(line: &str, status: u16) -> Option<&str> {
    let rest = line.strip_prefix(&format!("HTTP {status} · "))?;
    let (millis, rest) = rest.split_once(" ms")?;
    millis.parse::<u64>().ok().map(|_| rest)
}

#[test]
fn errors_alone_come_back_in_full_with_the_request_id() {
    let errors = json!([{
        "message": "Cannot query viewer: token expired",
        "path": ["viewer"],
        "locations": [{ "line": 1, "column": 12 }],
        "extensions": { "code": "UNAUTHENTICATED", "stacktrace": ["at auth (auth.js:12)", "at resolve (viewer.js:3)"] },
    }]);
    let mock = mock_with_headers(
        &json!({ "errors": errors }),
        &[
            ("x-request-id", "req-abc123"),
            ("traceparent", "00-4bf92f3577b34da6-00f067aa0ba902b7-01"),
        ],
    );
    let mut mcp = start(&mock, "response_headers = [\"x-request-id\", \"traceparent\"]\n");

    let reply = execute(&mut mcp);

    assert!(
        !reply.is_error,
        "errors with no data are a result, not a tool error: {}",
        reply.text
    );
    assert_eq!(
        after_timing(status_line(&reply.text), 200),
        Some(" · x-request-id req-abc123 · traceparent 00-4bf92f3577b34da6-00f067aa0ba902b7-01"),
        "the status line has the status, the time taken and the chosen headers in config order: {}",
        reply.text
    );
    assert_eq!(
        reply.text.lines().nth(2),
        Some(format!("errors: {errors}").as_str()),
        "the errors follow, whole, with their path, locations and extensions"
    );
    assert_eq!(reply.text.lines().count(), 3, "and nothing else: {}", reply.text);
}

#[test]
fn errors_come_before_data() {
    let mock = MockServer::answering(&json!({
        "data": { "viewer": null },
        "errors": [{ "message": "viewer failed", "path": ["viewer"] }],
        "extensions": { "cost": 1 },
    }));
    let mut mcp = start(&mock, "");

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "{}", reply.text);
    let parts: Vec<&str> = reply
        .text
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').map_or(line, |(key, _)| key))
        .collect();
    assert_eq!(parts, ["errors", "data", "extensions"], "{}", reply.text);
    assert!(
        after_timing(status_line(&reply.text), 200) == Some(""),
        "with no `response_headers`, the status line has only the status and the time: {}",
        reply.text
    );
}

#[test]
fn errors_from_a_failing_status_are_a_result_too() {
    let mock = MockServer::start(|_| {
        Response::new(
            500,
            json!({ "errors": [{ "message": "Internal server error" }] }).to_string(),
        )
    });
    let mut mcp = start(&mock, "");

    let reply = execute(&mut mcp);

    assert!(
        !reply.is_error,
        "a GraphQL response is a result whatever the status: {}",
        reply.text
    );
    assert!(after_timing(status_line(&reply.text), 500).is_some(), "{}", reply.text);
    assert!(reply.text.contains("Internal server error"), "{}", reply.text);
}

#[test]
fn a_header_that_is_not_configured_is_not_shown() {
    let mock = mock_with_headers(
        &json!({ "data": { "viewer": { "name": "Ada" } } }),
        &[("x-request-id", "req-abc123"), ("x-internal-session", "sess-998877")],
    );
    let mut mcp = start(
        &mock,
        "response_headers = [\"X-Request-ID\", \"x-ratelimit-remaining\"]\n",
    );

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(
        after_timing(status_line(&reply.text), 200),
        Some(" · X-Request-ID req-abc123"),
        "a configured header is matched ignoring case and shown by its configured name, and one that did not come \
         back is left out: {}",
        reply.text
    );
    assert!(!reply.text.contains("x-internal-session"), "{}", reply.text);
    assert!(!reply.text.contains("sess-998877"), "{}", reply.text);
}

#[test]
fn a_header_that_echoes_a_secret_is_redacted() {
    let mock = mock_with_headers(
        &json!({ "data": { "viewer": { "name": "Ada" } } }),
        &[("x-debug-auth", &format!("key={SECRET}"))],
    );
    let settings = "response_headers = [\"x-debug-auth\"]\n\n\
                    [endpoints.store.headers]\nX-Api-Key = { env = \"FIELDTRIP_SPEC_SECRET\" }\n";
    let mut mcp = start(&mock, settings);

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "{}", reply.text);
    assert!(!reply.text.contains(SECRET), "the secret is not shown: {}", reply.text);
    assert_eq!(
        after_timing(status_line(&reply.text), 200),
        Some(" · x-debug-auth key=<redacted>"),
        "{}",
        reply.text
    );
}

#[test]
fn an_endpoint_takes_response_headers_from_its_template_unless_it_sets_them() {
    let mock = mock_with_headers(
        &json!({ "data": { "viewer": { "name": "Ada" } } }),
        &[("x-request-id", "req-abc123"), ("traceparent", "00-trace-01")],
    );
    let config = format!(
        "[templates.acme]\nschema = \"schema.graphql\"\nresponse_headers = [\"x-request-id\"]\n\n\
         [endpoints.inherits]\ntemplate = \"acme\"\nurl = \"{url}\"\napproval = \"client\"\n\n\
         [endpoints.overrides]\ntemplate = \"acme\"\nurl = \"{url}\"\napproval = \"client\"\n\
         response_headers = [\"traceparent\"]\n",
        url = mock.url()
    );
    let mut mcp = Launch::new(&config).start();
    let operation = "{ viewer { name } }";

    let inherits = mcp.call("execute", &json!({ "endpoint": "inherits", "operation": operation }));
    let overrides = mcp.call("execute", &json!({ "endpoint": "overrides", "operation": operation }));

    assert_eq!(
        after_timing(status_line(&inherits.text), 200),
        Some(" · x-request-id req-abc123")
    );
    assert_eq!(
        after_timing(status_line(&overrides.text), 200),
        Some(" · traceparent 00-trace-01")
    );
    let listed = mcp.call("list_endpoints", &json!({}));
    assert!(
        listed.text.contains("  response headers shown: x-request-id\n"),
        "list_endpoints shows which headers come back: {}",
        listed.text
    );
}

#[test]
fn errors_are_kept_whole_when_data_is_cut() {
    let message = format!("bad product: {}", "e".repeat(300));
    let title = "x".repeat(1000);
    let products: Vec<_> = (0..60).map(|_| json!({ "title": title })).collect();
    let mut errors: Vec<_> = (0..20).map(|_| json!({ "message": message })).collect();
    errors.push(json!({ "message": "the last error" }));
    let mock = MockServer::answering(&json!({ "data": { "products": products }, "errors": errors }));
    let mut mcp = start(&mock, "");

    let reply = execute(&mut mcp);

    assert!(!reply.is_error, "{}", reply.text.len());
    let errors_line = reply.text.lines().nth(2).unwrap_or_default();
    assert_eq!(
        errors_line,
        format!("errors: {}", json!(errors)),
        "the errors are whole"
    );
    let note = reply.text.lines().last().unwrap_or_default();
    assert!(
        note.starts_with("[fieldtrip cut this off at 40000 characters.") && note.contains("Errors are never cut."),
        "the data is cut instead, with a note: {note}"
    );
}

#[test]
fn a_bad_response_header_name_stops_the_server() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nresponse_headers = [\"x request id\"]\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("`response_headers`"), "names the setting: {stderr}");
    assert!(stderr.contains("x request id"), "names the bad entry: {stderr}");
}

#[test]
fn the_same_response_header_twice_stops_the_server() {
    let config = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\n\
                  response_headers = [\"X-Request-Id\", \"x-request-id\"]\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("twice"), "says what is wrong: {stderr}");
}

#[test]
fn a_connection_error_says_how_long_fieldtrip_waited() {
    // A port that was free a moment ago, so nothing answers on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port();
    let config = format!(
        "[endpoints.store]\nurl = \"http://127.0.0.1:{port}/graphql\"\nschema = \"schema.graphql\"\n\
         approval = \"client\"\n"
    );
    let mut mcp = Mcp::start(&config);

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("failed after ") && reply.text.contains(" ms: "),
        "says how long it waited: {}",
        reply.text
    );
}

#[test]
fn a_timeout_says_how_long_fieldtrip_waited() {
    let mock = MockServer::start(|_| {
        std::thread::sleep(Duration::from_secs(4));
        Response::new(200, "{\"data\":{\"viewer\":null}}")
    });
    let mut mcp = start(&mock, "timeout = 1\n");

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "{}", reply.text);
    let waited = reply
        .text
        .split_once("fieldtrip waited ")
        .and_then(|(_, rest)| rest.split_once(" ms"))
        .and_then(|(millis, _)| millis.parse::<u64>().ok());
    assert!(
        waited.is_some_and(|millis| (1000..3000).contains(&millis)),
        "says how long it waited, which is about the timeout: {}",
        reply.text
    );
}
