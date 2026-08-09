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

// Pane 1: Activity (lifecycle + failures). Pane 2: MCP interaction stream.
const activity = logEl ? createPane(logEl) : null;
const mcp = mcpEl ? createPane(mcpEl) : null;

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
