import { describe, it, expect } from "vitest";
import {
  rosterHtml,
  identityHtml,
  fleetConfigHtml,
  fleetDetailHeaderHtml,
  remoteHtml,
  agentListHtml,
  agentConsoleHeaderHtml,
  fsListingHtml,
  fsUnavailableHtml,
  filterByMembers,
  serviceName,
  deploymentKey,
} from "./render";
import {
  FIXTURE_AGENTS,
  FIXTURE_DEPLOYMENTS,
  FIXTURE_FLEET_CONFIG,
  FIXTURE_REMOTE_CONFIG,
  FIXTURE_RUNTIME_CONTEXT,
} from "./fixtures";
import {
  AGENT_STATES,
  type AgentEndpointView,
  type Deployment,
  type FsListing,
  type RuntimeContext,
} from "./types";

function ctx(partial: Partial<RuntimeContext>): RuntimeContext {
  return { ...structuredClone(FIXTURE_RUNTIME_CONTEXT), ...partial };
}

function dep(partial: Partial<Deployment>): Deployment {
  return {
    name: "x",
    namespace: "n",
    desired: 1,
    current: 1,
    ready: 1,
    instances: [],
    ...partial,
  };
}

describe("rosterHtml", () => {
  it("renders one row per deployment (plus the header row)", () => {
    const html = rosterHtml(FIXTURE_DEPLOYMENTS);
    const rows = html.match(/<tr>/g) ?? [];
    expect(rows.length).toBe(1 + FIXTURE_DEPLOYMENTS.length);
  });

  it("sorts rows by namespace/name", () => {
    const html = rosterHtml(FIXTURE_DEPLOYMENTS);
    expect(html.indexOf("prod/mira")).toBeLessThan(html.indexOf("prod/orca"));
    expect(html.indexOf("prod/orca")).toBeLessThan(html.indexOf("work/falcon"));
    expect(html.indexOf("work/falcon")).toBeLessThan(html.indexOf("work/kirin"));
  });

  it("badges each instance with its state class", () => {
    const html = rosterHtml(FIXTURE_DEPLOYMENTS);
    expect(html).toContain('class="badge s-running"');
    expect(html).toContain('class="badge s-unhealthy"');
    expect(html).toContain('class="badge s-starting"');
  });

  it("marks ready==desired ok and ready<desired warn", () => {
    const html = rosterHtml(FIXTURE_DEPLOYMENTS);
    expect(html).toContain('class="counts ok"');
    expect(html).toContain('class="counts warn"');
  });

  it("shows an em-dash when a deployment has no instances", () => {
    expect(rosterHtml([dep({ instances: [] })])).toContain("—");
  });

  it("renders an empty state for an empty roster", () => {
    expect(rosterHtml([])).toContain("No deployments");
  });

  it("has a badge class for every canonical state", () => {
    for (const state of AGENT_STATES) {
      const html = rosterHtml([dep({ instances: [{ id: "i", state }] })]);
      expect(html).toMatch(/class="badge s-[a-z]+"/);
    }
  });

  it("escapes deployment names", () => {
    const html = rosterHtml([dep({ namespace: "n", name: "<x>" })]);
    expect(html).toContain("&lt;x&gt;");
    expect(html).not.toContain("<x>");
  });

  it("offers Stop for a running deployment and Start for a stopped one", () => {
    const html = rosterHtml([
      dep({ name: "on", namespace: "prod", desired: 1 }),
      dep({ name: "off", namespace: "prod", desired: 0, instances: [] }),
    ]);
    expect(html).toContain('data-action="stop"');
    expect(html).toContain('data-action="start"');
    expect(html).toContain(">Stop</button>");
    expect(html).toContain(">Start</button>");
  });

  it("carries name + namespace on the action button for the scale call", () => {
    const html = rosterHtml([dep({ name: "orca", namespace: "prod" })]);
    expect(html).toContain('data-name="orca"');
    expect(html).toContain('data-namespace="prod"');
  });

  it("escapes name + namespace in action button data attributes", () => {
    const html = rosterHtml([dep({ name: '"x', namespace: "n" })]);
    expect(html).toContain("&quot;x");
    expect(html).not.toContain('data-name=""x"');
  });

  it("renders a disabled placeholder for a deployment with a scale in flight", () => {
    const d = dep({ name: "orca", namespace: "prod" });
    const html = rosterHtml([d], new Set([deploymentKey(d)]));
    expect(html).toContain("act-pending");
    expect(html).toContain("disabled");
    // no live action attributes on a pending button
    expect(html).not.toContain('data-action="stop"');
    expect(html).not.toContain('data-action="start"');
  });

  it("leaves non-pending deployments interactive when another is in flight", () => {
    const busy = dep({ name: "orca", namespace: "prod" });
    const free = dep({ name: "mira", namespace: "prod", desired: 0 });
    const html = rosterHtml([busy, free], new Set([deploymentKey(busy)]));
    // orca is pending → placeholder; mira is free → a live Start button
    expect(html).toContain("act-pending");
    expect(html).toContain('data-action="start"');
    expect(html).toContain('data-name="mira"');
  });

  it("defaults to no pending set (all buttons live)", () => {
    const html = rosterHtml([dep({ name: "orca", namespace: "prod" })]);
    expect(html).not.toContain("act-pending");
    expect(html).toContain('data-action="stop"');
  });
});

describe("deploymentKey", () => {
  it("is the namespace/name pair (not the ECS service name)", () => {
    expect(deploymentKey({ ...FIXTURE_DEPLOYMENTS[0] })).toBe("prod/orca");
  });
});

describe("identityHtml", () => {
  it("shows principal, account, region and the role kind badge", () => {
    const html = identityHtml(FIXTURE_RUNTIME_CONTEXT);
    expect(html).toContain('class="kind k-role"');
    expect(html).toContain("504190915686");
    expect(html).toContain("ap-east-2");
    expect(html).toContain("openab-orca-task-role");
  });

  it("flags a mismatch and shows the expected principal", () => {
    const html = identityHtml(
      ctx({
        principal: "arn:aws:iam::916371022086:user/brett.chien",
        principal_kind: "user",
        scope: "916371022086",
        identity_matches: false,
      }),
    );
    expect(html).toContain('class="identity mismatch"');
    expect(html).toContain("identity mismatch");
    expect(html).toContain("class=\"kind k-user\"");
    expect(html).toContain("arn:aws:iam::504190915686:role/openab-orca-task-role");
  });

  it("shows a matches verdict when identity_matches is true", () => {
    expect(identityHtml(ctx({ identity_matches: true }))).toContain(
      "matches expected",
    );
  });

  it("shows no verdict when there is no expectation", () => {
    const html = identityHtml(
      ctx({ identity_matches: null, expected_principal: null }),
    );
    expect(html).not.toContain("mismatch");
    expect(html).not.toContain("matches expected");
  });

  it("renders an unavailable state for null", () => {
    expect(identityHtml(null)).toContain("identity unavailable");
  });

  it("escapes the principal ARN", () => {
    const html = identityHtml(ctx({ principal: "<script>" }));
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("<script>");
  });
});

describe("fleetConfigHtml", () => {
  it("renders one button per fleet, switchable by name", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "orca");
    const buttons = html.match(/class="cfg-fleet/g) ?? [];
    expect(buttons.length).toBe(FIXTURE_FLEET_CONFIG.fleets.length);
    expect(html).toContain('data-fleet="orca"');
    expect(html).toContain('data-fleet="mira"');
  });

  it("switches by fleet identity, not cluster (two fleets share a cluster)", () => {
    // Both fixture fleets are on cluster "oab" — the switch key must be the name.
    expect(FIXTURE_FLEET_CONFIG.fleets.every((f) => f.cluster === "oab")).toBe(
      true,
    );
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "mira");
    const active = html.match(/cfg-fleet is-active/g) ?? [];
    expect(active.length).toBe(1);
    // the active button is the mira one
    const idx = html.indexOf('data-fleet="mira"');
    expect(html.lastIndexOf("is-active", idx)).toBeGreaterThan(-1);
  });

  it("marks no fleet active when the selection is null", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, null);
    expect(html).not.toContain("is-active");
  });

  it("renders each fleet's members", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "orca");
    expect(html).toContain("oab-prod-orca");
    expect(html).toContain("oab-prod-mira");
    expect(html).toContain('class="cfg-member"');
  });

  it("shows 'whole cluster' when a fleet has no explicit members", () => {
    const cfg = structuredClone(FIXTURE_FLEET_CONFIG);
    cfg.fleets[0].members = [];
    const html = fleetConfigHtml(cfg, "orca");
    expect(html).toContain("whole cluster");
  });

  it("shows the profile and region as the credential line", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "orca");
    expect(html).toContain("oab-fleet");
    expect(html).toContain("ap-east-2");
  });

  it("falls back to 'default chain' when a fleet has no profile", () => {
    const cfg = structuredClone(FIXTURE_FLEET_CONFIG);
    cfg.fleets[0].profile = null;
    cfg.fleets[0].region = null;
    expect(fleetConfigHtml(cfg, "orca")).toContain("default chain");
  });

  it("renders an empty state with the config path when no fleets", () => {
    const html = fleetConfigHtml(
      {
        path: "~/.config/oab-studio/fleets.toml",
        default_cluster: "oab",
        fleets: [],
        text: "",
      },
      null,
    );
    expect(html).toContain("No fleets configured");
    expect(html).toContain("fleets.toml");
    expect(html).not.toContain("cfg-fleet");
  });

  it("always offers the Edit config action (even with fleets)", () => {
    expect(fleetConfigHtml(FIXTURE_FLEET_CONFIG, "orca")).toContain(
      'data-action="edit-config"',
    );
  });

  it("renders an unavailable state for null", () => {
    expect(fleetConfigHtml(null, null)).toContain("fleet config unavailable");
  });

  it("escapes fleet fields", () => {
    const cfg = structuredClone(FIXTURE_FLEET_CONFIG);
    cfg.fleets[0].name = "<x>";
    const html = fleetConfigHtml(cfg, "orca");
    expect(html).toContain("&lt;x&gt;");
    expect(html).not.toContain("<x>");
  });
});

describe("fleetDetailHeaderHtml", () => {
  it("renders the breadcrumb back to Fleets and the fleet name", () => {
    const html = fleetDetailHeaderHtml("oab-prod-orca");
    expect(html).toContain('data-action="back-to-fleets"');
    expect(html).toContain("oab-prod-orca");
  });

  it("renders the deploy and debug-drawer entry points disabled (slice 2 scope)", () => {
    const html = fleetDetailHeaderHtml("oab-prod-orca");
    expect(html).toContain('data-action="add-instance" disabled');
    expect(html).toContain('data-action="fleet-debug" disabled');
  });

  it("escapes the fleet name", () => {
    const html = fleetDetailHeaderHtml("<x>");
    expect(html).toContain("&lt;x&gt;");
    expect(html).not.toContain("<x>");
  });
});

describe("remoteHtml", () => {
  it("shows the endpoint and an Activate button when configured + disconnected", () => {
    const html = remoteHtml(FIXTURE_REMOTE_CONFIG);
    expect(html).toContain("wss://gateway.example/acp");
    expect(html).toContain('data-action="remote-connect"');
    expect(html).toContain("Activate remote connection");
    expect(html).not.toContain("disabled");
    // status badge reflects the state
    expect(html).toContain("rm-disconnected");
  });

  it("disables Activate until url + token are configured", () => {
    const html = remoteHtml({
      ...FIXTURE_REMOTE_CONFIG,
      configured: false,
      url: "",
    });
    expect(html).toContain('data-action="remote-connect"');
    expect(html).toContain("disabled");
    expect(html).toContain("not configured");
  });

  it("shows Disconnect (not Activate) while connected", () => {
    const html = remoteHtml({ ...FIXTURE_REMOTE_CONFIG, status: "connected" });
    expect(html).toContain('data-action="remote-disconnect"');
    expect(html).toContain("rm-connected");
    expect(html).not.toContain('data-action="remote-connect"');
  });

  it("marks an error status", () => {
    const html = remoteHtml({
      ...FIXTURE_REMOTE_CONFIG,
      status: "error: dial refused",
    });
    expect(html).toContain("rm-error");
    expect(html).toContain("error: dial refused");
    // an error is not "live", so it offers Activate again
    expect(html).toContain('data-action="remote-connect"');
  });

  it("always offers Edit config", () => {
    expect(remoteHtml(FIXTURE_REMOTE_CONFIG)).toContain(
      'data-action="edit-remote-config"',
    );
  });

  it("labels the registry path (agents.toml) that Edit config opens, not remote.toml", () => {
    const html = remoteHtml(
      { ...FIXTURE_REMOTE_CONFIG, path: "~/.config/oab-studio/remote.toml" },
      "~/.config/oab-studio/agents.toml",
    );
    expect(html).toContain("agents.toml");
    expect(html).not.toContain("remote.toml");
  });

  it("omits the path label when no registry path is known (no mislabel)", () => {
    const html = remoteHtml({
      ...FIXTURE_REMOTE_CONFIG,
      path: "~/.config/oab-studio/remote.toml",
    });
    expect(html).not.toContain("cfg-path");
    expect(html).not.toContain("remote.toml");
  });

  it("renders an unavailable state for null", () => {
    expect(remoteHtml(null)).toContain("remote connection unavailable");
  });

  it("escapes the url", () => {
    const html = remoteHtml({ ...FIXTURE_REMOTE_CONFIG, url: "wss://<x>/acp" });
    expect(html).toContain("&lt;x&gt;");
    expect(html).not.toContain("<x>/acp");
  });
});

describe("filterByMembers", () => {
  it("keeps only deployments whose ECS service name is a member", () => {
    const kept = filterByMembers(FIXTURE_DEPLOYMENTS, ["oab-prod-orca"]);
    expect(kept.map((d) => d.name)).toEqual(["orca"]);
  });

  it("matches multiple members across the roster", () => {
    const kept = filterByMembers(FIXTURE_DEPLOYMENTS, [
      "oab-prod-orca",
      "oab-prod-mira",
    ]);
    expect(kept.map((d) => d.name).sort()).toEqual(["mira", "orca"]);
  });

  it("treats an empty member list as the whole cluster (unfiltered)", () => {
    expect(filterByMembers(FIXTURE_DEPLOYMENTS, [])).toEqual(
      FIXTURE_DEPLOYMENTS,
    );
  });

  it("also accepts the short deployment name as a member (like resolve_service)", () => {
    const kept = filterByMembers(FIXTURE_DEPLOYMENTS, ["kirin"]);
    expect(kept.map((d) => d.name)).toEqual(["kirin"]);
  });

  it("drops everything when no member matches", () => {
    expect(filterByMembers(FIXTURE_DEPLOYMENTS, ["oab-prod-nobody"])).toEqual(
      [],
    );
  });

  it("derives the ECS service name as oab-{namespace}-{name}", () => {
    expect(serviceName({ ...FIXTURE_DEPLOYMENTS[0] })).toBe("oab-prod-orca");
  });
});

describe("agentListHtml", () => {
  function ep(partial: Partial<AgentEndpointView>): AgentEndpointView {
    return {
      name: "x",
      url: "wss://x.example/acp",
      cwd: "/home/node",
      management: false,
      configured: true,
      status: "disconnected",
      ...partial,
    };
  }

  it("renders an empty-state pointing at agents.toml when the registry is empty", () => {
    const html = agentListHtml([], null);
    expect(html).toContain("ag-empty");
    expect(html).toContain("agents.toml");
  });

  it("makes an ordinary configured endpoint an openable button", () => {
    const html = agentListHtml([ep({ name: "mira" })], null);
    expect(html).toContain('data-agent="mira"');
    expect(html).toContain("<button");
    expect(html).not.toContain("disabled");
  });

  it("disables an unconfigured endpoint (no url+token to dial)", () => {
    const html = agentListHtml([ep({ name: "falcon", configured: false, url: "" })], null);
    expect(html).toContain('data-agent="falcon"');
    expect(html).toContain("disabled");
    expect(html).toContain("not configured");
  });

  it("shows the management endpoint but does not make it openable", () => {
    const html = agentListHtml([ep({ name: "orca", management: true })], null);
    // no data-agent hook → the delegated open handler can't fire for it
    expect(html).not.toContain('data-agent="orca"');
    expect(html).toContain("management");
    expect(html).toContain("console above");
  });

  it("marks the currently open console as pressed", () => {
    const html = agentListHtml([ep({ name: "mira" })], "mira");
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain("is-open");
  });

  it("renders every fixture endpoint", () => {
    const html = agentListHtml(FIXTURE_AGENTS, null);
    for (const a of FIXTURE_AGENTS) expect(html).toContain(a.name);
  });

  it("escapes endpoint names and urls", () => {
    const html = agentListHtml(
      [ep({ name: "a<b", url: "wss://x/?q=<script>" })],
      null,
    );
    expect(html).not.toContain("<script>");
    expect(html).toContain("a&lt;b");
  });
});

describe("agentConsoleHeaderHtml", () => {
  const orca = FIXTURE_AGENTS[0];

  it("shows the endpoint's identity, dial target, and the passed live status", () => {
    const html = agentConsoleHeaderHtml(orca, "connected");
    expect(html).toContain(orca.name);
    expect(html).toContain(orca.url);
    expect(html).toContain(orca.cwd);
    expect(html).toContain("rm-connected");
  });

  it("never leaks a token field (secrets don't cross the bridge)", () => {
    const html = agentConsoleHeaderHtml(orca, "connected");
    expect(html.toLowerCase()).not.toContain("token");
  });

  it("notes the read-only editor limitation until the fs MCP files server lands", () => {
    expect(agentConsoleHeaderHtml(orca, "disconnected")).toContain("Read-only");
  });
});

describe("fsListingHtml", () => {
  const listing: FsListing = {
    path: "/home/node",
    entries: [
      { name: "notes.md", path: "/home/node/notes.md", kind: "file", size: 140 },
      { name: "agent_profiling", path: "/home/node/agent_profiling", kind: "dir" },
      { name: "CLAUDE.md", path: "/home/node/CLAUDE.md", kind: "file", size: 2048 },
    ],
  };

  it("sorts directories before files, each alphabetically", () => {
    const html = fsListingHtml(listing);
    const iDir = html.indexOf("agent_profiling");
    const iClaude = html.indexOf("CLAUDE.md");
    const iNotes = html.indexOf("notes.md");
    expect(iDir).toBeLessThan(iClaude); // dir before any file
    expect(iClaude).toBeLessThan(iNotes); // files alphabetical
  });

  it("hooks dirs and files with the right navigation attributes", () => {
    const html = fsListingHtml(listing);
    expect(html).toContain('data-fs-dir="/home/node/agent_profiling"');
    expect(html).toContain('data-fs-file="/home/node/CLAUDE.md"');
  });

  it("shows a human-readable size for files only", () => {
    const html = fsListingHtml(listing);
    expect(html).toContain("2.0 KB"); // CLAUDE.md
    expect(html).toContain("140 B"); // notes.md
  });

  it("renders the breadcrumb path", () => {
    expect(fsListingHtml(listing)).toContain("/home/node");
  });

  it("marks the open file", () => {
    const html = fsListingHtml(listing, { selectedPath: "/home/node/CLAUDE.md" });
    expect(html).toMatch(/is-open[^>]*data-fs-file="\/home\/node\/CLAUDE\.md"/);
  });

  it("shows an up affordance only when canGoUp", () => {
    expect(fsListingHtml(listing, { canGoUp: true })).toContain("data-fs-up");
    expect(fsListingHtml(listing, { canGoUp: false })).not.toContain("data-fs-up");
  });

  it("renders an empty-directory note when there are no entries and no up", () => {
    expect(fsListingHtml({ path: "/x", entries: [] })).toContain("empty directory");
  });

  it("escapes entry names and paths", () => {
    const html = fsListingHtml({
      path: "/x",
      entries: [{ name: "<script>", path: "/x/<script>", kind: "file" }],
    });
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
});

describe("fsUnavailableHtml", () => {
  it("renders the pending reason, escaped", () => {
    const html = fsUnavailableHtml("pending the fs MCP files server");
    expect(html).toContain("fs-unavailable");
    expect(html).toContain("pending the fs MCP files server");
  });
});
