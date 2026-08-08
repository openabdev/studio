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
}
