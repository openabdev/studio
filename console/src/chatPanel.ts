// The chat panel as a **reusable primitive** (ADR agent-consoles Part A: "the
// chat panel is one component, instantiated against a chosen endpoint"). Both
// the management console and each per-agent console mount one of these; the only
// difference is the endpoint the turns are sent to (`opts.agent`) and the DOM it
// binds. All the turn machinery — the transcript, the one-turn-at-a-time queue,
// the streaming spinner, stop/retry, copy — lives here so it is written once.
//
// The pure transcript reducers and HTML stay in `chat.ts` (unit-tested without a
// DOM); this module owns the imperative shell: DOM writes, the `<form>`/keydown
// listeners, and the queue. It does **not** subscribe to backend events itself —
// the caller (`main.ts`) owns the single `agent-update` / `remote-status`
// subscription and routes each event to the matching panel by endpoint name via
// `onChunk` / `onTurnEnd` / `setConnected`. That keeps one listener for N panels
// and makes routing explicit.

import DOMPurify from "dompurify";
import {
  transcriptHtml,
  mdToHtml,
  appendUser,
  settlePending,
  appendChunk,
  appendImage,
  endTurn,
  isRenderableImage,
  type ChatTurn,
  type ChatImage,
} from "./chat";
import type { Source } from "./source";

// The DOM a panel drives. The management console and each agent console pass
// their own set of these (same roles, different nodes).
export interface ChatPanelElements {
  log: HTMLElement;
  form: HTMLFormElement;
  text: HTMLTextAreaElement;
  send: HTMLButtonElement;
  stop: HTMLButtonElement;
  conn: HTMLElement;
  // A fixed line above the input box, shown only while a turn is in flight —
  // unlike `conn` (which also carries the not-ready/connected states and can
  // scroll out of view above a long transcript), this sits right where the
  // user is about to type, so "is it still working?" never requires a glance
  // up top or a scroll through the log.
  status: HTMLElement;
}

export interface ChatPanelOptions {
  // The registry endpoint name turns are sent to (`agentPrompt`/`agentCancel`
  // pass it through). Omitted ⇒ the management endpoint (legacy single-console
  // commands). Event routing keys off this same name in `main.ts`.
  agent?: string;
  source: Source;
  // True in the browser build (no live agent): drive a canned reply locally so
  // the panel stays demonstrable without a gateway.
  mock: boolean;
  note: (level: "info" | "error", msg: string) => void;
  // Shown on the connection pill when chat isn't usable yet. Defaults to the
  // management console's wording; an agent console overrides it (it auto-dials).
  notReadyLabel?: string;
}

// The imperative handle the caller drives. `onChunk`/`onTurnEnd` feed streamed
// backend events in; `setConnected` reflects the live transport state; `dispose`
// tears down the DOM listeners (an agent console re-binds a panel per selection).
export interface ChatPanel {
  readonly agent?: string;
  onChunk(text: string): void;
  onImage(image: ChatImage): void;
  onTurnEnd(stopReason: string): void;
  setConnected(connected: boolean): void;
  isConnected(): boolean;
  render(): void;
  dispose(): void;
}

// Markdown → HTML for a turn body: markdown-it (`html: false`) escapes raw HTML
// and blocks dangerous link protocols; DOMPurify is the second layer (ADR:
// markdown-it + DOMPurify). Both user prompts and finalized agent turns take
// this (see `chat.ts`); only a still-streaming agent turn is raw.
function renderMarkdownBody(text: string): string {
  return DOMPurify.sanitize(mdToHtml(text));
}

// Max raw (pre-base64) bytes for one pasted image, and max images per message.
// ACP ships images as base64-in-JSON over the same single WS/stdio pipe as
// turn-taking chat (issue #158's stated concern) — no downscale in this slice,
// an oversized paste is rejected with a note rather than silently shrunk.
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const MAX_IMAGES_PER_TURN = 4;

// `FileReader.readAsDataURL` yields `"data:<mime>;base64,<payload>"` — keep only
// the payload, the mime type is read separately off the `File`.
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      const comma = result.indexOf(",");
      resolve(comma === -1 ? result : result.slice(comma + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("image read failed"));
    reader.readAsDataURL(file);
  });
}

export function createChatPanel(
  els: ChatPanelElements,
  opts: ChatPanelOptions,
): ChatPanel {
  const notReady = opts.notReadyLabel ?? "activate the remote connection to chat";
  // Per-panel transcript state (was module-global in main.ts's single console).
  let turns: ChatTurn[] = [];
  let turnActive = false;
  // Each queued prompt carries the id of its (already-rendered) pending turn, so
  // `flush` can settle that exact turn when it starts sending.
  const queue: { id: number; text: string; images: ChatImage[] }[] = [];
  let seq = 0;
  let connected = false;
  // Images pasted into `els.text` since the last send (issue #158) — held here so
  // `submit()` can bundle them into the next queued turn; cleared once queued.
  let pendingImages: ChatImage[] = [];
  // DOM listeners are scoped to this controller so `dispose()` removes them all
  // at once — an agent console mounts a fresh panel each time it opens.
  const ac = new AbortController();
  const { signal } = ac;

  // A small preview strip above the textarea so the operator can see (and drop)
  // an attached image before sending. Built here rather than threaded through
  // `ChatPanelElements` (and both its callers) — this panel already owns
  // `els.form`'s DOM, so growing it in place keeps the change panel-local.
  const attachEl = document.createElement("div");
  attachEl.className = "chat-attachments";
  attachEl.hidden = true;
  els.form.insertBefore(attachEl, els.text);

  function renderAttachments(): void {
    attachEl.hidden = pendingImages.length === 0;
    attachEl.innerHTML = pendingImages
      .map(
        (img, i) =>
          `<span class="chat-attachment">` +
          `<img src="data:${img.mimeType};base64,${img.data}" alt="pasted image">` +
          `<button type="button" class="chat-attachment-remove" data-index="${i}" aria-label="remove image">×</button>` +
          `</span>`,
      )
      .join("");
  }

  function errText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  // Chat is usable once the connection is live (or always, in the mock).
  function ready(): boolean {
    return opts.mock || connected;
  }

  function render(): void {
    els.log.innerHTML = transcriptHtml(turns, renderMarkdownBody);
    els.log.scrollTop = els.log.scrollHeight; // keep the latest turn in view
  }

  function updateControls(): void {
    const r = ready();
    els.send.disabled = !r;
    els.text.disabled = !r;
    els.stop.hidden = !turnActive;
    const label = !r ? notReady : turnActive ? "agent is responding…" : "connected";
    els.conn.textContent = label;
    els.conn.classList.toggle("is-connected", r && !turnActive);
    els.conn.classList.toggle("is-error", false);
    els.status.hidden = !turnActive;
  }

  // Enqueue a prompt and try to release it. `flush` is the single choke point
  // that enforces one-turn-at-a-time; typing mid-turn just grows the queue. Text
  // is optional when at least one image is attached (issue #158) — an image-only
  // message is valid.
  function submit(text: string): void {
    const trimmed = text.trim();
    const images = pendingImages;
    if (!trimmed && images.length === 0) return;
    pendingImages = [];
    renderAttachments();
    // Show the message immediately — as `pending` if a turn is in flight (so it
    // sits visibly queued), else it is settled the instant `flush` runs.
    seq += 1;
    const id = seq;
    turns = appendUser(turns, id, trimmed, turnActive, images.length ? images : undefined);
    queue.push({ id, text: trimmed, images });
    render();
    void flush();
  }

  async function flush(): Promise<void> {
    if (turnActive) return; // a turn is in flight — wait for its `turn_end`
    const next = queue.shift();
    if (next === undefined) return;
    turnActive = true;
    turns = settlePending(turns, next.id); // now the active turn, no longer queued
    render();
    updateControls();
    try {
      await opts.source.agentPrompt(next.text, opts.agent, next.images.length ? next.images : undefined);
      if (opts.mock) mockReply(next.text); // browser preview: synthesize the reply
    } catch (e) {
      // Send failed (not connected / socket just closed): close the turn with an
      // error, surface it, release the queue so a later prompt can still go.
      turnActive = false;
      opts.note("error", `chat: ${errText(e)}`);
      seq += 1;
      turns = endTurn(turns, seq, "error");
      render();
      updateControls();
      void flush();
    }
  }

  // A streamed `chunk`: open the agent turn on the first one (stable id for its
  // copy button), append thereafter.
  function onChunk(text: string): void {
    const last = turns[turns.length - 1];
    const open = last?.role === "agent" && last.streaming;
    const id = open ? (last as ChatTurn).id : (seq += 1);
    turns = appendChunk(turns, id, text);
    render();
  }

  // A streamed inline `image` (issue #158): same open-turn rule as `onChunk` —
  // the image and text chunks of one reply share a turn regardless of arrival
  // order.
  function onImage(image: ChatImage): void {
    const last = turns[turns.length - 1];
    const open = last?.role === "agent" && last.streaming;
    const id = open ? (last as ChatTurn).id : (seq += 1);
    turns = appendImage(turns, id, image);
    render();
  }

  // `turn_end`: finalize the open agent turn (markdown render), free the gate,
  // and release any queued prompt.
  function onTurnEnd(stopReason: string): void {
    seq += 1;
    turns = endTurn(turns, seq, stopReason);
    turnActive = false;
    render();
    updateControls();
    void flush();
  }

  async function stopTurn(): Promise<void> {
    if (!turnActive) return;
    try {
      await opts.source.agentCancel(opts.agent);
      opts.note("info", "chat: cancel sent");
    } catch (e) {
      opts.note("error", `chat: cancel failed — ${errText(e)}`);
    }
    // The backend still emits a `turn_end` (stopReason `cancelled`), which clears
    // `turnActive` and flushes the queue — no local state change needed here.
  }

  // Browser preview only: stream a short canned markdown reply so the chunk →
  // turn_end → markdown path is visible without a live gateway.
  function mockReply(prompt: string): void {
    const parts = [
      `You said: **${prompt}**.\n\n`,
      "Here's what the panel renders:\n\n",
      "- streamed *chunks*\n- then final `markdown`\n\n",
      "```\ncode stays monospaced\n```",
    ];
    let i = 0;
    const step = (): void => {
      if (i < parts.length) {
        onChunk(parts[i]);
        i += 1;
        window.setTimeout(step, 130);
      } else {
        onTurnEnd("end_turn");
      }
    };
    window.setTimeout(step, 150);
  }

  function setConnected(next: boolean): void {
    connected = next;
    // If the socket drops mid-turn, no `turn_end` will arrive — close the open
    // turn so the panel doesn't hang on a spinner.
    if (!connected && turnActive) onTurnEnd("disconnected");
    updateControls();
  }

  // ---- wiring ---------------------------------------------------------------
  els.form.addEventListener(
    "submit",
    (ev) => {
      ev.preventDefault();
      submit(els.text.value);
      els.text.value = "";
    },
    { signal },
  );
  // Enter sends; Shift+Enter inserts a newline.
  els.text.addEventListener(
    "keydown",
    (ev) => {
      if (ev.key === "Enter" && !ev.shiftKey) {
        ev.preventDefault();
        els.form.requestSubmit();
      }
    },
    { signal },
  );
  // Paste an image → attach it to the next send instead of dropping it (issue
  // #158). A paste with no image items falls through to the default text paste.
  els.text.addEventListener(
    "paste",
    (ev) => {
      const items = ev.clipboardData?.items;
      if (!items) return;
      const files = Array.from(items)
        .filter((it) => it.kind === "file" && it.type.startsWith("image/"))
        .map((it) => it.getAsFile())
        .filter((f): f is File => f !== null);
      if (files.length === 0) return;
      ev.preventDefault();
      for (const file of files) {
        if (pendingImages.length >= MAX_IMAGES_PER_TURN) {
          opts.note("error", `chat: only ${MAX_IMAGES_PER_TURN} images per message`);
          break;
        }
        if (file.size > MAX_IMAGE_BYTES) {
          opts.note(
            "error",
            `chat: image too large (${(file.size / 1024 / 1024).toFixed(1)}MB, max ${MAX_IMAGE_BYTES / 1024 / 1024}MB)`,
          );
          continue;
        }
        void fileToBase64(file).then((data) => {
          const image: ChatImage = { data, mimeType: file.type };
          if (!isRenderableImage(image)) {
            opts.note("error", `chat: unsupported image type — ${file.type}`);
            return;
          }
          pendingImages = [...pendingImages, image];
          renderAttachments();
        });
      }
    },
    { signal },
  );
  // Delegated remove: drop one pending attachment before it's sent.
  attachEl.addEventListener(
    "click",
    (ev) => {
      const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>(
        "button.chat-attachment-remove",
      );
      if (!btn) return;
      const idx = Number(btn.dataset.index);
      pendingImages = pendingImages.filter((_, i) => i !== idx);
      renderAttachments();
    },
    { signal },
  );
  els.stop.addEventListener("click", () => void stopTurn(), { signal });
  // Delegated copy: copy the raw turn text (not the rendered HTML).
  els.log.addEventListener(
    "click",
    (ev) => {
      const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>(
        "button.chat-copy",
      );
      if (!btn) return;
      const turn = turns.find((t) => t.id === Number(btn.dataset.copy));
      if (!turn) return;
      void navigator.clipboard?.writeText(turn.text).then(
        () => {
          btn.textContent = "Copied";
          window.setTimeout(() => {
            if (btn.isConnected) btn.textContent = "Copy";
          }, 1500);
        },
        () => opts.note("error", "chat: copy failed"),
      );
    },
    { signal },
  );

  render();
  updateControls();

  return {
    agent: opts.agent,
    onChunk,
    onImage,
    onTurnEnd,
    setConnected,
    isConnected: () => connected,
    render,
    // `attachEl` was inserted into the caller's static form DOM (not owned by
    // `els`), so it must be pulled back out — an agent console re-binds a fresh
    // panel to the same `<form>` each time it opens, and a leftover node would
    // both linger visibly and accumulate a duplicate every re-open.
    dispose: () => {
      ac.abort();
      attachEl.remove();
    },
  };
}
