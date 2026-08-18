// Config tab: read the current oab-mcp target from the backend, let the user pin
// cluster / profile / region, and on save persist + reload the core onto it.
// Only meaningful inside the Tauri shell; the browser build disables the form.
//
// The backend target is provider-tagged (`McpTarget`, config.rs). Today the only
// variant is ECS, so the form maps 1:1 to `{ provider: "ecs", cluster, profile,
// region }`; a future provider would add its own fields/section.

export interface EcsTarget {
  provider: "ecs";
  cluster: string;
  profile?: string | null;
  region?: string | null;
}
export type McpTarget = EcsTarget;

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function tauriInvoke(): Invoke | null {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

// Tauri command rejections arrive as plain strings, not Error objects.
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface ConfigHooks {
  /** Fired with the target loaded from disk at startup. */
  onLoaded?: (t: McpTarget) => void;
  /** Fired after a successful save + core reload. */
  onSaved?: (t: McpTarget) => void;
}

export function initConfigTab(hooks: ConfigHooks = {}): void {
  const form = document.getElementById("config-form") as HTMLFormElement | null;
  const profile = document.getElementById("cfg-profile") as HTMLInputElement | null;
  const region = document.getElementById("cfg-region") as HTMLInputElement | null;
  const cluster = document.getElementById("cfg-cluster") as HTMLInputElement | null;
  const save = document.getElementById("debug-cfg-save") as HTMLButtonElement | null;
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

  // Populate the form from the persisted (or env-seeded) target.
  invoke<McpTarget>("mcp_target_get")
    .then((t) => {
      cluster.value = t.cluster ?? "";
      profile.value = t.profile ?? "";
      region.value = t.region ?? "";
      hooks.onLoaded?.(t);
    })
    .catch((e) => setStatus(`load failed: ${errText(e)}`, "err"));

  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const target: McpTarget = {
      provider: "ecs",
      cluster: cluster.value.trim() || "oab",
      profile: profile.value.trim() || null,
      region: region.value.trim() || null,
    };
    if (save) save.disabled = true;
    setStatus("saving & reloading core…");
    try {
      await invoke("mcp_target_set", { target });
      setStatus("saved — core reloaded", "ok");
      hooks.onSaved?.(target);
    } catch (e) {
      setStatus(`save failed: ${errText(e)}`, "err");
    } finally {
      if (save) save.disabled = false;
    }
  });
}
