//! Library status API — the **data half** of `oabctl get`, exposed so downstream
//! consumers (Studio) can map OAB service status onto their own model instead of
//! parsing CLI table output. This is the seam by which oabctl exposes live
//! deployment status to Studio.
//!
//! Service-level only (ECS `DescribeServices`): running/desired counts + the ECS
//! service status string. Per-*instance* lifecycle (the canonical 6-state model)
//! is derived downstream from per-task observation; it is intentionally not
//! computed here.

use anyhow::{Context, Result};

/// Structured status of one OAB ECS service (the data behind `oabctl get`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// Agent name (the `{name}` in `oab-{namespace}-{name}`).
    pub name: String,
    pub namespace: String,
    /// Full ECS service name exactly as ECS returned it (`oab-{namespace}-{name}`).
    /// Carried verbatim so downstream ECS calls (`ListTasks`) query by the
    /// authoritative name instead of rebuilding it from `namespace`/`name` —
    /// rebuilding is wrong for any service that doesn't fit the `oab-<ns>-<name>`
    /// shape (where the parser falls back to `namespace = "?"`).
    pub service_name: String,
    pub cpu: String,
    pub memory: String,
    pub capacity: String,
    pub running: i32,
    pub desired: i32,
    /// ECS service status string (`ACTIVE` / `DRAINING` / `INACTIVE` / `UNKNOWN`).
    pub status: String,
}

/// List every `oab-` ECS service in `cluster` with its live status.
///
/// ```no_run
/// # async fn demo() -> anyhow::Result<()> {
/// let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
/// let services = oabctl::service_status(&aws, "oab").await?;
/// for s in services {
///     println!("{}/{} {}/{} {}", s.namespace, s.name, s.running, s.desired, s.status);
/// }
/// # Ok(())
/// # }
/// ```
pub async fn service_status(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
) -> Result<Vec<ServiceStatus>> {
    let ecs = aws_sdk_ecs::Client::new(aws_config);

    // List all oab- service ARNs (paginated).
    let mut service_arns = Vec::new();
    let mut next_token = None;
    loop {
        let mut req = ecs.list_services().cluster(cluster);
        if let Some(token) = &next_token {
            req = req.next_token(token);
        }
        let resp = req.send().await.context("failed to list ECS services")?;
        for arn in resp.service_arns() {
            if arn.contains("/oab-") {
                service_arns.push(arn.to_string());
            }
        }
        next_token = resp.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    let mut out = Vec::new();
    for chunk in service_arns.chunks(10) {
        let resp = ecs
            .describe_services()
            .cluster(cluster)
            .set_services(Some(chunk.to_vec()))
            .send()
            .await
            .context("failed to describe ECS services")?;

        for svc in resp.services() {
            let svc_name = svc.service_name().unwrap_or("-");
            // Parse oab-{namespace}-{name}.
            let parts: Vec<&str> = svc_name.splitn(3, '-').collect();
            let (namespace, agent_name) = if parts.len() == 3 {
                (parts[1].to_string(), parts[2].to_string())
            } else {
                ("?".to_string(), svc_name.to_string())
            };

            let (cpu, memory) = if let Some(td_arn) = svc.task_definition() {
                match ecs
                    .describe_task_definition()
                    .task_definition(td_arn)
                    .send()
                    .await
                {
                    Ok(td) => {
                        let td = td.task_definition();
                        (
                            td.and_then(|t| t.cpu()).unwrap_or("-").to_string(),
                            td.and_then(|t| t.memory()).unwrap_or("-").to_string(),
                        )
                    }
                    Err(_) => ("-".to_string(), "-".to_string()),
                }
            } else {
                ("-".to_string(), "-".to_string())
            };

            let capacity = svc
                .capacity_provider_strategy()
                .first()
                .map(|c| c.capacity_provider().to_string())
                .unwrap_or_else(|| "FARGATE".to_string());

            out.push(ServiceStatus {
                name: agent_name,
                namespace,
                service_name: svc_name.to_string(),
                cpu,
                memory,
                capacity,
                running: svc.running_count(),
                desired: svc.desired_count(),
                status: svc.status().unwrap_or("UNKNOWN").to_string(),
            });
        }
    }

    Ok(out)
}

/// Per-Instance (ECS task) observation — the granularity ADR-1's four
/// discriminators need (`DescribeServices` alone is service-level and cannot
/// yield these). See ADR-2 §7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceStatus {
    /// Task ARN.
    pub id: String,
    /// ECS `lastStatus`: PROVISIONING/PENDING/ACTIVATING/RUNNING/DEACTIVATING/STOPPING/STOPPED.
    pub last_status: String,
    /// ECS container `healthStatus`: HEALTHY/UNHEALTHY/UNKNOWN.
    pub health_status: String,
    /// Whether the task definition declares a container health check. Lets the
    /// read model tell "no probe" (UNKNOWN forever, benign) apart from a defined
    /// check whose signal is UNKNOWN (lost). Best-effort: `false` if the task
    /// definition can't be read.
    pub health_check_defined: bool,
    /// `desiredStatus == STOPPED`.
    pub desired_stopped: bool,
    /// ECS `stopCode` when stopped (else `None`).
    pub stop_code: Option<String>,
}

/// List the tasks (Instances) of one service with per-task status.
///
/// Aggregates `ListTasks` + `DescribeTasks`; the caller maps these onto the
/// canonical model (Studio does this in `studio-cp`).
pub async fn instance_status(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
    service: &str,
) -> Result<Vec<InstanceStatus>> {
    // ECS `ListTasks` filters by the FULL service name (`oab-{ns}-{name}`); a
    // display short name (`orca`) silently 404s as `ServiceNotFoundException`.
    // Fail loud at the boundary so the mistake is unambiguous rather than an
    // opaque AWS error — callers resolve the full name in
    // `studio_cp::observe_deployment` before reaching here.
    if !service.starts_with("oab-") {
        anyhow::bail!(
            "instance_status: expected full ECS service name `oab-<ns>-<name>`, got `{service}` \
             — a short/display name never matches an ECS service_name filter"
        );
    }

    let ecs = aws_sdk_ecs::Client::new(aws_config);

    // List task ARNs for the service (paginated).
    let mut task_arns = Vec::new();
    let mut next_token = None;
    loop {
        let mut req = ecs.list_tasks().cluster(cluster).service_name(service);
        if let Some(t) = &next_token {
            req = req.next_token(t);
        }
        let resp = req.send().await.context("failed to list ECS tasks")?;
        for arn in resp.task_arns() {
            task_arns.push(arn.to_string());
        }
        next_token = resp.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    // Whether a task definition declares a container health check, cached by ARN
    // so a service's tasks (which share a task def) cost one DescribeTaskDefinition.
    let mut hc_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    let mut out = Vec::new();
    for chunk in task_arns.chunks(100) {
        let resp = ecs
            .describe_tasks()
            .cluster(cluster)
            .set_tasks(Some(chunk.to_vec()))
            .send()
            .await
            .context("failed to describe ECS tasks")?;
        for task in resp.tasks() {
            let health_check_defined = match task.task_definition_arn() {
                Some(arn) => task_def_has_health_check(&ecs, arn, &mut hc_cache).await,
                None => false,
            };
            out.push(InstanceStatus {
                id: task.task_arn().unwrap_or("-").to_string(),
                last_status: task.last_status().unwrap_or("UNKNOWN").to_string(),
                health_status: task
                    .health_status()
                    .map(|h| h.as_str().to_string())
                    .unwrap_or_else(|| "UNKNOWN".to_string()),
                health_check_defined,
                desired_stopped: task.desired_status() == Some("STOPPED"),
                stop_code: task.stop_code().map(|c| c.as_str().to_string()),
            });
        }
    }

    Ok(out)
}

/// Does this task definition declare a container health check? Cached by ARN.
/// Best-effort: on any read error (e.g. missing `ecs:DescribeTaskDefinition`),
/// returns `false` so an UNKNOWN health reads as benign (no probe) rather than
/// faulting the instance on a permission gap.
async fn task_def_has_health_check(
    ecs: &aws_sdk_ecs::Client,
    arn: &str,
    cache: &mut std::collections::HashMap<String, bool>,
) -> bool {
    if let Some(v) = cache.get(arn) {
        return *v;
    }
    let defined = match ecs.describe_task_definition().task_definition(arn).send().await {
        Ok(resp) => resp
            .task_definition()
            .map(|td| {
                td.container_definitions()
                    .iter()
                    .any(|c| c.health_check().is_some())
            })
            .unwrap_or(false),
        Err(_) => false,
    };
    cache.insert(arn.to_string(), defined);
    defined
}
