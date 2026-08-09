import type { Deployment } from "./types";

// Stand-in data so the console renders without a live core. Mirrors the shape
// studio-cp's `deploy_list` / `deploy_get` return. Swapped for the Tauri source
// in the desktop shell (slice-2).
export const FIXTURE_DEPLOYMENTS: Deployment[] = [
  {
    name: "orca",
    namespace: "prod",
    desired: 1,
    current: 1,
    ready: 1,
    instances: [{ id: "task/oab/orca-1", state: "Running" }],
  },
  {
    name: "mira",
    namespace: "prod",
    desired: 1,
    current: 1,
    ready: 0,
    instances: [{ id: "task/oab/mira-1", state: "Unhealthy" }],
  },
  {
    name: "kirin",
    namespace: "work",
    desired: 1,
    current: 1,
    ready: 0,
    instances: [{ id: "task/oab/kirin-1", state: "Starting" }],
  },
  {
    name: "falcon",
    namespace: "work",
    desired: 0,
    current: 0,
    ready: 0,
    instances: [],
  },
];
