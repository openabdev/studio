# Connecting an agent to an OAB fleet

How an AI agent gets the OAB fleet tool surface (`deploy_list`, `deploy_get`,
`get_agent_states`, `deploy_events`, `deploy_apply`, `deploy_scale`,
`deploy_delete`, `runtime_context`, `fleet_config`, `fleet_config_write`).

There are two ways, for two different scenarios (see the
[Fleet grouping & connection model ADR](./adr/fleet-grouping-and-connection-model.md)):

- **(A) Headless `oab-mcp`** — the agent runs its **own** `oab-mcp`, no Studio.
  For CI, scripts, and always-on agents. **Available today.**
- **(B) Reverse MCP** — the agent attaches to a **running Studio** through OpenAB
  core, using Studio's identity. **Target; the serve mode is not built yet.**

---

## Worked example: connecting **Orca** (the always-on ECS agent)

Orca runs 24/7 in AWS ECS with an IAM **task role** (`openab-orca-task-role` @
`504190915686` / `ap-east-2`) that already reaches cluster `oab`. Three levels,
increasing capability:

### 0. Raw AWS — already working

Orca can observe and drive the cluster with the `aws` CLI/SDK under its task role
today (this is how the fleet was diagnosed and managed before Studio existed). No
setup. It just lacks the ergonomic `oab-mcp` tool surface.

### A. Headless `oab-mcp` — near-term, small setup

Give Orca the `oab-mcp` tools by running `oab-mcp` as an MCP server in its runtime:

1. **Provision the binary.** Put `oab-mcp` on Orca's image/host — bundled from
   Studio's release, or built from `crates/oab-mcp` (note: the workspace
   statically links the full aws-sdk; build with `-j1`, it OOMs otherwise).
2. **Credentials.** Orca needs no profile — its **task role** is the ambient
   credential chain, and `oab-mcp` uses `aws_config::load_defaults`. (A laptop
   would instead set `AWS_PROFILE=oab-fleet`.)
3. **Register the MCP server** in the agent's MCP config:

   ```json
   {
     "mcpServers": {
       "oab": {
         "command": "/path/to/oab-mcp",
         "env": { "OAB_CLUSTER": "oab", "AWS_REGION": "ap-east-2" }
       }
     }
   }
   ```
   - Fleet bindings are optional here: with the task role, calls resolve directly
     to `oab` @ `504190915686`. To pin a binding, add
     `"OAB_FLEETS_CONFIG": "/path/to/fleets.toml"`.
4. **Verify.** Have the agent call `runtime_context` — it should report
   `principal = …assumed-role/openab-orca-task-role`, `account = 504190915686`,
   `region = ap-east-2`. Then `deploy_list` should list `oab-prod-orca` /
   `oab-prod-mira`.

This is an **independent** `oab-mcp` instance (Orca's own creds, own process) —
it does not touch, and does not need, a running Studio.

### B. Reverse MCP through Studio — target (not yet built)

Once Studio implements the ACP-WS-client **serve** mode, Orca attaches to a
**running** Studio through OpenAB core and drives **Studio's** `oab-mcp` instance
using **Studio's** identity, with its operations visible in Studio's UI. No AWS
credentials provisioned on Orca; no inbound port (Studio dials out). This is the
model for "the operator sits at Studio, a remote agent assists through it." It is
gated on the reverse-MCP auth/scoping design (ADR §5).

Until then, use **(A)** for programmatic/headless operation, or **(0)** raw AWS.

---

## Laptop / local agent (summary)

- **Headless:** same as (A), but set `AWS_PROFILE=oab-fleet` (the laptop profile
  wired in the fleet-credential setup) instead of relying on a task role. Studio's
  **Copy MCP config** action emits this JSON ready to paste.
- **Reverse MCP:** a local agent attaches to the local running Studio the same way
  a remote one does (once the serve mode ships) — co-located, but still through
  the same reverse-MCP path for uniformity.
