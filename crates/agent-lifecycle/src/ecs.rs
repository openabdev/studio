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

    fn observe(&self, _id: &Self::InstanceId) -> Option<Self::Native> {
        // Slice-1: wiring to the ECS API is deferred. The real implementation
        // calls DescribeTasks and returns `None` when the task is absent from
        // the response (⇒ Stopped).
        unimplemented!("ECS DescribeTasks wiring is a later slice")
    }

    fn project(&self, task: &EcsTask, verified_before: bool) -> Discriminator {
        let desired_status = if task.desired_status_stopped {
            DesiredStatus::Stopped
        } else {
            DesiredStatus::Running
        };

        // `identity_verified` latches once `lastStatus` has ever reached RUNNING.
        let identity_verified =
            verified_before || task.last_status == EcsLastStatus::Running;

        // Faulted on an unhealthy check, an unknown/unobservable status
        // (node lost), or a lost lease.
        let health = match task.health {
            EcsHealth::Healthy if task.lease_valid => Health::Ok,
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
        EcsTask {
            last_status: ls,
            desired_status_stopped: stopped,
            health: h,
            lease_valid: lease,
            accepting_work: accepting,
        }
    }

    #[test]
    fn activating_task_projects_to_starting() {
        let d = EcsDriver.project(
            &task(EcsLastStatus::Activating, false, EcsHealth::Unknown, false, false),
            false,
        );
        assert_eq!(d.classify(), AgentState::Starting);
    }

    #[test]
    fn running_healthy_task_projects_to_running() {
        let d = EcsDriver.project(
            &task(EcsLastStatus::Running, false, EcsHealth::Healthy, true, true),
            true,
        );
        assert_eq!(d.classify(), AgentState::Running);
    }

    #[test]
    fn unhealthy_after_verified() {
        // Was RUNNING before, now healthStatus UNHEALTHY ⇒ Unhealthy (not Starting).
        let d = EcsDriver.project(
            &task(EcsLastStatus::Running, false, EcsHealth::Unhealthy, true, false),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn node_lost_unknown_is_unhealthy_not_stopped() {
        // Unknown health while verified ⇒ Unhealthy(fenced), not Stopped.
        let d = EcsDriver.project(
            &task(EcsLastStatus::Running, false, EcsHealth::Unknown, false, false),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn desired_stopped_is_stopping_while_observable() {
        let d = EcsDriver.project(
            &task(EcsLastStatus::Deactivating, true, EcsHealth::Healthy, true, false),
            true,
        );
        assert_eq!(d.classify(), AgentState::Stopping);
    }
}
