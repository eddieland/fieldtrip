//! Slice 45: a header can carry a value fieldtrip generates for each request, such as a correlation ID or a W3C
//! `traceparent`, so that a request can be found in the endpoint's logs even when the endpoint returns no ID of its
//! own, or never answers. The value is not a secret: `execute` shows it and the audit log records it.

use std::time::Duration;

use serde_json::{Value, json};

use crate::support::{Launch, Mcp, MockServer, Response};

const QUERY: &str = "query Viewer { viewer { id } }";

/// The headers of every config here: a correlation ID and a trace context, both generated.
const GENERATED: &str = "X-Correlation-Id = { generate = \"uuid\" }\ntraceparent = { generate = \"traceparent\" }\n";

/// A config whose `store` endpoint sends to `mock` without asking, logs to `audit.jsonl`, and carries `headers`.
fn config(mock: &MockServer, settings: &str, headers: &str) -> String {
    format!(
        "audit_log = \"audit.jsonl\"\n\n[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n\
         approval = \"client\"\n{settings}\n[endpoints.store.headers]\n{headers}",
        mock.url()
    )
}

/// A mock that answers every request with some data.
fn answering() -> MockServer {
    MockServer::answering(&json!({ "data": { "viewer": { "id": "u-1" } } }))
}

/// Runs [`QUERY`] on `store`.
fn execute(mcp: &mut Mcp) -> crate::support::Reply {
    mcp.call("execute", &json!({ "endpoint": "store", "operation": QUERY }))
}

/// Every line of the audit log next to the config of `mcp`, parsed.
fn audit(mcp: &Mcp) -> Vec<Value> {
    let text = std::fs::read_to_string(mcp.dir().join("audit.jsonl")).expect("read the audit log");
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect()
}

/// Whether `value` is a version 4 UUID in lowercase hex.
fn is_uuid_v4(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
        && value.chars().nth(14) == Some('4')
        && value.chars().nth(19).is_some_and(|variant| "89ab".contains(variant))
}

/// Whether `value` is a W3C `traceparent` with random, non-zero IDs and the sampled flag.
fn is_traceparent(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let hex = |part: &str, len: usize| {
        part.len() == len
            && part.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && part.chars().any(|c| c != '0')
    };
    parts.len() == 4
        && parts.first() == Some(&"00")
        && parts.get(1).is_some_and(|trace| hex(trace, 32))
        && parts.get(2).is_some_and(|span| hex(span, 16))
        && parts.get(3) == Some(&"01")
}

/// The value of the header `name` in each request `mock` received.
fn sent(mock: &MockServer, name: &str) -> Vec<String> {
    mock.requests()
        .iter()
        .map(|request| request.header(name).unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn each_request_carries_new_values_and_the_result_shows_them() {
    let mock = answering();
    let mut mcp = Launch::new(&config(&mock, "", GENERATED)).start();

    let first = execute(&mut mcp);
    let second = execute(&mut mcp);

    assert!(!first.is_error, "{}", first.text);
    assert!(!second.is_error, "{}", second.text);
    let ids = sent(&mock, "x-correlation-id");
    let traces = sent(&mock, "traceparent");
    assert_eq!(ids.len(), 2, "two requests");
    assert!(ids.iter().all(|id| is_uuid_v4(id)), "a UUID v4 each time: {ids:?}");
    assert!(
        traces.iter().all(|trace| is_traceparent(trace)),
        "a traceparent each time: {traces:?}"
    );
    assert_ne!(ids.first(), ids.get(1), "a new ID for each request");
    assert_ne!(traces.first(), traces.get(1), "a new trace for each request");
    for (reply, (id, trace)) in [first, second].iter().zip(ids.iter().zip(&traces)) {
        assert!(
            reply.text.contains(&format!("sent X-Correlation-Id {id}")),
            "the result shows the ID that was sent, under its configured name: {}",
            reply.text
        );
        assert!(
            reply.text.contains(&format!("sent traceparent {trace}")),
            "and the trace: {}",
            reply.text
        );
    }
}

#[test]
fn the_audit_line_records_what_was_sent() {
    let mock = answering();
    let mut mcp = Launch::new(&config(&mock, "", GENERATED)).start();

    let reply = execute(&mut mcp);
    assert!(!reply.is_error, "{}", reply.text);

    let entries = audit(&mcp);
    let entry = entries.first().expect("one line");
    assert_eq!(entry["status"], 200, "{entry}");
    assert_eq!(
        entry["sent"]["X-Correlation-Id"],
        sent(&mock, "x-correlation-id").remove(0).as_str(),
        "{entry}"
    );
    assert_eq!(
        entry["sent"]["traceparent"],
        sent(&mock, "traceparent").remove(0).as_str(),
        "{entry}"
    );
}

#[test]
fn a_request_that_times_out_still_shows_and_records_its_id() {
    let mock = MockServer::start(|_| {
        std::thread::sleep(Duration::from_secs(4));
        Response::new(200, "{\"data\":{\"viewer\":null}}")
    });
    let mut mcp = Launch::new(&config(&mock, "timeout = 1\n", GENERATED)).start();

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "a timeout is an error: {}", reply.text);
    let id = sent(&mock, "x-correlation-id").remove(0);
    assert!(is_uuid_v4(&id), "{id}");
    assert!(
        reply.text.contains(&format!("X-Correlation-Id {id}")),
        "the error gives the ID the request carried: {}",
        reply.text
    );
    assert!(reply.text.contains("logs"), "and says what it is for: {}", reply.text);
    let entries = audit(&mcp);
    let entry = entries.first().expect("one line");
    assert!(entry.get("status").is_none(), "no answer came: {entry}");
    assert!(
        entry["error"]
            .as_str()
            .is_some_and(|error| error.contains("did not answer")),
        "{entry}"
    );
    assert_eq!(entry["sent"]["X-Correlation-Id"], id.as_str(), "{entry}");
}

#[test]
fn a_retry_after_a_401_repeats_the_same_values() {
    let mock = MockServer::start(|_| Response::new(401, "{\"errors\":[{\"message\":\"token expired\"}]}"));
    let headers = format!("{GENERATED}Authorization = {{ command = [\"sh\", \"token.sh\", \"count\"] }}\n");
    let mut mcp = Launch::new(&config(&mock, "", &headers)).start();

    let reply = execute(&mut mcp);

    assert!(
        reply.text.contains("HTTP 401 twice"),
        "the retry happened: {}",
        reply.text
    );
    let ids = sent(&mock, "x-correlation-id");
    let traces = sent(&mock, "traceparent");
    assert_eq!(ids.len(), 2, "one retry");
    assert_eq!(ids.first(), ids.get(1), "both attempts share an ID");
    assert_eq!(traces.first(), traces.get(1), "and a trace");
    let tokens = sent(&mock, "authorization");
    assert_ne!(tokens.first(), tokens.get(1), "while the credential was fetched again");
    let next = execute(&mut mcp);
    assert!(next.text.contains("HTTP 401"), "{}", next.text);
    assert_ne!(
        sent(&mock, "x-correlation-id").get(2),
        ids.first(),
        "a new call is a new request, with a new ID"
    );
}

#[test]
fn a_request_that_never_left_names_no_id() {
    let mock = MockServer::unreachable();
    let headers = format!("{GENERATED}X-Key = {{ env = \"GENERATED_TEST_UNSET\" }}\n");
    let mut mcp = Launch::new(&config(&mock, "", &headers)).start();

    let reply = execute(&mut mcp);

    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("GENERATED_TEST_UNSET"), "{}", reply.text);
    assert!(
        !reply.text.contains("X-Correlation-Id"),
        "nothing was sent, so there is no ID: {}",
        reply.text
    );
    assert!(mock.requests().is_empty(), "nothing was sent");
    let entries = audit(&mcp);
    let entry = entries.first().expect("one line");
    assert!(entry.get("sent").is_none(), "{entry}");
}

#[test]
fn introspection_carries_them_too() {
    let mock = MockServer::serving(crate::support::FixtureApi::new());
    let config = format!(
        "[endpoints.live]\nurl = \"{}\"\n\n[endpoints.live.headers]\n{GENERATED}",
        mock.url()
    );
    let mut mcp = Mcp::start(&config);

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(!reply.is_error, "{}", reply.text);
    let ids = sent(&mock, "x-correlation-id");
    assert!(!ids.is_empty() && ids.iter().all(|id| is_uuid_v4(id)), "{ids:?}");
}

#[test]
fn a_generated_value_is_not_treated_as_a_secret() {
    // The server echoes the ID back in its body, as many do in an error.
    let mock = MockServer::start(|request| {
        let id = request.header("x-correlation-id").unwrap_or_default();
        Response::new(200, json!({ "data": { "viewer": { "id": id } } }).to_string())
    });
    let mut mcp = Launch::new(&config(&mock, "", GENERATED)).start();

    let reply = execute(&mut mcp);

    let id = sent(&mock, "x-correlation-id").remove(0);
    assert!(
        reply.text.contains(&format!("\"id\":\"{id}\"")) || reply.text.contains(&format!("\"id\": \"{id}\"")),
        "an echoed ID is left as it is: {}",
        reply.text
    );
    assert!(!reply.text.contains("<redacted>"), "{}", reply.text);
}

#[test]
fn list_endpoints_says_which_headers_are_generated() {
    let mock = answering();
    let mut mcp = Launch::new(&config(&mock, "", GENERATED)).start();

    let reply = mcp.call("list_endpoints", &json!({}));

    assert!(
        reply
            .text
            .contains("header X-Correlation-Id: a new uuid for each request"),
        "{}",
        reply.text
    );
    assert!(
        reply
            .text
            .contains("header traceparent: a new traceparent for each request"),
        "{}",
        reply.text
    );
    assert!(mock.requests().is_empty(), "listing sends nothing");
}

#[test]
fn check_shows_the_generator() {
    let checked = Launch::new(&format!(
        "[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\n\n[endpoints.store.headers]\n{GENERATED}"
    ))
    .check();

    assert!(checked.passed, "{}\n{}", checked.stdout, checked.stderr);
    assert!(checked.stdout.contains("generated (uuid)"), "{}", checked.stdout);
    assert!(checked.stdout.contains("generated (traceparent)"), "{}", checked.stdout);
}

#[test]
fn a_generate_that_is_not_understood_stops_the_server() {
    let start = |header: &str| {
        Launch::new(&format!(
            "[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\n\n[endpoints.store.headers]\n{header}\n"
        ))
        .fails_to_start()
    };

    let unknown = start("X-Id = { generate = \"ulid\" }");
    assert!(
        unknown.contains("\"uuid\"") && unknown.contains("\"traceparent\""),
        "names the kinds: {unknown}"
    );

    let formatted = start("X-Id = { generate = \"uuid\", format = \"id-{}\" }");
    assert!(formatted.contains("`format`"), "{formatted}");

    let both = start("X-Id = { generate = \"uuid\", env = \"ID\" }");
    assert!(both.contains("Use one of them"), "{both}");
}
