//! Studio control-plane library.
//!
//! This crate is the **seam** between the two halves of PR #2:
//!
//! - [`oabctl`] (vendored) is the management + status engine. It exposes live
//!   deployment status as data via [`oabctl::service_status`].
//! - [`agent_lifecycle`] is the canonical instance-lifecycle vocabulary (the
//!   6-state model + 4-axis discriminator).
//!
//! Studio consumes oabctl here and — in the next slice — maps per-task
//! observation onto [`AgentState`]. Every Studio front-end (CLI / TUI / GUI, and
//! an MCP surface later) is a downstream client of this crate, so oabctl stays
//! clean and upstream-contributable.

pub use agent_lifecycle::AgentState;
pub use oabctl::ServiceStatus;

/// Observe all OAB services in `cluster` — a thin passthrough over oabctl's
/// library status API.
///
/// This is the entry point the 6-state mapping and the MCP surface build on.
/// Per-instance [`AgentState`] derivation needs per-task observation
/// (`DescribeTasks`) and lands in the next slice; today this returns the
/// service-level [`ServiceStatus`] oabctl already produces.
pub async fn observe_services(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
) -> anyhow::Result<Vec<ServiceStatus>> {
    oabctl::service_status(aws_config, cluster).await
}

pub use oabctl::InstanceStatus;

/// Map an oabctl ECS [`InstanceStatus`] onto the canonical [`AgentState`]
/// (ADR-2 read model: `DescribeTasks` → 4 discriminators → `phase`).
///
/// Only the **ECS-observable** axes are derived here: `last_status` (drives the
/// `identity_verified` latch and desired) and `health_status`. Admission
/// (`accepting_work`) and the CP lease are **app/CP-level**, not ECS-observable
/// (ADR-1 F2 / ADR-2 N1), so they default to admitting / valid; the control
/// plane overrides them.
pub fn instance_phase(inst: &InstanceStatus, verified_before: bool) -> AgentState {
    use agent_lifecycle::ecs::{EcsDriver, EcsHealth, EcsLastStatus, EcsTask};
    use agent_lifecycle::RuntimeDriver;

    let last_status = match inst.last_status.as_str() {
        "PROVISIONING" => EcsLastStatus::Provisioning,
        "PENDING" => EcsLastStatus::Pending,
        "ACTIVATING" => EcsLastStatus::Activating,
        "RUNNING" => EcsLastStatus::Running,
        "DEACTIVATING" => EcsLastStatus::Deactivating,
        "STOPPING" => EcsLastStatus::Stopping,
        _ => EcsLastStatus::Stopped,
    };
    let health = match inst.health_status.as_str() {
        "HEALTHY" => EcsHealth::Healthy,
        "UNHEALTHY" => EcsHealth::Unhealthy,
        _ => EcsHealth::Unknown,
    };
    let task = EcsTask {
        last_status,
        desired_status_stopped: inst.desired_stopped,
        health,
        lease_valid: true,    // CP-level, not ECS-observable
        accepting_work: true, // CP-level admission, not ECS-observable
    };
    EcsDriver.project(&task, verified_before).classify()
}

/// One Instance's identity + phase within a Deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePhase {
    pub id: String,
    pub phase: AgentState,
}

/// The generic Deployment read-model (ADR-2 §4): Deployment-level replica
/// **counters** + per-Instance `phase`. A Deployment has *counts*, **not** an
/// `AgentState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub name: String,
    pub namespace: String,
    /// Desired replica count.
    pub desired: i32,
    /// Instances currently observed.
    pub current: i32,
    /// Instances whose `phase` is `Running`.
    pub ready: i32,
    pub instances: Vec<InstancePhase>,
}

/// One-shot approximation of the `identity_verified` latch from the current
/// `last_status`. The real latch needs CP-persisted history; here an Instance
/// counts as verified once its ECS `lastStatus` is at or past `RUNNING`.
fn latched_verified(last_status: &str) -> bool {
    matches!(last_status, "RUNNING" | "DEACTIVATING" | "STOPPING")
}

/// Build the Deployment read-model from service-level + per-Instance status.
pub fn build_deployment(svc: &ServiceStatus, instances: &[InstanceStatus]) -> Deployment {
    let instances: Vec<InstancePhase> = instances
        .iter()
        .map(|i| InstancePhase {
            id: i.id.clone(),
            phase: instance_phase(i, latched_verified(&i.last_status)),
        })
        .collect();
    let ready = instances
        .iter()
        .filter(|p| p.phase == AgentState::Running)
        .count() as i32;
    Deployment {
        name: svc.name.clone(),
        namespace: svc.namespace.clone(),
        desired: svc.desired,
        current: instances.len() as i32,
        ready,
        instances,
    }
}

/// Observe one Deployment end-to-end: service-level counters + per-Instance
/// phases. `service` is the ECS service name (`oab-{namespace}-{name}`).
pub async fn observe_deployment(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
    service: &str,
) -> anyhow::Result<Option<Deployment>> {
    let svc = oabctl::service_status(aws_config, cluster)
        .await?
        .into_iter()
        .find(|s| service == format!("oab-{}-{}", s.namespace, s.name) || service == s.name);
    let Some(svc) = svc else { return Ok(None) };
    let instances = oabctl::instance_status(aws_config, cluster, service).await?;
    Ok(Some(build_deployment(&svc, &instances)))
}

// ---- Write side (ADR-2 write model) ------------------------------------
//
// The read side above observes; these mutate. Each is a thin passthrough to
// oabctl's programmatic `studio_api` — Studio owns the vocabulary and the MCP
// surface, oabctl owns the AWS reconciliation.

pub use oabctl::{ApplyOptions, ApplyReport};

/// Apply one or more service manifests (create/update).
///
/// Parses the YAML document into manifests, then runs oabctl's **programmatic**
/// apply (no stdout/stderr side effects) and returns the structured
/// [`ApplyReport`]. An `OABFleet` document applies every expanded service.
pub async fn apply_deployment(
    aws_config: &aws_config::SdkConfig,
    manifest_yaml: &str,
    cluster: &str,
    wait: bool,
) -> anyhow::Result<ApplyReport> {
    let manifests = oabctl::studio_api::parse_manifests(manifest_yaml)?;
    let opts = ApplyOptions::new(cluster).with_wait(wait);
    oabctl::apply_manifests(aws_config, &manifests, &opts)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Scale an agent/service to `size` replicas.
pub async fn scale_deployment(
    aws_config: &aws_config::SdkConfig,
    alias: &str,
    size: i32,
) -> anyhow::Result<()> {
    oabctl::studio_api::scale(aws_config, alias, size).await
}

/// Delete a control-plane resource (e.g. an `OABService`).
pub async fn delete_deployment(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    oabctl::studio_api::delete(aws_config, resource, name, cluster, namespace).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(last: &str, health: &str, stopped: bool) -> InstanceStatus {
        InstanceStatus {
            id: "arn".into(),
            last_status: last.into(),
            health_status: health.into(),
            desired_stopped: stopped,
            stop_code: None,
        }
    }

    #[test]
    fn activating_maps_to_starting() {
        assert_eq!(
            instance_phase(&inst("ACTIVATING", "UNKNOWN", false), false),
            AgentState::Starting
        );
    }

    #[test]
    fn running_healthy_maps_to_running() {
        assert_eq!(
            instance_phase(&inst("RUNNING", "HEALTHY", false), true),
            AgentState::Running
        );
    }

    #[test]
    fn running_unhealthy_after_verified_maps_to_unhealthy() {
        assert_eq!(
            instance_phase(&inst("RUNNING", "UNHEALTHY", false), true),
            AgentState::Unhealthy
        );
    }

    #[test]
    fn desired_stopped_maps_to_stopping() {
        assert_eq!(
            instance_phase(&inst("DEACTIVATING", "HEALTHY", true), true),
            AgentState::Stopping
        );
    }

    fn svc(desired: i32, running: i32) -> ServiceStatus {
        ServiceStatus {
            name: "orca".into(),
            namespace: "prod".into(),
            cpu: "512".into(),
            memory: "1024".into(),
            capacity: "FARGATE".into(),
            running,
            desired,
            status: "ACTIVE".into(),
        }
    }

    #[test]
    fn build_deployment_counts_ready_and_phases() {
        let insts = vec![
            inst("RUNNING", "HEALTHY", false),
            inst("ACTIVATING", "UNKNOWN", false),
        ];
        let d = build_deployment(&svc(2, 1), &insts);
        assert_eq!(d.desired, 2);
        assert_eq!(d.current, 2);
        assert_eq!(d.ready, 1); // one Running, one Starting
        assert_eq!(d.instances[0].phase, AgentState::Running);
        assert_eq!(d.instances[1].phase, AgentState::Starting);
    }
}
