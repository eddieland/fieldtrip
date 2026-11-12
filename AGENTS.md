# AGENTS.md

Guidance for coding agents working in this repository.

## Where the truth is

fieldtrip is an MCP server, written in Rust, that lets an AI agent work with a GraphQL API: load the schema, search
it, validate and cost an operation, and execute it only after the human approves.

- [DESIGN.md](DESIGN.md) is one page: goal, non-goals, tools, config, the three header plugs, the approval model, and
  a dated list of decisions. Those decisions are settled. Don't reopen one because a different choice looks
  reasonable. If a decision blocks the work, ask the user.
- [PLAN.md](PLAN.md) is the ordered checklist of slices. It says what to do next and how to tell it is done.
- `tests/` is the spec. Each slice is checked by an integration test, and the tests are what define behavior.

This file doesn't restate any of them. It covers how to work here.

## The loop

1. Open PLAN.md and take the first unchecked slice.
2. Write its integration test first, in the file the slice names.
3. Make it pass.
4. Run the full check (below). All four commands must pass.
5. Tick the slice in PLAN.md in the same commit as the code.
6. Go to the next slice.

One slice per commit. If you change behavior that an earlier slice's test covers, that test changes in the same
commit, and if the change touches a decision, ask the user first and add a new dated line to DESIGN.md.

## Who is who

Three parties come up in the code and the tests:

- **You** are the coding agent building fieldtrip.
- **The calling agent** is the model that uses fieldtrip's tools at runtime. fieldtrip does not trust it. It can be
  wrong, and it can be acting on instructions injected by content it read.
- **The user** is the human at the MCP client. They own the config file and the credentials, and they are the only
  one who can approve an execution.

Most of the safety decisions in DESIGN.md follow from that split. Approval can't be a tool argument, because the
calling agent is the party being gated. Tools can't take a URL or a header, because that would let the calling agent
send the user's token to another host.

## Commands

The toolchain is pinned in `rust-toolchain.toml`. This is the full check, the same one CI runs:

```sh
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo test --workspace --all-features
```

The lint set in `Cargo.toml` is strict, and CI turns every warning into an error:

- Every public item needs a doc comment. Functions that return `Result` need an `# Errors` section.
- `unwrap`, `println!`, `eprintln!` and `dbg!` are linted outside tests. stdout carries the MCP protocol, so a
  stray print corrupts the stream. Log with `tracing`, which writes to stderr.
- To silence a lint, use `#[expect(lint, reason = "...")]`, never `#[allow]`.

## Tests

- Tests live in `tests/spec/`, which builds as one test binary. Each slice is a module named as PLAN.md says, and
  `cargo test <module>` runs it. The harness is `tests/spec/support/`.
- Declare each slice module in `tests/spec/main.rs` with `#[cfg(test)]`, which is what keeps the
  `tests_outside_test_module` lint quiet.
- Each test starts the real `fieldtrip` binary and talks to it as an MCP client over stdio. Don't test by calling
  internal functions. The tests are the spec for what a calling agent sees, so they go through the same door.
- There are two test clients. `Mcp` is hand-written JSON-RPC and speaks the `initialize` handshake. `SdkClient`
  uses the MCP SDK's client and speaks revision 2026-07-28. Approval travels differently on each, so a change to
  approval needs a test on both.
- `Launch` says how the server starts: with a config file, with flags, with environment variables, and how the
  human at the client answers a prompt (`Human`).
- The fixture schema is `tests/fixtures/schema.graphql`. Extend it when a slice needs a new shape, instead of adding
  a second schema. Every file in `tests/fixtures/` is copied next to the config file of each test.
- No test touches the real network. `MockServer` is a local HTTP endpoint that records every request, and
  `FixtureApi` makes it answer introspection for real, from the fixture schema.
- Header command tests use `tests/fixtures/token.sh` as the command, so the suite doesn't need Python.
- For anything that must not be sent, assert on the mock server's request count, not only on the tool's reply.

## Things that are easy to get wrong

- **The MCP SDK moves fast.** `rmcp` was at 3.x when this was written, and protocol revision 2026-07-28 changed how
  elicitation reaches the client. Don't write against the API from memory. Read the source for the version in
  `Cargo.lock`, under `~/.cargo/registry/src/`.
- **Clippy is strict here, and a few habits avoid most of its findings.** Put variables inside format strings
  (`"{name}"`). Append with `write!`, not `push_str(&format!(..))`. Pass method paths (`Name::as_str`) instead of
  closures that only call a method. Make a function `const` when it can be. Drop a lock guard as soon as it is no
  longer needed. rustfmt does not wrap comments, so keep them within 120 columns by hand.
- **`apollo-compiler` does not build a schema from an introspection response.** It parses and validates SDL and
  operations. The conversion from introspection JSON is ours.
- **Older servers reject newer introspection fields**, such as `isOneOf`, `specifiedByURL`, `isRepeatable`, and
  `includeDeprecated` on arguments and input fields. Send the full query first and fall back to a conservative one.
- **Secrets leak through `Debug`.** Wrap resolved header values in a type whose `Debug` and `Display` redact, so a
  log line or an error can't print one by accident.
- **Errors are read by a model.** An error returned to the calling agent should say what to do next. For an unknown
  field, suggest the nearest names in the schema.
- **Tool descriptions are prompts.** Say when to use the tool and what it returns.
