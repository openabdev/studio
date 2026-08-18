import type {
  AgentEndpointView,
  AgentState,
  Deployment,
  FleetConfig,
  FsListing,
  RemoteConfig,
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

// A deployment's identity within the roster — the key the in-flight scale guard
// (`main.ts`) and the render agree on. Not the ECS service name; just the
// namespace/name pair a scale action targets.
export function deploymentKey(d: Deployment): string {
  return `${d.namespace}/${d.name}`;
}

// Start (scale→1) when the deployment is off, Stop (scale→0) when it's on.
// Stop keeps the Spec — ECS retains the service at desiredCount 0 — so it's
// reversible, no state store needed. The `data-*` carry the identity the
// delegated handler needs; the managing credential is resolved per-cluster, so
// the row only needs name + namespace (service = `oab-{namespace}-{name}`).
//
// `pending` ⇒ a scale is in flight (or the observed count hasn't flipped yet):
// render a disabled placeholder so the 5s poll re-render can't hand back a fresh
// enabled button mid-action. The guard lives in module state, not on the DOM
// node, so it survives the re-render.
function actionButton(d: Deployment, pending: boolean): string {
  if (pending) {
    return `<button class="act act-pending" type="button" disabled>…</button>`;
  }
  const off = d.desired === 0;
  const action = off ? "start" : "stop";
  const label = off ? "Start" : "Stop";
  const cls = off ? "act act-start" : "act act-stop";
  return `<button class="${cls}" type="button" data-action="${action}" data-name="${escapeHtml(d.name)}" data-namespace="${escapeHtml(d.namespace)}">${label}</button>`;
}

// The row's name cell is a button, not plain text — clicking a member drills
// into its Agent console (ADR #83 slice 3: mockup 7.4), if one is registered.
// `data-open-agent`/`data-open-agent-alt` carry both candidate identities
// (service name and short name) the delegated handler tries against the
// registry, same precedence as `filterByMembers`'s member match.
function rowHtml(d: Deployment, pending: ReadonlySet<string>): string {
  const phases = d.instances.length
    ? d.instances.map((i) => badge(i.state)).join(" ")
    : `<span class="muted">—</span>`;
  const health = d.ready === d.desired ? "ok" : "warn";
  const name = escapeHtml(`${d.namespace}/${d.name}`);
  const svc = escapeHtml(serviceName(d));
  const shortName = escapeHtml(d.name);
  return `<tr>
      <td class="name"><button class="row-open" type="button" data-open-agent="${svc}" data-open-agent-alt="${shortName}" title="open agent console">${name}</button></td>
      <td class="counts ${health}">${d.ready}/${d.desired}<span class="muted"> · cur ${d.current}</span></td>
      <td class="phases">${phases}</td>
      <td class="actions">${actionButton(d, pending.has(deploymentKey(d)))}</td>
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
// testable without a DOM. `pending` is the set of `deploymentKey`s with a scale
// in flight — their action buttons render disabled.
export function rosterHtml(
  deployments: Deployment[],
  pending: ReadonlySet<string> = new Set(),
): string {
  if (deployments.length === 0) {
    return `<p class="empty">No deployments in this cluster.</p>`;
  }
  const rows = [...deployments]
    .sort((a, b) =>
      `${a.namespace}/${a.name}`.localeCompare(`${b.namespace}/${b.name}`),
    )
    .map((d) => rowHtml(d, pending))
    .join("");
  return `<table class="roster">
      <thead>
        <tr><th>Deployment</th><th>Ready / Desired</th><th>Instances · 6-state</th><th class="actions-h">Actions</th></tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

export function renderRoster(
  el: HTMLElement,
  deployments: Deployment[],
  pending: ReadonlySet<string> = new Set(),
): void {
  el.innerHTML = rosterHtml(deployments, pending);
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

// The `[⚙]` sits beside, not inside, the switch button — a fleet row is two
// independent click targets (select vs. debug), not one giant button, so
// they're siblings under a `.fleets-row` wrapper rather than nested
// `<button>`s (7.2: "a `[⚙]` that opens the Debug drawer scoped to that
// fleet's Activity/MCP/Config", slice 6).
function fleetButton(
  f: FleetConfig["fleets"][number],
  activeFleet: string | null,
): string {
  const active = f.name === activeFleet;
  const cls = active ? "cfg-fleet is-active" : "cfg-fleet";
  const name = escapeHtml(f.name);
  return `<div class="fleets-row">
      <button class="${cls}" type="button" data-fleet="${name}" aria-pressed="${active}">
        <span class="cfg-name">${escapeHtml(f.name || f.cluster)}</span>
        <span class="cfg-cluster">${escapeHtml(f.cluster)}</span>
        ${membersLine(f)}
        <span class="cfg-cred">${credLine(f)}</span>
      </button>
      <button class="fd-btn fd-gear" type="button" data-action="fleet-debug" data-fleet="${name}" title="Debug: ${name}">⚙</button>
    </div>`;
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
        <h2 class="cfg-title">Fleets</h2>
        ${path}
        <span class="cfg-head-spacer"></span>
        <button class="cfg-btn" type="button" data-action="new-fleet">+ New fleet</button>
        <button class="cfg-edit" type="button" data-action="edit-config">Edit config</button>
      </div>
      <p class="cfg-subtitle">Select a fleet to manage.</p>
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

// ---- Fleet detail screen header (ADR #83 slice 2) -----------------------------
// The breadcrumb + action row shown above the roster once a fleet is selected —
// "← Fleets" returns to the Fleets screen (Part A's drill-down). `[+ Add
// instance]` is the slice 5 entry point (7.5.2: deploy into this fleet, no new
// fleet-identity step). `[⚙]` is the slice 6 entry point — opens the Debug
// drawer (Activity/MCP/Config) scoped to this fleet.
export function fleetDetailHeaderHtml(fleetName: string): string {
  return `<div class="fd-head">
      <button class="fd-back" type="button" data-action="back-to-fleets">&larr; Fleets</button>
      <span class="fd-sep">/</span>
      <span class="fd-name">${escapeHtml(fleetName)}</span>
      <span class="fd-spacer"></span>
      <button class="fd-btn" type="button" data-action="add-instance">+ Add instance</button>
      <button class="fd-btn fd-gear" type="button" data-action="fleet-debug" title="Debug: ${escapeHtml(fleetName)}">⚙</button>
    </div>`;
}

export function renderFleetDetailHeader(el: HTMLElement, fleetName: string): void {
  el.innerHTML = fleetDetailHeaderHtml(fleetName);
}

// ---- Remote reverse-MCP connection panel (Part B) ---------------------------
// The "activate the remote connection" surface: the /acp endpoint, live status,
// an explicit Activate/Disconnect button, and Edit config. Since #69 the editor
// opens the registry (`agents.toml`, the source of truth), not the deprecated
// `remote.toml`, so the panel labels the registry path — passed in, since it
// lives in a different view-model (`RegistryConfig`) than the connection view.

const REMOTE_STATUS_CLASS: Record<string, string> = {
  connected: "rm-connected",
  connecting: "rm-connecting",
  disconnected: "rm-disconnected",
  error: "rm-error",
};

function remoteStatusBadge(status: string): string {
  const key = status.startsWith("error") ? "error" : status;
  const cls = REMOTE_STATUS_CLASS[key] ?? "rm-disconnected";
  return `<span class="rm-status ${cls}">${escapeHtml(status || "disconnected")}</span>`;
}

// Pure: the remote-connection view -> the panel HTML. `null` renders an
// unavailable state. The button is Disconnect while connected/connecting, else
// Activate — disabled until a URL + token are configured. `registryPath` is the
// `agents.toml` path "Edit config" opens; omitted/empty ⇒ the path label is
// hidden (better than labelling a file the button no longer edits).
export function remoteHtml(
  view: RemoteConfig | null,
  registryPath?: string | null,
): string {
  if (!view) {
    return `<div class="remote"><span class="muted">remote connection unavailable</span></div>`;
  }
  const live = view.status === "connected" || view.status === "connecting";
  const action = live
    ? `<button class="rm-btn rm-disconnect" type="button" data-action="remote-disconnect">Disconnect</button>`
    : `<button class="rm-btn rm-connect" type="button" data-action="remote-connect"${view.configured ? "" : " disabled"}>Activate remote connection</button>`;
  const target = view.configured
    ? `<code class="rm-url">${escapeHtml(view.url)}</code>`
    : `<span class="muted">not configured — set <code>url</code> + <code>token</code> in the config</span>`;
  const path = registryPath
    ? `<span class="cfg-path" title="Edit config opens this registry file (agents.toml)"><code>${escapeHtml(registryPath)}</code></span>`
    : "";
  return `<div class="remote">
      <div class="rm-head">
        <span class="cfg-label">remote</span>
        ${path}
        <button class="cfg-edit" type="button" data-action="edit-remote-config">Edit config</button>
      </div>
      <div class="rm-body">
        ${remoteStatusBadge(view.status)}
        <span class="rm-target">${target}</span>
        ${action}
      </div>
    </div>`;
}

export function renderRemote(
  el: HTMLElement,
  view: RemoteConfig | null,
  registryPath?: string | null,
): void {
  el.innerHTML = remoteHtml(view, registryPath);
}

// ---- Agent consoles (ADR agent-consoles, Parts B/C) --------------------------
// The per-agent endpoint registry: a selector of every reachable agent + the
// open console's read-only config header. The chat region is the reusable
// `chatPanel` primitive (mounted imperatively in `agentConsole.ts`); the file
// editor is a later slice, so config is read-only here.

// A connection-status pill, reusing the remote panel's status classes so the two
// surfaces read the same. `error: …` collapses to the error class.
function endpointStatusBadge(status: string): string {
  const s = status || "disconnected";
  const key = s.startsWith("error") ? "error" : s;
  const cls = REMOTE_STATUS_CLASS[key] ?? "rm-disconnected";
  return `<span class="rm-status ${cls}">${escapeHtml(s)}</span>`;
}

// One selector row per endpoint. The management endpoint is shown but not
// openable as an agent console — it has its own top-level console (with chat +
// fleet control); a duplicate per-agent console for it is redundant. Ordinary
// endpoints open on click (`data-agent`); an unconfigured one (no url+token) is
// disabled. `openName` marks the currently open console.
function agentRow(a: AgentEndpointView, openName: string | null): string {
  const name = escapeHtml(a.name);
  const url = a.url
    ? `<code class="ag-url">${escapeHtml(a.url)}</code>`
    : `<span class="muted">not configured</span>`;
  const tags = a.management
    ? `<span class="ag-tag ag-mgmt">management</span>`
    : "";
  if (a.management) {
    return `<div class="ag-row ag-row-mgmt" aria-disabled="true">
        <span class="ag-name">${name}</span>${tags}
        ${url}
        ${endpointStatusBadge(a.status)}
        <span class="ag-hint muted">console above</span>
      </div>`;
  }
  const open = a.name === openName;
  const cls = open ? "ag-row is-open" : "ag-row";
  const disabled = a.configured ? "" : " disabled";
  return `<button class="${cls}" type="button" data-agent="${name}" aria-pressed="${open}"${disabled}>
      <span class="ag-name">${name}</span>${tags}
      ${url}
      ${endpointStatusBadge(a.status)}
    </button>`;
}

// Pure: the endpoint registry -> the selector HTML. An empty registry renders a
// hint pointing at the config file (there is no in-app registry editor yet — a
// later slice). `openName` is the console currently open (or `null`).
export function agentListHtml(
  agents: AgentEndpointView[],
  openName: string | null,
): string {
  if (agents.length === 0) {
    return `<p class="ag-empty">No agent endpoints configured — add <code>[[agent]]</code> entries to <code>agents.toml</code> to reach more agents.</p>`;
  }
  return `<div class="ag-list">${agents
    .map((a) => agentRow(a, openName))
    .join("")}</div>`;
}

export function renderAgentList(
  el: HTMLElement,
  agents: AgentEndpointView[],
  openName: string | null,
): void {
  el.innerHTML = agentListHtml(agents, openName);
}

// Pure: the open console's read-only config header — identity + dial target +
// live status. The bearer token is never here (secrets don't cross the bridge).
// `status` is passed separately so a live `remote-status` update can refresh just
// the badge without re-fetching the whole registry.
export function agentConsoleHeaderHtml(
  a: AgentEndpointView,
  status: string,
): string {
  return `<div class="ac-id">
      <span class="ac-name">${escapeHtml(a.name)}</span>
      ${endpointStatusBadge(status)}
    </div>
    <div class="ac-fields">
      ${field("url", a.url || "—")}
      ${field("cwd", a.cwd || "—")}
    </div>
    <p class="ac-note muted">Read-only — the remote file editor (view / edit / apply the agent's files) arrives once the fs MCP files server lands.</p>`;
}

// ---- Remote file browser (ADR agent-consoles Part D, read path) --------------
// A directory listing over the agent's filesystem: dirs first, then files, each
// a click target the controller (`fileBrowser.ts`) navigates or opens. Pure so
// the sort/labels/escaping are unit-testable without a DOM. The read-only viewer
// (CodeMirror) is imperative in the controller. The fs MCP files server (+ `oab`
// relay) is upstream and absent today, so on real endpoints this is gated behind
// `fsUnavailableHtml`.

const FS_ICON: Record<string, string> = {
  dir: "📁",
  file: "📄",
  symlink: "🔗",
  other: "•",
};

// Human-readable byte size for the file rows (kept tiny; no external dep).
function fsSize(bytes: number | undefined): string {
  if (bytes === undefined) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export interface FsListOptions {
  // The file currently open in the viewer (marked in the list).
  selectedPath?: string | null;
  // Show an "up one level" affordance (false at an editable root's top).
  canGoUp?: boolean;
}

// Pure: a directory listing -> the browser HTML. Directories sort before files,
// each alphabetically; the open file is marked. `data-fs-dir` / `data-fs-file` /
// `data-fs-up` are the hooks the controller's delegated handler navigates by.
export function fsListingHtml(
  listing: FsListing,
  opts: FsListOptions = {},
): string {
  const dirs = listing.entries
    .filter((e) => e.kind === "dir")
    .sort((a, b) => a.name.localeCompare(b.name));
  const files = listing.entries
    .filter((e) => e.kind !== "dir")
    .sort((a, b) => a.name.localeCompare(b.name));
  const up = opts.canGoUp
    ? `<button class="fs-row fs-up" type="button" data-fs-up><span class="fs-ic">↰</span><span class="fs-name">..</span></button>`
    : "";
  const rowFor = (
    e: FsListing["entries"][number],
  ): string => {
    const isDir = e.kind === "dir";
    const attr = isDir
      ? `data-fs-dir="${escapeHtml(e.path)}"`
      : `data-fs-file="${escapeHtml(e.path)}"`;
    const open = !isDir && e.path === opts.selectedPath ? " is-open" : "";
    const size = isDir
      ? ""
      : `<span class="fs-size muted">${escapeHtml(fsSize(e.size))}</span>`;
    return `<button class="fs-row${open}" type="button" ${attr}>
        <span class="fs-ic">${FS_ICON[e.kind] ?? FS_ICON.other}</span>
        <span class="fs-name">${escapeHtml(e.name)}</span>
        ${size}
      </button>`;
  };
  const body =
    listing.entries.length === 0 && !opts.canGoUp
      ? `<p class="fs-empty muted">empty directory</p>`
      : up + dirs.map(rowFor).join("") + files.map(rowFor).join("");
  return `<div class="fs-crumb"><code>${escapeHtml(listing.path)}</code></div>
    <div class="fs-list">${body}</div>`;
}

// Pure: the placeholder shown when an endpoint has no fs capability (every
// endpoint today) or a browse/read call fails. Keeps the read-only, honest
// "pending the fs MCP files server" state in one place.
export function fsUnavailableHtml(reason: string): string {
  return `<p class="fs-unavailable muted">${escapeHtml(reason)}</p>`;
}
