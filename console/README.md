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
| `src/fixtures.ts` | stand-in roster data |

## Wiring to the core (slice-2)

`TauriSource` calls the Tauri `deploy_list` command via the global bridge
(`window.__TAURI__.core.invoke`), so slice-1 carries no `@tauri-apps/api`
dependency. Slice-2 adds `src-tauri/` whose Rust `deploy_list` command bridges
to `studio-cp::observe_services` / `observe_deployment`. Because the boundary is
the read-model shape (and, later, MCP), swapping `MockSource` → `TauriSource` is
the only change the UI sees.
