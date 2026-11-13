# Using fieldtrip with OpenCode

This guide sets fieldtrip up as a local MCP server in [OpenCode](https://opencode.ai) V2. It covers installing the
binary, registering it, and checking that the approval prompt reaches you before anything is sent.

> fieldtrip has not yet been tested end to end in OpenCode. The steps follow OpenCode's V2 documentation for
> [MCP servers](https://opencode.ai/docs/mcp-servers/) and [permissions](https://opencode.ai/docs/permissions/). If
> a step doesn't match what your version does, please open an issue. OpenCode V1 uses a different config shape, and
> this guide doesn't cover it.

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

The quickest way is the CLI. Run it from the project that should use fieldtrip, or add `--global` to make it
available in every project:

```sh
opencode mcp add fieldtrip -- fieldtrip --config ~/.config/fieldtrip/config.toml
```

To write the config by hand, add fieldtrip under `mcp.servers` in `opencode.jsonc` at the root of a project, or in
`~/.config/opencode/opencode.jsonc` for every project:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "fieldtrip": {
        "type": "local",
        "command": ["fieldtrip", "--config", "/home/you/.config/fieldtrip/config.toml"],
        "codemode": false,
        "environment": {
          "FIELDTRIP_LOG": "info",
        },
      },
    },
  },
}
```

- `command` is one list: the program, then its arguments. JSON doesn't expand `~`, so write the full path.
- `"codemode": false` puts fieldtrip's six tools directly on the agent's tool list instead of behind OpenCode's Code
  Mode. That way the agent reads each tool's description, which says when to use the tool and in what order. This
  is a recommendation, not a requirement.
- For a single endpoint that needs no headers you can skip the fieldtrip config file:
  `"command": ["fieldtrip", "--url", "https://api.example.com/graphql"]`.
- Keep secrets out of OpenCode's config, especially a config committed to a project. fieldtrip inherits OpenCode's
  environment, so export `ACME_API_KEY` in the shell you start `opencode` from.

Run `opencode mcp list` and check that fieldtrip shows as connected. Its tools are named `<server>_<tool>`:

| Tool | What it's for |
| --- | --- |
| `fieldtrip_list_endpoints` | The endpoints in your config, and which headers each one has |
| `fieldtrip_introspect` | Load an endpoint's schema and get an overview of it |
| `fieldtrip_search_schema` | Find queries, mutations, types and fields by name |
| `fieldtrip_describe` | Read one type or field as SDL |
| `fieldtrip_estimate_cost` | Validate and cost an operation without sending it |
| `fieldtrip_execute` | Run an operation, after you approve it |

## 4. Check that approval reaches you

fieldtrip asks you to approve each operation through MCP elicitation, and OpenCode shows such a request as a form.
Ask the agent to run something small:

> Use fieldtrip to run `{ __typename }` against acme.

OpenCode should show *"fieldtrip is requesting input"* with the endpoint, the operation, its variables and its
estimated cost. Accept it and the query runs. Decline or dismiss it and nothing is sent.

If the tool instead fails with *"This MCP client cannot show approval prompts, because it does not support
elicitation…"*, nothing was sent, and the client didn't offer elicitation. Use the fallback below.

### Fallback: let OpenCode do the approving

Use this only if the prompt above never appears. Set `approval = "client"` on the endpoint:

```toml
[endpoints.acme]
url      = "https://api.acme.example/graphql"
approval = "client"
```

With this set, fieldtrip sends every operation it is given, mutations included, without prompting. OpenCode's own
permission check becomes the only gate. OpenCode V2 allows every action by default, so add a rule that asks before
each `execute`:

```jsonc
{
  "permissions": [
    { "action": "fieldtrip_execute", "resource": "*", "effect": "ask" },
  ],
}
```

Then repeat the test above and confirm that OpenCode asks before the query is sent.

When it asks, choose **Allow once**. **Allow always** saves a rule that lets every later `execute` run without a
prompt, so after that nothing stands between the agent and your API. If you chose it by mistake, remove the saved
approval.

This fallback also gives up two things:

- OpenCode's prompt shows the tool's arguments, which are the operation and its variables, but not fieldtrip's cost
  estimate. Ask the agent to call `fieldtrip_estimate_cost` first, or call it yourself.
- `auto_approve_cost` has no effect, because fieldtrip no longer prompts for anything.

## Protocol revision

OpenCode starts local servers with the classic `initialize` handshake by default, and fieldtrip supports it. fieldtrip
also speaks MCP revision 2026-07-28, so `"protocol": "auto"` on the server works too. You don't need it.

## Example prompts

- "List the fieldtrip endpoints, then introspect acme."
- "Search the acme schema for refunds and describe `Mutation.refundOrder`."
- "Write a query for the 20 most recent orders with their line items, estimate its cost, and run it if it's under
  100."

## Troubleshooting

- **fieldtrip doesn't connect.** Run the same command in a terminal. A config or cost map that isn't valid stops
  fieldtrip at startup, and the error says why. Use `/mcps` in OpenCode to reconnect after fixing it.
- **You need more detail.** Set `FIELDTRIP_LOG` to `debug` in `environment`. fieldtrip logs only to stderr, so
  logging can't corrupt the connection.
- **A header seems to be missing.** `fieldtrip_list_endpoints` shows each endpoint's header names, though never
  their values. For an `env` header, check that the variable is set in the shell that started OpenCode.
