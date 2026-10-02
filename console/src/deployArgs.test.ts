// studio#111: the "+ New fleet" / "+ Add instance" wizard's single
// `deploy_provision_agent` call is where #111's create-or-redeploy branch is
// reached from — a brand-new agent (no stored manifest) gets a freshly built
// `OABServiceManifest` applied through `apply_manifests`, an existing one takes
// `redeploy`'s patch path. That decision is the sidecar's, but everything it
// needs to make it — and to build a correct first manifest — arrives in this
// one argument object, so the object is pinned here rather than left implicit
// in the panel's submit handler.
//
// The AWS-credential half is the load-bearing one (studio#111): the wizard's
// identity step already collects Region + Credential profile and writes them
// into `fleets.toml`, but oab-mcp resolves a fleet's credential by the fleet a
// call names (`fleet:`), else by a binding's `cluster` key, and this wizard
// names neither — so the sidecar falls back to the ambient `[default]` chain.
// On a first create there is no binding at all
// yet (`fleets.toml` is only written *after* a confirmed successful deploy),
// so the first manifest's VPC/subnet/security-group discovery — and the apply
// itself — would run against whatever account the ambient chain happens to
// resolve. The operator's chosen region/profile have to ride along with the
// call.

import { describe, it, expect } from "vitest";
import { provisionAgentArgs, awsIdentityFor } from "./deployArgs";
import { appendFleetBlock } from "./fleetToml";

const ecs = {
  image: "ghcr.io/openabdev/openab:0.10.0-beta.3-codex",
  name: "zeus",
  namespace: "default",
};

describe("provisionAgentArgs — the shape every provision call must have", () => {
  it("always names the image, agent and namespace", () => {
    const args = provisionAgentArgs({ ...ecs, acpEnabled: true });
    expect(args.image).toBe(ecs.image);
    expect(args.name).toBe("zeus");
    expect(args.namespace).toBe("default");
  });

  it("never sends a k8s dispatch for an ECS submit", () => {
    const args = provisionAgentArgs({ ...ecs, acpEnabled: true });
    expect(args.provider).toBeUndefined();
    expect(args.context).toBeUndefined();
    expect(args.expected_principal).toBeUndefined();
  });
});

describe("provisionAgentArgs — AWS credentials (studio#111)", () => {
  it("forwards the region and credential profile the wizard collected", () => {
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      region: "ap-northeast-1",
      profile: "studio-prod",
    });
    expect(args.region).toBe("ap-northeast-1");
    expect(args.profile).toBe("studio-prod");
  });

  it("omits blank credentials rather than sending empty strings", () => {
    const args = provisionAgentArgs({ ...ecs, acpEnabled: true, region: "  ", profile: "" });
    expect("region" in args).toBe(false);
    expect("profile" in args).toBe(false);
  });

  it("keeps a region-only submit from inventing a profile", () => {
    const args = provisionAgentArgs({ ...ecs, acpEnabled: true, region: "eu-west-1" });
    expect(args.region).toBe("eu-west-1");
    expect("profile" in args).toBe(false);
  });
});

describe("provisionAgentArgs — k8s dispatch (studio#104/#153)", () => {
  const k8s = {
    ...ecs,
    namespace: "persephone",
    acpEnabled: true,
    k8s: { context: "orbstack", expectedPrincipal: "system:serviceaccount:persephone:runner" },
  };

  it("passes provider/context/expected_principal together", () => {
    const args = provisionAgentArgs(k8s);
    expect(args.provider).toBe("k8s");
    expect(args.context).toBe("orbstack");
    expect(args.expected_principal).toBe("system:serviceaccount:persephone:runner");
  });

  it("omits an unset context so the kubeconfig current-context applies", () => {
    const args = provisionAgentArgs({ ...k8s, k8s: { context: undefined, expectedPrincipal: undefined } });
    expect(args.provider).toBe("k8s");
    expect("context" in args).toBe(false);
    expect("expected_principal" in args).toBe(false);
  });

  it("never sends AWS credentials on a k8s submit", () => {
    // A k8s pod has no AWS credential chain (studio#104/#128), so a region or
    // profile left over from the identity step's AWS fields must not travel
    // with the call and mislead the sidecar into resolving one anyway.
    const args = provisionAgentArgs({ ...k8s, region: "ap-northeast-1", profile: "studio-prod" });
    expect("region" in args).toBe(false);
    expect("profile" in args).toBe(false);
  });
});

describe("awsIdentityFor — which source answers a submit (studio#111)", () => {
  it("reads new-fleet's own identity step fields", () => {
    expect(
      awsIdentityFor({ kind: "new-fleet", region: "ap-northeast-1", profile: "studio-prod" }),
    ).toEqual({ region: "ap-northeast-1", profile: "studio-prod" });
  });

  it("inherits add-instance's fleet-recorded pair, not an empty form", () => {
    // The panel has no AWS field group in add-instance mode, so reading the
    // form there would ship "no override" for a fleet that has one recorded.
    expect(
      awsIdentityFor({ kind: "add-instance", region: "eu-west-1", profile: "prod-admin" }),
    ).toEqual({ region: "eu-west-1", profile: "prod-admin" });
  });

  it("treats an unrecorded fleet pair as no override at all", () => {
    // Both undefined, not empty strings: the sidecar must keep resolving the
    // credential itself rather than being handed a blank region/profile.
    expect(awsIdentityFor({ kind: "add-instance", region: null, profile: null })).toEqual({
      region: undefined,
      profile: undefined,
    });
  });

  it("trims surrounding whitespace but keeps a real value", () => {
    expect(
      awsIdentityFor({ kind: "new-fleet", region: "  ap-northeast-1  ", profile: " studio-prod " }),
    ).toEqual({ region: "ap-northeast-1", profile: "studio-prod" });
  });

  it("drops a whitespace-only field", () => {
    expect(
      awsIdentityFor({ kind: "new-fleet", region: "   ", profile: "  studio-prod " }),
    ).toEqual({ region: undefined, profile: "studio-prod" });
  });
});

describe("the two seams compose — what an add-instance submit actually sends", () => {
  it("takes region/profile from the fleet binding, not from empty form fields", () => {
    // The panel's submit handler is exactly this composition; `mode.region` /
    // `mode.profile` (the fleet `main.ts` read off `FleetConfigEntry`) have to
    // be what reaches the call, since add-instance has no AWS field group.
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      ...awsIdentityFor({ kind: "add-instance", region: "eu-west-1", profile: "prod-admin" }),
    });
    expect(args.region).toBe("eu-west-1");
    expect(args.profile).toBe("prod-admin");
  });

  it("sends nothing to override with when the fleet records no identity", () => {
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      ...awsIdentityFor({ kind: "add-instance", region: null, profile: null }),
    });
    expect("region" in args).toBe(false);
    expect("profile" in args).toBe(false);
  });

  it("still carries the new-fleet identity step's own fields through", () => {
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      ...awsIdentityFor({ kind: "new-fleet", region: "ap-northeast-1", profile: "studio-prod" }),
    });
    expect(args.region).toBe("ap-northeast-1");
    expect(args.profile).toBe("studio-prod");
  });
});

describe("the deployed identity and the recorded one cannot drift", () => {
  // The submit handler reads the identity once (`awsIdentityFor`) and feeds it
  // to both the call and the `[fleet.<name>]` entry. Asserted against the real
  // `appendFleetBlock` output rather than a hand-rolled stand-in, because the
  // thing that must not drift is what actually lands in `fleets.toml`.
  const ecsEntry = (region: string | null, profile: string | null) => ({
    name: "support",
    member: "oab-default-zeus",
    expectedPrincipal: null,
    runtime: { kind: "ecs" as const, region, profile },
  });

  it("records exactly what it deployed under", () => {
    const identity = awsIdentityFor({
      kind: "new-fleet",
      region: "  ap-northeast-1  ",
      profile: "studio-prod",
    });
    const call = provisionAgentArgs({ ...ecs, acpEnabled: true, ...identity });
    const toml = appendFleetBlock("", ecsEntry(identity.region ?? null, identity.profile ?? null));
    expect(call.region).toBe("ap-northeast-1");
    expect(toml).toContain('region = "ap-northeast-1"');
    expect(toml).toContain('profile = "studio-prod"');
  });

  it("records a blank identity as absent, not as an empty string", () => {
    // An empty `region = ""` in fleets.toml would later read as a pinned-but-
    // blank region to every consumer of the file.
    const identity = awsIdentityFor({ kind: "new-fleet", region: "   ", profile: "" });
    expect(identity).toEqual({ region: undefined, profile: undefined });
    const toml = appendFleetBlock("", ecsEntry(identity.region ?? null, identity.profile ?? null));
    expect(toml).not.toContain("region");
    expect(toml).not.toContain("profile");
    expect(toml).toContain('members = ["oab-default-zeus"]');
  });
});

describe("provisionAgentArgs — optional wizard fields", () => {
  it("forwards chat platform secrets and the local config folder when set", () => {
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      chatPlatform: "line",
      chatBotToken: "channel-token",
      chatChannelSecret: "channel-secret",
      apiKey: "sk-vendor",
      localConfigFolder: "/home/op/studio-config",
    });
    expect(args.chat_platform).toBe("line");
    expect(args.chat_bot_token).toBe("channel-token");
    expect(args.chat_channel_secret).toBe("channel-secret");
    expect(args.api_key).toBe("sk-vendor");
    expect(args.local_config_folder).toBe("/home/op/studio-config");
  });

  it("drops every blank optional field instead of sending empty strings", () => {
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: false,
      apiKey: "  ",
      chatPlatform: "",
      chatBotToken: "",
      chatChannelSecret: "",
      acpToken: "",
      localConfigFolder: "",
    });
    for (const key of [
      "api_key",
      "chat_platform",
      "chat_bot_token",
      "chat_channel_secret",
      "acp_token",
      "local_config_folder",
    ]) {
      expect(key in args).toBe(false);
    }
  });

  it("trims surrounding whitespace on every optional field", () => {
    // Pre-existing behavior for most of these; pinned because a trimmed
    // `local_config_folder` in particular is new (it used to go over untrimmed,
    // and a trailing space in a path is a different directory).
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      apiKey: "  sk-vendor  ",
      chatPlatform: " discord ",
      chatBotToken: "  bot-token ",
      chatChannelSecret: "  channel-secret ",
      acpToken: "  acp-key ",
      localConfigFolder: "  /home/op/studio-config  ",
      region: "  ap-northeast-1  ",
      profile: "  studio-prod  ",
    });
    expect(args.api_key).toBe("sk-vendor");
    expect(args.chat_platform).toBe("discord");
    expect(args.chat_bot_token).toBe("bot-token");
    expect(args.chat_channel_secret).toBe("channel-secret");
    expect(args.acp_token).toBe("acp-key");
    expect(args.local_config_folder).toBe("/home/op/studio-config");
    expect(args.region).toBe("ap-northeast-1");
    expect(args.profile).toBe("studio-prod");
  });

  it("trims the k8s placement pair too", () => {
    // A hand-edited `fleets.toml` is the only in-app source of these (via
    // add-instance's inherited placement), and padding on either one selects
    // nothing.
    const args = provisionAgentArgs({
      ...ecs,
      acpEnabled: true,
      k8s: { context: "  orbstack  ", expectedPrincipal: "  system:serviceaccount:persephone:runner  " },
    });
    expect(args.context).toBe("orbstack");
    expect(args.expected_principal).toBe("system:serviceaccount:persephone:runner");
  });

  it("carries acp_enabled verbatim — the sidecar owns the default-when-absent rule", () => {
    expect(provisionAgentArgs({ ...ecs, acpEnabled: false }).acp_enabled).toBe(false);
    expect(provisionAgentArgs({ ...ecs, acpEnabled: true }).acp_enabled).toBe(true);
  });

  it("never sends an acp_token for an ACP-off agent (the sidecar ignores it, and it would be a stray secret)", () => {
    const args = provisionAgentArgs({ ...ecs, acpEnabled: false, acpToken: "typed-anyway" });
    expect("acp_token" in args).toBe(false);
  });
});
