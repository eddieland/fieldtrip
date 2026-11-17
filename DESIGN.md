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
| `list_endpoints` | none | Each endpoint's name, URL, schema source, header names and header commands' last run |
| `list_operations` | `endpoint`, `query?`, `uses?`, `kind?`, `name?` | Saved operations: name, kind, description, variables, cost, text |
| `introspect` | `endpoint`, `refresh?` | Overview: root types, counts, query and mutation names |
| `search_schema` | `endpoint`, `query` | Glossary targets, then ranked matches as one-line signatures |
| `describe` | `endpoint`, `name` (`Type` or `Type.field`) | That definition as compact SDL. A type adds what uses it and paths to it from `Query` |
| `estimate_cost` | `endpoint`, `operation` or `operation_name`, `variables?` | Validation errors, or cost, depth, breakdown, assumptions |
| `execute` | `endpoint`, `operation` or `operation_name`, `variables?`, `select?`, `save?`, `paginate?` | Status, `errors`, then the rest, or a file's path with a preview |
| `execute_batch` | `endpoint`, `operation` or `operation_name`, `variables` (a list of sets), `select?` | One section per run: status, `errors`, then the rest or a summary |
| `read_result` | `result`, `select?` | Part of a response `execute` wrote to a file, by a path. Sends nothing |
| `history` | `endpoint?`, `limit?` | The latest `execute` attempts from the audit log. Only offered with `audit_log` |

## Config shape

```toml
audit_log = "~/.local/state/fieldtrip/audit.jsonl"   # optional: one JSON line per `execute` attempt
result_ttl = 86400                           # optional, seconds a response file is kept, default 1 day

[templates.acme]                             # optional: settings that several endpoints share
costs             = "acme.costs.toml"        # optional cost map, relative to this file
glossary          = "acme.glossary.toml"     # optional: the user's words for things in the schema
operations        = "operations/acme/"       # optional directory of saved operations, one per .graphql file
fragments         = "operations/acme/fragments/"   # optional directory of fragments the saved operations share
client_directives = ["paginated", "baseClass"]     # optional: directives removed from saved operations
assumed_list_size = 20                       # optional, default 10
timeout           = 30                       # optional, seconds, default 30
response_headers  = ["x-request-id", "traceparent"]   # optional, shown by `execute`
max_schema_bytes  = 33554432                 # optional, default 32 MiB, read of an introspection response
max_response_bytes = 8388608                 # optional, default 8 MiB, read of a response to `execute`
max_output_chars  = 40000                    # optional, default 40,000, characters in one tool result
max_inline_chars  = 10000                    # optional, default 10,000, a longer response goes to a file
max_batch         = 100                      # optional, default 100, requests one call sends: runs, or pages
audit_variables   = false                    # optional, default true: leave variables out of the audit log
schema_ttl        = 86400                    # optional, seconds a schema is cached, default 1 day

[templates.acme.headers]
X-Client      = "fieldtrip"
Authorization = { command = ["python", "-m", "acme_auth", "token"], format = "Bearer {}", ttl = 300 }
X-Correlation-Id = { generate = "uuid" }     # optional: a new value for each request, shown by `execute`

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

An endpoint takes `schema`, `schema_ttl`, `headers`, `costs`, `glossary`, `operations`, `fragments`, `client_directives`,
`assumed_list_size`, `timeout`, `response_headers`, the four `max_` limits, `max_batch` and `audit_variables` from its
template unless it sets them.
Headers merge by name, ignoring case, and the endpoint's win.
`url`, `description`, `approval` and `auto_approve_cost` are only ever set on the endpoint. `mutations` comes from the
template too, but an endpoint can only make it stricter. A path may start with `~/`.

The config path comes from `--config`, defaulting to `$XDG_CONFIG_HOME/fieldtrip/config.toml` or
`~/.config/fieldtrip/config.toml`. `--url`, with an optional `--schema`, defines a single endpoint with no headers and
no file. `fieldtrip check` loads the same configuration, prints what each endpoint resolves to and exits.
`fieldtrip check --introspect` also fetches the schema of each endpoint that has no `schema` file.

## The plugs

A header value comes from one of four plugs. `env` and `command` take an optional `format`, in which `{}` is replaced
by the value.

| Plug | Config | Value |
| --- | --- | --- |
| Literal | `"fieldtrip"` | The string as written |
| Environment | `{ env = "NAME" }` | The variable, read when the request is made |
| Command | `{ command = ["python", "-m", "acme_auth", "token"] }` | The command's stdout, trimmed |
| Generated | `{ generate = "uuid" }` or `{ generate = "traceparent" }` | A new UUID v4, or W3C trace context, per request |

The first three are secrets. A generated value is an ID, and is shown so the request can be found in the server's logs.

## Cost map

The built-in costs are guesses. An endpoint's `costs` file replaces them wherever its owner knows better. Each rule
is a pattern over the fields of an operation, and sets a `cost`, a `multiplier`, or both.

```toml
"Query.products"  = { multiplier = 500 }      # an exact path from the root
"User.orders"     = { multiplier = "take" }   # sized by the `take` argument
"Query.orders"    = { multiplier = "first", default = 100 }   # and 100 when `first` is left out
"Query.itemsByIds" = { multiplier = { length = "ids" } }   # one item for each of `ids`. `per = 3` gives three
"*.reviews"       = { multiplier = 3 }        # `reviews` on any type
"Mutation.refund" = { cost = 100 }
```

A pattern is a type followed by one or more field names, and it matches a field reached through that chain. `*`
stands for any one name. `cost` replaces what the field itself costs. `multiplier` replaces the size of a list.

## Glossary

People name things in the words of the product they use, not the schema's. An endpoint's `glossary` file maps each
of the user's terms to the schema's names for it: a type, or a type and one of its fields, input fields or enum values.

```toml
[glossary]
"PR"    = ["PullRequest"]
"draft" = ["PullRequest.isDraft"]
"org"   = ["Organization"]
```

`search_schema` and `list_operations` look each word of a query up here first, and mark what they found with the
term, as in `draft → PullRequest.isDraft`.

## Approval model

`execute` validates and costs the operation, then asks the human through MCP elicitation. The prompt is built from
the operation that will be sent and shows the endpoint, the kind (query or mutation), the operation text, the
variables and the estimated cost. The request is sent only on an explicit accept. Up to MCP revision 2025-11-25 the
prompt is a request from the server. From 2026-07-28 it is an "input required" result, answered on a retry.

A query's prompt has one optional field, `approve`: "this request only", the default, or "this query with any
variables, until fieldtrip restarts". A mutation's prompt has no field.

| Situation | Outcome |
| --- | --- |
| Human accepts | Sent |
| Human declines, cancels, or the prompt fails or times out | Not sent |
| Client has no elicitation support | Not sent |
| Query with cost at or below `auto_approve_cost` | Sent without a prompt |
| Query the human approved for the session, with any variables | Sent without a prompt |
| `execute_batch` | One prompt for all its runs, or none when the total cost is waived as a query's is |
| `execute_batch` with a mutation | Not sent, and no prompt is shown |
| `execute` with `paginate` | One prompt for all its pages, at `max_pages` times the cost of one |
| Endpoint sets `approval = "client"` | Sent without a prompt, mutations included |
| Mutation, and the endpoint or its template sets `mutations = "deny"` | Not sent, and no prompt is shown |
| A prompt is needed, and the client is known to accept prompts unseen | Not sent, and no prompt is shown |

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
- 2026-11-13: Parts after `errors` too long for a result are summarized as valid JSON, not cut as text. This refines
  the line above that cut what follows `errors`. Parts that fit are as written.
- 2026-11-13: A summary cuts lists and strings, each marked with what was dropped. `errors` is still never cut.
- 2026-11-13: `select` is a path that picks part of a response after it arrives. It is not in the request or prompt.
- 2026-11-13: `save` writes the whole response to a new file in fieldtrip's cache directory. The agent cannot name it.
- 2026-11-13: A saved response has header values removed, as any response does. Only the user can read the file.
- 2026-11-13: A connection whose `pageInfo` says `hasNextPage: true` is pointed out in the result, with its cursor.
- 2026-11-14: `operations` names a directory of `.graphql` and `.gql` files, each one named query or mutation. It is
  config: read once, at startup, and no tool can change it. An endpoint takes it from its template unless it sets it.
- 2026-11-14: A saved operation's leading `#` comment lines are its description. Its name is how tools refer to it.
- 2026-11-14: A file that is not one named query or mutation, two files with one name, or a missing directory stop
  the server. An operation that no longer validates against the schema is only a warning, and is listed as broken.
- 2026-11-14: `execute` and `estimate_cost` take `operation_name` instead of `operation`. Approval does not change:
  the prompt shows the saved operation's canonical text, and that is what is sent.
- 2026-11-14: Saved operations are checked whenever the schema loads, which for a schema file is at startup. `check`
  checks them against schema files only, since it sends nothing.
- 2026-11-14: A saved operation is approved like any other. It carries no approval setting of its own.
- 2026-11-14: `list_operations` filters by `uses`, written in the cost map's pattern language. A bare type name is
  allowed there too, and matches a field on that type or one that returns it. Every pattern must match.
- 2026-11-14: A library can hold hundreds of operations. Past five matches, `list_operations` gives one line each and
  `name` shows one in full. `search_schema` lists at most ten saved operations.
- 2026-11-14: `audit_log`, set once at the top of the config, appends one JSON line per `execute` attempt. Off unless
  set. It is opened at startup, and a log that cannot be opened stops the server.
- 2026-11-14: An attempt is a call that reached the approval step, or a mutation refused by `mutations = "deny"`. An
  operation that is not valid is not one. A prompt that is never answered is not one either.
- 2026-11-14: A line records the operation's text and SHA-256, its variables, who approved it (`accepted`, `auto`,
  `client`) or why it was not sent (`declined`, `cancelled`, `unavailable`, `failed`, `denied`), and what came back.
- 2026-11-14: The log never holds a header value. `env` and `command` values in a line are removed, as from a body,
  using values already known: no command runs for the log. `audit_variables = false` leaves the variables out.
- 2026-11-14: A line that cannot be written is logged to stderr. The attempt has happened, so the call is not failed.
- 2026-11-14: `history` reads the log back as it is written, newest last, and is offered only when `audit_log` is set.
- 2026-11-14: `history` shows only endpoints in the current config, since several configs can share one log.
- 2026-11-14: `list_endpoints` shows how each header command's last run went: ok, failed and why, or not run yet.
- 2026-11-14: That status is only read, never produced: listing endpoints runs no command and sends nothing.
- 2026-11-14: A failure is shown by a short reason, such as `exit 3`. Stdout and stderr are not shown there.
- 2026-11-14: When the retry after a 401 is refused with a 401 too, the result says both happened and names each
  header and program that ran again, so the user knows to sign in again rather than ask for a permission.
- 2026-11-14: A command that fails when run again after a 401 is reported with that 401 as the reason it ran.
- 2026-11-14: An introspected schema is kept as SDL under `fieldtrip/schemas` in the user's cache directory, per URL.
- 2026-11-14: A kept schema is used instead of introspecting for `schema_ttl` seconds, a day unless set. 0 keeps none.
- 2026-11-14: `introspect(refresh = true)` never reads the cache. It introspects, and keeps the result.
- 2026-11-14: When introspection fails, a kept schema of any age stands in, except on a refresh. `introspect` says
  how old it is and why, and every other tool that reads it says it may be out of date.
- 2026-11-14: A kept schema's file is named by a hash of the URL and holds only the shown URL. Only the user can
  read it.
- 2026-11-14: A client known to accept a prompt with no fields without showing it is never asked. Where a prompt is
  needed, `execute` refuses, on both protocol revisions. The client is known by its `clientInfo.name`.
- 2026-11-14: OpenCode (`opencode`, any case) is such a client. The list is a constant in the code, not config.
- 2026-11-14: `approval = "client"` and `auto_approve_cost` still apply to such a client: neither depends on a prompt.
- 2026-11-14: When such a client connects with the `initialize` handshake, a warning names the endpoints affected.
  `list_endpoints` says so on each of them.
- 2026-11-14: The audit log records that refusal as `unavailable`, with a reason that names the client.
- 2026-11-16: `fragments` names a directory of `.graphql` and `.gql` files holding only fragment definitions, shared by
  the saved operations of an endpoint or template. It is config like `operations`: read once, at startup.
- 2026-11-16: A saved operation gets each shared fragment it spreads and does not define, following spreads, appended
  to its text once. From then on it is as if its file held them. A fragment in the operation's own file wins.
- 2026-11-16: The prompt and the request carry the full text, shared fragments included, as for any saved operation.
- 2026-11-16: Only saved operations use shared fragments. An operation the calling agent writes stays self-contained.
- 2026-11-16: A fragment name defined twice in `fragments`, a file there holding anything else, a missing directory,
  or `fragments` on an endpoint with no `operations` stop the server. A fragment found nowhere breaks only the
  operation that spreads it. `check` warns about a shared fragment no operation uses.
- 2026-11-16: A fourth plug, `{ generate = "uuid" }` or `{ generate = "traceparent" }`, gives a header a new value for
  each request. This replaces the enum of three set on 2026-11-12. It takes no `format` and no `ttl`.
- 2026-11-16: A generated value is not a secret. `execute` shows it, as `sent <name> <value>`, and the audit log
  records it under `sent`, for a request that got no answer too. It is never removed from a response.
- 2026-11-16: A value is generated once per call. The retry after a 401 repeats it, so both attempts share an ID.
- 2026-11-16: A request that never left, because a header could not be resolved, reports no generated value.
- 2026-11-16: A bad file in `operations` or `fragments` is left out and reported, and the rest load. This replaces
  the 2026-11-14 line that stopped the server for such a file, and the matching part of the 2026-11-16 line for
  fragments. A missing directory, and `fragments` with no `operations`, still stop it.
- 2026-11-16: When several files use one operation name, all of them are left out, so a name never runs a file the
  user didn't mean. `execute` with that name says which files clash.
- 2026-11-16: A fragment name defined twice in `fragments` is left out, and so is every operation that spreads it.
- 2026-11-16: `list_operations` lists the files left out and what is wrong with each, `list_endpoints` counts them,
  and `check` and the startup log give a warning for each. A file left out is a warning, as a broken operation is.
- 2026-11-16: `client_directives`, on an endpoint or template, names directives that only the user's code generator
  reads. Every use of one is removed from saved operations and shared fragments as they are read, so what is
  validated, costed, shown in the prompt and sent has none of them. The prompt still shows what the server receives.
- 2026-11-16: Only listed names are removed. A directive neither listed nor in the schema still breaks the operation,
  so a typo is caught. An operation the calling agent writes is validated as written.
- 2026-11-16: `skip`, `include`, `defer` and `stream`, a name written with `@`, or one that is not a GraphQL name, stop
  the server, as does `client_directives` on an endpoint with no `operations`. `check` warns about a listed directive
  that no saved operation or shared fragment uses.
- 2026-11-17: A rule whose `multiplier` names an argument can set `default`: the size when the operation leaves the
  argument out, gives it as null, or gives a variable with no value. It stands for the page size a server applies on
  its own, so nothing is assumed. `default` with any other rule stops the server.
- 2026-11-17: A default the schema declares for the argument still wins over the rule's `default`. The estimate says
  which of the argument and the default it used.
- 2026-11-17: A `multiplier` written `{ length = "ids" }` sizes a list by how many items the list argument `ids`
  holds, in the operation or in the variables. `per`, 1 unless set, multiplies it: the items that come back for each
  one passed in. One value where a list is expected counts as one.
- 2026-11-17: When that length is not known, as for a variable with no value, the assumed size is used and listed.
  `introspect` and `check` report a `length` of an argument that no matched field has as a list.
- 2026-11-17: `check` checks the cost map and saved operations of an introspected endpoint against the kept copy of
  its schema, whatever its age, and says how old the copy is. It still sends nothing. This replaces the part of the
  2026-11-14 line that had `check` use schema files only. With no copy, or `schema_ttl = 0`, they are not checked.
- 2026-11-17: `check --introspect` introspects each endpoint that has no `schema` file, ignoring any kept copy, and
  keeps the result as `introspect` does. It is the one way `check` sends anything, and only the user can ask for it.
  This refines the 2026-11-13 line that `check` sends nothing. An introspection that fails is a problem.
- 2026-11-17: A variable that only removed client directives used is removed from the saved operation with them,
  since a declared variable that nothing uses is not valid. Shared fragments count, both for what was removed and for
  what still uses a variable. One that was unused before the directives were removed stays and is reported.
- 2026-11-17: A dropped variable passed with `operation_name` is left out with a note, not refused, since the
  generated client would pass it. The prompt, the request and the audit log show the variables that are sent.
- 2026-11-17: An endpoint can name a `glossary`: a TOML file whose `[glossary]` table maps each of the user's terms to
  a list of targets, each a `Type` or a `Type.member`. It is config like the cost map: read once, at startup, no tool
  can change it, and an endpoint takes it from its template unless it sets its own.
- 2026-11-17: A term is one or more words, matched as whole words in a row, ignoring case and spacing. Where terms
  overlap in a query, the longest wins.
- 2026-11-17: `search_schema` shows the targets of each term in the query first, before saved operations and the
  schema's own matches, each as `term → signature`. A target is not repeated among the matches below it.
- 2026-11-17: In the `query` of `list_operations`, and in the saved operations `search_schema` lists, a term matches
  an operation that uses one of its targets, as `uses` would, or one whose name or description holds the term. Those
  found through a target come first and are marked `glossary: term → Target`. Every other word matches as before.
- 2026-11-17: A glossary that is not valid stops the server at startup: a file that does not parse, a key outside
  `[glossary]`, a term with no words or no targets, two terms that differ only in case, or a target that is not a
  `Type` or `Type.member`. A target the schema lacks is not an error.
- 2026-11-17: `introspect` reports the targets the schema lacks, as it does unmatched cost rules, and `check` warns
  about them. A search for such a term still shows the target, marked as not in the schema. `list_endpoints` shows
  the glossary file and how many terms it has.
- 2026-11-17: A response to `execute` longer than `max_inline_chars` (10,000, set per endpoint or template) is written
  whole to a file without being asked, and the result has its path and size, `errors` in full, a preview and the shape
  of the response, never the body. A shorter one is shown as before. This replaces the 2026-11-13 summary of up to
  `max_output_chars` for a response to `execute`. That summary remains for a file that could not be written.
- 2026-11-17: `max_inline_chars` cannot show more than `max_output_chars`: the smaller applies. Under 1,000 stops the
  server.
- 2026-11-17: The preview is the 2026-11-13 summary, in at most 4,000 characters, and at least 1,000 of them after
  `errors` however long those are. The shape is one line per path with its type, list items merged, at most 40 lines.
- 2026-11-17: With `select`, the response is still written whole when it is too long, and the selection is shown up to
  `max_inline_chars`.
- 2026-11-17: A response file goes in a directory of the session's own, `fieldtrip/responses/<session>`, in the cache
  directory, and is named by the endpoint and an ID fieldtrip makes for it. This refines the 2026-11-13 line on `save`,
  which still forces a file at any size.
- 2026-11-17: `read_result` takes a result, by the ID or the path `execute` gave, and a `select` path, and returns that
  part of the file, after `errors` in full. It sends nothing, so it asks nobody. Without a path it gives the preview
  and the shape again. No jq: `select` is the one path language.
- 2026-11-17: `read_result` reads only files under `fieldtrip/responses`, once links and `..` are followed. The calling
  agent is not trusted with a path to anything else, so any other path is refused, whatever the file holds.
- 2026-11-17: A result's ID is its file's name without `.json`. An ID is looked for in this session's directory first,
  then in those of earlier sessions.
- 2026-11-17: What `read_result` selects is held to the `max_output_chars` of the endpoint the result is named after,
  not to `max_inline_chars`, because the calling agent asked for that part.
- 2026-11-17: A response file is deleted `result_ttl` seconds after it was written, a day unless set, at the top of
  the config like `audit_log`, since every endpoint's files share one directory. Zero stops the server.
- 2026-11-17: Nothing runs in the background. Old files are deleted at startup, before a file is written, and before
  one is looked for, whichever session wrote them. A session's directory goes with its last file.
- 2026-11-17: Only `.json` files in `fieldtrip/responses` and in the directories directly inside it are deleted, which
  includes what `save` wrote there before sessions had directories. Links are not followed.
- 2026-11-17: An audit line for an attempt whose response went to a file has `result`, the file's ID. The file is that
  session's `<result>.json`, so the log holds no path.
- 2026-11-17: `history` still gives the lines as written. After them it lists the files that still exist for the lines
  shown, and counts those that are gone. This adds to the 2026-11-14 line on `history`.
- 2026-11-17: `describe` of a type ends with what uses it: `# member of:` names the unions it belongs to, then
  `# used by:` lists every field that returns it, argument that takes it, as `Type.field(argument:)`, and input field
  that holds it, lists and `!` aside. A type nothing uses says so. `describe` of a field is unchanged.
- 2026-11-17: Uses are ordered by the root types, query, mutation and subscription, then the other types by name, and
  by field order within a type, so an introspected schema and a schema file answer alike. At most 30 are listed and
  the rest are counted. Root types and built-in types list none.
- 2026-11-17: Directive arguments are not uses, since an introspected schema carries no directive definitions.
- 2026-11-17: `describe` of a type that can be selected ends with the shortest paths to it from `Query`: for each field
  of `Query` that leads there, the shortest way through it, as `Query.users.nodes`. A step from an interface or a
  union to one of its types is written `{... on Type}` and is not counted, since it selects nothing.
- 2026-11-17: Paths are ordered by how many fields they hold, then by the order of the fields of `Query`. At most five
  are shown and the rest counted. Where several ways through one root field are as short, fields come before
  `... on`, then the schema's field order, then possible types by name, so both schema sources answer alike.
- 2026-11-17: A path never passes through a root type, so a field that returns `Query` is not a way in. A type with no
  path says so. Input objects, root types, built-in types and `describe` of a field have none.
- 2026-11-17: A query's prompt has one optional field, with which the human approves that query for the rest of the
  session. This replaces the 2026-11-12 line that gave the prompt no form fields, for queries. A mutation's prompt
  still has none, and a mutation prompts every time, which the 2026-11-12 line on mutations already says.
- 2026-11-17: Only an accept that explicitly chooses the session grants it. An accept with the field left out or set
  to anything else approves the one request, so a client that accepts unseen can grant nothing more.
- 2026-11-17: A session approval covers one endpoint and one canonical text, by its SHA-256, with any variables,
  whatever they cost. It is kept in memory and ends when the server does. No tool can grant, list or extend one.
- 2026-11-17: A run let through by a session approval says so in its result, and the audit log records it as
  `session`. The accept that granted it is `accepted`, with a reason that says so.
- 2026-11-17: An approval on revision 2026-07-28 is tied to the tool that asked, as well as to the operation and its
  variables.
- 2026-11-17: `execute_batch` sends one query once for each set in a list of variables, under one approval. It is a
  tool of its own, so the approval covers the batch and `execute` stays one request for one approval.
- 2026-11-17: A batch's prompt shows the canonical text, the number of runs, the first five sets of variables and
  the total estimated cost. The sets past the fifth are counted, not shown. This is the one place where the human
  approves variables they were not shown, and it refines the 2026-11-12 line that the prompt shows the variables.
- 2026-11-17: A batch is queries only. A mutation is refused with no prompt, whatever `approval` says, since each
  mutation is approved on its own.
- 2026-11-17: `auto_approve_cost` is compared with a batch's total cost. A query approved for the session runs as a
  batch unasked. A batch's prompt has no field, so accepting one never approves the query for the session.
- 2026-11-17: `max_batch`, 100 unless an endpoint or its template sets it, caps the runs of one batch. Zero stops
  the server.
- 2026-11-17: Every set is validated and costed before anyone is asked. The runs are sent in order, one at a time.
  A run's GraphQL `errors` do not stop the batch. A run that gets no answer does, and the rest are not sent.
- 2026-11-17: A batch's result has a section per run, numbered from 0 by the position of its set, and each gets an
  equal share of `max_output_chars`, summarized as any response is. `select` applies to every run. There is no
  `save`, and no run's response is written to a file, whatever `max_inline_chars` is.
- 2026-11-17: The audit log has a line for each run that was sent, with `batch: {run, of}`. A batch that was not sent
  is one line, with every set under `variables` and `batch: {of}`.
- 2026-11-17: `execute` takes `paginate = { path, max_pages?, variable? }` and follows the pages of the connection
  at `path` under one approval. `path` is `data` and names, to the one object that holds `pageInfo`.
- 2026-11-17: After each page, `endCursor` goes into the variable named by `variable`, `after` unless given, which
  the query must declare. Nothing else about the request changes. The first page starts from the variables given.
- 2026-11-17: `max_pages` is 5 unless given, and `max_batch` caps it as it caps a batch: it is the most requests one
  call sends.
- 2026-11-17: The prompt shows the canonical text, the variables, the path, the variable and the worst case:
  `max_pages` times the estimate for one page. That worst case is what `auto_approve_cost` is compared with. The
  prompt has no field. A query approved for the session is followed unasked.
- 2026-11-17: Following is for queries. A mutation with `paginate` is refused with no prompt. `paginate` does not
  combine with `save`. `select` applies to every page.
- 2026-11-17: It stops at `hasNextPage: false`, at `max_pages`, at a page with no `pageInfo` at the path, at a cursor
  that did not move, and at a request that gets no answer. The result's second line says which, and from which
  cursor to go on. Each page then has its own section, with an equal share of `max_output_chars`. No page is
  written to a file.
- 2026-11-17: The audit log has a line for each page sent, with `page: {page, max}`. A call that was not sent is one
  line, with `page: {max}` and the worst-case cost.
