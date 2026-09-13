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
  // A user turn typed while a prior turn is still in flight: shown immediately in
  // a "queued" state so it isn't invisible until the queue drains. Cleared when
  // its turn actually starts.
  pending?: boolean;
  // Inline images attached to this turn (issue #158) — pasted by the operator on
  // a `user` turn, or streamed in from the agent's reply on an `agent` turn.
  // Rendered after the turn's text; order relative to interleaved text chunks
  // isn't preserved, the same simplification `appendChunk` already makes for
  // multiple text chunks accumulating into one string.
  images?: ChatImage[];
}

// One inline image — the wire shape is already `{ data, mimeType }` end to end
// (ACP `ImageContent`, `agent-update` events, the `agent_prompt` payload), so the
// transcript keeps it as-is rather than inventing another shape.
export interface ChatImage {
  data: string;
  mimeType: string;
}

// Only these are rendered — image content is agent- or clipboard-sourced, less
// trusted than markdown text, so it gets an allow-list instead of open season on
// `<img src="data:...">`.
const ALLOWED_IMAGE_MIME = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

// Base64's alphabet has no `<`/`>`/`"`/`'` — a `data` value that doesn't match
// this shape isn't real base64 and must not reach a `src` attribute.
const BASE64_RE = /^[A-Za-z0-9+/]+={0,2}$/;

// Hard cap on one image's base64 payload: ~8MB decoded (≈10.9M base64 chars).
// Mirrors the ~5MB raw-file cap `chatPanel.ts` enforces before it ever base64s a
// pasted image — this is the render-side half of the same guard, so a malformed
// or oversized image (however it got here) is dropped rather than rendered.
const MAX_IMAGE_BASE64_CHARS = Math.ceil((8 * 1024 * 1024 * 4) / 3);

// Exported so `chatPanel.ts` can apply the same allow-list/shape check to a
// freshly pasted image before it ever gets attached to an outgoing turn.
export function isRenderableImage(img: ChatImage): boolean {
  return (
    ALLOWED_IMAGE_MIME.has(img.mimeType) &&
    img.data.length > 0 &&
    img.data.length <= MAX_IMAGE_BASE64_CHARS &&
    BASE64_RE.test(img.data)
  );
}

// `mimeType` is allow-listed and `data` is base64-shape-checked above, so neither
// can carry a quote/angle-bracket — safe to interpolate directly into the `src`
// attribute without a further escape pass.
function imagesHtml(images: ChatImage[] | undefined): string {
  const valid = (images ?? []).filter(isRenderableImage);
  if (valid.length === 0) return "";
  return (
    `<div class="chat-images">` +
    valid
      .map((img) => `<img class="chat-image" src="data:${img.mimeType};base64,${img.data}" alt="pasted image">`)
      .join("") +
    `</div>`
  );
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

// A finalized turn's body is rendered markdown → HTML. `chatPanel.ts` injects
// `(t) => DOMPurify.sanitize(mdToHtml(t))`; tests use the `mdToHtml` default so
// they need no DOM. **Both** the operator's prompts and finalized agent turns
// take this path — markdown-it (`html: false`) escapes any raw HTML and DOMPurify
// is the second layer, so rendering the operator's own markdown is safe. Only a
// still-streaming agent turn stays raw (partial markdown mustn't half-render).
export type RenderMarkdown = (text: string) => string;

export function turnHtml(turn: ChatTurn, renderMd: RenderMarkdown): string {
  if (turn.role === "user") {
    const cls = turn.pending ? "chat-turn chat-user chat-pending" : "chat-turn chat-user";
    const queued = turn.pending ? `<div class="chat-queued">queued…</div>` : "";
    return (
      `<div class="${cls}">` +
      `<div class="chat-body chat-md">${renderMd(turn.text)}</div>` +
      imagesHtml(turn.images) +
      queued +
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
      imagesHtml(turn.images) +
      `</div>`
    );
  }
  const reason =
    turn.stopReason && turn.stopReason !== "end_turn"
      ? `<span class="chat-reason">${escapeHtml(turn.stopReason)}</span>`
      : "";
  return (
    `<div class="chat-turn chat-agent" data-id="${turn.id}">` +
    `<div class="chat-body chat-md">${renderMd(turn.text)}</div>` +
    imagesHtml(turn.images) +
    `<div class="chat-tools">` +
    `<button class="chat-copy" type="button" data-copy="${turn.id}">Copy</button>${reason}` +
    `</div>` +
    `</div>`
  );
}

// The whole transcript. `renderMd` defaults to `mdToHtml` so tests exercise the
// real markdown path without a DOM; `chatPanel.ts` passes the sanitizing wrapper.
export function transcriptHtml(
  turns: ChatTurn[],
  renderMd: RenderMarkdown = mdToHtml,
): string {
  if (turns.length === 0) {
    return `<p class="chat-empty">No messages yet — send a prompt to the connected agent.</p>`;
  }
  return turns.map((t) => turnHtml(t, renderMd)).join("");
}

// ---- transcript reducers ----------------------------------------------------
// Pure updates: `main.ts` holds the `ChatTurn[]` and re-renders after each. The
// id generator is the caller's (a monotonic counter), so turns keep stable keys
// for the copy action across re-renders.

export function appendUser(
  turns: ChatTurn[],
  id: number,
  text: string,
  pending = false,
  images?: ChatImage[],
): ChatTurn[] {
  return [...turns, { id, role: "user", text, streaming: false, pending, images }];
}

// Clear the `pending` flag on the user turn `id` — its queued message is now the
// active turn (being sent), so it renders as a normal sent message.
export function settlePending(turns: ChatTurn[], id: number): ChatTurn[] {
  return turns.map((t) => (t.id === id ? { ...t, pending: false } : t));
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

// Append a streamed inline `image` (issue #158). Mirrors `appendChunk`: opens the
// turn on the first update of a reply (chunk or image), otherwise appends to the
// still-open turn's image list.
export function appendImage(
  turns: ChatTurn[],
  id: number,
  image: ChatImage,
): ChatTurn[] {
  const last = turns[turns.length - 1];
  if (last && last.role === "agent" && last.streaming) {
    const images = [...(last.images ?? []), image];
    const updated: ChatTurn = { ...last, images };
    return [...turns.slice(0, -1), updated];
  }
  return [...turns, { id, role: "agent", text: "", streaming: true, images: [image] }];
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
