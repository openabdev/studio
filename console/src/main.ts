import { defaultSource } from "./source";
import {
  renderRoster,
  renderIdentity,
  renderFleetConfig,
  renderRemote,
  filterByMembers,
} from "./render";
import type { FleetConfig, RemoteConfig } from "./types";
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

async function tick(): Promise<void> {
  if (!roster) return;
  try {
    const all = await source.listDeployments(activeCluster);
    // Filter to the active fleet's members (empty ⇒ whole cluster).
    const deployments = filterByMembers(all, activeMembers);
    renderRoster(roster, deployments);
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

// ---- start / stop (ADR-2 write model: stop = scale→0, start = scale→1) -------
// Scale a deployment off (0) or on (1). Reversible — ECS keeps the Spec at
// desiredCount 0 — so this needs no state store. On success `tick()` re-renders
// the roster (which recycles the button DOM), so we only re-enable on error.
async function scale(
  action: "start" | "stop",
  name: string,
  namespace: string,
  btn: HTMLButtonElement,
): Promise<void> {
  const size = action === "start" ? 1 : 0;
  btn.disabled = true;
  try {
    await source.scaleDeployment(name, size, namespace, activeCluster);
    note("info", `${action === "start" ? "started" : "stopped"} ${namespace}/${name}`);
    await tick();
  } catch (e) {
    note("error", `${action} ${namespace}/${name}: ${errText(e)}`);
    btn.disabled = false;
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
    void scale(action, name, namespace, btn);
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
  });
}

// Boot order matters: subscribe to the log streams FIRST, then start the core,
// so the spawn → handshake → ready lifecycle lines are captured, not lost.
async function boot(): Promise<void> {
  note("info", `OAB Studio ${BUILD} (built ${__BUILD_TIME__})`);
  if (activity && mcp) await bindBackend(activity, mcp);
  await bindRemoteStatus();
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
