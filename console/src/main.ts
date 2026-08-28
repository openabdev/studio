import { defaultSource } from "./source";
import { initConfigTab } from "./config";
import {
  renderRoster,
  renderFleetConfig,
  renderFleetDetailHeader,
  renderRemote,
  filterByMembers,
  deploymentKey,
} from "./render";
import type {
  Deployment,
  FleetConfig,
  RegistryConfig,
  RemoteConfig,
} from "./types";
import { createChatPanel, type ChatPanel } from "./chatPanel";
import { initAgentConsole, type AgentConsole } from "./agentConsole";
import { initDeployPanel, type DeployPanelHandle, type DeployedInfo } from "./deploy";
import { createPane, bindBackend, paneText, type Level } from "./log";
import { initThemeToggle } from "./theme";
import { initSplitPane } from "./splitPane";
import { initRowResize, applySavedRowHeight } from "./rowResize";
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
// The registry file (`agents.toml`) as the editor sees it — loaded lazily the
// first time "Edit config" opens it, and refreshed after a save.
let registryConfig: RegistryConfig | null = null;
let agentConsole: AgentConsole | null = null;

const roster = document.getElementById("roster");
const configEl = document.getElementById("config");
const fleetDetailEl = document.getElementById("fleet-detail");
const fdHeaderEl = document.getElementById("fd-header");
const remoteEl = document.getElementById("remote");
const drilldownRowEl = document.getElementById("drilldown-row");
const rowResizerEl = document.getElementById("row-resizer");
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

// The Debug drawer (ADR #83 Part D / slice 6, 7.6): Activity/MCP/Config
// collapsed off the top-level nav into one secondary panel, opened by any
// `[⚙]`. Internally it keeps the same three-tab switch the top-level strip
// used to have — `data-target` still links a tab to its pane id.
const debugDrawer = document.getElementById("debug-drawer");
const debugTitleEl = document.getElementById("debug-title");
const debugTabs = Array.from(
  document.querySelectorAll<HTMLButtonElement>("#debug-tabs .tab"),
);
let debugActiveTarget = debugTabs.find((t) =>
  t.classList.contains("is-active"),
)?.dataset.target;

function showDebugTab(target: string): void {
  debugActiveTarget = target;
  for (const tab of debugTabs) {
    const on = tab.dataset.target === target;
    tab.classList.toggle("is-active", on);
    if (on) tab.classList.remove("has-new");
    const pane = document.getElementById(tab.dataset.target ?? "");
    if (pane) pane.hidden = !on;
  }
}
for (const tab of debugTabs) {
  tab.addEventListener("click", () => showDebugTab(tab.dataset.target ?? ""));
}

// Flag a debug tab when its (hidden) pane gets new lines, so nothing is missed
// even while the drawer itself is closed.
function flag(target: string): void {
  if (target === debugActiveTarget && debugDrawer && !debugDrawer.hidden) return;
  debugTabs.find((t) => t.dataset.target === target)?.classList.add("has-new");
}

function openDebugDrawer(scopeLabel: string): void {
  if (!debugDrawer) return;
  if (debugTitleEl) debugTitleEl.textContent = `Debug: ${scopeLabel}`;
  debugDrawer.hidden = false;
}

function closeDebugDrawer(): void {
  if (!debugDrawer) return;
  debugDrawer.hidden = true;
}

document
  .getElementById("debug-close")
  ?.addEventListener("click", closeDebugDrawer);

// studio#119: "download log" for whichever Debug drawer tab is active —
// the MCP tab in particular can push many lines a second, making manual
// copy-paste out of the scrolling pane impractical.
document.getElementById("log-download")?.addEventListener("click", () => {
  const paneEl = debugActiveTarget === "mcpio" ? mcpEl : debugActiveTarget === "log" ? logEl : null;
  if (!paneEl) return;
  const blob = new Blob([paneText(paneEl)], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `studio-${debugActiveTarget}-${new Date().toISOString().replace(/[:.]/g, "-")}.log`;
  a.click();
  URL.revokeObjectURL(url);
});

// Global entry point (top bar, right of "Check for updates") — opens the
// same drawer the per-fleet `[⚙]` buttons do, scoped to whichever cluster is
// currently active rather than a specific fleet, so it's reachable without
// drilling into a fleet first.
document.getElementById("global-debug-btn")?.addEventListener("click", () => {
  openDebugDrawer(clusterLabel?.textContent || "studio");
});

const activity = logEl ? createPane(logEl, () => flag("log")) : null;
const mcp = mcpEl ? createPane(mcpEl, () => flag("mcpio")) : null;

// Activity verbosity: INFO+ (default) hides DEBUG lines (e.g. keepalives) via the
// `min-info` class; DEBUG+ shows everything. Persisted so the choice sticks.
(function setupLogLevel(): void {
  const btn = document.getElementById("log-level");
  if (!btn || !logEl) return;
  const KEY = "oab-studio.logdebug";
  let showDebug = false;
  try {
    showDebug = localStorage.getItem(KEY) === "1";
  } catch {
    /* storage unavailable — default to INFO+ */
  }
  const apply = (): void => {
    logEl.classList.toggle("min-info", !showDebug);
    btn.textContent = showDebug ? "DEBUG+" : "INFO+";
  };
  apply();
  btn.addEventListener("click", () => {
    showDebug = !showDebug;
    try {
      localStorage.setItem(KEY, showDebug ? "1" : "0");
    } catch {
      /* storage unavailable — the choice still applies this session */
    }
    apply();
  });
})();

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
      note("info", `roster: recovered — ${deployments.length} deployment(s)`);
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

// The fleet-binding config panel (ADR #19 "declare"). Fetched once on boot; the
// bindings are read at core startup, so they don't change under us at runtime.
async function refreshConfig(): Promise<void> {
  if (!configEl) return;
  try {
    fleetConfig = await source.fleetConfig();
    renderFleetConfig(configEl, fleetConfig, activeFleet);
  } catch (e) {
    note("error", `config: fleet load failed — ${errText(e)}`);
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
    // The panel labels the file "Edit config" opens — the registry
    // (`agents.toml`), not the deprecated `remote.toml`. Load it best-effort so a
    // registry read error never blanks the connection panel; a later edit/save
    // refreshes the cache.
    if (!registryConfig) {
      try {
        registryConfig = await source.registryConfig();
      } catch {
        /* no path label until it loads — better than mislabelling remote.toml */
      }
    }
    renderRemote(remoteEl, remoteConfig, registryConfig?.path);
  } catch (e) {
    note("error", `config: remote load failed — ${errText(e)}`);
    remoteConfig = null;
    renderRemote(remoteEl, null, registryConfig?.path);
  }
}

// The console's top-level drill-down (ADR #83 Part A): Fleets ↔ Fleet detail.
// `activeFleet === null` shows the Fleets screen (the `#config` list); a
// selected fleet shows Fleet detail (breadcrumb header + the members roster,
// with each member drilling further into its Agent console — slice 3) instead.
// Remote lives in the persistent side column — it stays visible at every
// depth, same as Agent chat, since it's the connection status backing that
// same persistent chat.
function updateScreen(): void {
  if (configEl) configEl.hidden = activeFleet !== null;
  if (fleetDetailEl) fleetDetailEl.hidden = activeFleet === null;
  if (activeFleet && fdHeaderEl) renderFleetDetailHeader(fdHeaderEl, activeFleet);
}

// Leaving the fleet (back to Fleets, or switching to another one) also leaves
// any open Agent console — reuses the console's own wired Close button (see
// `openAgentForRow`'s note on why: `agentConsole.ts` stays untouched).
function closeOpenAgentConsole(): void {
  document
    .querySelector<HTMLButtonElement>('#agent-console [data-action="close-console"]')
    ?.click();
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
  closeOpenAgentConsole();
  activeFleet = name;
  activeCluster = fleet.cluster;
  activeMembers = fleet.members;
  if (clusterLabel) clusterLabel.textContent = `${activeFleet} · ${activeCluster}`;
  note("info", `config: switched to fleet "${activeFleet}" (cluster "${activeCluster}")`);
  if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
  updateScreen();
  void tick();
}

// The Fleet detail screen's "← Fleets" breadcrumb: back to the Fleets list,
// unfiltered roster (whole default cluster) until another fleet is picked.
function deselectFleet(): void {
  if (!activeFleet) return;
  closeOpenAgentConsole();
  activeFleet = null;
  activeCluster = DEFAULT_CLUSTER;
  activeMembers = [];
  if (clusterLabel) clusterLabel.textContent = activeCluster;
  note("info", "config: back to Fleets");
  if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
  updateScreen();
  void tick();
}

// After a deploy panel run succeeds (deploy_provision + fleets.toml write both
// landed — `deploy.ts` guarantees that ordering): re-read fleets.toml, then
// either land on the new fleet's detail screen (7.5.1 step 4) or, if we're
// already looking at the fleet an instance was just added to, just refresh its
// member filter + roster in place (7.5.2 step 4).
async function handleDeployed(info: DeployedInfo): Promise<void> {
  note("info", `deploy: ${info.service} @ ${info.image} provisioned, fleets.toml updated (${info.fleetName})`);
  try {
    fleetConfig = await source.fleetConfig();
  } catch (e) {
    note("error", `config: fleet reload failed — ${errText(e)}`);
  }
  if (activeFleet === info.fleetName) {
    const fleet = fleetConfig?.fleets.find((f) => f.name === info.fleetName);
    if (fleet) activeMembers = fleet.members;
    if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
    void tick();
  } else {
    selectFleet(info.fleetName);
  }
}

const deployPanel: DeployPanelHandle | null = initDeployPanel({
  source,
  onDeployed: handleDeployed,
  // Deploy replaces whichever of #config/#fleet-detail is showing in the main
  // column while open (Brett: it read as a stray extra panel when it instead
  // rendered full-width below an unrelated-looking roster+chat row); closing
  // it just re-runs the same screen logic used everywhere else `activeFleet`
  // changes, rather than duplicating "which one was showing" here.
  restoreScreen: updateScreen,
});

// Fleet detail shows either the members roster or the open Agent console, never
// both (Part A: "the only navigation model," no tab-peer surfaces) — mirror
// `#agent-console`'s own `hidden` state (owned by `agentConsole.ts`, untouched)
// onto the roster rather than adding a second source of truth for what's open.
(function watchAgentConsoleVisibility(): void {
  const consoleEl = document.getElementById("agent-console");
  if (!consoleEl || !roster) return;
  const sync = (): void => {
    roster.hidden = !consoleEl.hidden;
  };
  new MutationObserver(sync).observe(consoleEl, {
    attributes: true,
    attributeFilter: ["hidden"],
  });
  sync();
})();

// ---- TOML editor (fleets.toml + remote.toml) ---------------------------------
// One CodeMirror TOML editor, shared by both config files (which one is set by
// `editorTarget`). Kept imperative (CM owns real DOM) and separate from the
// re-rendered panels, so a background refresh never wipes an open editor.
// The remote panel's editor targets the registry (`agents.toml`) — the source of
// truth since slice 1 — not the deprecated `remote.toml`.
type EditorTarget = "fleet" | "registry";
let editorView: EditorView | null = null;
let editorTarget: EditorTarget = "fleet";

function showEditorError(msg: string | null): void {
  if (!editorError) return;
  editorError.textContent = msg ?? "";
  editorError.hidden = !msg;
}

// Floors the row's height at the window's bottom edge — the default, "not
// split" mode (`.drilldown-row` without `.is-split`). While the editor is
// open, `openEditor`/`closeEditor` switch the row into a fixed, user-
// draggable height instead (`rowResize.ts`'s `applySavedRowHeight`/
// `initRowResize`) and this is what `closeEditor` restores on the way out.
function syncDrilldownHeight(): void {
  if (!drilldownRowEl) return;
  const top = drilldownRowEl.getBoundingClientRect().top;
  const h = Math.max(200, window.innerHeight - top - 16);
  drilldownRowEl.style.setProperty("--drilldown-row-h", `${h}px`);
}

// Fill the editor down to the window's bottom edge (Brett: it capped at a
// fixed height with a chunk of blank space below) instead of a flat number —
// the space above it (the Fleets row's height) isn't constant, so this is
// measured live rather than hard-coded. No-ops while the editor is closed;
// wired to `resize` once at boot below since the window is resizable.
function syncEditorHeight(): void {
  if (!editorMount || !editorSection || editorSection.hidden) return;
  const top = editorMount.getBoundingClientRect().top;
  const h = Math.max(200, window.innerHeight - top - 16);
  editorMount.style.setProperty("--cfg-editor-h", `${h}px`);
}

async function openEditor(target: EditorTarget): Promise<void> {
  if (!editorSection || !editorMount) return;
  editorTarget = target;
  showEditorError(null);
  let doc = "";
  let path = "";
  let title = "edit fleets.toml";
  if (target === "registry") {
    // Load `agents.toml` lazily (and re-read on each open, so an external edit
    // or a prior save shows up). Missing file → the backend seeds it from the
    // adopted legacy `remote.toml`, so the first save migrates it.
    try {
      registryConfig = await source.registryConfig();
    } catch (e) {
      note("error", `config: agents.toml load failed — ${errText(e)}`);
      return;
    }
    doc = registryConfig?.text ?? "";
    path = registryConfig?.path ?? "";
    title = "edit agents.toml";
  } else {
    doc = fleetConfig?.text ?? "";
    path = fleetConfig?.path ?? "";
    title = "edit fleets.toml";
  }
  if (editorTitleEl) editorTitleEl.textContent = title;
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
  // The row switches from auto-fill into a fixed, draggable height once
  // there's something below it to divide space against (Brett wanted the
  // row's own height adjustable, not just the left/right split).
  if (drilldownRowEl) {
    drilldownRowEl.classList.add("is-split");
    applySavedRowHeight(drilldownRowEl, syncEditorHeight);
  }
  if (rowResizerEl) rowResizerEl.hidden = false;
  syncEditorHeight();
  editorView.focus();
}

function closeEditor(): void {
  editorView?.destroy();
  editorView = null;
  if (editorSection) editorSection.hidden = true;
  if (rowResizerEl) rowResizerEl.hidden = true;
  if (drilldownRowEl) {
    drilldownRowEl.classList.remove("is-split");
    syncDrilldownHeight();
  }
  showEditorError(null);
}

async function saveEditor(): Promise<void> {
  if (!editorView || !saveBtn) return;
  const text = editorView.state.doc.toString();
  saveBtn.disabled = true;
  showEditorError(null);
  try {
    // The backend validates the TOML and rejects (without writing) on error.
    if (editorTarget === "registry") {
      registryConfig = await source.writeRegistryConfig(text);
      note("info", "config: agents.toml saved");
      closeEditor();
      // The registry drives both the management panel (its management entry) and
      // the agent-console selector — refresh both to reflect the edit.
      void refreshRemote();
      await agentConsole?.refresh();
    } else {
      fleetConfig = await source.writeFleetConfig(text);
      if (configEl) renderFleetConfig(configEl, fleetConfig, activeFleet);
      note("info", "config: fleet saved");
      closeEditor();
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
      void openEditor("fleet");
      return;
    }
    if (target.closest('[data-action="new-fleet"]')) {
      deployPanel?.open({ kind: "new-fleet" });
      return;
    }
    const debugBtn = target.closest<HTMLElement>('[data-action="fleet-debug"]');
    if (debugBtn) {
      openDebugDrawer(debugBtn.dataset.fleet ?? "");
      return;
    }
    const btn = target.closest<HTMLElement>("[data-fleet]");
    if (btn?.dataset.fleet) selectFleet(btn.dataset.fleet);
  });
}

// The Fleet detail header: "← Fleets" backs out; "+ Add instance" opens the
// deploy panel scoped to the active fleet (7.5.2); `⚙` opens the Debug drawer
// scoped to the active fleet (slice 6, 7.6).
if (fleetDetailEl) {
  fleetDetailEl.addEventListener("click", (ev) => {
    const target = ev.target as HTMLElement;
    if (target.closest('[data-action="back-to-fleets"]')) {
      deselectFleet();
      return;
    }
    if (target.closest('[data-action="add-instance"]') && activeFleet) {
      deployPanel?.open({ kind: "add-instance", fleetName: activeFleet });
      return;
    }
    if (target.closest('[data-action="fleet-debug"]') && activeFleet) {
      openDebugDrawer(activeFleet);
    }
  });
}

// The remote panel: "Edit config" opens remote.toml in the editor; "Activate"
// dials the /acp endpoint; "Disconnect" tears it down. Status then updates via
// the `remote-status` event, with a refresh as a fallback.
async function remoteAction(kind: "connect" | "disconnect"): Promise<void> {
  try {
    if (kind === "connect") {
      await source.remoteConnect();
      note("info", "remote: activating…");
    } else {
      await source.remoteDisconnect();
      note("info", "remote: deactivated");
    }
  } catch (e) {
    note("error", `remote: ${kind} failed — ${errText(e)}`);
  }
  void refreshRemote();
}

if (remoteEl) {
  remoteEl.addEventListener("click", (ev) => {
    const target = ev.target as HTMLElement;
    if (target.closest('[data-action="edit-remote-config"]')) {
      // Edit the registry (`agents.toml`), not the deprecated `remote.toml`.
      void openEditor("registry");
    } else if (target.closest('[data-action="remote-connect"]')) {
      void remoteAction("connect");
    } else if (target.closest('[data-action="remote-disconnect"]')) {
      void remoteAction("disconnect");
    }
  });
}

// ---- chat panels (Part C, reusable primitive) --------------------------------
// The chat panel is one component (`chatPanel.ts`) instantiated per endpoint. The
// management console mounts one against the management binding; each agent
// console mounts one against its agent (`agentConsole.ts`). This module owns the
// single `agent-update` / `remote-status` subscription and routes each event to
// the matching panel by endpoint name — the backend tags every event with the
// agent it belongs to, so N panels share one listener.
const chatPanels = new Map<string, ChatPanel>();

// True in the browser build (no Tauri shell): there is no live agent, so panels
// drive a canned reply locally to stay demonstrable.
function isMock(): boolean {
  return tauriInvoke() === undefined;
}

// The management console's chat panel + the endpoint name its events carry. The
// panel is built at boot; its name is learned from the registry (the
// `management: true` entry) so events tagged with that name route here. `agent:
// undefined` ⇒ the legacy single-console commands (no `agent` arg).
let managementPanel: ChatPanel | null = null;
let managementName: string | null = null;

function buildManagementPanel(): void {
  if (
    !chatLogEl ||
    !chatFormEl ||
    !chatTextEl ||
    !chatSendEl ||
    !chatStopEl ||
    !chatConnEl
  )
    return;
  managementPanel = createChatPanel(
    {
      log: chatLogEl,
      form: chatFormEl,
      text: chatTextEl,
      send: chatSendEl,
      stop: chatStopEl,
      conn: chatConnEl,
    },
    { source, mock: isMock(), note },
  );
}

// Key the management panel under its endpoint name (from the registry) so its
// `agent-update` / `remote-status` events route to it. The legacy `remote.toml`
// setup is adopted as a `management` entry named "management".
async function registerManagementPanel(): Promise<void> {
  if (!managementPanel) return;
  try {
    const agents = await source.remoteAgents();
    managementName = agents.find((a) => a.management)?.name ?? "management";
  } catch {
    managementName = "management";
  }
  chatPanels.set(managementName, managementPanel);
}

// Route a backend event to the panel that owns the endpoint. Unknown names (a
// console that was closed, or an agent with no open panel) are dropped.
function routeChunk(agent: string, text: string): void {
  chatPanels.get(agent)?.onChunk(text);
}
function routeTurnEnd(agent: string, stopReason: string): void {
  chatPanels.get(agent)?.onTurnEnd(stopReason);
}
function routeStatus(agent: string, status: string): void {
  chatPanels.get(agent)?.setConnected(status === "connected");
  // Also reflect it on the agent console's read-only status badge (no-op when
  // the event isn't for the currently open console).
  agentConsole?.onStatus(agent, status);
}

// Subscribe to the backend's streamed chat updates (desktop only). Each event is
// tagged with the `agent` endpoint it belongs to; route it to that panel. The
// browser build has no bridge and drives the mock reply path per panel instead.
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
  await listen<{
    agent?: string;
    kind?: string;
    text?: string;
    stopReason?: string;
  }>("agent-update", (e) => {
    const p = e.payload;
    const agent = p.agent ?? managementName ?? "management";
    if (p.kind === "chunk") routeChunk(agent, p.text ?? "");
    else if (p.kind === "turn_end") routeTurnEnd(agent, p.stopReason ?? "end_turn");
  });
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
    note("info", `roster: ${action === "start" ? "started" : "stopped"} ${namespace}/${name}`);
    // tick() observes the new desiredCount and prunes the guard when it flips.
    await tick();
  } catch (e) {
    note("error", `roster: ${action} ${namespace}/${name} failed — ${errText(e)}`);
    clearPending(key);
    repaintRoster();
  }
}

// Drill into a roster row's Agent console (ADR #83 slice 3: mockup 7.4). The
// agent-console shell (`agentConsole.ts`) is untouched — its selector
// (`#agent-list`, now hidden via CSS in favor of this roster-row entry point)
// already opens-on-click for a `[data-agent]` button, so reuse that exact,
// already-tested path via a synthetic click rather than duplicating the
// open/dial/teardown logic here. Tries the service-name form first, then the
// short name — the same precedence `filterByMembers` uses for fleet members.
// `source` is the roster row button that was clicked — every "can't open"
// path flashes feedback on it, since `note()` alone only reaches the (closed)
// Debug drawer's Activity tab and a click that does nothing there reads as
// the roster row simply not working.
function openAgentForRow(svc: string, alt: string, source: HTMLButtonElement): void {
  const btn =
    document.querySelector<HTMLButtonElement>(
      `#agent-list [data-agent="${CSS.escape(svc)}"]`,
    ) ??
    document.querySelector<HTMLButtonElement>(
      `#agent-list [data-agent="${CSS.escape(alt)}"]`,
    );
  if (btn && !btn.disabled) {
    btn.click();
    return;
  }
  const reason = btn
    ? "registered in agents.toml but not configured (no url/token)"
    : "no matching agents.toml entry — or it's the management agent, whose console is already open above";
  note("info", `agents: "${alt}" ${reason} — nothing opened`);
  flashRowFeedback(source, btn ? "not configured" : "no console");
}

// Briefly relabel the clicked roster button so a click that can't open a
// console still visibly does *something*, instead of looking identical to a
// dead button.
function flashRowFeedback(btn: HTMLButtonElement, label: string): void {
  const original = btn.textContent;
  btn.disabled = true;
  btn.textContent = label;
  setTimeout(() => {
    btn.textContent = original;
    btn.disabled = false;
  }, 1800);
}

// One delegated listener on the roster. Start executes on click; Stop is
// disruptive (kills the running instance, though reversible), so it arms on the
// first click and only executes on a confirming second click within 3s — a
// webview-safe confirm that needs no dialog plugin. The 5s poll re-renders the
// roster and would reset an armed button on its own; the 3s timer is tighter.
if (roster) {
  roster.addEventListener("click", (ev) => {
    const target = ev.target as HTMLElement;
    const openBtn = target.closest<HTMLButtonElement>("button.row-open");
    if (openBtn) {
      const { openAgent, openAgentAlt } = openBtn.dataset;
      if (openAgent) openAgentForRow(openAgent, openAgentAlt ?? openAgent, openBtn);
      return;
    }
    const btn = target.closest<HTMLButtonElement>("button.act");
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
    note("error", `core: start failed — ${errText(e)}`);
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
  const dismissEl = document.getElementById("update-dismiss") as HTMLButtonElement | null;
  const invoke = tauriInvoke();
  if (!el || !invoke) return; // browser build — no updater
  el.hidden = false;
  let pending: UpdateInfo | null = null;

  // Finding an update must not force installing it (you may want to keep working
  // or wait for a later nightly). The install button + this dismiss share the
  // pending state; dismiss clears it back to "Check for updates".
  const setPending = (info: UpdateInfo | null): void => {
    pending = info;
    if (dismissEl) dismissEl.hidden = info === null;
  };

  const reset = (): void => {
    setPending(null);
    el.textContent = "Check for updates";
    el.classList.remove("has-update");
  };

  async function check(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "Checking…";
    try {
      const info = await inv<UpdateInfo | null>("check_update");
      if (info) {
        setPending(info);
        btn.textContent = `Update to v${info.version} ↻`;
        btn.classList.add("has-update");
        note("info", `update: v${info.version} available (current v${info.current}) — click to install, or Later to keep this build`);
      } else {
        note("info", "update: already up to date");
        btn.textContent = "Up to date";
        window.setTimeout(reset, 4000);
      }
    } catch (e) {
      reset();
      note("error", `update: check failed — ${errText(e)}`);
    } finally {
      btn.disabled = false;
    }
  }

  async function install(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "Installing…";
    if (dismissEl) dismissEl.hidden = true;
    try {
      // On success the backend restarts the app, so this may never resolve.
      await inv("install_update");
    } catch (e) {
      btn.disabled = false;
      btn.textContent = pending ? `Update to v${pending.version} ↻` : "Check for updates";
      if (dismissEl) dismissEl.hidden = pending === null;
      note("error", `update: install failed — ${errText(e)}`);
    }
  }

  el.addEventListener("click", () => void (pending ? install(el, invoke) : check(el, invoke)));
  dismissEl?.addEventListener("click", () => {
    const skipped = pending?.version;
    reset();
    if (skipped) note("info", `update: skipped v${skipped} — staying on the current build`);
  });
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
  await listen<{ agent?: string; status: string }>("remote-status", (e) => {
    const status = e.payload?.status ?? "disconnected";
    const agent = e.payload?.agent ?? managementName ?? "management";
    // The legacy remote panel shows only the management connection's status.
    if (agent === managementName && remoteConfig) {
      remoteConfig = { ...remoteConfig, status };
      if (remoteEl) renderRemote(remoteEl, remoteConfig, registryConfig?.path);
    }
    // Route the live state to the owning chat panel — it re-enables its input on
    // `connected` and, on a mid-turn drop, closes the open turn so it doesn't
    // hang on a spinner (handled inside the panel's `setConnected`).
    routeStatus(agent, status);
  });
}

// Boot order matters: subscribe to the log streams FIRST, then start the core,
// so the spawn → handshake → ready lifecycle lines are captured, not lost.
async function boot(): Promise<void> {
  note("info", `OAB Studio ${BUILD} (built ${__BUILD_TIME__})`);
  if (activity && mcp) await bindBackend(activity, mcp);
  // Mount the management chat panel and learn its endpoint name before binding
  // the event listeners, so status/chat events route to it from the first tick.
  buildManagementPanel();
  await registerManagementPanel();
  await bindRemoteStatus();
  await bindAgentUpdates();
  // The agent-console shell: the endpoint selector + a per-agent console (dial +
  // read-only config + chat) sharing the same event router (`chatPanels`).
  agentConsole = initAgentConsole({
    source,
    mock: isMock(),
    note,
    panels: chatPanels,
    managementName: () => managementName,
    managementUrl: () =>
      remoteConfig && remoteConfig.status !== "disconnected" ? remoteConfig.url : null,
  });
  if (clusterLabel) clusterLabel.textContent = activeCluster;
  note("info", `app: polling cluster "${activeCluster}" every ${POLL_MS / 1000}s`);
  // Appearance toggle (System / Light / Dark) — an override on top of the OS
  // prefers-color-scheme default.
  const themeBtn = document.getElementById("theme-btn");
  if (themeBtn) initThemeToggle(themeBtn as HTMLButtonElement);
  // Drag-resize between the drill-down column and the persistent Agent chat
  // side (`.drilldown-side`) — widening the side is how the operator makes
  // the chat panel bigger.
  const drilldownResizer = document.getElementById("drilldown-resizer");
  const drilldownSide = document.getElementById("drilldown-side");
  if (drilldownResizer && drilldownSide) initSplitPane(drilldownResizer, drilldownSide);
  // The Debug drawer anchors below the topbar (`--topbar-h`, styles.css) so it
  // slides in without hiding the topbar or the drawer's own header underneath
  // it. Tracked live — the topbar's height isn't hard-coded.
  const topbarEl = document.querySelector<HTMLElement>(".topbar");
  if (topbarEl) {
    const syncTopbarHeight = (): void => {
      document.documentElement.style.setProperty(
        "--topbar-h",
        `${topbarEl.getBoundingClientRect().height}px`,
      );
    };
    syncTopbarHeight();
    new ResizeObserver(syncTopbarHeight).observe(topbarEl);
  }
  // The Fleets column and the Agent chat column used to size to their own
  // content, so a short Fleets list ended up visibly shorter than the taller
  // chat panel (Brett). `--drilldown-row-h` floors the row's height at the
  // window's bottom edge; `align-items: stretch` (styles.css) then makes both
  // columns match it. `.drilldown-row`'s own top is fixed (topbar height +
  // `.content`'s padding, neither content-dependent), so this only needs
  // `resize`, not a ResizeObserver on the row itself.
  syncDrilldownHeight();
  window.addEventListener("resize", syncDrilldownHeight);
  // Drag-resize between the row and the editor, shown only while the editor
  // is open (`openEditor`/`closeEditor` toggle `rowResizerEl.hidden` and the
  // row's `is-split` mode) — Brett wanted the row's own height adjustable
  // too, not just auto-filled.
  if (rowResizerEl && drilldownRowEl) {
    initRowResize(rowResizerEl, drilldownRowEl, syncEditorHeight);
  }
  // Keep the fleets.toml/agents.toml editor filling the window as it's
  // resized (`syncEditorHeight` no-ops while closed).
  window.addEventListener("resize", syncEditorHeight);
  setupUpdater();
  await startCore();
  // Debug drawer's Config tab: pin the oab-mcp target (cluster/profile/region →
  // hermetic env); on save the backend reloads the core, so refresh the roster
  // after.
  initConfigTab({ onSaved: () => void tick() });
  updateScreen();
  void refreshConfig();
  void refreshRemote();
  void tick();
  window.setInterval(() => void tick(), POLL_MS);
}

void boot();
