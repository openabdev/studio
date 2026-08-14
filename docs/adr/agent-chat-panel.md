# ADR: Chatting with a connected agent — `session/prompt` over the live `/acp` socket (Part C)

- **Status:** Proposed
- **Date:** 2026-08-14
- **Author:** Orca (`ecs-claude`)
- **Extends:** [Fleet grouping & connection model](./fleet-grouping-and-connection-model.md) Part B (reverse MCP-over-ACP); [agent connection guide](../connecting-an-agent.md)
- **Reference:** [`brettchien/katashiro`](https://github.com/brettchien/katashiro) — a working ACP chat client against the **same** openab `/acp` gateway; [MCP-over-ACP tunnel contract](https://github.com/openabdev/openab/blob/main/docs/mcp-over-acp-tunnel-contract.md)

---

> Part B made Studio **publish** its `oab` tools to the agent (reverse MCP). This ADR adds the other direction — a **chat panel** that drives the agent with `session/prompt` and renders its streamed `session/update` reply — on the **same** live `/acp` session. The bar is **parity with katashiro's chat experience**; the reference implementation already exists and hits the same gateway, so the wire contract and UX are **copied, not invented**.

## 1. Context

- The remote `/acp` connection now succeeds end-to-end (#45 handshake, #46 `protocolVersion`), reaching `session active — oab tools published`. But that is **only the reverse-MCP direction**: the agent can call Studio's `oab` tools; the operator has **no way to talk to the agent** — "we can't work with the agent."
- `remote.rs` opens exactly one ACP session (`initialize` → `session/new`, declaring `oab`) and then serves the gateway-initiated tunnel. Agent chat frames (`session/update`) already arrive on that socket and are **silently dropped** — `parse_inbound` → `Inbound::Other` → the empty match arm.
- `katashiro` is a working ACP chat client on the **same** gateway. It proves the full contract (`session/prompt` / `session/update` / `session/cancel`) and a complete chat UX (streaming markdown, stop/retry, history). It is the parity target for the **chat**, not the room — we follow its wire and its rendering, and deliberately leave its multi-agent surface behind (§4).

## 2. Decision — Part A: chat rides the existing session

Chat is **additive on the one live `/acp` session**, not a second connection. `session/new` already declares `oab` (reverse MCP); the same session carries prompts and streamed replies (katashiro does exactly this — one socket, both directions). Wire contract (verified live, same gateway):

- **Prompt** — request `session/prompt` `{ sessionId, prompt: [{ type:"text", text }] }` → resolves `{ stopReason }` (`"cancelled"` ⇒ keep the partial reply). Turns stream long; the request stays pending until end-of-turn (timeout ~10 min, per katashiro `ACP_PROMPT_TIMEOUT_MS`).
- **Stream** — notification `session/update`, `params.update.sessionUpdate === "agent_message_chunk"`, text at `params.update.content.text`. (Other update kinds — thoughts, tool calls — are out of MVP scope; see §5.)
- **Stop** — one-way notification `session/cancel` `{ sessionId }`; the gateway resolves the in-flight `session/prompt` with `stopReason:"cancelled"`, so cancellation flows through the normal resolve path — nothing extra to settle.

## 3. Decision — Part B: backend (Rust)

**`crates/acp-tunnel`:**
- Add a `Session::prompt(text)` request builder and a `session/cancel` notification builder, mirroring `open_session`.
- Classify `session/update` (`agent_message_chunk`) into a typed `Inbound::AgentChunk { text }` instead of the catch-all `Inbound::Other`.

**`src-tauri/src/remote.rs`:**
- **`RemoteState` grows a live write handle** — an `mpsc::UnboundedSender<Value>` set when the session goes active (alongside the `sessionId`), cleared on disconnect. This is the missing "reach into the live socket": today `disconnect` can only `.abort()` the whole task.
- `run_once` `tokio::select!`s between `read.next()` and `prompt_rx.recv()`; a received frame goes out through the existing private `send()` helper, so the WS `write` sink never leaves the task.
- **The drop site becomes the chat hook** — the new `Inbound::AgentChunk` arm `app.emit("agent-update", …)`, the same emit/listen model as `remote-status`.
- **Correctness — id correlation:** the read loop today treats *any* method-less frame as a handshake ack that advances the session phase. `session/prompt` responses are **also** method-less; they must be **correlated by request id** (a `pendingReqs` map, as katashiro does) so a prompt result never mis-drives the phase state machine.
- **Resume, not new:** persist the `sessionId`; on connect, drive `Session::resume()` (already in `acp-tunnel`) when we have one and fall back to `session/new` if the gateway rejects it (katashiro's pattern). This is what keeps the **agent's own** memory of the conversation continuous across reconnect/restart — not just the local scrollback. The `sessionId` lives in the Rust core (`RemoteState`/on disk), out of the webview's reach.

**`src-tauri/src/lib.rs`:** two `#[tauri::command]`s next to `remote_connect` — `agent_prompt` (push a `session/prompt` `Value` into the sender) and `agent_cancel` (push `session/cancel`) — taking `tauri::State<'_, remote::Remote>` (+ `AppHandle`), registered in the `invoke_handler!` list.

## 4. Decision — Part C: the chat panel (parity with katashiro)

A new panel/tab in the vanilla-TS `console` (add `<section id="chat">` + a `#tabs` button, `chatHtml`/`renderChat` in `render.ts`, wiring + `listen("agent-update")` in `main.ts`, `agent_prompt`/`agent_cancel` on the delegated click handler). The **load-bearing UX to replicate** (from katashiro's chat):

- **Two-phase agent rendering** — plain `textContent` while streaming (a typing indicator before the first token), then **markdown rendered once at finalize**. Re-parsing per token flickers on half-open code fences and is O(n²); a stopped/errored stream still finalizes.
- **One sanitized markdown sink** — markdown-it (`html:false`, linkify, GFM tables, `hljs` code fences) → **DOMPurify** (pinned config, relaxing forbidden) → link/media scheme hardening → post-sanitize copy-code buttons. Agent output is **remote-controlled**, so the sanitizer is mandatory, not optional, and is treated as a **security dependency** (pin + track advisories). Links open via Tauri's opener API (re-validating the scheme at click time), not `chrome.tabs`.
- **Turn controls** — Enter sends / Shift+Enter newline; a **stop** button (→ `agent_cancel`; the partial reply is kept and annotated `⏹ 已停止`); a **retry** on failed turns; a **batched** prompt queue (messages typed while the agent is busy coalesce into one next turn, blank-line joined, order preserved).
- **Scroll** — stick-to-bottom with an 80px threshold and a **jump-to-latest** pill when scrolled up; sending always pulls to the bottom, receiving only follows if already there.
- **System/error rows**, single-letter avatars, `HH:MM (TPE)` timestamps — all `textContent`, never markdown.
- **History + resume** — the transcript **persists and is restored before connect**, and the stored `sessionId` drives `session/resume` so the agent's side continues too (§3). Single-window desktop, so none of katashiro's per-window `chrome.storage` isolation is needed; `clear` wipes the local transcript but keeps the session (the agent doesn't forget).

**Single-agent, by design.** Studio has one remote endpoint (`remote.toml`) and chat targets exactly that bound agent — today Orca. **No multi-agent room** (Brett, 2026-08-14): katashiro's room / @mention / relay / loop-guard is explicitly **not a goal**. We take its chat primitives — streaming, markdown, turn controls — and leave the room machinery behind, so `remote.rs` and the panel stay one session with no routing layer (`resolveTargets`, `relayAgentReply`, the loop guard, batching-for-relays are all dropped).

## 5. Scope (now)

- ✅ **In (MVP):** the single bound agent; text single-turn streaming (`agent_message_chunk`); markdown render at finalize; stop / retry; batched prompt queue; autoscroll + jump pill; system/error rows; **persistent chat history + `session/resume`** on reconnect/restart.
- ⚠️ **Out (later slices):** the `tool_call` / thought `session/update` kinds (rendered as cards); a browser-tunnel status strip.
- 🚫 **Not a goal:** (a) the multi-agent room — @mention routing, agent→agent relay, loop guard (Brett, 2026-08-14); Studio chats one bound agent, there is no room to light up later. (b) **Accessibility** (screen-reader / `aria-live` / `role="log"`) — this is a single-operator internal console, so a11y is YAGNI. (Rationale is the single user, *not* "it's native": a11y is a webview-UI concern that would otherwise apply to a Tauri app just as it does to an extension.)

## 6. Consequences

- ✅ The remote connection becomes **usable** — the operator drives the agent, not just publishes tools to it.
- ✅ Reuses the existing session / reconnect / status machinery unchanged; only the per-connection loop and `RemoteState` grow a channel.
- ✅ Wire contract and UX are **copied from a working reference on the same gateway** — low protocol risk.
- ⚠️ `remote.rs` gains real bidirectional state (a pending-request map, an outbound channel); "a method-less frame is always a handshake ack" is no longer true.
- ⚠️ Rendering remote agent output via `innerHTML` reintroduces an XSS surface; the DOMPurify sink **plus** a Tauri-side CSP (not the MV3 manifest CSP katashiro uses) are load-bearing, and the `/acp` bearer should live in the Rust core, out of the webview's reach.

## 7. Open questions

1. **Facade routing** — does the gateway/facade route `session/prompt` to the bound agent (Orca) and stream `session/update` back, or is prompting gated behind a specific ACP capability? Confirm server-side (Jellyfish) before Part B lands.
2. **`session/update` taxonomy** — beyond `agent_message_chunk`, what kinds does the openab agent emit (thoughts, `tool_call` / `tool_call_update`)? Defines the phase-2 card model.
3. **Persistence store** — history + resume are **in MVP** (Brett); the open detail is *where* state lives. Leaning: the `sessionId` in the Rust core (`RemoteState` + a small on-disk file, out of the webview), and the transcript in a Tauri app-data store keyed by agent. Confirm the store choice when Part C lands.
4. **Threat model under Tauri** — where the `/acp` bearer lives relative to the webview, and what Studio CSP replaces katashiro's MV3 egress lock.

This is a direction-alignment ADR; implementation lands in slices (Part B backend, then Part C MVP panel), each verified against katashiro parity and the live gateway.
