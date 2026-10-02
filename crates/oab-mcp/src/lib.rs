//! Studio control-plane MCP handler (ADR-2).
//!
//! A minimal, hand-rolled `rmcp` [`ServerHandler`] (matching the openab-mcp
//! native-adapter style) that exposes the `studio-cp` read/write model as MCP
//! tools:
//!
//! - read: `deploy_list`, `deploy_get`, `get_agent_states`, `deploy_events`,
//!   `k8s_logs`, `runtime_context`, `fleet_config`
//! - write: `deploy_apply`, `deploy_provision`, `deploy_scale`, `deploy_delete`,
//!   `fleet_config_write`
//!
//! **Transport-agnostic on purpose.** The handler is a *library* so the same
//! tool logic serves two front doors: the `oab-mcp` binary drives it over
//! **stdio** (the headless standalone case), and a reverse-MCP-over-ACP tunnel
//! (Studio's ACP-WS client) drives it **in-process** via [`OabMcp::dispatch`] /
//! [`OabMcp::tools`] / [`OabMcp::info`] — no stdio, no second implementation
//! (reverse-MCP client ADR, Part B slice 2). Every tool is a thin dispatch into
//! `studio-cp`; this crate owns only the wire (JSON) representation and argument
//! plumbing. Cluster defaults to `$OAB_CLUSTER` (then `oab`) and is overridable
//! per call.

use anyhow::Result;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use studio_cp as scp;

/// The control-plane server. Holds the shared AWS config and the default
/// cluster; cheap to clone (one handler per session).
#[derive(Clone)]
pub struct OabMcp {
    /// Fallback AWS config (the default credential chain), used when no fleet
    /// binding governs the target cluster.
    aws: aws_config::SdkConfig,
    default_cluster: String,
    /// Declarative fleet → managing-credential bindings (ADR: Per-Fleet
    /// managing identity). Behind an `RwLock` so the write tools can hot-reload
    /// it after editing the config file, without a restart.
    bindings: Arc<RwLock<scp::FleetBindings>>,
    /// Where the bindings were loaded from (surfaced by `fleet_config` so the
    /// operator knows which file to edit); `None` when no config dir resolved.
    bindings_path: Option<String>,
    /// Resolved configs, memoized per identity source (`fleet:<name>` /
    /// `cluster:<cluster>`) so a binding is resolved once and can't be served
    /// to a different source.
    resolved: Arc<Mutex<HashMap<String, aws_config::SdkConfig>>>,
}

fn as_map(v: Value) -> Arc<Map<String, Value>> {
    Arc::new(v.as_object().expect("schema literal is an object").clone())
}

const INSTRUCTIONS: &str = "OAB Studio control plane. Observe deployments and \
per-instance lifecycle states (6-state model), and drive writes (apply a \
manifest, scale, delete). Reads are safe; writes mutate live ECS services.";

/// The read/write tool surface. Argument shapes are plain JSON Schema. Public so
/// an in-process driver (the reverse-MCP tunnel) can answer `tools/list` without
/// the stdio transport.
pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "deploy_list",
            "List all OAB deployments in the cluster (ECS) or namespace (k8s) with replica counts. For a k8s-runtime fleet, pass `fleet` — the fleet's own context/namespace govern the call, `cluster` is ignored.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster/context and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg. Required for a k8s-runtime fleet." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster). Ignored for a k8s-runtime fleet." }
                }
            })),
        ),
        Tool::new(
            "deploy_get",
            "Get one deployment's read-model: replica counters plus each instance's canonical lifecycle phase (6-state). Works for both ECS and k8s-runtime fleets (pass `fleet` for k8s).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string", "description": "ECS service name, k8s reconstructed oab-{namespace}-{name}, or bare agent name." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster/context and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg. Required for a k8s-runtime fleet." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster). Ignored for a k8s-runtime fleet." }
                },
                "required": ["service"]
            })),
        ),
        Tool::new(
            "get_agent_states",
            "List instances across the cluster (or one service) mapped to their canonical AgentState.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string", "description": "Optional: limit to one service." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." }
                }
            })),
        ),
        Tool::new(
            "deploy_events",
            "Read recent ECS control-plane events (task/service/deployment state changes) archived from EventBridge — the lifecycle timeline DescribeTasks cannot show (task stops + reasons, service impairment, deployments). NOTE: a container health-check flip while a task stays RUNNING is not emitted by ECS and will not appear here.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string", "description": "Optional: limit to one OAB service (oab-{namespace}-{name} or bare agent name)." },
                    "since_minutes": { "type": "integer", "description": "Look-back window in minutes (default 1440 = 24h)." },
                    "limit": { "type": "integer", "description": "Max events, newest first (default 50, max 1000)." },
                    "log_group": { "type": "string", "description": "CloudWatch Logs group the events are archived to (default $OAB_EVENTS_LOG_GROUP, else /oab/ecs-events)." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." }
                }
            })),
        ),
        Tool::new(
            "k8s_logs",
            "Fetch one Pod's container log for a k8s-runtime fleet — the k8s counterpart to `deploy_events`, which only supports ECS (control-plane events aren't container stdout/stderr, and k8s has no archival system to read; the k8s API serves pod logs directly). Requires `fleet` naming a k8s-runtime fleet (see fleet_config), same as deploy_get/deploy_list's k8s dispatch.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string", "description": "k8s reconstructed oab-{namespace}-{name}, or bare agent name." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config) naming a k8s-runtime fleet; targets its context/namespace. Required — no bare-cluster k8s dispatch exists, same as deploy_get/deploy_list." },
                    "instance_id": { "type": "string", "description": "Pod uid to select, from deploy_get/get_agent_states's InstancePhase.id. Required when more than one Pod matches (a rollout in flight, or a crashed Pod next to its replacement); optional with exactly one live Pod." },
                    "tail_lines": { "type": "integer", "description": "Max lines from the end of the log (default 200, max 10000)." },
                    "previous": { "type": "boolean", "description": "Read the last terminated container's log instead of the current one (like `kubectl logs -p`) — the only way to see why a CrashLoopBackOff pod died, since its current log is empty post-restart. Default false." }
                },
                "required": ["service"]
            })),
        ),
        Tool::new(
            "deploy_apply",
            "Apply an OABService/OABFleet manifest (create or update). Returns the number of services reconciled.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "manifest_yaml": { "type": "string", "description": "Full manifest YAML document." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." },
                    "wait": { "type": "boolean", "description": "Wait for services to stabilize (default false)." }
                },
                "required": ["manifest_yaml"]
            })),
        ),
        Tool::new(
            "deploy_scale",
            "Scale an OAB service on or off. OAB services run a single bot token, so size must be 0 (off) or 1 (on). Works for both ECS and k8s-runtime fleets (pass `fleet` for k8s — required, since a k8s-runtime fleet has no ECS cluster to fall back to).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Agent / service name (service = oab-{namespace}-{name})." },
                    "size": { "type": "integer", "enum": [0, 1], "description": "0 = off, 1 = on." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster/context and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg. Required for a k8s-runtime fleet." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster). Ignored for a k8s-runtime fleet." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\"). Ignored for a k8s-runtime fleet — the fleet binding's own namespace is used." }
                },
                "required": ["name", "size"]
            })),
        ),
        Tool::new(
            "deploy_provision",
            "Provision an agent from the compose library: compose template ⊕ overlay into a file bundle and push it to the agent's S3 artifacts prefix (bundle carrier — shared regardless of provider). If the agent already has a stored manifest, patches its image/bundle and re-applies (networking/resources/secrets/runtime ride along unchanged). If not, builds a fresh manifest with sensible defaults and creates the agent — this now works for a genuinely brand-new agent, not just a redeploy of one already created via `oabctl create`. `provider` (default \"aws\") selects the target: \"aws\" applies via ECS (`fleet`/`cluster` select the credential); \"k8s\" applies via the given kubeconfig `context` instead, with `expected_principal` optionally naming a service account (`system:serviceaccount:<ns>:<name>` — the bare name becomes the pod's serviceAccountName; unset uses the namespace's default).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "library": { "type": "object", "description": "The compose library document: { templates, overlays, skills }." },
                    "template": { "type": "string", "description": "Template name in the library." },
                    "overlay": { "type": "string", "description": "Overlay name (optional; omitted composes the bare template)." },
                    "name": { "type": "string", "description": "Agent / service name (service = oab-{namespace}-{name})." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\")." },
                    "image_tag": { "type": "string", "description": "Image tag override (defaults to the bundle's own image tag)." },
                    "provider": { "type": "string", "description": "\"aws\" (default) or \"k8s\" — which driver applies the result." },
                    "fleet": { "type": "string", "description": "AWS only. Fleet name (see fleet_config): targets the fleet's cluster and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "AWS only. ECS cluster (defaults to the server's configured cluster)." },
                    "context": { "type": "string", "description": "k8s only. Kubeconfig context to apply through. Omit to use the kubeconfig's current-context." },
                    "expected_principal": { "type": "string", "description": "k8s only, optional. `system:serviceaccount:<namespace>:<name>` to set the pod's service account; unset uses the namespace's default." },
                    "region": { "type": "string", "description": "AWS only, optional. Region to provision into, overriding the `fleet` binding's — or else the `cluster` binding's — `region` (studio#111); it LAYERS, so a field you omit still comes from that binding. Needed whenever that binding can't supply it: a `[fleet.<name>]` block need not declare the `cluster` key a per-cluster lookup matches on (the console's writer never emits one), and on a first create there is no block at all yet. Not memoized: an override is never cached for later calls that name none." },
                    "profile": { "type": "string", "description": "AWS only, optional. Named AWS profile to provision under (profile-first), overriding the `fleet` binding's — or else the `cluster` binding's — `profile`; it LAYERS, so a field you omit still comes from that binding. Same gap as `region` (studio#111), and not memoized either." }
                },
                "required": ["library", "template", "name"]
            })),
        ),
        Tool::new(
            "deploy_provision_agent",
            "Provision an agent directly from structured inputs — no compose library, no template ⊕ overlay (studio#128: the New Fleet wizard's vendor/chat-platform/ACP flow has no template to point at). config.toml is rendered server-side from these fields (the single source of truth — any caller, wizard or otherwise, that sends the same fields gets byte-identical config.toml, no drift between generators). Same create-vs-redeploy behavior as deploy_provision: patches an existing stored manifest's image/bundle if this agent already has one, otherwise builds a fresh manifest with sensible defaults. `provider` (default \"aws\") selects the target, same as deploy_provision. k8s deploys refuse a non-empty `chat_platform`: config.toml's secret resolution only supports aws-sm:// (AWS Secrets Manager), which a k8s pod has no credential chain to reach — ACP is the k8s connection path today.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "image": { "type": "string", "description": "Container image (e.g. ghcr.io/openabdev/openab:<tag>-<vendor>)." },
                    "name": { "type": "string", "description": "Agent / service name (service = oab-{namespace}-{name})." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\")." },
                    "api_key": { "type": "string", "description": "Optional vendor API key. Captured and stored as a secret; not yet wired into config.toml (which env var a given vendor's CLI expects it under needs vendor-specific follow-up)." },
                    "chat_platform": { "type": "string", "description": "Optional: \"discord\" | \"telegram\" | \"line\". Omit for no chat platform (connect via ACP directly). AWS only — refused for k8s deploys." },
                    "chat_bot_token": { "type": "string", "description": "Discord/Telegram bot token, or LINE's channel access token." },
                    "chat_channel_secret": { "type": "string", "description": "LINE only." },
                    "acp_enabled": { "type": "boolean", "description": "Enable the reverse-MCP-over-ACP tunnel on this agent. Defaults to true when omitted (studio#119: Studio-deployed agents default to ACP on). Not honorable for every vendor — the caller is responsible for not setting this true for a vendor that can't support it (e.g. agy, whose bridge bypasses /acp entirely)." },
                    "acp_token": { "type": "string", "description": "Optional (studio#136), only meaningful when acp_enabled is true. The OPENAB_ACP_AUTH_KEY to use, instead of generating a random one." },
                    "local_config_folder": { "type": "string", "description": "Optional local directory (studio#135) — when set, config.toml is written to <local_config_folder>/<name>/config.toml *before* anything touches S3. Omit to skip the local mirror entirely." },
                    "provider": { "type": "string", "description": "\"aws\" (default) or \"k8s\" — which driver applies the result." },
                    "fleet": { "type": "string", "description": "AWS only. Fleet name (see fleet_config): targets the fleet's cluster and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "AWS only. ECS cluster (defaults to the server's configured cluster)." },
                    "context": { "type": "string", "description": "k8s only. Kubeconfig context to apply through. Omit to use the kubeconfig's current-context." },
                    "expected_principal": { "type": "string", "description": "k8s only, optional. `system:serviceaccount:<namespace>:<name>` to set the pod's service account; unset uses the namespace's default." },
                    "region": { "type": "string", "description": "AWS only, optional. Region to provision into, overriding the `fleet` binding's — or else the `cluster` binding's — `region` (studio#111); it LAYERS, so a field you omit still comes from that binding. Needed whenever that binding can't supply it: a `[fleet.<name>]` block need not declare the `cluster` key a per-cluster lookup matches on (the console's writer never emits one), and on a first create there is no block at all yet. Not memoized: an override is never cached for later calls that name none." },
                    "profile": { "type": "string", "description": "AWS only, optional. Named AWS profile to provision under (profile-first), overriding the `fleet` binding's — or else the `cluster` binding's — `profile`; it LAYERS, so a field you omit still comes from that binding. Same gap as `region` (studio#111), and not memoized either." }
                },
                "required": ["image", "name"]
            })),
        ),
        Tool::new(
            "deploy_delete",
            "Delete a control-plane resource (e.g. an OABService).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "resource": { "type": "string", "description": "Resource kind, e.g. \"service\"." },
                    "name": { "type": "string" },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\")." }
                },
                "required": ["resource", "name"]
            })),
        ),
        Tool::new(
            "runtime_context",
            "Show the effective runtime identity/context this control plane resolved for a cluster/fleet: for ECS, the acting principal (STS caller ARN), its kind (role vs static user), account (scope), region (location), a best-effort credential-source hint; for a k8s-runtime fleet, the SelfSubjectReview-resolved principal against the fleet's kubeconfig context (cluster is null, context/namespace are set instead). Also returns the fleet binding in effect (if any) and — when the binding declares an expected_principal — whether the resolved identity matches it (identity_matches; a non-blocking IdentityMismatch when false). Read-only; answers \"who am I acting as, against what account?\" and surfaces silent credential fallback.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "fleet": { "type": "string", "description": "Resolve the identity the named fleet's binding selects (distinguishes two fleets that share a cluster). Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "Resolve the identity the fleet binding for this cluster selects (defaults to the server's configured cluster)." }
                }
            })),
        ),
        Tool::new(
            "fleet_config",
            "List the configured per-fleet managing-credential/context bindings (ADR: Per-Fleet managing identity): each fleet's name, runtime (`ecs` or `k8s`), ECS target cluster/AWS profile/region OR k8s context/namespace (whichever the runtime uses), members, and expected_principal, plus the config file path they load from and the server's default cluster. Read-only; the declarative 'declare' side of the config→switch→observe→reconcile loop. Profiles are names, not secrets.",
            as_map(json!({
                "type": "object",
                "properties": {}
            })),
        ),
        Tool::new(
            "fleet_config_write",
            "Persist the whole fleet-binding config from raw TOML `text` (what the UI's editor holds), then hot-reload. Validates the text parses before writing — a bad edit never lands on disk — and the bytes are stored verbatim, so comments/layout are preserved. Returns the updated fleet_config. Write tool: overwrites the operator's fleets.toml (both `ecs` and `k8s` runtime fleets live in this one file).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Full TOML document for fleets.toml (`[fleet.<name>]` tables, each requiring `runtime = \"ecs\"` or `runtime = \"k8s\"`)." }
                },
                "required": ["text"]
            })),
        ),
        Tool::new(
            "list_aws_profiles",
            "List AWS credential profiles discovered on this machine (scans ~/.aws/config and ~/.aws/credentials — this server runs as a local sidecar next to the console, so this reads the operator's own machine, not a remote one). Each entry has a name and region (region omitted when not set). `exists=false` means no AWS config was found at all — the caller should suggest `aws configure`/`aws sso login` rather than treat it as an error. Read-only; backs the New Fleet wizard's profile picker.",
            as_map(json!({
                "type": "object",
                "properties": {}
            })),
        ),
        Tool::new(
            "list_k8s_contexts",
            "List kubeconfig contexts discovered on this machine (reads $KUBECONFIG or ~/.kube/config — local sidecar, same machine as the console). Each entry has the context name, its cluster, namespace (if set), and user. `current_context` names the kubeconfig's default. `exists=false` means no kubeconfig was found at all — the caller should suggest installing a local cluster (OrbStack/kind/minikube) or merging in a cloud kubeconfig, rather than treat it as an error. Read-only; backs the New Fleet wizard's k8s context picker.",
            as_map(json!({
                "type": "object",
                "properties": {}
            })),
        ),
        Tool::new(
            "resolve_vendor_image_tags",
            "Resolve a vendor's real, currently-published Stable/Beta image tags on ghcr.io/openabdev/openab (studio#128 — backs the New Fleet wizard's Vendor + Image tag fields). Both are pinned <version>-<vendor> tags, never a rolling moving tag: \"Beta\" is the newest beta-named openab release (<version>-beta.N) whose matching <version>-beta.N-<vendor> image is confirmed to actually exist; \"Stable\" is the newest non-beta release whose matching <version>-<vendor> image is confirmed to actually exist (a release existing doesn't guarantee a matching image was ever built — the build workflow is a manual, disconnected step). Anonymous GHCR/GitHub access, no auth needed (public package/repo). Either or both fields come back null if nothing verified — not an error; the caller should fall back to a plain editable text field.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "vendor": { "type": "string", "description": "Vendor name, e.g. \"claude\", \"codex\", \"cursor\", \"kiro\", \"antigravity\"." }
                },
                "required": ["vendor"]
            })),
        ),
        Tool::new(
            "list_namespaces",
            "List namespaces in the cluster a kubeconfig context resolves to. Read-only; backs the New Fleet wizard's namespace <select> (with a manual-entry fallback for a namespace that doesn't exist yet — this can only list what's already there).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string", "description": "Kubeconfig context name. Omit to use the kubeconfig's current-context." }
                }
            })),
        ),
        Tool::new(
            "list_service_accounts",
            "List service accounts in one namespace of a kubeconfig context. Read-only; backs the New Fleet wizard's optional service-account <select>. Any failure here (including an RBAC-denied list) should be treated by the caller as \"leave it unset\" — the namespace's `default` service account applies — not surfaced as an error.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string", "description": "Kubeconfig context name. Omit to use the kubeconfig's current-context." },
                    "namespace": { "type": "string", "description": "k8s namespace to list service accounts in." }
                },
                "required": ["namespace"]
            })),
        ),
    ]
}

fn deployment_json(d: &scp::Deployment) -> Value {
    json!({
        "name": d.name,
        "namespace": d.namespace,
        "desired": d.desired,
        "current": d.current,
        "ready": d.ready,
        "instances": d
            .instances
            .iter()
            .map(|i| json!({ "id": i.id, "state": format!("{:?}", i.phase) }))
            .collect::<Vec<_>>(),
    })
}

fn event_json(e: &scp::EcsEvent) -> Value {
    json!({
        "time": e.time,
        "type": e.detail_type,
        "service": e.service,
        "last_status": e.last_status,
        "desired_status": e.desired_status,
        "stop_code": e.stop_code,
        "reason": e.reason,
    })
}

/// The resolved target of a tool call: which cluster to act against (→ which
/// credential) and, when a fleet was named, which fleet governs the call (its
/// members scope listing tools; its binding drives `runtime_context`).
struct Target {
    cluster: String,
    /// The governing fleet when the call named one, else `None` (a bare
    /// `cluster`/default call, unscoped).
    binding: Option<scp::FleetBinding>,
}

impl Target {
    /// Whether a service is in scope for this target — always true for an
    /// unscoped call; else the fleet's membership test (empty members ⇒ whole
    /// cluster).
    fn includes(&self, service_name: &str, short_name: &str) -> bool {
        match &self.binding {
            None => true,
            Some(b) => b.includes(service_name, short_name),
        }
    }
}

/// The fleet binding whose credential a call should act under (studio#111):
/// the **named** fleet's own binding when the caller passed `fleet`, else the
/// binding that governs `cluster`.
///
/// Fleet-name-first is the whole point of the resolution seam. `for_cluster`
/// alone can only ever match a binding that declares a `cluster` key, and
/// neither the console's writer nor the ADR's canonical `[fleet.<name>]`
/// example emits one — so every console-created fleet's recorded
/// `region`/`profile` was read by nothing, on writes *and* on reads alike. A
/// deploy that honors the operator's chosen profile while the roster that must
/// then show the agent keeps asking the ambient `[default]` chain is worse than
/// either half alone, so observation has to resolve by the same key.
///
/// An unknown/blank `fleet` name simply falls through to the cluster lookup —
/// this helper never introduces an error the caller didn't already have (the
/// handlers that name a fleet still reject an unknown one via `named_fleet` /
/// `target` before reaching any of this).
fn base_binding(
    bindings: &scp::FleetBindings,
    fleet: Option<&str>,
    cluster: &str,
) -> Option<scp::FleetBinding> {
    fleet
        .and_then(|name| bindings.get(name))
        .filter(|b| b.runtime == scp::FleetRuntime::Ecs)
        .cloned()
        .or_else(|| bindings.for_cluster(cluster).cloned())
}

/// [`base_binding`] with the caller's own `region`/`profile` layered on top.
///
/// Kept free of `.await` so the fleet-name-first lookup, the layering and the
/// argument order are all unit-testable without an AWS client. Returns a blank
/// binding when no fleet is named and none governs the cluster (the common case
/// for a console-created fleet), in which case the caller's own answer is the
/// only identity there is.
fn identity_binding(
    bindings: &scp::FleetBindings,
    args: &Map<String, Value>,
    cluster: &str,
    region: Option<&str>,
    profile: Option<&str>,
) -> scp::FleetBinding {
    base_binding(bindings, args.get("fleet").and_then(Value::as_str), cluster)
        .unwrap_or_default()
        .with_identity(region, profile)
}

/// The caller's own `region`/`profile` override, read out of a call's args, as
/// `(region, profile)` in that order. Extracted beside [`base_binding`] so the
/// two halves cannot be swapped — the covered failure being a handler pairing
/// the region with the profile's field, which would resolve a config no caller
/// asked for and would otherwise no-op with every test in the repo green.
fn identity_args(args: &Map<String, Value>) -> (Option<&str>, Option<&str>) {
    (
        args.get("region").and_then(Value::as_str),
        args.get("profile").and_then(Value::as_str),
    )
}

/// The credential a call should act under, as `(base binding, memo key)` — the
/// whole decision [`OabMcp::aws_for_binding`] makes, minus the `.await`.
///
/// `base` is `None` when nothing governs the call, **or** when the governing
/// binding names no profile and no region of its own: both mean "the ambient
/// chain answers", and answering it with the already-loaded config is cheaper
/// than resolving a duplicate. Keeping that short-circuit — and the `fleet`-name
/// extraction that picks the memo namespace — here (rather than inline in the
/// async fn) is what makes both testable without an AWS client.
///
/// Deliberate, operator-visible: a `fleet`-scoped call naming a binding that
/// declares no credential of its own now answers ambient, where `for_cluster`'s
/// first match on the same `cluster` key used to lend it that sibling's
/// credential. Shadowing a sibling by accident is exactly what per-fleet scoping
/// exists to stop, and `resolve_binding_config` pins only what a binding
/// actually names — a credential-less block says nothing about the credential,
/// so ambient is what it is asking for. It can still be given one, per call or
/// in its own block.
fn binding_and_key(
    bindings: &scp::FleetBindings,
    args: &Map<String, Value>,
    cluster: &str,
) -> (Option<scp::FleetBinding>, String) {
    let fleet = args.get("fleet").and_then(Value::as_str);
    let key = memo_key(fleet, cluster);
    // `bindings` is a plain borrow — the caller's `read()` guard died at the end
    // of its own statement, so nothing here can hold a lock across an await
    // (there isn't one). With no fleet named, `base_binding` reduces to the
    // `for_cluster` fallback, so both cases are one expression.
    let binding = base_binding(bindings, fleet, cluster)
        .filter(|b| b.profile.is_some() || b.region.is_some());
    (binding, key)
}

/// The memo key for a resolved credential: namespaced by identity source, so a
/// fleet-scoped entry can never be served to a cluster-scoped call (or to a
/// different fleet) that resolves a different binding. Pure, so the two
/// namespaces provably stay disjoint.
fn memo_key(fleet: Option<&str>, cluster: &str) -> String {
    match fleet {
        Some(name) => format!("fleet:{name}"),
        None => format!("cluster:{cluster}"),
    }
}

/// Whether a call carries enough of its own answer to resolve an identity
/// without consulting a binding at all. Blank/whitespace counts as unset, so a
/// client that always sends both fields (an empty text input) still takes the
/// ambient path. Split out of [`OabMcp::aws_or`] so that rule is testable.
fn has_identity_override(region: Option<&str>, profile: Option<&str>) -> bool {
    let named = |s: Option<&str>| s.is_some_and(|s| !s.trim().is_empty());
    named(region) || named(profile)
}

impl OabMcp {
    /// Build the handler from the environment: the default AWS credential chain,
    /// `$OAB_CLUSTER` (then `oab`), and the opt-in fleet bindings file (a missing
    /// file yields an empty set; a load failure warns on stderr — stdout is the
    /// MCP JSON-RPC wire for the stdio front door). Shared by the stdio binary
    /// and the in-process tunnel driver.
    pub async fn from_env() -> Result<Self> {
        // studio#119: install a process-level rustls CryptoProvider before any
        // TLS handshake can happen (k8s client build, AWS SDK calls). Without
        // this, the first handshake panics — both `ring` (via kube's
        // rustls-tls) and `aws-lc-rs` (via the AWS SDK crates) are present in
        // the dependency graph and rustls refuses to guess. Ignore the error:
        // it just means some other path already installed one first, which is
        // fine — we only need *a* provider installed, not this exact call to
        // win.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let default_cluster = std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string());
        // Two one-time, idempotent startup migrations (2026-09-06
        // unification) — order matters: normalize must run first, since
        // migrate_legacy_k8s_bindings re-validates the whole file (including
        // pre-existing entries) via save_bindings_text, which now requires
        // `runtime` on every entry.
        // 1. Give every pre-existing entry an explicit `runtime = "ecs"` (the
        //    field used to default silently; it's required now).
        if let Some(p) = scp::default_bindings_path() {
            if let Err(e) = scp::normalize_bindings_runtime_field(&p) {
                eprintln!("warning: failed to normalize fleets.toml runtime field: {e:#}");
            }
        }
        // 2. Fold a legacy fleets-k8s.toml into fleets.toml if one is still
        //    around. No-op after the first successful run (legacy file gets
        //    renamed away).
        if let Err(e) = scp::migrate_legacy_k8s_bindings() {
            eprintln!("warning: failed to migrate legacy fleets-k8s.toml: {e:#}");
        }
        let bindings_path = scp::default_bindings_path();
        let bindings = match &bindings_path {
            Some(path) => scp::load_bindings(path).unwrap_or_else(|e| {
                eprintln!(
                    "warning: failed to load fleet bindings from {}: {e:#}",
                    path.display()
                );
                scp::FleetBindings::default()
            }),
            None => scp::FleetBindings::default(),
        };
        Ok(OabMcp {
            aws,
            default_cluster,
            bindings: Arc::new(RwLock::new(bindings)),
            bindings_path: bindings_path.map(|p| p.display().to_string()),
            resolved: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The MCP server info / capabilities — the `initialize` result. Public so the
    /// in-process tunnel driver answers inner `initialize` identically to the
    /// stdio server.
    pub fn info(&self) -> ServerInfo {
        let mut server_info = Implementation::default();
        server_info.name = "oab-studio".into();
        server_info.version = env!("CARGO_PKG_VERSION").into();
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(INSTRUCTIONS.into());
        info
    }

    /// Execute one tool by name, returning its JSON result (or an error). The
    /// transport-agnostic core of the handler: the stdio `call_tool` wraps this
    /// into a `CallToolResult`, and the reverse-MCP tunnel will wrap it the same
    /// way from an inner `tools/call` — one dispatch, two front doors.
    pub async fn dispatch(&self, name: &str, args: &Map<String, Value>) -> Result<Value> {
        match name {
            "deploy_list" => self.t_list(args).await,
            "deploy_get" => self.t_get(args).await,
            "get_agent_states" => self.t_states(args).await,
            "deploy_events" => self.t_events(args).await,
            "k8s_logs" => self.t_k8s_logs(args).await,
            "deploy_apply" => self.t_apply(args).await,
            "deploy_provision" => self.t_provision(args).await,
            "deploy_provision_agent" => self.t_provision_agent(args).await,
            "resolve_vendor_image_tags" => self.t_resolve_vendor_image_tags(args).await,
            "deploy_scale" => self.t_scale(args).await,
            "deploy_delete" => self.t_delete(args).await,
            "runtime_context" => self.t_runtime_context(args).await,
            "fleet_config" => self.t_fleet_config(args),
            "fleet_config_write" => self.t_fleet_write(args),
            "list_aws_profiles" => self.t_list_aws_profiles(args),
            "list_k8s_contexts" => self.t_list_k8s_contexts(args),
            "list_namespaces" => self.t_list_namespaces(args).await,
            "list_service_accounts" => self.t_list_service_accounts(args).await,
            other => anyhow::bail!("unknown tool {other:?}"),
        }
    }

    fn cluster(&self, args: &Map<String, Value>) -> String {
        args.get("cluster")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.default_cluster.clone())
    }

    /// Resolve a call's target from its args. A **`fleet`** (name) selects the
    /// fleet's cluster (and thus its managing credential) and its member scope;
    /// an unknown name is an **error**, never a silent fall-through to the
    /// default cluster (that silent fallback is the AccessDenied class of bug the
    /// fleet work exists to kill). Absent a `fleet`, the target is the `cluster`
    /// arg (or the server default) with no member scope — back-compat.
    fn target(&self, args: &Map<String, Value>) -> Result<Target> {
        if let Some(name) = args.get("fleet").and_then(Value::as_str) {
            let guard = self.bindings.read().unwrap();
            let binding = guard.get(name).cloned().ok_or_else(|| {
                let known: Vec<&str> = guard.fleets.iter().map(|f| f.name.as_str()).collect();
                anyhow::anyhow!("unknown fleet {name:?}; configured fleets: [{}]", known.join(", "))
            })?;
            let cluster = binding.cluster.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "fleet {name:?} is a k8s-runtime fleet; this tool only supports ecs fleets"
                )
            })?;
            return Ok(Target {
                cluster,
                binding: Some(binding),
            });
        }
        Ok(Target {
            cluster: self.cluster(args),
            binding: None,
        })
    }

    /// Like [`Self::target`], but doesn't reject a k8s-runtime fleet — for
    /// the calls this made runtime-aware (studio#146): roster list/get and
    /// `runtime_context`. Returns the named fleet's binding, or `None` for
    /// an unscoped call (back-compat, same as `target()`'s bare-cluster
    /// path). An unknown fleet name is still an error, same as `target()`.
    /// `target()` itself stays ecs-only — every other call (scale/delete/
    /// apply/provision/events) genuinely has no k8s-runtime dispatch yet, so
    /// it should keep failing loudly rather than silently acting against an
    /// empty cluster string.
    fn named_fleet(&self, args: &Map<String, Value>) -> Result<Option<scp::FleetBinding>> {
        let Some(name) = args.get("fleet").and_then(Value::as_str) else {
            return Ok(None);
        };
        let guard = self.bindings.read().unwrap();
        let binding = guard.get(name).cloned().ok_or_else(|| {
            let known: Vec<&str> = guard.fleets.iter().map(|f| f.name.as_str()).collect();
            anyhow::anyhow!("unknown fleet {name:?}; configured fleets: [{}]", known.join(", "))
        })?;
        Ok(Some(binding))
    }

    /// The AWS config to act as for a call: the **named** fleet's own binding
    /// when the caller passed `fleet`, else the binding governing `cluster`,
    /// else the ambient default chain. Resolved once per identity source, then
    /// memoized. This is where the per-fleet **switch** takes effect — a
    /// bound fleet's calls run under its credential, not whatever ambient
    /// `[default]` the chain resolves first.
    ///
    /// Fleet-name-first matters because [`FleetBindings::for_cluster`] can only
    /// ever match a binding that declares a `cluster` key. Two ecs fleets on
    /// one cluster key are then indistinguishable by cluster alone, and
    /// whichever came first in the file won both the credential and the
    /// override base (studio#111).
    async fn aws_for_call(
        &self,
        args: &Map<String, Value>,
        cluster: &str,
    ) -> aws_config::SdkConfig {
        self.aws_for_binding(args, cluster).await
    }

    /// Resolve (and memoize) the credential the base binding selects, or the
    /// already-loaded ambient chain when there is none worth resolving — the
    /// whole decision being [`binding_and_key`].
    async fn aws_for_binding(&self, args: &Map<String, Value>, cluster: &str) -> aws_config::SdkConfig {
        // Short read-lock, handed over by reference, guard dropped at this
        // statement's end — nothing below holds a lock across an await.
        let (binding, key) = binding_and_key(&self.bindings.read().unwrap(), args, cluster);
        let Some(binding) = binding else {
            // Nothing governs this call, or what does names no credential of
            // its own: the ambient chain answers, and it is already loaded —
            // resolving it again would burn a config load and cache a duplicate
            // under this source's key.
            return self.aws.clone();
        };
        if let Some(cfg) = self.resolved.lock().unwrap().get(&key) {
            return cfg.clone();
        }
        let cfg = scp::resolve_binding_config(&binding).await;
        self.resolved.lock().unwrap().insert(key, cfg.clone());
        cfg
    }

    /// [`Self::aws_for_call`], but with the caller's own `region`/`profile`
    /// taking precedence (studio#111).
    ///
    /// The cluster **fallback** inside `aws_for_call` keys on a binding's
    /// `cluster` field, which a `[fleet.<name>]` block need not declare — the
    /// console never writes one (`appendFleetBlock` emits
    /// runtime/region/profile/members, not `cluster`), and neither does the
    /// shape the ADR's canonical example shows — so for those fleets it finds
    /// nothing and the fleet's recorded `region`/`profile` are read by nothing.
    /// A
    /// deploy is exactly where that hurts most: on a *first* create there is no
    /// block at all yet (`fleets.toml` is written only after a confirmed
    /// successful provision), and the manifest built by #111's
    /// create-from-scratch path resolves its VPC/subnet/security-group
    /// defaults against whatever config it is handed, so a create meant for
    /// one account/region would land in another. Passing the wizard's answer
    /// with the call closes that.
    ///
    /// The base is the *named* fleet's binding when the caller passed `fleet`,
    /// else the one governing `cluster` (see [`Self::aws_for_call`]); the
    /// overrides are layered onto it, never substituted for it, so a caller that
    /// pins only `region` still acts under whichever profile that base selects.
    /// With neither override it is exactly [`Self::aws_for_call`].
    ///
    /// Note the asymmetry this deliberately leaves. The console writes
    /// `[fleet.<name>]` blocks with no `cluster` key and sends no `fleet`, so
    /// for a console-created fleet this base is `for_cluster`'s *first* match —
    /// and none, since no such block declares a `cluster`; only the caller's own
    /// pair applies, which is the point. Two further consequences, both
    /// pre-existing and neither fixable from here: `target()` rejects a
    /// `fleet`-scoped ECS call whose binding omits `cluster`, so reads of a
    /// console-created fleet never reach this seam at all; and an operator who
    /// hand-adds a `cluster` key in the console's `fleets.toml` editor can
    /// reach the cluster base, where a region-only override pairs with that
    /// cluster's first fleet's profile. Both need credential selection keyed on
    /// the governing fleet rather than the cluster — a separate change.
    async fn aws_or(
        &self,
        args: &Map<String, Value>,
        cluster: &str,
        region: Option<&str>,
        profile: Option<&str>,
    ) -> aws_config::SdkConfig {
        if !has_identity_override(region, profile) {
            return self.aws_for_call(args, cluster).await;
        }
        // Short read-lock, handed over by reference, guard dropped at this
        // statement's end — nothing below holds a lock across an await.
        let binding = identity_binding(
            &self.bindings.read().unwrap(),
            args,
            cluster,
            region,
            profile,
        );
        scp::resolve_binding_config(&binding).await
    }

    async fn t_list(&self, args: &Map<String, Value>) -> Result<Value> {
        if let Some(b) = self.named_fleet(args)? {
            if b.runtime == scp::FleetRuntime::K8s {
                let namespace = b.namespace.clone().unwrap_or_else(|| "default".to_string());
                let svcs = scp::observe_k8s_services(b.context.as_deref(), &namespace).await?;
                let deployments: Vec<Value> = svcs
                    .iter()
                    .filter(|s| b.includes(&s.service_name, &s.name))
                    .map(|s| json!({ "name": s.name, "namespace": s.namespace, "desired": s.desired }))
                    .collect();
                return Ok(json!({ "context": b.context, "namespace": namespace, "deployments": deployments }));
            }
        }
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let svcs =
            scp::observe_services(&self.aws_for_call(args, &cluster).await, &cluster).await?;
        let deployments: Vec<Value> = svcs
            .iter()
            .filter(|s| t.includes(&s.service_name, &s.name))
            .map(|s| {
                json!({
                    "name": s.name,
                    "namespace": s.namespace,
                    "running": s.running,
                    "desired": s.desired,
                    "status": s.status,
                    "cpu": s.cpu,
                    "memory": s.memory,
                    "capacity": s.capacity,
                })
            })
            .collect();
        Ok(json!({ "cluster": cluster, "deployments": deployments }))
    }

    async fn t_get(&self, args: &Map<String, Value>) -> Result<Value> {
        let service = args
            .get("service")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: service"))?;
        if let Some(b) = self.named_fleet(args)? {
            if b.runtime == scp::FleetRuntime::K8s {
                let namespace = b.namespace.clone().unwrap_or_else(|| "default".to_string());
                return match scp::observe_k8s_deployment(b.context.as_deref(), &namespace, service).await? {
                    Some(d) => Ok(deployment_json(&d)),
                    None => Ok(json!({ "found": false, "service": service })),
                };
            }
        }
        let cluster = self.target(args)?.cluster;
        match scp::observe_deployment(&self.aws_for_call(args, &cluster).await, &cluster, service)
            .await?
        {
            Some(d) => Ok(deployment_json(&d)),
            None => Ok(json!({ "found": false, "service": service })),
        }
    }

    async fn t_states(&self, args: &Map<String, Value>) -> Result<Value> {
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let services: Vec<String> = match args.get("service").and_then(Value::as_str) {
            Some(s) => vec![s.to_string()],
            None => scp::observe_services(&self.aws_for_call(args, &cluster).await, &cluster)
                .await?
                .into_iter()
                .filter(|s| t.includes(&s.service_name, &s.name))
                .map(|s| s.name)
                .collect(),
        };
        let mut instances = Vec::new();
        for svc in services {
            if let Some(d) =
                scp::observe_deployment(&self.aws_for_call(args, &cluster).await, &cluster, &svc)
                    .await?
            {
                for inst in &d.instances {
                    instances.push(json!({
                        "service": d.name,
                        "namespace": d.namespace,
                        "instance": inst.id,
                        "state": format!("{:?}", inst.phase),
                    }));
                }
            }
        }
        Ok(json!({ "cluster": cluster, "instances": instances }))
    }

    async fn t_events(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.target(args)?.cluster;
        let service = args.get("service").and_then(Value::as_str);
        let since_minutes = args
            .get("since_minutes")
            .and_then(Value::as_i64)
            .unwrap_or(1440)
            .max(1);
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 1000) as i32;
        let log_group = args
            .get("log_group")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                std::env::var("OAB_EVENTS_LOG_GROUP")
                    .unwrap_or_else(|_| scp::DEFAULT_EVENTS_LOG_GROUP.to_string())
            });

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let since_ms = now_ms - since_minutes * 60_000;

        let events = scp::observe_events(
            &self.aws_for_call(args, &cluster).await,
            &log_group,
            &cluster,
            service,
            since_ms,
            limit,
        )
        .await?;
        Ok(json!({
            "cluster": cluster,
            "log_group": log_group,
            "since_minutes": since_minutes,
            "count": events.len(),
            "events": events.iter().map(event_json).collect::<Vec<_>>(),
        }))
    }

    async fn t_k8s_logs(&self, args: &Map<String, Value>) -> Result<Value> {
        let service = args
            .get("service")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: service"))?;
        let Some(b) = self.named_fleet(args)? else {
            anyhow::bail!(
                "k8s_logs requires `fleet` naming a k8s-runtime fleet (see fleet_config) — no bare-cluster k8s dispatch exists, same as deploy_get/deploy_list"
            );
        };
        if b.runtime != scp::FleetRuntime::K8s {
            anyhow::bail!(
                "fleet {:?} is not a k8s-runtime fleet; use deploy_events for an ecs fleet's logs",
                b.name
            );
        }
        let namespace = b.namespace.clone().unwrap_or_else(|| "default".to_string());
        let instance_id = args.get("instance_id").and_then(Value::as_str);
        let tail_lines = args
            .get("tail_lines")
            .and_then(Value::as_i64)
            .unwrap_or(200)
            .clamp(1, 10_000);
        let previous = args
            .get("previous")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match scp::fetch_k8s_pod_logs(
            b.context.as_deref(),
            &namespace,
            service,
            instance_id,
            tail_lines,
            previous,
        )
        .await?
        {
            Some(logs) => Ok(json!({
                "service": service,
                "namespace": namespace,
                "previous": previous,
                "logs": logs,
            })),
            None => Ok(json!({ "found": false, "service": service })),
        }
    }

    async fn t_provision(&self, args: &Map<String, Value>) -> Result<Value> {
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: name"))?;
        let template = args
            .get("template")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: template"))?;
        let overlay = args.get("overlay").and_then(Value::as_str);
        let image = args.get("image_tag").and_then(Value::as_str);
        let (region, profile) = identity_args(args);
        let library: scp::Library = serde_json::from_value(
            args.get("library")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing required arg: library"))?,
        )
        .map_err(|e| anyhow::anyhow!("invalid library: {e}"))?;

        // studio#104: k8s dispatch. `context`/`expected_principal` come as
        // direct args — the console's identity form already collects them
        // (context/namespace/service-account <select>s, #108/#109); a
        // fleet-scoped k8s lookup (resolving these from an existing
        // `FleetBinding{runtime: K8s}` instead) is future work for redeploys
        // into an *existing* k8s fleet, not needed for this dispatch to exist.
        if args.get("provider").and_then(Value::as_str) == Some("k8s") {
            let context = args.get("context").and_then(Value::as_str);
            let expected_principal = args.get("expected_principal").and_then(Value::as_str);
            let outcome = scp::provision_from_library_k8s(
                &self.aws,
                context,
                namespace,
                name,
                &library,
                template,
                overlay,
                image,
                expected_principal,
            )
            .await?;
            return Ok(json!({
                "ok": true,
                "context": context,
                "namespace": namespace,
                "name": name,
                "image": outcome.image,
                "digest": outcome.digest,
                "objects": outcome.objects,
                "action": outcome.action,
                "services_applied": outcome.services_applied,
            }));
        }

        let t = self.target(args)?;
        let cluster = t.cluster.clone();

        // Same fleet-scope guard as scale/delete: a fleet handle only provisions
        // its own members, so a scoped call can't reach a co-located non-member.
        let service_name = format!("oab-{namespace}-{name}");
        if !t.includes(&service_name, name) {
            anyhow::bail!("service {service_name:?} is not a member of the named fleet");
        }

        let outcome = scp::provision_from_library(
            &self.aws_or(args, &cluster, region, profile).await,
            &cluster,
            namespace,
            name,
            &library,
            template,
            overlay,
            image,
        )
        .await?;
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "namespace": namespace,
            "name": name,
            "image": outcome.image,
            "digest": outcome.digest,
            "objects": outcome.objects,
            "action": outcome.action,
            "services_applied": outcome.services_applied,
        }))
    }

    /// [`t_provision`], but for `deploy_provision_agent` (studio#128) — no
    /// `library`/`template`/`overlay` args, `config_toml` is used as-is.
    async fn t_provision_agent(&self, args: &Map<String, Value>) -> Result<Value> {
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: name"))?;
        let image = args
            .get("image")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: image"))?;
        // studio#111: the caller's own AWS identity, overriding the fleet
        // binding's `region`/`profile` — see `aws_or`.
        let (region, profile) = identity_args(args);
        let input = scp::AgentWizardInput {
            api_key: args.get("api_key").and_then(Value::as_str).map(str::to_string),
            chat_platform: args.get("chat_platform").and_then(Value::as_str).map(str::to_string),
            chat_bot_token: args.get("chat_bot_token").and_then(Value::as_str).map(str::to_string),
            chat_channel_secret: args
                .get("chat_channel_secret")
                .and_then(Value::as_str)
                .map(str::to_string),
            // studio#119: default on when the caller doesn't say — matches
            // build_default_manifest's original hardcoded behavior before
            // studio#128 made it caller-controlled.
            acp_enabled: args.get("acp_enabled").and_then(Value::as_bool).unwrap_or(true),
            acp_token: args.get("acp_token").and_then(Value::as_str).map(str::to_string),
        };
        // studio#135: Brett's explicit ordering — write local first, S3
        // (via provision_agent[_k8s]'s existing upload) after. Optional:
        // the console only ever sends this when the operator has a local
        // Config folder set at all (its own opt-in setting).
        let local_config_folder = args.get("local_config_folder").and_then(Value::as_str);

        if args.get("provider").and_then(Value::as_str) == Some("k8s") {
            let context = args.get("context").and_then(Value::as_str);
            let expected_principal = args.get("expected_principal").and_then(Value::as_str);
            let outcome = scp::provision_agent_k8s(
                &self.aws,
                context,
                namespace,
                name,
                image,
                input,
                expected_principal,
                local_config_folder,
            )
            .await?;
            return Ok(json!({
                "ok": true,
                "context": context,
                "namespace": namespace,
                "name": name,
                "image": outcome.image,
                "digest": outcome.digest,
                "objects": outcome.objects,
                "action": outcome.action,
                "services_applied": outcome.services_applied,
            }));
        }

        let t = self.target(args)?;
        let cluster = t.cluster.clone();

        let service_name = format!("oab-{namespace}-{name}");
        if !t.includes(&service_name, name) {
            anyhow::bail!("service {service_name:?} is not a member of the named fleet");
        }

        let outcome = scp::provision_agent(
            &self.aws_or(args, &cluster, region, profile).await,
            &cluster,
            namespace,
            name,
            image,
            input,
            local_config_folder,
        )
        .await?;
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "namespace": namespace,
            "name": name,
            "image": outcome.image,
            "digest": outcome.digest,
            "objects": outcome.objects,
            "action": outcome.action,
            "services_applied": outcome.services_applied,
        }))
    }

    /// `resolve_vendor_image_tags` (studio#128) — no fleet/cluster/AWS
    /// credential resolution at all, this is pure GHCR/GitHub read access.
    async fn t_resolve_vendor_image_tags(&self, args: &Map<String, Value>) -> Result<Value> {
        let vendor = args
            .get("vendor")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: vendor"))?;
        let tags = scp::resolve_vendor_image_tags(vendor).await;
        Ok(json!({ "beta": tags.beta, "stable": tags.stable }))
    }

    async fn t_apply(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.target(args)?.cluster;
        let manifest = args
            .get("manifest_yaml")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: manifest_yaml"))?;
        let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let report = scp::apply_deployment(
            &self.aws_for_call(args, &cluster).await,
            manifest,
            &cluster,
            wait,
        )
        .await?;
        Ok(json!({ "ok": true, "services_applied": report.services.len() }))
    }

    async fn t_scale(&self, args: &Map<String, Value>) -> Result<Value> {
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: name"))?;
        let size =
            args.get("size")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("missing or invalid arg: size"))? as i32;
        let service_name = format!("oab-{namespace}-{name}");
        if let Some(b) = self.named_fleet(args)? {
            if b.runtime == scp::FleetRuntime::K8s {
                // Guard: a fleet handle only operates its own members — same
                // reasoning as the ECS path below.
                if !b.includes(&service_name, name) {
                    anyhow::bail!("service {service_name:?} is not a member of the named fleet");
                }
                let namespace = b.namespace.clone().unwrap_or_else(|| namespace.to_string());
                scp::scale_k8s_deployment(b.context.as_deref(), &namespace, name, size).await?;
                return Ok(json!({
                    "ok": true, "cluster": null, "context": b.context, "namespace": namespace,
                    "name": name, "size": size,
                }));
            }
        }
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        // Guard: a fleet handle only operates its own members — refuse to scale a
        // service outside the named fleet (a no-op for unscoped or whole-cluster
        // calls). Stops a fleet-scoped call from reaching a co-located non-member.
        if !t.includes(&service_name, name) {
            anyhow::bail!("service {service_name:?} is not a member of the named fleet");
        }
        scp::scale_deployment(
            &self.aws_for_call(args, &cluster).await,
            &cluster,
            namespace,
            name,
            size,
        )
        .await?;
        Ok(
            json!({ "ok": true, "cluster": cluster, "namespace": namespace, "name": name, "size": size }),
        )
    }

    async fn t_runtime_context(&self, args: &Map<String, Value>) -> Result<Value> {
        if let Some(b) = self.named_fleet(args)? {
            if b.runtime == scp::FleetRuntime::K8s {
                let ctx = scp::observe_k8s_identity(b.context.as_deref()).await?;
                let expected = b.expected_principal.clone();
                let identity_matches = expected
                    .as_ref()
                    .map(|e| scp::principal_matches(e, &ctx.principal));
                return Ok(json!({
                    "cluster": null,
                    "context": b.context,
                    "namespace": b.namespace,
                    "principal": ctx.principal,
                    "principal_kind": ctx.principal_kind,
                    "scope": ctx.scope,
                    "location": ctx.location,
                    "source": ctx.source,
                    "caller_id": ctx.caller_id,
                    "binding": json!({
                        "name": b.name,
                        "context": b.context,
                        "namespace": b.namespace,
                        "expected_principal": b.expected_principal,
                    }),
                    "expected_principal": expected,
                    "identity_matches": identity_matches,
                }));
            }
        }
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let aws = self.aws_for_call(args, &cluster).await;
        let ctx = scp::observe_identity(&aws).await?;
        // The governing binding: the **named fleet** when the call named one (so
        // two fleets sharing a cluster resolve distinctly), else the first fleet
        // bound to the cluster. Cloned so no lock is held across the await above.
        let governing = match t.binding {
            Some(b) => Some(b),
            None => self.bindings.read().unwrap().for_cluster(&cluster).cloned(),
        };
        let (binding, expected) = match governing {
            Some(b) => (
                Some(json!({
                    "name": b.name,
                    "profile": b.profile,
                    "region": b.region,
                    "expected_principal": b.expected_principal,
                })),
                b.expected_principal.clone(),
            ),
            None => (None, None),
        };
        // Reconcile: expected (declared) vs actual (resolved). null when no
        // expectation is declared. Non-blocking — a warning signal, not a gate.
        let identity_matches = expected
            .as_ref()
            .map(|e| scp::principal_matches(e, &ctx.principal));
        Ok(json!({
            "cluster": cluster,
            "principal": ctx.principal,
            "principal_kind": ctx.principal_kind,
            "scope": ctx.scope,
            "location": ctx.location,
            "source": ctx.source,
            "caller_id": ctx.caller_id,
            "binding": binding,
            "expected_principal": expected,
            "identity_matches": identity_matches,
        }))
    }

    /// The declarative fleet-binding config, read-only: which credential manages
    /// which fleet/cluster, plus the file it loads from and the default cluster.
    /// The `declare` side of the loop — what a config panel renders and switches
    /// between. Profiles are names, never secret material.
    fn t_fleet_config(&self, _args: &Map<String, Value>) -> Result<Value> {
        let fleets: Vec<Value> = self
            .bindings
            .read()
            .unwrap()
            .fleets
            .iter()
            .map(|b| {
                json!({
                    "name": b.name,
                    "runtime": match b.runtime {
                        scp::FleetRuntime::Ecs => "ecs",
                        scp::FleetRuntime::K8s => "k8s",
                    },
                    "cluster": b.cluster,
                    "members": b.members,
                    "region": b.region,
                    "profile": b.profile,
                    "context": b.context,
                    "namespace": b.namespace,
                    "expected_principal": b.expected_principal,
                })
            })
            .collect();
        // Raw file text so the UI's TOML editor loads the actual file (comments
        // and all); empty when there's no path or the file doesn't exist yet.
        let text = match &self.bindings_path {
            Some(p) => scp::read_bindings_text(std::path::Path::new(p)).unwrap_or_default(),
            None => String::new(),
        };
        Ok(json!({
            "path": self.bindings_path,
            "default_cluster": self.default_cluster,
            "fleets": fleets,
            "text": text,
        }))
    }

    /// The config file to edit, or an error when no config dir resolved (the
    /// write tool needs a concrete path).
    fn bindings_path(&self) -> Result<std::path::PathBuf> {
        self.bindings_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("no fleet config path resolved; cannot write bindings"))
    }

    /// Write tool: persist the whole `fleets.toml` from the editor's `text`
    /// after validating it parses (a bad edit never lands on disk), then
    /// hot-reload — swap in the reparsed bindings and drop the memoized
    /// configs (a changed profile/region invalidates every one of them). Returns
    /// the new `fleet_config`.
    fn t_fleet_write(&self, args: &Map<String, Value>) -> Result<Value> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: text"))?;
        let path = self.bindings_path()?;
        let next = scp::save_bindings_text(&path, text)?;
        *self.bindings.write().unwrap() = next;
        self.resolved.lock().unwrap().clear();
        self.t_fleet_config(args)
    }

    /// Local AWS profile discovery (studio#104): backs the New Fleet wizard's
    /// profile `<select>`. `exists`/`error` are surfaced separately so the
    /// console can tell "nothing configured yet" (show setup guidance) apart
    /// from "found a config but couldn't read it" (show the raw error).
    fn t_list_aws_profiles(&self, _args: &Map<String, Value>) -> Result<Value> {
        let r = scp::list_aws_profiles();
        Ok(json!({
            "profiles": r.profiles.iter().map(|p| json!({ "name": p.name, "region": p.region })).collect::<Vec<_>>(),
            "source_path": r.source_path,
            "exists": r.exists,
            "error": r.error,
        }))
    }

    /// Local kubeconfig context discovery (studio#104): backs the New Fleet
    /// wizard's k8s context `<select>`. Same `exists`/`error` split as
    /// `list_aws_profiles`.
    fn t_list_k8s_contexts(&self, _args: &Map<String, Value>) -> Result<Value> {
        let r = scp::list_k8s_contexts();
        Ok(json!({
            "contexts": r.contexts.iter().map(|c| json!({
                "name": c.name,
                "cluster": c.cluster,
                "namespace": c.namespace,
                "user": c.user,
            })).collect::<Vec<_>>(),
            "current_context": r.current_context,
            "exists": r.exists,
            "error": r.error,
        }))
    }

    /// Namespace discovery (studio#104): backs the New Fleet wizard's
    /// namespace `<select>`.
    async fn t_list_namespaces(&self, args: &Map<String, Value>) -> Result<Value> {
        let context = args.get("context").and_then(Value::as_str);
        let namespaces = scp::list_namespaces(context).await?;
        Ok(json!({ "namespaces": namespaces }))
    }

    /// Service-account discovery (studio#104): backs the New Fleet wizard's
    /// optional service-account `<select>`. Errors (including RBAC-denied)
    /// propagate as a normal tool error — per the design, the caller treats
    /// any failure here as "leave it unset", not something to surface.
    async fn t_list_service_accounts(&self, args: &Map<String, Value>) -> Result<Value> {
        let context = args.get("context").and_then(Value::as_str);
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: namespace"))?;
        let service_accounts = scp::list_service_accounts(context, namespace).await?;
        Ok(json!({ "service_accounts": service_accounts }))
    }


    async fn t_delete(&self, args: &Map<String, Value>) -> Result<Value> {
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let resource = args
            .get("resource")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: resource"))?;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: name"))?;
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("default");
        // Guard: a fleet handle only operates its own members (no-op unscoped /
        // whole-cluster).
        let service_name = format!("oab-{namespace}-{name}");
        if !t.includes(&service_name, name) {
            anyhow::bail!("service {service_name:?} is not a member of the named fleet");
        }
        scp::delete_deployment(
            &self.aws_for_call(args, &cluster).await,
            resource,
            name,
            &cluster,
            namespace,
        )
        .await?;
        Ok(json!({ "ok": true, "resource": resource, "name": name }))
    }
}

impl ServerHandler for OabMcp {
    fn get_info(&self) -> ServerInfo {
        self.info()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: tools(),
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let empty = Map::new();
        let args = request.arguments.as_ref().unwrap_or(&empty);
        // Delegate to the transport-agnostic dispatch, then wrap: a tool's JSON
        // result becomes a success `CallToolResult`, any error (including an
        // unknown tool) becomes an `isError` result the model can read.
        Ok(match self.dispatch(request.name.as_ref(), args).await {
            Ok(v) => CallToolResult::success(vec![Content::text(
                serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()),
            )]),
            Err(e) => CallToolResult::error(vec![Content::text(format!("{e:#}"))]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_advertises_the_named_tools() {
        let catalog = serde_json::to_value(tools()).expect("tools serialize");
        let names: Vec<String> = catalog
            .as_array()
            .expect("tool list is an array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool has a name").to_string())
            .collect();
        assert_eq!(names.len(), 18);
        for expected in [
            "deploy_list",
            "deploy_get",
            "get_agent_states",
            "deploy_events",
            "k8s_logs",
            "deploy_apply",
            "deploy_provision",
            "deploy_provision_agent",
            "resolve_vendor_image_tags",
            "deploy_scale",
            "deploy_delete",
            "runtime_context",
            "fleet_config",
            "fleet_config_write",
            "list_aws_profiles",
            "list_k8s_contexts",
            "list_namespaces",
            "list_service_accounts",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn event_json_carries_normalized_fields() {
        let e = scp::EcsEvent {
            time: "2026-08-11T04:42:00Z".into(),
            detail_type: "ECS Task State Change".into(),
            service: Some("oab-prod-mira".into()),
            cluster_arn: Some("arn:aws:ecs:ap-east-2:1:cluster/oab".into()),
            last_status: Some("STOPPED".into()),
            desired_status: Some("STOPPED".into()),
            stop_code: Some("EssentialContainerExited".into()),
            reason: Some("Essential container in task exited".into()),
        };
        let v = event_json(&e);
        assert_eq!(v["type"], "ECS Task State Change");
        assert_eq!(v["service"], "oab-prod-mira");
        assert_eq!(v["last_status"], "STOPPED");
        assert_eq!(v["stop_code"], "EssentialContainerExited");
    }

    // studio#111 — `identity_binding`, the seam `aws_or` resolves a deploy
    // call's AWS identity through. Pure, so the layering rule and the
    // cluster→binding lookup are pinned without an AWS client.

    fn bindings_with_cluster_binding() -> scp::FleetBindings {
        scp::FleetBindings {
            fleets: vec![scp::FleetBinding {
                name: "prod".into(),
                runtime: scp::FleetRuntime::Ecs,
                cluster: Some("oab".into()),
                region: Some("us-east-1".into()),
                profile: Some("prod-admin".into()),
                ..Default::default()
            }],
        }
    }

    fn args(fleet: Option<&str>) -> Map<String, Value> {
        let mut m = Map::new();
        if let Some(f) = fleet {
            m.insert("fleet".to_string(), Value::String(f.to_string()));
        }
        m
    }

    #[test]
    fn identity_binding_layers_a_region_only_override_onto_the_cluster_binding() {
        // The regression: substituting the caller's fields for the binding's
        // would drop "prod-admin" and silently act as the ambient `[default]`.
        let b = identity_binding(
            &bindings_with_cluster_binding(),
            &args(None),
            "oab",
            Some("eu-west-1"),
            None,
        );
        assert_eq!(b.region.as_deref(), Some("eu-west-1"));
        assert_eq!(b.profile.as_deref(), Some("prod-admin"));
    }

    #[test]
    fn identity_binding_layers_a_profile_only_override_onto_the_cluster_binding() {
        let b = identity_binding(
            &bindings_with_cluster_binding(),
            &args(None),
            "oab",
            None,
            Some("studio-prod"),
        );
        assert_eq!(b.region.as_deref(), Some("us-east-1"));
        assert_eq!(b.profile.as_deref(), Some("studio-prod"));
    }

    #[test]
    fn identity_binding_keeps_both_when_the_caller_names_both() {
        let b = identity_binding(
            &bindings_with_cluster_binding(),
            &args(None),
            "oab",
            Some("ap-northeast-1"),
            Some("studio-prod"),
        );
        assert_eq!(b.region.as_deref(), Some("ap-northeast-1"));
        assert_eq!(b.profile.as_deref(), Some("studio-prod"));
    }

    #[test]
    fn identity_binding_uses_the_callers_answer_when_no_fleet_governs_the_cluster() {
        // The console's own shape: `[fleet.<name>]` declares no `cluster`, so
        // `for_cluster` finds nothing and the caller's pair is all there is.
        let b = identity_binding(
            &scp::FleetBindings::default(),
            &args(None),
            "oab",
            Some("ap-northeast-1"),
            Some("studio-prod"),
        );
        assert_eq!(b.region.as_deref(), Some("ap-northeast-1"));
        assert_eq!(b.profile.as_deref(), Some("studio-prod"));
    }

    #[test]
    fn identity_binding_ignores_a_binding_for_a_different_cluster() {
        let b = identity_binding(
            &bindings_with_cluster_binding(),
            &args(None),
            "other",
            Some("eu-west-1"),
            None,
        );
        assert_eq!(b.region.as_deref(), Some("eu-west-1"));
        assert_eq!(b.profile, None);
    }

    #[test]
    fn identity_binding_leaves_the_binding_untouched_when_the_caller_names_neither() {
        let b = identity_binding(
            &bindings_with_cluster_binding(),
            &args(None),
            "oab",
            None,
            None,
        );
        assert_eq!(b.region.as_deref(), Some("us-east-1"));
        assert_eq!(b.profile.as_deref(), Some("prod-admin"));
        assert_eq!(b.cluster.as_deref(), Some("oab"));
    }

    fn bindings_sharing_one_cluster_key() -> scp::FleetBindings {
        scp::FleetBindings {
            fleets: vec![
                scp::FleetBinding {
                    name: "prod".into(),
                    runtime: scp::FleetRuntime::Ecs,
                    cluster: Some("oab".into()),
                    region: Some("us-east-1".into()),
                    profile: Some("prod-admin".into()),
                    ..Default::default()
                },
                scp::FleetBinding {
                    name: "staging".into(),
                    runtime: scp::FleetRuntime::Ecs,
                    cluster: Some("oab".into()),
                    region: Some("eu-west-1".into()),
                    profile: Some("staging-admin".into()),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn base_binding_prefers_the_named_fleet_over_the_cluster_lookup() {
        // Two ecs fleets on one `cluster` key: naming the fleet is the only
        // way to tell them apart, so it has to win over `for_cluster`'s first
        // match (which would always be "prod").
        let bindings = bindings_sharing_one_cluster_key();
        let b = base_binding(&bindings, Some("staging"), "oab").expect("named fleet");
        assert_eq!(b.name, "staging");
        assert_eq!(b.profile.as_deref(), Some("staging-admin"));
    }

    #[test]
    fn identity_binding_layers_onto_the_named_fleet_not_the_cluster_one() {
        let bindings = bindings_sharing_one_cluster_key();
        let b = identity_binding(
            &bindings,
            &args(Some("staging")),
            "oab",
            Some("ap-northeast-1"),
            None,
        );
        assert_eq!(b.region.as_deref(), Some("ap-northeast-1"));
        // …the named fleet's own profile, not "prod"'s.
        assert_eq!(b.profile.as_deref(), Some("staging-admin"));
    }

    #[test]
    fn base_binding_falls_back_to_the_cluster_lookup_without_a_fleet_name() {
        let bindings = bindings_sharing_one_cluster_key();
        let b = base_binding(&bindings, None, "oab").expect("cluster binding");
        assert_eq!(b.name, "prod");
    }

    #[test]
    fn base_binding_falls_through_for_an_unknown_fleet_name() {
        // A credential lookup must never add an error the caller didn't have —
        // `named_fleet`/`target` already reject an unknown fleet by name.
        let bindings = bindings_sharing_one_cluster_key();
        let b = base_binding(&bindings, Some("ghost"), "oab").expect("cluster binding");
        assert_eq!(b.name, "prod");
    }

    #[test]
    fn base_binding_skips_a_k8s_runtime_named_fleet() {
        let bindings = scp::FleetBindings {
            fleets: vec![
                scp::FleetBinding {
                    name: "kube".into(),
                    runtime: scp::FleetRuntime::K8s,
                    context: Some("orbstack".into()),
                    ..Default::default()
                },
                scp::FleetBinding {
                    name: "prod".into(),
                    runtime: scp::FleetRuntime::Ecs,
                    cluster: Some("oab".into()),
                    profile: Some("prod-admin".into()),
                    ..Default::default()
                },
            ],
        };
        let b = base_binding(&bindings, Some("kube"), "oab").expect("ecs binding");
        assert_eq!(b.name, "prod");
    }

    #[test]
    fn has_identity_override_rejects_blanks() {
        // The guard in `aws_or` that keeps credential-less calls on the
        // memoized ambient path: a client that always sends both fields must
        // still reach it when both are empty.
        assert!(!has_identity_override(None, None));
        assert!(!has_identity_override(Some(""), Some("   ")));
        assert!(!has_identity_override(Some("\t",), None));
        assert!(has_identity_override(Some("eu-west-1"), None));
        assert!(has_identity_override(None, Some("studio-prod")));
    }

    #[test]
    fn identity_args_reads_region_and_profile_in_order() {
        // Both provision tools call this; swapping the tuple's halves would
        // provision under the profile's region instead of the named one.
        let mut args = Map::new();
        args.insert("region".to_string(), Value::String("ap-northeast-1".into()));
        args.insert("profile".to_string(), Value::String("studio-prod".into()));
        assert_eq!(
            identity_args(&args),
            (Some("ap-northeast-1"), Some("studio-prod"))
        );
    }

    #[test]
    fn identity_args_is_absent_for_a_call_that_names_neither() {
        assert_eq!(identity_args(&Map::new()), (None, None));
        // A non-string value is as absent as a missing key — `aws_or`'s
        // `has_identity_override` then takes the memoized ambient path.
        let mut args = Map::new();
        args.insert("region".to_string(), Value::Bool(true));
        assert_eq!(identity_args(&args), (None, None));
    }

    /// A handler with no ambient work left to do: the region is pinned on the
    /// loader because `ConfigLoader::load()` eagerly awaits the default region
    /// chain (env → profile file → **IMDS**), and with no `AWS_REGION` and no
    /// `~/.aws/config` that chain reaches for `169.254.169.254`, which in a
    /// sandbox *times out* rather than failing fast. Credentials stay lazy.
    async fn handler_for_test(bindings: scp::FleetBindings) -> OabMcp {
        OabMcp {
            aws: aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_config::Region::new("us-east-1"))
                .load()
                .await,
            default_cluster: "oab".into(),
            bindings: Arc::new(RwLock::new(bindings)),
            bindings_path: None,
            resolved: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn an_override_resolves_its_own_identity_without_touching_the_memo() {
        // Memoizing an override would serve one caller's credential to every
        // later call on the same fleet that names no override. The override
        // path must therefore never reach `self.resolved` at all.
        let handler = handler_for_test(scp::FleetBindings::default()).await;
        let cfg = handler.aws_or(&args(None), "oab", Some("eu-west-1"), None).await;
        assert_eq!(cfg.region().map(|r| r.to_string()).as_deref(), Some("eu-west-1"));
        assert!(
            handler.resolved.lock().unwrap().is_empty(),
            "an override must not be cached under any identity source"
        );
    }

    #[tokio::test]
    async fn a_credential_free_call_does_reach_the_memo() {
        // The other half: the early return has to hand the call to the normal
        // memoized path, or every credential-less call re-resolves.
        let handler = handler_for_test(bindings_sharing_one_cluster_key()).await;
        let cfg = handler.aws_for_call(&args(None), "oab").await;
        assert_eq!(cfg.region().map(|r| r.to_string()).as_deref(), Some("us-east-1"));
        assert!(handler.resolved.lock().unwrap().contains_key("cluster:oab"));
        // …and a second identical call is served from the memo, not resolved
        // again (the entry count can't grow).
        let before = handler.resolved.lock().unwrap().len();
        handler.aws_for_call(&args(None), "oab").await;
        assert_eq!(handler.resolved.lock().unwrap().len(), before);
    }

    #[test]
    fn both_provision_tools_advertise_the_deploy_identity_args() {
        // The only way a caller learns these exist (studio#111): a console, an
        // MCP client or an admin agent reads the catalog, not this source.
        // Dropping either property from either schema would leave every other
        // test in the repo green while the argument silently did nothing.
        let catalog = serde_json::to_value(tools()).expect("tools serialize");
        for tool in ["deploy_provision", "deploy_provision_agent"] {
            let entry = catalog
                .as_array()
                .expect("tool list is an array")
                .iter()
                .find(|t| t["name"] == tool)
                .unwrap_or_else(|| panic!("{tool} is not in the catalog"));
            for arg in ["region", "profile"] {
                assert!(
                    entry["inputSchema"]["properties"][arg].is_object(),
                    "{tool} must advertise `{arg}`"
                );
            }
            // AWS-only args on a tool whose k8s path ignores them, so they stay
            // optional — and `required` must not have grown to include them.
            let required = entry["inputSchema"]["required"]
                .as_array()
                .expect("required is an array")
                .clone();
            assert!(!required.contains(&Value::String("region".into())));
            assert!(!required.contains(&Value::String("profile".into())));
        }
    }

    #[test]
    fn memo_key_namespaces_fleet_scoped_and_cluster_scoped_credentials() {
        // The read path's half of the "right fleet's identity" guarantee: a
        // memo entry resolved for one fleet must never be served to another, or
        // to the same cluster looked up by key.
        assert_eq!(memo_key(Some("prod"), "oab"), "fleet:prod");
        assert_eq!(memo_key(None, "oab"), "cluster:oab");
        assert_ne!(
            memo_key(Some("prod"), "oab"),
            memo_key(Some("staging"), "oab")
        );
        // …and a fleet name that looks like the other namespace's contents
        // still lands in its own.
        assert_ne!(memo_key(Some("oab"), "oab"), memo_key(None, "oab"));
        assert_ne!(memo_key(Some("cluster:oab"), "oab"), memo_key(None, "oab"));
    }

    #[test]
    fn binding_and_key_returns_the_named_fleets_credential_and_its_own_key() {
        let bindings = bindings_sharing_one_cluster_key();
        let (b, key) = binding_and_key(&bindings, &args(Some("staging")), "oab");
        assert_eq!(key, "fleet:staging");
        let b = b.expect("staging resolves its own credential");
        assert_eq!(b.profile.as_deref(), Some("staging-admin"));
    }

    #[test]
    fn binding_and_key_short_circuits_a_credential_less_named_fleet_to_ambient() {
        // A binding that names no profile/region has nothing to resolve — the
        // ambient chain answers, and it is already loaded, so it must not be
        // resolved again or cached a second time under this fleet's key.
        let bindings = scp::FleetBindings {
            fleets: vec![scp::FleetBinding {
                name: "bare".into(),
                runtime: scp::FleetRuntime::Ecs,
                cluster: Some("oab".into()),
                ..Default::default()
            }],
        };
        let (b, key) = binding_and_key(&bindings, &args(Some("bare")), "oab");
        assert_eq!(key, "fleet:bare");
        assert!(b.is_none());
    }

    #[test]
    fn binding_and_key_short_circuits_when_no_fleet_governs_the_call() {
        let (b, key) = binding_and_key(&scp::FleetBindings::default(), &args(None), "oab");
        assert_eq!(key, "cluster:oab");
        assert!(b.is_none());
    }

    #[test]
    fn binding_and_key_falls_back_to_the_cluster_binding_without_a_fleet_name() {
        let bindings = bindings_sharing_one_cluster_key();
        let (b, key) = binding_and_key(&bindings, &args(None), "oab");
        assert_eq!(key, "cluster:oab");
        assert_eq!(b.expect("cluster binding").name, "prod");
    }

    #[test]
    fn binding_and_key_short_circuits_a_credential_less_cluster_binding() {
        let bindings = scp::FleetBindings {
            fleets: vec![scp::FleetBinding {
                name: "bare".into(),
                runtime: scp::FleetRuntime::Ecs,
                cluster: Some("oab".into()),
                region: None,
                profile: None,
                ..Default::default()
            }],
        };
        let (b, key) = binding_and_key(&bindings, &args(None), "oab");
        assert_eq!(key, "cluster:oab");
        assert!(b.is_none());
    }

    #[test]
    fn deployment_json_carries_counters_and_instance_states() {
        let d = scp::Deployment {
            name: "orca".into(),
            namespace: "prod".into(),
            desired: 1,
            current: 1,
            ready: 1,
            instances: vec![scp::InstancePhase {
                id: "task-arn".into(),
                phase: scp::AgentState::Running,
            }],
        };
        let v = deployment_json(&d);
        assert_eq!(v["desired"], 1);
        assert_eq!(v["ready"], 1);
        assert_eq!(v["instances"][0]["id"], "task-arn");
        assert_eq!(v["instances"][0]["state"], "Running");
    }
}
