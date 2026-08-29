// The deploy action panel (ADR #83 §7.5): `[+ New fleet]` (7.2) and `[+ Add
// instance]` (7.3) both land here — the same compose→preview→deploy engine
// (agent-deployment-templates.md, reused via `compose.ts`'s exported pure
// helpers), differing only in whether a fleet-identity step runs first.
//
// After a successful `deploy_provision`, this module computes the updated
// `fleets.toml` text (`fleetToml.ts`, pure) and persists it via
// `source.writeFleetConfig` — per the ADR, `fleets.toml` is only ever mutated
// after a confirmed successful provision, never before or speculatively.

import type { Source } from "./source";
import { libraryNames, renderPreviewHtml, type Library, type BundlePreview } from "./compose";
import { appendMember, appendFleetBlock } from "./fleetToml";
import { appendK8sFleetBlock } from "./fleetsK8sToml";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function tauriInvoke(): Invoke | null {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function fillOptions(sel: HTMLSelectElement, names: string[], keepNoneFirst: boolean): void {
  const opts = keepNoneFirst ? ['<option value="">— none (bare template) —</option>'] : [];
  for (const n of names) opts.push(`<option value="${escapeHtml(n)}">${escapeHtml(n)}</option>`);
  sel.innerHTML = opts.join("");
}

// list_k8s_contexts / list_namespaces response shapes (oab-mcp, studio#104) —
// kept minimal (just what this panel reads), not the tools' full contract.
interface K8sContextsResponse {
  contexts: { name: string }[];
  current_context: string | null;
}
interface K8sNamespacesResponse {
  namespaces: string[];
}
interface K8sServiceAccountsResponse {
  service_accounts: string[];
}

export type DeployMode = { kind: "new-fleet" } | { kind: "add-instance"; fleetName: string };

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
  const profileInput = document.getElementById("deploy-profile") as HTMLInputElement | null;
  const principalInput = document.getElementById("deploy-principal") as HTMLInputElement | null;
  const k8sFieldsEl = document.getElementById("deploy-k8s-fields");
  const k8sContextSel = document.getElementById("deploy-k8s-context") as HTMLSelectElement | null;
  // Namespace is a <select> of what already exists, plus a sentinel
  // "+ Create new namespace…" option (studio#119 — the original free-text
  // <input>+<datalist> didn't read as "selectable" per Brett) that reveals a
  // plain text field for the not-yet-existing case a select alone can't
  // express.
  const k8sNamespaceSel = document.getElementById("deploy-k8s-namespace") as HTMLSelectElement | null;
  const k8sNamespaceNewWrap = document.getElementById("deploy-k8s-namespace-new-wrap");
  const k8sNamespaceNewInput = document.getElementById("deploy-k8s-namespace-new") as HTMLInputElement | null;
  const NAMESPACE_NEW_SENTINEL = "__new__";
  // Service account, unlike namespace, must already exist for k8s to accept
  // it as a pod's serviceAccountName — so (unlike namespace) a plain <select>
  // is the right shape here, no free-text escape hatch needed.
  const k8sServiceAccountSel = document.getElementById("deploy-k8s-service-account") as HTMLSelectElement | null;
  const identityStatusEl = document.getElementById("deploy-identity-status");
  const composeSection = document.getElementById("deploy-compose");
  const composeHeading = document.getElementById("deploy-compose-heading");
  const tmplSel = document.getElementById("deploy-template") as HTMLSelectElement | null;
  const ovlSel = document.getElementById("deploy-overlay") as HTMLSelectElement | null;
  const previewForm = document.getElementById("deploy-form") as HTMLFormElement | null;
  const previewOut = document.getElementById("deploy-preview");
  const previewStatusEl = document.getElementById("deploy-status");
  const deployForm = document.getElementById("deploy-deploy-form") as HTMLFormElement | null;
  const agentNameInput = document.getElementById("deploy-name") as HTMLInputElement | null;
  const imageInput = document.getElementById("deploy-image") as HTMLInputElement | null;
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
    !profileInput ||
    !principalInput ||
    !k8sFieldsEl ||
    !k8sContextSel ||
    !k8sNamespaceSel ||
    !k8sNamespaceNewWrap ||
    !k8sNamespaceNewInput ||
    !k8sServiceAccountSel ||
    !composeSection ||
    !tmplSel ||
    !ovlSel ||
    !previewForm ||
    !deployForm ||
    !agentNameInput ||
    !imageInput ||
    !deployBtn
  ) {
    return null;
  }

  let mode: DeployMode | null = null;
  let library: Library | null = null;

  const setStatus = (el: HTMLElement | null, msg: string, cls = ""): void => {
    if (!el) return;
    el.textContent = msg;
    el.className = cls ? `compose-status ${cls}` : "compose-status";
  };

  const reset = (): void => {
    identityForm.reset();
    previewForm.reset();
    deployForm.reset();
    deployForm.hidden = true;
    if (previewOut) previewOut.innerHTML = "";
    setStatus(identityStatusEl, "");
    setStatus(previewStatusEl, "");
    setStatus(deployStatusEl, "");
    // identityForm.reset() puts <select id="deploy-provider"> back to its
    // `selected` default ("aws"), but doesn't touch the field-group `hidden`
    // attributes this panel manages by hand — sync those too.
    showProviderFields(providerSel.value);
    applyNamespaceMode();
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

  // Toggle the AWS/k8s field groups per studio#104's design: switching
  // providers resets which group is visible; field *values* aren't cleared
  // here (identityForm.reset() already did that on open/close) since the
  // two groups have no overlapping semantics to accidentally carry over.
  const showProviderFields = (provider: string): void => {
    awsFieldsEl.hidden = provider !== "aws";
    k8sFieldsEl.hidden = provider !== "k8s";
  };

  const loadK8sNamespaces = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) return;
    const context = k8sContextSel.value || undefined;
    try {
      const res = await invoke<K8sNamespacesResponse>(
        "list_namespaces",
        context ? { context } : {},
      );
      const previous = k8sNamespaceSel.value;
      const opts = [
        '<option value="">— pick a namespace —</option>',
        ...res.namespaces.map((n) => `<option value="${escapeHtml(n)}">${escapeHtml(n)}</option>`),
        `<option value="${NAMESPACE_NEW_SENTINEL}">+ Create new namespace…</option>`,
      ];
      k8sNamespaceSel.innerHTML = opts.join("");
      // innerHTML replacement always resets selection to the first option —
      // restore it if the previously-selected namespace is still in the
      // refreshed list (e.g. reloading against the same context).
      if (previous && Array.from(k8sNamespaceSel.options).some((o) => o.value === previous)) {
        k8sNamespaceSel.value = previous;
      }
      applyNamespaceMode();
      // studio#119: a prior failed attempt (e.g. before switching context)
      // can leave its error text on screen — clear it once a load actually
      // succeeds, otherwise a stale error outlives the state it described.
      setStatus(identityStatusEl, "");
    } catch (e) {
      setStatus(identityStatusEl, `namespace list unavailable: ${errText(e)}`, "err");
    }
  };

  // Service account is scoped to (context, namespace) and per #104's design
  // fails *silently* — unlike context/namespace, any error here (including an
  // RBAC-denied list, which is common against a scoped-down cluster identity)
  // means "leave it unset" (the namespace's default service account applies),
  // not something worth surfacing a status message for.
  const loadK8sServiceAccounts = async (): Promise<void> => {
    const defaultOption = '<option value="">— namespace default —</option>';
    const invoke = tauriInvoke();
    const namespace = currentNamespace();
    if (!invoke || !namespace) {
      k8sServiceAccountSel.innerHTML = defaultOption;
      return;
    }
    const context = k8sContextSel.value || undefined;
    try {
      const res = await invoke<K8sServiceAccountsResponse>(
        "list_service_accounts",
        context ? { context, namespace } : { namespace },
      );
      k8sServiceAccountSel.innerHTML =
        defaultOption +
        res.service_accounts
          .map((sa) => `<option value="${escapeHtml(sa)}">${escapeHtml(sa)}</option>`)
          .join("");
    } catch {
      k8sServiceAccountSel.innerHTML = defaultOption;
    }
  };

  const loadK8sContexts = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) return;
    try {
      const res = await invoke<K8sContextsResponse>("list_k8s_contexts");
      const opts = ['<option value="">— kubeconfig current-context —</option>'];
      for (const c of res.contexts) {
        const label = c.name === res.current_context ? `${c.name} (current)` : c.name;
        opts.push(`<option value="${escapeHtml(c.name)}">${escapeHtml(label)}</option>`);
      }
      k8sContextSel.innerHTML = opts.join("");
      // studio#119: same staleness fix as loadK8sNamespaces — the
      // loadK8sNamespaces() call below will overwrite this with its own
      // result once it resolves, but clear here too so a stale error doesn't
      // linger for the gap between the two if that call is slow.
      setStatus(identityStatusEl, "");
    } catch (e) {
      setStatus(identityStatusEl, `k8s context list unavailable: ${errText(e)}`, "err");
    }
    void loadK8sNamespaces();
  };

  providerSel.addEventListener("change", () => {
    showProviderFields(providerSel.value);
    if (providerSel.value === "k8s") void loadK8sContexts();
  });
  k8sContextSel.addEventListener("change", () => {
    void loadK8sNamespaces();
    void loadK8sServiceAccounts();
  });
  k8sNamespaceSel.addEventListener("change", () => {
    applyNamespaceMode();
    void loadK8sServiceAccounts();
  });
  // "change" (fires on commit/blur), not "input" (every keystroke) — avoids a
  // tool call per character typed into the new-namespace field.
  k8sNamespaceNewInput.addEventListener("change", () => void loadK8sServiceAccounts());

  const loadLibraryAndPickers = async (): Promise<void> => {
    const invoke = tauriInvoke();
    if (!invoke) {
      setStatus(previewStatusEl, "browser build — deploy unavailable");
      return;
    }
    try {
      library = await invoke<Library>("compose_library_get");
      const { templates, overlays } = libraryNames(library);
      fillOptions(tmplSel, templates, false);
      fillOptions(ovlSel, overlays, true);
    } catch (e) {
      setStatus(previewStatusEl, `library load failed: ${errText(e)}`, "err");
    }
  };

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
    void loadLibraryAndPickers();
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
  identityForm.addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (!nameInput.value.trim()) {
      setStatus(identityStatusEl, "fleet name is required", "err");
      return;
    }
    identityForm.hidden = true;
    composeSection.hidden = false;
    if (composeHeading) composeHeading.textContent = "Step 2 — first instance";
  });

  previewForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const invoke = tauriInvoke();
    if (!invoke || !library) {
      setStatus(previewStatusEl, "deploy unavailable", "err");
      return;
    }
    const template = tmplSel.value;
    if (!template) {
      setStatus(previewStatusEl, "pick a template to preview", "err");
      return;
    }
    const overlay = ovlSel.value || null;
    setStatus(previewStatusEl, "composing…");
    try {
      const preview = await invoke<BundlePreview>("compose_preview", { library, template, overlay });
      if (previewOut) previewOut.innerHTML = renderPreviewHtml(preview);
      setStatus(previewStatusEl, `composed — ${preview.files.length} files`, "ok");
      deployForm.hidden = false;
      if (!agentNameInput.value) agentNameInput.value = overlay ?? template;
      imageInput.placeholder = preview.image_tag;
      setStatus(deployStatusEl, "");
    } catch (e) {
      if (previewOut) previewOut.innerHTML = "";
      deployForm.hidden = true;
      setStatus(previewStatusEl, `compose failed: ${errText(e)}`, "err");
    }
  });

  // The failure rule from 7.5.1/7.5.2: if `deploy_provision` fails, stop — no
  // `fleet_config_write` call, `fleets.toml` is untouched.
  deployForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const invoke = tauriInvoke();
    if (!invoke || !library || !mode) {
      setStatus(deployStatusEl, "deploy unavailable", "err");
      return;
    }
    const template = tmplSel.value;
    const name = agentNameInput.value.trim();
    if (!template) {
      setStatus(deployStatusEl, "preview a template first", "err");
      return;
    }
    if (!name) {
      setStatus(deployStatusEl, "agent name is required", "err");
      return;
    }
    const overlay = ovlSel.value || null;
    // Only "new-fleet" ever reaches provider "k8s" — identityForm (where the
    // provider <select> lives) is skipped for "add-instance", and reset()
    // (run on every open()) puts the <select> back to its "aws" default, so
    // an add-instance submit always sees "aws" here regardless of the fleet
    // it's adding to. Adding an instance to an *existing* k8s fleet isn't
    // wired through this wizard yet.
    const isK8s = mode.kind === "new-fleet" && providerSel.value === "k8s";
    const namespace = isK8s ? currentNamespace() || "default" : "default";
    const context = isK8s ? k8sContextSel.value || undefined : undefined;
    const serviceAccount = isK8s ? k8sServiceAccountSel.value : "";
    // studio-cp's provision_from_library_k8s expects the full
    // `system:serviceaccount:<ns>:<name>` form (it extracts the bare name
    // itself) — same shape as K8sFleetBinding.expected_principal.
    const expectedPrincipal = serviceAccount
      ? `system:serviceaccount:${namespace}:${serviceAccount}`
      : undefined;
    const image = imageInput.value.trim() || null;
    deployBtn.disabled = true;
    setStatus(deployStatusEl, "deploying…");
    let res: { image?: string; digest?: string; objects?: number };
    try {
      res = await invoke("deploy_provision", {
        library,
        template,
        overlay,
        name,
        namespace,
        image,
        ...(isK8s ? { provider: "k8s", context, expected_principal: expectedPrincipal } : {}),
      });
    } catch (e) {
      setStatus(deployStatusEl, `deploy failed: ${errText(e)}`, "err");
      deployBtn.disabled = false;
      return;
    }
    const service = `oab-${namespace}-${name}`;
    const fleetName = mode.kind === "new-fleet" ? nameInput.value.trim() : mode.fleetName;
    const configFile = isK8s ? "fleets-k8s.toml" : "fleets.toml";
    setStatus(deployStatusEl, `deployed ${service} — updating ${configFile}…`, "ok");
    try {
      if (isK8s) {
        const current = await deps.source.k8sFleetConfig();
        const nextText = appendK8sFleetBlock(current.text, {
          name: fleetName,
          member: service,
          context: context ?? null,
          namespace,
          expectedPrincipal: expectedPrincipal ?? null,
        });
        await deps.source.writeK8sFleetConfig(nextText);
      } else {
        const current = await deps.source.fleetConfig();
        const nextText =
          mode.kind === "new-fleet"
            ? appendFleetBlock(current.text, {
                name: fleetName,
                member: service,
                region: regionInput.value.trim() || null,
                profile: profileInput.value.trim() || null,
                expectedPrincipal: principalInput.value.trim() || null,
              })
            : appendMember(current.text, fleetName, service);
        await deps.source.writeFleetConfig(nextText);
      }
    } catch (e) {
      // The instance is live but the config file wasn't updated — surface it
      // rather than silently leaving the roster's membership stale.
      setStatus(deployStatusEl, `deployed ${service}, but ${configFile} update failed: ${errText(e)}`, "err");
      deployBtn.disabled = false;
      return;
    }
    deployBtn.disabled = false;
    const info: DeployedInfo = { fleetName, service, image: res.image ?? image ?? template };
    close();
    await deps.onDeployed(info);
  });

  return { open, close };
}
