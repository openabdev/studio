// Wire types — mirror studio-cp's read-model (ADR-2). Kept in lockstep with
// `crates/studio-cp` (`Deployment` / `InstancePhase`) and the canonical
// 6-state `AgentState` (ADR-1). This is the view-model contract the skin
// depends on; any skin (web now, SwiftUI later) shares the same shape.

export type AgentState =
  | "Starting"
  | "Running"
  | "Paused"
  | "Unhealthy"
  | "Stopping"
  | "Stopped";

export const AGENT_STATES: readonly AgentState[] = [
  "Starting",
  "Running",
  "Paused",
  "Unhealthy",
  "Stopping",
  "Stopped",
];

export interface InstancePhase {
  id: string;
  state: AgentState;
}

export interface Deployment {
  name: string;
  namespace: string;
  desired: number;
  current: number;
  ready: number;
  instances: InstancePhase[];
}

// The fleet binding in effect for a cluster (ADR #19). Mirrors oab-mcp's
// `runtime_context.binding`.
export interface FleetBinding {
  name: string;
  profile: string | null;
  region: string | null;
  expected_principal: string | null;
}

// One configured fleet → managing-credential binding — mirrors an entry of
// oab-mcp's `fleet_config` tool (ADR #19, fleet-grouping ADR). `name` is the
// switch key: a fleet is a usage-based logical group, so two fleets may share a
// `cluster` (and thus one credential) while listing different `members`.
// Selecting a fleet targets its `cluster` for reads and filters the roster to
// its `members`. `members` are the ECS service names in the group; empty ⇒ the
// fleet covers the whole cluster (legacy `[[fleet]]` semantics).
export interface FleetConfigEntry {
  name: string;
  cluster: string;
  members: string[];
  region: string | null;
  profile: string | null;
  expected_principal: string | null;
}

// The declarative fleet-binding config — mirrors oab-mcp's `fleet_config` tool
// (ADR #19). `path` is the file the bindings load from (so the panel can show
// where to edit them); `default_cluster` is the fallback target when no fleet is
// selected.
export interface FleetConfig {
  path: string | null;
  default_cluster: string;
  fleets: FleetConfigEntry[];
  // Raw TOML text of the config file (what the editor loads/saves); empty when
  // no file exists yet.
  text: string;
}

// The remote reverse-MCP connection view — mirrors src-tauri's `remote_config`
// command (reverse-MCP client ADR, Part B). `path`/`text` back the in-app editor
// for `remote.toml`; `url` + `configured` drive the panel; `status` is the live
// connection state (`disconnected` | `connecting` | `connected` | `error: …`).
export interface RemoteConfig {
  path: string;
  text: string;
  url: string;
  configured: boolean;
  status: string;
}

// The per-agent endpoint registry file (`agents.toml`) as the editor sees it:
// its path + raw text. Mirrors src-tauri's `registry_config` command. Unlike the
// panel/selector views this carries the raw file, so it *does* include tokens —
// it is editor-only and never fed into `RemoteConfig` / `AgentEndpointView`.
export interface RegistryConfig {
  path: string;
  text: string;
}

// One entry of the per-agent endpoint registry — mirrors src-tauri's
// `remote_agents` command (ADR agent-consoles, Parts B/C). It is the view an
// agent-console selector renders: an identity (`name`), the dial target
// (`url`/`cwd`), whether this entry backs the management console (`management`),
// whether it has a usable url+token (`configured`), and the live per-endpoint
// connection `status` (`disconnected` | `connecting` | `connected` | `error: …`).
// The bearer `token` is deliberately absent — secrets never cross this bridge.
export interface AgentEndpointView {
  name: string;
  url: string;
  cwd: string;
  management: boolean;
  configured: boolean;
  status: string;
}

// The remote file editor's read-path view-model (ADR agent-consoles Part D).
// fs is an MCP files server the target agent exposes, reached Studio-brokered
// via the `oab` reverse-MCP tool (ADR resolved OQ#1 — not a bespoke `fs/*` wire
// on `/acp`). The server + relay are upstream (openab) and do not exist yet;
// these are the shapes the UI is built against so it is ready when they land.
// Tokens/secrets never appear here — this is filesystem content, gated at the
// fs server's tool level by the agent's declared roots.
export type FsKind = "file" | "dir" | "symlink" | "other";

export interface FsEntry {
  name: string;
  path: string;
  kind: FsKind;
  size?: number;
}

// A directory listing result: a directory and its entries.
export interface FsListing {
  path: string;
  entries: FsEntry[];
}

// A file-read result: a file's text. `truncated` marks a body the fs server
// clipped (large/binary file); text-only by design.
export interface FsFile {
  path: string;
  text: string;
  truncated: boolean;
  size?: number;
}

// Whether an endpoint supports the remote file editor, and within what bounds
// (agent-declared editable `roots`; `writable` gates the slice-4 write path).
// `supported: false` (every endpoint today) ⇒ the console shows read-only config
// and the file browser stays a "pending the fs MCP files server" placeholder.
export interface FsCapability {
  supported: boolean;
  roots: string[];
  writable: boolean;
}

// The effective runtime identity/context the control plane resolved for a
// cluster — mirrors oab-mcp's `runtime_context` tool (ADR #19). `identity_matches`
// is `null` when the binding declares no `expected_principal`.
export interface RuntimeContext {
  cluster: string;
  principal: string;
  principal_kind: string; // "role" | "user" | "unknown"
  scope: string; // AWS account id
  location: string; // region
  source: string;
  caller_id: string;
  binding: FleetBinding | null;
  expected_principal: string | null;
  identity_matches: boolean | null;
}
