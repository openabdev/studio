import { defaultSource } from "./source";
import { renderRoster } from "./render";

const POLL_MS = 5000;
const CLUSTER = "oab";

const roster = document.getElementById("roster");
const clusterLabel = document.getElementById("cluster-label");
const pollStatus = document.getElementById("poll-status");
const source = defaultSource();

async function tick(): Promise<void> {
  if (!roster) return;
  try {
    const deployments = await source.listDeployments(CLUSTER);
    renderRoster(roster, deployments);
    if (pollStatus) {
      pollStatus.textContent = `updated ${new Date().toLocaleTimeString()}`;
      pollStatus.classList.remove("err");
    }
  } catch (e) {
    if (pollStatus) {
      pollStatus.textContent = `error: ${(e as Error).message}`;
      pollStatus.classList.add("err");
    }
  }
}

if (clusterLabel) clusterLabel.textContent = CLUSTER;
void tick();
window.setInterval(() => void tick(), POLL_MS);
