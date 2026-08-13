import type { AgentState, Deployment, RuntimeContext } from "./types";

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

// ---- Runtime identity / context panel (ADR #19) ------------------------------

const KIND_CLASS: Record<string, string> = {
  role: "k-role",
  user: "k-user",
  unknown: "k-unknown",
};

function kindBadge(kind: string): string {
  return `<span class="kind ${KIND_CLASS[kind] ?? "k-unknown"}">${escapeHtml(kind)}</span>`;
}

function field(label: string, value: string, mono = true): string {
  const v = mono ? `<code>${escapeHtml(value)}</code>` : escapeHtml(value);
  return `<div class="id-field"><span class="k">${label}</span>${v}</div>`;
}

// Pure: a RuntimeContext -> the identity panel HTML. `null` renders an
// unavailable state (core not started / call failed). Highlights a mismatch
// when the resolved principal doesn't satisfy the binding's expectation.
export function identityHtml(ctx: RuntimeContext | null): string {
  if (!ctx) {
    return `<div class="identity"><span class="muted">identity unavailable</span></div>`;
  }
  const mismatch = ctx.identity_matches === false;
  const matched = ctx.identity_matches === true;
  const cls = mismatch ? "identity mismatch" : matched ? "identity ok" : "identity";
  const binding = ctx.binding
    ? field("binding", ctx.binding.name || ctx.binding.profile || "—", false)
    : `<div class="id-field"><span class="k">binding</span><span class="muted">none (default chain)</span></div>`;
  const verdict = mismatch
    ? `<div class="id-warn">⚠ identity mismatch — expected <code>${escapeHtml(ctx.expected_principal ?? "")}</code></div>`
    : matched
      ? `<div class="id-ok">✓ matches expected principal</div>`
      : "";
  return `<div class="${cls}">
      <div class="id-head">
        <span class="id-label">managing</span>
        <span class="id-cluster">${escapeHtml(ctx.cluster)}</span>
        <span class="id-as">as</span> ${kindBadge(ctx.principal_kind)}
      </div>
      <div class="id-grid">
        ${field("principal", ctx.principal || "—")}
        ${field("account", ctx.scope || "—")}
        ${field("region", ctx.location || "—")}
        ${field("source", ctx.source || "—", false)}
        ${binding}
      </div>
      ${verdict}
    </div>`;
}

export function renderIdentity(el: HTMLElement, ctx: RuntimeContext | null): void {
  el.innerHTML = identityHtml(ctx);
}
