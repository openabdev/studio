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
