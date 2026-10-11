# ADR: Fleet Store & Control-Plane State Ownership

- **Status:** Proposed (draft)
- **Date:** 2026-08-12
- **Author:** Orca (ecs-claude), drafting for @brettchien
- **Reviewers:** TBD
- **Builds on:** [agent-lifecycle](./agent-lifecycle.md) (ADR-1), [deployment-control-plane](./deployment-control-plane.md) (ADR-2)
- **Enables:** the continuous reconcile loop (ADR-4), per-caller authz (ADR-3), and a future *ephemeral instances + reaper* ADR

> **Y-statement.** In the context of a control plane that must persist desired
> state and canonical runtime metadata **no runtime platform can hold** — fleet
> membership, owner, TTL, lease, fencing epoch, the `identity_verified` latch —
> and must do so **across many stateless front-ends** (MCP-per-client, CLI) and
> platform reclamation, we decided to introduce a **backend-agnostic
> `FleetStore`** owned by a **single long-running control-plane controller** as
> the **sole writer**, with a **file / S3 object** as the reference backend and
> **optimistic epoch-CAS** for correctness, to get one durable, portable source
> of truth, **accepting** that we promote the CP from a stateless projection
> over ECS into a stateful controller (a new deployable + a single-writer
> constraint), and **deferring** the reconcile algorithm (ADR-4) and per-caller
> authz (ADR-3).

---

## 1. Context & Problem

ADR-2 gave a **stateless** read/write model projected *live* from ECS. That is
insufficient for what we are building next:

- The `identity_verified` latch is only **approximated** from ECS `lastStatus`
  (studio-cp: *"the real latch needs CP-persisted history"*). Today every
  chat-native agent with no ECS health check therefore reads as a **false
  `Unhealthy`**.
- **owner / TTL / lease / fencing epoch have no home** in ECS.
- **Ephemeral / stdio workers are not ECS objects at all.**
- **Fleet membership is authoring-time only** — `OABFleet.expand()` fans out to
  N `OABService`s and the fleet grouping is then *lost* (no back-reference, not
  queryable, not mutable).
- There is **no single serialization point**: `oab-mcp` is stdio-**per-client**
  (N processes), the `oabctl` CLI writes AWS **directly**, and ECS writes the
  *actual*. Nothing owns a canonical desired state.

To support persistent fleet membership, a real latch, leases/epochs, and (later)
ephemeral instances with reaping, the CP needs **its own durable state** and a
**single writer**.

## 2. Decision Drivers

- **Persist what platforms can't hold** — canonical metadata above any runtime.
- **Survive stateless front-ends + reclamation** — state outlives any task/process.
- **Backend-agnostic** — mirror `RuntimeDriver`; the store must not re-lock the
  stack to AWS.
- **Safe concurrency without a heavy DB** when the content is cold and rarely
  changes.
- **One unambiguous writer** for mutable state.

## 3. Decision

### 3.1 Introduce `FleetStore` — the CP-owned source of truth
`FleetStore` holds **DESIRED + canonical runtime metadata**. Source-of-truth is
split (the etcd model):

```
FleetStore = DESIRED + canonical metadata (membership / policy / owner / ttl /
             lease / fencing epoch / identity_verified latch / phase cache)
Driver     = ACTUAL (does the task/pod/process exist right now)
reconcile  = compare store.desired ↔ driver.observed → act   (ADR-4)
```

### 3.2 Storage is a driver too — the `FleetStore` port
The CP talks only to a `FleetStore` trait; storage/vendor terms live **only** in
impls. Contract = **lowest-common-denominator semantics**; native features are
optimizations, never in the contract.

Port operations:
- **membership** — put/get/del fleet; add/remove instance↔fleet; list instances in fleet
- **desired spec** — put/get fleet spec (with `generation`); list fleets
- **instance registry** — register / heartbeat-update / get / list by (fleet|owner) / delete
- **epoch-CAS** — conditional write "*apply only if epoch ≥ stored*" (the fencing primitive)
- **expiry query** — `list_expired(now)` for the reaper to poll

**One hard requirement:** a **single-item linearizable conditional write** (for
epoch-CAS). Backends that cannot provide it (plain eventually-consistent KV) do
not qualify as the runtime registry.

Impls (all behind the port; choosing one does not lock in):
- **file / S3 object — reference.** S3 is strongly consistent and supports
  conditional `PUT` (`If-Match` on ETag) → whole-object optimistic CAS. Reuses
  the existing control-plane bucket.
- **private git repo** (NOT a secret gist — secret gists are unlisted, not
  access-controlled). git push non-fast-forward = CAS; free version history.
  Good for local / stdio / OSS.
- **sqlite / in-memory** — local, stdio driver, tests.
- **DynamoDB / Postgres** — scale / HA (conditional write = CAS; native TTL is a
  *backstop* only). The reaper always works by polling `list_expired`, so no
  backend's native TTL is on the contract.

### 3.3 Writer model — one controller, sole writer
Exactly **one long-running controller** owns the store and is its **only
writer**. `studio-cp` is promoted from observe-only to this controller. `oab-mcp`
and the `oabctl` CLI become **clients that submit intents**; they no longer write
the store or AWS directly.

- **Regulation:** *one active controller per store.*
- **Correctness backstop:** **epoch-CAS**. Single-writer is for *simplicity*;
  epoch-CAS defeats the *transient double-writer* (controller failover, Spot
  reclamation leaving a zombie, stale process) — this is ADR-1 principle 2's
  monotonic fencing epoch realized at the store layer. **Never rely on "there is
  only one controller" for correctness.**

### 3.4 Canonical schema

Per the **unified fleet model** (§3.6) the store has **two tiers, not three**: the
desired spec ADR-2 carried on a separate `Deployment` is **folded into the
`Fleet`** (a singleton agent is a *size-1 fleet*), and the `Instance` is the unit.

```
Fleet     key=(cluster, namespace, name); provider;
          spec{ configRef, image/template, credentialSource,
                replicas(desired), admission(accepting_work, ADR-1),
                identityPolicy: pinned | ephemeral };   # spec folded in from ADR-2's Deployment
          members=[instance refs]; generation           # persistent membership (fixes authoring-only gap)
Instance  handle(canonical id); nativeRef(opaque, driver-owned); fleet ref;
          identity_verified(latch, persisted); lease{tokenId, expiry}; epoch(monotonic);
          phaseCache(AgentState)+observedAt; credentialHandle(ref, not the secret);
          owner ref; ttl/idle deadline                  # slots; filled by the ephemeral ADR
```

`identityPolicy` is the real axis (not a kind): **pinned** (named / fixed token /
long-lived — today's Orca, Mira) vs **ephemeral** (minted seat/API + TTL + owner,
reapable). A singleton is just `replicas=1, identityPolicy=pinned, ttl=∞` — no
special-case code path.

### 3.5 Regulation carried from design
A **Fleet is bound to a single cluster** (= single driver target / single
provider). Fleet key is `(cluster, namespace, name)`; cross-cluster is modeled as
N single-cluster fleets + a higher placement layer (out of scope).

**Namespace is the implicit fleet.** With no explicitly configured fleet, the
**namespace itself is the fleet** — so there is *always* a fleet even at zero
config. Hierarchy: `namespace ⊇ [explicit fleet │ implicit namespace-fleet] ⊇
instance`. Policy cascades **namespace default → fleet override → instance
override**, so `identityPolicy` / `ttl` resolve at the instance.

> **Reaper safety (hard rule).** An implicit namespace-fleet holds today's
> **pinned, long-lived** agents (e.g. `prod` carries orca + mira). Therefore the
> namespace-fleet's **default policy is `no-reap` / `pinned`**, and the reaper
> decides what to kill **strictly per-instance `identityPolicy`** — it **never**
> does a namespace-wide reap, or it would take down resident agents.
> Per-instance policy is exactly what makes mixing pinned + ephemeral in one
> namespace safe. (The reap triggers themselves are ADR-4 / the ephemeral ADR;
> this ADR only fixes *where* owner/ttl/policy live and the safety invariant.)

### 3.6 Alignment with the unified fleet model
This ADR is written against the **unified fleet decision** (2026-08-11): the
resource model is **always fleet + instance**; there is **one kind — `Fleet`** (the
`OABService` vs `OABFleet` two-KIND split is retired), a singleton is a size-1
fleet, and identity/credential live at the **instance** layer while the fleet
holds template + policy. Consequences for this store:

- **Amends ADR-2's vocabulary.** ADR-2 modelled `Agent → Deployment(→N Instances);
  Fleet = set of Deployments`. Here the desired spec is folded onto the `Fleet`
  (§3.4); "Deployment" survives only as ADR-2 prose, not as a store tier.
- **One reconcile loop** maintains desired *and* reaps the dead; a singleton just
  never triggers reap (`ttl=∞`, `identityPolicy=pinned`). No separate
  self-heal-vs-reap paths.
- The read-model is uniformly *"list a fleet's instances `{handle, owner,
  state(6), ttl}`"* — Orca today = `fleet=orca, instances=[1]`.

## 4. Principles

1. **Platforms hold actual; the store holds desired + canonical truth.** Never
   persist a vendor identifier (task ARN, pod UID) as canonical — map it to a
   handle; the `nativeRef` is opaque and owned by the driver.
2. **Storage is a driver.** Contract is LCD; native features (TTL, streams) are
   optimizations behind the port. The reaper polls `list_expired`.
3. **Single writer for simplicity, fencing epoch for correctness.**
4. **Keep the store cold.** Hot per-instance signals (heartbeat/phase) are
   **derived by the controller polling drivers**, not written by instances; the
   file stays small and rarely rewritten. This is what makes the file/S3
   reference impl viable.
5. **Two stores, two write models.** This registry is mutable →
   single-writer/CAS. The shared *context / knowledge* layer (institutional
   memory) is append-mostly → multi-writer via **sharded per-contribution
   objects**; that is a **separate concern**, not this store.

## 5. Non-goals (deferred)

- The **reconcile algorithm** and **reaper triggers** (owner_dead / ttl / idle) → **ADR-4**.
- **Per-caller authz** (who may apply/scale/spawn/kill) → **ADR-3**, attached at the controller.
- **Ephemeral identity policy, `replicas > 1`, per-instance minted credentials at scale** → future *ephemeral + reaper* ADR (the schema reserves the slots).
- The **shared context Workspace** (append store) → separate ADR.

## 6. Consequences

**Enables**
- Persistent, queryable fleet membership.
- The real `identity_verified` latch → **fixes the false `Unhealthy`**.
- Lease + fencing epoch (ADR-1 principle 2) via epoch-CAS.
- A home for owner / ttl / idle → the reaper becomes possible (ADR-4 + ephemeral ADR).
- Canonical handles in the read-model → **fixes the ARN leak** (ADR-2 §7).

**Costs / trade-offs**
- A **new deployable**: a single long-running controller (backup, availability,
  the one-active-controller constraint).
- Front-ends must be **refactored to submit intents** instead of writing directly
  (the `oabctl` CLI loses its direct-to-AWS write path).
- The file/S3 impl has a **whole-object read-modify-write ceiling** → migrate to
  a per-item backend (sqlite/Dynamo) at high write rate or thousands of
  instances. Mitigated by principle 4 (keep it cold).
