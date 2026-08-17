import type {
  AgentEndpointView,
  Deployment,
  FleetConfig,
  RegistryConfig,
  RemoteConfig,
  RuntimeContext,
} from "./types";
import {
  FIXTURE_AGENTS,
  FIXTURE_DEPLOYMENTS,
  FIXTURE_FLEET_CONFIG,
  FIXTURE_REGISTRY_CONFIG,
  FIXTURE_REMOTE_CONFIG,
  FIXTURE_RUNTIME_CONTEXT,
} from "./fixtures";

// A read source for the console. Desktop (Tauri → studio-cp) and the standalone
// browser build implement this identically, so the UI never knows which it is.
export interface Source {
  listDeployments(cluster?: string): Promise<Deployment[]>;
  runtimeContext(cluster?: string): Promise<RuntimeContext>;
  fleetConfig(): Promise<FleetConfig>;
  // Persist the raw TOML `text` of the config file, returning the reloaded
  // config. Rejects (without writing) when the text doesn't parse.
  writeFleetConfig(text: string): Promise<FleetConfig>;
  // Scale a deployment on (size 1) or off (size 0) — the start/stop action.
  // Reversible: ECS keeps the Spec at desiredCount 0, so no state store is
  // needed. `namespace` is required (the service is `oab-{namespace}-{name}`);
  // the managing credential is resolved per-cluster from `cluster`.
  scaleDeployment(
    name: string,
    size: 0 | 1,
    namespace: string,
    cluster?: string,
  ): Promise<void>;
  // The remote reverse-MCP connection (Part B): the management endpoint's parsed
  // url + live status for the panel. The editor now edits the registry
  // (`agents.toml`, below); `writeRemoteConfig` remains for the legacy
  // `remote.toml` path but is no longer wired to a button.
  remoteConfig(): Promise<RemoteConfig>;
  writeRemoteConfig(text: string): Promise<RemoteConfig>;
  // The per-agent endpoint registry (`agents.toml`) as raw text for the editor.
  // `registryConfig` is seeded from the adopted legacy `remote.toml` when the
  // file is absent (first save migrates); `writeRegistryConfig` validates the
  // structure (unique names, ≤1 management) and rejects without writing on error.
  registryConfig(): Promise<RegistryConfig>;
  writeRegistryConfig(text: string): Promise<RegistryConfig>;
  // Dial / tear down a connection. `agent` names a registry endpoint (an agent
  // console); omitted ⇒ the management endpoint (legacy single-console path).
  remoteConnect(agent?: string): Promise<void>;
  remoteDisconnect(agent?: string): Promise<void>;
  // The per-agent endpoint registry with each entry's live status — what an
  // agent-console selector renders (ADR agent-consoles Parts B/C). Tokens are
  // never included.
  remoteAgents(): Promise<AgentEndpointView[]>;
  // Chat over the live connection (Part C): send one turn (`session/prompt`) or
  // cancel the in-flight turn (`session/cancel`) for a given endpoint. `agent`
  // names the registry endpoint; omitted ⇒ the management endpoint. The reply
  // streams back as `agent-update` events, not through this call, so both
  // resolve immediately.
  agentPrompt(text: string, agent?: string): Promise<void>;
  agentCancel(agent?: string): Promise<void>;
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
  // Browser preview: no core, so "saving" just echoes the text back (no
  // persistence, no server-side TOML validation).
  async writeFleetConfig(text: string): Promise<FleetConfig> {
    return { ...structuredClone(FIXTURE_FLEET_CONFIG), text };
  }
  // Browser preview: no core, so scaling is a no-op — the fixture roster is
  // re-cloned each poll, so nothing would persist anyway.
  async scaleDeployment(): Promise<void> {}
  async remoteConfig(): Promise<RemoteConfig> {
    return structuredClone(FIXTURE_REMOTE_CONFIG);
  }
  async writeRemoteConfig(text: string): Promise<RemoteConfig> {
    return { ...structuredClone(FIXTURE_REMOTE_CONFIG), text };
  }
  async registryConfig(): Promise<RegistryConfig> {
    return structuredClone(FIXTURE_REGISTRY_CONFIG);
  }
  // Browser preview: no core, so "saving" echoes the text back — no persistence
  // and no server-side structural validation.
  async writeRegistryConfig(text: string): Promise<RegistryConfig> {
    return { ...structuredClone(FIXTURE_REGISTRY_CONFIG), text };
  }
  // Browser preview: no core to dial, so activate/deactivate are no-ops.
  async remoteConnect(): Promise<void> {}
  async remoteDisconnect(): Promise<void> {}
  async remoteAgents(): Promise<AgentEndpointView[]> {
    return structuredClone(FIXTURE_AGENTS);
  }
  // Browser preview: no live agent. `main.ts` detects the mock (no Tauri shell)
  // and drives a canned reply locally so the panel is still demonstrable.
  async agentPrompt(): Promise<void> {}
  async agentCancel(): Promise<void> {}
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
  async writeFleetConfig(text: string): Promise<FleetConfig> {
    return this.invoke()<FleetConfig>("fleet_config_write", { text });
  }
  async scaleDeployment(
    name: string,
    size: 0 | 1,
    namespace: string,
    cluster?: string,
  ): Promise<void> {
    await this.invoke()<unknown>("deploy_scale", {
      name,
      size,
      namespace,
      cluster,
    });
  }
  async remoteConfig(): Promise<RemoteConfig> {
    return this.invoke()<RemoteConfig>("remote_config");
  }
  async writeRemoteConfig(text: string): Promise<RemoteConfig> {
    return this.invoke()<RemoteConfig>("remote_config_write", { text });
  }
  async registryConfig(): Promise<RegistryConfig> {
    return this.invoke()<RegistryConfig>("registry_config");
  }
  async writeRegistryConfig(text: string): Promise<RegistryConfig> {
    return this.invoke()<RegistryConfig>("registry_config_write", { text });
  }
  async remoteConnect(agent?: string): Promise<void> {
    await this.invoke()<unknown>("remote_connect", { agent });
  }
  async remoteDisconnect(agent?: string): Promise<void> {
    await this.invoke()<unknown>("remote_disconnect", { agent });
  }
  async remoteAgents(): Promise<AgentEndpointView[]> {
    const res = await this.invoke()<{ agents: AgentEndpointView[] }>(
      "remote_agents",
    );
    return res.agents;
  }
  async agentPrompt(text: string, agent?: string): Promise<void> {
    await this.invoke()<unknown>("agent_prompt", { text, agent });
  }
  async agentCancel(agent?: string): Promise<void> {
    await this.invoke()<unknown>("agent_cancel", { agent });
  }
}

// Pick a source: Tauri when running inside the shell, else the mock.
export function defaultSource(): Source {
  const inTauri =
    typeof (globalThis as { __TAURI__?: unknown }).__TAURI__ !== "undefined";
  return inTauri ? new TauriSource() : new MockSource();
}
