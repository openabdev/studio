//! Studio control-plane MCP handler (ADR-2).
//!
//! A minimal, hand-rolled `rmcp` [`ServerHandler`] (matching the openab-mcp
//! native-adapter style) that exposes the `studio-cp` read/write model as MCP
//! tools:
//!
//! - read: `deploy_list`, `deploy_get`, `get_agent_states`, `deploy_events`,
//!   `runtime_context`, `fleet_config`
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
    /// Per-cluster resolved configs, memoized so a binding is resolved once.
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
            "List all OAB deployments (ECS services) in the cluster with replica counts and status.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." }
                }
            })),
        ),
        Tool::new(
            "deploy_get",
            "Get one deployment's read-model: replica counters plus each instance's canonical lifecycle phase (6-state).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string", "description": "ECS service name (or bare agent name)." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential and, for listing tools, restricts results to its members. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." }
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
            "Scale an OAB service on or off. OAB services run a single bot token, so size must be 0 (off) or 1 (on).",
            as_map(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Agent / service name (service = oab-{namespace}-{name})." },
                    "size": { "type": "integer", "enum": [0, 1], "description": "0 = off, 1 = on." },
                    "fleet": { "type": "string", "description": "Fleet name (see fleet_config): targets the fleet's cluster and managing credential; a write to a service outside the fleet's members is refused. Overrides the cluster arg." },
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\")." }
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
                    "expected_principal": { "type": "string", "description": "k8s only, optional. `system:serviceaccount:<namespace>:<name>` to set the pod's service account; unset uses the namespace's default." }
                },
                "required": ["library", "template", "name"]
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
            "Show the effective runtime identity/context this control plane resolved for a cluster/fleet: the acting principal (STS caller ARN), its kind (role vs static user), account (scope), region (location), a best-effort credential-source hint, the fleet binding in effect (if any), and — when the binding declares an expected_principal — whether the resolved identity matches it (identity_matches; a non-blocking IdentityMismatch when false). Read-only; answers \"who am I acting as, against what account?\" and surfaces silent credential fallback.",
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
            "List the configured per-fleet managing-credential bindings (ADR: Per-Fleet managing identity): each fleet's name, target cluster, AWS profile and region, and expected_principal, plus the config file path they load from and the server's default cluster. Read-only; the declarative 'declare' side of the config→switch→observe→reconcile loop. Profiles are names, not secrets.",
            as_map(json!({
                "type": "object",
                "properties": {}
            })),
        ),
        Tool::new(
            "fleet_config_write",
            "Persist the whole fleet-binding config from raw TOML `text` (what the UI's editor holds), then hot-reload. Validates the text parses before writing — a bad edit never lands on disk — and the bytes are stored verbatim, so comments/layout are preserved. Returns the updated fleet_config. Write tool: overwrites the operator's fleets.toml.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Full TOML document for fleets.toml (a list of [[fleet]] tables)." }
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
        Tool::new(
            "k8s_fleet_config",
            "List the configured k8s fleet bindings (fleets-k8s.toml, separate from AWS's fleets.toml): each fleet's name, kubeconfig context, namespace, members, and expected_principal, plus the config file path and raw TOML text. Read-only; the k8s counterpart to `fleet_config` — used to read the current file before computing an appended block, since `k8s_fleet_config_write` takes the whole file's text with no partial/append primitive.",
            as_map(json!({
                "type": "object",
                "properties": {}
            })),
        ),
        Tool::new(
            "k8s_fleet_config_write",
            "Persist the whole k8s fleet-binding config (fleets-k8s.toml, separate from AWS's fleets.toml) from raw TOML `text`. Validates the text parses before writing — a bad edit never lands on disk — and the bytes are stored verbatim, so comments/layout are preserved. Returns the parsed fleets (name, context, namespace, members, expected_principal) plus the raw text. Write tool: overwrites the operator's fleets-k8s.toml.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Full TOML document for fleets-k8s.toml (a list of [fleet.<name>] tables)." }
                },
                "required": ["text"]
            })),
        ),
    ]
}

fn k8s_fleets_json(bindings: &scp::K8sFleetBindings) -> Vec<Value> {
    bindings
        .fleets
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "context": b.context,
                "namespace": b.namespace,
                "members": b.members,
                "expected_principal": b.expected_principal,
            })
        })
        .collect()
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
            "deploy_apply" => self.t_apply(args).await,
            "deploy_provision" => self.t_provision(args).await,
            "deploy_scale" => self.t_scale(args).await,
            "deploy_delete" => self.t_delete(args).await,
            "runtime_context" => self.t_runtime_context(args).await,
            "fleet_config" => self.t_fleet_config(args),
            "fleet_config_write" => self.t_fleet_write(args),
            "list_aws_profiles" => self.t_list_aws_profiles(args),
            "list_k8s_contexts" => self.t_list_k8s_contexts(args),
            "list_namespaces" => self.t_list_namespaces(args).await,
            "list_service_accounts" => self.t_list_service_accounts(args).await,
            "k8s_fleet_config" => self.t_k8s_fleet_config(args),
            "k8s_fleet_config_write" => self.t_k8s_fleet_write(args),
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
            return Ok(Target {
                cluster: binding.cluster.clone(),
                binding: Some(binding),
            });
        }
        Ok(Target {
            cluster: self.cluster(args),
            binding: None,
        })
    }

    /// The AWS config to act as for `cluster`: the fleet binding's credential
    /// when one governs it (resolved once, then memoized), else the default
    /// chain. This is where the per-fleet **switch** takes effect — a bound
    /// cluster's calls run under its credential, not whatever ambient
    /// `[default]` the chain resolves first.
    async fn aws_for(&self, cluster: &str) -> aws_config::SdkConfig {
        // Short read-lock: clone the governing binding, then drop the guard
        // before any await (never hold a std lock across .await).
        let binding = {
            let guard = self.bindings.read().unwrap();
            match guard.for_cluster(cluster) {
                Some(b) if b.profile.is_some() || b.region.is_some() => b.clone(),
                _ => return self.aws.clone(),
            }
        };
        if let Some(cfg) = self.resolved.lock().unwrap().get(cluster) {
            return cfg.clone();
        }
        let cfg = scp::resolve_binding_config(&binding).await;
        self.resolved
            .lock()
            .unwrap()
            .insert(cluster.to_string(), cfg.clone());
        cfg
    }

    async fn t_list(&self, args: &Map<String, Value>) -> Result<Value> {
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let svcs = scp::observe_services(&self.aws_for(&cluster).await, &cluster).await?;
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
        let cluster = self.target(args)?.cluster;
        let service = args
            .get("service")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: service"))?;
        match scp::observe_deployment(&self.aws_for(&cluster).await, &cluster, service).await? {
            Some(d) => Ok(deployment_json(&d)),
            None => Ok(json!({ "found": false, "service": service })),
        }
    }

    async fn t_states(&self, args: &Map<String, Value>) -> Result<Value> {
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let services: Vec<String> = match args.get("service").and_then(Value::as_str) {
            Some(s) => vec![s.to_string()],
            None => scp::observe_services(&self.aws_for(&cluster).await, &cluster)
                .await?
                .into_iter()
                .filter(|s| t.includes(&s.service_name, &s.name))
                .map(|s| s.name)
                .collect(),
        };
        let mut instances = Vec::new();
        for svc in services {
            if let Some(d) =
                scp::observe_deployment(&self.aws_for(&cluster).await, &cluster, &svc).await?
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
            &self.aws_for(&cluster).await,
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
        let library: scp::Library = serde_json::from_value(
            args.get("library")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing required arg: library"))?,
        )
        .map_err(|e| anyhow::anyhow!("invalid library: {e}"))?;

        // studio#104: k8s dispatch. `context`/`expected_principal` come as
        // direct args — the console's identity form already collects them
        // (context/namespace/service-account <select>s, #108/#109), there's
        // no existing K8sFleetBinding to resolve them from for a brand-new
        // fleet (unlike AWS's `fleet` + `self.target()` — a fleet-scoped k8s
        // lookup is future work for redeploys into an *existing* k8s fleet,
        // not needed for this dispatch to exist).
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
            &self.aws_for(&cluster).await,
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

    async fn t_apply(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.target(args)?.cluster;
        let manifest = args
            .get("manifest_yaml")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: manifest_yaml"))?;
        let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let report =
            scp::apply_deployment(&self.aws_for(&cluster).await, manifest, &cluster, wait).await?;
        Ok(json!({ "ok": true, "services_applied": report.services.len() }))
    }

    async fn t_scale(&self, args: &Map<String, Value>) -> Result<Value> {
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
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
        // Guard: a fleet handle only operates its own members — refuse to scale a
        // service outside the named fleet (a no-op for unscoped or whole-cluster
        // calls). Stops a fleet-scoped call from reaching a co-located non-member.
        let service_name = format!("oab-{namespace}-{name}");
        if !t.includes(&service_name, name) {
            anyhow::bail!("service {service_name:?} is not a member of the named fleet");
        }
        scp::scale_deployment(
            &self.aws_for(&cluster).await,
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
        let t = self.target(args)?;
        let cluster = t.cluster.clone();
        let aws = self.aws_for(&cluster).await;
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
                    "cluster": b.cluster,
                    "members": b.members,
                    "region": b.region,
                    "profile": b.profile,
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
    /// per-cluster configs (a changed profile/region invalidates them). Returns
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

    /// The declarative k8s fleet-binding config, read-only — the k8s
    /// counterpart to `t_fleet_config`. Unlike AWS bindings, k8s bindings
    /// aren't cached anywhere in `OabMcp`, so this loads fresh from disk each
    /// call. Exists so a caller (the New Fleet wizard's k8s submit path) can
    /// read the current `fleets-k8s.toml` text before computing an appended
    /// block — `k8s_fleet_config_write` takes the whole file, no partial/
    /// append primitive.
    fn t_k8s_fleet_config(&self, _args: &Map<String, Value>) -> Result<Value> {
        let path = scp::default_k8s_bindings_path();
        let bindings = match &path {
            Some(p) => scp::load_k8s_bindings(p).unwrap_or_default(),
            None => scp::K8sFleetBindings::default(),
        };
        let text = match &path {
            Some(p) => scp::read_bindings_text(p).unwrap_or_default(),
            None => String::new(),
        };
        Ok(json!({
            "path": path.map(|p| p.display().to_string()),
            "fleets": k8s_fleets_json(&bindings),
            "text": text,
        }))
    }

    /// Write tool: persist the whole `fleets-k8s.toml` from the editor's
    /// `text` after validating it parses, mirroring `t_fleet_write`'s
    /// AWS-side shape. Unlike AWS bindings, k8s bindings aren't cached
    /// anywhere in `OabMcp`, so this is a plain validate-then-write with no
    /// in-memory state to invalidate.
    fn t_k8s_fleet_write(&self, args: &Map<String, Value>) -> Result<Value> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: text"))?;
        let path = scp::default_k8s_bindings_path()
            .ok_or_else(|| anyhow::anyhow!("no k8s fleet config path resolved; cannot write bindings"))?;
        let bindings = scp::save_k8s_bindings_text(&path, text)?;
        Ok(json!({
            "path": path.display().to_string(),
            "fleets": k8s_fleets_json(&bindings),
            "text": text,
        }))
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
            &self.aws_for(&cluster).await,
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
        assert_eq!(names.len(), 17);
        for expected in [
            "deploy_list",
            "deploy_get",
            "get_agent_states",
            "deploy_events",
            "deploy_apply",
            "deploy_provision",
            "deploy_scale",
            "deploy_delete",
            "runtime_context",
            "fleet_config",
            "fleet_config_write",
            "list_aws_profiles",
            "list_k8s_contexts",
            "list_namespaces",
            "list_service_accounts",
            "k8s_fleet_config",
            "k8s_fleet_config_write",
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
