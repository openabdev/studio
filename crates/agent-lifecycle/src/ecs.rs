//! ECS runtime driver — projects ECS task signals onto the canonical model.
//!
//! Mapping (ADR §6): `lastStatus` ever RUNNING ⇒ `identity_verified`;
//! `desiredStatus == STOPPED` ⇒ `DesiredStatus::Stopped`; `healthStatus` + lease
//! ⇒ `Health`; CP/director cordon ⇒ `accepting_work`.

use crate::{DesiredStatus, Discriminator, Health, RuntimeDriver};

/// ECS `lastStatus` values relevant to the projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcsLastStatus {
    Provisioning,
    Pending,
    Activating,
    Running,
    Deactivating,
    Stopping,
    Stopped,
}

/// ECS container `healthStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcsHealth {
    Healthy,
    Unhealthy,
    Unknown,
}

/// A single ECS task observation (the subset the projection needs).
#[derive(Debug, Clone, Copy)]
pub struct EcsTask {
    pub last_status: EcsLastStatus,
    /// ECS `desiredStatus == STOPPED`.
    pub desired_status_stopped: bool,
    pub health: EcsHealth,
    /// Whether the task definition declares a container health check. When it
    /// does not, ECS reports `healthStatus = UNKNOWN` forever — that is "no
    /// probe", not a fault — so an UNKNOWN with no check must not read as
    /// Unhealthy. A defined check reporting UNKNOWN is a lost signal (fault).
    pub health_check_defined: bool,
    /// CP-issued lease still valid (heartbeat authorized).
    pub lease_valid: bool,
    /// CP/director cordon: `false` ⇒ not admitting new work (→ Paused).
    pub accepting_work: bool,
}

/// Projects ECS task state onto the canonical lifecycle model.
pub struct EcsDriver;

impl RuntimeDriver for EcsDriver {
    type Native = EcsTask;
    /// Task ARN.
    type InstanceId = String;

    fn project(&self, task: &EcsTask, verified_before: bool) -> Discriminator {
        let desired_status = if task.desired_status_stopped {
            DesiredStatus::Stopped
        } else {
            DesiredStatus::Running
        };

        // `identity_verified` latches once `lastStatus` has ever reached RUNNING.
        let identity_verified = verified_before || task.last_status == EcsLastStatus::Running;

        // Healthy (with a valid lease) is a clean Ok. UNHEALTHY, a lost lease,
        // or a UNKNOWN from a *defined* health check (lost signal / node lost)
        // all fault. But when NO health check is defined, ECS reports UNKNOWN
        // forever — that is "no probe", not a fault — so a running, leased
        // instance stays Ok instead of reading as Unhealthy.
        let health = match task.health {
            EcsHealth::Healthy if task.lease_valid => Health::Ok,
            EcsHealth::Unknown if task.lease_valid && !task.health_check_defined => Health::Ok,
            _ => Health::Faulted,
        };

        Discriminator {
            desired_status,
            accepting_work: task.accepting_work,
            health,
            identity_verified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentState;

    fn task(
        ls: EcsLastStatus,
        stopped: bool,
        h: EcsHealth,
        lease: bool,
        accepting: bool,
    ) -> EcsTask {
        // Default: no health check defined (the common case for OAB agents).
        task_hc(ls, stopped, h, lease, accepting, false)
    }

    fn task_hc(
        ls: EcsLastStatus,
        stopped: bool,
        h: EcsHealth,
        lease: bool,
        accepting: bool,
        health_check_defined: bool,
    ) -> EcsTask {
        EcsTask {
            last_status: ls,
            desired_status_stopped: stopped,
            health: h,
            health_check_defined,
            lease_valid: lease,
            accepting_work: accepting,
        }
    }

    #[test]
    fn activating_task_projects_to_starting() {
        let d = EcsDriver.project(
            &task(
                EcsLastStatus::Activating,
                false,
                EcsHealth::Unknown,
                false,
                false,
            ),
            false,
        );
        assert_eq!(d.classify(), AgentState::Starting);
    }

    #[test]
    fn running_healthy_task_projects_to_running() {
        let d = EcsDriver.project(
            &task(
                EcsLastStatus::Running,
                false,
                EcsHealth::Healthy,
                true,
                true,
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Running);
    }

    #[test]
    fn unhealthy_after_verified() {
        // Was RUNNING before, now healthStatus UNHEALTHY ⇒ Unhealthy (not Starting).
        let d = EcsDriver.project(
            &task(
                EcsLastStatus::Running,
                false,
                EcsHealth::Unhealthy,
                true,
                false,
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn node_lost_unknown_is_unhealthy_not_stopped() {
        // Unknown health while verified ⇒ Unhealthy(fenced), not Stopped.
        let d = EcsDriver.project(
            &task(
                EcsLastStatus::Running,
                false,
                EcsHealth::Unknown,
                false,
                false,
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn running_unknown_without_health_check_is_running() {
        // No health check defined ⇒ ECS reports healthStatus UNKNOWN forever; a
        // running, leased instance is Running, NOT Unhealthy (the reported bug).
        let d = EcsDriver.project(
            &task_hc(
                EcsLastStatus::Running,
                false,
                EcsHealth::Unknown,
                true,
                true,
                false, // no health check defined
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Running);
    }

    #[test]
    fn running_unknown_with_defined_health_check_is_unhealthy() {
        // A *defined* check reporting UNKNOWN (lost signal) still fences.
        let d = EcsDriver.project(
            &task_hc(
                EcsLastStatus::Running,
                false,
                EcsHealth::Unknown,
                true,
                false,
                true, // health check defined, signal unknown
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn desired_stopped_is_stopping_while_observable() {
        let d = EcsDriver.project(
            &task(
                EcsLastStatus::Deactivating,
                true,
                EcsHealth::Healthy,
                true,
                false,
            ),
            true,
        );
        assert_eq!(d.classify(), AgentState::Stopping);
    }
}
