//! Slice 36: `audit_log` appends one JSON line per `execute` attempt, so the user can tell afterwards what an agent
//! ran, where, and who approved it. Attempts that were refused or declined are recorded too, with the reason. Header
//! values never are, secrets are removed from what the calling agent wrote, and `audit_variables = false` leaves the
//! variables out.

use std::path::{Path, PathBuf};

use rmcp::model::{CallToolResponse, InputResponses};
use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Response, Retry, SdkClient, sdk_reply};

/// A value the `env` plug sends, which the calling agent might paste into a variable.
const SECRET: &str = "env-audit-s3cret-77";

const QUERY: &str = "query Find($text: String!) { search(text: $text) { __typename } }";
const MUTATION: &str = "mutation Remove { deleteUser(id: \"u-1\") }";

/// A mock that answers every request with some data, one error and a request ID.
fn mock() -> MockServer {
    let body = json!({
        "data": { "search": [], "deleteUser": true, "viewer": null },
        "errors": [{ "message": "partly failed", "path": ["search"] }],
    })
    .to_string();
    MockServer::start(move |_| Response {
        status: 200,
        headers: vec![("x-request-id".to_owned(), "req-abc123".to_owned())],
        body: body.clone(),
    })
}

/// The `store` endpoint with `settings`, sending to `mock`, under a config that logs to `audit.jsonl`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "audit_log = \"audit.jsonl\"\n\n[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n\
         response_headers = [\"x-request-id\"]\n{settings}\n[endpoints.store.headers]\n\
         X-Key = {{ env = \"AUDIT_KEY\" }}\n",
        mock.url()
    )
}

/// Starts a server for `config` whose human answers as `human` does.
fn start(config: &str, human: Human) -> Mcp {
    Launch::new(config).env("AUDIT_KEY", SECRET).human(human).start()
}

/// The arguments of `execute` for `operation` with `variables` on the `store` endpoint.
fn arguments(operation: &str, variables: &Value) -> Value {
    json!({ "endpoint": "store", "operation": operation, "variables": variables })
}

/// Every line of the audit log at `path`, parsed. Each must be one JSON object.
fn lines(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(text.ends_with('\n'), "every line is ended: {text:?}");
    text.lines()
        .map(|line| {
            let entry: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}"));
            assert!(entry.is_object(), "each line is a JSON object: {line}");
            entry
        })
        .collect()
}

/// The only line of the audit log next to the config of `mcp`.
fn only_line(mcp: &Mcp) -> Value {
    let mut entries = lines(&mcp.dir().join("audit.jsonl"));
    assert_eq!(entries.len(), 1, "one attempt, one line: {entries:#?}");
    entries.remove(0)
}

/// Asserts that `entry` names the `store` endpoint, the session, the time, the operation and its hash.
fn assert_describes_the_operation(entry: &Value, kind: &str, name: &str) {
    assert_eq!(entry["endpoint"], "store", "{entry}");
    assert_eq!(entry["kind"], kind, "{entry}");
    assert_eq!(entry["operation"], name, "{entry}");
    let at = entry["at"].as_str().unwrap_or_default();
    assert!(
        at.len() == 20 && at.ends_with('Z') && at.chars().nth(10) == Some('T') && at.starts_with("20"),
        "the time is RFC 3339 in UTC, to the second: {at}"
    );
    let hash = entry["hash"].as_str().unwrap_or_default();
    assert!(
        hash.strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|digit| digit.is_ascii_hexdigit())),
        "the hash is a SHA-256 of the text: {hash}"
    );
    assert!(
        entry["text"].as_str().is_some_and(|text| text.contains(name)),
        "the text is recorded: {entry}"
    );
    assert!(
        entry["session"].as_str().is_some_and(|session| session.len() == 16),
        "the session is recorded: {entry}"
    );
}

#[test]
fn an_accepted_query_is_recorded_with_its_outcome() {
    let mock = mock();
    let mut mcp = start(&config(&mock, ""), Human::Accepts);

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(!reply.is_error, "{}", reply.text);

    let entry = only_line(&mcp);
    assert_describes_the_operation(&entry, "query", "Find");
    assert_eq!(entry["approval"], "accepted", "{entry}");
    assert_eq!(entry["variables"], json!({ "text": "lamp" }), "{entry}");
    assert!(entry["cost"].is_u64(), "the estimated cost is recorded: {entry}");
    assert_eq!(entry["status"], 200, "{entry}");
    assert!(entry["ms"].is_u64(), "the time taken is recorded: {entry}");
    assert_eq!(entry["errors"], 1, "the number of GraphQL errors is recorded: {entry}");
    assert_eq!(
        entry["headers"],
        json!({ "x-request-id": "req-abc123" }),
        "the response headers the user chose are recorded: {entry}"
    );
    assert!(entry.get("reason").is_none(), "nothing was refused: {entry}");
}

#[test]
fn a_declined_attempt_is_recorded_with_the_reason() {
    let mock = mock();
    let mut mcp = start(&config(&mock, ""), Human::Declines);

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(reply.is_error, "{}", reply.text);
    assert_eq!(mock.requests().len(), 0, "nothing was sent");

    let entry = only_line(&mcp);
    assert_describes_the_operation(&entry, "query", "Find");
    assert_eq!(entry["approval"], "declined", "{entry}");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("declined")),
        "{entry}"
    );
    assert!(entry.get("status").is_none(), "nothing was sent: {entry}");
}

#[test]
fn every_refusal_is_recorded_under_its_own_name() {
    for (human, approval) in [
        (Human::Cancels, "cancelled"),
        (Human::Absent, "unavailable"),
        (Human::Broken, "failed"),
    ] {
        let mock = mock();
        let mut mcp = start(&config(&mock, ""), human);

        let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
        assert!(reply.is_error, "{}", reply.text);

        let entry = only_line(&mcp);
        assert_eq!(entry["approval"], approval, "{human:?}: {entry}");
        assert!(entry["reason"].is_string(), "{human:?}: {entry}");
        assert_eq!(mock.requests().len(), 0, "{human:?}: nothing was sent");
    }
}

#[test]
fn a_denied_mutation_is_recorded() {
    let mock = mock();
    let mut mcp = start(&config(&mock, "mutations = \"deny\"\n"), Human::Accepts);

    let reply = mcp.call("execute", &arguments(MUTATION, &json!({})));
    assert!(reply.is_error, "{}", reply.text);

    let entry = only_line(&mcp);
    assert_describes_the_operation(&entry, "mutation", "Remove");
    assert_eq!(entry["approval"], "denied", "{entry}");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("mutations = \"deny\"")),
        "the reason names the setting: {entry}"
    );
    assert!(entry.get("cost").is_none(), "a denied mutation is not costed: {entry}");
    assert_eq!(mock.requests().len(), 0, "nothing was sent");
}

#[test]
fn an_operation_sent_without_asking_says_why() {
    let first = mock();
    let mut mcp = start(&config(&first, "auto_approve_cost = 1000\n"), Human::Absent);
    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(!reply.is_error, "{}", reply.text);
    let entry = only_line(&mcp);
    assert_eq!(entry["approval"], "auto", "{entry}");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("auto_approve_cost")),
        "{entry}"
    );
    assert_eq!(entry["status"], 200, "{entry}");

    let other = mock();
    let mut mcp = start(&config(&other, "approval = \"client\"\n"), Human::Absent);
    let reply = mcp.call("execute", &arguments(MUTATION, &json!({})));
    assert!(!reply.is_error, "{}", reply.text);
    let entry = only_line(&mcp);
    assert_eq!(entry["approval"], "client", "{entry}");
    assert_eq!(entry["kind"], "mutation", "{entry}");
    assert_eq!(entry["status"], 200, "{entry}");
}

#[test]
fn a_request_that_gets_no_answer_is_recorded_with_the_error() {
    // Nothing listens on the discard port, so the request fails to connect.
    let config = "audit_log = \"audit.jsonl\"\n\n[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\n\
                  schema = \"schema.graphql\"\napproval = \"client\"\n";
    let mut mcp = start(config, Human::Absent);

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(reply.is_error, "{}", reply.text);

    let entry = only_line(&mcp);
    assert_eq!(entry["approval"], "client", "{entry}");
    assert!(entry.get("status").is_none(), "no status came back: {entry}");
    assert!(
        entry["error"].as_str().is_some_and(|error| error.contains("ms")),
        "the error is recorded: {entry}"
    );
}

#[test]
fn an_operation_that_is_not_valid_is_not_an_attempt() {
    let mock = mock();
    let mut mcp = start(&config(&mock, ""), Human::Accepts);

    let reply = mcp.call("execute", &arguments("{ nope }", &json!({})));
    assert!(reply.is_error, "{}", reply.text);
    let reply = mcp.call("execute", &arguments(QUERY, &json!({})));
    assert!(reply.is_error, "a missing variable is refused: {}", reply.text);

    let path = mcp.dir().join("audit.jsonl");
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(written, "", "nothing could have been sent, so nothing is recorded");
}

#[test]
fn a_secret_in_a_variable_is_redacted_and_header_values_are_never_recorded() {
    let mock = mock();
    let mut mcp = start(&config(&mock, ""), Human::Accepts);

    let reply = mcp.call(
        "execute",
        &arguments(QUERY, &json!({ "text": format!("key is {SECRET}") })),
    );
    assert!(!reply.is_error, "{}", reply.text);
    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": SECRET })));
    assert!(!reply.is_error, "{}", reply.text);

    let written = std::fs::read_to_string(mcp.dir().join("audit.jsonl")).expect("read the audit log");
    assert!(!written.contains(SECRET), "the secret is not in the log:\n{written}");
    let entries = lines(&mcp.dir().join("audit.jsonl"));
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert_eq!(entries[0]["variables"], json!({ "text": "key is <redacted>" }));
    assert_eq!(entries[1]["variables"], json!({ "text": "<redacted>" }));
    let sent = mock.requests();
    assert!(
        sent.first().is_some_and(|request| request.body.contains(SECRET)),
        "the endpoint still receives the variable as written"
    );
}

#[test]
fn audit_variables_off_leaves_the_variables_out() {
    let mock = mock();
    let mut mcp = start(&config(&mock, "audit_variables = false\n"), Human::Accepts);

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "a private search" })));
    assert!(!reply.is_error, "{}", reply.text);

    let entry = only_line(&mcp);
    assert!(entry.get("variables").is_none(), "{entry}");
    assert_eq!(entry["approval"], "accepted", "the rest is recorded: {entry}");
    let written = std::fs::read_to_string(mcp.dir().join("audit.jsonl")).expect("read the audit log");
    assert!(!written.contains("a private search"), "{written}");
}

#[test]
fn audit_variables_comes_from_a_template() {
    let mock = mock();
    let config = format!(
        "audit_log = \"audit.jsonl\"\n\n[templates.shop]\naudit_variables = false\n\n[endpoints.store]\n\
         template = \"shop\"\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    let mut mcp = start(&config, Human::Accepts);

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "a private search" })));
    assert!(!reply.is_error, "{}", reply.text);
    let entry = only_line(&mcp);
    assert!(entry.get("variables").is_none(), "{entry}");
}

#[test]
fn lines_are_appended_across_sessions() {
    let mock = mock();
    let config = config(&mock, "");
    let mut mcp = start(&config, Human::Accepts);
    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "one" })));
    assert!(!reply.is_error, "{}", reply.text);
    let path = mcp.dir().join("audit.jsonl");
    let earlier = std::fs::read_to_string(&path).expect("read the audit log");

    // A second server, with the same config file, in the same directory.
    let second = Launch::new(&config)
        .env("AUDIT_KEY", SECRET)
        .file("audit.jsonl", &earlier)
        .human(Human::Accepts);
    let mut mcp = second.start();
    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "two" })));
    assert!(!reply.is_error, "{}", reply.text);

    let entries = lines(&mcp.dir().join("audit.jsonl"));
    assert_eq!(entries.len(), 2, "the earlier line is kept: {entries:#?}");
    assert_eq!(entries[0]["variables"]["text"], "one");
    assert_eq!(entries[1]["variables"]["text"], "two");
    assert_ne!(
        entries[0]["session"], entries[1]["session"],
        "each server is its own session"
    );
}

#[test]
fn without_the_setting_no_file_is_created() {
    let mock = mock();
    let config = format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    let mut mcp = start(&config, Human::Accepts);
    let before = files(mcp.dir());

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(!reply.is_error, "{}", reply.text);
    let reply = mcp.call("execute", &arguments(MUTATION, &json!({})));
    assert!(!reply.is_error, "{}", reply.text);

    assert_eq!(files(mcp.dir()), before, "no file was written anywhere");
}

/// Every file under `dir`, relative to it, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).expect("read a directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(dir).expect("under dir").to_path_buf());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_file_and_its_folders_are_created_in_the_home_directory_for_the_user_alone() {
    let mock = mock();
    let config = format!(
        "audit_log = \"~/state/fieldtrip/audit.jsonl\"\n\n[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        mock.url()
    );
    let home = tempfile::tempdir().expect("create a home directory");
    let mut mcp = Launch::new(&config)
        .env("HOME", &home.path().display().to_string())
        .human(Human::Accepts)
        .start();

    let reply = mcp.call("execute", &arguments(QUERY, &json!({ "text": "lamp" })));
    assert!(!reply.is_error, "{}", reply.text);

    let path = home.path().join("state").join("fieldtrip").join("audit.jsonl");
    assert_eq!(lines(&path).len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("stat the log").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only the user can read the log");
    }
}

#[test]
fn a_log_that_cannot_be_written_stops_the_server() {
    // `schema.graphql` is a file, so there can be no folder of that name to put the log in.
    let stderr = Launch::new(
        "audit_log = \"schema.graphql/audit.jsonl\"\n\n[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\n",
    )
    .fails_to_start();
    assert!(stderr.contains("audit_log"), "names the setting: {stderr}");
    assert!(stderr.contains("audit.jsonl"), "names the file: {stderr}");
}

#[test]
fn audit_log_is_not_an_endpoint_setting() {
    let stderr = Launch::new("[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\naudit_log = \"audit.jsonl\"\n")
        .fails_to_start();
    assert!(stderr.contains("audit_log"), "{stderr}");
}

#[test]
fn check_shows_the_audit_log() {
    let checked = Launch::new(
        "audit_log = \"audit.jsonl\"\n\n[endpoints.store]\nurl = \"http://127.0.0.1:9/graphql\"\n\
         schema = \"schema.graphql\"\naudit_variables = false\n",
    )
    .check();
    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    let path = checked.dir.path().join("audit.jsonl");
    assert!(
        checked.stdout.contains(&format!("Audit log: {}", path.display())),
        "{}",
        checked.stdout
    );
    assert!(
        checked.stdout.contains("audit variables: not recorded"),
        "{}",
        checked.stdout
    );
    assert!(!path.exists(), "check writes nothing");
}

#[test]
fn on_protocol_2026_only_the_answered_retry_is_recorded() {
    let mock = mock();
    let state = tempfile::tempdir().expect("create a folder for the log");
    let path = state.path().join("audit.jsonl");
    let config = format!(
        "audit_log = \"{}\"\n\n[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n",
        path.display(),
        mock.url()
    );
    let client = SdkClient::start(&Launch::new(&config).human(Human::Accepts));
    let arguments = arguments(QUERY, &json!({ "text": "lamp" }));

    for action in ["decline", "accept"] {
        let CallToolResponse::InputRequired(required) = client.call_once("execute", &arguments, None) else {
            panic!("the first round asks for approval");
        };
        let retry = Retry {
            responses: InputResponses::from([(
                "approval".to_owned(),
                serde_json::from_value(json!({ "action": action })).expect("json"),
            )]),
            state: required.request_state,
        };
        let CallToolResponse::Complete(result) = client.call_once("execute", &arguments, Some(retry)) else {
            panic!("the retry finishes the call");
        };
        assert_eq!(sdk_reply(&result).is_error, action == "decline");
    }

    let entries = lines(&path);
    assert_eq!(entries.len(), 2, "one line per answered prompt: {entries:#?}");
    assert_eq!(entries[0]["approval"], "declined");
    assert_eq!(entries[1]["approval"], "accepted");
    assert_eq!(entries[1]["status"], 200);
}
