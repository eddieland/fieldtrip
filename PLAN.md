# Plan

Ordered slices. Each one is a thin vertical cut, from config through to an MCP tool, and it is done when its
integration test passes. [DESIGN.md](DESIGN.md) says what is being built and which questions are settled.

## How to work it

1. Take the first unchecked slice. Don't skip ahead.
2. Write its test first, in the file the slice names. The test starts the real binary, talks to it over MCP, and
   uses the fixtures in `tests/fixtures/`.
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

- [ ] **1. `list_endpoints`.** The server starts on stdio with `--config` and lists each endpoint's name, URL and
  schema source. A config with a header in it shows no header value.
  Done when: `cargo test --test list_endpoints`
- [ ] **2. `describe`.** For an endpoint whose `schema` is the fixture SDL, `describe` returns compact SDL for a
  type (`User`) and for a field (`Query.user`). An unknown name is an error that suggests the nearest names.
  Done when: `cargo test --test describe`
- [ ] **3. `introspect` from a file.** `introspect` returns the overview for the fixture: root types, counts, and
  the query and mutation names. No HTTP request is made.
  Done when: `cargo test --test introspect_file`
- [ ] **4. `search_schema`.** A search finds queries, mutations, types, fields, arguments and enum values by name
  and by description, best match first. A broad search is cut off with a note saying how many were left out.
  Done when: `cargo test --test search_schema`
- [ ] **5. `introspect` over HTTP.** With no `schema` file, the schema comes from an introspection query to a mock
  server. `describe` and `search_schema` give the same output as they do for the SDL file. A server that rejects
  the newer introspection fields still loads, through a fallback query.
  Done when: `cargo test --test introspect_http`
- [ ] **6. `estimate_cost`.** An invalid operation returns the validation errors. A valid one returns the cost, the
  depth, a breakdown per top-level field and the assumptions. Covers the scalar, object, list and mutation rules,
  size arguments given through variables, fragments, `assumed_list_size`, and the rejection of subscriptions and of
  documents with more than one operation. The mock server receives nothing.
  Done when: `cargo test --test estimate_cost`
- [ ] **7. Literal and `env` plugs.** The introspection request carries the configured literal and `env` headers,
  with `format` applied. A missing variable is an error that names the header.
  Done when: `cargo test --test headers_static`
- [ ] **8. Command plug.** A command's trimmed stdout, passed through `format`, arrives as the header. The command
  runs with no shell. A non-zero exit or empty output is an error that gives the header name, exit status and
  stderr, and not the stdout.
  Done when: `cargo test --test headers_command`
- [ ] **9. Command cache and refresh.** The command runs once within `ttl`. A 401 runs it again and retries the
  request once.
  Done when: `cargo test --test headers_refresh`
- [ ] **10. `execute`, approved.** With a client that accepts the prompt, the operation and variables reach the mock
  server and the tool returns `data`, `errors` and `extensions` with the estimate. The prompt names the endpoint,
  the kind, the operation, the variables and the cost.
  Done when: `cargo test --test execute_approved`
- [ ] **11. `execute`, not approved.** On decline, on cancel, and with a client that has no elicitation support, the
  mock server receives zero requests and the tool result says the operation was not sent.
  Done when: `cargo test --test execute_refused`
- [ ] **12. Approval policy.** A mutation prompts even when a query of the same cost would not. A query at or below
  `auto_approve_cost` runs with no prompt. `approval = "client"` runs with no prompt.
  Done when: `cargo test --test approval_policy`
- [ ] **13. No secrets in output.** With all three plugs configured, the header values appear in no tool result, no
  approval prompt, no error and nothing written to stderr, across every tool.
  Done when: `cargo test --test redaction`
- [ ] **14. Limits.** A redirect from the endpoint is refused and its target receives no request. An oversized
  response is truncated with a note. An endpoint that doesn't answer times out with an error.
  Done when: `cargo test --test limits`
- [ ] **15. `--url` and `--schema`.** The server starts with no config file and serves one endpoint from the flags.
  Done when: `cargo test --test cli_flags`
