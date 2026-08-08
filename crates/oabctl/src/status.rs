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
