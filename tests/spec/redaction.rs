//! Slice 13: header values never reach the calling agent, the approval prompt or the log.

use serde_json::json;

use crate::support::{FixtureApi, Human, Launch, MockServer, Response};

/// The literal header's value.
const LITERAL: &str = "lit-sekrit-0001";
/// The value of the environment variable.
const FROM_ENV: &str = "env-sekrit-0002";
/// What the header command prints.
const FROM_COMMAND: &str = "cmd-sekrit-0003";
/// A secret-looking word inside a `format`.
const IN_FORMAT: &str = "fmt-sekrit-0004";

/// Every string that must never be shown.
const SECRETS: [&str; 4] = [LITERAL, FROM_ENV, FROM_COMMAND, IN_FORMAT];

/// A config with all three plugs on a `live` endpoint that is introspected through `mock`.
fn config(mock: &MockServer) -> String {
    format!(
        "[endpoints.live]\nurl = \"{url}\"\n\n[endpoints.live.headers]\n\
         X-Literal = \"{LITERAL}\"\n\
         X-Env     = {{ env = \"FIELDTRIP_SPEC_SECRET\", format = \"{IN_FORMAT} {{}}\" }}\n\
         X-Command = {{ command = [\"sh\", \"token.sh\", \"echo\", \"{FROM_COMMAND}\"], format = \"Bearer {{}}\" }}\n",
        url = mock.url()
    )
}

/// Fails if `text` contains any of the secrets. `place` says where the text came from.
fn assert_clean(place: &str, text: &str) {
    for secret in SECRETS {
        assert!(
            !text.contains(secret),
            "{place} contains the secret `{secret}`:\n{text}"
        );
    }
}

#[test]
fn no_tool_result_prompt_or_log_line_contains_a_header_value() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&config(&mock))
        .env("FIELDTRIP_SPEC_SECRET", FROM_ENV)
        .env("FIELDTRIP_LOG", "trace")
        .human(Human::Accepts)
        .start();

    let calls = [
        ("list_endpoints", json!({})),
        ("introspect", json!({ "endpoint": "live" })),
        ("introspect", json!({ "endpoint": "live", "refresh": true })),
        ("describe", json!({ "endpoint": "live", "name": "User" })),
        ("describe", json!({ "endpoint": "live", "name": "Nope" })),
        ("search_schema", json!({ "endpoint": "live", "query": "user" })),
        (
            "estimate_cost",
            json!({ "endpoint": "live", "operation": "{ viewer { name } }" }),
        ),
        (
            "estimate_cost",
            json!({ "endpoint": "live", "operation": "{ viewer { nope } }" }),
        ),
        (
            "execute",
            json!({ "endpoint": "live", "operation": "{ viewer { name } }" }),
        ),
        (
            "execute",
            json!({ "endpoint": "live", "operation": "mutation { deleteUser(id: \"u-1\") }" }),
        ),
        (
            "execute",
            json!({ "endpoint": "nope", "operation": "{ viewer { name } }" }),
        ),
    ];
    for (tool, arguments) in calls {
        let reply = mcp.call(tool, &arguments);
        assert_clean(&format!("the result of {tool} {arguments}"), &reply.text);
    }
    assert_eq!(mcp.prompts().len(), 2, "both executions prompted");
    for prompt in mcp.prompts() {
        assert_clean("an approval prompt", prompt);
    }

    // The headers did go out, so the checks above were looking at a server that had the secrets in hand.
    let requests = mock.requests();
    let last = requests.last().expect("at least one request");
    assert_eq!(last.header("x-literal"), Some(LITERAL), "the literal header was sent");
    assert_eq!(
        last.header("x-env"),
        Some(format!("{IN_FORMAT} {FROM_ENV}").as_str()),
        "the env header was sent"
    );
    assert_eq!(
        last.header("x-command"),
        Some(format!("Bearer {FROM_COMMAND}").as_str()),
        "the command header was sent"
    );

    let stderr = mcp.finish();
    assert!(
        stderr.contains("list_endpoints"),
        "trace logging was on, so the log is not empty:\n{stderr}"
    );
    assert_clean("the log on stderr", &stderr);
}

#[test]
fn a_secret_the_server_echoes_back_is_removed_from_the_result() {
    // Some servers repeat the credential they were given, in an error message or a debug field.
    let mock = MockServer::start(|request| {
        let env = request.header("x-env").unwrap_or_default();
        let command = request.header("x-command").unwrap_or_default();
        let body = json!({
            "data": { "viewer": null },
            "errors": [{ "message": format!("rejected credentials: {env} / {command}") }],
        });
        Response::new(200, body.to_string())
    });
    let config = config(&mock).replace("[endpoints.live]\n", "[endpoints.live]\nschema = \"schema.graphql\"\n");
    let mut mcp = Launch::new(&config)
        .env("FIELDTRIP_SPEC_SECRET", FROM_ENV)
        .human(Human::Accepts)
        .start();

    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "live", "operation": "{ viewer { name } }" }),
    );

    assert!(
        reply.text.contains("rejected credentials"),
        "the server's message still comes through: {}",
        reply.text
    );
    assert!(
        reply.text.contains("<redacted>"),
        "the echoed values are replaced: {}",
        reply.text
    );
    assert!(
        !reply.text.contains(FROM_ENV),
        "the env value is removed: {}",
        reply.text
    );
    assert!(
        !reply.text.contains(FROM_COMMAND),
        "the command output is removed: {}",
        reply.text
    );
}

#[test]
fn a_refused_credential_does_not_quote_it() {
    let mock = MockServer::start(|request| {
        let command = request.header("x-command").unwrap_or_default();
        Response::new(401, format!("{{\"errors\":[{{\"message\":\"bad token {command}\"}}]}}"))
    });
    let mut mcp = Launch::new(&config(&mock))
        .env("FIELDTRIP_SPEC_SECRET", FROM_ENV)
        .start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a 401 is an error: {}", reply.text);
    assert_clean("the result of a refused introspection", &reply.text);
}

#[test]
fn a_value_that_cannot_be_a_header_is_not_quoted() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&config(&mock))
        .env("FIELDTRIP_SPEC_SECRET", "env-sekrit-0002\nsecond line")
        .start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(
        reply.is_error,
        "a value with a line break cannot be sent: {}",
        reply.text
    );
    assert!(reply.text.contains("`X-Env`"), "names the header: {}", reply.text);
    assert_clean("the result", &reply.text);
    assert_eq!(mock.requests().len(), 0, "nothing is sent");
}

#[test]
fn a_config_that_does_not_parse_does_not_quote_the_line() {
    let config = format!(
        "[endpoints.live]\nurl = \"http://localhost:9/graphql\"\n\n[endpoints.live.headers]\nX-Literal = \"{LITERAL}\n"
    );

    let stderr = Launch::new(&config).fails_to_start();

    assert!(stderr.contains("line 5"), "says where the problem is: {stderr}");
    assert_clean("the startup error", &stderr);
}
