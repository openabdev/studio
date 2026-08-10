// Config tab: read the current oab-mcp target from the backend, let the user
// pin profile / region / cluster, and on save persist + reload the core. Only
// meaningful inside the Tauri shell; the browser build disables the form.

export interface McpConfig {
  cluster: string;
  profile?: string | null;
  region?: string | null;
}

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function tauriInvoke(): Invoke | null {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } })
    .__TAURI__;
  return t?.core?.invoke ?? null;
}

// Tauri command rejections arrive as plain strings, not Error objects.
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface ConfigHooks {
  /** Fired with the config loaded from disk at startup. */
  onLoaded?: (cfg: McpConfig) => void;
  /** Fired after a successful save + core reload. */
  onSaved?: (cfg: McpConfig) => void;
}

export function initConfigTab(hooks: ConfigHooks = {}): void {
  const form = document.getElementById("config-form") as HTMLFormElement | null;
  const profile = document.getElementById("cfg-profile") as HTMLInputElement | null;
  const region = document.getElementById("cfg-region") as HTMLInputElement | null;
  const cluster = document.getElementById("cfg-cluster") as HTMLInputElement | null;
  const save = document.getElementById("cfg-save") as HTMLButtonElement | null;
  const status = document.getElementById("cfg-status");
  if (!form || !profile || !region || !cluster) return;

  const setStatus = (msg: string, cls = ""): void => {
    if (status) {
      status.textContent = msg;
      status.className = cls ? `config-status ${cls}` : "config-status";
    }
  };

  const invoke = tauriInvoke();
  if (!invoke) {
    // Browser build — no core to configure.
    setStatus("browser build — config unavailable");
    for (const el of form.querySelectorAll<HTMLInputElement | HTMLButtonElement>(
      "input, button",
    )) {
      el.disabled = true;
    }
    return;
  }

  // Populate the form from the persisted target.
  invoke<McpConfig>("get_config")
    .then((cfg) => {
      cluster.value = cfg.cluster ?? "";
      profile.value = cfg.profile ?? "";
      region.value = cfg.region ?? "";
      hooks.onLoaded?.(cfg);
    })
    .catch((e) => setStatus(`load failed: ${errText(e)}`, "err"));

  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const cfg: McpConfig = {
      cluster: cluster.value.trim() || "oab",
      profile: profile.value.trim() || null,
      region: region.value.trim() || null,
    };
    if (save) save.disabled = true;
    setStatus("saving & reloading core…");
    try {
      await invoke("set_config", { newConfig: cfg });
      setStatus("saved — core reloaded", "ok");
      hooks.onSaved?.(cfg);
    } catch (e) {
      setStatus(`save failed: ${errText(e)}`, "err");
    } finally {
      if (save) save.disabled = false;
    }
  });
}
