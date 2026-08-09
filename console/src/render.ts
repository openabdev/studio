import type { AgentState, Deployment } from "./types";

const STATE_CLASS: Record<AgentState, string> = {
  Starting: "s-starting",
  Running: "s-running",
  Paused: "s-paused",
  Unhealthy: "s-unhealthy",
  Stopping: "s-stopping",
  Stopped: "s-stopped",
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function badge(state: AgentState): string {
  return `<span class="badge ${STATE_CLASS[state]}">${state}</span>`;
}

function rowHtml(d: Deployment): string {
  const phases = d.instances.length
    ? d.instances.map((i) => badge(i.state)).join(" ")
    : `<span class="muted">—</span>`;
  const health = d.ready === d.desired ? "ok" : "warn";
  const name = escapeHtml(`${d.namespace}/${d.name}`);
  return `<tr>
      <td class="name">${name}</td>
      <td class="counts ${health}">${d.ready}/${d.desired}<span class="muted"> · cur ${d.current}</span></td>
      <td class="phases">${phases}</td>
    </tr>`;
}

// Pure: deployments -> roster table HTML. Kept side-effect-free so it is unit
// testable without a DOM.
export function rosterHtml(deployments: Deployment[]): string {
  if (deployments.length === 0) {
    return `<p class="empty">No deployments in this cluster.</p>`;
  }
  const rows = [...deployments]
    .sort((a, b) =>
      `${a.namespace}/${a.name}`.localeCompare(`${b.namespace}/${b.name}`),
    )
    .map(rowHtml)
    .join("");
  return `<table class="roster">
      <thead>
        <tr><th>Deployment</th><th>Ready / Desired</th><th>Instances · 6-state</th></tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

export function renderRoster(el: HTMLElement, deployments: Deployment[]): void {
  el.innerHTML = rosterHtml(deployments);
}
