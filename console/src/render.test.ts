import { describe, it, expect } from "vitest";
import { rosterHtml } from "./render";
import { FIXTURE_DEPLOYMENTS } from "./fixtures";
import { AGENT_STATES, type Deployment } from "./types";

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
