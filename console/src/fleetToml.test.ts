import { describe, it, expect } from "vitest";
import { appendMember, appendFleetBlock, fleetBlockExists, removeFleetBlock } from "./fleetToml";

describe("appendMember", () => {
  const text = `default_cluster = "oab"

[fleet.oab-prod-orca]
members = ["oab-default-agent-1", "oab-default-agent-2"]
region = "ap-east-2"
profile = "oab-fleet"

[fleet.oab-prod-mira]
members = ["oab-default-mira-1"]
`;

  it("appends the new member to the named fleet's array", () => {
    const out = appendMember(text, "oab-prod-orca", "oab-default-agent-3");
    expect(out).toContain(
      'members = ["oab-default-agent-1", "oab-default-agent-2", "oab-default-agent-3"]',
    );
  });

  it("leaves region/profile and the rest of the file untouched", () => {
    const out = appendMember(text, "oab-prod-orca", "oab-default-agent-3");
    expect(out).toContain('region = "ap-east-2"');
    expect(out).toContain('profile = "oab-fleet"');
    expect(out).toContain('[fleet.oab-prod-mira]\nmembers = ["oab-default-mira-1"]');
  });

  it("only edits the targeted fleet's members array", () => {
    const out = appendMember(text, "oab-prod-mira", "oab-default-mira-2");
    expect(out).toContain('members = ["oab-default-mira-1", "oab-default-mira-2"]');
    expect(out).toContain('members = ["oab-default-agent-1", "oab-default-agent-2"]');
  });

  it("is a no-op when the member is already listed", () => {
    const out = appendMember(text, "oab-prod-orca", "oab-default-agent-1");
    expect(out).toBe(text);
  });

  it("is a no-op when the fleet isn't found", () => {
    const out = appendMember(text, "no-such-fleet", "x");
    expect(out).toBe(text);
  });

  it("inserts a members line when the block doesn't have one", () => {
    const noMembers = `[fleet.empty-fleet]\nregion = "ap-east-2"\n`;
    const out = appendMember(noMembers, "empty-fleet", "oab-default-a1");
    expect(out).toContain('members = ["oab-default-a1"]');
    expect(out).toContain('region = "ap-east-2"');
  });
});

describe("fleetBlockExists", () => {
  const text = `default_cluster = "oab"

[fleet.oab-prod-orca]
members = ["oab-default-agent-1"]
`;

  it("is true when a [fleet.<name>] block is present", () => {
    expect(fleetBlockExists(text, "oab-prod-orca")).toBe(true);
  });

  it("is false when the name isn't present", () => {
    expect(fleetBlockExists(text, "no-such-fleet")).toBe(false);
  });

  it("is false against an empty file", () => {
    expect(fleetBlockExists("", "oab-prod-orca")).toBe(false);
  });
});

describe("appendFleetBlock", () => {
  it("appends a new ecs [fleet.<name>] block with the given fields", () => {
    const out = appendFleetBlock("default_cluster = \"oab\"\n", {
      name: "support-fleet",
      member: "oab-default-support-bot-1",
      expectedPrincipal: "arn:aws:iam::123:role/oab-fleet",
      runtime: { kind: "ecs", region: "ap-east-2", profile: "oab-fleet" },
    });
    expect(out).toContain("[fleet.support-fleet]");
    expect(out).toContain('runtime = "ecs"');
    expect(out).toContain('members = ["oab-default-support-bot-1"]');
    expect(out).toContain('region = "ap-east-2"');
    expect(out).toContain('profile = "oab-fleet"');
    expect(out).toContain('expected_principal = "arn:aws:iam::123:role/oab-fleet"');
  });

  it("appends a new k8s [fleet.<name>] block with context, namespace, members, expected_principal", () => {
    const out = appendFleetBlock('default_cluster = "oab"\n', {
      name: "orbstack-dev",
      member: "oab-dev-scratch-agent",
      expectedPrincipal: "system:serviceaccount:dev:oab-agent",
      runtime: { kind: "k8s", context: "orbstack", namespace: "dev" },
    });
    expect(out).toContain("[fleet.orbstack-dev]");
    expect(out).toContain('runtime = "k8s"');
    expect(out).toContain('context = "orbstack"');
    expect(out).toContain('namespace = "dev"');
    expect(out).toContain('members = ["oab-dev-scratch-agent"]');
    expect(out).toContain('expected_principal = "system:serviceaccount:dev:oab-agent"');
  });

  it("omits optional ecs fields that weren't provided", () => {
    const out = appendFleetBlock("", {
      name: "support-fleet",
      member: "oab-default-support-bot-1",
      expectedPrincipal: null,
      runtime: { kind: "ecs", region: null, profile: null },
    });
    expect(out).not.toContain("region =");
    expect(out).not.toContain("profile =");
    expect(out).not.toContain("expected_principal =");
  });

  it("omits context but always writes namespace for k8s", () => {
    const out = appendFleetBlock("", {
      name: "orca-k8s",
      member: "oab-prod-orca",
      expectedPrincipal: null,
      runtime: { kind: "k8s", context: null, namespace: "prod" },
    });
    expect(out).not.toContain("context =");
    expect(out).not.toContain("expected_principal =");
    expect(out).toContain('namespace = "prod"');
  });

  it("separates the new block from existing content with exactly one blank line", () => {
    const out = appendFleetBlock('default_cluster = "oab"\n', {
      name: "x",
      member: "m",
      expectedPrincipal: null,
      runtime: { kind: "ecs", region: null, profile: null },
    });
    expect(out).toBe('default_cluster = "oab"\n\n[fleet.x]\nruntime = "ecs"\nmembers = ["m"]\n');
  });
});

describe("removeFleetBlock", () => {
  const text = `default_cluster = "oab"

[fleet.a]
members = ["oab-default-a1"]

[fleet.b]
members = ["oab-default-b1"]

[fleet.c]
members = ["oab-default-c1"]
`;

  it("removes a middle block, leaving one blank line between its neighbors", () => {
    const out = removeFleetBlock(text, "b");
    expect(out).toBe(
      'default_cluster = "oab"\n\n[fleet.a]\nmembers = ["oab-default-a1"]\n\n[fleet.c]\nmembers = ["oab-default-c1"]\n',
    );
  });

  it("removes the first block with no leading blank line left behind", () => {
    const out = removeFleetBlock(text, "a");
    expect(out).toBe(
      'default_cluster = "oab"\n\n[fleet.b]\nmembers = ["oab-default-b1"]\n\n[fleet.c]\nmembers = ["oab-default-c1"]\n',
    );
  });

  it("removes the last block with no trailing blank line left behind", () => {
    const out = removeFleetBlock(text, "c");
    expect(out).toBe(
      'default_cluster = "oab"\n\n[fleet.a]\nmembers = ["oab-default-a1"]\n\n[fleet.b]\nmembers = ["oab-default-b1"]\n',
    );
  });

  it("removes the only fleet block, leaving the rest of the file intact", () => {
    const onlyOne = 'default_cluster = "oab"\n\n[fleet.a]\nmembers = ["oab-default-a1"]\n';
    expect(removeFleetBlock(onlyOne, "a")).toBe('default_cluster = "oab"\n');
  });

  it("is a no-op when the fleet isn't found", () => {
    expect(removeFleetBlock(text, "no-such-fleet")).toBe(text);
  });
});
