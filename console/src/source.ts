import type { Deployment, FleetConfig, RuntimeContext } from "./types";
import {
  FIXTURE_DEPLOYMENTS,
  FIXTURE_FLEET_CONFIG,
  FIXTURE_RUNTIME_CONTEXT,
} from "./fixtures";

// A read source for the console. Desktop (Tauri → studio-cp) and the standalone
// browser build implement this identically, so the UI never knows which it is.
export interface Source {
  listDeployments(cluster?: string): Promise<Deployment[]>;
  runtimeContext(cluster?: string): Promise<RuntimeContext>;
  fleetConfig(): Promise<FleetConfig>;
}

// Fixture-backed source for the standalone / browser build — no core required.
export class MockSource implements Source {
  async listDeployments(): Promise<Deployment[]> {
    return structuredClone(FIXTURE_DEPLOYMENTS);
  }
  async runtimeContext(): Promise<RuntimeContext> {
    return structuredClone(FIXTURE_RUNTIME_CONTEXT);
  }
  async fleetConfig(): Promise<FleetConfig> {
    return structuredClone(FIXTURE_FLEET_CONFIG);
  }
}

// Minimal shape of the Tauri global bridge (v2, `withGlobalTauri`). Accessed via
// the global so slice-1 carries no `@tauri-apps/api` dependency; slice-2 wires
// the `deploy_list` command to studio-cp.
interface TauriGlobal {
  core?: { invoke?<T>(cmd: string, args?: Record<string, unknown>): Promise<T> };
}

// Desktop source: invokes the Tauri `deploy_list` command, which bridges to
// studio-cp. Active only inside the Tauri shell.
export class TauriSource implements Source {
  private invoke(): <T>(
    cmd: string,
    args?: Record<string, unknown>,
  ) => Promise<T> {
    const tauri = (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__;
    const invoke = tauri?.core?.invoke;
    if (!invoke) throw new Error("Tauri bridge unavailable");
    return invoke;
  }
  async listDeployments(cluster?: string): Promise<Deployment[]> {
    return this.invoke()<Deployment[]>("deploy_list", { cluster });
  }
  async runtimeContext(cluster?: string): Promise<RuntimeContext> {
    return this.invoke()<RuntimeContext>("runtime_context", { cluster });
  }
  async fleetConfig(): Promise<FleetConfig> {
    return this.invoke()<FleetConfig>("fleet_config");
  }
}

// Pick a source: Tauri when running inside the shell, else the mock.
export function defaultSource(): Source {
  const inTauri =
    typeof (globalThis as { __TAURI__?: unknown }).__TAURI__ !== "undefined";
  return inTauri ? new TauriSource() : new MockSource();
}
