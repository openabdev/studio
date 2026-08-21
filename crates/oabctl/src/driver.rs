//! Provisioning driver seam (K8s driver, ADR #63 slice 3, sub-slice 3a).
//!
//! `studio_api`'s write path (`provision`/`redeploy`/`scale`/`delete`) is the
//! actual surface `studio-cp` depends on for mutation. This trait carves the
//! ECS specifics out from behind that surface so a `K8sDriver` can be added
//! later (slice 3b) without changing `studio_api`'s public signatures again.
//!
//! `EcsDriver` below is a pass-through: it wraps the existing free functions
//! in `apply`/`delete`/`ecsctl` unchanged, so this file changes no behavior.
//! It has exactly one implementation and isn't yet load-bearing for dispatch
//! (there's nothing to dispatch to until 3b) — same shape as
//! `manifest::Runtime::Kubernetes`, which has sat as a validated-but-rejected
//! schema stub since slice-0 for the same reason: declare the seam, fill the
//! second side in when it exists.

use crate::apply::{ApplyOptions, ApplyReport};
use crate::manifest::OABServiceManifest;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ProvisionDriver {
    /// Create-or-update the given manifests.
    async fn apply(&self, manifests: &[OABServiceManifest], opts: &ApplyOptions) -> Result<ApplyReport>;

    /// Scale a single service. OAB services carry a single bot token, so
    /// `size` must be 0 (off) or 1 (on) — enforced by the implementation.
    async fn scale(&self, cluster: &str, namespace: &str, name: &str, size: i32) -> Result<()>;

    /// Delete a control-plane resource (`resource` is currently always
    /// `"oabservice"`). `control_plane_bucket` is the already-resolved bucket
    /// (see `control_plane::resolve_bucket`) — the driver does not resolve it.
    async fn delete(
        &self,
        resource: &str,
        name: &str,
        cluster: &str,
        namespace: &str,
        control_plane_bucket: &str,
    ) -> Result<()>;
}

/// The ECS implementation. Every method is a thin wrapper over the existing
/// `apply`/`delete`/`ecsctl` free functions — no logic moved or changed.
pub struct EcsDriver<'a> {
    pub aws_config: &'a aws_config::SdkConfig,
}

#[async_trait]
impl<'a> ProvisionDriver for EcsDriver<'a> {
    async fn apply(&self, manifests: &[OABServiceManifest], opts: &ApplyOptions) -> Result<ApplyReport> {
        crate::apply::apply_manifests(self.aws_config, manifests, opts)
            .await
            .map_err(|e| anyhow::anyhow!("apply failed [{:?}]: {e}", e.kind))
    }

    async fn scale(&self, cluster: &str, namespace: &str, name: &str, size: i32) -> Result<()> {
        if size != 0 && size != 1 {
            anyhow::bail!(
                "invalid size: {size}. OAB services scale only to 0 (off) or 1 (on) — \
                 each runs a single bot token and scaling above 1 duplicates responses."
            );
        }
        let service_name = format!("oab-{namespace}-{name}");
        let ecs = aws_sdk_ecs::Client::new(self.aws_config);
        ecsctl::scale::scale_service(&ecs, cluster, &service_name, size, false).await
    }

    async fn delete(
        &self,
        resource: &str,
        name: &str,
        cluster: &str,
        namespace: &str,
        control_plane_bucket: &str,
    ) -> Result<()> {
        crate::delete::run_with_bucket(
            self.aws_config,
            resource,
            name,
            cluster,
            namespace,
            control_plane_bucket,
        )
        .await
    }
}
