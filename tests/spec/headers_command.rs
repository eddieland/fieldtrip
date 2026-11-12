//! Slice 8: the command plug runs a program and sends its output as a header.
//!
//! `tests/fixtures/token.sh` stands in for the user's auth CLI. Commands run in the config file's directory, which
//! is where the harness copies the fixtures.

use serde_json::json;

use crate::support::{FixtureApi, Launch, Mcp, MockServer};

/// A config whose `live` endpoint is introspected through `mock` and carries `headers`.
fn config(mock: &MockServer, headers: &str) -> String {
    format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\n{headers}",
        mock.url()
    )
}

#[test]
fn the_trimmed_output_goes_through_the_format() {
    let mock = MockServer::serving(FixtureApi::new());
    let headers = "Authorization = { command = [\"sh\", \"token.sh\", \"ok\"], format = \"Bearer {}\" }\n";
    let mut mcp = Mcp::start(&config(&mock, headers));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    let requests = mock.requests();
    let request = requests.first().expect("the introspection request");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer t0ken-from-command"),
        "the whitespace around the output is dropped and the rest is placed into the format"
    );
}

#[test]
fn the_command_runs_without_a_shell() {
    let mock = MockServer::serving(FixtureApi::new());
    let headers = "X-Probe = { command = [\"sh\", \"token.sh\", \"echo\", \"$HOME && echo injected\"] }\n";
    let mut mcp = Mcp::start(&config(&mock, headers));

    mcp.call("introspect", &json!({ "endpoint": "live" }));

    let requests = mock.requests();
    let request = requests.first().expect("the introspection request");
    assert_eq!(
        request.header("x-probe"),
        Some("$HOME && echo injected"),
        "each array item reaches the program as one argument, with nothing expanded"
    );
}

#[test]
fn a_failing_command_reports_status_and_stderr_but_never_stdout() {
    let mock = MockServer::serving(FixtureApi::new());
    let headers = "Authorization = { command = [\"sh\", \"token.sh\", \"fail\"], format = \"Bearer {}\" }\n";
    let mut mcp = Mcp::start(&config(&mock, headers));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a failing command should be an error: {}", reply.text);
    assert!(
        reply.text.contains("`Authorization`"),
        "names the header: {}",
        reply.text
    );
    assert!(
        reply.text.contains("exit status: 3"),
        "gives the exit status: {}",
        reply.text
    );
    assert!(
        reply.text.contains("not logged in: run acme login"),
        "passes on stderr: {}",
        reply.text
    );
    assert!(
        !reply.text.contains("sekrit-stdout-of-failed-command"),
        "stdout may hold a token: {}",
        reply.text
    );
    assert_eq!(
        mock.requests().len(),
        0,
        "no request goes out when a header cannot be produced"
    );
}

#[test]
fn a_command_that_prints_nothing_is_an_error() {
    let mock = MockServer::serving(FixtureApi::new());
    let headers = "Authorization = { command = [\"sh\", \"token.sh\", \"empty\"] }\n";
    let mut mcp = Mcp::start(&config(&mock, headers));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "empty output should be an error: {}", reply.text);
    assert!(
        reply.text.contains("`Authorization`"),
        "names the header: {}",
        reply.text
    );
    assert!(
        reply.text.contains("printed nothing"),
        "says what went wrong: {}",
        reply.text
    );
    assert_eq!(mock.requests().len(), 0, "no request goes out with an empty header");
}

#[test]
fn a_program_that_does_not_exist_is_an_error_that_names_it() {
    let mock = MockServer::serving(FixtureApi::new());
    let headers = "Authorization = { command = [\"fieldtrip-no-such-program\", \"sekrit-argument\"] }\n";
    let mut mcp = Mcp::start(&config(&mock, headers));

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a missing program should be an error: {}", reply.text);
    assert!(
        reply.text.contains("`fieldtrip-no-such-program`"),
        "names the program: {}",
        reply.text
    );
    assert!(
        !reply.text.contains("sekrit-argument"),
        "arguments may hold secrets: {}",
        reply.text
    );
}

#[test]
fn a_command_must_be_a_list_of_strings() {
    let mock = MockServer::unreachable();

    let string = Launch::new(&config(&mock, "X-Key = { command = \"sh token.sh ok\" }\n")).fails_to_start();
    assert!(string.contains("`X-Key`"), "names the header: {string}");
    assert!(
        string.contains("list"),
        "says a list is needed, which is what rules a shell out: {string}"
    );

    let empty = Launch::new(&config(&mock, "X-Key = { command = [] }\n")).fails_to_start();
    assert!(empty.contains("`X-Key`"), "names the header: {empty}");
}
