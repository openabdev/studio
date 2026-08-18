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
- **Fleet detail** — that fleet's member agents (a fleet may be size-1 — a single agent — per fleet-grouping-and-connection-model.md's `members = ["oab-prod-orca"]` single-member example) with health/status, plus the entry points for Part C (deploy) actions scoped to this fleet.
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
┌─ Studio ── OAB · v0.1.0-nightly.202608190125 · b6b6492 ── cluster: oab ── ● polling ── [🌓 System] ── [Check for updates] ─┐
└───────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```
Unchanged from today's topbar (Part E) — `brand`+build stamp left, `cluster-label`/`poll-status` + `theme-btn` + `update-btn`/`update-dismiss` right. Spans every screen below.

### 7.2 Fleets (top level)

```
┌─ Studio ── OAB · v0.1.0-nightly.202608190125 · b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
├───────────────────────────────────────────┬────────────────────────────────────────────────────────────────┤
│ Fleets                        [+ New fleet]│  Studio 主 chat (常駐 — management console)                   │
│ ────────────────────────────────────────── │  ──────────────────────────────────────────                   │
│  oab-prod-orca      ●2/2 running    [⚙]     │  你: 幫我查一下 orca fleet 現在的狀態                            │
│  oab-prod-mira      ●1/1 running    [⚙]     │  Studio: oab-prod-orca 現在 2/2，都健康...                     │
│  support-bot        ○0/1 idle       [⚙]     │                                                                │
│                                              │  [                                        ] [Send]            │
└───────────────────────────────────────────┴────────────────────────────────────────────────────────────────┘
```
- Left column = the drill-down entry point (fleet-grouping-and-connection-model.md's fleet list, promoted from sidebar to primary screen). Each row: fleet name, member count + aggregate health, a `[⚙]` that opens the **Debug drawer** scoped to that fleet's Activity/MCP/Config.
- `[+ New fleet]` is the Part C deploy entry point — opens Compose with no fleet pre-selected (creating a new one).
- Right column = the persistent management chat (Part B) — present here and on every screen below, unchanged in content.

### 7.3 Fleet detail (members of one fleet)

```
┌─ Studio ── OAB · v0.1.0-nightly.202608190125 · b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
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
- `[⚙]` here opens the Debug drawer scoped to this fleet specifically (narrower than the global one from 7.2, if that distinction survives implementation — see 10.2).
- Selecting a member row drills into 7.4.

### 7.4 Agent console (one member, selected)

```
┌─ Studio ── OAB · v0.1.0-nightly.202608190125 · b6b6492 ── [🌓 System] ── [Check for updates] ────────────────────────────┐
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

### 7.5 Deploy (Compose) — two distinct flows, one shared engine

Both `[+ New fleet]` (7.2) and `[+ Add instance]` (7.3) end at the **same** compose→preview→deploy engine (agent-deployment-templates.md, unchanged) calling the **same** `deploy_provision` MCP tool. They differ in what happens *before* that (does a `[fleet.<name>]` block already exist?) and *after* it (how `fleets.toml` gets updated) — spelled out separately because that's exactly the part that's new.

`fleets.toml` membership is **not** a live MCP mutation today — the only write primitive is `fleet_config_write`, which persists the **whole file** from raw TOML text ("overwrites the operator's fleets.toml", per `oab-mcp`'s tool description). So both flows below compute the new/edited TOML client-side and call `fleet_config_write` with the full updated text — there is no `fleet_config_add_member` tool to reach for.

#### 7.5.1 `[+ New fleet]` — from the Fleets screen (7.2), no existing `[fleet.*]` block

```
Step 1 — fleet identity                       Step 2 — first instance (Compose, shared engine)
┌─ New fleet ───────────────[Cancel]┐          ┌─ support-fleet — first instance ────[Back]┐
│ Fleet name  [ support-fleet     ] │          │ Template ▾ golden-oab                     │
│ Region      [ ap-east-2        ▾] │  ──▶     │ Overlay  ▾ support-persona                │
│ Credential  [ oab-fleet (profile)▾]│          │ [Preview bundle]                          │
│ Principal   [ arn:aws:iam::…    ] │          │  image/digest/files preview…              │
│         [Next: first instance →]  │          │ Name [ support-bot-1 ]        [Deploy]    │
└─────────────────────────────────────┘          └───────────────────────────────────────────┘
                                                            │ Deploy
                                                            ▼
                                     1. deploy_provision → ECS registers task-def + service "support-bot-1"
                                     2. Studio appends a new block to the in-memory fleets.toml text:
                                          [fleet.support-fleet]
                                          members = ["support-bot-1"]
                                          region  = "ap-east-2"
                                          profile = "oab-fleet"
                                          expected_principal = "arn:aws:iam::…"
                                     3. fleet_config_write(text) → persists + hot-reloads
                                     4. lands on Fleet detail (7.3) for support-fleet,
                                        support-bot-1 shown in a transient "provisioning" state
                                        (reuses the existing scale-guard pending-map machinery)
```
Step 1 is **net-new UI** — nothing today collects region/profile/`expected_principal` for a fleet that doesn't exist yet; the `Principal` field is optional (per `fleet-grouping-and-connection-model.md`'s schema, `expected_principal` isn't required). Step 2 is the existing Compose form, unchanged, just seeded with no fleet context. If step 2 fails (deploy error), step 1's fleet identity is **not** written — `fleet_config_write` only fires after a successful `deploy_provision`, so a failed first instance never leaves an empty orphan fleet in `fleets.toml`.

#### 7.5.2 `[+ Add instance]` — from an existing Fleet detail (7.3), `[fleet.<name>]` already exists

```
┌─ oab-prod-orca — add instance ────────[Cancel]┐
│ Template ▾ golden-oab   Overlay ▾ orca-persona │
│ [Preview bundle]                               │
│  image/digest/files preview…                   │
│ Name [ agent-3 ]                    [Deploy]   │
└─────────────────────────────────────────────────┘
        │ Deploy
        ▼
1. deploy_provision → ECS registers task-def + service "agent-3"
2. Studio edits the existing [fleet.oab-prod-orca] block in-memory:
     members = ["agent-1", "agent-2", "agent-3"]   # appended, region/profile untouched
3. fleet_config_write(text) → persists + hot-reloads
4. lands back on Fleet detail (7.3), now 3 members, agent-3 "provisioning"
```
No fleet-identity step — region/profile/`expected_principal` are inherited from the fleet the operator already drilled into, so this flow is strictly shorter than 7.5.1: it's the existing Compose form with no new UI in front of it.

**Failure handling (both flows):** if `deploy_provision` fails, stop — no `fleet_config_write` call, `fleets.toml` is untouched, the operator sees the compose form's existing error surface (`deploy failed: …`, per today's `compose.ts`). `fleets.toml` is only ever mutated *after* a confirmed successful provision, never before or speculatively.

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
- ⚠️ **`fleets.toml` writes are whole-file overwrites** (`fleet_config_write` has no partial/append primitive) — both 7.5.1 and 7.5.2 read-modify-write the full text. Two deploys racing (two operators, or one operator double-clicking) can lose a concurrent edit; today's single-operator usage makes this low-risk but it's a real gap, not a hypothetical (see Open question 10.5).

## 10. Open questions

1. **Back-navigation** — breadcrumb (`Fleets / oab-prod-orca / agent-1`) vs a plain back button? Affects whether drill-down state is representable as a URL/deep-link later.
2. **Debug drawer shape** — a slide-over panel, a modal, or a persistent-but-collapsed rail? Not decided; the affordance and its contents (Activity/MCP/Config, unchanged) are decided, the container isn't.
3. ~~**"+ New fleet" vs "+ Add instance"** — one deploy entry point or two?~~ **Resolved by 7.5.1/7.5.2: two distinct flows**, sharing the same compose→preview→deploy engine — they differ in the fleet-identity step (7.5.1 has one, 7.5.2 doesn't) and in how `fleets.toml` is edited (new block vs appended member), not in the deploy mechanics.
4. **Fleets-list empty state** — first-run UX (no fleets configured yet) isn't addressed here; likely folds into the "+ New fleet" affordance being the obvious first action.
5. **`fleet_config_write` race safety** — read-modify-write on the whole file (see Consequences) has no CAS/version check today. Worth a guard (re-read + diff before write, or surfacing a conflict) before this ships multi-operator, but not blocking for the current single-operator (Brett) usage.

## 11. Implementation slices

Six slices, ordered by dependency (each ships independently, keeps `tsc`/vitest/`vite build` green, and gets its own visual pass — same bar as #80–83):

1. **Fleets screen (7.2)** — promote today's sidebar fleet-config panel (`renderFleetConfig`, already does the switch) into the primary top-level screen. Lowest risk: the underlying fleet-switch logic (`activeFleet`/`activeCluster`/`activeMembers`) is unchanged, only its placement and the "click a row to drill in" affordance are new.
2. **Fleet detail (7.3)** — split the flat, `activeMembers`-filtered roster into its own screen reached from slice 1, with the `← Fleets` breadcrumb and per-member health rows. No `[+ Add instance]`/`[⚙]` wiring yet (can render disabled/stubbed).
3. **Agent console wiring (7.4)** — relocate the *existing* agent console (files/config + per-agent chat, agent-consoles.md Part C, already built) so it's reached by drill-down from slice 2 instead of a flat `agents-wrap` list. Internals untouched — this is a reachability change, not new surface.
4. **Persistent management chat (Part B)** — confirm `chat-wrap` stays mounted, unchanged in content, across slices 1–3's drill-down; mostly layout/CSS, since the chat primitive and its connection are already always-on today (not tab-gated) — this slice formalizes placement, it doesn't add new plumbing.
5. **Deploy actions (7.5.1 + 7.5.2)** — the heaviest slice: `[+ New fleet]`'s net-new fleet-identity step, `[+ Add instance]`'s shortcut into the existing Compose engine, and the `fleet_config_write` read-modify-write wiring for both. Could split further into **5a** (`+ Add instance` — no new UI, just context-passing into existing Compose) and **5b** (`+ New fleet` — the new identity-step form) if a smaller unit is wanted; 5a has no dependency on 5b.
6. **Debug drawer (7.6)** — collapse `Activity`/`MCP`/`Config` from top-level tabs into the drawer component, wire the `[⚙]` affordances from slices 1–2. Independent of slice 5; can land before or after it.

Slice 5 (or 5a/5b) is the only one with real new backend-adjacent logic (the `fleets.toml` read-modify-write); 1–4 and 6 are UI relocation/reachability changes over unchanged underlying mechanisms.

This is a direction-alignment ADR — Parts A–E are the agreed shape; the slice order above is a recommendation, not a commitment — re-sequencing (e.g. shipping the Debug drawer before Fleet detail) doesn't break any dependency.
