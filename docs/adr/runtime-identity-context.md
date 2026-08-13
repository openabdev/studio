# ADR: Per-Fleet managing identity — binding config, context switch, and verification

- **Status:** Proposed (stub)
- **Date:** 2026-08-13
- **Author:** Orca (ecs-claude)
- **Builds on:** [deployment-control-plane](./deployment-control-plane.md) (ADR-2, read model), [desktop-core-sidecar](./desktop-core-sidecar.md) (credentials resolve in the sidecar from the standard chain)
- **Related:** desktop console (ADR-3), fleet-store (#18)

> **Y-statement.** In the context of Studio managing multiple Fleets that may
> each live under a different AWS account / role / project (and later a different
> k8s context), facing the problem that the *managing* credential is ambient and
> invisible — so a deploy call can silently run under the wrong principal (a
> static `[default]` profile shadowed the intended task role, discovered only via
> a manual `sts get-caller-identity`) — we decided to make the per-Fleet managing
> identity **explicit, switchable, and verified**: a **FleetBinding config**
> (Fleet → managing context), an **active context switch** so selecting a Fleet
> binds subsequent calls to that Fleet's credential, and a **read-only
> RuntimeContext panel + IdentityMismatch flag** that confirms the effective
> identity matches the declared binding — **accepting that this is operator-side
> credential *selection*, not per-caller authz** (authz still deferred per ADR-2).

---

## 1. Context & Problem

Motivating incident (2026-08-13): `deploy_list` returned `AccessDenied` on
`ecs:ListServices`. The root cause was **not** a missing permission — the SDK
credential chain resolved a static `[default]` profile (an IAM *user* in a
different account and region) **before** the container-credentials (task-role)
provider, so the call executed as the wrong principal entirely. Nothing declared
which identity *should* manage that target, and nothing surfaced which one
actually did; it took a manual `aws sts get-caller-identity` to see it.

Studio's north star is managing **multiple Fleets** across runtimes and **different
AWS accounts / roles / projects** (later **k8s contexts**). Two gaps follow:

1. **No declared binding.** Nothing says "to manage Fleet X, act as identity Y in
   account Z." The managing credential is whatever the ambient chain resolves —
   easy to get silently wrong, especially when switching between Fleets.
2. **No verification.** Even once resolved, the *effective* identity is invisible,
   so a wrong/misfired binding is undetectable at a glance.

Per ADR-2 credential resolution is deferred to "the AWS-credential ceiling", and
(ADR-5) resolves lazily inside the sidecar from the standard chain. This ADR does
**not** introduce per-caller authz. It makes the operator's *own* managing
identity per Fleet **declarative, actively selected, and observable**.

## 2. Decision

A `declare → switch → observe → reconcile` loop, in three parts.

### 2a. FleetBinding config (declare)

A declarative Studio/control-plane config mapping each managed **Fleet → its
managing context**. Generic fields; vendor specifics resolved by the
`RuntimeDriver`:

| field | meaning | AWS driver | k8s driver (later) |
|---|---|---|---|
| `fleet` | the managed Fleet | — | — |
| `principal_source` | how to obtain the managing credential | named profile / assume-role ARN / task-role | kubeconfig context / SA |
| `scope` | account / project boundary | account id | cluster / namespace |
| `location` | region / zone | region | context region |

This is the operator/CP's binding, **not** the agent's `Spec.identity` (that is
the agent's own logical identity). Where it is persisted — Studio config vs
control-plane state — interacts with fleet-store (#18); see §5.

### 2b. Active context switch (apply)

Selecting / switching to a Fleet in Studio **switches the active managing
context** to that Fleet's binding: the driver/sidecar resolves credentials
**parameterized by the binding** (the chosen profile / assumed role), instead of
falling through to whatever ambient `[default]` the chain finds first. This is
the *proactive* fix — the wrong-account call never happens because the binding is
selected before the call, not discovered after it fails.

### 2c. RuntimeContext panel + IdentityMismatch (observe & reconcile)

A read-only **`RuntimeContext`** Status projection (observed, per Instance;
foldable per Fleet) that the panel renders:

| field | meaning | AWS driver | k8s driver (later) |
|---|---|---|---|
| `principal` | effective acting identity | STS caller ARN (role vs user) | context user / SA |
| `scope` | account / project | account id | cluster / namespace |
| `location` | region / zone | region | context region |
| `source` | where the credential came from | container-creds / named profile / static keys / env | kubeconfig / in-cluster SA |
| `verified_at` | last resolved | STS call ts | `auth whoami` ts |

- **Read-only**, emitted by the driver — the only place vendor calls (STS /
  `kubectl auth whoami`) live (consistent with ADR-2 §4: state is observed).
- **`IdentityMismatch`** — a **non-blocking** Condition raised when the effective
  `principal` ≠ the Fleet's declared binding (2a). It catches a switch that did
  not take, or an ambient fallback. Today's incident trips it on sight. Stays on
  the read side: **no enforcement**, consistent with ADR-2's deferred authz.

## 3. Scope (now / later)

- **Now (AWS):** FleetBinding config with a per-Fleet credential source
  (named profile / assume-role / task-role); switch binds the driver's resolution
  to it; panel renders `RuntimeContext` per Instance/Fleet; `IdentityMismatch`
  fires when effective ≠ declared.
- **Later:** k8s driver mapping (context / SA / namespace); a **deploy-call
  preflight** that resolves and shows the *caller's own* effective identity before
  a write, so a mis-resolved CLI/sidecar is caught before it acts; richer `source`
  provenance.

## 4. Consequences

- ✅ Switching Fleets no longer risks acting under the wrong account — the binding
  is selected up front (proactive), and the panel + mismatch verify it (detective).
- ✅ Stays within ADR-2's generic vocabulary and deferred-authz stance: operator
  credential *selection*, not per-caller *authorization*; vendor terms scoped to
  the driver.
- ⚠️ The sidecar (ADR-5) currently resolves lazily from the **ambient** chain;
  honoring a FleetBinding means Studio must **parameterize** that resolution per
  Fleet (pass the profile / role in), not rely on the default chain order.
- ⚠️ The driver must make a live identity call (STS / `whoami`); cache with a
  `verified_at`, treat failure as `Unknown`, and never render access keys/tokens.

## 5. Open questions

- Where does the FleetBinding live — Studio-local config vs control-plane state
  (fleet-store #18)? Single source of truth either way.
- How does `principal_source` map to a concrete credential — named profile,
  `sts:AssumeRole`, or an expected task-role — and how is that handed to the
  sidecar's lazy resolution (ADR-5)?
- Should the deploy-call preflight (caller's own identity) be standing, or
  on-demand before writes only?
- Per-caller authz remains deferred (ADR-2) — this ADR deliberately does not
  touch it; the FleetBinding is *selection*, not *authorization*.
