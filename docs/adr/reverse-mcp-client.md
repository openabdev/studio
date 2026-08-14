# ADR: Studio as a reverse MCP-over-ACP client (the Part B connection model)

- **Status:** Proposed
- **Date:** 2026-08-14
- **Author:** Orca (`ecs-claude`)
- **Builds on:** [Fleet grouping + Studio connection model](./fleet-grouping-and-connection-model.md)
  — this ADR realizes its **Part B, path (i)** and resolves its §5 open questions
  Q1 (write permission) and Q2 (fleet ↔ session mapping).
- **Upstream (accepted + as-built in `openabdev/openab`):**
  [Reverse MCP-over-ACP over WebSocket](https://github.com/openabdev/openab/blob/main/docs/adr/acp-server-websocket-reverse-mcp.md)
  (#1447) and its client wire contract
  [MCP-over-ACP tunnel contract](https://github.com/openabdev/openab/blob/main/docs/mcp-over-acp-tunnel-contract.md).

---

## 1. Context

The fleet ADR (§3) named two first-class ways an agent reaches the fleet:
(i) **reverse MCP** — attach to a *running* Studio; (ii) **headless standalone
`oab-mcp`** — no Studio. Path (ii) needs no new Studio code (provision the stdio
binary, done). Path (i) is unbuilt: today Studio spawns `oab-mcp` as a **stdio
subprocess** (`rmcp`, `transport-io`) on a private pipe — nothing external can
attach.

The driving consumer is concrete: **Orca**, the always-on ECS agent, should drive
Brett's fleet **through Brett's running Studio** — using **Studio's** identity and
credentials, with every fleet operation visible in Studio's UI — instead of
carrying its own AWS credentials (which re-introduces the silent
credential-fallback class of bug the fleet work exists to kill).

The upstream mechanism that makes this possible is **accepted and as-built**: any
ACP WebSocket client may declare one or more `type:acp` MCP servers, and OpenAB
core proxies their tools to the in-pod agent. This ADR decides **what Studio
implements as that client**, and names the **one** thing that is genuinely an
OpenAB-side dependency.

## 2. Roles & topology

Reverse MCP inverts the usual "who listens" intuition: the party that **cannot be
dialled** (behind NAT, no inbound port) is the one that **serves** tools, over the
outbound WebSocket it opened.

| Party | Role | Where |
|---|---|---|
| **Studio** (Brett's laptop) | **ACP WS client + MCP server** — dials out, serves `oab-mcp` tools over the socket it holds | laptop (NAT'd) |
| **OpenAB core / gateway** | ACP server + **MCP proxy/aggregator** (the OAB MCP Facade); spawns the agent for the session Studio creates | the agent runtime |
| **the agent** (Orca) | **MCP client** — discovers & calls Studio's tools via the facade meta-tools, executing under **Studio's** identity | ECS Fargate |

- **Studio dials out and is the session creator (decision, 2026-08-14).** Studio,
  the ACP client, opens the `/acp` WSS to the **remote agent runtime** (Orca's
  OpenAB endpoint) and **creates the session** with `session/new`. The gateway
  spawns the agent for that session — the **katashiro model** (§5, "Model A"): the
  connection that serves the tools is the same one that owns the session, so its
  `oab-mcp` tools reach *that session's* agent by construction. This is a
  **dedicated fleet-management session**, distinct from Orca's Discord chat
  session; a human later drives it through a chat surface in Studio (deferred —
  §7). It solves NAT (nothing connects *to* Studio) and is reachable only while
  Studio runs (path (ii) covers the no-Studio case).
- **"Orca attaches to a running Studio"** (fleet ADR §3(i)) is the *logical*
  reading — the agent gains Studio's tools. The *physical* dial is
  **Studio → the agent runtime's gateway**, Studio creating the session.
- **Studio's identity executes.** `oab-mcp` runs in Studio with Studio's
  `fleets.toml` / AWS credentials (the fleet work of Part A). The agent calls the
  tools; Studio performs the AWS actions. No AWS credentials are provisioned on
  the agent.

```
  Orca (agent, ECS) ──in-pod──▶ OpenAB core (OAB MCP Facade)
                                      ▲
                                      │  /acp WSS (outbound from Studio)
                                      │  MCP-over-ACP, multiplexed with ACP chat
                                      │
                                 Studio (laptop) = MCP server: oab-mcp tools
                                      │
                                      ▼  Studio's fleets.toml + AWS creds
                                 AWS (ECS: the fleet)
```

## 3. What Studio reuses from OpenAB (do not rebuild)

The upstream side is settled; Studio codes against it, it does not re-invent it:

- **Gateway `/acp` WS + `AcpTunnelRegistry`**, keyed `(channel_id, serverId)`.
  Reverse MCP-over-ACP is **generic** (#1447): a client declares **N** `type:acp`
  servers on `session/new` (re-declared on `session/resume`), each with `{id,
  name}`.
- **The OAB MCP Facade + `AcpTunnelSource`** exposes those servers to the agent
  through the two meta-tools `search_capabilities` / `execute_capability`.
  Discovery is **pull-based, gateway-initiated**, cached per `(channel_id, name)`.
- **Trust = the `/acp` transport auth alone** (D-29 removed the per-tool
  allowlist). Admission over the authed transport *is* the grant; a connected
  server publishes every tool it declares.
- **The wire contract is fully specified** — `initialize` handshake, `mcp/connect`
  → `connectionId`, `mcp/message` framing (flattened inner MCP, correlate by the
  **outer** ACP frame id — there is **no** inner MCP id), `mcp/disconnect`,
  `mcp/cancel`, and the limits table.

Studio's job is to be a correct **client** of that contract — the role the
browser extension plays in the upstream example, with `oab-mcp` in place of the
DOM tools.

## 4. Decision — Studio's reverse-MCP client

### 4.1 An ACP-WS client component (in `src-tauri`)

A background task (Tauri side, process-lifetime, decoupled from any UI) that:

- **Dials** the gateway `GET /acp` WSS. Bearer auth rides the WebSocket
  sub-protocol offer `openab.bearer.<token>, acp.v1`; the server echoes `acp.v1`.
  All frames are JSON-RPC 2.0.
- **Drives the session:** `initialize` (read back `agentCapabilities`; the gateway
  advertises reverse-MCP support as the `_meta` key `dev.openab/acp: true`), then
  `session/new` declaring the server (§4.4). On reconnect, `session/resume`
  **re-declaring the whole set** — a resume withdraws any `type:acp` server it does
  not re-present.
- **Reconnects** with backoff; the `serverId` (`id`) is a fresh UUID **per
  connection** while the `name` is stable (the registry keys on `id`, routing
  resolves by `name`; upstream §6.1). New WS deps for Studio:
  `tokio-tungstenite` + `rustls`.

### 4.2 Tunnel dispatch — answer the gateway's MCP requests

The tunnel is **gateway-initiated**: the gateway asks, Studio answers. Studio
handles, on each `mcp/connect` `connectionId`:

- `mcp/connect { acpId }` → reply a fresh `connectionId`.
- `mcp/message { connectionId, method, params }` (**request**, has outer `id`):
  dispatch the inner MCP method to the in-process `oab-mcp` handler and reply the
  inner MCP **result** as the ACP response `result`; an inner MCP error → the outer
  JSON-RPC `error`. Inner methods handled as an MCP **server**: `initialize` →
  `InitializeResult`; `notifications/initialized` (notification, no reply) →
  forward; `tools/list` → the `oab-mcp` tools; `tools/call` → execute, return an
  MCP `CallToolResult`.
- `mcp/disconnect { connectionId }` → release, reply `{}`.
- `mcp/cancel { requestId }` (notification; `requestId` = the **outer** frame id) →
  abort that in-flight call.

Correlation is **always** by the outer ACP frame id. **Limits** (from the
contract) Studio must respect:

| Limit | Value |
|---|---|
| Tunnel request timeout | `170s` default (`180s` ACP idle ceiling) |
| Connect / handshake timeout | `30s` |
| `type:acp` servers per session | `8` |
| Any inbound frame | `8 MiB` (exceed → connection closed) |
| A method-bearing frame (request/notification) | `1 MiB` |

### 4.3 `oab-mcp` becomes in-process callable

Today `oab-mcp` is only reachable over stdio (`rmcp` `transport-io`). The tunnel
dispatch needs to invoke the **same** tool logic in-process. Refactor `oab-mcp` so
its `ServerHandler` (the `initialize` / `tools/list` / `tools/call` surface) is
callable directly, and let **both** transports sit on top of it:

- **path (i)** — the reverse-MCP tunnel dispatch (this ADR);
- **path (ii)** — the standalone stdio binary (`transport-io`), unchanged for the
  headless case.

One tool implementation, two front doors. No tool logic is duplicated or forked.

### 4.4 D1 — one `oab` server, fleet-parameterized tools (resolves fleet ADR §5 Q2)

Studio declares a **single** `type:acp` server (`name: "oab"`), not one per fleet.
A single server suffices for **any number of fleets** **because the tools are
fleet-aware**: they take a `fleet` (name) argument and resolve members/credential
through the Part-A binding (`FleetBindings::get(name)` / `members`).

This matters precisely because **fleets can share a cluster** (`orca` and `mira`
both on `oab`): a `cluster` argument alone cannot name a fleet, so the tool surface
must accept a `fleet`. `fleet_config` already enumerates every fleet, so Orca
discovers them and passes the right `fleet` per call.

- **Rejected — one `type:acp` server per fleet.** Its only advantage is
  tunnel-layer isolation (a session pinned to one fleet). That is not a boundary we
  need: fleets share Studio's identity/credentials (credential is
  cluster/account-granular), so isolation here is ergonomic, not security — and the
  `fleet` argument already provides it. The cost is real: N tunnels/declarations,
  the 8-servers/session ceiling, same-name rank/evict handling, and re-declaring
  all on every resume. **Deferred** to if/when fleets carry **distinct
  credentials/accounts** (fleet ADR §5 Q4, cross-account) or a session must be
  hard-restricted to one fleet.
- **Consequence — a tool-surface evolution.** The tools take `cluster` today, not
  `fleet`. Making them `fleet`-aware (resolve by name → members + credential) is
  **implementation slice 1** below. It is also the completion of Part A on the MCP
  side: it makes fleet-scoped operation real beyond the console.

### 4.5 D2 — writes are granted by the authed connection (resolves fleet ADR §5 Q1)

`deploy_apply` / `deploy_scale` / `deploy_delete` are auto-approved, like reads.
**The authed `/acp` connection is the grant** — consistent with upstream D-29
("the `/acp` transport is the gate") and OpenAB core's D1 (it auto-approves
`session/request_permission`). A client that holds a valid bearer to Studio's
gateway is, by construction, one Brett trusts to operate his fleet; there is no
second per-call consent gate, and the ADR does **not** add one. (A future
fine-grained consent surface is possible but explicitly out of scope — it would be
a change to the upstream contract's §8, not a Studio-local feature.)

### 4.6 D3 — no server-initiated inbound MCP

Studio serves a **gateway-initiated** tunnel only: it answers `initialize` /
`tools/list` / `tools/call` and does not push. `oab-mcp`'s tool set is effectively
**static**, so `notifications/tools/list_changed` has no consumer, and
server-originated requests (`sampling`/`elicitation`/`roots`) are not needed. This
matches the upstream contract and the 2026-07-28 MCP spec direction (those
surfaces are on a deprecation offramp). If a dynamic tool set ever appears, adopt
the upstream mechanism then rather than shipping a deprecated form now.

## 5. Session model — Model A (Studio creates the session)

There are two ways Studio's tools could reach an agent. **Decision (2026-08-14):
Model A.**

**Model A — Studio creates a dedicated fleet-management session (chosen).** Studio,
as the ACP client, dials the remote agent runtime and **creates the session**; the
gateway spawns the agent for it. This is exactly the **katashiro pattern**: the
connection that serves the tools also owns the session, so `session/new`'s declared
`oab-mcp` server reaches *that session's* agent **by construction**. No
"attach to a session you did not create" mechanism is required — the mechanism is
already as-built upstream (#1447). A human later drives this session through a chat
surface in Studio, to *work with the agent* on fleet management (that chat is
deferred — §7).

Because Model A reuses the settled path, the only OpenAB-side needs are **ordinary
ACP-client concerns**, not a new capability:

1. **Endpoint.** Which agent-runtime gateway Studio dials (Orca's OpenAB endpoint,
   reachable from the laptop) — topology/deployment, not protocol.
2. **Bearer.** How Studio obtains the `/acp` transport bearer — the settled
   transport auth, nothing new.

**Model B — inject tools into an agent's *already-running* session (deferred).**
Making the always-on, Discord-driven Orca gain `oab-mcp` tools *in its existing
chat session* would require a third party (Studio) to contribute a `type:acp`
server into a session it did **not** create — keyed by that session's `channel_id`,
with a bearer scoping the attacher to it. Upstream has `(channel_id, serverId)`
keying and `session/resume` (a session's **own** client reconnecting), but **not**
an admission path for a non-session-driving attacher. That is a genuine new
OpenAB mechanism; it is **out of scope** here and revisited only if operating the
live Discord Orca in-place proves necessary. Model A already delivers
"an agent operates the fleet through Studio."

**Path (ii)** (headless standalone `oab-mcp`) remains the no-Studio fallback and
depends on none of this.

## 6. Consequences & implementation slices

- Studio gains an **outbound network client** and answers gateway-initiated
  requests — a new posture for the desktop app (previously it only *hosted* a stdio
  sidecar). New deps: `tokio-tungstenite`, `rustls`.
- The **`fleet`-aware tool surface** (slice 1) is independently useful: it makes
  MCP-side fleet scoping real, and unblocks path (ii) fleet operation too.
- **`oab-mcp` handler reuse** keeps one tool implementation behind both transports.

**Slices** (each a PR; docs/backend before the network client):

1. **`oab-mcp` tools become `fleet`-aware** — accept `fleet` (name), resolve via
   `FleetBindings::get`/`members`; keep `cluster` as a compatible fallback. Backend
   + tests; no network. *(Also completes Part A on the MCP side.)*
2. **Extract the in-process `oab-mcp` handler** — one `ServerHandler`, the stdio
   binary re-expressed on top of it. Pure refactor, behaviour-preserving.
3. **ACP-WS client + session lifecycle** — dial, bearer sub-protocol,
   `initialize`/`session/new`/`resume`, reconnect. Gated behind config; no tunnel
   dispatch yet.
4. **Tunnel dispatch** — `mcp/connect` / `mcp/message` / `mcp/disconnect` /
   `mcp/cancel` wired to the in-process handler; limits + id correlation. End-to-end
   against a real agent-runtime gateway (§5, Model A).

Slices 1–2 need **no** OpenAB dependency and can land immediately. Slices 3–4 need
only a reachable agent-runtime endpoint + bearer (§5) — ordinary ACP-client
concerns, not a new upstream mechanism.

## 7. Open questions

1. **Endpoint + bearer (§5)** — which agent-runtime gateway Studio dials and how it
   gets the transport bearer. Deployment/topology, not protocol.
2. **The session chat surface** — Model A's session is driven by a human working
   *with* the agent on fleet management; Studio needs a chat UI for that session.
   Deferred; the mechanism (an ACP chat over the same `/acp` WS) exists.
3. **Per-fleet servers / cross-account (§4.4, fleet ADR §5 Q4)** — revisit if a
   fleet ever carries distinct credentials or a session must be pinned to one fleet.
4. **Fine-grained write consent (§4.5)** — deliberately deferred; would be an
   upstream contract change, not Studio-local.
5. **Model B (inject into a live session)** — only if operating the always-on
   Discord Orca in-place is later required; needs the new upstream admission path
   described in §5.
