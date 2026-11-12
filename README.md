# fieldtrip

GraphQL introspection and execution over MCP. Agents find real operations, plan and cost them, and run them with your approval.

> **Status: design stage.** Nothing here is implemented yet. This README describes the intended behavior and is the spec the first implementation is built against. Details may change as the code lands.

## What it does

A GraphQL schema is usually too large to hand to an agent whole, so agents guess at field names, write operations that don't validate, or write ones that validate and cost far more than they look. fieldtrip is an MCP server, written in Rust, that sits between an agent and a GraphQL API and gives the agent four steps:

1. **Load the schema** by introspecting the endpoint, or from a `schema.graphql` file when introspection is turned off.
2. **Discover** queries, mutations, types and fields by searching the schema and reading only the parts it needs.
3. **Estimate** by validating an operation against the schema and costing it before anything is sent.
4. **Execute with approval.** You see the exact operation, its variables and its estimated cost, and nothing is sent until you approve.

Credentials stay on your side. Headers are defined in your config file, the agent never sees their values, and it can't point them at a different host.

## Setup

Install the binary:

```sh
cargo install --git https://github.com/eddieland/fieldtrip
```

Register it with your MCP client as a stdio server:

```json
{
  "mcpServers": {
    "fieldtrip": {
      "command": "fieldtrip",
      "args": ["--config", "/path/to/fieldtrip.toml"]
    }
  }
}
```

Without `--config`, fieldtrip reads `~/.config/fieldtrip/config.toml`. For a single endpoint that needs no headers you can skip the file and pass `--url https://api.example.com/graphql`, optionally with `--schema schema.graphql`.

## Configuration

Each endpoint has a name, a URL, and optionally a schema file and headers:

```toml
[endpoints.acme]
url    = "https://api.acme.example/graphql"
schema = "schemas/acme.graphql"   # optional: read SDL from this file instead of introspecting

[endpoints.acme.headers]
X-Client      = "fieldtrip"
X-Api-Key     = { env = "ACME_API_KEY" }
Authorization = { command = ["python", "-m", "acme_auth", "token"], format = "Bearer {}" }
```

A relative `schema` path is resolved against the config file's directory. When `schema` is set, fieldtrip never sends an introspection query to that endpoint.

### Headers

A header value comes from one of three sources:

| Source | Example | Value |
| --- | --- | --- |
| Literal | `"fieldtrip"` | The string as written |
| `env` | `{ env = "ACME_API_KEY" }` | The environment variable, read when the request is made |
| `command` | `{ command = ["python", "-m", "acme_auth", "token"] }` | The command's stdout, with surrounding whitespace trimmed |

`env` and `command` accept a `format` string in which `{}` is replaced by the value, so a CLI that prints a bare token becomes `Authorization: Bearer <token>` with `format = "Bearer {}"`.

Commands behave as follows:

- The command is run directly from the argument list, with no shell. Use `["sh", "-c", "..."]` if you need one.
- Its output is cached for `ttl` seconds (default 300). If the endpoint answers 401, the command is run again and the request retried once.
- A non-zero exit or empty output fails the request with an error that names the header and never includes its value.

Header values never appear in tool results, approval prompts or logs. Tools take an endpoint name, never a URL, header or command, so an agent acting on injected instructions can't send your token somewhere else.

## Tools

| Tool | What it does |
| --- | --- |
| `list_endpoints` | Lists the configured endpoints with their URL and schema source. |
| `introspect` | Loads an endpoint's schema, from the introspection query or the SDL file, and returns an overview: root types, counts, and the names of the queries and mutations. Pass `refresh` to reload. |
| `search_schema` | Searches queries, mutations, types, fields, arguments and enum values by name or description, and returns ranked matches as compact signatures. |
| `describe` | Returns the full definition of one type or field (`User`, `Query.user`, `Mutation.createUser`) as SDL, with descriptions, arguments and deprecations. |
| `estimate_cost` | Parses and validates an operation against the schema and returns its estimated cost. The operation is not sent. |
| `execute` | Validates and costs the operation, asks you to approve it, sends it, and returns the response. |

Every tool returns a bounded amount of text. Large schemas and large responses are paged or truncated with a note saying so, never dumped whole.

`estimate_cost` and `execute` take one operation per call, plus its variables. Subscriptions are rejected.

## Cost estimates

The estimate comes from static analysis of the operation against the schema:

- A field that returns a scalar or enum costs 0.
- A field that returns an object, interface or union costs 1.
- A list field multiplies the cost of everything selected beneath it by the list's size. The size is the field's `first`, `last` or `limit` argument, whether given as a literal or through a variable. With no such argument, `assumed_list_size` is used (default 10).
- Each top-level mutation field costs 10.

The result gives the total, the maximum depth, a breakdown per top-level field, and every assumption that went into it, such as "`Query.users` has no size argument, assumed 10 items".

This is an estimate of the worst case for the page sizes requested. It is not the server's own accounting, and servers with their own cost rules will disagree with it. If the server reports cost in the response's `extensions`, as Shopify does, `execute` returns that alongside the estimate.

## Approval

`execute` asks for approval through MCP elicitation, so the prompt appears in your MCP client. It shows the endpoint, whether the operation is a query or a mutation, the operation text, the variables and the estimated cost. fieldtrip builds the prompt from the operation it is about to send, not from anything the agent says about it.

- The operation is sent only if you accept. If you decline, dismiss the prompt or let it time out, nothing is sent and the agent is told so.
- Mutations always ask.
- Queries ask by default. Set `auto_approve_cost` on an endpoint to let queries at or below that cost run without a prompt.
- If your client doesn't support elicitation, `execute` refuses to run. If your client already asks you before every tool call and you want to rely on that instead, set `approval = "client"` on the endpoint. That turns off fieldtrip's own prompt for that endpoint, mutations included.

The cost and approval settings live on the endpoint:

```toml
[endpoints.acme]
url               = "https://api.acme.example/graphql"
auto_approve_cost = 50
assumed_list_size = 20
```

## Out of scope for now

- Subscriptions
- Persisted queries
- Transports other than stdio
- Cost rules from `@cost` and `@listSize` schema directives

## Development

The toolchain is pinned in `rust-toolchain.toml` (the MSRV is always the latest stable), so `rustup` picks it up
automatically. Lints are configured in `Cargo.toml` and are warnings locally; CI fails on any warning.

```sh
cargo fmt --all                                                   # 120-column formatting
cargo clippy --workspace --all-targets --all-features             # pedantic + nursery + curated restriction lints
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
cargo test --workspace --all-features
```

Every public item needs a doc comment; functions returning `Result` need an `# Errors` section and functions that
can panic need a `# Panics` section. To silence a lint locally, use `#[expect(lint, reason = "...")]` rather than
`#[allow]`.

## License

[MIT](LICENSE)
