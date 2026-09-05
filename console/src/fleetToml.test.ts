import { describe, it, expect } from "vitest";
import { appendMember, appendFleetBlock, fleetBlockExists } from "./fleetToml";

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
  it("appends a new [fleet.<name>] block with the given fields", () => {
    const out = appendFleetBlock("default_cluster = \"oab\"\n", {
      name: "support-fleet",
      member: "oab-default-support-bot-1",
      region: "ap-east-2",
      profile: "oab-fleet",
      expectedPrincipal: "arn:aws:iam::123:role/oab-fleet",
    });
    expect(out).toContain("[fleet.support-fleet]");
    expect(out).toContain('members = ["oab-default-support-bot-1"]');
    expect(out).toContain('region = "ap-east-2"');
    expect(out).toContain('profile = "oab-fleet"');
    expect(out).toContain('expected_principal = "arn:aws:iam::123:role/oab-fleet"');
  });

  it("omits optional fields that weren't provided", () => {
    const out = appendFleetBlock("", {
      name: "support-fleet",
      member: "oab-default-support-bot-1",
      region: null,
      profile: null,
      expectedPrincipal: null,
    });
    expect(out).not.toContain("region =");
    expect(out).not.toContain("profile =");
    expect(out).not.toContain("expected_principal =");
  });

  it("separates the new block from existing content with exactly one blank line", () => {
    const out = appendFleetBlock('default_cluster = "oab"\n', {
      name: "x",
      member: "m",
      region: null,
      profile: null,
      expectedPrincipal: null,
    });
    expect(out).toBe('default_cluster = "oab"\n\n[fleet.x]\nmembers = ["m"]\n');
  });
});
