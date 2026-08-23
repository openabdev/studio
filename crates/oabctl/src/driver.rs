//! Provisioning driver seam (K8s driver, ADR #63 slice 3, sub-slice 3a).
//!
//! `studio_api`'s write path (`provision`/`redeploy`/`scale`/`delete`) is the
//! actual surface `studio-cp` depends on for mutation. This trait carves the
//! ECS specifics out from behind that surface so a `K8sDriver` can be added
//! (slice 3b) without changing `studio_api`'s public signatures again.
//!
//! `EcsDriver` below is a pass-through: it wraps the existing free functions
//! in `apply`/`delete`/`ecsctl` unchanged, so this file changes no ECS
//! behavior. `cluster` lives on the driver instance rather than in a per-call
//! parameter or in `ApplyOptions` — a target (ECS cluster, or later a k8s
//! context+namespace) is a property of *which driver* you built, not
//! something you repeat on every call. `ProvisionOptions` carries only the
//! fields that actually generalize across drivers (bucket, wait); ECS's own
//! `ApplyOptions` (used by `apply_manifests` directly, still ECS-specific)
//! is unaffected.
//!
//! It has exactly one implementation and isn't yet load-bearing for dispatch
//! (there's nothing to dispatch to until 3b) — same shape as
//! `manifest::Runtime::Kubernetes`, which has sat as a validated-but-rejected
//! schema stub since slice-0 for the same reason: declare the seam, fill the
//! second side in when it exists.

use crate::apply::ApplyReport;
use crate::manifest::OABServiceManifest;
use anyhow::Result;
use async_trait::async_trait;

/// Generic apply options — the subset of `apply::ApplyOptions` that isn't
/// ECS-specific. `cluster` lives on the driver instance instead (see module
/// docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvisionOptions {
    pub control_plane_bucket: Option<String>,
    pub wait: bool,
}

#[async_trait]
pub trait ProvisionDriver {
    /// Create-or-update the given manifests.
    async fn apply(&self, manifests: &[OABServiceManifest], opts: &ProvisionOptions) -> Result<ApplyReport>;

    /// Scale a single service. OAB services carry a single bot token, so
    /// `size` must be 0 (off) or 1 (on) — enforced by the implementation.
    async fn scale(&self, namespace: &str, name: &str, size: i32) -> Result<()>;

    /// Delete a control-plane resource (`resource` is currently always
    /// `"oabservice"`). `control_plane_bucket` is the already-resolved bucket
    /// (see `control_plane::resolve_bucket`) — the driver does not resolve it.
    async fn delete(&self, resource: &str, name: &str, namespace: &str, control_plane_bucket: &str) -> Result<()>;
}

/// The ECS implementation. Every method is a thin wrapper over the existing
/// `apply`/`delete`/`ecsctl` free functions — no logic moved or changed.
pub struct EcsDriver<'a> {
    pub aws_config: &'a aws_config::SdkConfig,
    pub cluster: &'a str,
}

#[async_trait]
impl<'a> ProvisionDriver for EcsDriver<'a> {
    async fn apply(&self, manifests: &[OABServiceManifest], opts: &ProvisionOptions) -> Result<ApplyReport> {
        let mut ecs_opts = crate::apply::ApplyOptions::new(self.cluster).with_wait(opts.wait);
        if let Some(bucket) = &opts.control_plane_bucket {
            ecs_opts = ecs_opts.with_control_plane_bucket(bucket.clone());
        }
        crate::apply::apply_manifests(self.aws_config, manifests, &ecs_opts)
            .await
            .map_err(ecs_apply_error_to_anyhow)
    }

    async fn scale(&self, namespace: &str, name: &str, size: i32) -> Result<()> {
        if size != 0 && size != 1 {
            anyhow::bail!(
                "invalid size: {size}. OAB services scale only to 0 (off) or 1 (on) — \
                 each runs a single bot token and scaling above 1 duplicates responses."
            );
        }
        let service_name = format!("oab-{namespace}-{name}");
        let ecs = aws_sdk_ecs::Client::new(self.aws_config);
        ecsctl::scale::scale_service(&ecs, self.cluster, &service_name, size, false).await
    }

    async fn delete(&self, resource: &str, name: &str, namespace: &str, control_plane_bucket: &str) -> Result<()> {
        crate::delete::run_with_bucket(
            self.aws_config,
            resource,
            name,
            self.cluster,
            namespace,
            control_plane_bucket,
        )
        .await
    }
}

/// Convert `apply_manifests`'s structured error into an `anyhow::Error`
/// while keeping the `ApplyError` itself in the source chain — a caller can
/// still `err.chain().find_map(anyhow::Error::downcast_ref::<apply::ApplyError>)`
/// to recover `.completed`/`.failed_service` for a partial fleet-apply
/// failure, same as callers could before `ProvisionDriver` existed (when
/// `studio_api::provision` used `.context(..)` directly on `apply_manifests`'s
/// result). A prior version of this flattened `e` into `anyhow::anyhow!(...)`,
/// which silently dropped that recovery path — extracted as its own function
/// so the conversion is unit-testable without a live ECS call.
fn ecs_apply_error_to_anyhow(e: crate::apply::ApplyError) -> anyhow::Error {
    let kind = e.kind;
    anyhow::Error::new(e).context(format!("apply failed [{kind:?}]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{ApplyAction, AppliedService, ApplyError, ApplyErrorKind, ApplyReport, ServiceTarget};

    #[test]
    fn ecs_apply_error_to_anyhow_preserves_downcast_and_partial_progress() {
        let completed = ApplyReport {
            services: vec![AppliedService {
                namespace: "prod".to_string(),
                name: "orca".to_string(),
                resource_name: "oab-prod-orca".to_string(),
                action: ApplyAction::Updated,
                webhook_urls: vec![],
                warnings: vec![],
            }],
        };
        let failed = ServiceTarget {
            namespace: "prod".to_string(),
            name: "mira".to_string(),
            ecs_service_name: "oab-prod-mira".to_string(),
        };
        let source = ApplyError::reconciliation(failed.clone(), completed.clone(), anyhow::anyhow!("boom"));

        let err = ecs_apply_error_to_anyhow(source);

        assert!(err.to_string().contains("Reconciliation"));
        let recovered = err
            .chain()
            .find_map(|e| e.downcast_ref::<ApplyError>())
            .expect("ApplyError must survive in the source chain");
        assert_eq!(recovered.kind, ApplyErrorKind::Reconciliation);
        assert_eq!(recovered.failed_service, Some(failed));
        assert_eq!(recovered.completed, completed);
    }
}
