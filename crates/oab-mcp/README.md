# oab-mcp

The Studio control-plane **MCP server** (ADR-2). It exposes the `studio-cp`
read/write model as MCP tools over **stdio**, so an agent operates the OAB
control plane as a first-class client — "agents do control, humans direct."

## Tools

| Tool | Kind | Arguments |
|------|------|-----------|
| `deploy_list` | read | `cluster?` |
| `deploy_get` | read | `service`, `cluster?` |
| `get_agent_states` | read | `service?`, `cluster?` |
| `deploy_apply` | write | `manifest_yaml`, `cluster?`, `wait?` |
| `deploy_scale` | write | `name`, `size` (0/1), `cluster?`, `namespace?` |
| `deploy_delete` | write | `resource`, `name`, `cluster?`, `namespace?` |

Reads project each ECS instance onto the canonical 6-state `AgentState`
(ADR-1). `deploy_scale` is 0 (off) / 1 (on) only — an OAB service runs a single
bot token, so >1 would duplicate responders.

## Run

```sh
OAB_CLUSTER=oab cargo run -p oab-mcp
```

The server speaks newline-delimited JSON-RPC on stdin/stdout. AWS credentials
are resolved from the standard chain **lazily**, on the first real tool call —
`initialize` and `tools/list` need none. `deploy_delete` resolves the
control-plane bucket from `$OAB_CONTROL_PLANE_BUCKET` (or the caller's account);
none of the write paths read `~/.oabctl/config.toml`.

`cluster` / `namespace` default to `$OAB_CLUSTER` (then `oab`) and `default`,
and are overridable per call.

## Register (mcp.json)

```json
{
  "mcpServers": {
    "oab-studio": {
      "command": "oab-mcp",
      "env": { "OAB_CLUSTER": "oab" }
    }
  }
}
```
