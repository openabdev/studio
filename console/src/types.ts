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
