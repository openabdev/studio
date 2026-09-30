// The deploy action panel (ADR #83 §7.5, redesigned per studio#128):
// `[+ New fleet]` (7.2) and `[+ Add instance]` (7.3) both land here —
// vendor + optional API key + optional chat platform + ACP toggle compose
// config.toml server-side (`deploy_provision_agent`, studio-cp's
// `generate_agent_config`) — differing only in whether a fleet-identity
// step runs first. No compose library / template ⊕ overlay involved
// anymore (that path — `compose.ts`, `deploy_provision` — still exists for
// anything still using it, just not this wizard).
//
// After a successful `deploy_provision_agent`, this module computes the
// updated `fleets.toml` text (`fleetToml.ts`, pure) and persists it via
// `source.writeFleetConfig` — per the ADR, `fleets.toml` is only ever mutated
// after a confirmed successful provision, never before or speculatively.

import type { Source } from "./source";
import { appendMember, appendFleetBlock, fleetBlockExists } from "./fleetToml";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function tauriInvoke(): Invoke | null {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// studio#135: same localStorage key main.ts's Config-folder setting uses
// (kept as a duplicated literal, not a shared export — every module in
// this console reads its own localStorage keys independently, matching
// the existing theme/log-level/config-folder settings' own pattern).
// Brett's explicit ordering ("write to local first, write to s3 if
// needed") can only actually be guaranteed inside the sidecar — passing
// the folder through lets provision_agent[_k8s] write it before touching
// S3 at all, rather than the console writing a copy after the fact.
function localConfigFolder(): string | undefined {
  try {
    return localStorage.getItem("oab-studio.configFolder") ?? undefined;
  } catch {
    return undefined;
  }
}

// list_aws_profiles / list_k8s_contexts / list_namespaces response shapes
// (oab-mcp, studio#104) — kept minimal (just what this panel reads), not the
// tools' full contract.
export interface AwsProfilesResponse {
  profiles: { name: string; region: string | null }[];
  // `exists`/`error` drive the spec's failure tiers: missing/empty config →
  // actionable guidance; a file that exists but can't be read → raw error.
  exists: boolean;
  error: string | null;
}
export interface K8sContextsResponse {
  contexts: { name: string }[];
  current_context: string | null;
  exists: boolean;
  error: string | null;
}
export interface K8sNamespacesResponse {
  namespaces: string[];
}
interface K8sServiceAccountsResponse {
  service_accounts: string[];
}

// What a tool-backed enumeration <select> resolves to: the options to offer
// plus, when enumeration degraded, the single status line to show. studio#104's
// failure tiers: missing/empty config → actionable guidance (`err: false`);
// call/read failure → the raw error (`err: true`). The manual-entry sentinel
// stays in `options` either way — the wizard is never blocked by enumeration.
export interface EnumFieldResult {
  options: { value: string; label: string }[];
  status: { text: string; err: boolean } | null;
}

// Manual-entry sentinel values for the AWS-profile / k8s-context selects —
// picking one reveals the field's plain text input (the image tag field's
// "Custom…" / namespace's "+ Create new…" pattern).
export const PROFILE_MANUAL = "__manual__";
export const CONTEXT_MANUAL = "__manual__";
export const NAMESPACE_NEW = "__new__";

// `list_aws_profiles` → the Credential profile select's options + status.
// `null`/`err` is the tool call itself failing (sidecar down, core not
// started) — same "raw error, manual fallback" tier as `res.error`.
export function awsProfileField(
  res: AwsProfilesResponse | null,
  err?: unknown,
): EnumFieldResult {
  const manual = { value: PROFILE_MANUAL, label: "+ Enter a profile name…" };
  const defaultChain = { value: "", label: "— default credential chain —" };
  if (res === null) {
    return {
      options: [defaultChain, manual],
      status: { text: `aws profile list unavailable: ${errText(err)}`, err: true },
    };
  }
  if (res.error) {
    return {
      options: [defaultChain, manual],
      status: { text: `aws profile list unavailable: ${res.error}`, err: true },
    };
  }
  if (!res.exists || res.profiles.length === 0) {
    return {
      options: [defaultChain, manual],
      status: {
        text: "no AWS credential profiles found — run `aws configure` or `aws sso login`, or enter a profile name manually",
        err: false,
      },
    };
  }
  return {
    options: [
      defaultChain,
      ...res.profiles.map((p) => ({
        value: p.name,
        label: p.region ? `${p.name} (${p.region})` : p.name,
      })),
      manual,
    ],
    status: null,
  };
}

// `list_k8s_contexts` → the Context select's options + status. Same three
// tiers as the AWS profile field.
export function k8sContextField(
  res: K8sContextsResponse | null,
  err?: unknown,
): EnumFieldResult {
  const manual = { value: CONTEXT_MANUAL, label: "+ Enter a context name…" };
  const ambient = { value: "", label: "— kubeconfig current-context —" };
  if (res === null) {
    return {
      options: [ambient, manual],
      status: { text: `k8s context list unavailable: ${errText(err)}`, err: true },
    };
  }
  if (res.error) {
    return {
      options: [ambient, manual],
      status: { text: `k8s context list unavailable: ${res.error}`, err: true },
    };
  }
  if (!res.exists || res.contexts.length === 0) {
    return {
      options: [ambient, manual],
      status: {
        text: "no kubeconfig found — install OrbStack/kind/minikube for a local cluster, or merge your cloud vendor's kubeconfig into ~/.kube/config; you can also enter a context name manually",
        err: false,
      },
    };
  }
  return {
    options: [
      ambient,
      ...res.contexts.map((c) => ({
        value: c.name,
        label: c.name === res.current_context ? `${c.name} (current)` : c.name,
      })),
      manual,
    ],
    status: null,
  };
}

// `list_namespaces` → the Namespace select's options + status. On failure the
// "+ Create new namespace…" sentinel must survive — losing it blocks the
// wizard's only not-yet-existing-namespace path (studio#104).
export function k8sNamespaceField(
  res: K8sNamespacesResponse | null,
  err?: unknown,
): EnumFieldResult {
  const placeholder = { value: "", label: "— pick a namespace —" };
  const createNew = { value: NAMESPACE_NEW, label: "+ Create new namespace…" };
  if (res === null) {
    return {
      options: [placeholder, createNew],
      status: { text: `namespace list unavailable: ${errText(err)}`, err: true },
    };
  }
  return {
    options: [
      placeholder,
      ...res.namespaces.map((n) => ({ value: n, label: n })),
      createNew,
    ],
    status: null,
  };
}

// `list_service_accounts` → the Service account select's options. No status:
// per #104's design any failure here (including an RBAC-denied list) means
// "leave it unset" — the namespace's `default` service account applies.
export function k8sServiceAccountOptions(
  accounts: string[] | null,
): { value: string; label: string }[] {
  const fallback = { value: "", label: "— namespace default —" };
  return [fallback, ...(accounts ?? []).map((sa) => ({ value: sa, label: sa }))];
}
interface VendorImageTagsResponse {
  beta: string | null;
  stable: string | null;
}

// Vendors known to use an interactive device-auth login (`<cli> login
// --device-auth`/`--use-device-flow`) rather than a static API key — can't
// be completed from this wizard, has to happen after the agent is up.
const DEVICE_AUTH_VENDORS = new Set(["codex", "kiro"]);

// `resolve_vendor_image_tags` (vendor_images.rs) only resolves bare GHCR
// tags (e.g. "0.9.0-claude") — it has no opinion on registry/repo, it's
// just answering "does this tag exist". `deploy_provision_agent`/k8s's
// `build_default_k8s_manifest` write whatever `image` this panel sends
// straight into the ECS task def / pod spec with no normalization, so a
// bare tag becomes a literal image *name* to the container runtime —
// Docker Hub's `library/<tag>` — not a GHCR pull. Full ref has to be
// built here, once, before it ever leaves this panel.
const IMAGE_REPO = "ghcr.io/openabdev/openab";

// studio#128: pre-fills the Agent name field; still freely editable.
const GREEK_GODS = [
  "Zeus", "Hera", "Poseidon", "Demeter", "Athena", "Apollo", "Artemis", "Ares",
  "Aphrodite", "Hephaestus", "Hermes", "Dionysus", "Hades", "Hestia", "Persephone",
  "Hypnos", "Nike", "Iris", "Eros", "Pan",
];
function randomGreekName(): string {
  return GREEK_GODS[Math.floor(Math.random() * GREEK_GODS.length)];
}

// studio#153: "add-instance" now carries the target fleet's existing
// runtime binding (read once, by the caller, from `FleetConfigEntry` —
// `main.ts` already has it in hand from the fleet the operator drilled
// into) instead of assuming ECS. This wizard has no k8s identity step for
// "add-instance" (that only exists for "new-fleet"), so there's nothing to
// re-collect from the operator — a k8s fleet's context/namespace/service
// account were fixed the moment the fleet was created.
export type DeployMode =
  | { kind: "new-fleet" }
  | {
      kind: "add-instance";
      fleetName: string;
      runtime: "ecs" | "k8s";
      context: string | null;
      namespace: string | null;
      expectedPrincipal: string | null;
    };

// What the panel reports back once a deploy + fleets.toml write both succeed —
// enough for the caller (`main.ts`) to log it and re-derive screen state
// (select the fleet, refresh the roster) without this module reaching into
// main.ts's own state.
export interface DeployedInfo {
  fleetName: string;
  service: string;
  image: string;
}

export interface DeployPanelDeps {
  source: Source;
  onDeployed(info: DeployedInfo): void | Promise<void>;
  // Re-run the #config/#fleet-detail visibility logic on close — main.ts owns
  // which of the two `activeFleet` selects, this panel doesn't need to know.
  restoreScreen(): void;
}

export interface DeployPanelHandle {
  open(mode: DeployMode): void;
  close(): void;
}

// Wires the `#deploy-wrap` panel declared in index.html. It lives inside
// `.drilldown-main` (a sibling of `#config`/`#fleet-detail`) so it takes over
// just the main column while open — the persistent side column (identity +
// Agent chat) stays put, same as every other depth of the drill-down. `null`
// if the DOM isn't present (mirrors the rest of the console's init* functions).
export function initDeployPanel(deps: DeployPanelDeps): DeployPanelHandle | null {
  const wrap = document.getElementById("deploy-wrap");
  const configEl = document.getElementById("config");
  const fleetDetailEl = document.getElementById("fleet-detail");
  const titleEl = document.getElementById("deploy-title");
  const cancelBtn = document.getElementById("deploy-cancel") as HTMLButtonElement | null;
  const identityForm = document.getElementById("deploy-identity-form") as HTMLFormElement | null;
  const nameInput = document.getElementById("deploy-fleet-name") as HTMLInputElement | null;
  const providerSel = document.getElementById("deploy-provider") as HTMLSelectElement | null;
  const awsFieldsEl = document.getElementById("deploy-aws-fields");
  const regionInput = document.getElementById("deploy-region") as HTMLInputElement | null;
  // Credential profile is a <select> fed by `list_aws_profiles` (studio#104),
  // with a "+ Enter a profile name…" sentinel revealing a plain text input —
  // the manual-entry fallback for when enumeration is empty/failed.
  const profileSel = document.getElementById("deploy-profile") as HTMLSelectElement | null;
  const profileCustomWrap = document.getElementById("deploy-profile-custom-wrap");
  const profileCustomInput = document.getElementById("deploy-profile-custom") as HTMLInputElement | null;
  const principalInput = document.getElementById("deploy-principal") as HTMLInputElement | null;
  const k8sFieldsEl = document.getElementById("deploy-k8s-fields");
  const k8sContextSel = document.getElementById("deploy-k8s-context") as HTMLSelectElement | null;
  const k8sContextCustomWrap = document.getElementById("deploy-k8s-context-custom-wrap");
  const k8sContextCustomInput = document.getElementById("deploy-k8s-context-custom") as HTMLInputElement | null;
  // Namespace is a <select> of what already exists, plus a sentinel
  // "+ Create new namespace…" option (studio#119 — the original free-text
  // <input>+<datalist> didn't read as "selectable" per Brett) that reveals a
  // plain text field for the not-yet-existing case a select alone can't
  // express.
  const k8sNamespaceSel = document.getElementById("deploy-k8s-namespace") as HTMLSelectElement | null;
  const k8sNamespaceNewWrap = document.getElementById("deploy-k8s-namespace-new-wrap");
  const k8sNamespaceNewInput = document.getElementById("deploy-k8s-namespace-new") as HTMLInputElement | null;
  const NAMESPACE_NEW_SENTINEL = NAMESPACE_NEW;
  // Service account, unlike namespace, must already exist for k8s to accept
  // it as a pod's serviceAccountName — so (unlike namespace) a plain <select>
  // is the right shape here, no free-text escape hatch needed.
  const k8sServiceAccountSel = document.getElementById("deploy-k8s-service-account") as HTMLSelectElement | null;
  const identityStatusEl = document.getElementById("deploy-identity-status");
  const composeSection = document.getElementById("deploy-compose");
  const composeHeading = document.getElementById("deploy-compose-heading");
  const deployForm = document.getElementById("deploy-agent-form") as HTMLFormElement | null;
  const vendorSel = document.getElementById("deploy-vendor") as HTMLSelectElement | null;
  const imageSelectEl = document.getElementById("deploy-image-select") as HTMLSelectElement | null;
  const imageCustomWrap = document.getElementById("deploy-image-custom-wrap");
  const imageCustomInput = document.getElementById("deploy-image-custom") as HTMLInputElement | null;
  const apiKeyInput = document.getElementById("deploy-api-key") as HTMLInputElement | null;
  const deviceAuthHint = document.getElementById("deploy-device-auth-hint");
  const chatPlatformSel = document.getElementById("deploy-chat-platform") as HTMLSelectElement | null;
  const chatTokenWrap = document.getElementById("deploy-chat-token-wrap");
  const chatTokenInput = document.getElementById("deploy-chat-token") as HTMLInputElement | null;
  const chatSecretWrap = document.getElementById("deploy-chat-secret-wrap");
  const chatSecretInput = document.getElementById("deploy-chat-secret") as HTMLInputElement | null;
  const acpCheckbox = document.getElementById("deploy-acp-enabled") as HTMLInputElement | null;
  const acpAgyHint = document.getElementById("deploy-acp-agy-hint");
  const acpTokenWrap = document.getElementById("deploy-acp-token-wrap");
  const acpTokenInput = document.getElementById("deploy-acp-token") as HTMLInputElement | null;
  const acpTokenGenerateBtn = document.getElementById("deploy-acp-token-generate") as HTMLButtonElement | null;
  const agentNameInput = document.getElementById("deploy-name") as HTMLInputElement | null;
  const agentNameShuffleBtn = document.getElementById("deploy-name-shuffle") as HTMLButtonElement | null;
  const agentNamePreviewEl = document.getElementById("deploy-name-preview");
  const deployBtn = document.getElementById("deploy-deploy-btn") as HTMLButtonElement | null;
  const deployStatusEl = document.getElementById("deploy-deploy-status");

  if (
    !wrap ||
    !cancelBtn ||
    !identityForm ||
    !nameInput ||
    !providerSel ||
    !awsFieldsEl ||
    !regionInput ||
    !profileSel ||
    !profileCustomWrap ||
    !profileCustomInput ||
    !principalInput ||
    !k8sFieldsEl ||
    !k8sContextSel ||
    !k8sContextCustomWrap ||
    !k8sContextCustomInput ||
    !k8sNamespaceSel ||
    !k8sNamespaceNewWrap ||
    !k8sNamespaceNewInput ||
    !k8sServiceAccountSel ||
    !composeSection ||
    !deployForm ||
    !vendorSel ||
    !imageSelectEl ||
    !imageCustomWrap ||
    !imageCustomInput ||
    !apiKeyInput ||
    !deviceAuthHint ||
    !chatPlatformSel ||
    !chatTokenWrap ||
    !chatTokenInput ||
    !chatSecretWrap ||
    !chatSecretInput ||
    !acpCheckbox ||
    !acpAgyHint ||
    !acpTokenWrap ||
    !acpTokenInput ||
    !acpTokenGenerateBtn ||
    !agentNameInput ||
    !agentNameShuffleBtn ||
    !agentNamePreviewEl ||
    !deployBtn
  ) {
    return null;
  }

  let mode: DeployMode | null = null;

  const setStatus = (el: HTMLElement | null, msg: string, cls = ""): void => {
    if (!el) return;
    el.textContent = msg;
    el.className = cls ? `compose-status ${cls}` : "compose-status";
  };

  // studio#128: which chat-token field(s) apply depends on the platform —
  // Discord/Telegram need just a bot token, LINE needs both a channel
  // access token *and* a channel secret. "— none —" needs neither (ACP is
  // the connection path).
  const applyChatPlatformMode = (): void => {
    const platform = chatPlatformSel.value;
    chatTokenWrap.hidden = platform === "";
    chatSecretWrap.hidden = platform !== "line";
    if (platform === "") {
      chatTokenInput.value = "";
      chatSecretInput.value = "";
    }
  };

  // studio#136: the ACP token field only makes sense while ACP itself is
  // on — hidden (not just left blank) when the checkbox is unchecked or
  // disabled, same show/hide-on-selection pattern as the chat token
  // fields above.
  const applyAcpMode = (): void => {
    const enabled = acpCheckbox.checked && !acpCheckbox.disabled;
    acpTokenWrap.hidden = !enabled;
    if (!enabled) acpTokenInput.value = "";
  };

  // studio#128: agy's bridge bypasses openab-gateway's /acp route entirely
  // (confirmed by reading agy-acp/src/main.rs) — forced off, not just
  // defaulted off, so a leftover checked state from a previous vendor can't
  // silently carry through to a vendor that can't honor it. Device-auth
  // vendors (codex/kiro) get an inline note since the API key field can't
  // help them — that login has to happen after the agent is up.
  const applyVendorMode = (): void => {
    const vendor = vendorSel.value;
    const isAgy = vendor === "antigravity";
    acpCheckbox.disabled = isAgy;
    acpAgyHint.hidden = !isAgy;
    if (isAgy) acpCheckbox.checked = false;
    applyAcpMode();
    deviceAuthHint.hidden = !DEVICE_AUTH_VENDORS.has(vendor);
  };

  const IMAGE_CUSTOM_SENTINEL = "__custom__";

  // studio#136: which value is actually in play — a resolved Stable/Beta
  // <option> (its value is already the full `ghcr.io/...` ref, built in
  // `loadVendorImage` below) or the free-text Custom field, where the user
  // is expected to type the full ref themselves.
  const currentImage = (): string =>
    imageSelectEl.value === IMAGE_CUSTOM_SENTINEL ? imageCustomInput.value.trim() : imageSelectEl.value;

  const applyImageMode = (): void => {
    const isCustom = imageSelectEl.value === IMAGE_CUSTOM_SENTINEL;
    imageCustomWrap.hidden = !isCustom;
    if (isCustom) imageCustomInput.focus();
  };

  // studio#153: ACP-enabled deploys need an image with ACP wired as a
  // first-class adapter (openab#1418, first shipped in 0.10.0-beta.2) —
  // Stable can lag behind that fix for a long stretch (it did: Stable was
  // pinned to 0.9.0, which predates the fix entirely, and picking it for an
  // ACP-enabled agent reproduces the exact "no adapter configured" crash
  // this was written to catch). ACP-on should not silently inherit the
  // select's implicit first-option default. Only nudges *into* Beta the
  // moment ACP is turned on; never fights a selection made afterward.
  const preferBetaForAcp = (): void => {
    if (!acpCheckbox.checked) return;
    const betaOption = Array.from(imageSelectEl.options).find((o) => o.textContent?.startsWith("Beta"));
    if (betaOption) imageSelectEl.value = betaOption.value;
  };

  // studio#128/#136: a real <select> of GHCR's actually-published tags for
  // the selected vendor (Stable/Beta, whichever resolved) plus a "Custom…"
  // escape hatch — rebuilding the option list on every vendor change (not
  // silently mutating one text field's value) is what makes "the image tag
  // changed" visibly obvious, per Brett's report that a plain text field's
  // value quietly updating read as "nothing happened".
  const loadVendorImage = async (): Promise<void> => {
    const invoke = tauriInvoke();
    const vendor = vendorSel.value;
    const opts: { value: string; label: string }[] = [];
    if (invoke) {
      try {
        const res = await invoke<VendorImageTagsResponse>("resolve_vendor_image_tags", { vendor });
        // `res.stable`/`res.beta` are bare tags (see IMAGE_REPO above) —
        // the label keeps the short tag for readability, the value carries
        // the full ref that actually gets pulled.
        if (res.stable) opts.push({ value: `${IMAGE_REPO}:${res.stable}`, label: `Stable (${res.stable})` });
        if (res.beta) opts.push({ value: `${IMAGE_REPO}:${res.beta}`, label: `Beta (${res.beta})` });
      } catch (e) {
        setStatus(deployStatusEl, `image tag lookup unavailable: ${errText(e)}`, "err");
      }
    }
    opts.push({ value: IMAGE_CUSTOM_SENTINEL, label: "Custom…" });
    imageSelectEl.innerHTML = "";
    for (const o of opts) {
      const opt = document.createElement("option");
      opt.value = o.value;
      opt.textContent = o.label;
      imageSelectEl.appendChild(opt);
    }
    // No selected-attribute set above, so the browser defaults to the first
    // option — Stable if it resolved, else Beta, else Custom (never stuck
    // on a meaningless blank selection) — unless ACP is already on, in
    // which case preferBetaForAcp overrides that default (see its comment).
    preferBetaForAcp();
    applyImageMode();
  };

  const reset = (): void => {
    identityForm.reset();
    deployForm.reset();
    setStatus(identityStatusEl, "");
    setStatus(deployStatusEl, "");
    // identityForm.reset() puts <select id="deploy-provider"> back to its
    // `selected` default ("aws"), but doesn't touch the field-group `hidden`
    // attributes this panel manages by hand — sync those too.
    showProviderFields(providerSel.value);
    applyProfileMode();
    applyContextMode();
    applyNamespaceMode();
    applyChatPlatformMode();
    applyVendorMode();
    agentNameInput.value = randomGreekName();
    updateNamePreview();
    void loadVendorImage();
    // studio#104: enumeration loads fire on open (new-fleet only — the
    // "add-instance" mode never shows this step) so the provider-specific
    // fields are already populated by the time the operator reaches them.
    if (mode?.kind === "new-fleet") {
      if (providerSel.value === "k8s") void loadK8sContexts();
      else void loadAwsProfiles();
    }
  };

  // studio#119: the namespace <select>'s "+ Create new namespace…" sentinel
  // reveals a plain text field for the not-yet-existing case (a <select>
  // alone can only offer what list_namespaces already returned).
  const applyNamespaceMode = (): void => {
    const isNew = k8sNamespaceSel.value === NAMESPACE_NEW_SENTINEL;
    k8sNamespaceNewWrap.hidden = !isNew;
    if (isNew) k8sNamespaceNewInput.focus();
    else k8sNamespaceNewInput.value = "";
  };

  const currentNamespace = (): string =>
    k8sNamespaceSel.value === NAMESPACE_NEW_SENTINEL
      ? k8sNamespaceNewInput.value.trim()
      : k8sNamespaceSel.value;

  // studio#153: the single source of truth for "is this submit targeting
  // k8s, and with what context/namespace/service-account" — "new-fleet"
  // reads it live off the identity step's fields (the only mode with that
  // step); "add-instance" reads it off the fleet binding `open()` was given
  // (see the `DeployMode` comment), since that fleet's k8s placement was
  // fixed at creation and this wizard never re-asks for it. `null` means
  // "not a k8s deploy" (ecs, or a new-fleet submit with Provider left on
  // "aws").
  const currentK8sTarget = (): { context?: string; namespace: string; expectedPrincipal?: string } | null => {
    if (mode?.kind === "new-fleet") {
      if (providerSel.value !== "k8s") return null;
      const namespace = currentNamespace() || "default";
      const serviceAccount = k8sServiceAccountSel.value;
      return {
        context: currentContext(),
        namespace,
        expectedPrincipal: serviceAccount ? `system:serviceaccount:${namespace}:${serviceAccount}` : undefined,
      };
    }
    if (mode?.kind === "add-instance") {
      if (mode.runtime !== "k8s") return null;
      return {
        context: mode.context ?? undefined,
        namespace: mode.namespace ?? "default",
        expectedPrincipal: mode.expectedPrincipal ?? undefined,
      };
    }
    return null;
  };

  // The `oab-${namespace}-${name}` service-name convention (mirrored from the
  // deploy submit handler below) was previously discoverable only by reading
  // source — nothing in the wizard showed what actually lands in fleets.toml's
  // `members` array (Brett, 2026-09-07).
  const updateNamePreview = (): void => {
    const namespace = currentK8sTarget()?.namespace ?? "default";
    const name = agentNameInput.value.trim() || "<name>";
    agentNamePreviewEl.textContent = `→ recorded in fleets.toml as oab-${namespace}-${name}`;
  };

  // Toggle the AWS/k8s field groups per studio#104's design. On a provider
  // switch the *departing* group's fields are reset (see the provider change
  // handler below) — field semantics don't map across providers.
  const showProviderFields = (provider: string): void => {
    awsFieldsEl.hidden = provider !== "aws";
    k8sFieldsEl.hidden = provider !== "k8s";
  };

  // Rebuild a <select>'s options from a pure option list, restoring the
  // previous selection when it's still offered (element-by-element, not
  // innerHTML — enumerated values come back over MCP and never reach the DOM
  // as markup).
  const setSelectOptions = (sel: HTMLSelectElement, opts: { value: string; label: string }[]): void => {
    const previous = sel.value;
    sel.innerHTML = "";
    for (const o of opts) {
      const el = document.createElement("option");
      el.value = o.value;
      el.textContent = o.label;
      sel.appendChild(el);
    }
    if (previous && opts.some((o) => o.value === previous)) sel.value = previous;
  };

  // Apply an EnumFieldResult to a select + the shared identity status line:
  // degraded enumerations show their guidance/raw error; a clean one clears
  // whatever a previous failed attempt left behind (studio#119's staleness
  // rule — an error must not outlive the state it described).
  const applyEnumField = (sel: HTMLSelectElement, field: EnumFieldResult): void => {
    setSelectOptions(sel, field.options);
    if (field.status) setStatus(identityStatusEl, field.status.text, field.status.err ? "err" : "");
    else setStatus(identityStatusEl, "");
  };

  const applyProfileMode = (): void => {
    const isManual = profileSel.value === PROFILE_MANUAL;
    profileCustomWrap.hidden = !isManual;
    if (isManual) profileCustomInput.focus();
    else profileCustomInput.value = "";
  };

  const applyContextMode = (): void => {
    const isManual = k8sContextSel.value === CONTEXT_MANUAL;
    k8sContextCustomWrap.hidden = !isManual;
    if (isManual) k8sContextCustomInput.focus();
    else k8sContextCustomInput.value = "";
  };

  const currentProfile = (): string =>
    profileSel.value === PROFILE_MANUAL ? profileCustomInput.value.trim() : profileSel.value;

  const currentContext = (): string | undefined =>
    (k8sContextSel.value === CONTEXT_MANUAL
      ? k8sContextCustomInput.value.trim()
      : k8sContextSel.value) || undefined;

  // list_aws_profiles (studio#104) → the Credential profile select.
  // `lastAwsProfiles` keeps the raw entries so a change event can pre-fill
  // Region from the selected profile's own configured region.
  let lastAwsProfiles: { name: string; region: string | null }[] = [];
  const loadAwsProfiles = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) {
      // No sidecar (vite preview) — still offer the manual-entry fallback so
      // the select isn't an empty dead end; no status, nothing actually
      // failed.
      setSelectOptions(profileSel, awsProfileField(null).options);
      applyProfileMode();
      return;
    }
    let field: EnumFieldResult;
    try {
      const res = await invoke<AwsProfilesResponse>("list_aws_profiles");
      lastAwsProfiles = res.profiles;
      field = awsProfileField(res);
    } catch (e) {
      lastAwsProfiles = [];
      field = awsProfileField(null, e);
    }
    // Stale-flight guard: the operator may have switched providers (or left
    // the step) while the tool call was in flight — don't repaint hidden
    // fields or overwrite the status line for the departed provider.
    if (providerSel.value !== "aws") return;
    applyEnumField(profileSel, field);
    applyProfileMode();
  };

  const loadK8sNamespaces = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) {
      setSelectOptions(k8sNamespaceSel, k8sNamespaceField(null).options);
      applyNamespaceMode();
      void loadK8sServiceAccounts();
      return;
    }
    const context = currentContext();
    let field: EnumFieldResult;
    try {
      field = k8sNamespaceField(
        await invoke<K8sNamespacesResponse>("list_namespaces", context ? { context } : {}),
      );
    } catch (e) {
      // studio#104: even on failure the select must still offer
      // "+ Create new namespace…" — that sentinel is the manual-entry
      // fallback, and without it a failed first load leaves zero options and
      // blocks the wizard.
      field = k8sNamespaceField(null, e);
    }
    // Same stale-flight guard as loadAwsProfiles.
    if (providerSel.value !== "k8s") return;
    applyEnumField(k8sNamespaceSel, field);
    applyNamespaceMode();
    // Service accounts are scoped to (context, namespace) — refresh them
    // only after the namespace selection itself settles, never concurrently
    // (a concurrent load could read a stale namespace selection).
    void loadK8sServiceAccounts();
  };

  // Service account is scoped to (context, namespace) and per #104's design
  // fails *silently* — unlike context/namespace, any error here (including an
  // RBAC-denied list, which is common against a scoped-down cluster identity)
  // means "leave it unset" (the namespace's default service account applies),
  // not something worth surfacing a status message for.
  const loadK8sServiceAccounts = async (): Promise<void> => {
    const invoke = tauriInvoke();
    const namespace = currentNamespace();
    if (!invoke || !namespace) {
      setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(null));
      return;
    }
    const context = currentContext();
    try {
      const res = await invoke<K8sServiceAccountsResponse>(
        "list_service_accounts",
        context ? { context, namespace } : { namespace },
      );
      setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(res.service_accounts));
    } catch {
      setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(null));
    }
  };

  const loadK8sContexts = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) {
      // Same "seed the manual fallbacks" as loadAwsProfiles' no-sidecar path.
      setSelectOptions(k8sContextSel, k8sContextField(null).options);
      applyContextMode();
      setSelectOptions(k8sNamespaceSel, k8sNamespaceField(null).options);
      applyNamespaceMode();
      setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(null));
      return;
    }
    let field: EnumFieldResult;
    try {
      field = k8sContextField(await invoke<K8sContextsResponse>("list_k8s_contexts"));
    } catch (e) {
      field = k8sContextField(null, e);
    }
    // Same stale-flight guard as loadAwsProfiles.
    if (providerSel.value !== "k8s") return;
    applyEnumField(k8sContextSel, field);
    applyContextMode();
    if (!field.status) {
      void loadK8sNamespaces();
    } else {
      // Enumeration degraded — listing namespaces against an unreadable or
      // absent kubeconfig would only overwrite the guidance/raw error with a
      // secondary failure. Reset the dependent selects to their manual
      // fallbacks instead, so the wizard stays unblocked.
      setSelectOptions(k8sNamespaceSel, k8sNamespaceField({ namespaces: [] }).options);
      applyNamespaceMode();
      setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(null));
    }
  };

  const resetAwsFields = (): void => {
    regionInput.value = "";
    principalInput.value = "";
    profileCustomInput.value = "";
    lastAwsProfiles = [];
    if (profileSel.options.length > 0) profileSel.selectedIndex = 0;
    applyProfileMode();
  };

  const resetK8sFields = (): void => {
    if (k8sContextSel.options.length > 0) k8sContextSel.selectedIndex = 0;
    k8sContextCustomInput.value = "";
    applyContextMode();
    if (k8sNamespaceSel.options.length > 0) k8sNamespaceSel.selectedIndex = 0;
    k8sNamespaceNewInput.value = "";
    applyNamespaceMode();
    setSelectOptions(k8sServiceAccountSel, k8sServiceAccountOptions(null));
  };

  providerSel.addEventListener("change", () => {
    showProviderFields(providerSel.value);
    // studio#104: switching providers resets the departing group's fields —
    // field semantics don't map across providers, so a hidden stale value is
    // dropped, not silently carried into a later submit.
    if (providerSel.value === "k8s") {
      resetAwsFields();
      void loadK8sContexts();
    } else {
      resetK8sFields();
      void loadAwsProfiles();
    }
    updateNamePreview();
  });
  profileSel.addEventListener("change", () => {
    applyProfileMode();
    // A profile's configured region is the region the SDK would resolve for
    // it anyway — surface it in the Region field (still freely editable).
    const p = lastAwsProfiles.find((p) => p.name === profileSel.value);
    if (p?.region) regionInput.value = p.region;
  });
  k8sContextSel.addEventListener("change", () => {
    applyContextMode();
    void loadK8sNamespaces();
  });
  // Manual context entry re-drives the dependent lists on commit ("change",
  // not "input" — avoids a tool call per keystroke).
  k8sContextCustomInput.addEventListener("change", () => void loadK8sNamespaces());
  k8sNamespaceSel.addEventListener("change", () => {
    applyNamespaceMode();
    void loadK8sServiceAccounts();
    updateNamePreview();
  });
  // "change" (fires on commit/blur), not "input" (every keystroke) — avoids a
  // tool call per character typed into the new-namespace field. The name
  // preview updates live regardless (no tool call involved).
  k8sNamespaceNewInput.addEventListener("change", () => void loadK8sServiceAccounts());
  k8sNamespaceNewInput.addEventListener("input", updateNamePreview);

  vendorSel.addEventListener("change", () => {
    applyVendorMode();
    void loadVendorImage();
  });
  imageSelectEl.addEventListener("change", applyImageMode);
  chatPlatformSel.addEventListener("change", applyChatPlatformMode);
  acpCheckbox.addEventListener("change", () => {
    applyAcpMode();
    preferBetaForAcp();
    applyImageMode();
  });
  acpTokenGenerateBtn.addEventListener("click", () => {
    // Same shape the sidecar generates itself (uuid v4) when this field is
    // left blank — a convenience for operators who want to know the token
    // before deploying (e.g. to hand it to a client ahead of time), not a
    // requirement: an empty field still gets a server-generated one.
    acpTokenInput.value = crypto.randomUUID();
  });
  agentNameShuffleBtn.addEventListener("click", () => {
    agentNameInput.value = randomGreekName();
    updateNamePreview();
  });
  agentNameInput.addEventListener("input", updateNamePreview);

  const open = (m: DeployMode): void => {
    mode = m;
    reset();
    if (titleEl) {
      titleEl.textContent =
        m.kind === "new-fleet" ? "New fleet — fleet identity" : `${m.fleetName} — add instance`;
    }
    identityForm.hidden = m.kind !== "new-fleet";
    composeSection.hidden = m.kind === "new-fleet";
    if (composeHeading) {
      composeHeading.textContent =
        m.kind === "new-fleet" ? "Step 2 — first instance" : "Compose";
    }
    if (configEl) configEl.hidden = true;
    if (fleetDetailEl) fleetDetailEl.hidden = true;
    wrap.hidden = false;
  };

  const close = (): void => {
    mode = null;
    wrap.hidden = true;
    deps.restoreScreen();
    reset();
  };

  cancelBtn.addEventListener("click", close);

  // Step 1 (new-fleet only): collect the fleet identity, then reveal the
  // shared Compose step — 7.5.1's "Next: first instance →".
  identityForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fleetName = nameInput.value.trim();
    if (!fleetName) {
      setStatus(identityStatusEl, "fleet name is required", "err");
      return;
    }
    // Reject a colliding name here, before Step 2 provisions anything —
    // appendFleetBlock always appends a brand-new `[fleet.<name>]` block, so
    // reusing an existing name would otherwise only surface as a
    // duplicate-key TOML parse error *after* the instance was already
    // deployed (it has no partial/merge fallback — use "Add instance" on
    // that fleet instead, per the error message below).
    try {
      const current = await deps.source.fleetConfig();
      if (fleetBlockExists(current.text, fleetName)) {
        setStatus(
          identityStatusEl,
          `a fleet named "${fleetName}" already exists — use "Add instance" on that fleet instead`,
          "err",
        );
        return;
      }
    } catch (e) {
      setStatus(identityStatusEl, `fleet name check unavailable: ${errText(e)}`, "err");
      return;
    }
    identityForm.hidden = true;
    composeSection.hidden = false;
    if (composeHeading) composeHeading.textContent = "Step 2 — first instance";
  });

  // The failure rule from 7.5.1/7.5.2: if `deploy_provision_agent` fails,
  // stop — no `fleet_config_write` call, `fleets.toml` is untouched.
  deployForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const invoke = tauriInvoke();
    if (!invoke || !mode) {
      setStatus(deployStatusEl, "deploy unavailable", "err");
      return;
    }
    const name = agentNameInput.value.trim();
    if (!name) {
      setStatus(deployStatusEl, "agent name is required", "err");
      return;
    }
    const image = currentImage();
    if (!image) {
      setStatus(deployStatusEl, "image tag is required", "err");
      return;
    }
    // studio#153: "new-fleet" reads this live off the identity step;
    // "add-instance" reads it off the existing fleet's binding — either way
    // `currentK8sTarget()` is the single place that decides it (see its
    // comment). `expectedPrincipal` here is already in
    // `system:serviceaccount:<ns>:<name>` form either way — `new-fleet`
    // builds it fresh from the service-account picker, `add-instance`
    // inherits it as-is from `K8sFleetBinding.expected_principal`.
    const k8sTarget = currentK8sTarget();
    const isK8s = k8sTarget !== null;
    const namespace = k8sTarget?.namespace ?? "default";
    const context = k8sTarget?.context;
    const expectedPrincipal = k8sTarget?.expectedPrincipal;
    const chatPlatform = chatPlatformSel.value || undefined;
    // k8s deploys refuse a chat platform server-side (config.toml secret
    // resolution needs AWS credentials a k8s pod doesn't have) — check here
    // too so the failure reads as a validation message, not a deploy error.
    if (isK8s && chatPlatform) {
      setStatus(
        deployStatusEl,
        "chat platform integration isn't available for k8s deploys yet — use ACP, or switch Provider to AWS",
        "err",
      );
      return;
    }
    deployBtn.disabled = true;
    setStatus(deployStatusEl, "deploying…");
    let res: { image?: string; digest?: string; objects?: number };
    try {
      res = await invoke("deploy_provision_agent", {
        image,
        name,
        namespace,
        api_key: apiKeyInput.value.trim() || undefined,
        chat_platform: chatPlatform,
        chat_bot_token: chatTokenInput.value.trim() || undefined,
        chat_channel_secret: chatSecretInput.value.trim() || undefined,
        acp_enabled: acpCheckbox.checked,
        acp_token: acpCheckbox.checked ? acpTokenInput.value.trim() || undefined : undefined,
        local_config_folder: localConfigFolder(),
        ...(isK8s ? { provider: "k8s", context, expected_principal: expectedPrincipal } : {}),
      });
    } catch (e) {
      setStatus(deployStatusEl, `deploy failed: ${errText(e)}`, "err");
      deployBtn.disabled = false;
      return;
    }
    const service = `oab-${namespace}-${name}`;
    const fleetName = mode.kind === "new-fleet" ? nameInput.value.trim() : mode.fleetName;
    setStatus(deployStatusEl, `deployed ${service} — updating fleets.toml…`, "ok");
    try {
      const current = await deps.source.fleetConfig();
      const nextText =
        mode.kind === "new-fleet"
          ? appendFleetBlock(current.text, {
              name: fleetName,
              member: service,
              expectedPrincipal: (isK8s ? expectedPrincipal : principalInput.value.trim()) || null,
              runtime: isK8s
                ? { kind: "k8s", context: context ?? null, namespace }
                : {
                    kind: "ecs",
                    region: regionInput.value.trim() || null,
                    profile: currentProfile() || null,
                  },
            })
          : appendMember(current.text, fleetName, service);
      await deps.source.writeFleetConfig(nextText);
    } catch (e) {
      // The instance is live but the config file wasn't updated — surface it
      // rather than silently leaving the roster's membership stale.
      setStatus(deployStatusEl, `deployed ${service}, but fleets.toml update failed: ${errText(e)}`, "err");
      deployBtn.disabled = false;
      return;
    }
    deployBtn.disabled = false;
    const info: DeployedInfo = { fleetName, service, image: res.image ?? image };
    close();
    await deps.onDeployed(info);
  });

  return { open, close };
}
