# ADR: Studio Desktop Console — a swappable skin over an MCP core

- **Status:** Proposed
- **Date:** 2026-08-09
- **Author:** @brettchien
- **Reviewers:** _pending_
- **Tracking issues:** builds on [deployment-control-plane](./deployment-control-plane.md) (ADR-2) and [agent-lifecycle](./agent-lifecycle.md) (ADR-1)

> **Y-statement.** In the context of giving humans a director's console over the
> control plane, facing the need to ship a cross-platform GUI now yet rewrite
> the skin natively (SwiftUI) on Apple platforms later — and to serve mobile
> where a device must not hold cloud credentials — we decided to make the
> **skin ↔ core boundary the existing MCP surface** (`oab-mcp`), served over
> **stdio locally and streamable-HTTP remotely**, with a **Tauri web skin** as
> the interim front-end and a thin Rust bridge, **accepting that the interim web
> UI is throwaway** when the native skins arrive and that **live updates start
> as polling**, not push.

---

## 1. Context & Problem

ADR-2 gave us a control plane — a generic read/write model over the vendored
`oabctl`, exposed to agents through an MCP surface (`oab-mcp`, six `deploy_*`
tools). "Agents do control, humans direct" needs the *humans direct* half: a
**director's console** that surfaces the live deployment roster and lets a human
approve/drive the same actions.

Constraints that shape the design:

- **Native endgame is Swift.** On macOS/iOS the long-term skin is SwiftUI, not a
  webview. Whatever we build now is an **interim** skin.
- **Mobile can't hold cloud creds and can't spawn subprocesses.** An iOS app
  must be a thin client to a **remote** core; it cannot run the AWS-touching
  control plane locally, nor speak to it over stdio.
- **The console is a dense, live dashboard.** Roster + per-instance 6-state +
  counters now; per-agent diff and wave/phase orchestration later.
- **We must not fork control logic.** The console is *another front-end*, not a
  second implementation of observe/apply.

## 2. Decision Drivers

- **One control surface for humans and agents** — the GUI drives the *same* MCP
  tools an agent does; no parallel command path that can diverge.
- **Skin is swappable** — web now, SwiftUI later, with **zero change to core**.
- **Credentials stay server-side** — the device authenticates to a core service;
  AWS keys never leave the cloud boundary.
- **Iteration speed for a visual dashboard** — reuse the mature web charting /
  table / layout ecosystem for the first console.
- **Reuse what exists** — `studio-cp` (read/write model) and `oab-mcp` (tools).

## 3. Decision

### 3.1 Three layers, boundary at MCP

```
 core     studio-cp (Rust)         read/write model over oabctl (ADR-2)
 service  oab-mcp   (Rust)         core exposed as MCP tools  ── the contract
            ├─ stdio                 local desktop (spawned child)
            └─ streamable-HTTP       remote / browser / mobile
 skin     ├─ web (Tauri desktop, or plain browser)   ← now
          └─ SwiftUI (macOS + iOS, native)           ← later
                 every skin — and every agent — is a client of the SAME MCP surface
```

The **skin ↔ core contract is MCP**, a language-neutral wire protocol. A web
skin (JS), a SwiftUI skin (Swift), and an agent all speak it. Swapping the skin
is swapping an MCP client; the core and service are untouched. This is the
crux — we deliberately **do not** bind the skin to Rust (Tauri commands / FFI)
as the primary contract, because Swift and remote/mobile clients cannot use a
Rust-internal boundary.

### 3.2 Transports

`oab-mcp` today serves **stdio**. We add a **streamable-HTTP** transport (rmcp
supports it; openab's facade already uses it) so the identical tool surface is
reachable remotely. Desktop uses stdio (or local HTTP); browser and iOS use
HTTP to a remote core that holds the AWS credentials.

### 3.3 Interim skin: Tauri

The first skin is a **web front-end wrapped by Tauri**. Rationale: the interim
UI is throwaway (native endgame is Swift), so optimize for speed and polish on a
dense dashboard — where the web ecosystem wins. The same web UI **doubles as a
browser console** (talking MCP-over-HTTP), so we get desktop + browser from one
build. Tauri's Rust backend is a **thin bridge only** — it spawns/connects the
local `oab-mcp` (or embeds `studio-cp` and re-exposes the same MCP surface to the
webview); it does **not** introduce a second Rust command API that could drift
from MCP.

### 3.4 Live updates: polling first

The console **polls** `deploy_list` / `get_agent_states` on an interval to keep
the roster and 6-state live. A push/subscribe channel on `oab-mcp` is a later
increment, not a prerequisite for the console skeleton.

### 3.5 First console scope

Aligned to the reference director's console: **deployment roster**, each row's
**live 6-state** (ADR-1) plus `desired`/`current`/`ready`, and **per-instance
phase**. Deferred to later slices: per-agent diff, wave/phase orchestration, the
environment/action panel.

## 4. Consequences

**Positive**
- Humans and agents share one control surface; no divergent command path.
- Skin swap (web → SwiftUI) costs zero core change; interim throwaway is bounded
  to UI code.
- Credentials never reach a device; mobile is a thin client by construction.
- Desktop **and** browser from a single web build.

**Negative / costs**
- The interim web UI is discarded when native skins land (accepted).
- A new **streamable-HTTP** transport + a **remote-core hosting** story (auth,
  where it runs) are now on the roadmap.
- Polling has latency/refresh-rate limits until streaming is added.

**Neutral**
- Per-caller authorization stays deferred (inherits ADR-2's interim ceiling: the
  AWS credential boundary). A human console makes this more pressing — see §6.

## 5. Alternatives Considered

- **Dioxus (Rust-native skin).** One Rust codebase for web/desktop/mobile,
  shares types with core. Rejected as the *primary* path because the native
  endgame is Swift (so a Rust cross-platform skin is still interim) and the Rust
  UI ecosystem is thinner for a dense dashboard. Revisit only if the endgame
  ever becomes "one Rust app, no Swift."
- **FFI (UniFFI / swift-bridge): Swift links the Rust core directly.** Tightest
  native integration, but the contract becomes Rust-generated Swift bindings —
  not language-neutral, unusable by a web or remote client, and a second
  boundary to maintain. Keep as an *optimization* a native app may add later,
  not the primary contract.
- **Electron.** Heavy, Node-centric bridge to a Rust core; no advantage over
  Tauri here.

## 6. Open Questions / Deferred

- **Authorization.** A human-facing console sharpens the need for per-caller
  authz beyond the AWS credential ceiling (ADR-2 §deferred). Where does identity
  live — the core service, an existing broker, OIDC?
- **Remote-core hosting.** Where the streamable-HTTP core runs, and how a
  browser/iOS client authenticates to it.
- **Streaming.** The subscribe/push channel that replaces polling.
