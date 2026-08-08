//! Canonical agent lifecycle state machine.
//!
//! Implements the 6-state model and 4-axis discriminator from
//! `docs/adr/agent-lifecycle.md`. The control plane classifies every agent
//! instance, at any moment, into exactly one [`AgentState`]. Runtime drivers
//! project their native signals onto the [`Discriminator`]; the machine itself
//! never changes per runtime.

pub mod ecs;

/// Whether the control plane wants this instance running or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredStatus {
    Running,
    Stopped,
}

/// Point-in-time health / sync — "is it OK right now".
///
/// `Faulted` covers a lost heartbeat / failed probe / lost lease / not-in-sync,
/// as well as an *unobservable* instance (node lost). It does **not** cover
/// version skew — that is a healthy `superseded` attribute, not a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    Faulted,
}

/// The four observable discriminator axes (ADR §3).
///
/// `identity_verified` is **latching**: set true the first time the instance
/// reaches Running, never cleared for the life of that instance. It is what
/// separates `Starting` (never verified) from `Unhealthy` (was verified, now
/// faulted) — without it their other three axes collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discriminator {
    pub desired_status: DesiredStatus,
    pub accepting_work: bool,
    pub health: Health,
    pub identity_verified: bool,
}

/// The canonical six states. Exactly one holds at any moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Starting,
    Running,
    Paused,
    Unhealthy,
    Stopping,
    Stopped,
}

impl Discriminator {
    /// Classify a **live** (still-observable) instance into a lifecycle state.
    ///
    /// Never returns [`AgentState::Stopped`]; that is terminal and represented
    /// by the absence of an observation (see [`classify`]).
    pub fn classify(&self) -> AgentState {
        match self.desired_status {
            // Terminate committed; still observable ⇒ graceful teardown window.
            DesiredStatus::Stopped => AgentState::Stopping,
            DesiredStatus::Running => {
                if !self.identity_verified {
                    // Never came up yet — coming-up, not a fault.
                    AgentState::Starting
                } else if self.health == Health::Faulted {
                    // Came up before, now faulted — alive but fenced.
                    AgentState::Unhealthy
                } else if self.accepting_work {
                    AgentState::Running
                } else {
                    // Healthy and in-sync, but deliberately not admitting.
                    AgentState::Paused
                }
            }
        }
    }
}

/// Classify from an optional observation.
///
/// `None` means the instance no longer exists (terminated / hard loss) ⇒
/// [`AgentState::Stopped`] (terminal, absorbing). `Some(disc)` delegates to
/// [`Discriminator::classify`].
pub fn classify(observation: Option<&Discriminator>) -> AgentState {
    match observation {
        None => AgentState::Stopped,
        Some(disc) => disc.classify(),
    }
}

/// Maintains the latching `identity_verified` bit across observations.
///
/// The control plane owns this bit (default-deny; never the agent's
/// self-report). Feed it the observed health while `desired_status == Running`;
/// it flips to `true` the first time health is `Ok` and stays there for the
/// life of the instance.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdentityLatch(bool);

impl IdentityLatch {
    pub fn new() -> Self {
        Self(false)
    }

    /// Update with the latest observed health; returns the latched value.
    pub fn observe(&mut self, health: Health) -> bool {
        if health == Health::Ok {
            self.0 = true;
        }
        self.0
    }

    pub fn verified(&self) -> bool {
        self.0
    }
}

/// A runtime driver projects its native signals onto the canonical model.
pub trait RuntimeDriver {
    /// The driver's native, per-instance observation type.
    type Native;
    /// Opaque per-instance identifier in this runtime.
    type InstanceId;

    /// Observe an instance. `None` ⇒ the instance no longer exists (⇒ Stopped).
    fn observe(&self, id: &Self::InstanceId) -> Option<Self::Native>;

    /// Project a native observation onto the four discriminator axes.
    ///
    /// `verified_before` is the latched `identity_verified` the control plane
    /// has tracked for this instance so far.
    fn project(&self, native: &Self::Native, verified_before: bool) -> Discriminator;

    /// Convenience: observe + project + classify into an [`AgentState`].
    fn state(&self, id: &Self::InstanceId, verified_before: bool) -> AgentState {
        match self.observe(id) {
            None => AgentState::Stopped,
            Some(native) => self.project(&native, verified_before).classify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(d: DesiredStatus, accepting: bool, h: Health, verified: bool) -> Discriminator {
        Discriminator {
            desired_status: d,
            accepting_work: accepting,
            health: h,
            identity_verified: verified,
        }
    }

    #[test]
    fn starting_vs_unhealthy_differ_only_by_identity_latch() {
        // F1: identical (desired, accepting_work, health), opposite identity_verified.
        let starting = disc(DesiredStatus::Running, false, Health::Faulted, false);
        let unhealthy = disc(DesiredStatus::Running, false, Health::Faulted, true);
        assert_eq!(starting.classify(), AgentState::Starting);
        assert_eq!(unhealthy.classify(), AgentState::Unhealthy);
    }

    #[test]
    fn running_and_paused() {
        let running = disc(DesiredStatus::Running, true, Health::Ok, true);
        let paused = disc(DesiredStatus::Running, false, Health::Ok, true);
        assert_eq!(running.classify(), AgentState::Running);
        assert_eq!(paused.classify(), AgentState::Paused);
    }

    #[test]
    fn superseded_is_paused_not_running() {
        // superseded ⇒ CP sets accepting_work=false ⇒ Paused (never dispatchable).
        let superseded = disc(DesiredStatus::Running, false, Health::Ok, true);
        assert_eq!(superseded.classify(), AgentState::Paused);
    }

    #[test]
    fn stopping_while_observable_stopped_when_gone() {
        let stopping = disc(DesiredStatus::Stopped, false, Health::Ok, true);
        assert_eq!(stopping.classify(), AgentState::Stopping);
        assert_eq!(classify(None), AgentState::Stopped);
        assert_eq!(classify(Some(&stopping)), AgentState::Stopping);
    }

    #[test]
    fn identity_latch_is_monotonic() {
        let mut latch = IdentityLatch::new();
        assert!(!latch.verified());
        assert!(!latch.observe(Health::Faulted)); // still starting
        assert!(latch.observe(Health::Ok)); // reached Running -> latched
        assert!(latch.observe(Health::Faulted)); // now Unhealthy, latch stays true
        assert!(latch.verified());
    }
}
