# ADR: Push notifications — event-driven read-model over polling

- **Status:** Proposed
- **Realizes:** ADR-3's deferred "streaming later" for live updates (it does not supersede ADR-3; it fulfills the part ADR-3 punted).
- **Builds on:** ADR-2 (control-plane read-model + MCP), ADR-1 (6-state lifecycle).

> Deployment state should **arrive as events**, not be discovered by polling.
> Two boundaries go push: **skin ↔ core** over MCP resource subscriptions, and
> **core ↔ AWS** over EventBridge. Polling stays only as a reconciliation
> fallback. State transitions (e.g. `Running → Unhealthy`) then surface in near
> real time, and can drive **user-facing OS notifications** as a downstream
> consumer.

## 1. Context & Problem

Today the console **polls** `deploy_list` every 5 s (ADR-3, slice-1). That was
the right first cut, but it has structural costs:

- **Latency & waste** — a state change is seen on average half a poll interval
  late; most polls return "no change" yet still spend an ECS API round-trip per
  service.
- **Doesn't scale to many fleets** — the cost is `O(deployments × 1/interval)`
  of API calls, per open skin. Multiple skins multiply it.
- **No transition semantics** — polling samples *state*, not *events*. A brief
  `Unhealthy` blip between two polls is invisible; there is nothing to attach a
  notification/alert to.
- **User-facing notifications need a stream** — "tell me when orca goes
  Unhealthy" has no source event to fire on.

We want an event-driven read-model: the core learns of changes and **pushes**
them; skins (and notifiers) **subscribe**.

## 2. Decision Drivers

- Near-real-time 6-state transitions (ADR-1), not sampled state.
- Fewer API calls; cost scales with *change rate*, not *poll rate × skins*.
- A single event stream that both the roster view **and** notifications consume.
- Keep the skin ↔ core contract on **MCP** (ADR-3), not a bespoke channel.
- Incremental: ship value without first building AWS event infrastructure.
- Correctness under missed events (delivery is best-effort) — never silently
  drift.

## 3. Decision

Push at **two independent boundaries**, landed in two phases.

### 3.1 skin ↔ core — MCP resource subscription (Phase 1)

- Model the roster as an **MCP resource** (e.g. `oab://deployments/{cluster}`)
  with the ADR-2 read-model shape.
- The skin calls `resources/subscribe`; the core sends
  `notifications/resources/updated` when the resource changes; the skin re-reads
  (or consumes a delta carried on the notification).
- The **5 s poll is removed** from the skin. The core MAY still poll AWS
  internally in Phase 1 — the skin no longer knows or cares.
- Desktop plumbing already half-exists: `src-tauri`'s MCP client reader receives
  unsolicited lines today (they land in the MCP pane) but only correlates by
  `id`. Phase 1 makes it **act on id-less notifications** and forward
  `resources/updated` to the frontend as an event.

### 3.2 core ↔ AWS — EventBridge (Phase 2)

- Subscribe to **ECS Task State Change** and **ECS Service Action** events via
  an EventBridge rule → target (SQS queue the core drains, or a direct
  listener).
- On each event the core updates its cached read-model and emits the
  corresponding MCP `resources/updated`. Steady-state polling of ECS drops to
  **zero**.
- **Poll remains as reconciliation**, not the primary path: a low-frequency full
  resync (e.g. every N minutes, and on subscribe/reconnect) repairs any missed
  or out-of-order events. Events are best-effort; the periodic resync is the
  correctness backstop.

### 3.3 Provider-native event sources — normalized to one contract

EventBridge is the **AWS instance** of a general pattern: every provider already
emits its own change stream. The connection descriptor (per-`oab-mcp`, the
fleet-registry direction) owns the adapter that maps each into the same MCP
`resources/updated`; the skin never learns which provider is underneath.

| Provider | Native event source | Notes |
|---|---|---|
| **AWS / ECS** | EventBridge — `ECS Task State Change`, `ECS Service Action` | delivered via a rule → target (SQS/listener) |
| **Kubernetes** | **Watch API** — `?watch=true&resourceVersion=…` on Deployments/Pods (client-go **informer** = list→watch→cache→resync); plus the **Events API** (`Event` objects: `CrashLoopBackOff`, `OOMKilled`, … as ADR-1 Unhealthy *reasons*) | most native of the set |
| others | provider stream (poll bridge if none) | must normalize to the same contract |

**Kubernetes bakes in our reconciliation model.** A dropped watch — or a
`resourceVersion` too old (`410 Gone`) — forces the informer to **re-list**;
that relist *is* the §3.2 reconciliation resync, provided by the protocol rather
than bolted on. It is the reference shape the ECS adapter emulates (EventBridge
for deltas + a periodic ECS resync for repair).

### 3.4 User-facing notifications (consumer, not mechanism)

OS/mobile "push notifications" (a macOS notification when a watched deployment
enters `Unhealthy`/`Stopped`) are a **downstream consumer** of the same event
stream — a small rule engine over `resources/updated`, not a separate pipeline.
Kept out of scope for the mechanism ADR; unlocked by it.

## 4. Considered Options

1. **Keep polling (status quo).** Simple; but the costs in §1 stand and there is
   no event to notify on. Rejected as the end state; retained as fallback.
2. **Poll faster.** Lower latency at strictly higher cost; still no transition
   semantics. Rejected.
3. **skin↔core push only, core keeps polling AWS (Phase 1 alone).** Removes
   per-skin polling and gives the skin real events with **no AWS infra**. Good
   interim; core→AWS still polls. Chosen as Phase 1.
4. **Full event pipeline (Phase 1 + EventBridge).** Zero steady-state polling,
   true real-time. More infra (EventBridge rule + target, IAM). Chosen as the
   end state (Phase 2).

## 5. Consequences

- **The core becomes stateful**: it holds a cached read-model + a subscriber set.
  A new subscriber reads current state (`resources/read`), then receives
  `resources/updated` deltas; on reconnect it re-reads to re-sync. Previously each
  `deploy_list` was stateless.
- **Desktop MCP client** must handle id-less notifications (small change to the
  sidecar reader introduced in PR #9 / the sidecar ADR, once that lands).
- **Transport ceiling**: stdio carries server→client notifications fine for the
  local desktop core. Remote/iOS skins (ADR-3 deferred) need a streaming
  transport (streamable-HTTP/SSE) before they can subscribe — this ADR does not
  unblock remote; it defines the contract remote will use.
- **EventBridge infra** (Phase 2): a rule per account/region the fleet lives in,
  plus multi-account aggregation for cross-account fleets — ties into the
  fleet-registry / connection-descriptor direction (per-`oab-mcp` config).
- **Best-effort delivery**: without the reconciliation resync, a dropped event
  = silent drift. The resync interval is a correctness parameter, not a nicety.

## 6. Open Questions

- **Subscription granularity** — one resource per cluster (whole roster) vs per
  deployment. Start coarse (per cluster); revisit if churn is noisy.
- **Delta vs re-read** — does `resources/updated` carry the changed slice, or
  just signal "re-read the resource"? Re-read is simpler and idempotent; deltas
  cut bandwidth. Lean re-read first.
- **EventBridge delivery** — SQS-drained by the core vs a direct listener; and
  whether the core runs where it can reach the queue.
- **Reconciliation interval** — how stale is acceptable between resyncs.
- **Authorization** — who may subscribe (unchanged from ADR-2/ADR-3: currently
  the AWS-credential ceiling only; per-caller authz still deferred).
- **Multi-account / multi-vendor** — each provider's native stream (§3.3:
  EventBridge, k8s watch+events, …) must normalize to the same
  `resources/updated`; open points are cross-account EventBridge aggregation and
  where the k8s watch connection lives (in-core vs a per-cluster bridge).

## 7. More Information

Format follows MADR (context → drivers → options → decision → consequences),
consistent with ADR-1/ADR-2. Draft — for review/wording before Accepted.
