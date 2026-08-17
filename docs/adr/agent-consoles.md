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

## 5. Decision — Part D: the remote file editor (Studio-brokered fs — exec-backed for owned runtimes, MCP-server for federation)

Studio needs to **list / read / write** files on a remote agent's filesystem. **Decision: fs is a Studio-brokered capability with two interchangeable backends behind one surface. The default backend for a runtime Studio controls is the platform's exec channel (ECS `ExecuteCommand`, k8s `pods/exec`); the fallback for a runtime Studio does *not* control (a foreign agent reachable only over `/acp`) is an fs MCP server the target agent exposes.** The `oab` tool + the UI source are identical across both.

Two things are fixed regardless of backend; only the backend varies:

- **Topology — Studio-brokered (unchanged, decided).** The management agent never holds a target's exec credential or bearer. Its fs call arrives as a Studio reverse-MCP `oab` tool; **Studio** performs the operation with the credential *it* holds. **Credentials and policy stay in Studio; the `oab` grant stays management-only** (least privilege, Part A). The rejected alternative — handing the agent the exec cred / each target's bearer directly — concentrates cluster-grade RCE in an LLM and breaks least privilege.
- **Actor — unchanged.** fs is not only Studio's UI reading a disk; the driver is the **management agent reaching into a target to manage it**, editing a file being one op. Both consumers (the agent as an `oab` tool, the UI as the read-only browser) hit the same Studio surface — one fs policy point.

**The deciding axis is: does Studio own the target's runtime?**

- **Backend 1 — Studio platform-exec (primary; every agent in the fleet today).** Studio reaches the target's filesystem via the orchestrator's exec channel — ECS `ExecuteCommand` (SSM), k8s `pods/exec` — using a control-plane credential Studio already holds (it is the plane that scales these services). **Ships now; no openab dependency.** This covers 100% of the current fleet (all agents run on our own ECS/k8s).
- **Backend 2 — target-hosted fs MCP server (fallback; un-owned runtimes / federation).** For an agent Studio can reach only over `/acp` but whose platform it does **not** control, fs rides a small files server the agent exposes (`list`/`read`/`write`/`stat`), relayed through the `oab` tool over the token in `agents.toml`. This is the previously-decided MCP mechanism; it **depends on openab** and is **deferred until such an agent exists** — exec cannot reach a platform Studio doesn't own, so this is the escape hatch, not the day-1 path.

- **Editable-root scoping — same intent, different enforcement locus.** `writable` defaults **off**; no `/`-wide default; browsing/writes are bounded to declared root(s). **Enforcement locus differs by backend and this is the material trade the exec backend buys:** the MCP server enforces roots *at the resource* (the agent declares roots, the server refuses outside them); **exec has no OS root-fence, so with the exec backend `roots`/`writable` become Studio-side path validation + safe argv-form command construction — advisory, not resource-enforced.** A Studio path-validation bug is therefore a full-container write. Mitigations: hard path canonicalization + prefix checks, argv-form exec (never shell-string interpolation), `writable` default-off, explicit Apply. We accept this for owned runtimes because Studio is *already* the control plane holding a fleet-management credential — exec is the same trust tier, not a new principal — and the agent still never receives that credential.
- **Apply semantics (unchanged).** `write` persists the file. Some files hot-reload (the agent watches); others need a restart. Studio's **Apply** = write; for restart-required files, offer an explicit **"restart instance"** action reusing the control-plane scale-cycle (scale→0→1, ADR-2). Studio does not guess reload semantics — it writes and, on request, cycles.

## 6. Scope

- ✅ **In (this ADR's target):** the two-console architecture; the per-agent endpoint registry (`agents.toml`, backward-compatible with `remote.toml`); the agent console shell (selector + config region + chat region); per-endpoint chat via the existing primitive; reverse-MCP publication gated to the management binding.
- ✅ **In, once the exec provider lands:** the remote file editor's read *and* write paths for **owned runtimes** — over Studio's platform-exec backend (ECS/k8s), consumed by the UI and the `oab` fs tool (Part D). No openab dependency; covers the whole current fleet.
- ⛔ **Blocked on openab (upstream), unchanged from agent-chat-panel:** token streaming, `tool_call`/thought rendering (only `agent_message_chunk` today). The **fs MCP files server** on the target agent (Part D, backend 2) is in this bucket but is now the **federation fallback**, not the gating dependency — deferred until a Studio-un-owned agent exists.
- 🚫 **Not a goal:** the multi-agent room (@mention routing, agent→agent relay, loop guard) — N independent consoles, no cross-agent surface; accessibility (single-operator internal console, per agent-chat-panel §5).

## 7. Consequences

- ✅ The operator can inspect **and reconfigure** any reachable agent, and converse with each — not just the one management agent.
- ✅ Reuses the chat primitive, the CodeMirror editor, and the reconnect/status machinery; the new weight is the registry, the per-endpoint keying of `RemoteState`, and the **`oab` fs tool** (Studio → exec backend, or → target fs MCP server for federation).
- ✅ **The fleet's fs write ships without openab.** The exec backend uses a credential Studio already holds, so the remote file editor is no longer gated on an upstream fs server; that upstream item drops from "gating dependency for write" to "federation fallback, deferred." fs and management still ride **one** brokered channel (agent → `oab` → target), one fs policy point.
- ⚠️ **Security surface grows materially.** (a) The registry holds **N bearer secrets**, not one — and they stay in Studio (brokered topology), not fanned out to the management agent. (b) Remote **file write into a live agent** is among the most powerful actions Studio can take — hence `writable` default-off, explicit Apply, no `/`-wide default. **With the exec backend this gating is Studio-enforced (path validation + argv-form exec), not resource-enforced** — a real downgrade from the MCP server's server-side roots, accepted for owned runtimes because Studio is already the control plane and the credential never reaches the agent. (c) The exec backend puts a **control-plane / orchestrator credential in the fs path** — the very credential the earlier draft kept fs away from; it is held **only by Studio's broker**, never granted to an agent or an agent console. (d) Reverse-MCP fleet-control tools — including the fs tool — stay **least-privilege**, published only to the management binding. (e) Agent output rendered in chat keeps the mandatory DOMPurify sink + Tauri CSP (agent-chat-panel §6).
- ⚠️ `RemoteState` is no longer a singleton — it becomes a per-agent map of connections, each with its own status/reconnect. Bounded fan-out (operator opens a console at a time), but the lifecycle (open→dial, close→teardown) must be clean to avoid leaked sockets.
- ⚠️ The **new implementation weight is Studio's exec provider** — per-platform backends (ECS `ExecuteCommand` over SSM, k8s `pods/exec`), safe argv construction, and `ls`/`stat`/`cat`/`tee`-style op mapping with binary/large-file handling. The MCP-server backend (federation) reuses the `oab` relay and lands only when a Studio-un-owned agent needs it.

## 8. Open questions

1. ~~**`fs/*` wire** — native ACP methods or an agent-served MCP files server?~~ ~~Resolved: MCP files server, Studio-brokered.~~ **Re-resolved: Studio-brokered fs with two backends — platform-exec (primary, owned runtimes, ships now) + target-hosted fs MCP server (fallback, federation, deferred) (Part D).** Remaining sub-questions: the exact `oab` fs tool set/params (one `fs.*` family fanning out by target name, vs. per-op tools) — shared by both backends; and, for the federation backend, whether the target *runtime* ships the MCP server or Studio injects it. **No longer the gating dependency for write** — exec ships it.
2. **Enforcement locus for the exec backend** — with exec there is no server-side roots capability, so `roots`/`writable` are enforced by Studio: path canonicalization + prefix checks + argv-form exec. Open: how strict (a fixed allow-list of roots per agent in `agents.toml`? a `chroot`/`--workdir` confinement where the platform allows it?), and whether to surface roots to the UI up-front (grey out non-editable paths) or only enforce on write. For the federation (MCP) backend this is the server's **roots capability**, resource-enforced.
3. ~~**Registry vs `remote.toml`**~~ **Resolved & shipped:** new `agents.toml` with a `remote.toml` back-compat shim (slice 1, #66); the in-app editor now targets `agents.toml` (#69).
4. **Endpoint discovery** — purely manual (operator pastes url+token per agent) for now; could later be derived from the roster/control-plane if agents publish their `/acp` address. Out of scope here.

This is a direction-alignment ADR. Implementation lands in slices, each verified against the live gateway: (1) endpoint registry + `RemoteState` keying; (2) agent console shell + per-endpoint chat (reuses the Part C primitive); (3) remote file **read path** — the browser UI over a source-agnostic read, shipped read-only against fixtures (#68; the bespoke `fs/*` client explored in the slice-3 draft was dropped per Part D); (4) **read+write/apply over the exec backend** — Studio's platform-exec provider (ECS/k8s) behind the `oab` fs tool, consumed by both the UI and the management agent; lights up slice 3's read on real owned endpoints and adds write/Apply. No openab dependency. (5, when needed) **federation backend** — the target-hosted fs MCP server over the `oab` relay, for a Studio-un-owned agent.
