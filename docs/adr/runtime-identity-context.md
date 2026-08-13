# ADR: Runtime Identity & Context panel — surface the effective principal per Instance

- **Status:** Proposed (stub)
- **Date:** 2026-08-13
- **Author:** Orca (ecs-claude)
- **Builds on:** [deployment-control-plane](./deployment-control-plane.md) (ADR-2, read model), [desktop-core-sidecar](./desktop-core-sidecar.md) (credentials resolve in the sidecar from the standard chain)
- **Related:** desktop console (ADR-3), fleet-store (#18)

> **Y-statement.** In the context of Studio managing multiple Fleets whose
> Instances may run under different credentials, accounts/projects and runtimes,
> facing the problem that the *effective* identity a driver resolves is invisible
> to the operator — it took a manual `sts get-caller-identity` to discover a
> deploy call had silently fallen back to a static personal profile instead of
> the intended task role — we decided to add a **read-only Runtime Context
> projection to the Status model plus a Studio panel that renders it per
> Instance/Fleet**, so "who am I actually acting as, and against what
> account/context?" is answerable at a glance, **accepting that this is
> observability only — it introduces no per-caller authz** (still deferred per
> ADR-2).

---

## 1. Context & Problem

Motivating incident (2026-08-13): `deploy_list` returned `AccessDenied` on
`ecs:ListServices`. The root cause was **not** a missing permission — the SDK
credential chain resolved a static `[default]` profile (an IAM *user* in a
different account and region) **before** the container-credentials (task-role)
provider, so the call executed as the wrong principal entirely. Nothing in the
tooling surfaced the effective identity; it took a manual
`aws sts get-caller-identity` to see it.

Studio's north star is managing **multiple Fleets** across runtimes, **different
AWS roles / accounts / projects**, and later **different k8s contexts**. As that
fans out, "which credential/context is this Instance (or my current deploy call)
actually bound to?" becomes a first-order operability question. Today it is
implicit and easy to get silently wrong.

Per ADR-2 credential resolution is deferred to "the AWS-credential ceiling", and
(ADR-5) resolves lazily inside the sidecar from the standard chain. This ADR does
**not** change that model — it makes the *result* of that resolution observable.

## 2. Decision

Add a generic, read-only **`RuntimeContext`** projection to the read model, and a
Studio **Identity & Context panel** that renders it.

`RuntimeContext` (observed, per Instance; also foldable per Fleet) — generic
fields; vendor specifics stay inside the `RuntimeDriver`:

| field | meaning | AWS driver | k8s driver (later) |
|---|---|---|---|
| `principal` | the effective acting identity | STS caller ARN (role vs user) | context user / ServiceAccount |
| `scope` | the account/project boundary | account id | cluster / namespace |
| `location` | region / zone | region | context region |
| `source` | where the credential came from | container-creds (task role) / named profile / static keys / env | kubeconfig context / in-cluster SA |
| `verified_at` | when it was last resolved | STS call ts | `auth whoami` ts |

- **Read-only.** `RuntimeContext` is Status, never Spec — it is *observed*, like
  `phase` (ADR-2 §4). It is emitted by the driver, the only place the vendor
  calls (STS / `kubectl auth whoami`) live.
- **Expected-vs-actual flag.** When an intended identity binding is declared
  (e.g. an expected role/principal for the Deployment), the panel compares it to
  the resolved `principal` and raises a **non-blocking `IdentityMismatch`**
  Condition — a warning, not a gate. Today's incident would have tripped it on
  sight. This stays on the read side: **no enforcement**, consistent with ADR-2's
  deferred authz.
- **Panel.** A Studio surface (desktop skin, and any front-end over the same MCP
  read model) that lists, per Fleet → Instance, the `RuntimeContext` and any
  `IdentityMismatch`. The Fleet-level view folds distinct contexts, so "this
  Fleet spans 2 accounts" is visible at a glance.

## 3. Scope (now / later)

- **Now:** the AWS driver populates `RuntimeContext` (STS caller identity +
  resolved `source`); the read model carries it; the desktop panel renders it per
  Instance/Fleet; and the `IdentityMismatch` warning fires when an expected
  principal is declared.
- **Later:** k8s driver mapping (context / SA / namespace); richer `source`
  provenance; and surfacing the **deploy-call's own** resolved identity (the
  caller's principal, not just the managed Instances') so a mis-resolved
  CLI/sidecar is caught *before* it acts — the exact preflight that would have
  pre-empted the motivating incident.

## 4. Consequences

- ✅ "Who am I acting as, against what account?" is answerable at a glance;
  silent credential fallback (today's bug) becomes visible, ideally flagged.
- ✅ Stays within ADR-2's generic vocabulary and deferred-authz stance — pure
  observability, with vendor terms scoped to the driver.
- ⚠️ The driver must make a live identity call (STS / `whoami`); cache it with a
  `verified_at`, and treat failure as an `Unknown` context rather than a hard
  error.
- ⚠️ `RuntimeContext` surfaces account ids / ARNs in the UI — non-secret, but the
  panel must **never** render access keys or tokens.

## 5. Open questions

- Does "expected identity" belong in Spec (a new `identity.expectedPrincipal`),
  or is it derived from Fleet/project config? (Interacts with fleet-store #18.)
- Should the caller's **own** resolved identity be a standing panel element
  (deploy-call preflight), or only the managed Instances'?
- Granularity of `source` provenance without leaking secrets.
- Per-caller authz stays deferred (ADR-2) — this ADR deliberately does not touch
  it.
