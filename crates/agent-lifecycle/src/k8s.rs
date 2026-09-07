//! K8s runtime driver — projects k8s Pod signals onto the canonical model.
//!
//! Mapping (ADR §6, the k8s column): Pod phase + readinessProbe ⇒ Starting /
//! Running; `metadata.deletionTimestamp != null` ⇒ `DesiredStatus::Stopped`
//! (Terminating: preStop + grace); `Unknown` phase (node lost) ⇒
//! Unhealthy(fenced), same as a lost lease; CrashLoopBackOff ⇒ Unhealthy.
//! Deliberately mirrors `ecs.rs`'s shape — this is a direct port of an
//! already-approved ADR table, not a new design decision (studio#146, the
//! observability follow-up `k8s_driver.rs`'s module doc flagged as deferred
//! when the k8s `ProvisionDriver` write path landed).

use crate::{DesiredStatus, Discriminator, Health, RuntimeDriver};

/// The subset of k8s Pod `phase` relevant to the projection. `Unknown` is
/// k8s's own "kubelet not reporting" signal — the direct analogue of ECS's
/// node-loss case, not a leftover default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodPhase {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

/// A single k8s Pod observation (the subset the projection needs).
#[derive(Debug, Clone, Copy)]
pub struct K8sPod {
    pub phase: PodPhase,
    /// `metadata.deletionTimestamp != null`.
    pub deletion_timestamp_set: bool,
    /// The Pod's `Ready` condition (readiness probe result when one is
    /// declared; otherwise just reflects the container's running state).
    pub ready: bool,
    /// Whether a readiness probe is actually declared on the pod spec. When
    /// it is not, k8s's `Ready` condition tracks plain container-running
    /// state, not health — so `ready == false` there means "still starting",
    /// not a fault. Mirrors `EcsTask::health_check_defined`'s "no probe ≠
    /// fault" distinction; without it, a probe-less pod that just hasn't
    /// finished starting would misread as permanently degraded.
    pub ready_check_defined: bool,
    /// A container in this pod is in CrashLoopBackOff (or an equivalent
    /// terminal restart-loop waiting reason) — a fault signal distinct from
    /// `ready`, since a crash-looping container can still report a phase
    /// that looks superficially fine between restarts.
    pub crash_loop_back_off: bool,
    /// CP-issued lease still valid (heartbeat authorized).
    pub lease_valid: bool,
    /// CP/director cordon: `false` ⇒ not admitting new work (→ Paused).
    pub accepting_work: bool,
}

/// Projects k8s Pod state onto the canonical lifecycle model.
pub struct K8sDriver;

impl RuntimeDriver for K8sDriver {
    type Native = K8sPod;
    /// Pod UID.
    type InstanceId = String;

    fn project(&self, pod: &K8sPod, verified_before: bool) -> Discriminator {
        let desired_status = if pod.deletion_timestamp_set {
            DesiredStatus::Stopped
        } else {
            DesiredStatus::Running
        };

        // `identity_verified` latches once the pod has ever reported phase
        // Running with Ready true.
        let identity_verified =
            verified_before || (pod.phase == PodPhase::Running && pod.ready);

        // Node-unreachable (Unknown phase), a crash-loop, or a lost lease all
        // fault outright. A *declared* readiness probe failing faults too —
        // but an undeclared one reporting `ready == false` just means "still
        // starting" (see `ready_check_defined`'s doc), not a fault; that case
        // only matters once `identity_verified` is already true, since
        // `classify()` reads `Starting` ahead of `health` otherwise.
        let health = if pod.phase == PodPhase::Unknown
            || pod.crash_loop_back_off
            || !pod.lease_valid
            || (pod.ready_check_defined && !pod.ready)
        {
            Health::Faulted
        } else {
            Health::Ok
        };

        Discriminator {
            desired_status,
            accepting_work: pod.accepting_work,
            health,
            identity_verified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentState;

    fn pod(
        phase: PodPhase,
        deleting: bool,
        ready: bool,
        lease: bool,
        accepting: bool,
    ) -> K8sPod {
        // Default: no readiness probe declared (the common case for OAB agents).
        pod_probe(phase, deleting, ready, lease, accepting, false, false)
    }

    fn pod_probe(
        phase: PodPhase,
        deleting: bool,
        ready: bool,
        lease: bool,
        accepting: bool,
        ready_check_defined: bool,
        crash_loop_back_off: bool,
    ) -> K8sPod {
        K8sPod {
            phase,
            deletion_timestamp_set: deleting,
            ready,
            ready_check_defined,
            crash_loop_back_off,
            lease_valid: lease,
            accepting_work: accepting,
        }
    }

    #[test]
    fn pending_pod_projects_to_starting() {
        let d = K8sDriver.project(&pod(PodPhase::Pending, false, false, false, false), false);
        assert_eq!(d.classify(), AgentState::Starting);
    }

    #[test]
    fn running_ready_pod_projects_to_running() {
        let d = K8sDriver.project(&pod(PodPhase::Running, false, true, true, true), false);
        assert_eq!(d.classify(), AgentState::Running);
    }

    #[test]
    fn cordoned_running_pod_is_paused() {
        let d = K8sDriver.project(&pod(PodPhase::Running, false, true, true, false), true);
        assert_eq!(d.classify(), AgentState::Paused);
    }

    #[test]
    fn declared_probe_failing_after_verified_is_unhealthy() {
        // Was Ready before, now the declared readiness probe fails ⇒
        // Unhealthy (not Starting).
        let d = K8sDriver.project(
            &pod_probe(PodPhase::Running, false, false, true, false, true, false),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn node_lost_unknown_is_unhealthy_not_stopped() {
        // Unknown phase (kubelet not reporting) while verified ⇒
        // Unhealthy(fenced), not Stopped.
        let d = K8sDriver.project(&pod(PodPhase::Unknown, false, false, false, false), true);
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn crash_loop_back_off_is_unhealthy_even_if_ready() {
        let d = K8sDriver.project(
            &pod_probe(PodPhase::Running, false, true, true, true, true, true),
            true,
        );
        assert_eq!(d.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn running_without_declared_probe_and_not_ready_is_still_running() {
        // No readiness probe declared ⇒ k8s's Ready condition isn't a health
        // signal here — a running, leased instance stays Running (the ECS
        // side's equivalent case: `running_unknown_without_health_check_is_running`).
        let d = K8sDriver.project(&pod(PodPhase::Running, false, false, true, true), true);
        assert_eq!(d.classify(), AgentState::Running);
    }

    #[test]
    fn deletion_timestamp_is_stopping_while_observable() {
        let d = K8sDriver.project(&pod(PodPhase::Running, true, true, true, false), true);
        assert_eq!(d.classify(), AgentState::Stopping);
    }
}
