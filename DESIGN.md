# Design

One page: what fieldtrip is, and the questions that are already settled. Behavior is specified by the integration
tests in `tests/`. The order of work is in [PLAN.md](PLAN.md).

## Goal

An MCP server in Rust that lets an agent use a GraphQL API it has never seen. The agent loads the schema, finds real
queries and mutations, validates and costs an operation, and runs it only after the human approves. Credentials stay
in the user's config, out of the agent's sight and out of its control.

## Non-goals

- Subscriptions, persisted queries and file uploads.
- Transports other than stdio.
- Matching a server's own cost accounting, or reading `@cost` and `@listSize` directives.
- Loading plugins at runtime. The plugs are a closed set.
- Letting the agent choose a URL, a header or a command.

## Tool surface

| Tool | Input | Returns |
| --- | --- | --- |
| `list_endpoints` | none | Each endpoint's name, URL, schema source and header names |
| `introspect` | `endpoint`, `refresh?` | Overview: root types, counts, query and mutation names |
| `search_schema` | `endpoint`, `query` | Ranked matches as one-line signatures |
| `describe` | `endpoint`, `name` (`Type` or `Type.field`) | That definition as compact SDL |
| `estimate_cost` | `endpoint`, `operation`, `variables?` | Validation errors, or cost, depth, breakdown and assumptions |
| `execute` | `endpoint`, `operation`, `variables?` | The response `data`, `errors` and `extensions`, plus the estimate |

## Config shape

```toml
[endpoints.acme]
url               = "https://api.acme.example/graphql"
schema            = "schemas/acme.graphql"   # optional SDL file, relative to this config file
auto_approve_cost = 50                       # optional, queries only
assumed_list_size = 20                       # optional, default 10
approval          = "elicit"                 # or "client"

[endpoints.acme.headers]
X-Client      = "fieldtrip"
X-Api-Key     = { env = "ACME_API_KEY" }
Authorization = { command = ["python", "-m", "acme_auth", "token"], format = "Bearer {}", ttl = 300 }
```

The config path comes from `--config`, defaulting to `~/.config/fieldtrip/config.toml`. `--url`, with an optional
`--schema`, defines a single endpoint with no headers and no file.

## The three plugs

A header value comes from one of three plugs. `env` and `command` take an optional `format`, in which `{}` is replaced
by the value.

| Plug | Config | Value |
| --- | --- | --- |
| Literal | `"fieldtrip"` | The string as written |
| Environment | `{ env = "NAME" }` | The variable, read when the request is made |
| Command | `{ command = ["python", "-m", "acme_auth", "token"] }` | The command's stdout, trimmed |

## Approval model

`execute` validates and costs the operation, then asks the human through MCP elicitation. The prompt is built from
the operation that will be sent and shows the endpoint, the kind (query or mutation), the operation text, the
variables and the estimated cost. The request is sent only on an explicit accept.

| Situation | Outcome |
| --- | --- |
| Human accepts | Sent |
| Human declines, cancels, or the prompt fails or times out | Not sent |
| Client has no elicitation support | Not sent |
| Query with cost at or below `auto_approve_cost` | Sent without a prompt |
| Endpoint sets `approval = "client"` | Sent without a prompt, mutations included |

## Decisions

Settled. To change one, ask the user, then add a new dated line instead of editing an old one.

- 2026-11-12: Tools take an endpoint name. URLs, headers and commands come only from config or CLI flags.
- 2026-11-12: Approval comes from the human through elicitation, never from a tool argument. Only accept counts.
- 2026-11-12: Mutations always prompt. Queries prompt unless their cost is at or below `auto_approve_cost`.
- 2026-11-12: With no elicitation support `execute` refuses. `approval = "client"` is the only opt-out.
- 2026-11-12: The approval prompt contains nothing written by the calling agent except the operation and variables.
- 2026-11-12: One operation per document. Its kind comes from the parser. Subscriptions are rejected.
- 2026-11-12: When `schema` is set, no introspection query is ever sent to that endpoint.
- 2026-11-12: Both schema sources normalize to one in-memory schema, so every tool behaves the same on either.
- 2026-11-12: The plugs are an enum of three: literal, `env`, `command`. `{}` is the only `format` placeholder.
- 2026-11-12: Commands run as argv with no shell. Output is cached for `ttl` seconds (300) and re-run once on a 401.
- 2026-11-12: Header values never appear in tool output, prompts, errors or logs.
- 2026-11-12: A failed command reports its exit status and stderr, never its stdout.
- 2026-11-12: HTTP redirects are refused, so headers never reach a host the user didn't name.
- 2026-11-12: Cost: scalar 0, object 1, list multiplies by `first`, `last` or `limit`, mutation field 10.
- 2026-11-12: A list with no size argument uses `assumed_list_size` (10), and the estimate lists every assumption.
- 2026-11-12: Estimating never sends the operation.
- 2026-11-12: Every tool result is capped in size and says when it was truncated.
- 2026-11-12: stdout carries the protocol. Logs go to stderr.
- 2026-11-12: Stack: `rmcp`, `apollo-compiler` 1.x, `reqwest` with rustls, `tokio`.
- 2026-11-12: Every slice in PLAN.md is checked by an integration test that drives the binary over MCP.
- 2026-11-12: The server speaks MCP up to revision 2025-11-25 until PLAN slice 16 adds 2026-07-28.
- 2026-11-12: `list_endpoints` shows header names, so the agent can tell whether auth is configured.
- 2026-11-12: Cost: a size argument on a non-list field bounds the lists directly inside it, as connections page.
- 2026-11-12: Cost: a size argument left out takes the default the schema declares for it, when there is one.
- 2026-11-12: Cost: every fragment on an abstract type is counted, which is the worst case.
- 2026-11-12: Cost: introspection fields such as `__typename` are free.
- 2026-11-12: A header command runs in the config file's directory and has 30 seconds to finish.
- 2026-11-12: Errors about a header command name the program but never its arguments, which can hold secrets.
- 2026-11-12: The operation is sent in canonical form, printed from the parsed document, so comments are dropped.
- 2026-11-12: The prompt shows that same canonical text. What the human reads is what the server receives.
- 2026-11-12: Variables are checked against the operation before the human is asked.
- 2026-11-12: `execute` returns the response body as the server wrote it. A response with no `data` is an error.
- 2026-11-12: The prompt is a plain accept or decline, with no form fields. Any other answer counts as a refusal.
