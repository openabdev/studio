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
| `deploy_provision` | write | `library`, `template`, `overlay?`, `name`, `namespace?`, `image_tag?`, `provider?`, `fleet?`, `cluster?`, `region?`, `profile?`, `context?`, `expected_principal?` |
| `deploy_provision_agent` | write | `image`, `name`, `namespace?`, `api_key?`, `chat_platform?`, `chat_bot_token?`, `chat_channel_secret?`, `acp_enabled?`, `acp_token?`, `local_config_folder?`, `provider?`, `fleet?`, `cluster?`, `region?`, `profile?`, `context?`, `expected_principal?` |
| `deploy_scale` | write | `name`, `size` (0/1), `cluster?`, `namespace?` |
| `deploy_delete` | write | `resource`, `name`, `cluster?`, `namespace?` |

Reads project each ECS instance onto the canonical 6-state `AgentState`
(ADR-1). `deploy_scale` is 0 (off) / 1 (on) only — an OAB service runs a single
bot token, so >1 would duplicate responders. Both `deploy_provision*` are create-or-update
(studio#111): an agent with **no** stored manifest gets a fresh one built and
applied; one that already has a stored manifest takes `redeploy`'s patch path,
reusing that manifest's networking/resources/secrets and changing only the image
and bundle. The table above is the argument list, and each tool's description in
`tools()` carries the per-argument contract.

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

The managing credential is resolved from `fleets.toml` in two steps: the
**fleet the call named** (`fleet:`) wins, else the binding whose `cluster` key
matches the call's cluster, else the ambient `[default]` chain. Fleet-name-first
matters because a binding that declares no `cluster` key — which neither the
console's writer nor the ADR's canonical example emits — is unreachable by
cluster alone, so its recorded `region` / `profile` would otherwise be read by
nothing and every call would answer as the ambient account. Both
`deploy_provision*` tools also take optional per-call `region` / `profile`
(studio#111), which **layer** onto whatever the base binding already selects
rather than replacing it: naming only `region` keeps the binding's profile.
Per-call overrides are not memoized — an override must never be cached under a
fleet's key for a later call that named no override.

One behavior change to be aware of when upgrading: a `fleet`-scoped call naming
a binding that declares **no** `profile`/`region` now answers the ambient chain,
where the per-cluster lookup used to lend it a same-cluster sibling's credential.
That sibling-lending was the accident per-fleet scoping exists to prevent, and a
binding that names no credential is asking for the ambient one — but if you rely
on it, give the binding its own `profile`/`region` (or say so per call).

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
