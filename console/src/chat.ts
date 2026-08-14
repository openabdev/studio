// The chat panel's view-model and pure render (ADR *agent-chat-panel*, Part C).
// The backend (`remote.rs`, Part B) drives a **single-shot** turn over the live
// `/acp` session: `agent_prompt` sends one turn, `agent-update` events stream the
// reply back as `chunk`s and close it with `turn_end`. This module owns the
// transcript shape and its HTML; `main.ts` owns the DOM, the event wiring and the
// turn queue. Keeping the render a pure function of `ChatTurn[]` mirrors
// `render.ts` and keeps it unit-testable without a DOM.

import MarkdownIt from "markdown-it";

// One entry in the transcript. `user` turns are the operator's prompts; `agent`
// turns stream in and finalize. `streaming` marks an agent turn still receiving
// chunks — rendered as raw text with a spinner; once final we render markdown.
// `stopReason` is the ACP stop reason on a finished agent turn (`end_turn`,
// `cancelled`, …); shown only when it is not the ordinary `end_turn`.
export type ChatRole = "user" | "agent";

export interface ChatTurn {
  id: number;
  role: ChatRole;
  text: string;
  streaming: boolean;
  stopReason?: string;
}

// Markdown → HTML for a finalized agent turn. `html: false` escapes any raw HTML
// in the agent's output (so the only tags are markdown-it's own), and
// markdown-it's default `validateLink` already blocks `javascript:` / `data:`
// URLs. The caller still runs the result through DOMPurify at DOM-write time
// (ADR: markdown-it + DOMPurify) as a second layer.
const md = new MarkdownIt({ html: false, linkify: true, breaks: true });

export function mdToHtml(text: string): string {
  return md.render(text);
}

// Local copy of render.ts's escaper — the transcript never trusts turn text
// (user input and streaming agent text are both inserted as plain text).
function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// A finalized agent turn's body is untrusted markdown → HTML. `main.ts` injects
// `(t) => DOMPurify.sanitize(mdToHtml(t))`; tests use the `mdToHtml` default so
// they need no DOM. Only the agent-markdown path takes this — user text and the
// panel's own chrome are always escaped/trusted, so they are never sanitized.
export type RenderAgent = (text: string) => string;

export function turnHtml(turn: ChatTurn, renderAgent: RenderAgent): string {
  if (turn.role === "user") {
    return (
      `<div class="chat-turn chat-user">` +
      `<div class="chat-body">${escapeHtml(turn.text)}</div>` +
      `</div>`
    );
  }
  if (turn.streaming) {
    // Raw text while streaming (partial markdown mustn't half-render), plus a
    // spinner. Empty text before the first chunk still shows the spinner alone.
    return (
      `<div class="chat-turn chat-agent" data-id="${turn.id}">` +
      `<div class="chat-body chat-stream">${escapeHtml(turn.text)}` +
      `<span class="chat-spinner" aria-label="thinking"></span></div>` +
      `</div>`
    );
  }
  const reason =
    turn.stopReason && turn.stopReason !== "end_turn"
      ? `<span class="chat-reason">${escapeHtml(turn.stopReason)}</span>`
      : "";
  return (
    `<div class="chat-turn chat-agent" data-id="${turn.id}">` +
    `<div class="chat-body chat-md">${renderAgent(turn.text)}</div>` +
    `<div class="chat-tools">` +
    `<button class="chat-copy" type="button" data-copy="${turn.id}">Copy</button>${reason}` +
    `</div>` +
    `</div>`
  );
}

// The whole transcript. `renderAgent` defaults to `mdToHtml` so tests exercise the
// real markdown path without a DOM; `main.ts` passes the sanitizing wrapper.
export function transcriptHtml(
  turns: ChatTurn[],
  renderAgent: RenderAgent = mdToHtml,
): string {
  if (turns.length === 0) {
    return `<p class="chat-empty">No messages yet — send a prompt to the connected agent.</p>`;
  }
  return turns.map((t) => turnHtml(t, renderAgent)).join("");
}

// ---- transcript reducers ----------------------------------------------------
// Pure updates: `main.ts` holds the `ChatTurn[]` and re-renders after each. The
// id generator is the caller's (a monotonic counter), so turns keep stable keys
// for the copy action across re-renders.

export function appendUser(
  turns: ChatTurn[],
  id: number,
  text: string,
): ChatTurn[] {
  return [...turns, { id, role: "user", text, streaming: false }];
}

// Append a streamed `chunk`. If no agent turn is open yet (this is the turn's
// first chunk), start one with the given id; otherwise append to the open turn.
export function appendChunk(
  turns: ChatTurn[],
  id: number,
  text: string,
): ChatTurn[] {
  const last = turns[turns.length - 1];
  if (last && last.role === "agent" && last.streaming) {
    const updated: ChatTurn = { ...last, text: last.text + text };
    return [...turns.slice(0, -1), updated];
  }
  return [...turns, { id, role: "agent", text, streaming: true }];
}

// Close the open agent turn on `turn_end`. If a `turn_end` arrives with no open
// turn (e.g. a cancel before any chunk), synthesize an empty finalized turn so
// the stop reason is still visible. No-op shape otherwise stays pure.
export function endTurn(
  turns: ChatTurn[],
  id: number,
  stopReason: string,
): ChatTurn[] {
  const last = turns[turns.length - 1];
  if (last && last.role === "agent" && last.streaming) {
    const updated: ChatTurn = { ...last, streaming: false, stopReason };
    return [...turns.slice(0, -1), updated];
  }
  return [...turns, { id, role: "agent", text: "", streaming: false, stopReason }];
}
