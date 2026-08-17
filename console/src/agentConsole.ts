// The agent-console shell (ADR agent-consoles, Part C): an endpoint selector +
// a per-agent console that dials the endpoint, shows its read-only config, and
// mounts the reusable chat primitive bound to that agent. The operator opens one
// console at a time (ADR §7 bounded fan-out), so this owns a single open console
// and a clean open→dial / close→teardown lifecycle.
//
// It stays out of the transcript/turn machinery — that is `chatPanel.ts`. Its
// job is selection, the dial/teardown, the read-only config header, and
// registering the mounted panel in the shared event-router map so `main.ts` can
// route this agent's `agent-update` / `remote-status` events to it. The remote
// file editor (view/edit/apply) is a later slice; config is read-only here.

import type { Source } from "./source";
import type { AgentEndpointView } from "./types";
import { createChatPanel, type ChatPanel } from "./chatPanel";
import { createFileBrowser, type FileBrowser } from "./fileBrowser";
import { renderAgentList, agentConsoleHeaderHtml } from "./render";

export interface AgentConsoleConfig {
  source: Source;
  // Browser build (no live agent) → the mounted panel drives a canned reply.
  mock: boolean;
  note: (level: "info" | "error", msg: string) => void;
  // The shared event-router map (keyed by endpoint name) `main.ts` dispatches
  // backend events through. The open console registers/unregisters its panel here.
  panels: Map<string, ChatPanel>;
  // The management endpoint's name (resolved async in `main.ts`) — read lazily so
  // the console never tries to re-key a name the management console owns.
  managementName: () => string | null;
}

export interface AgentConsole {
  // Re-fetch the registry and re-render the selector.
  refresh(): Promise<void>;
  // Reflect a live `remote-status` on the open console's badge (no-op otherwise).
  onStatus(agent: string, status: string): void;
  dispose(): void;
}

export function initAgentConsole(cfg: AgentConsoleConfig): AgentConsole {
  const listEl = document.getElementById("agent-list");
  const consoleEl = document.getElementById("agent-console");
  const configEl = document.getElementById("ac-config");
  const log = document.getElementById("ac-chat-log");
  const form = document.getElementById("ac-chat-form") as HTMLFormElement | null;
  const text = document.getElementById("ac-chat-text") as HTMLTextAreaElement | null;
  const send = document.getElementById("ac-chat-send") as HTMLButtonElement | null;
  const stop = document.getElementById("ac-chat-stop") as HTMLButtonElement | null;
  const conn = document.getElementById("ac-chat-conn");
  const fbList = document.getElementById("ac-files-list");
  const fbViewer = document.getElementById("ac-files-viewer");
  const fbTitle = document.getElementById("ac-files-title");

  const noop: AgentConsole = {
    refresh: async () => {},
    onStatus: () => {},
    dispose: () => {},
  };
  if (!listEl || !consoleEl) return noop;

  let agents: AgentEndpointView[] = [];
  let openName: string | null = null;
  let panel: ChatPanel | null = null;
  let fileBrowser: FileBrowser | null = null;
  const ac = new AbortController();
  const { signal } = ac;

  function errText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function find(name: string): AgentEndpointView | undefined {
    return agents.find((a) => a.name === name);
  }

  function renderList(): void {
    if (listEl) renderAgentList(listEl, agents, openName);
  }

  function renderHeader(status: string): void {
    const a = openName ? find(openName) : undefined;
    if (!configEl || !a) return;
    configEl.innerHTML = agentConsoleHeaderHtml(a, status);
  }

  async function refresh(): Promise<void> {
    try {
      agents = await cfg.source.remoteAgents();
    } catch (e) {
      agents = [];
      cfg.note("error", `agents: registry load failed — ${errText(e)}`);
    }
    renderList();
    // Keep an open console's header status in sync with the refreshed registry.
    if (openName) renderHeader(find(openName)?.status ?? "disconnected");
  }

  // Tear down the open console: dispose its panel, drop it from the router, hide
  // the section, and disconnect the endpoint. Safe to call with nothing open.
  function close(): void {
    if (!openName) return;
    const name = openName;
    openName = null;
    panel?.dispose();
    panel = null;
    fileBrowser?.dispose();
    fileBrowser = null;
    cfg.panels.delete(name);
    if (consoleEl) consoleEl.hidden = true;
    // Fire-and-forget teardown; a failed disconnect is logged, not fatal.
    void cfg.source.remoteDisconnect(name).catch((e) => {
      cfg.note("error", `agents: disconnect ${name} failed — ${errText(e)}`);
    });
    cfg.note("info", `agents: closed console for "${name}"`);
    renderList();
  }

  // Open a console for a named endpoint: teardown any current one, mount a chat
  // panel bound to this agent, render its read-only config, and dial the
  // endpoint. The management endpoint is never opened here (it has its own
  // console); unconfigured endpoints are not dialable.
  async function open(name: string): Promise<void> {
    if (name === openName) return;
    if (name === cfg.managementName()) return; // has its own top-level console
    const a = find(name);
    if (!a || !a.configured) return;
    close();
    openName = name;
    if (consoleEl) consoleEl.hidden = false;
    // Optimistic "connecting" until the first `remote-status` (or ready in mock).
    renderHeader(cfg.mock ? "connected" : "connecting");
    renderList();
    if (log && form && text && send && stop && conn) {
      panel = createChatPanel(
        { log, form, text, send, stop, conn },
        {
          agent: name,
          source: cfg.source,
          mock: cfg.mock,
          note: cfg.note,
          notReadyLabel: "connecting to the agent…",
        },
      );
      cfg.panels.set(name, panel);
    }
    // Mount the read-only file browser for this agent (Part D). It probes fs
    // capability itself and shows a "pending the fs MCP files server" placeholder
    // when the endpoint has no fs support — which is every real endpoint today.
    if (fbList && fbViewer && fbTitle) {
      fileBrowser = createFileBrowser(
        { list: fbList, viewer: fbViewer, title: fbTitle },
        { agent: name, source: cfg.source, note: cfg.note },
      );
    }
    try {
      await cfg.source.remoteConnect(name);
      cfg.note("info", `agents: opened console for "${name}" — dialing ${a.url}`);
    } catch (e) {
      cfg.note("error", `agents: connect ${name} failed — ${errText(e)}`);
      renderHeader(`error: ${errText(e)}`);
    }
  }

  function onStatus(agent: string, status: string): void {
    // Update the cached entry so the selector badge reflects it too.
    const a = find(agent);
    if (a) a.status = status;
    renderList();
    if (agent === openName) renderHeader(status);
  }

  // Delegated: a selector row opens its endpoint; the console's Close tears down.
  listEl.addEventListener(
    "click",
    (ev) => {
      const btn = (ev.target as HTMLElement).closest<HTMLElement>("[data-agent]");
      if (btn?.dataset.agent) void open(btn.dataset.agent);
    },
    { signal },
  );
  consoleEl.addEventListener(
    "click",
    (ev) => {
      if ((ev.target as HTMLElement).closest('[data-action="close-console"]')) {
        close();
      }
    },
    { signal },
  );

  void refresh();

  return {
    refresh,
    onStatus,
    dispose: () => {
      close();
      ac.abort();
    },
  };
}
