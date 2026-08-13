import { defaultSource } from "./source";
import { renderRoster, renderIdentity, renderFleetConfig } from "./render";
import type { FleetConfig } from "./types";
import { createPane, bindBackend, type Level } from "./log";

const POLL_MS = 5000;
const DEFAULT_CLUSTER = "oab";

// The active fleet's cluster drives every read (roster + identity) and, through
// oab-mcp's per-cluster binding, which credential/account we manage as. Selecting
// a fleet in the config panel is the "switch" step of the ADR #19 loop.
let activeCluster = DEFAULT_CLUSTER;
let fleetConfig: FleetConfig | null = null;

const roster = document.getElementById("roster");
const identityEl = document.getElementById("identity");
const configEl = document.getElementById("config");
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
    const deployments = await source.listDeployments(activeCluster);
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
    renderFleetConfig(configEl, fleetConfig, activeCluster);
  } catch (e) {
    note("error", `fleet config: ${errText(e)}`);
    fleetConfig = null;
    renderFleetConfig(configEl, null, activeCluster);
  }
}

// Switch the active fleet: re-point every read at its cluster (and thus its
// bound credential) and refresh immediately, so "switch fleet" == "switch
// managing account" the ADR calls for. No-op if it's already active.
function selectCluster(cluster: string): void {
  if (!cluster || cluster === activeCluster) return;
  activeCluster = cluster;
  if (clusterLabel) clusterLabel.textContent = activeCluster;
  note("info", `switched to cluster "${activeCluster}"`);
  if (configEl) renderFleetConfig(configEl, fleetConfig, activeCluster);
  void refreshIdentity();
  void tick();
}

// One delegated listener: a click on any fleet button switches to its cluster.
if (configEl) {
  configEl.addEventListener("click", (ev) => {
    const btn = (ev.target as HTMLElement).closest<HTMLElement>("[data-cluster]");
    if (btn?.dataset.cluster) selectCluster(btn.dataset.cluster);
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

// Remote upgrade: the topbar "檢查更新" button. First click checks the nightly
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
    el.textContent = "檢查更新";
    el.classList.remove("has-update");
  };

  async function check(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "檢查中…";
    try {
      const info = await inv<UpdateInfo | null>("check_update");
      if (info) {
        pending = info;
        btn.textContent = `更新到 v${info.version} ↻`;
        btn.classList.add("has-update");
        note("info", `發現新版 v${info.version}（目前 v${info.current}）— 按按鈕安裝並重啟`);
      } else {
        note("info", "已是最新版");
        btn.textContent = "已是最新版";
        window.setTimeout(reset, 4000);
      }
    } catch (e) {
      reset();
      note("error", `檢查更新失敗：${errText(e)}`);
    } finally {
      btn.disabled = false;
    }
  }

  async function install(btn: HTMLButtonElement, inv: Invoke): Promise<void> {
    btn.disabled = true;
    btn.textContent = "安裝中…";
    try {
      // On success the backend restarts the app, so this may never resolve.
      await inv("install_update");
    } catch (e) {
      btn.disabled = false;
      btn.textContent = pending ? `更新到 v${pending.version} ↻` : "檢查更新";
      note("error", `安裝更新失敗：${errText(e)}`);
    }
  }

  el.addEventListener("click", () => void (pending ? install(el, invoke) : check(el, invoke)));
}

// Boot order matters: subscribe to the log streams FIRST, then start the core,
// so the spawn → handshake → ready lifecycle lines are captured, not lost.
async function boot(): Promise<void> {
  note("info", `OAB Studio ${BUILD} (built ${__BUILD_TIME__})`);
  if (activity && mcp) await bindBackend(activity, mcp);
  if (clusterLabel) clusterLabel.textContent = activeCluster;
  note("info", `polling cluster "${activeCluster}" every ${POLL_MS / 1000}s`);
  setupUpdater();
  await startCore();
  void refreshConfig();
  void refreshIdentity();
  void tick();
  window.setInterval(() => void tick(), POLL_MS);
}

void boot();
