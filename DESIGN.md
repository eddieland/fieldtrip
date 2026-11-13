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
- Reading `@cost` and `@listSize` directives.
- Learning costs from real queries. It is wanted later. Until then a cost map is written by hand or by an agent.
- Loading plugins at runtime. The plugs are a closed set.
- Letting the agent choose a URL, a header or a command.

## Tool surface

| Tool | Input | Returns |
| --- | --- | --- |
| `list_endpoints` | none | Each endpoint's name, URL, schema source and header names |
| `introspect` | `endpoint`, `refresh?` | Overview: root types, counts, query and mutation names |
| `search_schema` | `endpoint`, `query` | Ranked matches as one-line signatures |
| `describe` | `endpoint`, `name` (`Type` or `Type.field`) | That definition as compact SDL |
| `estimate_cost` | `endpoint`, `operation`, `variables?` | Validation errors, or cost, depth, breakdown, assumptions |
| `execute` | `endpoint`, `operation`, `variables?` | Status, time, chosen headers, then `errors` whole, then `data` |

## Config shape

```toml
[templates.acme]                             # optional: settings that several endpoints share
costs             = "acme.costs.toml"        # optional cost map, relative to this file
assumed_list_size = 20                       # optional, default 10
timeout           = 30                       # optional, seconds, default 30
response_headers  = ["x-request-id", "traceparent"]   # optional, shown by `execute`
max_schema_bytes  = 33554432                 # optional, default 32 MiB, read of an introspection response
max_response_bytes = 8388608                 # optional, default 8 MiB, read of a response to `execute`
max_output_chars  = 40000                    # optional, default 40,000, characters in one tool result

[templates.acme.headers]
X-Client      = "fieldtrip"
Authorization = { command = ["python", "-m", "acme_auth", "token"], format = "Bearer {}", ttl = 300 }

[endpoints.acme-prod]
template          = "acme"                   # optional
url               = "https://api.acme.example/graphql"
description       = "Production. Real customers."   # optional, shown to the agent
schema            = "schemas/acme.graphql"   # optional SDL file, relative to this file
approval          = "elicit"                 # or "client"
auto_approve_cost = 50                       # optional, queries only
mutations         = "deny"                   # optional, default "prompt". A template may set it too

[endpoints.acme-prod.headers]
X-Api-Key = { env = "ACME_API_KEY" }
```

An endpoint takes `schema`, `headers`, `costs`, `assumed_list_size`, `timeout`, `response_headers` and the three
`max_` limits from its template unless it sets them. Headers merge by name, ignoring case, and the endpoint's win.
`url`, `description`, `approval` and `auto_approve_cost` are only ever set on the endpoint. `mutations` comes from the
template too, but an endpoint can only make it stricter. A path may start with `~/`.

The config path comes from `--config`, defaulting to `$XDG_CONFIG_HOME/fieldtrip/config.toml` or
`~/.config/fieldtrip/config.toml`. `--url`, with an optional `--schema`, defines a single endpoint with no headers and
no file. `fieldtrip check` loads the same configuration, prints what each endpoint resolves to and exits.

## The three plugs

A header value comes from one of three plugs. `env` and `command` take an optional `format`, in which `{}` is replaced
by the value.

| Plug | Config | Value |
| --- | --- | --- |
| Literal | `"fieldtrip"` | The string as written |
| Environment | `{ env = "NAME" }` | The variable, read when the request is made |
| Command | `{ command = ["python", "-m", "acme_auth", "token"] }` | The command's stdout, trimmed |

## Cost map

The built-in costs are guesses. An endpoint's `costs` file replaces them wherever its owner knows better. Each rule
is a pattern over the fields of an operation, and sets a `cost`, a `multiplier`, or both.

```toml
"Query.products"  = { multiplier = 500 }      # an exact path from the root
"User.orders"     = { multiplier = "take" }   # sized by the `take` argument
"*.reviews"       = { multiplier = 3 }        # `reviews` on any type
"Mutation.refund" = { cost = 100 }
```

A pattern is a type followed by one or more field names, and it matches a field reached through that chain. `*`
stands for any one name. `cost` replaces what the field itself costs. `multiplier` replaces the size of a list.

## Approval model

`execute` validates and costs the operation, then asks the human through MCP elicitation. The prompt is built from
the operation that will be sent and shows the endpoint, the kind (query or mutation), the operation text, the
variables and the estimated cost. The request is sent only on an explicit accept. Up to MCP revision 2025-11-25 the
prompt is a request from the server. From 2026-07-28 it is an "input required" result, answered on a retry.

| Situation | Outcome |
| --- | --- |
| Human accepts | Sent |
| Human declines, cancels, or the prompt fails or times out | Not sent |
| Client has no elicitation support | Not sent |
| Query with cost at or below `auto_approve_cost` | Sent without a prompt |
| Endpoint sets `approval = "client"` | Sent without a prompt, mutations included |
| Mutation, and the endpoint or its template sets `mutations = "deny"` | Not sent, and no prompt is shown |

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
- 2026-11-12: A value from an `env` or `command` plug is removed from response bodies, in case the server echoes it.
- 2026-11-12: Literals and values shorter than 8 characters are not removed, because that would damage responses.
- 2026-11-12: `FIELDTRIP_LOG` sets the log level for fieldtrip and the MCP SDK. Other crates stay at warnings.
- 2026-11-12: A request has `timeout` seconds to complete, 30 unless the endpoint sets it.
- 2026-11-12: A tool result carries at most 40,000 characters, and at most 1 MiB of a response body is read.
- 2026-11-12: A redirect is reported as an error that names its target. It is never followed.
- 2026-11-12: `--url` defines one endpoint called `default`. A relative `--schema` is relative to the working directory.
- 2026-11-12: The server speaks every MCP revision the SDK knows. This lifts the 2025-11-25 ceiling set above.
- 2026-11-12: On revision 2026-07-28 the prompt is an input-required result and the answer comes with the retry.
- 2026-11-12: Each such prompt carries a single-use ticket tied to the operation. An accept without it sends nothing.
- 2026-11-12: Costs can be overridden per endpoint by a cost map: a TOML file of rules, named by `costs`.
- 2026-11-12: A rule's pattern is a type and a chain of field names, matched against the fields of the operation.
- 2026-11-12: A pattern that starts at a root type is an exact path. One that starts elsewhere matches wherever it fits.
- 2026-11-12: `*` in a pattern stands for any one type or field name.
- 2026-11-12: `cost` replaces the field's own cost: 0 for a scalar, 1 for an object, 10 for a top-level mutation field.
- 2026-11-12: `multiplier` replaces the size of a list, even a size the operation gives through an argument.
- 2026-11-12: A `multiplier` on a field that is not a list bounds the lists directly inside it, as a size argument does.
- 2026-11-12: A `multiplier` that is a string names the argument that carries the size.
- 2026-11-12: When several rules match a field, each setting comes from the most specific: most names, then fewest `*`.
- 2026-11-12: The cost map is config. No tool can change it, and it is read once, at startup.
- 2026-11-12: The estimate lists every rule it used.
- 2026-11-12: A cost map that is not valid stops the server at startup. A rule that cannot match is not an error.
- 2026-11-12: `introspect` reports the rules that match nothing in the schema, so a misspelled rule is noticed.
- 2026-11-13: An endpoint can name a template, which holds settings shared by instances of one service. One level only.
- 2026-11-13: `url`, `description`, `approval` and `auto_approve_cost` never come from a template. A missing one asks.
- 2026-11-13: Headers merge by name ignoring case, the endpoint's winning. A template header is shared, cache included.
- 2026-11-13: Several services live in one config file. Templates, not separate servers, are how they share settings.
- 2026-11-13: Header names that differ only in case are an error. Headers fieldtrip sets itself cannot be configured.
- 2026-11-13: A URL with a user name or password is refused. Query values are shown as `<redacted>` wherever a URL is.
- 2026-11-13: Startup refuses a URL with no host, a missing `schema` file, and `assumed_list_size = 0`.
- 2026-11-13: Startup refuses `auto_approve_cost` with `approval = "client"`, which would silently ignore it.
- 2026-11-13: An endpoint name has only letters, digits, `-`, `_` and `.`. A `description` is shown on one line.
- 2026-11-13: `fieldtrip check` resolves every header and reads schema files, but sends nothing to an endpoint.
- 2026-11-13: In `check`, a header or schema that fails is a problem and fails the run. An unmatched rule only warns.
- 2026-11-13: A relative `--config` is made absolute first, so header commands start in a real directory.
- 2026-11-13: `execute` gives a summary line, then status, time taken and chosen headers, then `errors`, then `data`.
- 2026-11-13: Each part of the response is quoted as the server wrote it. This replaces returning the body whole.
- 2026-11-13: A GraphQL response with `errors` and no `data` is a result, whatever the status. This replaces the
  2026-11-12 line that made it an error. JSON with neither `data` nor `errors` is still an error.
- 2026-11-13: `errors` are never cut. The 40,000-character cap is met by cutting what follows them.
- 2026-11-13: Response headers are shown only when `response_headers` names them. The agent cannot choose them.
- 2026-11-13: A shown response header has secrets removed as a body does, and is cut at 200 characters.
- 2026-11-13: A request that fails or times out says how many milliseconds fieldtrip waited.
- 2026-11-13: `mutations = "deny"` makes `execute` refuse every mutation, before validating it or asking anyone.
- 2026-11-13: The deny check runs on the parsed text, so a mutation is refused even if it isn't valid.
- 2026-11-13: `mutations` can come from a template, and an endpoint can only make it stricter.
- 2026-11-13: An endpoint that sets `mutations = "prompt"` under a template that denies them stops the server.
- 2026-11-13: With mutations denied, `estimate_cost` still costs them and `introspect` lists them as unavailable.
- 2026-11-13: Introspection reads up to `max_schema_bytes`, 32 MiB unless set. This lifts the 1 MiB read for schemas.
- 2026-11-13: A schema over that limit is an error naming it and `schema`. It never blames introspection being off.
- 2026-11-13: `execute` reads up to `max_response_bytes` (8 MiB) and a result holds up to `max_output_chars` (40,000).
- 2026-11-13: Both are set per endpoint or template. Together with the line above, they replace the 1 MiB and 40,000.
