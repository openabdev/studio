import type {
  AgentState,
  Deployment,
  FleetConfig,
  RuntimeContext,
} from "./types";

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

// Start (scale→1) when the deployment is off, Stop (scale→0) when it's on.
// Stop keeps the Spec — ECS retains the service at desiredCount 0 — so it's
// reversible, no state store needed. The `data-*` carry the identity the
// delegated handler needs; the managing credential is resolved per-cluster, so
// the row only needs name + namespace (service = `oab-{namespace}-{name}`).
function actionButton(d: Deployment): string {
  const off = d.desired === 0;
  const action = off ? "start" : "stop";
  const label = off ? "Start" : "Stop";
  const cls = off ? "act act-start" : "act act-stop";
  return `<button class="${cls}" type="button" data-action="${action}" data-name="${escapeHtml(d.name)}" data-namespace="${escapeHtml(d.namespace)}">${label}</button>`;
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
      <td class="actions">${actionButton(d)}</td>
    </tr>`;
}

// The ECS service name a Deployment maps to — the key `fleets.toml` `members`
// are written as (`oab-{namespace}-{name}`). Kept here so the member/deployment
// join lives in one place; mirrors studio-cp's `oab-{ns}-{name}` convention.
export function serviceName(d: Deployment): string {
  return `oab-${d.namespace}-${d.name}`;
}

// Pure: keep only the deployments belonging to a fleet's `members`. An empty
// member list ⇒ the fleet covers the whole cluster (legacy semantics), so the
// roster is unfiltered. A member matches either the full ECS service name
// (`oab-{ns}-{name}`) or the short deployment name — mirroring studio-cp's
// `resolve_service`, which accepts both forms.
export function filterByMembers(
  deployments: Deployment[],
  members: string[],
): Deployment[] {
  if (members.length === 0) return deployments;
  const wanted = new Set(members);
  return deployments.filter(
    (d) => wanted.has(serviceName(d)) || wanted.has(d.name),
  );
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
        <tr><th>Deployment</th><th>Ready / Desired</th><th>Instances · 6-state</th><th class="actions-h">Actions</th></tr>
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

// ---- Fleet config panel (ADR #19: the "declare" side) ------------------------

function credLine(f: FleetConfig["fleets"][number]): string {
  // Profile-first (assume-role is later work); region pins the fleet's location.
  const parts = [f.profile ?? "default chain", f.region].filter(
    (p): p is string => Boolean(p),
  );
  return parts.map(escapeHtml).join(" · ");
}

// The members line: the ECS services grouped into this fleet, or a note that an
// empty member list means the whole cluster (legacy semantics).
function membersLine(f: FleetConfig["fleets"][number]): string {
  if (f.members.length === 0) {
    return `<span class="cfg-members cfg-members-all">whole cluster</span>`;
  }
  const chips = f.members
    .map((m) => `<span class="cfg-member">${escapeHtml(m)}</span>`)
    .join("");
  return `<span class="cfg-members">${chips}</span>`;
}

function fleetButton(
  f: FleetConfig["fleets"][number],
  activeFleet: string | null,
): string {
  const active = f.name === activeFleet;
  const cls = active ? "cfg-fleet is-active" : "cfg-fleet";
  return `<button class="${cls}" type="button" data-fleet="${escapeHtml(f.name)}" aria-pressed="${active}">
      <span class="cfg-name">${escapeHtml(f.name || f.cluster)}</span>
      <span class="cfg-cluster">${escapeHtml(f.cluster)}</span>
      ${membersLine(f)}
      <span class="cfg-cred">${credLine(f)}</span>
    </button>`;
}

// Pure: the fleet-binding config -> the config panel HTML. A fleet is a
// usage-based group, so each button switches by fleet **identity** (name), not
// by cluster — two fleets may share a cluster. `activeFleet` (a name, or `null`
// when none is selected) marks the current one. An empty config still renders —
// it shows where to add bindings, which is exactly the "no panel for config" gap.
export function fleetConfigHtml(
  cfg: FleetConfig | null,
  activeFleet: string | null,
): string {
  if (!cfg) {
    return `<div class="config"><span class="muted">fleet config unavailable</span></div>`;
  }
  const path = cfg.path
    ? `<span class="cfg-path" title="edit this file to configure fleets"><code>${escapeHtml(cfg.path)}</code></span>`
    : "";
  const body = cfg.fleets.length
    ? `<div class="cfg-list">${cfg.fleets.map((f) => fleetButton(f, activeFleet)).join("")}</div>`
    : `<p class="cfg-empty">No fleets configured — add <code>[fleet.&lt;name&gt;]</code> entries to the config file above. Managing <code>${escapeHtml(cfg.default_cluster)}</code> via the default credential chain.</p>`;
  return `<div class="config">
      <div class="cfg-head">
        <span class="cfg-label">fleets</span>
        ${path}
        <button class="cfg-edit" type="button" data-action="edit-config">Edit config</button>
      </div>
      ${body}
    </div>`;
}

export function renderFleetConfig(
  el: HTMLElement,
  cfg: FleetConfig | null,
  activeFleet: string | null,
): void {
  el.innerHTML = fleetConfigHtml(cfg, activeFleet);
}
