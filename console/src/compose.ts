// Compose tab: author the template/overlay/skills library and preview the
// composed `{path → bytes}` agent bundle (agent-deployment ADR, slice 1).
//
// The library is edited as one JSON document (same "edit the config text, save,
// reload" idiom as fleets.toml / remote.toml) and the preview is rendered
// read-only. Everything real runs through the backend: the library is persisted
// by `compose_library_set`, and the preview is composed by `compose_preview`
// (the pure Rust `studio-compose` seam) — so the browser build, which has no
// backend, disables the panel rather than re-implementing compose in TS.
//
// These types mirror `studio_compose`'s serde shapes 1:1.

export interface Template {
  name: string;
  image_tag: string;
  files: Record<string, string>;
  skills: string[];
}
export interface Overlay {
  name: string;
  image_tag?: string | null;
  files: Record<string, string>;
  skills: string[];
}
export interface Skill {
  files: Record<string, string>;
}
export interface SkillsLibrary {
  skills: Record<string, Skill>;
}
export interface Library {
  templates: Record<string, Template>;
  overlays: Record<string, Overlay>;
  skills: SkillsLibrary;
}
export interface FilePreview {
  path: string;
  text: string;
  bytes: number;
  binary: boolean;
}
export interface BundlePreview {
  image_tag: string;
  digest: string;
  files: FilePreview[];
}

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function tauriInvoke(): Invoke | null {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

// Tauri command rejections arrive as plain strings, not Error objects.
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

/** Render the composed-bundle preview to HTML — a pure view (unit-tested). */
export function renderPreviewHtml(preview: BundlePreview): string {
  const rows = preview.files
    .map((f) => {
      const meta = f.binary ? `${f.bytes} bytes · binary` : `${f.bytes} bytes`;
      const body = f.binary
        ? `<pre class="compose-file-body compose-file-binary">(binary — ${f.bytes} bytes, not shown)</pre>`
        : `<pre class="compose-file-body">${escapeHtml(f.text)}</pre>`;
      return (
        `<details class="compose-file">` +
        `<summary><code>${escapeHtml(f.path)}</code><span class="compose-file-meta">${escapeHtml(meta)}</span></summary>` +
        body +
        `</details>`
      );
    })
    .join("");
  const count = preview.files.length;
  return (
    `<div class="compose-bundle-head">` +
    `<div><span class="compose-k">image</span> <code>${escapeHtml(preview.image_tag)}</code></div>` +
    `<div><span class="compose-k">digest</span> <code>${escapeHtml(preview.digest)}</code></div>` +
    `<div><span class="compose-k">files</span> ${count}</div>` +
    `</div>` +
    `<div class="compose-files">${rows}</div>`
  );
}

/** Names present in the parsed library, sorted — drives the picker `<option>`s. */
export function libraryNames(lib: Library): { templates: string[]; overlays: string[] } {
  const keys = (o: Record<string, unknown> | undefined): string[] =>
    o ? Object.keys(o).sort() : [];
  return { templates: keys(lib.templates), overlays: keys(lib.overlays) };
}

function fillOptions(sel: HTMLSelectElement, names: string[], keepNoneFirst: boolean): void {
  const prev = sel.value;
  const opts = keepNoneFirst ? ['<option value="">— none (bare template) —</option>'] : [];
  for (const n of names) opts.push(`<option value="${escapeHtml(n)}">${escapeHtml(n)}</option>`);
  sel.innerHTML = opts.join("");
  // Preserve the operator's selection across a repopulate if it still exists.
  if ([...sel.options].some((o) => o.value === prev)) sel.value = prev;
}

export function initComposeTab(): void {
  const text = document.getElementById("compose-lib-text") as HTMLTextAreaElement | null;
  const tmplSel = document.getElementById("compose-template") as HTMLSelectElement | null;
  const ovlSel = document.getElementById("compose-overlay") as HTMLSelectElement | null;
  const form = document.getElementById("compose-form") as HTMLFormElement | null;
  const saveBtn = document.getElementById("compose-save") as HTMLButtonElement | null;
  const out = document.getElementById("compose-preview");
  const statusEl = document.getElementById("compose-status");
  if (!text || !tmplSel || !ovlSel || !form) return;

  const setStatus = (msg: string, cls = ""): void => {
    if (statusEl) {
      statusEl.textContent = msg;
      statusEl.className = cls ? `compose-status ${cls}` : "compose-status";
    }
  };

  const invoke = tauriInvoke();
  if (!invoke) {
    setStatus("browser build — compose unavailable");
    text.disabled = true;
    tmplSel.disabled = true;
    ovlSel.disabled = true;
    for (const b of form.querySelectorAll("button")) b.disabled = true;
    if (saveBtn) saveBtn.disabled = true;
    return;
  }

  // Best-effort: parse the editor text and refresh the pickers so newly authored
  // templates/overlays are selectable before a save. Silent on parse error — the
  // Preview/Save buttons surface it with a real message.
  const syncPickers = (): void => {
    try {
      const lib = JSON.parse(text.value) as Library;
      const { templates, overlays } = libraryNames(lib);
      fillOptions(tmplSel, templates, false);
      fillOptions(ovlSel, overlays, true);
    } catch {
      /* leave pickers as-is until the JSON is valid */
    }
  };

  const parseLibrary = (): Library => JSON.parse(text.value) as Library;

  invoke<Library>("compose_library_get")
    .then((lib) => {
      text.value = JSON.stringify(lib, null, 2);
      syncPickers();
    })
    .catch((e) => setStatus(`load failed: ${errText(e)}`, "err"));

  text.addEventListener("input", syncPickers);

  saveBtn?.addEventListener("click", async () => {
    let lib: Library;
    try {
      lib = parseLibrary();
    } catch (e) {
      setStatus(`invalid JSON: ${errText(e)}`, "err");
      return;
    }
    saveBtn.disabled = true;
    setStatus("saving…");
    try {
      const saved = await invoke<Library>("compose_library_set", { library: lib });
      text.value = JSON.stringify(saved, null, 2);
      syncPickers();
      setStatus("saved", "ok");
    } catch (e) {
      setStatus(`save failed: ${errText(e)}`, "err");
    } finally {
      saveBtn.disabled = false;
    }
  });

  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    let lib: Library;
    try {
      lib = parseLibrary();
    } catch (e) {
      setStatus(`invalid JSON: ${errText(e)}`, "err");
      return;
    }
    const template = tmplSel.value;
    if (!template) {
      setStatus("pick a template to preview", "err");
      return;
    }
    const overlay = ovlSel.value || null;
    setStatus("composing…");
    try {
      const preview = await invoke<BundlePreview>("compose_preview", { library: lib, template, overlay });
      if (out) out.innerHTML = renderPreviewHtml(preview);
      setStatus(`composed — ${preview.files.length} files`, "ok");
    } catch (e) {
      if (out) out.innerHTML = "";
      setStatus(`compose failed: ${errText(e)}`, "err");
    }
  });
}
