import type {
  AgentEndpointView,
  Deployment,
  FleetConfig,
  RegistryConfig,
  RemoteConfig,
  RuntimeContext,
} from "./types";

// Stand-in endpoint registry so the browser build renders the agent-console
// selector without a core. Mirrors src-tauri's `remote_agents`: one management
// entry (backs the top-level console + reverse-MCP grant) plus ordinary agent
// consoles. Tokens are never present — only whether each entry is configured.
export const FIXTURE_AGENTS: AgentEndpointView[] = [
  {
    name: "orca",
    url: "wss://orca-acp.example/acp",
    cwd: "/home/node",
    management: true,
    configured: true,
    status: "disconnected",
  },
  {
    name: "mira",
    url: "wss://mira-acp.example/acp",
    cwd: "/home/node",
    management: false,
    configured: true,
    status: "disconnected",
  },
  {
    name: "falcon",
    url: "",
    cwd: "/home/node",
    management: false,
    configured: false,
    status: "disconnected",
  },
];

// Stand-in remote-connection view so the browser build renders the remote panel
// without a core. A configured-but-disconnected example — the shape the panel
// shows before the operator hits "Activate".
export const FIXTURE_REMOTE_CONFIG: RemoteConfig = {
  path: "~/.config/oab-studio/remote.toml",
  text: `# OAB Studio — remote reverse-MCP connection (the /acp endpoint Studio dials)
url = "wss://gateway.example/acp"
token = "…"
cwd = "/"
`,
  url: "wss://gateway.example/acp",
  configured: true,
  status: "disconnected",
};

// Stand-in registry file so the browser build's "Edit config" opens a realistic
// `agents.toml` (one management entry + ordinary agent consoles). Mirrors
// FIXTURE_AGENTS; unlike the panel views this is the raw file, so tokens appear.
export const FIXTURE_REGISTRY_CONFIG: RegistryConfig = {
  path: "~/.config/oab-studio/agents.toml",
  text: `# OAB Studio — per-agent endpoint registry (agents.toml)
# One entry per /acp endpoint. Rules: names unique; at most one management = true
# (it backs the management console + the reverse-MCP grant).

[[agent]]
name = "orca"
url = "wss://orca-acp.example/acp"
token = "…"
cwd = "/home/node"
management = true

[[agent]]
name = "mira"
url = "wss://mira-acp.example/acp"
token = "…"
cwd = "/home/node"
`,
};

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
