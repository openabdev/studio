import { describe, it, expect } from "vitest";
import {
  awsProfileField,
  isValidFleetName,
  k8sContextField,
  k8sNamespaceField,
  k8sServiceAccountOptions,
  PROFILE_MANUAL,
  CONTEXT_MANUAL,
  NAMESPACE_NEW,
} from "./deploy";

// studio#104: the "+ New fleet" identity step's tool-backed <select> fields.
// Each resolves an enumeration response (or a failed tool call) into
// {options, status} — the spec's three failure tiers:
//   - config missing/empty  → actionable guidance (non-error status);
//   - tool/read failure     → the raw error text;
//   - either way            → a manual-entry fallback stays in the options.

const manualIn = (opts: { value: string; label: string }[], sentinel: string) =>
  opts.some((o) => o.value === sentinel);

describe("awsProfileField", () => {
  it("lists the default chain, every profile, and a manual-entry fallback", () => {
    const f = awsProfileField({
      profiles: [
        { name: "oab-fleet", region: "ap-east-2" },
        { name: "dev", region: null },
      ],
      exists: true,
      error: null,
    });
    expect(f.status).toBeNull();
    expect(f.options[0].value).toBe("");
    expect(f.options.map((o) => o.value)).toContain("oab-fleet");
    expect(f.options.map((o) => o.value)).toContain("dev");
    // the configured region shows in the label so the operator sees it
    const fleet = f.options.find((o) => o.value === "oab-fleet");
    expect(fleet?.label).toContain("ap-east-2");
    expect(manualIn(f.options, PROFILE_MANUAL)).toBe(true);
  });

  it("missing AWS config → actionable guidance, not an error, still unblocked", () => {
    const f = awsProfileField({ profiles: [], exists: false, error: null });
    expect(f.status).not.toBeNull();
    expect(f.status?.err).toBe(false);
    expect(f.status?.text).toContain("aws configure");
    expect(f.status?.text).toContain("aws sso login");
    expect(manualIn(f.options, PROFILE_MANUAL)).toBe(true);
  });

  it("an existing-but-empty config lands on the same guidance tier", () => {
    const f = awsProfileField({ profiles: [], exists: true, error: null });
    expect(f.status?.err).toBe(false);
    expect(f.status?.text).toContain("aws configure");
    expect(manualIn(f.options, PROFILE_MANUAL)).toBe(true);
  });

  it("a read error (exists but unreadable) shows the raw error", () => {
    const f = awsProfileField({
      profiles: [],
      exists: true,
      error: "Permission denied (os error 13)",
    });
    expect(f.status?.err).toBe(true);
    expect(f.status?.text).toContain("Permission denied (os error 13)");
    expect(manualIn(f.options, PROFILE_MANUAL)).toBe(true);
  });

  it("a failed tool call shows the raw error and keeps manual entry", () => {
    const f = awsProfileField(null, new Error("core not started yet"));
    expect(f.status?.err).toBe(true);
    expect(f.status?.text).toContain("core not started yet");
    expect(manualIn(f.options, PROFILE_MANUAL)).toBe(true);
  });
});

describe("k8sContextField", () => {
  it("lists the ambient default, every context (marking current), and manual entry", () => {
    const f = k8sContextField({
      contexts: [{ name: "orbstack" }, { name: "gke-prod" }],
      current_context: "orbstack",
      exists: true,
      error: null,
    });
    expect(f.status).toBeNull();
    expect(f.options[0].value).toBe("");
    const current = f.options.find((o) => o.value === "orbstack");
    expect(current?.label).toContain("(current)");
    const other = f.options.find((o) => o.value === "gke-prod");
    expect(other?.label).not.toContain("(current)");
    expect(manualIn(f.options, CONTEXT_MANUAL)).toBe(true);
  });

  it("missing kubeconfig → actionable local-cluster guidance, still unblocked", () => {
    const f = k8sContextField({
      contexts: [],
      current_context: null,
      exists: false,
      error: null,
    });
    expect(f.status?.err).toBe(false);
    expect(f.status?.text).toMatch(/OrbStack|kind|minikube/);
    expect(f.status?.text).toContain(".kube/config");
    expect(manualIn(f.options, CONTEXT_MANUAL)).toBe(true);
  });

  it("an existing-but-unparseable kubeconfig shows the raw error", () => {
    const f = k8sContextField({
      contexts: [],
      current_context: null,
      exists: true,
      error: "failed to parse kubeconfig YAML",
    });
    expect(f.status?.err).toBe(true);
    expect(f.status?.text).toContain("failed to parse kubeconfig YAML");
    expect(manualIn(f.options, CONTEXT_MANUAL)).toBe(true);
  });

  it("a failed tool call shows the raw error and keeps manual entry", () => {
    const f = k8sContextField(null, new Error("spawn oab-mcp failed"));
    expect(f.status?.err).toBe(true);
    expect(f.status?.text).toContain("spawn oab-mcp failed");
    expect(manualIn(f.options, CONTEXT_MANUAL)).toBe(true);
  });
});

describe("k8sNamespaceField", () => {
  it("lists a placeholder, every namespace, and the create-new sentinel", () => {
    const f = k8sNamespaceField({ namespaces: ["default", "oab"] });
    expect(f.status).toBeNull();
    expect(f.options[0].value).toBe("");
    expect(f.options.map((o) => o.value)).toContain("default");
    expect(f.options.map((o) => o.value)).toContain("oab");
    expect(manualIn(f.options, NAMESPACE_NEW)).toBe(true);
  });

  it("a failed list still offers create-new so the wizard isn't blocked", () => {
    const f = k8sNamespaceField(null, new Error("dial tcp: connection refused"));
    expect(f.status?.err).toBe(true);
    expect(f.status?.text).toContain("connection refused");
    expect(manualIn(f.options, NAMESPACE_NEW)).toBe(true);
  });
});

describe("k8sServiceAccountOptions", () => {
  it("offers the namespace default plus each account", () => {
    const opts = k8sServiceAccountOptions(["builder", "oab-agent"]);
    expect(opts[0].value).toBe("");
    expect(opts.map((o) => o.value)).toEqual(["", "builder", "oab-agent"]);
  });

  it("a failed (or empty) list degrades to the namespace default only", () => {
    const opts = k8sServiceAccountOptions(null);
    expect(opts.map((o) => o.value)).toEqual([""]);
  });
});

describe("isValidFleetName", () => {
  it("accepts TOML bare-key charset", () => {
    for (const ok of ["support-fleet", "oab_prod_1", "Team2"]) {
      expect(isValidFleetName(ok)).toBe(true);
    }
  });

  it("rejects anything that would corrupt or split the [fleet.<name>] header", () => {
    for (const bad of ["a.b", "x]y", 'a"b', "my fleet", "f\\leet", "", "na\u{1f600}me"]) {
      expect(isValidFleetName(bad)).toBe(false);
    }
  });
});
