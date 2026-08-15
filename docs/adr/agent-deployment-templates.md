# ADR: Agent deployment — template/skills bundles → provider-tagged provisioning

- **Status:** Proposed (draft)
- **Date:** 2026-08-15
- **Author:** Orca (`ecs-claude`)
- **Builds on:** [Deployment control plane (ADR-2)](./deployment-control-plane.md); [Runtime identity/context (ADR-19)](./runtime-identity-context.md); [Desktop core sidecar](./desktop-core-sidecar.md)
- **Relates to:** Fleet Store & Control-Plane State Ownership (#18, the desired-state home); the provider-tagged + hermetic `oab-mcp` target (#60, same driver axis)
- **Defers:** the *remote* file editor over `/acp` (agent-consoles ADR #49 Part D — editing a **running** agent's files) — this ADR is about **authoring locally + provisioning**, a different surface.

---

> In the context of Studio needing to stand up openab agents — not just observe them — facing the facts that (a) an agent is a **prebuilt image + a bundle of plain files** (`config.toml`, persona, skills), (b) **compilation is CI/CD's job**, never the deploy path, and (c) the runtime is **not always ECS** (k8s is on the roadmap), we decided that Studio holds a **template/skills library**, **composes** a per-agent file bundle from a template + overlays, and **provisions** it via a **provider-tagged driver** — selecting a prebuilt image tag and pushing the bundle onto that runtime's file carrier (S3 state for ECS, ConfigMap/volume for k8s) — accepting a new authoring surface + a K8s driver to build, and deferring the running-agent remote editor (#49 Part D).

## 1. Context & Problem

Studio today can **observe** the fleet (roster, 6-state) and **scale** existing services (ADR-2 write model), but it cannot **create** an agent from nothing. To stand up an agent an operator still hand-assembles files and runs `oabctl` out-of-band. Three facts shape the fix:

- **An agent = a prebuilt image + a file bundle.** The image (`ghcr.io/openabdev/openab:<tag>`) carries the compiled `openab` binary + runtime; everything that makes it *this* agent is **plain files** — `config.toml` (runtime env/command), persona (`CLAUDE.md`, `agent_profiling/…`), and skills (`.claude/skills/…`). Nothing in the agent's identity needs compiling.
- **Compilation is CI/CD, not deploy.** The image is built by CI/CD (GitHub Actions → registry). Studio **consumes an image tag**; it never builds. A custom binary is a new CI/CD image tag first, then a deploy of that tag.
- **The runtime is not always ECS.** ECS provisioning exists (`oabctl`, `EcsDriver`); ADR-2 §10 reserves k8s/compose for later. Deployment must be **provider-tagged** from the start (mirrors the `RuntimeDriver` and the #60 hermetic-target work) so k8s is a new driver, not a rewrite.

## 2. Decision

### 2.1 The deploy unit = image tag + file bundle
A deployment references **one prebuilt image tag** and carries **one file bundle** (config + persona + skills). Deploy is provisioning, **never a build**: `deploy = pick image tag + push bundle to the runtime's file carrier + apply the workload`. This keeps the deploy path fast and decoupled from CI/CD.

### 2.2 Template/skills library + overlay (Studio-side authoring)
Studio holds a **library** the operator edits in-app (same CodeMirror/editor pattern as `fleets.toml`/`remote.toml` today):

- **Template** — a reusable base bundle: a `config.toml`, base persona, base skills, and a **default image tag**. A "golden bundle."
- **Overlay** — per-agent specifics layered on a template: a specific persona/prompt file, extra `.claude/skills/*`, config overrides, an image-tag override.
- **Compose** — `template ⊕ overlay → the concrete agent's file bundle`. Deterministic; last-writer-wins per file path. This is "Helm chart + values," for agents.

Skills live in a **shared library** (author once, attach to N agents by reference) *and* can be overlaid per agent.

### 2.3 Provision — provider-tagged driver, per-provider file carrier
One `RuntimeDriver`-shaped seam owns **both** halves of a deploy — starting the workload **and** landing the composed bundle on that runtime's file carrier:

| | **ECS** (`EcsDriver`, exists) | **k8s** (`K8sDriver`, new) |
|---|---|---|
| Image | task-def image = the chosen tag | pod image = the chosen tag |
| File carrier | **S3 state prefix** (`pre_seed` restores it into `~`) | **ConfigMap / Secret / volume** (or init-container that pulls the bundle) |
| Apply | `oabctl` apply/create/scale (ADR-2 sugar) | k8s apply (Deployment/StatefulSet) |
| Secrets | Secrets Manager ref | k8s Secret ref |

The driver is the **only** layer that knows S3-vs-ConfigMap; the compose step above it is provider-neutral (it just produces a `{path → bytes}` bundle + an image tag + a runtime spec). Credential/region binding for the driver reuses the **hermetic, provider-tagged** approach from #60.

### 2.4 Desired state
The composed spec (image tag + bundle digest + runtime spec + identity policy) is the agent's **desired state**. It records into the Fleet Store (#18) when that lands; until then Studio provisions directly (`Studio → oabctl/driver apply`) and the store is a follow-on. Either way the **compose + provider-driver** boundary is unchanged.

## 3. Scope & slices
1. **Template/skills library + compose** (Studio-local, no deploy yet): author templates/overlays, preview the composed bundle.
2. **ECS provision**: push the composed bundle to the agent's S3 state prefix + `oabctl` create/apply against a chosen image tag. (Reuses existing ECS provisioner.)
3. **K8s driver**: the second provider — ConfigMap/volume carrier + k8s apply, behind the same seam.
4. **Fleet Store wiring** (#18): record desired state so the CP reconciles, instead of one-shot apply.

## 4. Non-goals
- **Building/compiling anything.** Images come from CI/CD; Studio references tags.
- **The remote file editor over `/acp`** (#49 Part D) — editing a *running* agent's files is a separate surface, deferred.
- Multi-agent orchestration UI, cross-agent relay (per agent-consoles §non-goals).

## 5. Consequences
- ✅ Studio can stand up an agent end-to-end (compose → provision) without hand-assembly or out-of-band `oabctl`.
- ✅ One provider-tagged seam covers ECS now + k8s next; the compose layer is provider-neutral.
- ✅ Deploy stays fast/decoupled — no build in the path.
- ⚠️ **Security surface**: the bundle carries persona + skills = arbitrary agent behaviour, and config may reference secrets. Writing a bundle into a runtime is a powerful, audited action; secrets stay **references** (Secrets Manager / k8s Secret), never inlined into the bundle.
- ⚠️ **K8s driver is real new weight** (auth = kubeconfig/context, not `AWS_*`; the #60 hermetic env must be extended per the k8s/EKS notes).
- ⚠️ Template drift: a template change doesn't retro-apply to already-deployed agents; redeploy is explicit (no silent fan-out).

## 6. Open questions
1. **Bundle transport to S3** — does Studio write the S3 state prefix directly (needs creds/driver), or hand the bundle to a control-plane endpoint that does? Leaning: through the driver, so the same credential/authz path as other writes.
2. **Template storage** — where does the library live (Studio-local dir, a repo, an S3 prefix)? Versioned?
3. **Skills references** — attach-by-reference (shared library, resolved at compose) vs copy-in. Leaning: reference + resolve at compose, so a skill update is one place.
4. **Bundle vs image boundary** — anything that *must* be in the image (native deps) vs stays a file. Default: everything that can be a file, is.
