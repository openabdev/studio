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
  const regionInput = document.getElementById("deploy-region") as HTMLInputElement | null;
  const profileInput = document.getElementById("deploy-profile") as HTMLInputElement | null;
  const principalInput = document.getElementById("deploy-principal") as HTMLInputElement | null;
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
    !regionInput ||
    !profileInput ||
    !principalInput ||
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
  };

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
    const namespace = "default";
    const image = imageInput.value.trim() || null;
    deployBtn.disabled = true;
    setStatus(deployStatusEl, "deploying…");
    let res: { image?: string; digest?: string; objects?: number };
    try {
      res = await invoke("deploy_provision", { library, template, overlay, name, namespace, image });
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
              region: regionInput.value.trim() || null,
              profile: profileInput.value.trim() || null,
              expectedPrincipal: principalInput.value.trim() || null,
            })
          : appendMember(current.text, fleetName, service);
      await deps.source.writeFleetConfig(nextText);
    } catch (e) {
      // The instance is live but fleets.toml wasn't updated — surface it
      // rather than silently leaving the roster's membership stale.
      setStatus(deployStatusEl, `deployed ${service}, but fleets.toml update failed: ${errText(e)}`, "err");
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
