import { defaultSource } from "./source";
import { renderRoster } from "./render";
import { initLog, log, bindBackendLog } from "./log";

const POLL_MS = 5000;
const CLUSTER = "oab";

const roster = document.getElementById("roster");
const clusterLabel = document.getElementById("cluster-label");
const pollStatus = document.getElementById("poll-status");
const logEl = document.getElementById("log");
const source = defaultSource();

let lastError = "";

async function tick(): Promise<void> {
  if (!roster) return;
  try {
    const deployments = await source.listDeployments(CLUSTER);
    renderRoster(roster, deployments);
    if (lastError) {
      log("info", `roster recovered — ${deployments.length} deployment(s)`);
      lastError = "";
    }
    if (pollStatus) {
      pollStatus.textContent = `updated ${new Date().toLocaleTimeString()}`;
      pollStatus.classList.remove("err");
    }
  } catch (e) {
    const msg = (e as Error).message;
    if (msg !== lastError) {
      log("error", `roster: ${msg}`);
      lastError = msg;
    }
    if (pollStatus) {
      pollStatus.textContent = `error: ${msg}`;
      pollStatus.classList.add("err");
    }
  }
}

// Log pane is the first thing wired up, so launch + core lifecycle are visible
// immediately — before any roster data arrives.
if (logEl) initLog(logEl);
log("info", "console loaded");
void bindBackendLog();

if (clusterLabel) clusterLabel.textContent = CLUSTER;
log("info", `polling cluster "${CLUSTER}" every ${POLL_MS / 1000}s`);
void tick();
window.setInterval(() => void tick(), POLL_MS);
