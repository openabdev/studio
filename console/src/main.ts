import { defaultSource } from "./source";
import {
  renderRoster,
  renderIdentity,
  renderFleetConfig,
  renderRemote,
  filterByMembers,
  deploymentKey,
} from "./render";
import type { Deployment, FleetConfig, RemoteConfig } from "./types";
import {
  transcriptHtml,
  mdToHtml,
  appendUser,
  appendChunk,
  endTurn,
  type ChatTurn,
} from "./chat";
import DOMPurify from "dompurify";
import { createPane, bindBackend, type Level } from "./log";
import { EditorView, basicSetup } from "codemirror";
import { EditorState } from "@codemirror/state";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";

const POLL_MS = 5000;
const DEFAULT_CLUSTER = "oab";

// The selection is a fleet **identity** (name), not a cluster — a fleet is a
// usage-based group, so two fleets can share a cluster. The active fleet derives
// the `activeCluster` every read targets (and thus, via oab-mcp's binding, which
// credential/account we manage as) and the `activeMembers` the roster is filtered
// to. `null` = no fleet selected: the default cluster, roster unfiltered.
// Selecting a fleet in the config panel is the "switch" step of the ADR #19 loop.
let activeFleet: string | null = null;
let activeCluster = DEFAULT_CLUSTER;
let activeMembers: string[] = [];
let fleetConfig: FleetConfig | null = null;
let remoteConfig: RemoteConfig | null = null;

const roster = document.getElementById("roster");
const identityEl = document.getElementById("identity");
const configEl = document.getElementById("config");
const remoteEl = document.getElementById("remote");
const editorSection = document.getElementById("config-editor");
const editorMount = document.getElementById("cfg-editor-mount");
const editorError = document.getElementById("cfg-editor-error");
const editorPathEl = document.getElementById("cfg-editor-path");
const editorTitleEl = document.getElementById("cfg-editor-title");
const saveBtn = document.getElementById("cfg-save") as HTMLButtonElement | null;
const cancelBtn = document.getElementById("cfg-cancel") as HTMLButtonElement | null;
const clusterLabel = document.getElementById("cluster-label");
const pollStatus = document.getElementById("poll-status");
const logEl = document.getElementById("log");
const mcpEl = document.getElementById("mcpio");
const chatLogEl = document.getElementById("chat-log");
const chatFormEl = document.getElementById("chat-form") as HTMLFormElement | null;
const chatTextEl = document.getElementById("chat-text") as HTMLTextAreaElement | null;
const chatSendEl = document.getElementById("chat-send") as HTMLButtonElement | null;
const chatStopEl = document.getElementById("chat-stop") as HTMLButtonElement | null;
const chatConnEl = document.getElementById("chat-conn");
const source = defaultSource();

// Two tabs, one pane each: Activity (lifecycle + failures) and MCP (the raw
// oab-mcp JSON-RPC interaction). `data-target` links a tab to its pane id.
const tabs = Array.from(
  document.querySelectorAll<HTMLButtonElement>("#tabs .tab"),
);
let activeTarget = tabs.find((t) => t.classList.contains("is-active"))?.dataset
  .target;

function show(target: string): void {
  activeTarget = target;
  for (const tab of tabs) {
    const on = tab.dataset.target === target;
    tab.classList.toggle("is-active", on);
    if (on) tab.classList.remove("has-new");
    const pane = document.getElementById(tab.dataset.target ?? "");
    if (pane) pane.hidden = !on;
  }
}
for (const tab of tabs) {
  tab.addEventListener("click", () => show(tab.dataset.target ?? ""));
}

// Flag a tab when its (hidden) pane gets new lines, so nothing is missed.
function flag(target: string): void {
  if (target === activeTarget) return;
  tabs.find((t) => t.dataset.target === target)?.classList.add("has-new");
}

const activity = logEl ? createPane(logEl, () => flag("log")) : null;
const mcp = mcpEl ? createPane(mcpEl, () => flag("mcpio")) : null;

// Build stamp (injected by vite) — shown under the brand and logged on launch,
// so it's obvious which commit this build is.
const BUILD = `v${__APP_VERSION__} · ${__BUILD_SHA__}`;
const buildEl = document.getElementById("build-info");
if (buildEl) {
  buildEl.textContent = BUILD;
  buildEl.title = `built ${__BUILD_TIME__}`;
}

function note(level: Level, msg: string): void {
  activity?.push({ cls: `lv-${level}`, tag: level.toUpperCase(), msg });
}

// Tauri command rejections arrive as plain strings, not Error objects.
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

let lastError = "";

// ---- in-flight scale guard --------------------------------------------------
// Deployments with a scale in flight (or awaiting the observed desiredCount
// flip), keyed by `deploymentKey` → the count we're driving toward. Held in
// module state (not on the button DOM) so the 5s poll's re-render can't wash out
// the disabled guard. Pruned when the roster observes the target count, or by a
// safety timeout so a never-observed flip can't wedge a button forever.
const scaling = new Map<string, number>();
const scaleTimers = new Map<string, number>();
let lastDeployments: Deployment[] = [];
const SCALE_MAX_HOLD_MS = 15000;

function pendingKeys(): ReadonlySet<string> {
  return new Set(scaling.keys());
}

function clearPending(key: string): void {
  scaling.delete(key);
  const timer = scaleTimers.get(key);
  if (timer !== undefined) {
    window.clearTimeout(timer);
    scaleTimers.delete(key);
  }
}

// Drop the guard for any deployment whose observed desiredCount has reached the
// target we drove toward — the action landed, so its button re-enables.
function prunePending(deployments: Deployment[]): void {
  for (const d of deployments) {
    const key = deploymentKey(d);
    if (scaling.get(key) === d.desired) clearPending(key);
  }
}

// Re-render the roster from the last poll's data with the current pending
// overlay — instant feedback on click, no fetch needed.
function repaintRoster(): void {
  if (roster) renderRoster(roster, lastDeployments, pendingKeys());
}

async function tick(): Promise<void> {
  if (!roster) return;
  try {
    const all = await source.listDeployments(activeCluster);
    // Filter to the active fleet's members (empty ⇒ whole cluster).
    const deployments = filterByMembers(all, activeMembers);
    lastDeployments = deployments;
    prunePending(deployments);
    renderRoster(roster, deployments, pendingKeys());
    if (lastError) {
      note("info", `roster recovered — ${deployments.length} deployment(s)`);
      lastError = "";
    }
    if (pollStatus) {
      pollStatus.textContent = `updated ${new Date().toLocaleTimeString()}`;
      pollStatus.classList.remove("err");
    }
  } catch (e) {
    const msg = errText(e);
    if (msg !== lastError) {
      note("error", `roster: ${msg}`);
      lastError = msg;
    }
    if (pollStatus) {
      pollStatus.textContent = `error: ${msg}`;
      pollStatus.classList.add("err");
    }
  }
}

// The effective managing identity for this cluster (ADR #19). Fetched once on
// boot and refreshed when the roster recovers — it changes rarely, so it does
// not need the 5s poll (and each call is a live STS lookup server-side).
async function refreshIdentity(): Promise<void> {
  if (!identityEl) return;
  try {
    renderIdentity(identityEl, await source.runtimeContext(activeCluster));
  } catch (e) {
    note("error", `identity: ${errText(e)}`);
    renderIdentity(identityEl, null);
  }
}

// The fleet-binding config panel (ADR #19 "declare"). Fetched once on boot; the
// bindings are read at core startup, so they don't change under us at runtime.
async function refreshConfig(): Promise<void> {
  if (!configEl) return;
  try {
    fleetConfig = await source.fleetConfig();
    renderFleetConfig(configEl, fleetConfig, activeFleet);
  } catch (e) {
    note("error", `fleet config: ${errText(e)}`);
    fleetConfig = null;
    renderFleetConfig(configEl, null, activeFleet);
  }
}

// The remote reverse-MCP connection panel (Part B). Fetched on boot; the live
// status is also pushed via the `remote-status` event (see boot), so this is the
// initial render + a refresh after an activate/deactivate/save action.
async function refreshRemote(): Promise<void> {
  if (!remoteEl) return;
  try {
    remoteConfig = await source.remoteConfig();
    renderRemote(remoteEl, remoteConfig);
  } catch (e) {
    note("error", `remote config: ${errText(e)}`);
    remoteConfig = null;
    renderRemote(remoteEl, null);
  }
}

// Switch the active fleet by identity (name): re-point every read at its cluster
// (and thus its bound credential), filter the roster to its members, and refresh
// immediately — so "switch fleet" == "switch managing account + roster" the ADR
// calls for. Two fleets may share a cluster, so the key is the name, not the
// cluster. No-op if it's already active or unknown.
function selectFleet(name: string): void {
  if (!name || name === activeFleet) return;
  const fleet = fleetConfig?.fleets.find((f) => f.name === name);
  if (!fleet) return;
  activeFleet = name;
  activeCluster = fleet.cluster;
  activeMembers = fleet.members;
  if (clusterLabel) clusterLabel.textContent = `${activeFleet} · ${activeCluster}`;
  note("info", `switched to fleet "${activeFleet}" (cluster "${activeCluster}")`);
  if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
  void refreshIdentity();
  void tick();
}

// ---- TOML editor (fleets.toml + remote.toml) ---------------------------------
// One CodeMirror TOML editor, shared by both config files (which one is set by
// `editorTarget`). Kept imperative (CM owns real DOM) and separate from the
// re-rendered panels, so a background refresh never wipes an open editor.
type EditorTarget = "fleet" | "remote";
let editorView: EditorView | null = null;
let editorTarget: EditorTarget = "fleet";

function showEditorError(msg: string | null): void {
  if (!editorError) return;
  editorError.textContent = msg ?? "";
  editorError.hidden = !msg;
}

function openEditor(target: EditorTarget): void {
  if (!editorSection || !editorMount) return;
  editorTarget = target;
  const isRemote = target === "remote";
  const doc = (isRemote ? remoteConfig?.text : fleetConfig?.text) ?? "";
  const path = (isRemote ? remoteConfig?.path : fleetConfig?.path) ?? "";
  showEditorError(null);
  if (editorTitleEl)
    editorTitleEl.textContent = isRemote ? "edit remote.toml" : "edit fleets.toml";
  if (editorPathEl) editorPathEl.textContent = path;
  editorView?.destroy();
  editorView = new EditorView({
    parent: editorMount,
    state: EditorState.create({
      doc,
      extensions: [basicSetup, StreamLanguage.define(toml)],
    }),
  });
  editorSection.hidden = false;
  editorView.focus();
}

function closeEditor(): void {
  editorView?.destroy();
  editorView = null;
  if (editorSection) editorSection.hidden = true;
  showEditorError(null);
}

async function saveEditor(): Promise<void> {
  if (!editorView || !saveBtn) return;
  const text = editorView.state.doc.toString();
  saveBtn.disabled = true;
  showEditorError(null);
  try {
    // The backend validates the TOML and rejects (without writing) on error.
    if (editorTarget === "remote") {
      remoteConfig = await source.writeRemoteConfig(text);
      if (remoteEl) renderRemote(remoteEl, remoteConfig);
      note("info", "remote config saved");
      closeEditor();
    } else {
      fleetConfig = await source.writeFleetConfig(text);
      if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
      note("info", "fleet config saved");
      closeEditor();
      // A binding change may alter the active fleet's credential — re-observe.
      void refreshIdentity();
    }
  } catch (e) {
    showEditorError(`save failed — ${errText(e)}`);
  } finally {
    saveBtn.disabled = false;
  }
}

saveBtn?.addEventListener("click", () => void saveEditor());
cancelBtn?.addEventListener("click", () => closeEditor());

// One delegated listener on the config panel: "Edit config" opens the editor;
// a click on any fleet button switches to that fleet by name.
if (configEl) {
  configEl.addEventListener("click", (ev) => {
    const target = ev.target as HTMLElement;
    if (target.closest('[data-action="edit-config"]')) {
      openEditor("fleet");
      return;
    }
    const btn = target.closest<HTMLElement>("[data-fleet]");
    if (btn?.dataset.fleet) selectFleet(btn.dataset.fleet);
  });
}

// The remote panel: "Edit config" opens remote.toml in the editor; "Activate"
// dials the /acp endpoint; "Disconnect" tears it down. Status then updates via
// the `remote-status` event, with a refresh as a fallback.
async function remoteAction(kind: "connect" | "disconnect"): Promise<void> {
  try {
    if (kind === "connect") {
      await source.remoteConnect();
      note("info", "activating remote connection…");
    } else {
      await source.remoteDisconnect();
      note("info", "remote connection deactivated");
    }
  } catch (e) {
    note("error", `remote ${kind}: ${errText(e)}`);
  }
  void refreshRemote();
}

if (remoteEl) {
  remoteEl.addEventListener("click", (ev) => {
    const target = ev.target as HTMLElement;
    if (target.closest('[data-action="edit-remote-config"]')) {
      openEditor("remote");
    } else if (target.closest('[data-action="remote-connect"]')) {
      void remoteAction("connect");
    } else if (target.closest('[data-action="remote-disconnect"]')) {
      void remoteAction("disconnect");
    }
  });
}

// ---- chat panel (Part C) -----------------------------------------------------
// The backend serves ONE turn at a time over the live `/acp` session (Part B):
// `agent_prompt` sends a turn, and the reply streams back as `agent-update`
// events (`chunk` → `turn_end`). `turnActive` gates sends; a prompt typed
// mid-turn is queued and `flushQueue` releases the next only once the current
// turn ends (the katashiro turn model). This UI-level gate means two turns never
// overlap — so the backend's in-flight guard is a safety net, not the norm.
let chatTurns: ChatTurn[] = [];
let turnActive = false;
const promptQueue: string[] = [];
let chatSeq = 0;
let remoteConnected = false;

// True in the browser build (no Tauri shell): there is no live agent, so we drive
// a canned reply locally to keep the panel demonstrable.
function isMock(): boolean {
  return tauriInvoke() === undefined;
}

// Chat is usable once the remote connection is live (or always, in the mock).
function chatReady(): boolean {
  return isMock() || remoteConnected;
}

// Untrusted agent markdown → HTML: markdown-it escapes raw HTML and blocks
// dangerous link protocols; DOMPurify is the second layer (ADR: markdown-it +
// DOMPurify). Only the agent-markdown body takes this — user text and the
// panel's own chrome are escaped/trusted in `chat.ts`.
function renderAgentBody(text: string): string {
  return DOMPurify.sanitize(mdToHtml(text));
}

function renderChat(): void {
  if (!chatLogEl) return;
  chatLogEl.innerHTML = transcriptHtml(chatTurns, renderAgentBody);
  chatLogEl.scrollTop = chatLogEl.scrollHeight; // keep the latest turn in view
}

function updateChatControls(): void {
  const ready = chatReady();
  if (chatSendEl) chatSendEl.disabled = !ready;
  if (chatTextEl) chatTextEl.disabled = !ready;
  if (chatStopEl) chatStopEl.hidden = !turnActive;
  if (chatConnEl) {
    const label = !ready
      ? "activate the remote connection to chat"
      : turnActive
        ? "agent is responding…"
        : "connected";
    chatConnEl.textContent = label;
    chatConnEl.classList.toggle("is-connected", ready && !turnActive);
    chatConnEl.classList.toggle("is-error", false);
  }
}

// Enqueue a prompt and try to release it. `flushQueue` is the single choke point
// that enforces one-turn-at-a-time; typing mid-turn just grows the queue.
function submitPrompt(text: string): void {
  const trimmed = text.trim();
  if (!trimmed) return;
  promptQueue.push(trimmed);
  void flushQueue();
}

async function flushQueue(): Promise<void> {
  if (turnActive) return; // a turn is in flight — wait for its `turn_end`
  const next = promptQueue.shift();
  if (next === undefined) return;
  turnActive = true;
  chatSeq += 1;
  chatTurns = appendUser(chatTurns, chatSeq, next);
  renderChat();
  updateChatControls();
  try {
    await source.agentPrompt(next);
    if (isMock()) mockReply(next); // browser preview: synthesize the reply
  } catch (e) {
    // Send failed (not connected / socket just closed): don't leave the panel
    // hanging on a spinner — close the turn with an error, surface it, release
    // the queue so a later (connected) prompt can still go.
    turnActive = false;
    note("error", `chat: ${errText(e)}`);
    chatSeq += 1;
    chatTurns = endTurn(chatTurns, chatSeq, "error");
    renderChat();
    updateChatControls();
    void flushQueue();
  }
}

// A streamed `chunk`: open the agent turn on the first one (stable id for its
// copy button), append thereafter.
function onAgentChunk(text: string): void {
  const last = chatTurns[chatTurns.length - 1];
  const open = last?.role === "agent" && last.streaming;
  const id = open ? (last as ChatTurn).id : (chatSeq += 1);
  chatTurns = appendChunk(chatTurns, id, text);
  renderChat();
}

// `turn_end`: finalize the open agent turn (markdown render), free the gate, and
// release any queued prompt.
function onAgentTurnEnd(stopReason: string): void {
  chatSeq += 1;
  chatTurns = endTurn(chatTurns, chatSeq, stopReason);
  turnActive = false;
  renderChat();
  updateChatControls();
  void flushQueue();
}

async function stopTurn(): Promise<void> {
  if (!turnActive) return;
  try {
    await source.agentCancel();
    note("info", "chat: cancel sent");
  } catch (e) {
    note("error", `chat cancel: ${errText(e)}`);
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
      onAgentChunk(parts[i]);
      i += 1;
      window.setTimeout(step, 130);
    } else {
      onAgentTurnEnd("end_turn");
    }
  };
  window.setTimeout(step, 150);
}

chatFormEl?.addEventListener("submit", (ev) => {
  ev.preventDefault();
  if (!chatTextEl) return;
  submitPrompt(chatTextEl.value);
  chatTextEl.value = "";
});
// Enter sends; Shift+Enter inserts a newline.
chatTextEl?.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter" && !ev.shiftKey) {
    ev.preventDefault();
    chatFormEl?.requestSubmit();
  }
});
chatStopEl?.addEventListener("click", () => void stopTurn());
// Delegated copy: copy the raw turn text (not the rendered HTML).
chatLogEl?.addEventListener("click", (ev) => {
  const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>(
    "button.chat-copy",
  );
  if (!btn) return;
  const turn = chatTurns.find((t) => t.id === Number(btn.dataset.copy));
  if (!turn) return;
  void navigator.clipboard?.writeText(turn.text).then(
    () => {
      btn.textContent = "Copied";
      window.setTimeout(() => {
        if (btn.isConnected) btn.textContent = "Copy";
      }, 1500);
    },
    () => note("error", "chat: copy failed"),
  );
});

// Subscribe to the backend's streamed chat updates (desktop only). `chunk` and
// `turn_end` drive the transcript; the browser build has no bridge and uses the
// mock reply path instead.
async function bindAgentUpdates(): Promise<void> {
  const listen = (
    globalThis as {
      __TAURI__?: {
        event?: {
          listen?: <T>(
            e: string,
            h: (e: { payload: T }) => void,
          ) => Promise<unknown>;
        };
      };
    }
  ).__TAURI__?.event?.listen;
  if (!listen) return;
  await listen<{ kind?: string; text?: string; stopReason?: string }>(
    "agent-update",
    (e) => {
      const p = e.payload;
      if (p.kind === "chunk") onAgentChunk(p.text ?? "");
      else if (p.kind === "turn_end") onAgentTurnEnd(p.stopReason ?? "end_turn");
    },
  );
}

// ---- start / stop (ADR-2 write model: stop = scale→0, start = scale→1) -------
// Scale a deployment off (0) or on (1). Reversible — ECS keeps the Spec at
// desiredCount 0 — so this needs no state store. The in-flight guard lives in
// `scaling` (module state), so the button stays disabled across poll re-renders
// until the observed desiredCount flips (or the safety timeout fires).
async function scale(
  action: "start" | "stop",
  name: string,
  namespace: string,
): Promise<void> {
  const key = `${namespace}/${name}`;
  if (scaling.has(key)) return; // already in flight — poll-immune re-entry guard
  const size = action === "start" ? 1 : 0;
  scaling.set(key, size);
  scaleTimers.set(
    key,
    window.setTimeout(() => {
      clearPending(key);
      repaintRoster();
    }, SCALE_MAX_HOLD_MS),
  );
  repaintRoster(); // disable the button immediately
  try {
    await source.scaleDeployment(name, size, namespace, activeCluster);
    note("info", `${action === "start" ? "started" : "stopped"} ${namespace}/${name}`);
    // tick() observes the new desiredCount and prunes the guard when it flips.
    await tick();
  } catch (e) {
    note("error", `${action} ${namespace}/${name}: ${errText(e)}`);
    clearPending(key);
    repaintRoster();
  }
}

// One delegated listener on the roster. Start executes on click; Stop is
// disruptive (kills the running instance, though reversible), so it arms on the
// first click and only executes on a confirming second click within 3s — a
// webview-safe confirm that needs no dialog plugin. The 5s poll re-renders the
// roster and would reset an armed button on its own; the 3s timer is tighter.
if (roster) {
  roster.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest<HTMLButtonElement>("button.act");
    if (!btn) return;
    const action = btn.dataset.action;
    const { name, namespace } = btn.dataset;
    if ((action !== "start" && action !== "stop") || !name || !namespace) return;
    if (action === "stop" && btn.dataset.armed !== "1") {
      btn.dataset.armed = "1";
      btn.textContent = "Confirm stop";
      btn.classList.add("armed");
      window.setTimeout(() => {
        if (btn.isConnected && btn.dataset.armed === "1") {
          btn.dataset.armed = "";
          btn.textContent = "Stop";
          btn.classList.remove("armed");
        }
      }, 3000);
      return;
    }
    void scale(action, name, namespace);
  });
}

// The Tauri command bridge — present only inside the desktop shell (the browser
// build has no `__TAURI__`, so callers no-op / hide their UI).
type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
function tauriInvoke(): Invoke | undefined {
  return (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__?.core
    ?.invoke;
}

// Ask the backend to start the core — only meaningful inside the Tauri shell.
async function startCore(): Promise<void> {
  const invoke = tauriInvoke();
  if (!invoke) return; // browser build — MockSource, no core
  try {
    await invoke("start_core");
  } catch (e) {
    note("error", `start_core: ${errText(e)}`);
  }
}

// Remote upgrade: the topbar "Check for updates" button. First click checks the nightly
// release; if a newer signed build exists, the button turns into an install
// action that downloads, verifies, and restarts into it. Desktop-only — hidden
// in the browser build (no command bridge).
interface UpdateInfo {
  version: string;
  current: string;
  notes: string | null;
}
function setupUpdater(): void {
  const el = document.getElementById("update-btn") as HTMLButtonElement | null;
  const invoke = tauriInvoke();
  if (!el || !invoke) return; // browser build — no updater
  el.hidden = false;
  let pending: UpdateInfo | null = null;

  const reset = (): void => {
    pending = null;
    el.textContent = "Check for updates";
    el.classList.remove("has-update");
  };

  async function check(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "Checking…";
    try {
      const info = await inv<UpdateInfo | null>("check_update");
      if (info) {
        pending = info;
        btn.textContent = `Update to v${info.version} ↻`;
        btn.classList.add("has-update");
        note("info", `New version v${info.version} available (current v${info.current}) — click to install and restart`);
      } else {
        note("info", "Already up to date");
        btn.textContent = "Up to date";
        window.setTimeout(reset, 4000);
      }
    } catch (e) {
      reset();
      note("error", `Update check failed: ${errText(e)}`);
    } finally {
      btn.disabled = false;
    }
  }

  async function install(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "Installing…";
    try {
      // On success the backend restarts the app, so this may never resolve.
      await inv("install_update");
    } catch (e) {
      btn.disabled = false;
      btn.textContent = pending ? `Update to v${pending.version} ↻` : "Check for updates";
      note("error", `Update install failed: ${errText(e)}`);
    }
  }

  el.addEventListener("click", () => void (pending ? install(el, invoke) : check(el, invoke)));
}

// Live remote-connection status: the backend pushes `remote-status` events as the
// transport connects / drops / errors, so the panel reflects the real state
// without polling. Browser build (no `__TAURI__`) simply skips it.
async function bindRemoteStatus(): Promise<void> {
  interface EventGlobal {
    event?: {
      listen?: <T>(
        e: string,
        h: (e: { payload: T }) => void,
      ) => Promise<unknown>;
    };
  }
  const listen = (globalThis as { __TAURI__?: EventGlobal }).__TAURI__?.event
    ?.listen;
  if (!listen) return;
  await listen<{ status: string }>("remote-status", (e) => {
    const status = e.payload?.status ?? "disconnected";
    if (remoteConfig) {
      remoteConfig = { ...remoteConfig, status };
      if (remoteEl) renderRemote(remoteEl, remoteConfig);
    }
    remoteConnected = status === "connected";
    // If the socket drops mid-turn, no `turn_end` will arrive — close the open
    // turn so the panel doesn't hang on a spinner.
    if (!remoteConnected && turnActive) onAgentTurnEnd(status);
    updateChatControls();
  });
}

// Boot order matters: subscribe to the log streams FIRST, then start the core,
// so the spawn → handshake → ready lifecycle lines are captured, not lost.
async function boot(): Promise<void> {
  note("info", `OAB Studio ${BUILD} (built ${__BUILD_TIME__})`);
  if (activity && mcp) await bindBackend(activity, mcp);
  await bindRemoteStatus();
  await bindAgentUpdates();
  renderChat();
  updateChatControls();
  if (clusterLabel) clusterLabel.textContent = activeCluster;
  note("info", `polling cluster "${activeCluster}" every ${POLL_MS / 1000}s`);
  setupUpdater();
  await startCore();
  void refreshConfig();
  void refreshIdentity();
  void refreshRemote();
  void tick();
  window.setInterval(() => void tick(), POLL_MS);
}

void boot();
