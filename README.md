# fieldtrip

GraphQL introspection and execution over MCP. Agents find real operations, plan and cost them, and run them with your
approval.

> **Status: every slice in [PLAN.md](PLAN.md) is implemented and passes its tests.** Those tests drive the real
> binary, but against a mock endpoint and test clients. It has not yet been used from a real MCP client or against
> a real GraphQL API. Releases are built by pushing a `v*` tag; until the first one, the install scripts below have
> nothing to download, so build from source.

## What it does

A GraphQL schema is usually too large to hand to an agent whole, so agents guess at field names, write operations that
don't validate, or write ones that validate and cost far more than they look. fieldtrip is an MCP server, written in
Rust, that sits between an agent and a GraphQL API and gives the agent four steps:

1. **Load the schema** by introspecting the endpoint, or from a `schema.graphql` file when introspection is turned
   off.
2. **Discover** queries, mutations, types and fields by searching the schema and reading only the parts it needs,
   or start from the [operations your team has saved](#saved-operations).
3. **Estimate** by validating an operation against the schema and costing it before anything is sent.
4. **Execute with approval.** You see the exact operation, its variables and its estimated cost, and nothing is sent
   until you approve.

Credentials stay on your side. Headers are defined in your config file, the agent never sees their values, and it
can't point them at a different host.

## Setup

Install the binary. On Linux (x86_64, aarch64) and macOS (Intel, Apple silicon):

```sh
curl -fsSL https://github.com/eddieland/fieldtrip/releases/latest/download/install.sh | sh
```

On Windows (x64, or Arm under emulation), in PowerShell:

```powershell
irm https://github.com/eddieland/fieldtrip/releases/latest/download/install.ps1 | iex
```

The scripts download the archive for your platform from the latest GitHub release, check it against the release's
`SHA256SUMS`, and install `fieldtrip` to `~/.local/bin` (on Windows, `%LOCALAPPDATA%\fieldtrip\bin`, which is added
to your user `PATH`). Set `FIELDTRIP_VERSION=0.1.0` to pin a version or `FIELDTRIP_INSTALL_DIR` to install
elsewhere. Linux binaries are statically linked, so they run on any distribution. Each release asset carries a
build provenance attestation, which you can check with `gh attestation verify <file> --repo eddieland/fieldtrip`.

To build from source instead:

```sh
cargo install --locked --git https://github.com/eddieland/fieldtrip
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

For a step-by-step setup in a specific client, see [docs/opencode.md](docs/opencode.md) for OpenCode.

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
| `mutations` | `"prompt"` | `"deny"` makes `execute` refuse every mutation, whoever approves. Queries still run |
| `assumed_list_size` | `10` | Items assumed for a list that nothing bounds |
| `timeout` | `30` | Seconds a request may take |
| `response_headers` | none | Response headers `execute` shows, such as `["x-request-id", "traceparent"]` |
| `max_schema_bytes` | 32 MiB | Bytes of an introspection response that are read. Raise it for a larger schema |
| `max_response_bytes` | 8 MiB | Bytes of a response to `execute` that are read. A body cut off here cannot be parsed |
| `max_output_chars` | `40000` | Characters in one tool result, which is what reaches the agent's context |
| `audit_variables` | `true` | `false` leaves this endpoint's variables out of the audit log, below |
| `costs` | none | A cost map, below |

### Large schemas and responses

A large API, such as GitHub's, has an introspection response of several megabytes. fieldtrip reads up to
`max_schema_bytes` of it, 32 MiB unless you set it, and says so when a schema is larger. A response to `execute` is
read up to `max_response_bytes`, 8 MiB.

What reaches the agent is at most `max_output_chars` per tool result, 40,000 unless you set it. `errors` always comes
first and whole. When `data` and what follows it don't fit, they come back as a summary that is still valid JSON:
long lists cut to their first items and long strings to their first characters, each cut marked with what it
dropped, such as `"…(+412 more items)"`. The agent can also ask for less after the fact, without changing what you approve:

- `select` picks part of the response with a path such as `data.repository.pullRequests.nodes[*].number`.
- `save` writes the whole response to a new file in `fieldtrip/responses` under your cache directory
  (`$XDG_CACHE_HOME` or `~/.cache`), and returns its path, size and a short preview. Header values are removed from
  the file as from any response, and only you can read it. fieldtrip does not delete these files.

When a connection in the response has `pageInfo.hasNextPage: true`, the result points it out with its cursor, so the
agent pages through it instead of asking for a bigger page.

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
mutations   = "deny"
```

An endpoint takes `schema`, `headers`, `costs`, `assumed_list_size`, `timeout`, `response_headers`,
the three `max_` limits and `audit_variables` from its template unless it sets them itself. Headers merge by name, and the endpoint's win.
`approval`, `auto_approve_cost`, `url` and `description` are never taken from a template, so an endpoint that forgets
them asks before every operation. `mutations` is the other way round: a template can set `mutations = "deny"`, and an
endpoint that names it can't set it back to `"prompt"`, so forgetting the setting on one endpoint never opens writes.
A shared cost map is checked against each endpoint's own schema, so a rule for a field that only dev has shows up in
prod's `introspect`.

### Audit log

To keep a record of what agents ran, name a file at the top of the config, before any table:

```toml
audit_log = "~/.local/state/fieldtrip/audit.jsonl"
```

fieldtrip then appends one JSON line per `execute` attempt, including the ones that were not sent:

```json
{"at":"2026-11-13T14:02:11Z","session":"5f0c9a1e7d2b4c36","endpoint":"acme-prod","kind":"query",
 "operation":"OrderById","hash":"sha256:…","text":"query OrderById($id: ID!) { … }","variables":{"id":"123"},
 "approval":"accepted","cost":12,"status":200,"ms":812,"errors":0,"headers":{"x-request-id":"abc123"}}
```

`approval` says who let it through, `accepted`, `auto` (within `auto_approve_cost`) or `client`
(`approval = "client"`), or why it was not sent: `declined`, `cancelled`, `unavailable` (the client cannot show a
prompt), `failed` (the prompt did) or `denied` (`mutations = "deny"`). Anything but `accepted` comes with a `reason`.
A request that got no answer has an `error` instead of a `status`. An operation that is not valid is not recorded,
since nothing could have been sent. `session` is different for each run of the server.

The log never holds a header value, and a value from an `env` or `command` header that turns up in the operation or
its variables is replaced with `<redacted>`. For an API whose variables are sensitive in themselves, set
`audit_variables = false` on its endpoint or template to leave them out. The file and its folders are created
readable by you alone, fieldtrip never rotates or deletes it, and a log that cannot be opened stops the server.

With an audit log, the agent also gets a read-only `history` tool. It returns the latest attempts from the log, for
the endpoints in your config, so an agent starting a new session can see what was already run or declined before it
repeats anything. Responses are never in the log, only what was sent and how it went.

### Checking a config

```sh
fieldtrip check --config /path/to/fieldtrip.toml
```

This prints what each endpoint resolves to, runs every header command, reads every schema file, and checks each
cost map and saved operation against its schema, without printing a header value or sending anything to an endpoint. It exits non-zero
when something is wrong. MCP clients often show only "server failed to start", so run it after any config change.

Set `FIELDTRIP_LOG=debug` for more detail on stderr.

[DESIGN.md](DESIGN.md) has the full config shape, the tools, and the approval rules.

## Saved operations

An agent writes better GraphQL when it starts from an operation that is known to work. If your team already has the
operations it trusts, in client code or runbooks, give an endpoint (or a template) a directory of them:

```toml
[templates.acme]
operations = "operations/acme/"   # relative to the config file
```

Each `.graphql` or `.gql` file holds one named query or mutation, plus any fragments it uses. The comment lines at the
top are its description:

```graphql
# Find an order by ID with its line items and payment state.
query OrderById($id: ID!) {
  order(id: $id) { id status lineItems(first: 50) { nodes { sku quantity } } payment { state } }
}
```

`list_operations` shows the agent each one with its description, variables, estimated cost and text, and
`search_schema` lists matching ones first. A library can hold hundreds, so the agent can narrow the list: `query`
matches words in the name or description, `kind` keeps queries or mutations, and `uses` keeps operations that select
a type (`Order`) or a chain of fields written as in a [cost map](#cost-map) (`User.orders`, `Query.order.lineItems`,
`*.payment`), following fragments. When more than five match, each gets one line, and `name` shows one in full. The agent runs one with `execute` and `operation_name`. You approve it as
you would any other operation, and the prompt shows the full text that will be sent.

Every operation is validated against the schema at startup (for a `schema` file) or when the schema is first loaded
(by introspection). One that no longer validates is logged as a warning, listed as broken, and never sent, so a
change to the API shows up instead of failing quietly. `fieldtrip check` warns about them too. A file that is not one
named query or mutation, or two files with the same operation name, stop the server at startup.

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
| [docs/](docs/) | Setup guides for specific MCP clients |
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

### Releasing

Bump `version` in `Cargo.toml`, merge, then push a matching tag (`git tag v0.1.0 && git push origin v0.1.0`). The
release workflow in `.github/workflows/release.yml` builds every platform, and publishes the GitHub release with the
archives, `SHA256SUMS` and the install scripts from `scripts/` only once all of them succeed. A tag with a suffix,
such as `v0.2.0-rc.1`, is published as a prerelease.

## License

[MIT](LICENSE)
