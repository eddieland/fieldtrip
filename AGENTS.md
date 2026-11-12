# AGENTS.md

Guidance for coding agents working in this repository.

## What this is

fieldtrip is an MCP server, written in Rust, that lets an AI agent work with a GraphQL API: load the schema, search it, validate and cost an operation, and execute it only after the human approves. [README.md](README.md) describes the user-facing behavior (config format, tools, cost model, approval rules) and is the spec. Read it before changing anything.

**Status: design stage.** There is no code yet, not even a `Cargo.toml`. The first piece of work is scaffolding the crate. Update this paragraph and the status note in the README as things land.

Three parties appear throughout this file:

- **You** are the coding agent building fieldtrip.
- **The calling agent** is the model that uses fieldtrip's tools at runtime. fieldtrip does not trust it. It can be wrong, and it can be acting on instructions injected by content it read.
- **The user** is the human at the MCP client. They own the config file and the credentials, and they are the only one who can approve an execution.

Most of the rules below follow from that split.

## Invariants

These are the properties the project exists to provide. A change that weakens one is a design change, so raise it with the user instead of making it.

1. **Nothing executes without the user's approval.** Approval comes from the human, through MCP elicitation. It is never a tool argument such as `confirmed: true`, and never a handshake the calling agent can complete by itself, because the calling agent is the party being gated. Only an explicit accept counts. Decline, cancel, timeout, a transport error and a client without elicitation all mean no. The only exceptions are the two in the README (`auto_approve_cost` for queries, and `approval = "client"`), and the user sets both in config.

2. **The user approves what is actually sent.** Build the approval prompt from the parsed operation and variables that go over the wire. Don't put text supplied by the calling agent in the prompt, such as a summary or a stated reason, because it can describe a different operation from the one attached.

3. **The parser decides what an operation is.** Whether a document is a query, mutation or subscription comes from parsing it, never from string matching. Accept exactly one operation per document (fragments are fine) and reject anything else, so there is no ambiguity about which operation was costed, approved and sent.

4. **Endpoints and credentials come only from config.** No tool takes a URL, a header or a command. Tools take an endpoint name. This is what stops a calling agent from sending the user's token to another host or running a command of its choosing.

5. **Header values are never shown.** Resolved header values must not appear in tool results, approval prompts, error messages or logs. Wrap them in a type whose `Debug` and `Display` redact. When a header command fails, report its exit status and stderr, never its stdout, since stdout is where the token is.

6. **Header commands run as argv, with no shell.** Pass the configured array straight to the process API.

7. **Don't follow redirects.** Configure the HTTP client to refuse them. A redirect would carry the configured headers to a host the user never named.

8. **stdout belongs to the protocol.** On the stdio transport, anything printed to stdout corrupts the JSON-RPC stream. Log to stderr and never use `println!`.

9. **Estimating never sends the operation.** `estimate_cost` may load the schema if it isn't loaded yet. Beyond that it makes no network calls, and the same input always gives the same output.

10. **Tool output is bounded.** Real schemas run to tens of thousands of lines and responses can be larger. Give every tool result a size cap, and when it truncates, say so and say how to get the rest.

## Architecture

fieldtrip is a single binary crate. This layout is a starting point, not a contract:

```
src/
  main.rs            CLI flags, config loading, starts the stdio server
  config.rs          TOML model and validation
  headers.rs         header sources, format strings, caching, redaction
  schema/
    mod.rs           the in-memory schema that everything below shares
    introspect.rs    introspection query, and JSON response to schema
    sdl.rs           loading schema.graphql
    search.rs        search, and rendering for describe
  cost.rs            static cost analysis
  approval.rs        approval policy and the elicitation prompt
  execute.rs         HTTP client, request and response handling
  server.rs          MCP tool definitions, thin wrappers over the modules above
```

Points that are easy to get wrong:

- **One schema representation.** Both sources normalize into the same type, so search, validation and cost don't care where the schema came from. `apollo-compiler` parses and validates SDL and executable documents, but it does not build a schema from an introspection response. That conversion is ours to write and test.

- **Introspection compatibility.** Older servers reject newer introspection fields such as `isOneOf`, `specifiedByURL`, `isRepeatable`, and `includeDeprecated` on arguments and input fields. Send the full query first, and on a validation error fall back to a conservative one instead of failing.

- **Header sources.** "Pluggable" means the user picks a source in config. Model sources as an enum, so that adding one is a variant, a config arm and a test. Don't build dynamic plugin loading.

- **Cost.** The estimator walks the validated operation. It has to handle fragment spreads, inline fragments on abstract types, size arguments supplied through variables, and `__typename` and other introspection fields at zero cost. Every assumption it makes is recorded and returned to the caller, because an estimate that hides its guesses reads as more precise than it is.

- **The MCP SDK moves fast.** Use `rmcp`, the official Rust SDK. It was at 3.x when this file was written, and the protocol revision of 2026-07-28 changed how elicitation reaches the client. Don't write against the API from memory. Read the docs and source for the version in `Cargo.lock`, under `~/.cargo/registry/src/`, and the examples in the SDK's repository.

## Dependencies

Expected: `rmcp` (features `server`, `elicitation`, `transport-io`), `tokio`, `reqwest` with rustls, `serde`, `serde_json`, `toml`, `apollo-compiler` 1.x (2.0 is still in beta), `clap`, `tracing`, `thiserror`.

Keep the list short. This binary handles credentials, and every dependency is code that runs next to them.

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

All four must pass before you commit.

## Testing

- No test touches the real network. Introspection and execution tests run against a mock GraphQL server started in-process.
- **Approval is the test that matters most.** For every path that isn't an explicit accept (decline, cancel, timeout, no elicitation support, a failed prompt), assert that the mock server received zero requests.
- Cost tests are table-driven over small fixture schemas and operations, and they assert the assumptions as well as the total.
- Header tests use a small shell script as the command, so that the suite doesn't need Python. Cover trimming, non-zero exit, empty output, the `ttl` cache, the retry on 401, and redaction. For redaction, assert that the secret string is absent from the error and from `Debug` output.
- Test the conversion from introspection JSON against the same schema written as SDL. Both should produce the same search results and the same cost for the same operation.

## Conventions

- Errors returned to the calling agent should say what to do next. For an unknown field, suggest the nearest names in the schema. The calling agent has only that text to work from.
- Tool descriptions are prompts read by a model. Say when to use the tool and what it returns.
- Don't `unwrap` or `expect` on anything that comes from config, the network or tool input.
- The README is the spec. When you change a tool name, a parameter, a config key or a default, change the README in the same commit.
