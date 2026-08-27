import { describe, it, expect } from "vitest";
import { appendMember, appendK8sFleetBlock } from "./fleetsK8sToml";

describe("appendMember (reused from fleetToml.ts)", () => {
  it("works against fleets-k8s.toml's [fleet.<name>] shape too", () => {
    const text = `[fleet.orbstack-dev]
context = "orbstack"
namespace = "dev"
members = ["scratch-agent"]
`;
    const out = appendMember(text, "orbstack-dev", "scratch-agent-2");
    expect(out).toContain('members = ["scratch-agent", "scratch-agent-2"]');
    expect(out).toContain('context = "orbstack"');
    expect(out).toContain('namespace = "dev"');
  });
});

describe("appendK8sFleetBlock", () => {
  it("appends a new [fleet.<name>] block with context, namespace, members, expected_principal", () => {
    const out = appendK8sFleetBlock('default_cluster = "oab"\n', {
      name: "orbstack-dev",
      member: "oab-dev-scratch-agent",
      context: "orbstack",
      namespace: "dev",
      expectedPrincipal: "system:serviceaccount:dev:oab-agent",
    });
    expect(out).toContain("[fleet.orbstack-dev]");
    expect(out).toContain('context = "orbstack"');
    expect(out).toContain('namespace = "dev"');
    expect(out).toContain('members = ["oab-dev-scratch-agent"]');
    expect(out).toContain('expected_principal = "system:serviceaccount:dev:oab-agent"');
  });

  it("omits context and expected_principal when not provided, but always writes namespace", () => {
    const out = appendK8sFleetBlock("", {
      name: "orca-k8s",
      member: "oab-prod-orca",
      context: null,
      namespace: "prod",
      expectedPrincipal: null,
    });
    expect(out).not.toContain("context =");
    expect(out).not.toContain("expected_principal =");
    expect(out).toContain('namespace = "prod"');
  });

  it("separates the new block from existing content with exactly one blank line", () => {
    const out = appendK8sFleetBlock('default_cluster = "oab"\n', {
      name: "x",
      member: "m",
      context: null,
      namespace: "ns",
      expectedPrincipal: null,
    });
    expect(out).toBe('default_cluster = "oab"\n\n[fleet.x]\nnamespace = "ns"\nmembers = ["m"]\n');
  });
});
