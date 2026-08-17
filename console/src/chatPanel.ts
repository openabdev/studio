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
  appendChunk,
  endTurn,
  type ChatTurn,
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
  onTurnEnd(stopReason: string): void;
  setConnected(connected: boolean): void;
  isConnected(): boolean;
  render(): void;
  dispose(): void;
}

// Untrusted agent markdown → HTML: markdown-it escapes raw HTML and blocks
// dangerous link protocols; DOMPurify is the second layer (ADR: markdown-it +
// DOMPurify). Only the agent-markdown body takes this — user text and the
// panel's own chrome are escaped/trusted in `chat.ts`.
function renderAgentBody(text: string): string {
  return DOMPurify.sanitize(mdToHtml(text));
}

export function createChatPanel(
  els: ChatPanelElements,
  opts: ChatPanelOptions,
): ChatPanel {
  const notReady = opts.notReadyLabel ?? "activate the remote connection to chat";
  // Per-panel transcript state (was module-global in main.ts's single console).
  let turns: ChatTurn[] = [];
  let turnActive = false;
  const queue: string[] = [];
  let seq = 0;
  let connected = false;
  // DOM listeners are scoped to this controller so `dispose()` removes them all
  // at once — an agent console mounts a fresh panel each time it opens.
  const ac = new AbortController();
  const { signal } = ac;

  function errText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  // Chat is usable once the connection is live (or always, in the mock).
  function ready(): boolean {
    return opts.mock || connected;
  }

  function render(): void {
    els.log.innerHTML = transcriptHtml(turns, renderAgentBody);
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
  }

  // Enqueue a prompt and try to release it. `flush` is the single choke point
  // that enforces one-turn-at-a-time; typing mid-turn just grows the queue.
  function submit(text: string): void {
    const trimmed = text.trim();
    if (!trimmed) return;
    queue.push(trimmed);
    void flush();
  }

  async function flush(): Promise<void> {
    if (turnActive) return; // a turn is in flight — wait for its `turn_end`
    const next = queue.shift();
    if (next === undefined) return;
    turnActive = true;
    seq += 1;
    turns = appendUser(turns, seq, next);
    render();
    updateControls();
    try {
      await opts.source.agentPrompt(next, opts.agent);
      if (opts.mock) mockReply(next); // browser preview: synthesize the reply
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
    onTurnEnd,
    setConnected,
    isConnected: () => connected,
    render,
    dispose: () => ac.abort(),
  };
}
