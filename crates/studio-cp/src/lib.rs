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
