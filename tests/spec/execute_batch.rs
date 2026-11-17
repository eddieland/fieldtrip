//! Slice 60: `execute_batch` sends one query once for each of many sets of variables, with a single approval.
//!
//! The prompt shows the query, how many times it will run, the first few sets and the total cost. The result has a
//! section for each run, numbered by the position of its set, with that run's errors. Mutations are refused: each
//! one is approved on its own.

use rmcp::model::{CallToolResponse, InputRequest, InputResponses};
use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply, Response, Retry, SdkClient, sdk_reply};

const QUERY: &str = "query UserName($id: ID!) { user(id: $id) { name } }";
const MUTATION: &str = "mutation Remove($id: ID!) { deleteUser(id: $id) }";

/// A mock that answers with the name of the user asked for. `u-bad` gets a GraphQL error, and `u-slow` gets its
/// answer after two seconds.
fn mock() -> MockServer {
    MockServer::start(|request| {
        let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
        let id = body
            .pointer("/variables/id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let answer = match id {
            "u-bad" => json!({ "data": { "user": null }, "errors": [{ "message": "no such user", "path": ["user"] }] }),
            "u-slow" => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                json!({ "data": { "user": null } })
            }
            id => json!({ "data": { "user": { "name": format!("name of {id}") } } }),
        };
        Response::new(200, answer.to_string())
    })
}

/// The config of a `store` endpoint with `settings` that sends to `mock`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\n{settings}",
        mock.url()
    )
}

/// Starts a server on the older protocol revisions, whose human answers as `human` does.
fn start(mock: &MockServer, settings: &str, human: Human) -> Mcp {
    Launch::new(&config(mock, settings)).human(human).start()
}

/// The arguments of `execute_batch` for `operation` on `store`, with one set of variables for each of `ids`.
fn arguments(operation: &str, ids: &[&str]) -> Value {
    let sets: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
    json!({ "endpoint": "store", "operation": operation, "variables": sets })
}

/// Runs `operation` on `store` once for each of `ids`.
fn batch(mcp: &mut Mcp, operation: &str, ids: &[&str]) -> Reply {
    mcp.call("execute_batch", &arguments(operation, ids))
}

/// The `id` variable of each request the mock received, in order.
fn ids_sent(mock: &MockServer) -> Vec<String> {
    mock.requests()
        .iter()
        .map(|request| {
            let body: Value = serde_json::from_str(&request.body).expect("a JSON body");
            body.pointer("/variables/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[test]
fn a_batch_of_three_is_one_prompt_and_three_requests_with_results_in_order() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3"]);

    assert!(!reply.is_error, "{}", reply.text);
    let expected = format!(
        "fieldtrip wants to send a query to `store` ({}) 3 times, once for each set of variables.\n\n\
         query UserName($id: ID!) {{\n  user(id: $id) {{\n    name\n  }}\n}}\n\n\
         Variables, 3 sets:\n\
         [0] {{\"id\":\"u-1\"}}\n\
         [1] {{\"id\":\"u-2\"}}\n\
         [2] {{\"id\":\"u-3\"}}\n\
         Estimated cost: 3 in total, 1 for the costliest run\n\n\
         Accept to send all 3. Decline to stop.",
        mock.url()
    );
    assert_eq!(mcp.prompts(), [expected], "the human is asked once, about all of it");
    assert_eq!(
        mcp.forms()[0]["properties"],
        json!({}),
        "with a plain accept or decline"
    );
    assert_eq!(ids_sent(&mock), ["u-1", "u-2", "u-3"], "three requests, in order");

    assert!(
        reply.text.starts_with(
            "Sent query `UserName` to `store` 3 times, once for each set of variables. Estimated cost: 3 in total.\n"
        ),
        "{}",
        reply.text
    );
    let sections: Vec<&str> = reply.text.split("\n\n").skip(1).collect();
    assert_eq!(sections.len(), 3, "one section for each run: {}", reply.text);
    for (index, section) in sections.iter().enumerate() {
        assert!(
            section.starts_with(&format!("[{index}] HTTP 200 · ")),
            "run {index} starts with its position and status: {section}"
        );
        assert!(
            section.ends_with(&format!(
                "\ndata: {{\"user\":{{\"name\":\"name of u-{}\"}}}}",
                index + 1
            )),
            "run {index} has its own data: {section}"
        );
    }
}

#[test]
fn the_request_of_each_run_is_the_same_operation_with_its_own_variables() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Accepts);

    batch(&mut mcp, QUERY, &["u-1", "u-2"]);

    let bodies: Vec<Value> = mock
        .requests()
        .iter()
        .map(|request| serde_json::from_str(&request.body).expect("a JSON body"))
        .collect();
    assert_eq!(bodies[0]["query"], bodies[1]["query"]);
    assert_eq!(bodies[0]["operationName"], "UserName");
    assert_eq!(bodies[1]["variables"], json!({ "id": "u-2" }));
}

#[test]
fn the_errors_of_one_run_are_shown_with_it_and_do_not_stop_the_others() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-bad", "u-3"]);

    assert!(!reply.is_error, "{}", reply.text);
    let sections: Vec<&str> = reply.text.split("\n\n").skip(1).collect();
    assert!(
        sections[1].contains("\nerrors: [{\"message\":\"no such user\",\"path\":[\"user\"]}]\ndata: {\"user\":null}"),
        "the failed run shows its errors: {}",
        sections[1]
    );
    assert!(!sections[0].contains("errors"), "the others have none: {}", sections[0]);
    assert!(sections[2].contains("name of u-3"), "and the run after it was sent");
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn a_decline_sends_none_of_the_runs() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Declines);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3"]);

    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("The user declined. Nothing was sent"),
        "{}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 1);
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn a_client_that_cannot_prompt_sends_nothing() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Absent);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-2"]);

    assert!(reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("does not support elicitation"), "{}", reply.text);
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn a_mutation_is_refused_without_a_prompt() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = batch(&mut mcp, MUTATION, &["u-1", "u-2"]);

    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("`execute_batch` runs queries only") && reply.text.contains("`execute`"),
        "the refusal says what to use instead: {}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 0, "nobody is asked");
    assert_eq!(mock.requests().len(), 0, "and nothing is sent");

    let client_approves = mock;
    let mut mcp = start(&client_approves, "approval = \"client\"\n", Human::Absent);
    let reply = batch(&mut mcp, MUTATION, &["u-1"]);
    assert!(
        reply.is_error,
        "even when approval is left to the client: {}",
        reply.text
    );
    assert_eq!(client_approves.requests().len(), 0);
}

#[test]
fn a_set_that_does_not_fit_the_operation_stops_the_batch_before_anyone_is_asked() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = mcp.call(
        "execute_batch",
        &json!({ "endpoint": "store", "operation": QUERY, "variables": [{ "id": "u-1" }, {}, { "id": "u-3" }] }),
    );

    assert!(reply.is_error, "{}", reply.text);
    assert!(
        reply.text.starts_with("`variables[1]`: ") && reply.text.ends_with("Nothing was sent."),
        "the error names the set: {}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 0);
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn the_number_of_runs_is_capped_by_max_batch() {
    let mock = mock();
    let mut mcp = start(&mock, "max_batch = 2\n", Human::Accepts);

    let too_many = batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3"]);
    let none = batch(&mut mcp, QUERY, &[]);

    assert!(too_many.is_error, "{}", too_many.text);
    assert!(
        too_many
            .text
            .contains("has 3 sets, and one call to `execute_batch` on `store` sends at most 2 (`max_batch`"),
        "{}",
        too_many.text
    );
    assert!(
        none.is_error && none.text.contains("`variables` is empty"),
        "{}",
        none.text
    );
    assert_eq!(mcp.prompts().len(), 0);
    assert_eq!(mock.requests().len(), 0);

    let allowed = batch(&mut mcp, QUERY, &["u-1", "u-2"]);
    assert!(!allowed.is_error, "{}", allowed.text);

    let ids: Vec<String> = (0..101).map(|index| format!("u-{index}")).collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let mut by_default = start(&mock, "", Human::Accepts);
    let reply = batch(&mut by_default, QUERY, &ids);
    assert!(
        reply.is_error && reply.text.contains("sends at most 100"),
        "the cap is 100 unless set: {}",
        reply.text
    );
}

#[test]
fn max_batch_comes_from_a_template_is_shown_by_check_and_cannot_be_zero() {
    let mock = MockServer::unreachable();
    let templated = format!(
        "[templates.api]\nschema = \"schema.graphql\"\nmax_batch = 7\n\n\
         [endpoints.a]\ntemplate = \"api\"\nurl = \"{url}\"\n\n\
         [endpoints.b]\ntemplate = \"api\"\nurl = \"{url}\"\nmax_batch = 3\n",
        url = mock.url()
    );
    let checked = Launch::new(&templated).check();
    assert!(checked.passed, "{}{}", checked.stdout, checked.stderr);
    let lines: Vec<&str> = checked
        .stdout
        .lines()
        .filter(|line| line.starts_with("  max batch: "))
        .collect();
    assert_eq!(lines, ["  max batch: 7", "  max batch: 3"]);

    let stderr = Launch::new(&config(&mock, "max_batch = 0\n")).fails_to_start();
    assert!(
        stderr.contains("`max_batch`") && stderr.contains("at least 1"),
        "{stderr}"
    );
}

#[test]
fn the_prompt_shows_the_first_five_sets_and_counts_the_rest() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::Declines);

    batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3", "u-4", "u-5", "u-6", "u-7"]);

    let prompt = &mcp.prompts()[0];
    assert!(
        prompt.contains(
            "Variables, 7 sets:\n[0] {\"id\":\"u-1\"}\n[1] {\"id\":\"u-2\"}\n[2] {\"id\":\"u-3\"}\n\
             [3] {\"id\":\"u-4\"}\n[4] {\"id\":\"u-5\"}\n… and 2 more sets, which are not shown.\n\
             Estimated cost: 7 in total, 1 for the costliest run\n\nAccept to send all 7."
        ),
        "{prompt}"
    );
}

#[test]
fn the_total_cost_is_what_auto_approve_cost_is_compared_with() {
    let mock = mock();
    let mut mcp = start(&mock, "auto_approve_cost = 2\n", Human::Declines);

    let within = batch(&mut mcp, QUERY, &["u-1", "u-2"]);
    let above = batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3"]);

    assert!(!within.is_error, "a total of 2 runs unasked: {}", within.text);
    assert!(
        within.text.contains(
            "Estimated cost: 2 in total. The user was not asked: the cost is within `auto_approve_cost` (2)."
        ),
        "{}",
        within.text
    );
    assert!(
        above.is_error,
        "a total of 3 is asked about, and declined: {}",
        above.text
    );
    assert_eq!(mcp.prompts().len(), 1);
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn a_query_approved_for_the_session_runs_as_a_batch_unasked() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::AcceptsForSession);

    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": QUERY, "variables": { "id": "u-1" } }),
    );
    let reply = batch(&mut mcp, QUERY, &["u-2", "u-3"]);

    assert!(!reply.is_error, "{}", reply.text);
    assert!(
        reply.text.contains("The user was not asked: they approved this query"),
        "{}",
        reply.text
    );
    assert_eq!(mcp.prompts().len(), 1, "only `execute` asked");
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn a_batch_accepted_with_the_session_chosen_approves_only_the_batch() {
    let mock = mock();
    let mut mcp = start(&mock, "", Human::AcceptsForSession);

    batch(&mut mcp, QUERY, &["u-1", "u-2"]);
    batch(&mut mcp, QUERY, &["u-1", "u-2"]);

    assert_eq!(
        mcp.prompts().len(),
        2,
        "a batch's prompt has no field, so each batch is asked about"
    );
}

#[test]
fn select_picks_the_same_part_of_each_run() {
    let mock = mock();
    let mut mcp = start(&mock, "approval = \"client\"\n", Human::Absent);

    let mut arguments = arguments(QUERY, &["u-1", "u-bad"]);
    arguments["select"] = json!("data.user.name");
    let reply = mcp.call("execute_batch", &arguments);

    assert!(!reply.is_error, "{}", reply.text);
    let sections: Vec<&str> = reply.text.split("\n\n").skip(1).collect();
    assert!(
        sections[0].ends_with("\nSelected `data.user.name`.\nselected: \"name of u-1\""),
        "{}",
        sections[0]
    );
    assert!(
        sections[1].contains("`select` matched nothing")
            && sections[1].contains("errors: [{\"message\":\"no such user\""),
        "a run where the path misses says so and keeps its errors: {}",
        sections[1]
    );
}

#[test]
fn each_run_gets_a_share_of_the_result() {
    let long = "x".repeat(3_000);
    let body = json!({ "data": { "user": { "name": long } } }).to_string();
    let mock = MockServer::start(move |_| Response::new(200, body.clone()));
    let mut mcp = start(&mock, "approval = \"client\"\nmax_output_chars = 4000\n", Human::Absent);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-2", "u-3", "u-4"]);

    assert!(!reply.is_error, "{}", reply.text);
    assert!(
        reply.text.chars().count() <= 4_000,
        "the result fits: {}",
        reply.text.chars().count()
    );
    let sections: Vec<&str> = reply.text.split("\n\n").skip(1).collect();
    assert_eq!(sections.len(), 4, "every run is there");
    for section in sections {
        assert!(
            section.contains("more characters)") && section.contains("that each response gets of a result"),
            "each run is summarized to its share: {section}"
        );
    }
}

#[test]
fn a_run_that_gets_no_answer_stops_the_batch() {
    let mock = mock();
    let mut mcp = start(&mock, "approval = \"client\"\ntimeout = 1\n", Human::Absent);

    let reply = batch(&mut mcp, QUERY, &["u-1", "u-slow", "u-3", "u-4"]);

    assert!(!reply.is_error, "the first run was answered: {}", reply.text);
    assert!(reply.text.contains("\n\n[0] HTTP 200 · "), "{}", reply.text);
    assert!(reply.text.contains("\n\n[1] No answer: "), "{}", reply.text);
    assert!(
        reply
            .text
            .ends_with("[The 2 runs after `[1]` were not sent: a batch stops at the first request that gets no answer, since the rest would likely fail the same way.]"),
        "{}",
        reply.text
    );
    assert_eq!(ids_sent(&mock), ["u-1", "u-slow"]);
}

#[test]
fn the_audit_log_has_a_line_for_each_run_and_one_for_a_batch_that_was_not_sent() {
    let mock = mock();
    let audited = format!("audit_log = \"audit.jsonl\"\n\n{}", config(&mock, ""));
    let lines = |mcp: &Mcp| -> Vec<Value> {
        std::fs::read_to_string(mcp.dir().join("audit.jsonl"))
            .expect("read the audit log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    };

    let mut accepting = Launch::new(&audited).human(Human::Accepts).start();
    batch(&mut accepting, QUERY, &["u-1", "u-bad"]);
    let sent = lines(&accepting);
    assert_eq!(sent.len(), 2, "one line for each request: {sent:?}");
    assert_eq!(sent[0]["batch"], json!({ "run": 1, "of": 2 }));
    assert_eq!(sent[1]["batch"], json!({ "run": 2, "of": 2 }));
    assert_eq!(sent[1]["variables"], json!({ "id": "u-bad" }));
    assert_eq!(sent[1]["approval"], "accepted");
    assert_eq!(sent[1]["errors"], 1);
    assert_eq!(sent[0]["cost"], 1);

    let mut declining = Launch::new(&audited).human(Human::Declines).start();
    batch(&mut declining, QUERY, &["u-1", "u-2", "u-3"]);
    let unsent = lines(&declining);
    assert_eq!(unsent.len(), 1, "one line for the batch: {unsent:?}");
    assert_eq!(unsent[0]["approval"], "declined");
    assert_eq!(unsent[0]["batch"], json!({ "of": 3 }));
    assert_eq!(
        unsent[0]["variables"],
        json!([{ "id": "u-1" }, { "id": "u-2" }, { "id": "u-3" }])
    );
    assert_eq!(unsent[0]["cost"], 3);
    assert!(unsent[0].get("status").is_none());
}

/// Sends one round of `execute_batch` on a 2026-07-28 session.
fn round(client: &SdkClient, ids: &[&str], retry: Option<Retry>) -> CallToolResponse {
    client.call_once("execute_batch", &arguments(QUERY, ids), retry)
}

#[test]
fn on_revision_2026_a_batch_is_one_prompt_and_three_requests() {
    let mock = mock();
    let client = SdkClient::start(&Launch::new(&config(&mock, "")).human(Human::Accepts));

    let reply = client.call("execute_batch", &arguments(QUERY, &["u-1", "u-2", "u-3"]));

    assert!(!reply.is_error, "{}", reply.text);
    let prompts = client.prompts();
    assert_eq!(prompts.len(), 1, "the human is asked once");
    assert!(
        prompts[0].contains("3 times, once for each set of variables"),
        "{}",
        prompts[0]
    );
    assert_eq!(ids_sent(&mock), ["u-1", "u-2", "u-3"]);
    assert!(reply.text.contains("\n\n[2] HTTP 200 · "), "{}", reply.text);
}

#[test]
fn on_revision_2026_an_approval_covers_exactly_the_sets_that_were_shown() {
    let mock = mock();
    let client = SdkClient::start(&Launch::new(&config(&mock, "")).human(Human::Accepts));
    let CallToolResponse::InputRequired(required) = round(&client, &["u-1", "u-2"], None) else {
        panic!("the first round asks for input");
    };
    let requests = required.input_requests.expect("the server asks for something");
    assert!(
        matches!(requests.get("approval"), Some(InputRequest::Elicitation(_))),
        "{requests:?}"
    );
    let accept = |state: Option<String>| Retry {
        responses: InputResponses::from([("approval".to_owned(), json!({ "action": "accept" }))]),
        state,
    };
    let state = required.request_state;

    // The human saw two sets. The retry asks to run three, and then to run one of them with `execute`.
    let CallToolResponse::Complete(more) = round(&client, &["u-1", "u-2", "u-3"], Some(accept(state.clone()))) else {
        panic!("a retry finishes the call");
    };
    let other_tool = client.call_once(
        "execute",
        &json!({ "endpoint": "store", "operation": QUERY, "variables": { "id": "u-1" } }),
        Some(accept(state.clone())),
    );
    let CallToolResponse::Complete(other_tool) = other_tool else {
        panic!("a retry finishes the call");
    };
    assert!(sdk_reply(&more).is_error, "an approval for other sets is refused");
    assert!(
        sdk_reply(&other_tool).is_error,
        "an approval for a batch is no use to `execute`"
    );
    assert_eq!(mock.requests().len(), 0, "nothing was sent");

    let CallToolResponse::Complete(same) = round(&client, &["u-1", "u-2"], Some(accept(state))) else {
        panic!("a retry finishes the call");
    };
    assert!(!sdk_reply(&same).is_error, "the batch that was shown runs");
    assert_eq!(ids_sent(&mock), ["u-1", "u-2"]);
}
