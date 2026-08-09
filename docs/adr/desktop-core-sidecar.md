# ADR: Desktop hosts the core as a bundled MCP sidecar

- **Status:** Proposed
- **Amends:** ADR-3 (desktop console) slice-2 wiring.

> The desktop reaches the control-plane core over the **MCP contract**, by
> spawning a **bundled `oab-mcp` sidecar** over stdio at launch — not via an
> in-process crate link. One core path for both humans (desktop) and agents.

## 1. Context & Problem

ADR-3 says the desktop "runs the core locally over stdio (MCP)". Slice-2 took a
shortcut: the Tauri `deploy_list` command linked `studio-cp` as a Rust
dependency and called `observe_services` **in-process**, bypassing `oab-mcp`
entirely. That left **two divergent paths to the same core** — agents via the
MCP server, the desktop via a direct crate call — so the skin never actually
exercised the contract it claims to depend on, and the running app did not start
a core an external agent could later share.

## 2. Decision

The desktop **spawns `oab-mcp` as a Tauri sidecar** (`externalBin`) on startup
and talks to it as a first-class MCP client:

- `initialize` → `notifications/initialized` handshake, then `tools/call`.
- The `deploy_list` bridge lists services (`deploy_list`) then fetches each
  one's per-instance 6-state (`deploy_get`) — the same two-step as before, now
  over the wire. Console view-model shape is unchanged; `TauriSource` /
  `MockSource` stay interchangeable.
- `src-tauri` **drops** its `studio-cp` / `aws-config` dependencies. The core's
  AWS credentials resolve inside the sidecar, from the standard chain, lazily on
  first real tool call (the handshake needs none).
- One long-lived child; JSON-RPC requests are multiplexed by id. Process
  lifetime is bound to the app.

## 3. Scope (now)

- **macOS only** (arm64, unsigned). Windows / universal / code-signing /
  auto-update are deferred.
- Transport is **stdio only** — no local socket / streamable-HTTP yet, so the
  spawned core is not reachable by *external* agents from another process. That
  ("app hosts a core others attach to") is a later transport decision, and this
  ADR is a prerequisite for it (single contract first).

## 4. Consequences

- ✅ Single core path; the desktop dogfoods the MCP boundary. Write tools
  (`deploy_apply` / `deploy_scale` / `deploy_delete`) are now one `call_tool`
  away when the GUI is ready for them.
- ✅ Bundle no longer statically links the aws-sdk into the shell.
- ⚠️ The `.app` carries the `oab-mcp` binary (~larger bundle) and must ship the
  matching per-arch sidecar; CI builds `oab-mcp` for the target triple and
  places it under `src-tauri/binaries/` before `tauri build`.
- ⚠️ Startup now depends on the sidecar spawning + handshaking; failure is
  logged and surfaces as a bridge error rather than a half-wired app.

## 5. Open questions

- Per-caller **authz** once the core is shared beyond the local app (unchanged
  from ADR-2/ADR-3: currently the AWS-credential ceiling only).
- Exposing a **non-stdio transport** so external agents attach to the app's
  core.
- Sidecar **supervision** (restart on crash) and graceful shutdown semantics.
