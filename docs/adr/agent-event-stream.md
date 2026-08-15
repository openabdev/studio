# ADR: Agent Event Stream — platform-agnostic event source + notification facade

- **Status:** Proposed
- **Date:** 2026-08-11
- **Author:** Orca (ecs-claude)
- **Reviewers:** TBD — Mira (ECS), Jellyfish (control-plane), Falcon (MCP)
- **Realizes:** the observation half of **ADR-4**'s reservation in
  [ADR-2 §10](./deployment-control-plane.md) — a *durable status store* and a
  replacement for *"front-ends live-observe on demand"*. The controller /
  reconcile-loop half of ADR-4 is a sibling that can build on this.
- **Builds on:** [ADR-1](./agent-lifecycle.md) (6-state vocabulary),
  [ADR-2](./deployment-control-plane.md) (read/write model + MCP surface).
- **Consolidates:** the earlier *push-notifications* draft — its skin↔core
  subscription phasing, the poll-as-reconciliation-backstop invariant, and the
  user-facing-notification consumer fold into §2.4 here (single event canon).

> In the context of front-ends and agents needing to **know when an Instance
> changes** (a task dies, a service goes impaired, a probe fails), facing the
> fact that ADR-2's read model is **snapshot-only** (`DescribeTasks` tells you
> *now*, never *what happened*) and each runtime exposes events differently
> (ECS→EventBridge, k8s→watch), we introduce a single **`EventSource`** adapter
> (per platform) feeding one normalized **`EventHub`** facade, which serves both
> a **pull** history (`deploy_events`) and a **push** stream (MCP notifications /
> webhook), to achieve one contract over N runtimes and M consumers, accepting a
> background subscriber process and an external archive for durability.

## 1. Context & Problem

ADR-2's read model (`observe_deployment` → 6-state) is a **point-in-time
snapshot**. It cannot answer *"did Mira just die, and why?"* — only *"is Mira
`Running` right now?"*. Front-ends therefore **live-observe on demand** (poll),
which ADR-2 §10 explicitly flagged as temporary until ADR-4.

Two forces make an event dimension necessary now:

1. **A real timeline** — task stops (+`stoppedReason`), service impairment, and
   deployments are transitions `DescribeTasks` structurally cannot surface. The
   `deploy_events` tool (already merged, PR #16) reads them back from an archive
   but has **no source wired** and **no live path**.
2. **Push, not poll** — a desktop dashboard (the ADR desktop-core-sidecar MCP
   client) and agents both want to *be told* on change, not to poll. MCP is a
   bidirectional JSON-RPC contract: a server may emit `notifications/*` to a
   connected client at any time.

But **each runtime exposes events differently**, and ADR-2 §10 already reserves
*"k8s / compose drivers → later"*. Hard-wiring the ECS path (EventBridge) into
`oab-mcp` would repeat the mistake ADR desktop-core-sidecar fixed: a
platform-specific shortcut behind a contract that claims to be generic.

**A key runtime asymmetry** shapes scope: ECS **Task State Change events omit
container `healthStatus`** — a `RUNNING`→unhealthy flip is *not* emitted (only
stop/replace, service-action, deployment are). k8s **does** emit probe-failure
Events (`Unhealthy`, `BackOff`, `OOMKilling`). The abstraction must not assume
every transition is event-observable on every platform (this mirrors ADR-1's
*unobservable Unhealthy* case).

## 2. Decision

Introduce one adapter trait and one facade; keep the MCP tool surface unchanged.

### 2.1 Normalized event

Generalize the ECS-flavoured `EcsEvent` (in `oabctl::events`, from PR #16) into a
platform-neutral **`AgentEvent`**, in the `studio-cp` seam:

```
struct AgentEvent {
    time,                 // RFC3339, when it fired
    platform,             // ecs | k8s | …
    agent,                // canonical service/agent id (oab-{ns}-{name} / pod)
    kind,                 // Stopped | Unhealthy | Impaired | Deployment | …
    reason,               // stoppedReason / event message
    raw,                  // passthrough envelope
}
```

`kind` maps onto ADR-1 vocabulary where a transition corresponds to a 6-state
change; free-form runtime events (deployment start/complete) carry their own
`kind`.

### 2.2 `EventSource` — the per-platform adapter

```
trait EventSource {
    async fn list(&self, filter) -> Result<Vec<AgentEvent>>;   // pull / history
    fn subscribe(&self)         -> impl Stream<AgentEvent>;    // push / live
}
```

- **`EcsEventSource`** — `list` = CloudWatch Logs `FilterLogEvents` (**already
  built**: `oabctl::events::fetch_ecs_events`); `subscribe` = SQS long-poll fed
  by an EventBridge rule (`source: aws.ecs`, cluster-scoped).
- **`K8sEventSource`** — `list` = Events API list; `subscribe` = **watch /
  informer** (the API server *is* the push transport; no EventBridge/SQS needed).
- **`NoopEventSource`** — stdio/dev; empty stream.

The concrete source is chosen by config/env, exactly as `oab-mcp` already
threads its AWS config and default cluster (ADR-2).

### 2.3 `EventHub` — the normalization + fan-out facade

One hub decouples **N sources** from **M sinks**:

```
sources ──AgentEvent──▶ EventHub ──▶ pull:  deploy_events(list)
                                  └─▶ push:  ├─ MCP notifications/resources/updated → client
                                             └─ webhook (Discord/SNS) for offline alerting
```

- **Pull** backs `deploy_events` (ADR-2 tool surface, unchanged).
- **Push** emits MCP `notifications/resources/updated` (resource-subscription
  model) to whichever client is connected — the desktop sidecar's `McpClient`
  already forwards `(event, payload)` to its frontend, so a live dashboard is a
  UI subscription away.
- The **webhook** sink is the *only* path that reaches an operator while **no
  MCP client is connected** (see §5).

### 2.4 Delivery, phasing & the reconciliation backstop
(Folds the *push-notifications* draft into this ADR.)

- **Skin ↔ core, Phase 1 (client wiring).** Today the desktop `McpClient` reader
  correlates replies **by `id` only**; unsolicited `notifications/resources/updated`
  land inertly in the MCP pane. Phase 1 makes the reader **act on id-less
  notifications** — forward `resources/updated` to the frontend as an event — and
  **removes the skin's 5 s `deploy_list` poll**. The roster becomes an MCP
  resource (`oab://deployments/{cluster}`, ADR-2 shape) the skin
  `resources/subscribe`s.
- **Poll is the reconciliation backstop, not the primary path.** Push delivery is
  **best-effort**; a low-frequency full resync (on subscribe/reconnect + every N
  minutes) repairs missed or out-of-order events so state never silently drifts.
  In Phase 1 the core MAY still poll AWS internally; once `EcsEventSource::subscribe`
  (§3) lands, steady-state ECS polling drops to ~zero and the resync stays only as
  the backstop.
- **User-facing notifications are a downstream consumer**, not a bespoke channel:
  OS/desktop alerts ("tell me when orca goes `Unhealthy`") subscribe to the same
  `EventHub` stream that drives the roster; the §2.3 webhook sink covers the
  no-client-connected case. **Caveat (from §1):** on ECS the `RUNNING`→unhealthy
  flip is *not* event-emitted, so that specific alert rides the reconcile/probe
  path, not the stream — the abstraction must not promise every transition is
  push-observable on every platform.

## 3. Scope (now)

- **ECS adapter first** (per ADR-2 §10 "ECS driver first"): wire
  `EcsEventSource::list` from the existing code; add the `subscribe` (SQS) path.
- **Infra (separate, ops):** EventBridge rule → CloudWatch Logs `/oab/ecs-events`
  for history; EventBridge rule → SQS for live; `logs:FilterLogEvents` +
  `sqs:ReceiveMessage` on the `oab-mcp` task role.
- **k8s adapter** — trait shape proven against ECS first, then implemented with
  an informer. Deferred, not designed out.
- **Sinks** — MCP push + webhook. The webhook path may ship first (simplest,
  covers offline).

Non-goals here: the ADR-4 **controller / reconcile loop** (this ADR is the event
substrate it would consume); per-caller authz (ADR-3); durable long-term event
retention policy (archive TTL is an ops decision).

## 4. Consequences

- ✅ One contract over runtimes: `deploy_events` and the push emitter never learn
  whether the source is EventBridge or an informer.
- ✅ Reuses PR #16 wholesale — `EcsEvent`/`fetch_ecs_events` become
  `EcsEventSource::list` with a rename to `AgentEvent`.
- ✅ Fixes the ECS blind spot *by construction*: k8s's `EventSource` surfaces
  probe-failure events ECS cannot, without changing consumers.
- ⚠️ **Push requires a live subscriber.** `oab-mcp` is a per-session stdio
  process; SQS/watch backlog **buffers** while nothing is connected and drains on
  next connect — so events are not *lost*, but MCP push is not *offline alerting*.
  Offline coverage is the webhook sink only.
- ⚠️ **Competing consumers.** Orca's `oab-mcp` and a desktop sidecar's `oab-mcp`
  are separate processes; both long-polling one SQS queue would steal each
  other's messages. Fan-out (SNS → per-consumer queue) or a single designated
  subscriber is required for multi-client live push.
- ⚠️ A background subscriber task changes `oab-mcp` from pure request/response to
  holding a live stream + supervision (restart on drop) — new failure surface.

## 5. Open questions

- **Does the MCP client wake the agent?** A server-initiated
  `notifications/resources/updated` reaching the Claude Code / desktop client is
  not guaranteed to *interrupt* a turn vs. merely update state. Whether "event →
  agent acts now" needs harness support is unverified and gates the push sink's
  value for agents (the desktop UI sink is unaffected).
- **Naming.** The broker already has an `openab-mcp` `mcp::facade` (capability
  tools). This event facade must not collide — proposed `EventHub` / `EventSource`.
- **Retention.** History depth (CloudWatch Logs TTL; k8s Events ~1h etcd TTL →
  needs external archiving for parity).
- **Where the hub lives** — `studio-cp` (the existing platform-mapping seam) vs a
  new crate, once a second (k8s) source exists.
