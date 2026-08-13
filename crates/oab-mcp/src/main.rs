//! Studio control-plane MCP server (ADR-2).
//!
//! A minimal, hand-rolled `rmcp` [`ServerHandler`] (matching the openab-mcp
//! native-adapter style) that exposes the `studio-cp` read/write model as MCP
//! tools over **stdio**, so an agent operates the OAB control plane as a
//! first-class client:
//!
//! - read: `deploy_list`, `deploy_get`, `get_agent_states`, `deploy_events`,
//!   `runtime_context`, `fleet_config`
//! - write: `deploy_apply`, `deploy_scale`, `deploy_delete`, `fleet_config_write`
//!
//! Every tool is a thin dispatch into `studio-cp`; this crate owns only the
//! wire (JSON) representation and argument plumbing. Cluster defaults to
//! `$OAB_CLUSTER` (then `oab`) and is overridable per call.

use anyhow::Result;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServiceExt};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use studio_cp as scp;

/// The control-plane server. Holds the shared AWS config and the default
/// cluster; cheap to clone (one handler per session).
#[derive(Clone)]
struct OabMcp {
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

/// The read/write tool surface. Argument shapes are plain JSON Schema.
fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "deploy_list",
            "List all OAB deployments (ECS services) in the cluster with replica counts and status.",
            as_map(json!({
                "type": "object",
                "properties": {
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
                    "cluster": { "type": "string", "description": "ECS cluster (defaults to the server's configured cluster)." },
                    "namespace": { "type": "string", "description": "Namespace (default \"default\")." }
                },
                "required": ["name", "size"]
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

impl OabMcp {
    fn cluster(&self, args: &Map<String, Value>) -> String {
        args.get("cluster")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.default_cluster.clone())
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
        let cluster = self.cluster(args);
        let svcs = scp::observe_services(&self.aws_for(&cluster).await, &cluster).await?;
        let deployments: Vec<Value> = svcs
            .iter()
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
        let cluster = self.cluster(args);
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
        let cluster = self.cluster(args);
        let services: Vec<String> = match args.get("service").and_then(Value::as_str) {
            Some(s) => vec![s.to_string()],
            None => scp::observe_services(&self.aws_for(&cluster).await, &cluster)
                .await?
                .into_iter()
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
        let cluster = self.cluster(args);
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

    async fn t_apply(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.cluster(args);
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
        let cluster = self.cluster(args);
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
        let cluster = self.cluster(args);
        let aws = self.aws_for(&cluster).await;
        let ctx = scp::observe_identity(&aws).await?;
        // Snapshot the governing binding under a short read-lock (no await held).
        let (binding, expected) = {
            let guard = self.bindings.read().unwrap();
            match guard.for_cluster(&cluster) {
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
            }
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

    async fn t_delete(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.cluster(args);
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
        let mut server_info = Implementation::default();
        server_info.name = "oab-studio".into();
        server_info.version = env!("CARGO_PKG_VERSION").into();
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(INSTRUCTIONS.into());
        info
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
        let outcome = match request.name.as_ref() {
            "deploy_list" => self.t_list(args).await,
            "deploy_get" => self.t_get(args).await,
            "get_agent_states" => self.t_states(args).await,
            "deploy_events" => self.t_events(args).await,
            "deploy_apply" => self.t_apply(args).await,
            "deploy_scale" => self.t_scale(args).await,
            "deploy_delete" => self.t_delete(args).await,
            "runtime_context" => self.t_runtime_context(args).await,
            "fleet_config" => self.t_fleet_config(args),
            "fleet_config_write" => self.t_fleet_write(args),
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown tool {other:?}"),
                    None,
                ))
            }
        };
        Ok(match outcome {
            Ok(v) => CallToolResult::success(vec![Content::text(
                serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()),
            )]),
            Err(e) => CallToolResult::error(vec![Content::text(format!("{e:#}"))]),
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let default_cluster = std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string());
    // Fleet → managing-credential bindings are strictly opt-in: a missing file
    // yields an empty set. Warn on stderr only (stdout is the MCP JSON-RPC wire).
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
    let server = OabMcp {
        aws,
        default_cluster,
        bindings: Arc::new(RwLock::new(bindings)),
        bindings_path: bindings_path.map(|p| p.display().to_string()),
        resolved: Arc::new(Mutex::new(HashMap::new())),
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
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
        assert_eq!(names.len(), 10);
        for expected in [
            "deploy_list",
            "deploy_get",
            "get_agent_states",
            "deploy_events",
            "deploy_apply",
            "deploy_scale",
            "deploy_delete",
            "runtime_context",
            "fleet_config",
            "fleet_config_write",
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
