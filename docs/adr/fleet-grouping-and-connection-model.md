# ADR: Fleet as usage-based logical grouping + the Studio connection model

- **Status:** Proposed
- **Date:** 2026-08-14
- **Author:** Orca (`ecs-claude`)
- **Related:** [Deployment control plane (ADR-2)](./deployment-control-plane.md),
  [Runtime identity & context (per-Fleet managing identity)](./runtime-identity-context.md),
  and — in `openabdev/openab` — [Reverse MCP-over-ACP over WebSocket](https://github.com/openabdev/openab/blob/main/docs/adr/acp-server-websocket-reverse-mcp.md).

---

## 1. Context

Two gaps surfaced while wiring a real operator (Brett's laptop) to Studio:

1. **A "fleet" is currently a physical cluster.** The fleet-binding config
   (`fleets.toml`) is an array of `[[fleet]]` tables keyed by `cluster`
   (`FleetBindings::for_cluster`, first-match-wins). A fleet therefore *is* a
   `(cluster → managing credential)` mapping. Two agents in the same cluster —
   e.g. `oab-prod-orca` and `oab-prod-mira`, both services in cluster `oab` —
   cannot be presented or managed as separate fleets, because the only grouping
   axis is the cluster and they share it (and share one credential/account).

2. **There is no defined way for an *agent* to connect to Studio's `oab-mcp`.**
   Studio spawns `oab-mcp` as a **stdio subprocess** (a private pipe between the
   desktop shell and the sidecar); nothing else can attach to it. Operators asked
   how a local or remote agent could drive the fleet through Studio.

This ADR decides both: what a **fleet** is, and how an **agent connects**.

## 2. Decision — Part A: Fleet is a usage-based logical group

**A fleet is a named logical grouping of agents, defined by its members, not by
its cluster.** Grouping is decoupled from credential.

- **Grouping** is by *usage* (what the agents are for), chosen by the operator.
- **Credential** is a *consequence* of where the members physically live
  (cluster / account), not the grouping key. Multiple fleets may share a cluster
  and credential while remaining distinct fleets.

### Schema — `[fleet.<name>]`, explicit members

`fleets.toml` moves from `[[fleet]]` (array, `name` as a field) to
`[fleet.<name>]` (a map keyed by name). The name becomes the primary key —
unique, self-documenting, and the fleet's identity in the UI.

```toml
# ~/.config/oab-studio/fleets.toml

[fleet.orca]
members = ["oab-prod-orca"]        # explicit member list (Decision: option (a))
region  = "ap-east-2"
profile = "oab-fleet"
expected_principal = "arn:aws:iam::504190915686:user/oab-fleet-laptop"

[fleet.mira]
members = ["oab-prod-mira"]
region  = "ap-east-2"
profile = "oab-fleet"
expected_principal = "arn:aws:iam::504190915686:user/oab-fleet-laptop"
```

This lets `orca` and `mira` be **two distinct fleets that share the `oab`
cluster and one credential** — exactly the "group by usage, not by cluster" the
operator asked for.

- **Membership (a): explicit member list** — ship first. Full operator control,
  no external dependency.
- **Membership (b): tag selector** (e.g. `select = { usage = "prod" }` over ECS
  tags) — deferred follow-up; needs a tagging convention on agents so new agents
  auto-join a fleet.

### Credential resolution

A fleet's managing credential comes from its `profile`/`region` (as today), now
scoped to the fleet rather than a cluster. A member's cluster/account is derived
from its service (all members of a fleet are expected to be co-located in one
account for v1; a fleet spanning accounts is a follow-up). `runtime_context`
(ADR: per-Fleet managing identity) reports the effective principal **per fleet**.

## 3. Decision — Part B: two connection models, both first-class

An agent connects to the fleet in one of **two distinct ways** — not a primary +
fallback, but two capabilities for different scenarios:

### (i) Reverse MCP — attach to a *running* Studio

The single way for an agent (local **or** remote) to operate **through a running
Studio instance**. Studio becomes an ACP WebSocket **client** that serves its
`oab-mcp` tool surface over the **outbound `/acp` WS it already holds**; OpenAB
core proxies those tools to the agent (the mechanism is Accepted + as-built in
openab #1447, first used for browser control).

- Solves NAT / can't-listen (Studio dials out; no inbound port).
- The agent uses **Studio's** running `oab-mcp` instance and **Studio's
  identity/session** — no separate AWS credentials to provision on the agent
  (avoids re-introducing the silent-credential-fallback class of bug).
- **Human-in-the-loop:** the agent's fleet operations are visible in Studio's UI.
- Cost: Studio must implement the ACP-WS-client "serve" mode, and the fleet is
  only reachable while Studio is running.

### (ii) Headless standalone `oab-mcp` — no Studio

For **CI, scripts, and headless agents** that manage the fleet **without Studio
running**. The agent spawns its **own** `oab-mcp` (stdio) using its own
credentials. Studio provides a **"Copy MCP config"** action that emits a
ready-to-paste stdio MCP server spec:

```json
{
  "command": "<path to bundled oab-mcp>",
  "args": [],
  "env": {
    "OAB_CLUSTER": "oab",
    "OAB_FLEETS_CONFIG": "~/.config/oab-studio/fleets.toml",
    "AWS_PROFILE": "oab-fleet",
    "AWS_REGION": "ap-east-2"
  }
}
```

This is an **independent** instance (own creds, own process), deliberately not a
fallback for (i) — it serves the no-Studio case.

## 4. Consequences

- `fleets.toml` schema change (`[[fleet]]` → `[fleet.<name>]` + `members`); a
  parse/compat path or a one-time migration is needed.
- `studio-cp` `FleetBinding`/`FleetBindings` and the resolution seam move from
  `for_cluster` to fleet-by-name / member→fleet lookup; `oab-mcp` keys credential
  selection on the governing fleet, not the cluster.
- The **config panel** (ADR: per-Fleet managing identity, slices A+B+C) evolves:
  list fleets by name, roster filtered to a fleet's members, switch by fleet
  identity (not cluster). The `fleets.toml` editor already added in that work
  carries over.
- Studio gains an **ACP-WS-client serve mode** for (i), and a **Copy MCP config**
  action for (ii).

## 5. Open questions

1. **Reverse-MCP auth/scoping** — exposing fleet-control (incl. writes:
   `deploy_apply`/`scale`/`delete`) over a relay needs an auth token scoping
   *which* agent may attach and *what* it may do. Threat model TBD.
2. **Fleet ↔ reverse-MCP session mapping** — does an attached agent see all
   fleets, or is a session bound to one fleet?
3. **Membership (b)** — tag-selector grouping + the `usage` tagging convention.
4. **Cross-account fleets** — v1 assumes a fleet's members share one account;
   spanning accounts (multiple credentials in one fleet) is later work.
5. **Migration** — auto-convert existing `[[fleet]]` (cluster-keyed) files, or
   require a manual rewrite with a clear error.

This is a stub to align direction (both decisions are made); implementation lands
in slices, and the open questions are resolved as those slices are designed.
