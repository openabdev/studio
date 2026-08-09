// A small Activity/Log pane. It streams the desktop backend's `app-log` events
// (core lifecycle: spawn → handshake → ready, plus the sidecar's stderr and any
// failure) and local UI events, so the app's first screen shows whether it
// launched and whether anything went wrong.

export type Level = "info" | "warn" | "error";

const MAX_LINES = 500;
let listEl: HTMLElement | null = null;

export function initLog(el: HTMLElement): void {
  listEl = el;
  el.innerHTML = "";
}

export function log(level: Level, msg: string): void {
  if (!listEl) return;
  const line = document.createElement("div");
  line.className = `logline lv-${level}`;

  const t = document.createElement("span");
  t.className = "lt";
  t.textContent = new Date().toLocaleTimeString();

  const lv = document.createElement("span");
  lv.className = "ll";
  lv.textContent = level.toUpperCase();

  const m = document.createElement("span");
  m.className = "lm";
  m.textContent = msg; // textContent — no HTML injection from core output

  line.append(t, lv, m);
  listEl.appendChild(line);
  while (listEl.childElementCount > MAX_LINES && listEl.firstChild) {
    listEl.removeChild(listEl.firstChild);
  }
  listEl.scrollTop = listEl.scrollHeight;
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

// Subscribe to backend log events when inside the Tauri shell; in the browser
// build there is no core, so just say so.
export async function bindBackendLog(): Promise<void> {
  const tauri = (globalThis as { __TAURI__?: TauriEventGlobal }).__TAURI__;
  const listen = tauri?.event?.listen;
  if (!listen) {
    log("info", "browser build — no core (fixtures)");
    return;
  }
  await listen<{ level: Level; msg: string }>("app-log", (e) => {
    log(e.payload?.level ?? "info", e.payload?.msg ?? "");
  });
}
