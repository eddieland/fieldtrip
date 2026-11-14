# Plan

Ordered slices. Each one is a thin vertical cut, from config through to an MCP tool, and it is done when its
integration test passes. [DESIGN.md](DESIGN.md) says what is being built and which questions are settled.

## How to work it

1. Take the first unchecked slice. Don't skip ahead.
2. Write its test first, as the module `tests/spec/<name>.rs`, where `<name>` is the one in the slice's "Done when".
   The test starts the real binary, talks to it over MCP, and uses the fixtures in `tests/fixtures/`.
3. Make the test pass, then run the full check below.
4. Tick the box in the same commit as the code, and move on to the next slice.

If a slice can't be done without changing a decision in DESIGN.md, stop and ask the user. If you find work that no
slice covers, add a slice for it instead of folding it into the current one.

The full check, which is what CI runs:

```sh
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo test --workspace --all-features
```

## Slices

- [x] **1. `list_endpoints`.** The server starts on stdio with `--config` and lists each endpoint's name, URL,
  schema source and header names. A config with a header in it shows no header value.
  Done when: `cargo test list_endpoints`
- [x] **2. `describe`.** For an endpoint whose `schema` is the fixture SDL, `describe` returns compact SDL for a
  type (`User`) and for a field (`Query.user`). An unknown name is an error that suggests the nearest names.
  Done when: `cargo test describe`
- [x] **3. `introspect` from a file.** `introspect` returns the overview for the fixture: root types, counts, and
  the query and mutation names. No HTTP request is made.
  Done when: `cargo test introspect_file`
- [x] **4. `search_schema`.** A search finds queries, mutations, types, fields, arguments and enum values by name
  and by description, best match first. A broad search is cut off with a note saying how many were left out.
  Done when: `cargo test search_schema`
- [x] **5. `introspect` over HTTP.** With no `schema` file, the schema comes from an introspection query to a mock
  server. `describe` and `search_schema` give the same output as they do for the SDL file. A server that rejects
  the newer introspection fields still loads, through a fallback query.
  Done when: `cargo test introspect_http`
- [x] **6. `estimate_cost`.** An invalid operation returns the validation errors. A valid one returns the cost, the
  depth, a breakdown per top-level field and the assumptions. Covers the scalar, object, list and mutation rules,
  size arguments given through variables, fragments, `assumed_list_size`, and the rejection of subscriptions and of
  documents with more than one operation. The mock server receives nothing.
  Done when: `cargo test estimate_cost`
- [x] **7. Literal and `env` plugs.** The introspection request carries the configured literal and `env` headers,
  with `format` applied. A missing variable is an error that names the header.
  Done when: `cargo test headers_static`
- [x] **8. Command plug.** A command's trimmed stdout, passed through `format`, arrives as the header. The command
  runs with no shell. A non-zero exit or empty output is an error that gives the header name, exit status and
  stderr, and not the stdout.
  Done when: `cargo test headers_command`
- [x] **9. Command cache and refresh.** The command runs once within `ttl`. A 401 runs it again and retries the
  request once.
  Done when: `cargo test headers_refresh`
- [x] **10. `execute`, approved.** With a client that accepts the prompt, the operation and variables reach the mock
  server and the tool returns `data`, `errors` and `extensions` with the estimate. The prompt names the endpoint,
  the kind, the operation, the variables and the cost.
  Done when: `cargo test execute_approved`
- [x] **11. `execute`, not approved.** On decline, on cancel, and with a client that has no elicitation support, the
  mock server receives zero requests and the tool result says the operation was not sent.
  Done when: `cargo test execute_refused`
- [x] **12. Approval policy.** A mutation prompts even when a query of the same cost would not. A query at or below
  `auto_approve_cost` runs with no prompt. `approval = "client"` runs with no prompt.
  Done when: `cargo test approval_policy`
- [x] **13. No secrets in output.** With all three plugs configured, the header values appear in no tool result, no
  approval prompt, no error and nothing written to stderr, across every tool.
  Done when: `cargo test redaction`
- [x] **14. Limits.** A redirect from the endpoint is refused and its target receives no request. An oversized
  response is truncated with a note. An endpoint that doesn't answer times out with an error.
  Done when: `cargo test limits`
- [x] **15. `--url` and `--schema`.** The server starts with no config file and serves one endpoint from the flags.
  Done when: `cargo test cli_flags`
- [x] **16. Protocol 2026-07-28.** The server also speaks the MCP revision that has no `initialize` handshake. On
  it, `execute` asks for approval by returning an input-required result, and runs only when the client retries with
  an accepted response for the same operation.
  Done when: `cargo test protocol_2026`
- [x] **17. Cost map, exact paths.** An endpoint's `costs` file sets a `cost` or a `multiplier` for the field at a
  path such as `Query.viewer.orders` or `User.orders`. The estimate uses the rules and lists each one it used. A
  longer path wins over a shorter one. `list_endpoints` shows the map. A query made cheap by the map can pass
  `auto_approve_cost`.
  Done when: `cargo test cost_map`
- [x] **18. Cost map patterns.** `*` stands for any one type or field name. When several rules match, `cost` and
  `multiplier` each come from the most specific rule that sets them.
  Done when: `cargo test cost_patterns`
- [x] **19. Multiplier from an argument.** `multiplier = "take"` sizes the list by that argument, given as a
  literal, through a variable, or by its default in the schema. With no value, the assumed size is used and the
  estimate says so.
  Done when: `cargo test cost_argument`
- [x] **20. Cost map checks.** A map that does not parse, or has a rule that is not valid, stops the server at
  startup with a message that names the file and the rule. `introspect` reports the rules that match nothing in
  the schema.
  Done when: `cargo test cost_checks`
- [x] **21. Config checks.** A URL with no host, a missing `schema` file, `assumed_list_size = 0`,
  `auto_approve_cost` with `approval = "client"`, and an endpoint name with characters other than letters, digits,
  `-`, `_` and `.` stop the server at startup. A path that starts with `~/` is in the home directory.
  Done when: `cargo test config_checks`
- [x] **22. Header names.** Two header names that differ only in case, and the headers fieldtrip sets itself, stop
  the server at startup with a message that names them and quotes no value.
  Done when: `cargo test header_names`
- [x] **23. URL secrets.** The values of a URL's query parameters appear in no tool result, prompt, error or log,
  but the request still goes to the URL as configured. A URL with a user name or password stops the server.
  Done when: `cargo test url_secrets`
- [x] **24. Templates.** An endpoint takes the headers, cost map, schema, sizes and timeout of the template it
  names, and its own settings win. Approval settings, the URL and the description are never taken from a template.
  A shared command header runs once for every endpoint. A shared cost map is checked against each endpoint's schema.
  Done when: `cargo test templates`
- [x] **25. Descriptions.** An endpoint's `description` is shown by `list_endpoints` on one line.
  Done when: `cargo test descriptions`
- [x] **26. `fieldtrip check`.** Prints each endpoint as it resolves: template, URL, approval, sizes, schema file,
  cost map findings, and each header's plug with whether it resolves, with no value. Fails on a header or schema
  problem, warns on an unmatched rule, and sends nothing to any endpoint.
  Done when: `cargo test check_command`
- [x] **27. Tool descriptions.** Every tool has a title, a description that says when to use it and what it
  returns, and a description for each argument. Annotations mark every tool but `execute` as read-only, and
  `execute` as destructive and not idempotent. The server instructions lay out the workflow in tool order, and the
  descriptions carry the GraphQL habits where they apply: names from the schema, variables, page sizes, fragments
  on abstract types, deprecations, and reading `errors` alongside `data`.
  Done when: `cargo test tool_descriptions`
- [x] **28. Response metadata.** `execute` shows the HTTP status, the time taken and the response headers named by
  `response_headers`, then `errors` in full, then `data`. A response with only `errors` is a result, not a tool
  error. A header that is not configured is not shown, and one that echoes a secret is redacted. An endpoint takes
  `response_headers` from its template unless it sets them. When the data is cut to fit, the errors are not. A
  connection error or a timeout says how long fieldtrip waited.
  Done when: `cargo test response_metadata`
- [x] **29. Mutations denied.** `mutations = "deny"` on an endpoint, or on its template, makes `execute` refuse
  every mutation before it is validated, with no prompt on either protocol revision and no request to the mock,
  even with `approval = "client"`. Queries run as before. `estimate_cost` still costs a mutation and says `execute`
  will refuse it, `introspect` marks the mutations unavailable, and `list_endpoints` and `fieldtrip check` show
  `mutations: denied`. An endpoint that sets `"prompt"` under a template that denies, or any other value, stops the
  server at startup.
  Done when: `cargo test mutations_denied`
- [x] **30. Large schemas.** Introspection reads up to `max_schema_bytes`, 32 MiB by default, set on an endpoint or
  a template, apart from what `execute` reads. A schema over 1 MiB loads and `introspect` gives the same counts as
  the SDL file. One over the limit is an error that names the limit, `max_schema_bytes` and `schema`, never
  introspection being turned off, and the fallback query is not sent. `fieldtrip check` shows the limit.
  Done when: `cargo test large_schema`
- [x] **31. Response limits.** `execute` reads up to `max_response_bytes`, 8 MiB by default, and a tool result about
  an endpoint carries up to `max_output_chars`, 40,000 by default. Both are set on an endpoint or a template, and
  `fieldtrip check` shows them. A body over the read limit is reported with the limit's name and what was lost. A
  limit of zero, or under 1,000 characters, stops the server at startup.
  Done when: `cargo test response_limits`
- [x] **32. Response summaries.** When the parts of a response after `errors` are too long for `max_output_chars`,
  they come back as a summary in which each part is still valid JSON. `errors` stays whole. Long lists keep their
  first items and long strings their first characters, each cut marked with what it dropped, such as
  `"…(+412 more items)"`. A note after it says what was cut.
  Done when: `cargo test response_summary`
- [x] **33. `select`.** `execute` takes an optional path such as `data.products[*].id`, applied to the response
  after it arrives: names joined by `.`, `[N]` for one item and `[*]` for every item, which gives a list. The result
  has `errors` first and in full, then the selection, summarized like any other result when it is too long. A path
  that misses says what is there. A path that is not valid is an error before anything is sent or asked, and a path
  changes neither the request nor the prompt.
  Done when: `cargo test execute_select`
- [x] **34. `save`.** `execute` takes an optional `save` flag that writes the whole response, as the server wrote it
  and without secrets, to a new file in `fieldtrip/responses` under the user's cache directory. The result gives
  the path, the size and a preview of at most 4,000 characters, summarized like any other result. A file is never
  overwritten, and on Unix only the user can read it. Nothing is saved when nothing was sent or the body was cut
  off at `max_response_bytes`.
  Done when: `cargo test execute_save`
- [x] **35. Pagination hints.** When a connection in `data` has `pageInfo.hasNextPage: true`, the result says so
  right after the status line, with the connection's path and `endCursor`, and says to page with `after` rather than
  raise `first`. A connection inside a list is counted across its items. At most five are named. The hints come with
  a summary and with a selection too.
  Done when: `cargo test pagination_hints`
- [x] **36. Saved operations.** An endpoint or a template names a directory of `.graphql` files with `operations`,
  one named query or mutation each, whose leading comment is its description. `list_operations` lists each with its
  kind, description, variables, estimated cost and canonical text, and `query` narrows the list by name and
  description. `search_schema` lists the matching ones first, and `list_endpoints` says how many there are. `execute`
  and `estimate_cost` take `operation_name` instead of `operation`, and the prompt and the request carry the saved
  operation's canonical text. One that no longer validates is marked broken, is never sent or prompted for, and is a
  warning in `fieldtrip check` and in the startup log. A file that does not hold one named query or mutation, two
  files with one name, or a missing directory stop the server at startup.
  Done when: `cargo test saved_operations`
- [x] **37. Operation filters.** `list_operations` takes `uses`, patterns that every listed operation must match: a
  type name matches an operation that selects a field of that type or one that returns it, and a type followed by
  field names matches that chain as a cost map pattern does, `*` included, following fragments. `kind` keeps queries
  or mutations, and all filters combine. A pattern with an unknown type or first field is an error that suggests the
  nearest names, and a broken operation matches no pattern. Past five matches each operation is one line without its
  text, and `name` shows one in full. `search_schema` lists at most ten saved operations, and a list that matches
  nothing names the operations only when there are at most twenty.
  Done when: `cargo test operation_filters`
- [x] **38. Audit log.** With `audit_log` set at the top of the config, each `execute` attempt appends one JSON line
  to that file: the time, the session, the endpoint, the kind, the operation's name, text and SHA-256, the variables,
  the approval and its reason, the cost, and for a request that was sent the status, the time taken, the number of
  `errors` and the chosen response headers, or the error when no answer came. Accepted, waived (`auto`, `client`),
  declined, cancelled, unavailable, failed and denied attempts each get their own word. An operation that is not
  valid writes nothing. Header values never appear, an `env` or `command` value pasted into the operation or its
  variables is redacted, and `audit_variables = false` on an endpoint or template leaves the variables out. On
  revision 2026-07-28 only the answered retry is written. The file and its folders are created at startup, readable
  by the user alone, and a log that cannot be opened stops the server. Without the setting no file is created.
  `fieldtrip check` shows the path.
  Done when: `cargo test audit_log`
- [x] **39. `history`.** With `audit_log` set, a read-only `history` tool, taking an optional `endpoint` and `limit`
  (20, at most 100), returns the most recent attempts as the JSON lines the log holds, oldest first, after a line
  that names this session, so attempts from earlier sessions can be told apart. Only configured endpoints are shown,
  an unknown `endpoint` is an error that lists them, and lines that are not readable are counted and skipped. When the
  lines do not fit in `max_output_chars`, the oldest are left out and the result says so. Without `audit_log` the
  tool is not offered and the instructions do not mention it.
  Done when: `cargo test history`
- [x] **40. Credential status.** `list_endpoints` shows, for each header that comes from a command, how its last run
  went: `ok` and when, `failed` with when and a short reason such as `exit 3` or `printed nothing`, or `not run yet`.
  Listing runs nothing and sends nothing, and shows neither stdout nor stderr. A template's command reports the same
  run for every endpoint that shares it. Literal and `env` headers have no such line.
  Done when: `cargo test credential_status`
- [x] **41. The 401 retry is reported.** When a 401 makes the header commands run again and the retry is refused
  with a 401 too, `introspect` and `execute` say both happened, name each header and its program, and say the user
  has to sign in again, as distinct from a 403. `list_endpoints` shows the command's value as refused. A command that
  fails when run again says the 401 came first, and the retry is not sent. A retry that works adds nothing.
  Done when: `cargo test unauthorized_retry`
