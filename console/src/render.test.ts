import { describe, it, expect } from "vitest";
import {
  rosterHtml,
  identityHtml,
  fleetConfigHtml,
  filterByMembers,
  serviceName,
} from "./render";
import {
  FIXTURE_DEPLOYMENTS,
  FIXTURE_FLEET_CONFIG,
  FIXTURE_RUNTIME_CONTEXT,
} from "./fixtures";
import { AGENT_STATES, type Deployment, type RuntimeContext } from "./types";

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
