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
