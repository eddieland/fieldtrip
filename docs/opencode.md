# Using fieldtrip with OpenCode

This guide sets fieldtrip up as a local MCP server in [OpenCode](https://opencode.ai). It covers installing the
binary, registering it, and checking that the approval prompt reaches you before anything is sent.

> fieldtrip has not yet been tested end to end in OpenCode. If a step here doesn't match what your version of
> OpenCode does, please open an issue.

## 1. Install fieldtrip

```sh
cargo install --git https://github.com/eddieland/fieldtrip
```

Check that `fieldtrip` is on your `PATH` with `fieldtrip --help`. If OpenCode can't find it later, use the full
path, such as `~/.cargo/bin/fieldtrip`.

## 2. Write a fieldtrip config

fieldtrip reads `~/.config/fieldtrip/config.toml` unless you pass `--config`. A minimal config with one endpoint:

```toml
[endpoints.acme]
url               = "https://api.acme.example/graphql"
auto_approve_cost = 50   # optional: queries that cost 50 or less run without a prompt

[endpoints.acme.headers]
X-Api-Key = { env = "ACME_API_KEY" }
```

The [README](../README.md#configuration) and [DESIGN.md](../DESIGN.md#config-shape) describe every option, including
header commands and cost maps.

## 3. Register fieldtrip in OpenCode

Add fieldtrip under the `mcp` key of an OpenCode config file. Use `opencode.json` at the root of a project for that
project only, or `~/.config/opencode/opencode.json` for every project.

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "fieldtrip": {
      "type": "local",
      "command": ["fieldtrip", "--config", "/home/you/.config/fieldtrip/config.toml"],
      "environment": {
        "FIELDTRIP_LOG": "info"
      }
    }
  }
}
```

- `command` is one list: the program, then its arguments.
- For a single endpoint that needs no headers you can skip the fieldtrip config file:
  `"command": ["fieldtrip", "--url", "https://api.example.com/graphql"]`.
- Keep secrets out of `opencode.json`, especially one that is committed to a project. Export `ACME_API_KEY` in the
  shell you start `opencode` from, and fieldtrip reads it when it sends a request.

Restart OpenCode. fieldtrip's tools appear with the server name in front of them:

| Tool | What it's for |
| --- | --- |
| `fieldtrip_list_endpoints` | The endpoints in your config, and which headers each one has |
| `fieldtrip_introspect` | Load an endpoint's schema and get an overview of it |
| `fieldtrip_search_schema` | Find queries, mutations, types and fields by name |
| `fieldtrip_describe` | Read one type or field as SDL |
| `fieldtrip_estimate_cost` | Validate and cost an operation without sending it |
| `fieldtrip_execute` | Run an operation, after you approve it |

## 4. Check that approval reaches you

Ask the agent to run something small:

> Use fieldtrip to run `{ __typename }` against acme.

One of two things happens.

- **OpenCode shows you an accept or decline prompt** with the endpoint, the operation, its variables and its
  estimated cost. Approval works, and you're done. Nothing is sent until you accept.
- **The tool fails** with *"This MCP client cannot show approval prompts, because it does not support
  elicitation…"*. Nothing was sent. fieldtrip asks for approval through MCP elicitation, and your version of
  OpenCode doesn't support it. Use the fallback below.

### Fallback: let OpenCode do the approving

Set `approval = "client"` on the endpoint:

```toml
[endpoints.acme]
url      = "https://api.acme.example/graphql"
approval = "client"
```

With this set, fieldtrip sends every operation it is given, mutations included, without prompting. OpenCode's own
permission check becomes the only gate, so make sure it asks you before every `execute`:

```jsonc
{
  "permission": {
    "fieldtrip_execute": "ask"
  }
}
```

The exact `permission` syntax has changed between OpenCode versions, so check OpenCode's permissions docs for the
version you run. Then repeat the test above and confirm that OpenCode asks before the query is sent.

This fallback gives up two things:

- OpenCode's prompt shows the tool's arguments, which are the operation and its variables, but not fieldtrip's cost
  estimate. Ask the agent to call `fieldtrip_estimate_cost` first, or call it yourself.
- `auto_approve_cost` has no effect, because fieldtrip no longer prompts for anything.

## Example prompts

- "List the fieldtrip endpoints, then introspect acme."
- "Search the acme schema for refunds and describe `Mutation.refundOrder`."
- "Write a query for the 20 most recent orders with their line items, estimate its cost, and run it if it's under
  100."

## Troubleshooting

- **The server doesn't start.** Run the same `command` in a terminal. A config or cost map that isn't valid stops
  fieldtrip at startup, and the error says why.
- **You need more detail.** Set `FIELDTRIP_LOG` to `debug` in `environment`. fieldtrip logs only to stderr, so
  logging can't corrupt the connection.
- **A header seems to be missing.** `fieldtrip_list_endpoints` shows each endpoint's header names, though never
  their values. For an `env` header, check that the variable is set in the shell that started OpenCode.
