import { describe, it, expect } from "vitest";
import { rosterHtml, identityHtml, fleetConfigHtml } from "./render";
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
  it("renders one switchable button per configured fleet", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "oab");
    const buttons = html.match(/class="cfg-fleet/g) ?? [];
    expect(buttons.length).toBe(FIXTURE_FLEET_CONFIG.fleets.length);
    expect(html).toContain('data-cluster="oab"');
    expect(html).toContain('data-cluster="oab-staging"');
  });

  it("marks the active cluster and no other", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "oab-staging");
    const active = html.match(/cfg-fleet is-active/g) ?? [];
    expect(active.length).toBe(1);
    // the active button is the staging one
    const idx = html.indexOf("oab-staging");
    expect(html.lastIndexOf("is-active", idx)).toBeGreaterThan(-1);
  });

  it("shows the profile and region as the credential line", () => {
    const html = fleetConfigHtml(FIXTURE_FLEET_CONFIG, "oab");
    expect(html).toContain("orca-prod");
    expect(html).toContain("ap-east-2");
  });

  it("falls back to 'default chain' when a fleet has no profile", () => {
    const cfg = structuredClone(FIXTURE_FLEET_CONFIG);
    cfg.fleets[0].profile = null;
    cfg.fleets[0].region = null;
    expect(fleetConfigHtml(cfg, "oab")).toContain("default chain");
  });

  it("renders an empty state with the config path when no fleets", () => {
    const html = fleetConfigHtml(
      { path: "~/.config/oab-studio/fleets.toml", default_cluster: "oab", fleets: [] },
      "oab",
    );
    expect(html).toContain("No fleets configured");
    expect(html).toContain("fleets.toml");
    expect(html).not.toContain("cfg-fleet");
  });

  it("renders an unavailable state for null", () => {
    expect(fleetConfigHtml(null, "oab")).toContain("fleet config unavailable");
  });

  it("escapes fleet fields", () => {
    const cfg = structuredClone(FIXTURE_FLEET_CONFIG);
    cfg.fleets[0].name = "<x>";
    const html = fleetConfigHtml(cfg, "oab");
    expect(html).toContain("&lt;x&gt;");
    expect(html).not.toContain("<x>");
  });
});
