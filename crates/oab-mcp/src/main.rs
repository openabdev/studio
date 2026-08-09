//! Studio control-plane MCP server (ADR-2).
//!
//! A minimal, hand-rolled `rmcp` [`ServerHandler`] (matching the openab-mcp
//! native-adapter style) that exposes the `studio-cp` read/write model as MCP
//! tools over **stdio**, so an agent operates the OAB control plane as a
//! first-class client:
//!
//! - read: `deploy_list`, `deploy_get`, `get_agent_states`
//! - write: `deploy_apply`, `deploy_scale`, `deploy_delete`
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
use std::sync::Arc;
use studio_cp as scp;

/// The control-plane server. Holds the shared AWS config and the default
/// cluster; cheap to clone (one handler per session).
#[derive(Clone)]
struct OabMcp {
    aws: aws_config::SdkConfig,
    default_cluster: String,
}

fn as_map(v: Value) -> Arc<Map<String, Value>> {
    Arc::new(v.as_object().expect("schema literal is an object").clone())
}

const INSTRUCTIONS: &str = "OAB Studio control plane. Observe deployments and \
per-instance lifecycle states (6-state model), and drive writes (apply a \
manifest, scale, delete). Reads are safe; writes mutate live ECS services.";

/// The six-tool read/write surface. Argument shapes are plain JSON Schema.
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
            "Scale an agent/service to a target replica count.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "alias": { "type": "string", "description": "Agent alias / service name." },
                    "size": { "type": "integer", "description": "Desired replica count." }
                },
                "required": ["alias", "size"]
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

impl OabMcp {
    fn cluster(&self, args: &Map<String, Value>) -> String {
        args.get("cluster")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.default_cluster.clone())
    }

    async fn t_list(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.cluster(args);
        let svcs = scp::observe_services(&self.aws, &cluster).await?;
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
        match scp::observe_deployment(&self.aws, &cluster, service).await? {
            Some(d) => Ok(deployment_json(&d)),
            None => Ok(json!({ "found": false, "service": service })),
        }
    }

    async fn t_states(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.cluster(args);
        let services: Vec<String> = match args.get("service").and_then(Value::as_str) {
            Some(s) => vec![s.to_string()],
            None => scp::observe_services(&self.aws, &cluster)
                .await?
                .into_iter()
                .map(|s| s.name)
                .collect(),
        };
        let mut instances = Vec::new();
        for svc in services {
            if let Some(d) = scp::observe_deployment(&self.aws, &cluster, &svc).await? {
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

    async fn t_apply(&self, args: &Map<String, Value>) -> Result<Value> {
        let cluster = self.cluster(args);
        let manifest = args
            .get("manifest_yaml")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: manifest_yaml"))?;
        let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let report = scp::apply_deployment(&self.aws, manifest, &cluster, wait).await?;
        Ok(json!({ "ok": true, "services_applied": report.services.len() }))
    }

    async fn t_scale(&self, args: &Map<String, Value>) -> Result<Value> {
        let alias = args
            .get("alias")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing required arg: alias"))?;
        let size =
            args.get("size")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("missing or invalid arg: size"))? as i32;
        scp::scale_deployment(&self.aws, alias, size).await?;
        Ok(json!({ "ok": true, "alias": alias, "size": size }))
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
        scp::delete_deployment(&self.aws, resource, name, &cluster, namespace).await?;
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
            "deploy_apply" => self.t_apply(args).await,
            "deploy_scale" => self.t_scale(args).await,
            "deploy_delete" => self.t_delete(args).await,
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
    let server = OabMcp {
        aws,
        default_cluster,
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
