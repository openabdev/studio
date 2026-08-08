# ADR: Deployment Control Plane — read/write model + MCP

- **Status:** Proposed
- **Date:** 2026-08-08
- **Author:** @brettchien
- **Reviewers:**
- **Tracking issues:** builds on [agent-lifecycle](./agent-lifecycle.md) (ADR-1)

> **Y-statement.** In the context of operating agents across runtimes on top of
> the vendored `oabctl` engine, facing the need for agents and future front-ends
> to observe and control deployments without vendor lock-in, we decided a
> **generic read model (Spec/Status/phase) + write model (`apply` primitive) +
> an MCP adapter (`deploy_*`)**, to make a Studio control plane every front-end
> (CLI / TUI / GUI / agent) shares, **accepting that per-caller authorization is
> deferred** (interim: the AWS credential ceiling only).

---

## 1. Context & Problem

ADR-1 defined the canonical 6-state agent lifecycle. We now need the control
plane that (a) **observes** deployments and reports their state, and (b) lets a
caller apply **basic control** (create / scale / stop) — reachable by an agent
as a first-class MCP citizen, and later by a TUI/GUI.

We build on the vendored `oabctl` (the ECS provisioner engine, kept close to
upstream). The vocabulary must be **generic** so swapping the underlying engine
or cloud is a driver change, not a rewrite.

## 2. Decision Drivers

- **No vendor lock-in** — agent-facing vocabulary is generic; vendor terms live
  only in the driver.
- **One substrate, many front-ends** — CLI / TUI / GUI / MCP all consume the
  same models; none re-implements observation or control.
- **Declarative & idempotent** — one write primitive; imperative verbs are sugar.
- **Safe by construction** — reads are free; writes are explicit, support
  dry-run, and fail closed at the credential boundary.

## 3. Glossary (generic vocabulary)

Vendor-specific terms (ECS/task/ARN/S3/Fargate…) appear **only** inside a
`RuntimeDriver`. Everything above speaks these:

| Term | Meaning | (driver-level equivalent) |
|---|---|---|
| **Agent** | The managed logical entity | — |
| **Instance** | One running copy of an agent | ECS task / k8s pod / compose container |
| **Deployment** | An Agent's declared desired unit → N Instances | ECS service / k8s Deployment / compose service |
| **Fleet** | A set of Deployments | — |
| **Spec** | Desired: `identity + version + scale + admission + runtime + configRef` | `.spec` |
| **admission** | Spec field (`accepting_work`): may this Instance take new work | (readiness gate) |
| **Instance.Status** | Observed per Instance: `phase + conditions + …` | Pod `.status` |
| **phase** | Field on an **Instance**'s Status; value is an `AgentState` — **Instance-level only** | Pod `.status.phase` |
| **Deployment.Status** | Observed per Deployment: replica counters `desired/current/ready/available` + Conditions — **not** an `AgentState` | Deployment `.status` |
| **AgentState** | The 6 lifecycle states (ADR-1) | — |
| **Discriminators** | `desiredStatus · accepting_work · health · identity_verified` | ADR-1 |
| **RuntimeDriver** | Per-runtime translation layer (the only place vendor terms live) | Controller/Operator |
| **config** | The agent's app config (`config.toml`); a field within Spec | — |

## 4. Read model

`Instance = Spec (desired) + Status (observed)`; **state is observed, never part
of Spec** (ADR-1). A driver observes native signals and projects them onto:

- **Instance-level** — `Instance.status.phase` is one `AgentState` (ADR-1's
  `classify()`), plus `conditions[]` (ready, superseded, …).
- **Deployment-level** — `Deployment.status` is **replica counters**
  (`desired / current / ready / available`) + Deployment `conditions[]` (e.g.
  `Available`, `Progressing`). **It is NOT an `AgentState`**: a Deployment with N
  Instances (say one `Running`, one `Unhealthy`) has no single lifecycle value —
  it has *counts*. (Mirrors k8s exactly: a Pod has a `phase`; a Deployment has
  replica counts + conditions, never a phase.)

**Observation types are generic** (`Deployment`, `Instance`, `Status`, `phase`);
ECS strings (`ACTIVE`/`DRAINING`/task ARN) stay inside the ECS driver. The
current `studio-cp::ServiceStatus` (ECS-flavoured) is replaced by generic types
here (see §7).

### 6-state ⇄ k8s (mapping, with traps)

k8s has no single lifecycle enum; it is `phase + conditions + probes +
deletionTimestamp`. Traps to document so k8s intuition doesn't misread us:

- **`Running` is stricter than k8s** — ours = k8s `phase=Running ∧ Ready=True`.
- **`Paused` has no per-Pod k8s analog**, and is **not** k8s `Deployment.spec.paused` (that is rollout-pause).
- **`Stopping`** = k8s "Terminating" (`deletionTimestamp≠null`), which is not a `.status.phase` value.
- **`Stopped`** = k8s `Succeeded`/`Failed`; we keep one state + a death `cause`.
- **`Unhealthy`** is a first-class state; k8s expresses it via conditions/probes + `Unknown`.
- **`identity_verified`** (latch) ≈ k8s `startupProbe` first success.

## 5. Write model

One idempotent primitive: **`apply(Spec)`** — reconcile observed toward desired.
Named intents are **sugar over apply** (differ only in delta + guardrail):

| intent | reduces to | note |
|---|---|---|
| create | `apply(new Spec)` | first-time; provisions identity |
| scale | `apply(Spec with new replicas)` | count only; no new identity |
| **cordon** | `apply(Spec with admission=off)` → Instance `Paused` | stop taking new work, **stay alive** (drain / isolate); reversible |
| **resume** | `apply(Spec with admission=on)` → Instance `Running` | uncordon |
| stop / delete | `apply(absence)` / replicas→0 | destructive |

- **create is not a separate operation** — `apply` covers it.
- **cordon / resume** are the write path to ADR-1's `Paused` state (admission =
  `accepting_work`, a Spec field). Without them the write model could not reach a
  state ADR-1 defines. `cordon` is *not* `stop`: it keeps the Instance alive and
  resumable.
- **dry-run / diff**: every write supports a preview that returns *what would
  change* without mutating — a safety valve, and important for agents (look
  before leap). See §6 for `dry_run` as a first-class tool parameter.

## 6. MCP adapter

The control plane is exposed as an MCP server so an agent operates it
first-class. Every front-end (CLI/TUI/GUI) is a downstream client of the same
models; the MCP server is one adapter.

- **Server name:** `oabctl` (this control plane serves only the Studio/openab
  universe).
- **Tools** (generic verbs; server namespace disambiguates — no `oabctl_` prefix):

| tool | kind | maps to |
|---|---|---|
| `deploy_list` | read | list Deployments + `Deployment.status` counters |
| `deploy_get` | read | one Deployment's Spec + status (+ each Instance's phase) |
| `deploy_apply` | write | the declarative primitive |
| `deploy_scale` | write | change replicas |
| `deploy_cordon` | write | admission=off → `Paused` (stay alive) |
| `deploy_resume` | write | admission=on → `Running` |
| `deploy_stop` | write | destructive |

**`dry_run: bool` is a first-class, required parameter on every write tool** —
in the pre-authz interim it is the structured safety preview an agent relies on.

Excluded from MCP: `exec`/`cp`/`sync` (shell into containers — blast radius),
`bootstrap` (infra, one-time), `schedule` (automation). See Non-goals.

### Authorization — DEFERRED (read carefully)

Per-caller authorization is **out of scope for ADR-2** and deferred to ADR-3.

- **Interim posture:** the only gate on writes is the **AWS credential the
  `oabctl` process runs as** (its task role / credential file). This is a
  **coarse ceiling** that *cannot distinguish callers* — every caller reaching
  the MCP server shares the credential's full power.
- **Consequence / known risk:** until ADR-3, any caller wired to the write tools
  can do anything the credential allows. Therefore, interim operating rule:
  **least-privilege the `oabctl` role, and wire the write tools only to trusted
  callers.**
- ADR-3 adds the **per-caller / per-verb / per-scope** layer (default-deny
  allowlist, destructive-confirm, namespace scoping, audit; caller identity
  CP-verified per ADR-1). Two identities: the process (credential) vs the calling
  agent (ADR-3).

## 7. Exposure seam (oabctl → Studio)

oabctl exposes status as a **library API returning data** (not CLI table output):
`oabctl::service_status(...) -> Vec<ServiceStatus>` (added in PR #2). Studio
consumes it in `studio-cp` and maps it onto the generic read model. Vendored
oabctl stays additive/clean so changes are upstream-contributable.

**Granularity requirement.** `DescribeServices` gives only *service-level* data
(running/desired counts, service status) — it cannot yield a Task's `lastStatus`
(`PROVISIONING`/`ACTIVATING`/`RUNNING`/`STOPPED`), container `healthStatus`, or
`stopCode`. Those per-Task facts are exactly what ADR-1's four discriminators
(`identity_verified`, `health`, …) need, so a service-level `ServiceStatus`
alone **cannot produce the 6-state**. Therefore the seam must expose **per-Task
`InstanceStatus`**: the driver aggregates

- `DescribeServices` → **Deployment**-level counters + rollout state, and
- `DescribeTasks` → each **Instance**'s 4 discriminators → `phase` (`AgentState`).

`ServiceStatus` thus wraps a `Vec<InstanceStatus>`; the ECS specifics stay inside
the driver.

## 8. Alternatives Considered

- **MCP tools named `oabctl_*`** — rejected: re-introduces vendor lock-in in the
  agent-facing vocabulary and duplicates the server namespace.
- **Pure declarative MCP (`apply` + `get` only)** — rejected as the surface:
  agents get clearer, individually-guardable intents (`scale`/`stop`); they
  still compile to `apply`.
- **Read-only ADR-2 (defer all writes)** — rejected: we enable basic write now,
  and defer *authorization* instead (§6).

## 9. Consequences

- Studio's read model + every front-end speak the generic glossary; ECS terms
  are confined to the driver.
- Writes are enabled but only credential-gated until ADR-3 — a documented,
  time-boxed risk, not a silent hole.
- `studio-cp`'s ECS-flavoured types are replaced by generic ones.

## 10. Non-goals (deferred)

- **Per-caller authorization / guardrails** → ADR-3.
- **Controller + reconcile loop + State Store (S3) + `observedGeneration`** →
  ADR-4 (until then, front-ends live-observe on demand; no durable status store).
- **k8s / compose drivers** → later (ECS driver first).
- **`exec`/`cp`/`sync`/`bootstrap`/`schedule` via MCP** → out of scope.

## 11. More Information

Glossary here graduates to a repo-wide `GLOSSARY.md`. Format follows MADR +
Nygard + Y-statement, per [ADR-1](./agent-lifecycle.md) and
[`docs/review-runbook.md`](../review-runbook.md).
