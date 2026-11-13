# Using fieldtrip with OpenCode

This guide sets fieldtrip up as a local MCP server in [OpenCode](https://opencode.ai) V2. It covers installing the
binary, registering it, and making sure nothing runs against your API until you approve it.

> fieldtrip has not yet been tested end to end in OpenCode. The steps follow OpenCode 2.2.7's documentation for
> [MCP servers](https://opencode.ai/docs/mcp-servers/) and [permissions](https://opencode.ai/docs/permissions/), and
> its source. If a step doesn't match what your version does, please open an issue. OpenCode V1 uses a different
> config shape, and this guide doesn't cover it.

## Read this first: approval in OpenCode

fieldtrip normally asks you to approve each operation through an MCP elicitation prompt. Its prompt is a plain
accept or decline, with no fields to fill in. **OpenCode accepts a prompt like that automatically, without showing
it to you.** If you use fieldtrip's default approval in OpenCode, every operation runs unseen, mutations included.

So in OpenCode, approval has to come from OpenCode itself: set `approval = "client"` on every endpoint, and add an
OpenCode permission rule that asks before `fieldtrip_execute`. Steps 2 and 4 below do both. Don't skip either one.

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
url      = "https://api.acme.example/graphql"
approval = "client"   # required in OpenCode: see "Read this first"

[endpoints.acme.headers]
X-Api-Key = { env = "ACME_API_KEY" }
```

Set `approval = "client"` on every endpoint you use from OpenCode. With it, fieldtrip sends every operation it is
given without prompting, and OpenCode's permission prompt from step 4 is what stops it. `auto_approve_cost` has no
effect in this mode.

You can't use the `--url` flag from OpenCode, because an endpoint defined by `--url` uses fieldtrip's own prompt.
Write a config file instead.

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

## 4. Make OpenCode ask before every execute

OpenCode V2 allows every action unless a rule says otherwise, so add a rule that asks before `fieldtrip_execute`. Put
it in the same OpenCode config file as the server:

```jsonc
{
  "permissions": [
    { "action": "fieldtrip_execute", "resource": "*", "effect": "ask" },
  ],
}
```

When OpenCode asks, the request includes the tool's input: the endpoint, the operation and its variables. Read the
operation before you answer, and choose **Allow once**.

- **Never choose Allow always** for `fieldtrip_execute`. It saves a rule that lets every later `execute` run
  without asking, on that project, from then on. If you chose it by mistake, remove the saved approval.
- **Don't run `opencode run` with `--auto`** (or its hidden variants) in a project that has fieldtrip. That flag
  approves every permission that isn't explicitly denied, `fieldtrip_execute` included.
- The other five tools only read the schema or cost an operation, and never send it, so they don't need a rule.

## 5. Check that approval reaches you

Ask the agent to run something small:

> Use fieldtrip to run `{ __typename }` against acme.

OpenCode should ask for permission to use `fieldtrip_execute` before anything is sent. Reject it, then check that the
agent reports the call was rejected. Then try again and choose **Allow once** to see the query run.

If the query runs without OpenCode asking, stop and check two things: the `permissions` rule from step 4, and that
you didn't save an "Allow always" for it earlier.

OpenCode's prompt doesn't show fieldtrip's cost estimate. If cost matters, ask the agent to call
`fieldtrip_estimate_cost` before `execute`, or call it yourself.

## Protocol revision

OpenCode starts local servers with the classic `initialize` handshake by default, and fieldtrip supports it. fieldtrip
also speaks MCP revision 2026-07-28, so `"protocol": "auto"` on the server works too. You don't need it.

## Example prompts

- "List the fieldtrip endpoints, then introspect acme."
- "Search the acme schema for refunds and describe `Mutation.refundOrder`."
- "Write a query for the 20 most recent orders with their line items, estimate its cost, and show me both before you
  run it."

## Troubleshooting

- **fieldtrip doesn't connect.** Run the same command in a terminal. A config or cost map that isn't valid stops
  fieldtrip at startup, and the error says why. Use `/mcps` in OpenCode to reconnect after fixing it.
- **You need more detail.** Set `FIELDTRIP_LOG` to `debug` in `environment`. fieldtrip logs only to stderr, so
  logging can't corrupt the connection.
- **A header seems to be missing.** `fieldtrip_list_endpoints` shows each endpoint's header names, though never
  their values. For an `env` header, check that the variable is set in the shell that started OpenCode.
