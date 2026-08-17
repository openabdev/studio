# ADR: Two consoles — a management console and per-agent agent consoles (endpoint registry + remote file editor)

- **Status:** Accepted (2026-08-16 — implementation started, slices 1–4)
- **Date:** 2026-08-14
- **Author:** Orca (`ecs-claude`)
- **Extends:** [Chatting with a connected agent — `session/prompt` over `/acp` (Part C)](./agent-chat-panel.md); [Fleet grouping & connection model](./fleet-grouping-and-connection-model.md); [Deployment control plane (ADR-2)](./deployment-control-plane.md)
- **Reference:** [`brettchien/katashiro`](https://github.com/brettchien/katashiro) — working ACP chat client on the same `/acp` gateway; [MCP-over-ACP tunnel contract](https://github.com/openabdev/openab/blob/main/docs/mcp-over-acp-tunnel-contract.md)

---

> The [agent-chat-panel ADR](./agent-chat-panel.md) added a chat surface to Studio's **one** bound agent (single-agent, by design). This ADR steps up a level and fixes the **architecture**: Studio has **two kinds of console**. The **management console** (the current top-level view) chats with **one designated agent** that — via reverse-MCP — can *drive the fleet*. An **agent console** is a **per-agent** view where the operator can **view/edit that agent's files and apply them**, and **chat with it directly**. The chat panel from the agent-chat-panel ADR becomes a **reusable primitive** instantiated per endpoint; this ADR adds the **endpoint registry** that makes N agents reachable and the **remote file editor** that makes "view/edit config" real.

## 1. Context

Three things are now settled or in flight:

- The **management console** exists: the top-level roster + a single `/acp` connection (`remote.toml`) whose chat talks to the bound agent, and whose reverse-MCP tunnel publishes Studio's `oab` tools **to** that agent so it can control the fleet (Part B, #43/#48; chat panel Part C in progress).
- That model is **deliberately single-agent** (agent-chat-panel §4). Brett now wants the complementary surface: **one console per agent**, each able to **inspect/edit that agent's config and chat with it directly** — N independent single-agent consoles, **not** a multi-agent room (no @mention/relay/loop-guard; that remains a non-goal).
- The agent-chat-panel ADR is **still under development** (Part C not landed). Rather than churn an in-flight ADR, this new ADR captures the two-console architecture and the two new mechanisms it needs (endpoint registry, remote file editor). The chat *primitive* it defines is reused unchanged.

Operator decisions locked in this thread (Brett, 2026-08-14):

1. **Reachability is the operator's job.** *Any* openab agent can start an `/acp` endpoint; making it reachable is a deployment concern the operator handles (expose the k8s/orbstack or docker-compose port for a local agent; an ingress/DNS for a remote one). Studio only needs, per agent, a `(url, token, cwd)` — it does not provision the endpoint.
2. **"Config" = files.** Instead of modelling "runtime spec" vs "persona files" separately, Studio ships **one remote file editor**: browse/edit the agent's files on its remote filesystem and **apply**. Both runtime config and persona (`CLAUDE.md`, `agent_profiling/…`) are just files.
3. **The management agent is not special infrastructure.** It is simply the agent bound as the management endpoint; it **may** be a member of a managed fleet, and it is **not required** to be managed by Studio. Any agent (including it) can also have an agent console.

## 2. Decision — Part A: two consoles, one chat primitive

| | **Management console** | **Agent console (per agent)** |
|---|---|---|
| Scope | one designated agent | any agent with an endpoint |
| Chat | ✅ (agent-chat-panel primitive) | ✅ (same primitive) |
| Reverse-MCP `oab` tools published to the agent | ✅ — **the point**: the agent drives the fleet | ⛔ **off by default** (least privilege) |
| Remote file editor | — (not its job) | ✅ view/edit/apply the agent's files |
| Roster / fleet control UI | ✅ | — |

- **The chat panel is one component**, instantiated against a chosen endpoint. The management console instantiates it against the management binding; an agent console instantiates it against that agent's binding. Rendering, markdown sink, turn controls, autoscroll, history/resume — all from agent-chat-panel §4, unchanged.
- **Reverse-MCP publication is a per-binding capability, not automatic.** Handing an agent Studio's `oab` fleet-control tools is a grant; it stays a property of the **management** binding. An ordinary agent console chats with and configures an agent **without** giving it fleet control. (An agent console *may* opt into publishing `oab` via an explicit binding flag, but that is off by default.)

## 3. Decision — Part B: per-agent endpoint registry

Generalize the single `remote.toml` into a **registry of agent endpoints**. Each entry is the same shape as today's `RemoteConfig` plus an identity and capability flags:

```toml
# ~/.config/oab-studio/agents.toml   (edited in-app, same editor pattern as fleets.toml)

[[agent]]
name        = "orca"
url         = "wss://orca-acp.brettchien.cc/acp"
token       = "…"          # /acp bearer — SECRET, never logged
cwd         = "/home/node"
management  = true          # this entry backs the management console (reverse-MCP oab published)

[[agent]]
name = "mira"
url  = "wss://mira-acp.brettchien.cc/acp"
token = "…"
cwd  = "/home/node"
# management defaults to false → agent console only, no oab tools published
```

- **`management` is a policy flag**, not a separate file: exactly one entry carries it (it backs the top-level console and its reverse-MCP grant). All entries (including the management one) are selectable as agent consoles.
- **Backward compatibility:** a legacy single `remote.toml` is read as one `management = true` entry, so existing setups keep working while the registry is adopted.
- **Schema lives in `crates/acp-tunnel/config.rs`** (extend `RemoteConfig` → an `AgentEndpoint` + an `AgentRegistry` wrapper); parsing/validation stay pure there. Each endpoint validates as today (WSS scheme, token present). Tokens remain secrets that surface in the editor but are never logged.
- **No auto-connect.** Endpoints are dialed on demand: the management console dials its binding on "Activate" (as today); an agent console dials its endpoint when opened, tears down when closed. Reconnect/status machinery (`remote-status` per connection) is reused per endpoint — `RemoteState` becomes keyed by agent name rather than a singleton.

## 4. Decision — Part C: the agent console

Selecting an agent (from the roster or the registry) opens its console with two regions:

1. **Files / config** — a **remote file editor** (Part D) over that agent's filesystem: a tree/list scoped to an editable root, open a file into the existing CodeMirror editor, **Apply** writes it back over the endpoint. This is the concrete meaning of "view/edit each agent's config and apply."
2. **Chat** — the agent-chat-panel primitive bound to this agent's endpoint (single-shot reply behind a think-spinner today, markdown-at-finalize, stop/retry, batched queue, history + `session/resume`).

The management console keeps its current shape (roster + fleet control + its own chat); an agent console is a **focused, single-agent** view reached by selection. Both share the tab chrome; the agent console is a new `<section>` + tab in the vanilla-TS `console` (render + wiring in `render.ts`/`main.ts`), keyed by the selected agent.

## 5. Decision — Part D: the remote file editor (an MCP files server, management-plane)

Studio needs to **list / read / write** files on a remote agent's filesystem. **Decision: fs is an MCP surface — a files server the target agent exposes — not a bespoke `fs/*` method set on `/acp`.** (This resolves Part D's original open question in favour of the agent-served MCP variant.)

The reason is the *actor*. fs is not only Studio's UI reading a disk; the real driver is the **management agent reaching into a target agent to manage it**, of which editing a file is one operation. That is the ordinary MCP direction — an agent consuming tools — and the management→target control channel has to exist regardless. A bespoke `fs/*` RPC would build that channel a second time, just for files. (An earlier draft preferred bespoke `fs/*` by scoping fs as *Studio-reads-agent*, which is the reverse MCP direction; once the actor is the management agent, that objection dissolves.)

- **Mechanism — an fs MCP server on the target agent.** Each agent exposes a small files server (`list` / `read` / `write` / `stat` tools), roots-scoped (below). It is consumed by the **management agent** as tools, and by **Studio's UI** for the read-only browser — one fs surface, one policy point, no second wire. (Whether the target *runtime* ships this server or it is injected is coordinated with openab, the way streaming/`tool_call` are; but the shape is now settled as MCP, not native ACP methods.)
- **Topology — Studio-brokered (decided).** The management agent does **not** hold every target's bearer. Its fs/management call arrives as a Studio reverse-MCP `oab` tool; Studio then dials the target's `/acp` with the token it already holds in `agents.toml` and relays to that agent's fs server. **Bearers and policy stay in Studio; the `oab` grant stays management-only** (least privilege, Part A). Two MCP hops — the management agent's MCP into Studio, Studio's relay into the target's MCP ("MCP in MCP"). The rejected alternative — the management agent connecting to each target directly — spreads N bearers out of Studio and breaks least privilege.
- **Editable-root scoping (agent-declared).** The agent advertises its editable root(s) via the files server's roots capability; Studio / `oab` never assume `/`. `writable` defaults **off**; writes outside the declared root are refused server-side. This bounds a very powerful capability (arbitrary write into a running agent = arbitrary persona/behaviour change) — now enforced as **tool-level gating in the fs server**, not in a bespoke wire.
- **Apply semantics.** `write` persists the file. Some files hot-reload (the agent watches); others need a restart to take effect. Studio's **Apply** = write; for restart-required files, offer an explicit **"restart instance"** action reusing the control-plane scale-cycle (scale→0→1, ADR-2). Studio does not guess reload semantics — it writes and, on request, cycles.

## 6. Scope

- ✅ **In (this ADR's target):** the two-console architecture; the per-agent endpoint registry (`agents.toml`, backward-compatible with `remote.toml`); the agent console shell (selector + config region + chat region); per-endpoint chat via the existing primitive; reverse-MCP publication gated to the management binding.
- ⚠️ **Sequenced after the fs server lands:** the remote file editor's write path — depends on the target agent's **fs MCP server** + Studio's `oab` relay tool (Part D). Ships read-only until then.
- ⛔ **Blocked on openab (upstream), unchanged from agent-chat-panel:** token streaming, `tool_call`/thought rendering (only `agent_message_chunk` today); the **fs MCP files server** on the target agent (Part D) is a new item in this bucket.
- 🚫 **Not a goal:** the multi-agent room (@mention routing, agent→agent relay, loop guard) — N independent consoles, no cross-agent surface; accessibility (single-operator internal console, per agent-chat-panel §5).

## 7. Consequences

- ✅ The operator can inspect **and reconfigure** any reachable agent, and converse with each — not just the one management agent.
- ✅ Reuses the chat primitive, the CodeMirror editor, and the reconnect/status machinery; the new weight is the registry, the per-endpoint keying of `RemoteState`, and the **`oab` fs-relay tool** (Studio → target fs MCP server).
- ✅ fs and management ride **one** channel (the management agent → `oab` → target), not two. Studio's file browser and the management agent consume the same fs server, so there is a single fs policy point instead of a bespoke wire duplicated per consumer.
- ⚠️ **Security surface grows materially.** (a) The registry holds **N bearer secrets**, not one — and they stay in Studio (brokered topology), not fanned out to the management agent. (b) Remote **file write into a live agent** is among the most powerful actions Studio can take — hence agent-declared editable roots, `writable` default-off, explicit Apply, and no `/`-wide default, enforced as tool-level gating in the fs server. (c) Reverse-MCP fleet-control tools — now including the fs relay — stay **least-privilege**, published only to the management binding, never to an arbitrary agent console. (d) Agent output rendered in chat keeps the mandatory DOMPurify sink + Tauri CSP (agent-chat-panel §6).
- ⚠️ `RemoteState` is no longer a singleton — it becomes a per-agent map of connections, each with its own status/reconnect. Bounded fan-out (operator opens a console at a time), but the lifecycle (open→dial, close→teardown) must be clean to avoid leaked sockets.
- ⚠️ Depends on openab for the target's **fs MCP server**; the write path can't ship until that exists. The architecture and read-only browser are independent and land first.

## 8. Open questions

1. ~~**`fs/*` wire** — native ACP methods or an agent-served MCP files server?~~ **Resolved: MCP files server, Studio-brokered (Part D).** Remaining sub-questions: the exact tool set/params of the fs server, and the shape of the `oab` fs-relay tool (one `fs.*` tool family fanning out by target name, vs. per-op tools). Coordinate with openab on whether the target *runtime* ships the server or Studio injects it. **Still the gating dependency for the write path.**
2. **Editable-root declaration** — the agent advertises its editable root(s) via the fs server's **roots capability** (standard MCP), so Studio / `oab` can't overreach. Open: whether roots are also surfaced to the UI up-front (to grey out non-editable paths) or only enforced on write.
3. ~~**Registry vs `remote.toml`**~~ **Resolved & shipped:** new `agents.toml` with a `remote.toml` back-compat shim (slice 1, #66); the in-app editor now targets `agents.toml` (#69).
4. **Endpoint discovery** — purely manual (operator pastes url+token per agent) for now; could later be derived from the roster/control-plane if agents publish their `/acp` address. Out of scope here.

This is a direction-alignment ADR. Implementation lands in slices, each verified against the live gateway: (1) endpoint registry + `RemoteState` keying; (2) agent console shell + per-endpoint chat (reuses the Part C primitive); (3) remote file **read path** — the browser UI over an MCP-backed read (source-agnostic; the bespoke `fs/*` client explored in the slice-3 draft is replaced by the fs MCP server per Part D); (4) **write/apply path** — the target's fs MCP server + Studio's `oab` fs-relay tool, consumed by both the UI and the management agent.
