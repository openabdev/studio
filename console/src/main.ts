import { defaultSource } from "./source";
import { renderRoster } from "./render";
import { createPane, bindBackend, type Level } from "./log";

const POLL_MS = 5000;
const CLUSTER = "oab";

const roster = document.getElementById("roster");
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

function note(level: Level, msg: string): void {
  activity?.push({ cls: `lv-${level}`, tag: level.toUpperCase(), msg });
}

let lastError = "";

async function tick(): Promise<void> {
  if (!roster) return;
  try {
    const deployments = await source.listDeployments(CLUSTER);
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
    const msg = (e as Error).message;
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

// Panes are wired first, so launch + core lifecycle + MCP traffic are visible
// immediately — before any roster data arrives.
note("info", "console loaded");
if (activity && mcp) void bindBackend(activity, mcp);

if (clusterLabel) clusterLabel.textContent = CLUSTER;
note("info", `polling cluster "${CLUSTER}" every ${POLL_MS / 1000}s`);
void tick();
window.setInterval(() => void tick(), POLL_MS);
