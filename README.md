# fieldtrip

GraphQL introspection and execution over MCP. Agents find real operations, plan and cost them, and run them with your
approval.

> **Status: every slice in [PLAN.md](PLAN.md) is implemented and passes its tests.** Those tests drive the real
> binary, but against a mock endpoint and test clients. It has not yet been used from a real MCP client or against
> a real GraphQL API, and there is no release.

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

Each endpoint has a name, a URL, and optionally a description, a schema file and headers:

```toml
[endpoints.acme]
url         = "https://api.acme.example/graphql"
description = "The Acme store API."   # optional: tells the agent what this endpoint is for
schema      = "schemas/acme.graphql"  # optional: read SDL from this file instead of introspecting

[endpoints.acme.headers]
X-Client      = "fieldtrip"
X-Api-Key     = { env = "ACME_API_KEY" }
Authorization = { command = ["python", "-m", "acme_auth", "token"], format = "Bearer {}" }
```

A header value is a literal, an environment variable, or the trimmed stdout of a command. `format` wraps the value, so
a CLI that prints a bare token becomes `Authorization: Bearer <token>`. The command runs directly from the argument
list, with no shell.

| Setting | Default | What it does |
| --- | --- | --- |
| `approval` | `"elicit"` | `"client"` skips fieldtrip's prompt and relies on the MCP client's own tool approval |
| `auto_approve_cost` | none | Queries estimated at or below this run without a prompt. Mutations always prompt |
| `assumed_list_size` | `10` | Items assumed for a list that nothing bounds |
| `timeout` | `30` | Seconds a request may take |
| `costs` | none | A cost map, below |

### Several instances of one service

Put what dev, staging and prod share in a template, and name it from each endpoint:

```toml
[templates.acme]
costs = "acme.costs.toml"

[templates.acme.headers]
Authorization = { command = ["acme", "auth", "token"], format = "Bearer {}" }

[endpoints.acme-dev]
template = "acme"
url      = "https://dev.acme.example/graphql"
approval = "client"

[endpoints.acme-prod]
template    = "acme"
url         = "https://api.acme.example/graphql"
description = "Production. Real customer data."
schema      = "schemas/acme-prod.graphql"
```

An endpoint takes `schema`, `headers`, `costs`, `assumed_list_size` and `timeout` from its template unless it sets
them itself. Headers merge by name, and the endpoint's win. `approval`, `auto_approve_cost`, `url` and `description`
are never taken from a template, so an endpoint that forgets them asks before every operation. A shared cost map is
checked against each endpoint's own schema, so a rule for a field that only dev has shows up in prod's
`introspect`.

### Checking a config

```sh
fieldtrip check --config /path/to/fieldtrip.toml
```

This prints what each endpoint resolves to, runs every header command, reads every schema file, and checks each
cost map against its schema, without printing a header value or sending anything to an endpoint. It exits non-zero
when something is wrong. MCP clients often show only "server failed to start", so run it after any config change.

Set `FIELDTRIP_LOG=debug` for more detail on stderr.

[DESIGN.md](DESIGN.md) has the full config shape, the tools, and the approval rules.

## Cost map

The estimate uses built-in guesses: a scalar is free, an object costs 1, a list multiplies by `first`, `last` or
`limit`, and a list that nothing bounds is assumed to have 10 items. Where you know better, say so in a cost map and
name it on the endpoint with `costs = "acme.costs.toml"`. The path is relative to the config file.

```toml
"Query.products"      = { multiplier = 500 }      # this list has about 500 items
"Query.viewer.orders" = { multiplier = 2 }        # an exact path from the root
"User.orders"         = { multiplier = "take" }   # the list is sized by its `take` argument
"*.reviews"           = { multiplier = 3 }        # `reviews` on any type
"Mutation.refund"     = { cost = 100 }            # this mutation is expensive
"User.email"          = { cost = 2 }              # so is this scalar
```

- **A pattern** is a type name followed by one or more field names. It matches a field that is reached through that
  chain, starting from a selection on that type. A pattern that starts at `Query` or `Mutation` is therefore an
  exact path, and `User.orders` matches wherever `orders` is selected on a `User`. `*` stands for any one name.
  Aliases play no part, and fragments are followed.
- **`cost`** replaces what the field itself costs: 0 for a scalar, 1 for an object (per item, in a list), 10 for a
  top-level mutation field.
- **`multiplier`** replaces the size of a list. A number wins even over a size the operation gives, so use it for
  lists whose real size you know. A string names the argument that carries the size. On a field that is not a list,
  the multiplier bounds the lists directly inside it, which is how pages and connections work.
- **When several rules match**, `cost` and `multiplier` are each taken from the most specific rule that sets them:
  the one with the most parts, then the fewest `*`, then a name nearest the field.

To check a map, restart the server and call `introspect`: it reports the rules that match nothing in the schema.
`estimate_cost` lists every rule an estimate used. A map that is not valid stops the server at startup.

The map is read once, at startup, and no tool can change it. It does feed `auto_approve_cost`, so a map that makes
a query look cheap also lets it run without a prompt. Review a map before you trust it with that. Mutations prompt
whatever they cost.

## Where things are

| File | What it holds |
| --- | --- |
| [DESIGN.md](DESIGN.md) | Goal, non-goals, tools, config, the header plugs, the cost map, approval, and the dated decisions |
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
