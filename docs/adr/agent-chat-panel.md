# ADR: Chatting with a connected agent — `session/prompt` over the live `/acp` socket (Part C)

- **Status:** Proposed
- **Date:** 2026-08-14
- **Author:** Orca (`ecs-claude`)
- **Extends:** [Fleet grouping & connection model](./fleet-grouping-and-connection-model.md) Part B (reverse MCP-over-ACP); [agent connection guide](../connecting-an-agent.md)
- **Reference:** [`brettchien/katashiro`](https://github.com/brettchien/katashiro) — a working ACP chat client against the **same** openab `/acp` gateway; [MCP-over-ACP tunnel contract](https://github.com/openabdev/openab/blob/main/docs/mcp-over-acp-tunnel-contract.md)
- **Reviewed:** Jellyfish, 2026-08-14 — live server-side eval against Orca's `/acp` (routing, `session/update` taxonomy, refactor). Findings folded into §2/§4/§5/§7.

---

> Part B made Studio **publish** its `oab` tools to the agent (reverse MCP). This ADR adds the other direction — a **chat panel** that drives the agent with `session/prompt` and renders its `session/update` reply — on the **same** live `/acp` session. The bar is **parity with katashiro's chat surface**; the reference already exists and hits the same gateway, so the wire contract and UX are **followed, not invented** — including the live-verified fact that openab's `/acp` returns a **single terminal reply today**, not a token stream (§2).

## 1. Context

- The remote `/acp` connection now succeeds end-to-end (#45 handshake, #46 `protocolVersion`), reaching `session active — oab tools published`. But that is **only the reverse-MCP direction**: the agent can call Studio's `oab` tools; the operator has **no way to talk to the agent** — "we can't work with the agent."
- `remote.rs` opens exactly one ACP session (`initialize` → `session/new`, declaring `oab`) and then serves the gateway-initiated tunnel. Agent chat frames (`session/update`) already arrive on that socket and are **silently dropped** — `parse_inbound` → `Inbound::Other` → the empty match arm.
- `katashiro` is a working ACP chat client on the **same** gateway. It proves the full contract (`session/prompt` / `session/update` / `session/cancel`) and a complete chat UX (markdown, stop/retry, history). It is the parity target for the **chat surface**, not the room — we follow its wire and its rendering, and deliberately leave its multi-agent surface behind (§4). Note: katashiro's *token-streaming* animation is not exercised against this gateway either — openab returns one terminal chunk (§2), so "parity" is the chat surface, not a stream the server can't drive.

## 2. Decision — Part A: chat rides the existing session

Chat is **additive on the one live `/acp` session**, not a second connection. `session/new` already declares `oab` (reverse MCP); the same session carries prompts and the reply (katashiro does exactly this — one socket, both directions). Wire contract (verified live, same gateway):

- **Prompt** — request `session/prompt` `{ sessionId, prompt: [{ type:"text", text }] }` → resolves `{ stopReason }` (`"end_turn"` on success). The request stays pending for the **whole turn** (timeout ~10 min, per katashiro `ACP_PROMPT_TIMEOUT_MS`) — turns take real wall-clock time even though the reply arrives in one piece.
- **Reply** — notification `session/update`, `params.update.sessionUpdate === "agent_message_chunk"`, text at `params.update.content.text`. **openab's `/acp` emits a single *terminal* `agent_message_chunk` today** — verified live: ~5.5 s of silence, then one ~329-char chunk, then the result. It is **not** token-by-token streaming (openab source: single-terminal-chunk, `streaming=false`; the emitting test is named `phase1_…`, so a later phase may stream). The design below treats the reply as one chunk but keeps an accumulate-then-render shape so incremental chunks, if openab ever emits them, need no rework.
- **Stop** — one-way notification `session/cancel` `{ sessionId }`; the gateway resolves the in-flight `session/prompt` with `stopReason:"cancelled"`. In the single-terminal-chunk model a cancel before the chunk means **no partial reply exists** — stop = *abandon this turn*. (If openab later streams, the keep-the-partial semantics apply for free.)

## 3. Decision — Part B: backend (Rust)

**`crates/acp-tunnel`:**
- Add a `Session::prompt(text)` request builder and a `session/cancel` notification builder, mirroring `open_session`.
- Classify `session/update` (`agent_message_chunk`) into a typed `Inbound::AgentChunk { text }` instead of the catch-all `Inbound::Other`.

**`src-tauri/src/remote.rs`:**
- **`RemoteState` grows a live write handle** — an `mpsc::UnboundedSender<Value>` set when the session goes active (alongside the `sessionId`), cleared on disconnect. This is the missing "reach into the live socket": today `disconnect` can only `.abort()` the whole task.
- `run_once` `tokio::select!`s between `read.next()` and `prompt_rx.recv()`; a received frame goes out through the existing private `send()` helper, so the WS `write` sink never leaves the task.
- **The drop site becomes the chat hook** — the new `Inbound::AgentChunk` arm `app.emit("agent-update", …)`, the same emit/listen model as `remote-status`.
- **Correctness — id correlation:** the read loop today treats *any* method-less frame as a handshake ack that advances the session phase. `session/prompt` responses are **also** method-less; they must be **correlated by request id** (a `pendingReqs` map, as katashiro does) so a prompt result never mis-drives the phase state machine.
- **Resume, not new:** persist the `sessionId`; on connect, drive `Session::resume()` (already in `acp-tunnel`) when we have one and fall back to `session/new` if the gateway rejects it (katashiro's pattern). **Resume params are `{ sessionId, cwd, mcpServers }`** — verified live (Jellyfish): `{ sessionId }` alone fails `-32602 missing field cwd`, and the `oab` `mcpServers` **must be re-declared** so the reverse-MCP tunnel re-attaches on the resumed session — otherwise the agent remembers the conversation but **loses Studio's `oab` tools**. So resume keeps **both** the agent's memory *and* the tool tunnel continuous (confirmed: seed a codeword on one socket → close → resume on a fresh socket → the agent recalls it). The `sessionId` lives in the Rust core (`RemoteState`/on disk), out of the webview's reach. (Base semantics are confirmed; two edges to re-check when Part B wires resume for real — resuming after a *mid-turn* disconnect, and that the `oab` tunnel actually re-attaches post-resume.)

**`src-tauri/src/lib.rs`:** two `#[tauri::command]`s next to `remote_connect` — `agent_prompt` (push a `session/prompt` `Value` into the sender) and `agent_cancel` (push `session/cancel`) — taking `tauri::State<'_, remote::Remote>` (+ `AppHandle`), registered in the `invoke_handler!` list.

## 4. Decision — Part C: the chat panel (parity with katashiro)

A new panel/tab in the vanilla-TS `console` (add `<section id="chat">` + a `#tabs` button, `chatHtml`/`renderChat` in `render.ts`, wiring + `listen("agent-update")` in `main.ts`, `agent_prompt`/`agent_cancel` on the delegated click handler). The **load-bearing UX to replicate** (from katashiro's chat):

- **Reply rendering (single-shot today).** Send → a **spinner / typing indicator** covers the multi-second think (the wait is real — ~5.5 s+ before any reply, §2). The terminal `agent_message_chunk` arrives and is **rendered to markdown once**. The code keeps the **accumulate-then-render-once** shape (append chunk text into a buffer, markdown at end-of-turn), so it stays correct at one chunk *and* forward-compatible if openab later emits incremental chunks — but there is **no token-by-token phase today**, so per-token flicker / O(n²) concerns simply don't arise. A stopped/errored turn still finalizes (renders whatever, if anything, arrived).
- **One sanitized markdown sink** — markdown-it (`html:false`, linkify, GFM tables, `hljs` code fences) → **DOMPurify** (pinned config, relaxing forbidden) → link/media scheme hardening → post-sanitize copy-code buttons. Agent output is **remote-controlled**, so the sanitizer is mandatory, not optional, and is treated as a **security dependency** (pin + track advisories). Links open via Tauri's opener API (re-validating the scheme at click time), not `chrome.tabs`.
- **Turn controls** — Enter sends / Shift+Enter newline; a **stop** button (→ `agent_cancel`) that **abandons the turn** (single-shot ⇒ usually no partial to keep; annotate `⏹ 已停止`); a **retry** on failed turns; a **batched** prompt queue (messages typed while the agent is busy coalesce into one next turn, blank-line joined, order preserved).
- **Scroll** — stick-to-bottom with an 80px threshold and a **jump-to-latest** pill when scrolled up; sending always pulls to the bottom, receiving only follows if already there.
- **System/error rows**, single-letter avatars, `HH:MM (TPE)` timestamps — all `textContent`, never markdown.
- **History + resume** — the transcript **persists and is restored before connect**, and the stored `sessionId` drives `session/resume` so the agent's side continues too (§3). Single-window desktop, so none of katashiro's per-window `chrome.storage` isolation is needed; `clear` wipes the local transcript but keeps the session (the agent doesn't forget).

**Single-agent, by design.** Studio has one remote endpoint (`remote.toml`) and chat targets exactly that bound agent — today Orca. **No multi-agent room** (Brett, 2026-08-14): katashiro's room / @mention / relay / loop-guard is explicitly **not a goal**. We take its chat primitives — rendering, markdown, turn controls — and leave the room machinery behind, so `remote.rs` and the panel stay one session with no routing layer (`resolveTargets`, `relayAgentReply`, the loop guard, batching-for-relays are all dropped).

## 5. Scope (now)

- ✅ **In (MVP):** the single bound agent; **single-shot text reply** (one terminal `agent_message_chunk`) behind a think-spinner; markdown render at finalize; stop / retry; batched prompt queue; autoscroll + jump pill; system/error rows; **persistent chat history + `session/resume`** on reconnect/restart.
- ⚠️ **Out (later slices):** a browser-tunnel status strip.
- ⛔ **Blocked on openab (upstream), not a Studio slice:** `tool_call` / thought rendering — the `/acp` adapter emits **neither** today (verified live: only `agent_message_chunk`; the `tool_call` schema is inert). Real **token streaming** is likewise upstream (openab emits one terminal chunk, `phase1`). Studio's render is already forward-compatible; these light up only after openab's `/acp` emits the frames.
- 🚫 **Not a goal:** (a) the multi-agent room — @mention routing, agent→agent relay, loop guard (Brett, 2026-08-14); Studio chats one bound agent, there is no room to light up later. (b) **Accessibility** (screen-reader / `aria-live` / `role="log"`) — this is a single-operator internal console, so a11y is YAGNI. (Rationale is the single user, *not* "it's native": a11y is a webview-UI concern that would otherwise apply to a Tauri app just as it does to an extension.)

## 6. Consequences

- ✅ The remote connection becomes **usable** — the operator drives the agent, not just publishes tools to it.
- ✅ Reuses the existing session / reconnect / status machinery unchanged; only the per-connection loop and `RemoteState` grow a channel.
- ✅ Wire contract and UX are **copied from a working reference on the same gateway** — low protocol risk.
- ⚠️ `remote.rs` gains real bidirectional state (a pending-request map, an outbound channel); "a method-less frame is always a handshake ack" is no longer true.
- ⚠️ Rendering remote agent output via `innerHTML` reintroduces an XSS surface; the DOMPurify sink **plus** a Tauri-side CSP (not the MV3 manifest CSP katashiro uses) are load-bearing, and the `/acp` bearer should live in the Rust core, out of the webview's reach.
- ⚠️ The reply is **single-shot** (no live token stream) until openab's `/acp` streams — chat feels *submit → wait → answer*, so the think-spinner and a generous prompt timeout carry the UX, not incremental text. This is a gateway limitation, not a Studio one.

### Resolved by review (Jellyfish, live, 2026-08-14)

- **Facade routing — ✅ confirmed.** `session/prompt` routes to the bound agent, runs (incl. a `bash` tool call), and returns `{ stopReason: "end_turn" }`. Not gated behind a capability; Part B is viable.
- **`session/update` taxonomy — resolved.** openab emits **only** `agent_message_chunk`, a **single terminal** one; **zero** `tool_call` / thought frames (two live prompts, incl. a tool-forcing one). Folded into §2/§5 (streaming + cards are upstream-blocked).
- **`remote.rs` refactor — ✅ sound.** The mpsc + `select!` + `pendingReqs` id-correlation is correct and necessary (prompt results are method-less, same shape as handshake acks).
- **`session/resume` — ✅ honoured, memory preserved across reconnect.** Seed a codeword on one socket → close → `session/resume` on a fresh socket → the agent recalls it. Requires `{ sessionId, cwd, mcpServers }` with `oab` re-declared (folded into §3).

### Still open

1. **Persistence store** — history + resume are **in MVP** (Brett); open detail is *where* state lives. Leaning: `sessionId` in the Rust core (`RemoteState` + a small on-disk file, out of the webview), transcript in a Tauri app-data store keyed by agent. Confirm when Part C lands.
2. **Threat model under Tauri** — where the `/acp` bearer lives relative to the webview, and what Studio CSP replaces katashiro's MV3 egress lock.

This is a direction-alignment ADR; implementation lands in slices (Part B backend — builders + mpsc + id-correlation, streaming-independent, can start first — then Part C MVP panel), each verified against the live gateway.
