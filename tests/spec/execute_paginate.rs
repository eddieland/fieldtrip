//! Slice 61: `execute` with `paginate` follows the pages of one connection under a single approval.
//!
//! fieldtrip sends the query, reads `pageInfo` at the path given, and sends it again with `endCursor` as the cursor
//! variable, until the last page or `max_pages`. The prompt shows the worst case: `max_pages` times the estimate for
//! one page.

use rmcp::model::{CallToolResponse, InputResponses};
use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply, Response, Retry, SdkClient, sdk_reply};

const QUERY: &str = "query Users($after: String) { users(first: 2, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } }";
const QUERY_TEXT: &str = "query Users($after: String) {\n  users(first: 2, after: $after) {\n    nodes {\n      id\n    }\n    \
                          pageInfo {\n      hasNextPage\n      endCursor\n    }\n  }\n}";

/// The cursor variable of a request, `after` or `from`, when it has one.
fn after(body: &str) -> Option<String> {
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    ["/variables/after", "/variables/from"]
        .iter()
        .find_map(|pointer| body.pointer(pointer).and_then(Value::as_str).map(str::to_owned))
}

/// A mock whose `users` connection has `pages` pages. Page `n` ends with the cursor `c<n>`.
fn mock(pages: usize) -> MockServer {
    MockServer::start(move |request| {
        let page = after(&request.body)
            .and_then(|cursor| cursor.strip_prefix('c')?.parse::<usize>().ok())
            .map_or(1, |last| last + 1);
        let body = json!({ "data": { "users": {
            "nodes": [{ "id": format!("u-{page}a") }, { "id": format!("u-{page}b") }],
            "pageInfo": { "hasNextPage": page < pages, "endCursor": format!("c{page}") },
        } } });
        Response::new(200, body.to_string())
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

/// The arguments of `execute` for [`QUERY`] on `store`, following `data.users` for at most `max_pages`.
fn arguments(max_pages: usize) -> Value {
    json!({
        "endpoint": "store",
        "operation": QUERY,
        "paginate": { "path": "data.users", "max_pages": max_pages },
    })
}

/// Executes [`QUERY`] on `store` with `paginate`.
fn paginate(mcp: &mut Mcp, paginate: &Value) -> Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": QUERY, "paginate": paginate }),
    )
}

/// The `after` variable of each request the mock received, in order.
fn cursors_sent(mock: &MockServer) -> Vec<Option<String>> {
    mock.requests().iter().map(|request| after(&request.body)).collect()
}

#[test]
fn two_pages_of_five_are_two_requests_and_the_result_says_more_remain() {
    let mock = mock(5);
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = mcp.call("execute", &arguments(2));

    assert!(!reply.is_error, "{}", reply.text);
    let expected = format!(
        "fieldtrip wants to send a query to `store` ({}) up to 2 times, to follow the pages of `data.users`.\n\n\
         {QUERY_TEXT}\n\n\
         Variables: none\n\
         After the first page, `$after` is the `endCursor` of the page before.\n\
         Estimated cost: 8 at most, 4 for each page\n\n\
         Accept to send up to 2 pages. Decline to stop.",
        mock.url()
    );
    assert_eq!(
        mcp.prompts(),
        [expected],
        "the human is asked once, about the worst case"
    );
    assert_eq!(
        mcp.forms()[0]["properties"],
        json!({}),
        "with a plain accept or decline"
    );
    assert_eq!(
        cursors_sent(&mock),
        [None, Some("c1".to_owned())],
        "two requests, the second with the first page's cursor"
    );

    let mut sections = reply.text.split("\n\n");
    assert_eq!(
        sections.next(),
        Some(
            "Sent query `Users` to `store` 2 times, following the pages of `data.users`. Estimated cost: 8 at most.\n\
             More pages remain: `data.users` still has `hasNextPage: true` after 2 pages, which is the `max_pages` \
             given. Its `endCursor` is \"c2\". To go on, call again with `after` set to it in `variables`."
        )
    );
    let first = sections.next().expect("the first page");
    let second = sections.next().expect("the second page");
    assert!(first.starts_with("[page 1] HTTP 200 · "), "{first}");
    assert!(first.contains("{\"id\":\"u-1a\"}"), "{first}");
    assert!(second.starts_with("[page 2] HTTP 200 · "), "{second}");
    assert!(second.contains("{\"id\":\"u-2b\"}"), "{second}");
    assert_eq!(sections.next(), None, "{}", reply.text);
    assert!(
        !reply.text.contains("More pages: "),
        "the hint for a single request is not repeated on each page: {}",
        reply.text
    );
}

#[test]
fn the_last_page_ends_it_before_max_pages() {
    let mock = mock(3);
    let mut mcp = start(&mock, "", Human::Accepts);

    let reply = mcp.call("execute", &arguments(5));

    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(mock.requests().len(), 3, "it stops at the last page");
    assert!(
        reply.text.starts_with(
            "Sent query `Users` to `store` 3 times, following the pages of `data.users`. Estimated cost: 20 at \
             most.\nThat was every page: `data.users` has `hasNextPage: false` after 3 pages.\n\n[page 1] "
        ),
        "{}",
        reply.text
    );
    assert!(reply.text.contains("\n\n[page 3] HTTP 200 · "), "{}", reply.text);
}

#[test]
fn the_cursor_starts_from_the_variables_given_and_can_have_another_name() {
    let mock = mock(5);
    let mut mcp = start(&mock, "approval = \"client\"\n", Human::Absent);

    let reply = mcp.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": "query Users($from: String, $n: Int) { users(first: $n, after: $from) { pageInfo { hasNextPage endCursor } } }",
            "variables": { "from": "c2", "n": 2 },
            "paginate": { "path": "data.users", "max_pages": 2, "variable": "from" },
        }),
    );

    assert!(!reply.is_error, "{}", reply.text);
    let bodies: Vec<Value> = mock
        .requests()
        .iter()
        .map(|request| serde_json::from_str(&request.body).expect("a JSON body"))
        .collect();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["variables"], json!({ "from": "c2", "n": 2 }));
    assert_eq!(
        bodies[1]["variables"],
        json!({ "from": "c3", "n": 2 }),
        "the other variables stay"
    );
    assert!(
        reply.text.contains("call again with `from` set to it"),
        "{}",
        reply.text
    );
}

#[test]
fn max_pages_is_five_unless_given_and_capped_by_max_batch() {
    let mock = mock(9);
    let mut mcp = start(&mock, "max_batch = 6\n", Human::Accepts);

    let by_default = paginate(&mut mcp, &json!({ "path": "data.users" }));
    assert!(!by_default.is_error, "{}", by_default.text);
    assert_eq!(mock.requests().len(), 5, "five pages by default");
    assert!(mcp.prompts()[0].contains("up to 5 times"), "{}", mcp.prompts()[0]);

    let too_many = paginate(&mut mcp, &json!({ "path": "data.users", "max_pages": 7 }));
    let none = paginate(&mut mcp, &json!({ "path": "data.users", "max_pages": 0 }));
    assert!(
        too_many.is_error
            && too_many.text.contains("at most 6 (`max_batch`")
            && too_many.text.contains("Nothing was sent"),
        "{}",
        too_many.text
    );
    assert!(
        none.is_error && none.text.contains("max_pages` must be at least 1"),
        "{}",
        none.text
    );
    assert_eq!(mcp.prompts().len(), 1, "neither was asked about");
    assert_eq!(mock.requests().len(), 5, "nor sent");
}

#[test]
fn what_cannot_be_followed_is_refused_before_anyone_is_asked() {
    let mock = mock(5);
    let mut mcp = start(&mock, "", Human::Accepts);
    let call = |mcp: &mut Mcp, extra: Value| {
        let mut arguments = json!({ "endpoint": "store", "operation": QUERY });
        for (key, value) in extra.as_object().expect("an object") {
            arguments[key] = value.clone();
        }
        mcp.call("execute", &arguments)
    };

    let cases = [
        (json!({ "paginate": { "path": "users" } }), "starts with `data`"),
        (
            json!({ "paginate": { "path": "data.users.nodes[0]" } }),
            "names joined by `.`",
        ),
        (json!({ "paginate": { "path": "data" } }), "names joined by `.`"),
        (
            json!({ "paginate": { "path": "data.users", "variable": "cursor" } }),
            "does not declare `$cursor`",
        ),
        (json!({ "paginate": { "path": "data.users" }, "save": true }), "`save`"),
        (
            json!({
                "operation": "mutation M($after: ID!) { deleteUser(id: $after) }",
                "paginate": { "path": "data.deleteUser" },
            }),
            "queries only",
        ),
    ];
    for (extra, expected) in cases {
        let reply = call(&mut mcp, extra.clone());
        assert!(reply.is_error, "{extra}: {}", reply.text);
        assert!(
            reply.text.contains(expected),
            "{extra} should mention {expected}: {}",
            reply.text
        );
        assert!(reply.text.contains("Nothing was sent"), "{extra}: {}", reply.text);
    }
    assert_eq!(mcp.prompts().len(), 0, "nobody is asked");
    assert_eq!(mock.requests().len(), 0, "and nothing is sent");
}

#[test]
fn a_decline_sends_no_page() {
    let mock = mock(5);
    let mut mcp = start(&mock, "", Human::Declines);

    let reply = mcp.call("execute", &arguments(3));

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
fn the_worst_case_is_what_auto_approve_cost_is_compared_with() {
    let mock = mock(5);
    let mut mcp = start(&mock, "auto_approve_cost = 8\n", Human::Declines);

    let within = mcp.call("execute", &arguments(2));
    let above = mcp.call("execute", &arguments(3));

    assert!(!within.is_error, "two pages cost 8 at most: {}", within.text);
    assert!(
        within
            .text
            .contains("Estimated cost: 8 at most. The user was not asked: the cost is within `auto_approve_cost` (8)."),
        "{}",
        within.text
    );
    assert!(
        above.is_error,
        "three pages cost 12 at most, and the human declined: {}",
        above.text
    );
    assert_eq!(mcp.prompts().len(), 1);
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn a_query_approved_for_the_session_is_followed_unasked() {
    let mock = mock(5);
    let mut mcp = start(&mock, "", Human::AcceptsForSession);

    mcp.call("execute", &json!({ "endpoint": "store", "operation": QUERY }));
    let followed = mcp.call("execute", &arguments(2));
    assert!(
        followed
            .text
            .contains("The user was not asked: they approved this query"),
        "{}",
        followed.text
    );
    assert_eq!(mcp.prompts().len(), 1, "only the single request asked");

    // Accepting a prompt about pages approves those pages and no more, whatever the answer holds.
    let other = mock;
    let mut mcp = start(&other, "", Human::AcceptsForSession);
    mcp.call("execute", &arguments(2));
    mcp.call("execute", &arguments(2));
    assert_eq!(mcp.prompts().len(), 2, "a prompt about pages has no field");
}

#[test]
fn a_page_without_the_connection_stops_it_and_says_why() {
    let answers = |second: Value| {
        MockServer::start(move |request| {
            let body = if after(&request.body).is_none() {
                json!({ "data": { "users": { "nodes": [], "pageInfo": { "hasNextPage": true, "endCursor": "c1" } } } })
            } else {
                second.clone()
            };
            Response::new(200, body.to_string())
        })
    };

    let failing = answers(json!({ "data": null, "errors": [{ "message": "rate limited" }] }));
    let mut mcp = start(&failing, "approval = \"client\"\n", Human::Absent);
    let reply = mcp.call("execute", &arguments(4));
    assert!(!reply.is_error, "the first page arrived: {}", reply.text);
    assert_eq!(failing.requests().len(), 2, "it stops at the page that failed");
    assert!(
        reply.text.contains(
            "\nStopped after page 2: its response has no `pageInfo` at `data.users`, so there is no cursor to go on \
             from. Read that page's `errors`, and check that the path leads to the object that selects `pageInfo`. The \
             last cursor was \"c1\"."
        ),
        "{}",
        reply.text
    );
    assert!(
        reply.text.contains("\n\n[page 2] HTTP 200 · ")
            && reply.text.contains("errors: [{\"message\":\"rate limited\"}]"),
        "the page that failed is shown with its errors: {}",
        reply.text
    );

    let stuck = answers(
        json!({ "data": { "users": { "nodes": [], "pageInfo": { "hasNextPage": true, "endCursor": "c1" } } } }),
    );
    let mut mcp = start(&stuck, "approval = \"client\"\n", Human::Absent);
    let reply = mcp.call("execute", &arguments(4));
    assert_eq!(stuck.requests().len(), 2, "a cursor that does not move is not followed");
    assert!(
        reply
            .text
            .contains("\nStopped after page 2: it gave the same `endCursor` as the page before"),
        "{}",
        reply.text
    );

    let wrong_path = mock(5);
    let mut mcp = start(&wrong_path, "approval = \"client\"\n", Human::Absent);
    let reply = paginate(&mut mcp, &json!({ "path": "data.users.nodes" }));
    assert_eq!(wrong_path.requests().len(), 1);
    assert!(
        reply
            .text
            .contains("\nStopped after page 1: its response has no `pageInfo` at `data.users.nodes`"),
        "{}",
        reply.text
    );
}

#[test]
fn select_picks_the_same_part_of_each_page() {
    let mock = mock(5);
    let mut mcp = start(&mock, "approval = \"client\"\n", Human::Absent);

    let mut arguments = arguments(2);
    arguments["select"] = json!("data.users.nodes[*].id");
    let reply = mcp.call("execute", &arguments);

    assert!(!reply.is_error, "{}", reply.text);
    assert!(reply.text.contains("selected: [\"u-1a\",\"u-1b\"]"), "{}", reply.text);
    assert!(reply.text.contains("selected: [\"u-2a\",\"u-2b\"]"), "{}", reply.text);
    assert!(
        !reply.text.contains("pageInfo"),
        "only the selection is shown: {}",
        reply.text
    );
}

#[test]
fn the_audit_log_has_a_line_for_each_page() {
    let mock = mock(5);
    let audited = format!("audit_log = \"audit.jsonl\"\n\n{}", config(&mock, ""));
    let lines = |mcp: &Mcp| -> Vec<Value> {
        std::fs::read_to_string(mcp.dir().join("audit.jsonl"))
            .expect("read the audit log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    };

    let mut accepting = Launch::new(&audited).human(Human::Accepts).start();
    accepting.call("execute", &arguments(2));
    let sent = lines(&accepting);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0]["page"], json!({ "page": 1, "max": 2 }));
    assert_eq!(sent[0]["variables"], json!({}));
    assert_eq!(sent[1]["page"], json!({ "page": 2, "max": 2 }));
    assert_eq!(sent[1]["variables"], json!({ "after": "c1" }));
    assert_eq!(sent[1]["approval"], "accepted");
    assert_eq!(sent[1]["cost"], 4);

    let mut declining = Launch::new(&audited).human(Human::Declines).start();
    declining.call("execute", &arguments(4));
    let unsent = lines(&declining);
    assert_eq!(unsent.len(), 1, "{unsent:?}");
    assert_eq!(unsent[0]["approval"], "declined");
    assert_eq!(unsent[0]["page"], json!({ "max": 4 }));
    assert_eq!(unsent[0]["cost"], 16, "the worst case");
}

#[test]
fn on_revision_2026_the_pages_are_followed_after_one_approval_that_covers_just_them() {
    let mock = mock(5);
    let client = SdkClient::start(&Launch::new(&config(&mock, "")).human(Human::Accepts));

    let reply = client.call("execute", &arguments(2));
    assert!(!reply.is_error, "{}", reply.text);
    assert_eq!(client.prompts().len(), 1, "the human is asked once");
    assert_eq!(mock.requests().len(), 2);

    let CallToolResponse::InputRequired(required) = client.call_once("execute", &arguments(2), None) else {
        panic!("the first round asks for input");
    };
    let accept = |state: Option<String>| Retry {
        responses: InputResponses::from([("approval".to_owned(), json!({ "action": "accept" }))]),
        state,
    };
    // The human saw two pages. The retry asks for four.
    let CallToolResponse::Complete(more) =
        client.call_once("execute", &arguments(4), Some(accept(required.request_state)))
    else {
        panic!("a retry finishes the call");
    };
    assert!(
        sdk_reply(&more).is_error,
        "an approval for two pages does not cover four"
    );
    // And an approval of one request does not cover its pages.
    let single = json!({ "endpoint": "store", "operation": QUERY });
    let CallToolResponse::InputRequired(one) = client.call_once("execute", &single, None) else {
        panic!("the first round asks for input");
    };
    let CallToolResponse::Complete(pages) = client.call_once("execute", &arguments(2), Some(accept(one.request_state)))
    else {
        panic!("a retry finishes the call");
    };
    assert!(
        sdk_reply(&pages).is_error,
        "an approval of one request does not cover pages"
    );
    assert_eq!(mock.requests().len(), 2, "nothing more was sent");
}
