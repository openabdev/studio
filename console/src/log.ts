// Two observability panes for the desktop shell:
//   1. Activity  — backend `app-log` events (core lifecycle + failures) and
//      local UI events, so the first screen shows whether the app launched.
//   2. MCP       — backend `mcp-io` events: every JSON-RPC message to/from the
//      `oab-mcp` sidecar, so the raw interaction with the core is visible.
// In the browser build there is no core, so only the Activity pane says so.

export type Level = "debug" | "info" | "warn" | "error";

const MAX_LINES = 500;
const MAX_MSG = 4000;

export interface Pane {
  push(opts: { cls?: string; tag: string; msg: string }): void;
}

// `onPush` fires after every appended line — used to flag the tab when its pane
// is not the visible one.
export function createPane(el: HTMLElement, onPush?: () => void): Pane {
  el.innerHTML = "";
  return {
    push({ cls, tag, msg }) {
      const line = document.createElement("div");
      line.className = cls ? `logline ${cls}` : "logline";

      const t = document.createElement("span");
      t.className = "lt";
      t.textContent = new Date().toLocaleTimeString();

      const g = document.createElement("span");
      g.className = "ll";
      g.textContent = tag;

      const m = document.createElement("span");
      m.className = "lm";
      // textContent — never interpret core output as HTML.
      m.textContent = msg.length > MAX_MSG ? `${msg.slice(0, MAX_MSG)}…` : msg;

      line.append(t, g, m);
      el.appendChild(line);
      while (el.childElementCount > MAX_LINES && el.firstChild) {
        el.removeChild(el.firstChild);
      }
      el.scrollTop = el.scrollHeight;
      onPush?.();
    },
  };
}

// Minimal shape of the Tauri event global (v2, `withGlobalTauri`).
interface TauriEventGlobal {
  event?: {
    listen?: <T>(
      event: string,
      handler: (e: { payload: T }) => void,
    ) => Promise<unknown>;
  };
}

/** Route backend `app-log` / `mcp-io` events into the two panes. */
export async function bindBackend(activity: Pane, mcp: Pane): Promise<void> {
  const tauri = (globalThis as { __TAURI__?: TauriEventGlobal }).__TAURI__;
  const listen = tauri?.event?.listen;
  if (!listen) {
    activity.push({ cls: "lv-info", tag: "INFO", msg: "app: browser build — no core (fixtures)" });
    return;
  }
  await listen<{ level: Level; msg: string }>("app-log", (e) => {
    const level = e.payload?.level ?? "info";
    activity.push({ cls: `lv-${level}`, tag: level.toUpperCase(), msg: e.payload?.msg ?? "" });
  });
  await listen<{ dir: "out" | "in"; text: string }>("mcp-io", (e) => {
    const dir = e.payload?.dir === "out" ? "out" : "in";
    mcp.push({ cls: `io-${dir}`, tag: dir === "out" ? "→" : "←", msg: e.payload?.text ?? "" });
  });
}
