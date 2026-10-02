# studio-console (ADR-3 slice-1)

The Studio director's console — **web skin**. A thin front-end over the control
plane's read-model: it renders the deployment **roster** with each instance's
canonical **6-state** and `ready/desired/current` counters, refreshed by
**polling**.

Per [ADR-3](../docs/adr/desktop-console.md), the skin is a client of the
`oab-mcp` / studio-cp read-model — it does not re-implement observation. The
same web build is both the Tauri desktop front-end (slice-2) and a standalone
browser console.

## Run

```sh
cd console
npm install
npm run dev        # http://localhost:5173 — renders from MockSource fixtures
```

No core is required: outside the Tauri shell the console uses `MockSource`
(fixtures). A static preview is produced by `npm run build` → open
`dist/index.html` directly (file://).

## Verify

```sh
npm run typecheck  # tsc --noEmit, strict
npm test           # vitest — rosterHtml rendering logic
npm run build      # tsc + vite build → dist/
```

## Structure

| File | Role |
|------|------|
| `src/types.ts` | view-model contract — mirrors studio-cp `Deployment`/`InstancePhase` + `AgentState` |
| `src/source.ts` | `Source` interface + `MockSource` (fixtures) / `TauriSource` (desktop) |
| `src/render.ts` | pure `rosterHtml(deployments)` → table; `renderRoster` sets it on the DOM |
| `src/main.ts` | polls `Source` every 5s and re-renders |
| `src/deploy.ts` | `[+ New fleet]` / `[+ Add instance]` wizard — collects the fields, then makes the one `deploy_provision_agent` call |
| `src/deployArgs.ts` | pure wizard-fields → `deploy_provision_agent` args (studio#111) |
| `src/fleetToml.ts` | pure `fleets.toml` block edits — the post-deploy config write |
| `src/fixtures.ts` | stand-in roster data |

## Wiring to the core (slice-2)

`TauriSource` calls the Tauri `deploy_list` command via the global bridge
(`window.__TAURI__.core.invoke`), so slice-1 carries no `@tauri-apps/api`
dependency. Slice-2 adds `src-tauri/` whose Rust `deploy_list` command bridges
to `studio-cp::observe_services` / `observe_deployment`. Because the boundary is
the read-model shape (and, later, MCP), swapping `MockSource` → `TauriSource` is
the only change the UI sees.

## Deploy credentials (studio#111)

A deploy is the one call where the wizard's AWS answers have nowhere else to
live: `fleets.toml` is written only *after* a confirmed successful provision
(ADR-83 §7.5), so on a first create the Region + Credential profile the
identity step collected exist solely in the `deploy_provision_agent` arguments
(`src/deployArgs.ts`).

`oab-mcp` resolves the managing credential in two steps: the fleet a call *named*
(`fleet:`) wins, else the binding whose `cluster` key matches, else the ambient
`[default]` chain.

The wizard's deploy call matches neither key: it names no fleet, and a
console-written `[fleet.<name>]` block declares no `cluster` key — so the
recorded pair is read by nothing and the ambient chain answers. (The console's
*read* and scale calls do name a fleet, but `target()` rejects a `fleet`-scoped
ECS call whose binding omits `cluster` — a separate, pre-existing gap; the
deploy call steps around it by passing `cluster` instead.) On a first create the
ambient chain is also what `build_default_manifest`'s VPC/subnet/security-group
discovery runs against, so a fleet created for one account/region would land in
another. Drop the fields and the wizard's answer is silently discarded. Naming
only one of the two is safe: the sidecar layers it onto whatever base binding it
found rather than substituting for it.
