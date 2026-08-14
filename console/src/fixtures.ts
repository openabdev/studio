import type { Deployment, FleetConfig, RuntimeContext } from "./types";

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

// Stand-in identity so the browser build renders the panel without a core.
// A healthy example: a task role that matches its binding's expectation.
export const FIXTURE_RUNTIME_CONTEXT: RuntimeContext = {
  cluster: "oab",
  principal:
    "arn:aws:sts::504190915686:assumed-role/openab-orca-task-role/session",
  principal_kind: "role",
  scope: "504190915686",
  location: "ap-east-2",
  source: "container-credentials (task/pod role)",
  caller_id: "AROAEXAMPLE:session",
  binding: {
    name: "prod",
    profile: null,
    region: "ap-east-2",
    expected_principal:
      "arn:aws:iam::504190915686:role/openab-orca-task-role",
  },
  expected_principal: "arn:aws:iam::504190915686:role/openab-orca-task-role",
  identity_matches: true,
};

// Stand-in fleet-binding config so the browser build renders the config panel
// without a core. Two fleets that **share the `oab` cluster** (and one
// credential) but list different `members` — the exact "group by usage, not by
// cluster" shape the panel lets the operator switch between and filter the
// roster by.
export const FIXTURE_FLEET_CONFIG: FleetConfig = {
  path: "~/.config/oab-studio/fleets.toml",
  default_cluster: "oab",
  fleets: [
    {
      name: "orca",
      cluster: "oab",
      members: ["oab-prod-orca"],
      region: "ap-east-2",
      profile: "oab-fleet",
      expected_principal:
        "arn:aws:iam::504190915686:role/openab-orca-task-role",
    },
    {
      name: "mira",
      cluster: "oab",
      members: ["oab-prod-mira"],
      region: "ap-east-2",
      profile: "oab-fleet",
      expected_principal: null,
    },
  ],
  text: `# OAB Studio fleet bindings — which credential manages which fleet.
# A fleet is a usage-based group: orca and mira share the oab cluster (one
# credential) but list different members.

[fleet.orca]
cluster = "oab"
members = ["oab-prod-orca"]
region = "ap-east-2"
profile = "oab-fleet"
expected_principal = "arn:aws:iam::504190915686:role/openab-orca-task-role"

[fleet.mira]
cluster = "oab"
members = ["oab-prod-mira"]
region = "ap-east-2"
profile = "oab-fleet"
`,
};
