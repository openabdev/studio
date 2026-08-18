# ADR: Console navigation — Fleets → Agent drill-down, persistent management chat, Debug drawer

- **Status:** Proposed
- **Date:** 2026-08-19
- **Author:** Orca (`ecs-claude`)
- **Related:** [Fleet grouping & connection model](./fleet-grouping-and-connection-model.md) (Fleets as the grouping/switch unit); [Two consoles — management console + agent consoles](./agent-consoles.md) (the two chat surfaces this ADR places); [Agent deployment — templates](./agent-deployment-templates.md) (the Compose flow this ADR relocates); [Chatting with a connected agent](./agent-chat-panel.md) (the reused chat primitive)
- **Prompted by:** [openabdev/studio#82](https://github.com/openabdev/studio/pull/82) — the Config tab rendering empty turned out to be a duplicate `id="config"` between an always-visible sidebar section and a tab pane. Symptom of a deeper problem: the console mixes two layout mechanisms (always-visible `<section>`s + tab-switched panes) with no journey structure tying them together.

---

> Brett (2026-08-18/19): people use Studio to **deploy agents**, **manage fleets**, and **1:1 chat with spawned agents** — and the console's current information architecture doesn't reflect that journey. This ADR redraws the console's top-level structure around those three jobs, using pieces that are already decided (Fleets as the grouping unit, the management-vs-agent console split) but were never assembled into one coherent screen flow.

## 1. Context & Problem

`console/index.html` today stacks two layout mechanisms with no shared model:

- **Always-visible `<section>`s**, sequential in `main.content`: `identity`, `remote`, the sidebar fleet-config panel (`renderFleetConfig`), the main `chat-wrap`, and `agents-wrap` (roster + agent console).
- **Tab-switched panes** (`#tabs .tab[data-target]` → `document.getElementById(target)`): `Activity` (raw log), `MCP · oab-mcp` (raw JSON-RPC), `Compose` (template/overlay authoring + deploy), `Config` (mcp target: cluster/profile/region).

Nothing distinguishes "this is part of the usage journey" from "this is implementation detail I need to debug" — they're all flat tabs or flat sections. Two decided-but-unassembled pieces make this worse rather than better if left as-is:

- **fleet-grouping-and-connection-model.md** already decided Fleets are the grouping/switch unit and that "the config panel... evolves: list fleets by name, roster filtered to a fleet's members, switch by fleet identity" — but today that's a small sidebar section, not the primary navigation.
- **agent-consoles.md** already decided there are **two** chat surfaces — the **management console** (chats with the one designated agent that, via reverse-MCP, drives the fleet) and **N agent consoles** (per-agent, no fleet-control grant, file editor + chat) — but both are wired as if they were peers of the tab panes, with no rule for which one persists across navigation.

The immediate bug (#82) was a naming collision; the deeper issue is that the console has no router — every screen's visibility is independent, ad hoc `hidden` toggling, so nothing prevents two unrelated things claiming the same id, and nothing expresses "this panel always stays mounted."

## 2. Decision — Part A: three-job structure replaces tabs

The console's main content area becomes a **drill-down**, not a tab strip:

```
Fleets (top level)  →  Fleet detail (members)  →  Agent console
```

- **Fleets** — the list from fleet-grouping-and-connection-model.md: fleets by name, not by cluster. Selecting one is the "switch" step already specced there.
- **Fleet detail** — that fleet's member agents (a fleet may be size-1 — a single agent — per [[openab-fleet-instance-unified]]) with health/status, plus the entry points for Part C (deploy) actions scoped to this fleet.
- **Agent console** — unchanged from agent-consoles.md Part C: files/config region + this agent's own chat, reached by selecting a member.

This is the **only** navigation model for the main content area. There is no separate top-level tab strip for it.

## 3. Decision — Part B: the management chat is persistent, not tab content

The **management console's** chat (agent-chat-panel.md's primitive, bound to the one `management = true` endpoint in `agents.toml`) is **not** a pane that gets hidden by navigation. It is mounted once, alongside the Fleets/Fleet-detail/Agent-console content area, and stays visible regardless of drill-down depth — because it is a conversation with **the designated management agent**, not with whichever fleet/agent happens to be selected.

- An **agent console's own chat** (Part C above) is scoped to that agent and only exists while that agent console is open — it is a second, independent conversation, not a replacement for the management chat.
- This formalizes what agent-consoles.md §2 already implied (management console has fleet-control chat; agent console has per-agent chat) into a layout rule: **one persists, one is contextual.**

## 4. Decision — Part C: Deploy is an action, not a tab

The Compose flow (agent-deployment-templates.md: author library → preview → `deploy_provision`) drops its permanent top-level tab. It is reached as an action from the Fleets/Fleet-detail screens (e.g. "+ New fleet" from Fleets, "+ Add instance" from a Fleet's member list) and opens the same compose → preview → deploy sequence, unchanged. Deploy is something you *do* from within the fleet you're looking at, not a standing destination.

## 5. Decision — Part D: Activity / MCP / Config collapse into a Debug drawer

`Activity` (raw log), `MCP · oab-mcp` (raw JSON-RPC), and `Config` (mcp target: cluster/profile/region) are **implementation-detail surfaces**, not part of the deploy/manage/chat journey. They collapse into a single secondary **Debug drawer**, reachable via one affordance (e.g. a `⚙` control in the persistent chrome) from anywhere, rather than living as peer-level tabs beside Fleets/Compose. Their internals (log-level toggle, JSON-RPC transcript, cluster/profile/region form) are unchanged — only their standing in the top-level navigation changes.

## 6. Decision — Part E: global chrome is unaffected

The top bar (`brand` + build stamp `v${APP_VERSION} · ${BUILD_SHA}`, `cluster-label`/`poll-status`, `theme-btn`, `update-btn`/`update-dismiss`) is app-level chrome, not tied to any fleet/agent context. It stays exactly where it is — persistent, spanning every screen — and is out of scope for this ADR.

## 7. Mockups — one per screen level

ASCII wireframes, walked through with Brett live. These pin down *placement*, not final visuals — colors/spacing/exact controls are implementation detail.

### 7.1 Global chrome (persistent on every screen)

```
┌─ Studio ── OAB · v0.9.3-nightly-b6b6492 ── cluster: oab ── ● polling ── [🌓 System] ── [Check for updates] ─┐
└───────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```
Unchanged from today's topbar (Part E) — `brand`+build stamp left, `cluster-label`/`poll-status` + `theme-btn` + `update-btn`/`update-dismiss` right. Spans every screen below.

### 7.2 Fleets (top level)

```
┌─ Studio ── OAB · v0.9.3-nightly-b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
├───────────────────────────────────────────┬────────────────────────────────────────────────────────────────┤
│ Fleets                        [+ New fleet]│  Studio 主 chat (常駐 — management console)                   │
│ ────────────────────────────────────────── │  ──────────────────────────────────────────                   │
│  oab-prod-orca      ●2/2 running    [⚙]     │  你: 幫我查一下 orca fleet 現在的狀態                            │
│  oab-prod-mira      ●1/1 running    [⚙]     │  Studio: oab-prod-orca 現在 2/2，都健康...                     │
│  kiro-5952          ○0/1 idle       [⚙]     │                                                                │
│                                              │  [                                        ] [Send]            │
└───────────────────────────────────────────┴────────────────────────────────────────────────────────────────┘
```
- Left column = the drill-down entry point (fleet-grouping-and-connection-model.md's fleet list, promoted from sidebar to primary screen). Each row: fleet name, member count + aggregate health, a `[⚙]` that opens the **Debug drawer** scoped to that fleet's Activity/MCP/Config.
- `[+ New fleet]` is the Part C deploy entry point — opens Compose with no fleet pre-selected (creating a new one).
- Right column = the persistent management chat (Part B) — present here and on every screen below, unchanged in content.

### 7.3 Fleet detail (members of one fleet)

```
┌─ Studio ── OAB · v0.9.3-nightly-b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
├───────────────────────────────────────────┬────────────────────────────────────────────────────────────────┤
│ ← Fleets  /  oab-prod-orca   [+ Add instance] [⚙]  │  Studio 主 chat (常駐)                                │
│ ─────────────────────────────────────────────────  │  ──────────────────────────                          │
│  agent-1   ● healthy   1 vCPU · 2 GB               │  你: ...                                              │
│  agent-2   ● healthy   1 vCPU · 2 GB               │  Studio: ...                                          │
│                                                     │                                                       │
└───────────────────────────────────────────┴────────────────────────────────────────────────────────────────┘
```
- `← Fleets` = breadcrumb back to 7.2 (Open question 10.1 — breadcrumb vs back button; shown here as a breadcrumb for concreteness).
- `[+ Add instance]` = Part C deploy entry point pre-scoped to *this* fleet (adds a member, vs 7.2's `[+ New fleet]`).
- `[⚙]` here opens the Debug drawer scoped to this fleet specifically (narrower than the global one from 7.2, if that distinction survives implementation — see 9.2).
- Selecting a member row drills into 7.4.

### 7.4 Agent console (one member, selected)

```
┌─ Studio ── OAB · v0.9.3-nightly-b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
├───────────────────┬─────────────────────────┬────────────────────────────────────────────────────────────┤
│ ← oab-prod-orca /  │ agent-1 chat            │  Studio 主 chat (常駐)                                     │
│   agent-1          │ ─────────────────────── │  ──────────────────────────                                │
│ ─────────────────  │ 你: 幫我重啟              │  你: ...                                                   │
│ Files               │ agent-1: 重啟中...        │  Studio: ...                                             │
│  src/               │                          │                                                            │
│  config.toml        │ [                ] [Send]│                                                           │
│  agent_profiling/   │                          │                                                            │
└───────────────────┴─────────────────────────┴────────────────────────────────────────────────────────────┘
```
Unchanged from agent-consoles.md Part C (files/config region + this agent's own chat) — this ADR only fixes *where* it sits (reached by drill-down from 7.3, not a tab) and confirms it renders **alongside**, not instead of, the persistent management chat on the far right. Three columns simultaneously visible: agent's files, agent's own chat, Studio's chat.

### 7.5 Deploy (Compose) — opened from `[+ New fleet]` or `[+ Add instance]`

```
┌─ Deploy ── new instance in oab-prod-orca ──────────────── [Cancel] ┐
│ Template ▾  golden-oab           Overlay ▾  orca-persona           │
│ [Preview bundle]                                                   │
│ ── composed bundle preview ──────────────────────────────────────  │
│  image  ghcr.io/openabdev/openab:0.9.0-claude                      │
│  digest sha256:ab12…                                               │
│  files  config.toml, agent_profiling/identity.md, ...              │
│ ── deploy ─────────────────────────────────────────────────────── │
│ Name  agent-3          Namespace  default          [Deploy]        │
└──────────────────────────────────────────────────────────────────┘
```
Same compose→preview→deploy sequence as today's Compose tab (agent-deployment-templates.md), unchanged internals — presented as an overlay/panel invoked from 7.2/7.3 instead of a permanent tab. When opened via `[+ Add instance]` from 7.3, `Namespace`/target fleet are pre-filled from context (Open question 10.3); via `[+ New fleet]` from 7.2 they're blank.

### 7.6 Debug drawer — opened from any `[⚙]`

```
                                                          ┌─ Debug: oab-prod-orca ── [Close] ─┐
                                                          │ [Activity] [MCP · oab-mcp] [Config]│
                                                          │ ──────────────────────────────────  │
                                                          │  INFO  dial ws://orca-acp...        │
                                                          │  INFO  handshake ok                 │
                                                          │  DEBUG keepalive ping sent           │
                                                          │                                       │
                                                          │              [INFO+ / DEBUG+]        │
                                                          └───────────────────────────────────────┘
```
Slides over the right edge (shown here as an overlay; container shape is Open question 10.2). Internally it keeps today's three sub-views as a small tab strip *within* the drawer — `Activity`'s log-level toggle, `MCP`'s raw JSON-RPC transcript, `Config`'s cluster/profile/region form — none of that changes, only that they're no longer top-level.

## 8. Non-goals

- **openab-pty is not part of Studio.** Brett explicitly rejected folding pty/Connect's deploy flow into Studio (2026-08-18/19): Connect remains a separate product with its own UI. This ADR's Fleets/Deploy/Debug structure applies only to OAB (`OABService`/`OABFleet`) agents.
- **No change to `oab-mcp`'s tool surface or the backend driver model.** This is purely the console's information architecture; `deploy_apply`/`scale`/`delete`/`provision` and the ECS/k8s driver seam are untouched.
- **No multi-agent room / cross-agent relay** (carried over from agent-consoles.md's non-goals) — Fleet detail lists members; it does not add @mention or agent-to-agent routing.

## 9. Consequences

- ✅ Fixes the *class* of bug behind #82: navigation becomes a single router-like state (which fleet/agent is selected) instead of N independently-toggled `hidden` panes keyed by ad hoc ids.
- ✅ Reuses everything already built: `RemoteState` is already keyed per-agent (agent-consoles.md §3), the compose→preview→deploy flow is unchanged, the chat primitive is unchanged — this ADR only changes *where* they're mounted and *when* they're visible.
- ⚠️ `main.ts`'s tab-switcher (`show(target)` toggling `hidden` by id) needs to become a small view-stack/router (current fleet id, current agent id, debug-drawer open/closed) — real refactor weight, concentrated in `main.ts`/`render.ts`, no backend change.
- ⚠️ Losing the tab strip means the Debug drawer's discoverability depends entirely on its one affordance being findable — worth a first pass with real users (n=1: Brett) before treating the drawer's shape as settled.
- ⚠️ "Deploy as an action from Fleets" needs the entry points (`+ New fleet`, `+ Add instance`) to carry enough context (target fleet, whether it's a new fleet or a new member of an existing one) into the existing compose form — a small wiring change, not a redesign of Compose itself.

## 10. Open questions

1. **Back-navigation** — breadcrumb (`Fleets / orca-fleet / agent-1`) vs a plain back button? Affects whether drill-down state is representable as a URL/deep-link later.
2. **Debug drawer shape** — a slide-over panel, a modal, or a persistent-but-collapsed rail? Not decided; the affordance and its contents (Activity/MCP/Config, unchanged) are decided, the container isn't.
3. **"+ New fleet" vs "+ Add instance"** — one deploy entry point with a mode switch, or two distinct actions? Leaning toward one Compose flow with the target (new fleet vs existing fleet's member list) pre-filled from where it was invoked.
4. **Fleets-list empty state** — first-run UX (no fleets configured yet) isn't addressed here; likely folds into the "+ New fleet" affordance being the obvious first action.

This is a direction-alignment ADR — Parts A–E are the agreed shape; implementation lands in slices against `main.ts`/`render.ts`/`index.html`, each slice keeping `tsc`/vitest/`vite build` green per the existing console verification bar.
