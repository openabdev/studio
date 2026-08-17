// The remote file editor's **read** path (ADR agent-consoles Part D): a
// directory browser over an agent's filesystem + a read-only viewer, mounted in
// an open agent console. It is capability-gated — fs is an MCP files server the
// target agent exposes (reached Studio-brokered via the `oab` relay), and that
// server does not exist yet, so on a real endpoint `fsCapability` reports
// unsupported and this renders a "pending the fs MCP files server" placeholder.
// The browser build's mock source serves a fixture filesystem so the surface is
// still demonstrable.
//
// The listing HTML is pure (`render.ts`, unit-tested); this owns the imperative
// shell: the fetch/navigate lifecycle, the delegated click handler, and the
// read-only CodeMirror viewer. The **write** path (Apply) is slice 4.

import { EditorView, basicSetup } from "codemirror";
import { EditorState, type Extension } from "@codemirror/state";
import { StreamLanguage } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import type { Source } from "./source";
import { fsListingHtml, fsUnavailableHtml } from "./render";

export interface FileBrowserElements {
  // The listing container (directory rows).
  list: HTMLElement;
  // The read-only CodeMirror mount.
  viewer: HTMLElement;
  // The open-file path / status line.
  title: HTMLElement;
}

export interface FileBrowserOptions {
  // The registry endpoint name whose filesystem is browsed.
  agent: string;
  source: Source;
  note: (level: "info" | "error", msg: string) => void;
}

export interface FileBrowser {
  dispose(): void;
}

const UNAVAILABLE = "Remote file editor unavailable — pending the fs MCP files server.";

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function dirname(path: string): string {
  const cut = path.replace(/\/+$/, "").replace(/\/[^/]+$/, "");
  return cut === "" ? "/" : cut;
}

export function createFileBrowser(
  els: FileBrowserElements,
  opts: FileBrowserOptions,
): FileBrowser {
  let roots: string[] = [];
  let cwd = "";
  let selectedPath: string | null = null;
  let view: EditorView | null = null;
  const ac = new AbortController();
  const { signal } = ac;

  function destroyViewer(): void {
    view?.destroy();
    view = null;
  }

  // Show a file's text in a fresh read-only editor. `.toml` gets TOML highlighting
  // (the mode already bundled for the config editor); everything else is plain.
  function showFile(path: string, text: string, truncated: boolean): void {
    destroyViewer();
    const ext: Extension[] = [
      basicSetup,
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
    if (path.endsWith(".toml")) ext.push(StreamLanguage.define(toml));
    view = new EditorView({
      parent: els.viewer,
      state: EditorState.create({ doc: text, extensions: ext }),
    });
    els.title.textContent = truncated ? `${path} · truncated` : path;
  }

  // The "up one level" affordance shows while we're below an editable root.
  function canGoUp(): boolean {
    return !roots.includes(cwd) && cwd !== "/" && cwd !== "";
  }

  function renderList(listing: Parameters<typeof fsListingHtml>[0]): void {
    els.list.innerHTML = fsListingHtml(listing, {
      selectedPath,
      canGoUp: canGoUp(),
    });
  }

  async function loadDir(path: string): Promise<void> {
    try {
      const listing = await opts.source.fsList(path, opts.agent);
      cwd = listing.path || path;
      renderList(listing);
    } catch (e) {
      els.list.innerHTML = fsUnavailableHtml(`cannot list ${path} — ${errText(e)}`);
    }
  }

  async function openFile(path: string): Promise<void> {
    try {
      const file = await opts.source.fsRead(path, opts.agent);
      selectedPath = file.path || path;
      showFile(selectedPath, file.text, file.truncated);
      // Re-render the current listing so the open row is marked.
      await loadDir(cwd);
    } catch (e) {
      opts.note("error", `files: read ${path} failed — ${errText(e)}`);
      els.title.textContent = `${path} · read failed`;
    }
  }

  async function init(): Promise<void> {
    els.title.textContent = "files";
    let cap;
    try {
      cap = await opts.source.fsCapability(opts.agent);
    } catch (e) {
      els.list.innerHTML = fsUnavailableHtml(`fs capability check failed — ${errText(e)}`);
      return;
    }
    if (!cap.supported) {
      els.list.innerHTML = fsUnavailableHtml(UNAVAILABLE);
      return;
    }
    roots = cap.roots.length ? cap.roots : ["/"];
    await loadDir(roots[0]);
  }

  els.list.addEventListener(
    "click",
    (ev) => {
      const t = ev.target as HTMLElement;
      const dir = t.closest<HTMLElement>("[data-fs-dir]");
      if (dir?.dataset.fsDir) {
        void loadDir(dir.dataset.fsDir);
        return;
      }
      const file = t.closest<HTMLElement>("[data-fs-file]");
      if (file?.dataset.fsFile) {
        void openFile(file.dataset.fsFile);
        return;
      }
      if (t.closest("[data-fs-up]")) void loadDir(dirname(cwd));
    },
    { signal },
  );

  void init();

  return {
    dispose: () => {
      destroyViewer();
      ac.abort();
    },
  };
}
