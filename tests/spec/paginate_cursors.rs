//! Slice 62: `paginate` follows cursors that are not Relay's.
//!
//! Many APIs page without a `pageInfo`: a `nextToken` that is null on the last page, or a `cursor` beside a `hasMore`.
//! `paginate.cursor` and `paginate.more` name those fields, relative to the object at `paginate.path`. A cursor can
//! be a number. Paging by offset or page number has no cursor to follow, and the tools say to use `execute_batch`.

use serde_json::{Value, json};

use crate::support::{Human, Launch, Mcp, MockServer, Reply, Response, SdkClient, store_config};

const BY_TOKEN: &str =
    "query Page($nextToken: String) { userPage(limit: 2, nextToken: $nextToken) { items { id } nextToken } }";
const BY_NUMBER: &str =
    "query Page($cursor: Int) { userPage(limit: 2, cursor: $cursor) { items { id } cursor hasMore } }";

/// The variables of a request.
fn variables(body: &str) -> Value {
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    body.get("variables").cloned().unwrap_or(Value::Null)
}

/// A mock whose `userPage` has `pages` pages. Page `n` gives the token `t<n>` and the cursor `n * 2`, and the last
/// page gives a null token and `hasMore: false`.
fn mock(pages: u64) -> MockServer {
    MockServer::start(move |request| {
        let variables = variables(&request.body);
        let by_token = variables
            .get("nextToken")
            .and_then(Value::as_str)
            .and_then(|token| token.strip_prefix('t')?.parse::<u64>().ok());
        let by_number = variables.get("cursor").and_then(Value::as_u64).map(|cursor| cursor / 2);
        let page = by_token.or(by_number).map_or(1, |last| last + 1);
        let more = page < pages;
        let body = json!({ "data": { "userPage": {
            "items": [{ "id": format!("u-{page}") }],
            "nextToken": if more { json!(format!("t{page}")) } else { Value::Null },
            "cursor": page * 2,
            "hasMore": more,
        } } });
        Response::new(200, body.to_string())
    })
}

/// Starts a server that never prompts.
fn start(mock: &MockServer) -> Mcp {
    Launch::new(&store_config(mock, "approval = \"client\"\n"))
        .human(Human::Absent)
        .start()
}

/// Executes `operation` on `store` with `paginate`.
fn follow(mcp: &mut Mcp, operation: &str, paginate: &Value) -> Reply {
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": operation, "paginate": paginate }),
    )
}

/// The variables of each request the mock received, in order.
fn sent(mock: &MockServer) -> Vec<Value> {
    mock.requests().iter().map(|request| variables(&request.body)).collect()
}

#[test]
fn a_token_that_is_null_on_the_last_page_is_followed_to_the_end() {
    let mock = mock(3);
    let mut mcp = start(&mock);

    let reply = follow(
        &mut mcp,
        BY_TOKEN,
        &json!({ "path": "data.userPage", "cursor": "nextToken", "variable": "nextToken" }),
    );

    reply.succeeded();
    assert_eq!(
        sent(&mock),
        [json!({}), json!({ "nextToken": "t1" }), json!({ "nextToken": "t2" })]
    );
    assert!(
        reply.text.contains(
            "\nThat was every page: `data.userPage` has a null `nextToken` after 3 pages.\n\n[page 1] HTTP 200 · "
        ),
        "{}",
        reply.text
    );
    assert!(reply.text.contains("\n\n[page 3] HTTP 200 · "), "{}", reply.text);
}

#[test]
fn a_token_left_when_max_pages_is_reached_is_given_to_go_on_from() {
    let mock = mock(5);
    let mut mcp = start(&mock);

    let reply = follow(
        &mut mcp,
        BY_TOKEN,
        &json!({ "path": "data.userPage", "cursor": "nextToken", "variable": "nextToken", "max_pages": 2 }),
    );

    reply.succeeded();
    assert_eq!(mock.requests().len(), 2);
    assert!(
        reply.text.contains(
            "\nMore pages remain: `data.userPage` still has a `nextToken` after 2 pages, which is the `max_pages` \
             given. Its `nextToken` is \"t2\". To go on, call again with `nextToken` set to it in `variables`."
        ),
        "{}",
        reply.text
    );
}

#[test]
fn a_numeric_cursor_beside_its_own_flag_is_followed_and_sent_as_a_number() {
    let mock = mock(3);
    let mut mcp = start(&mock);

    let reply = follow(
        &mut mcp,
        BY_NUMBER,
        &json!({ "path": "data.userPage", "cursor": "cursor", "more": "hasMore", "variable": "cursor" }),
    );

    reply.succeeded();
    assert_eq!(sent(&mock), [json!({}), json!({ "cursor": 2 }), json!({ "cursor": 4 })]);
    assert!(
        reply
            .text
            .contains("\nThat was every page: `data.userPage` has `hasMore: false` after 3 pages."),
        "{}",
        reply.text
    );

    let capped = follow(
        &mut mcp,
        BY_NUMBER,
        &json!({ "path": "data.userPage", "cursor": "cursor", "more": "hasMore", "variable": "cursor", "max_pages": 1 }),
    );
    assert!(
        capped.text.contains(
            "`data.userPage` still has `hasMore: true` after 1 page, which is the `max_pages` given. Its `cursor` \
             is 2. To go on, call again with `cursor` set to it"
        ),
        "{}",
        capped.text
    );
}

#[test]
fn a_relay_end_cursor_that_is_a_number_is_followed_too() {
    let mock = MockServer::start(|request| {
        let after = variables(&request.body)
            .get("after")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let body =
            json!({ "data": { "users": { "pageInfo": { "hasNextPage": after < 20, "endCursor": after + 10 } } } });
        Response::new(200, body.to_string())
    });
    let mut mcp = start(&mock);

    let reply = follow(
        &mut mcp,
        "query Users($after: String) { users(after: $after) { pageInfo { hasNextPage endCursor } } }",
        &json!({ "path": "data.users" }),
    );

    reply.succeeded();
    assert_eq!(sent(&mock), [json!({}), json!({ "after": 10 }), json!({ "after": 20 })]);
    assert!(
        reply.text.contains("has `hasNextPage: false` after 3 pages"),
        "{}",
        reply.text
    );
}

#[test]
fn the_prompt_names_the_field_the_cursor_comes_from() {
    let mock = mock(3);
    let mut mcp = Launch::new(&store_config(&mock, "")).human(Human::Declines).start();

    follow(
        &mut mcp,
        BY_TOKEN,
        &json!({ "path": "data.userPage", "cursor": "nextToken", "variable": "nextToken" }),
    )
    .failed();

    let prompt = &mcp.prompts()[0];
    assert!(
        prompt.contains("to follow the pages of `data.userPage`.")
            && prompt.contains("\nAfter the first page, `$nextToken` is the `nextToken` of the page before.\n"),
        "{prompt}"
    );
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn a_page_that_lacks_the_named_fields_stops_it_and_says_which() {
    let mock = mock(5);
    let mut mcp = start(&mock);

    let no_token = follow(
        &mut mcp,
        "query Page($nextToken: String) { userPage(nextToken: $nextToken) { items { id } } }",
        &json!({ "path": "data.userPage", "cursor": "token", "variable": "nextToken" }),
    );
    let no_flag = follow(
        &mut mcp,
        BY_NUMBER,
        &json!({ "path": "data.userPage", "cursor": "cursor", "more": "more", "variable": "cursor" }),
    );
    let nothing_there = follow(
        &mut mcp,
        BY_TOKEN,
        &json!({ "path": "data.usersPage", "cursor": "nextToken", "variable": "nextToken" }),
    );

    assert!(
        no_token.text.contains(
            "\nStopped after page 1: the object at `data.userPage` has no `token` to go on from. Select it in the \
             query, or correct `paginate.cursor`."
        ),
        "{}",
        no_token.text
    );
    assert!(
        no_flag.text.contains(
            "\nStopped after page 1: the object at `data.userPage` has no `more`, which should say whether another \
             page follows. Select it in the query, or correct `paginate.more`."
        ),
        "{}",
        no_flag.text
    );
    assert!(
        nothing_there.text.contains(
            "\nStopped after page 1: its response has no object at `data.usersPage`, so there is no cursor to go on \
             from."
        ),
        "{}",
        nothing_there.text
    );
    assert_eq!(mock.requests().len(), 3, "each stopped after its first page");
}

#[test]
fn a_field_path_that_is_not_names_is_refused_before_anything_is_sent() {
    let mock = mock(3);
    let mut mcp = start(&mock);

    for (paginate, expected) in [
        (
            json!({ "path": "data.userPage", "cursor": "items[0].id" }),
            "`paginate.cursor`",
        ),
        (json!({ "path": "data.userPage", "cursor": "" }), "`paginate.cursor`"),
        (
            json!({ "path": "data.userPage", "cursor": "cursor", "more": "has.more!" }),
            "`paginate.more`",
        ),
    ] {
        let reply = follow(&mut mcp, BY_TOKEN, &paginate);
        reply.failed();
        assert!(
            reply.text.contains(expected) && reply.text.contains("Nothing was sent"),
            "{paginate}: {}",
            reply.text
        );
    }
    assert_eq!(mock.requests().len(), 0);
}

#[test]
fn a_nested_cursor_is_named_by_a_path() {
    let mock = MockServer::start(|request| {
        let token = variables(&request.body)
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let next = if token.is_none() { json!("t1") } else { Value::Null };
        let body = json!({ "data": { "userPage": { "items": [], "paging": { "next": next } } } });
        Response::new(200, body.to_string())
    });
    let mut mcp = start(&mock);

    let reply = follow(
        &mut mcp,
        BY_TOKEN,
        &json!({ "path": "data.userPage", "cursor": "paging.next", "variable": "nextToken" }),
    );

    reply.succeeded();
    assert_eq!(sent(&mock), [json!({}), json!({ "nextToken": "t1" })]);
    assert!(
        reply.text.contains("has a null `paging.next` after 2 pages"),
        "{}",
        reply.text
    );
}

#[test]
fn on_revision_2026_a_token_is_followed_after_one_approval() {
    let mock = mock(3);
    let client = SdkClient::start(&Launch::new(&store_config(&mock, "")).human(Human::Accepts));

    let reply = client.call(
        "execute",
        &json!({
            "endpoint": "store",
            "operation": BY_TOKEN,
            "paginate": { "path": "data.userPage", "cursor": "nextToken", "variable": "nextToken" },
        }),
    );

    reply.succeeded();
    assert_eq!(client.prompts().len(), 1, "the human is asked once");
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn the_tools_say_where_paging_by_offset_goes() {
    let mock = MockServer::unreachable();
    let mut mcp = start(&mock);
    let tools = mcp.tool_list();
    let described = |name: &str, pointer: &str| -> String {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .and_then(|tool| tool.pointer(pointer))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    let paginate = described("execute", "/inputSchema/properties/paginate/description");
    let batch = described("execute_batch", "/description");
    assert!(
        paginate.contains("nextToken") && paginate.contains("offset") && paginate.contains("`execute_batch`"),
        "{paginate}"
    );
    assert!(batch.contains("offset"), "{batch}");
}
