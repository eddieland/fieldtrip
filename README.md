# fieldtrip

GraphQL introspection and execution over MCP. Agents find real operations, plan and cost them, and run them with your
approval.

> **Status: in development.** [PLAN.md](PLAN.md) lists the slices in order, and the ticked ones work today.

## What it does

A GraphQL schema is usually too large to hand to an agent whole, so agents guess at field names, write operations that
don't validate, or write ones that validate and cost far more than they look. fieldtrip is an MCP server, written in
Rust, that sits between an agent and a GraphQL API and gives the agent four steps:

1. **Load the schema** by introspecting the endpoint, or from a `schema.graphql` file when introspection is turned
   off.
2. **Discover** queries, mutations, types and fields by searching the schema and reading only the parts it needs.
3. **Estimate** by validating an operation against the schema and costing it before anything is sent.
4. **Execute with approval.** You see the exact operation, its variables and its estimated cost, and nothing is sent
   until you approve.

Credentials stay on your side. Headers are defined in your config file, the agent never sees their values, and it
can't point them at a different host.

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

Without `--config`, fieldtrip reads `~/.config/fieldtrip/config.toml`. For a single endpoint that needs no headers
you can skip the file and pass `--url https://api.example.com/graphql`, optionally with `--schema schema.graphql`.

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

A header value is a literal, an environment variable, or the trimmed stdout of a command. `format` wraps the value, so
a CLI that prints a bare token becomes `Authorization: Bearer <token>`. The command runs directly from the argument
list, with no shell.

[DESIGN.md](DESIGN.md) has the full config shape, the tools, and the approval rules.

## Where things are

| File | What it holds |
| --- | --- |
| [DESIGN.md](DESIGN.md) | One page: goal, non-goals, tools, config, the three header plugs, approval, and a dated list of settled decisions |
| [PLAN.md](PLAN.md) | The ordered checklist of slices, each with a test command that says when it is done |
| `tests/` | The spec. Each slice has an integration test that drives the binary over MCP against a fixture schema |
| [AGENTS.md](AGENTS.md) | How to work in this repository |

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
