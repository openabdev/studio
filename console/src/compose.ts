// Compose: the template/overlay/skills library types + the composed
// `{path → bytes}` bundle preview (agent-deployment ADR, slice 1). The
// standalone authoring tab this module used to wire (`initComposeTab`) is
// gone — `[+ New fleet]`/`[+ Add instance]` (`deploy.ts`) reach the same
// compose→preview→deploy engine as an action instead, reusing the pure
// helpers below. Everything real runs through the backend: the library is
// persisted by `compose_library_set`, and the preview is composed by
// `compose_preview` (the pure Rust `studio-compose` seam).
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
