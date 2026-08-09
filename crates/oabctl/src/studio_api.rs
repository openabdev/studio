//! Studio-facing programmatic surface.
//!
//! Structured, non-interactive entry points over the CLI-oriented internals so
//! downstream crates (`studio-cp`, `oab-mcp`) call a library API instead of
//! shelling out to the `oabctl` binary. **Additive only** — this module adds no
//! behaviour to the existing CLI paths and changes no existing public type. See
//! `VENDORED.md`.
//!
//! Unlike the CLI entry points, these functions **never read
//! `~/.oabctl/config.toml`**: the cluster/namespace (and, for delete, the
//! control-plane bucket) are passed explicitly or resolved from the
//! environment, so an MCP/host process with no oabctl config can still drive
//! writes.

use crate::manifest::{OABFleetManifest, OABServiceManifest, RawManifest};
use anyhow::{Context, Result};

/// Parse a manifest YAML document into one or more service manifests.
///
/// Mirrors the private `apply::parse_manifest_file`, but from an in-memory
/// string (no filesystem): an `OABService` yields one manifest, an `OABFleet`
/// expands to many.
pub fn parse_manifests(yaml: &str) -> Result<Vec<OABServiceManifest>> {
    let raw: RawManifest = serde_yaml::from_str(yaml).context("failed to parse manifest")?;
    match raw.kind.as_str() {
        "OABService" => {
            let m: OABServiceManifest =
                serde_yaml::from_str(yaml).context("failed to parse OABService manifest")?;
            Ok(vec![m])
        }
        "OABFleet" => {
            let fleet: OABFleetManifest =
                serde_yaml::from_str(yaml).context("failed to parse OABFleet manifest")?;
            fleet.validate()?;
            Ok(fleet.expand())
        }
        other => anyhow::bail!("unsupported manifest kind '{other}'"),
    }
}

/// Immediate scale of an OAB service to `size` replicas via ECS `UpdateService`.
///
/// The service name is `oab-{namespace}-{name}`. OAB services carry a single
/// bot token, so `size` must be **0 (off) or 1 (on)** — anything else would
/// produce duplicate responders and is rejected. Config-free: cluster and
/// namespace are explicit.
pub async fn scale(
    config: &aws_config::SdkConfig,
    cluster: &str,
    namespace: &str,
    name: &str,
    size: i32,
) -> Result<()> {
    if size != 0 && size != 1 {
        anyhow::bail!(
            "invalid size: {size}. OAB services scale only to 0 (off) or 1 (on) — \
             each runs a single bot token and scaling above 1 duplicates responses."
        );
    }
    let service_name = format!("oab-{namespace}-{name}");
    let ecs = aws_sdk_ecs::Client::new(config);
    ecsctl::scale::scale_service(&ecs, cluster, &service_name, size, false).await
}

/// Delete a control-plane resource (currently `oabservice`).
///
/// Config-free: cluster and namespace are explicit; the control-plane bucket is
/// resolved from `control_plane_bucket`, then `$OAB_CONTROL_PLANE_BUCKET`, then
/// the caller's account — never from `~/.oabctl/config.toml`.
pub async fn delete(
    config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
    control_plane_bucket: Option<&str>,
) -> Result<()> {
    let bucket = crate::control_plane::resolve_bucket(config, control_plane_bucket).await?;
    crate::delete::run_with_bucket(config, resource, name, cluster, namespace, &bucket).await
}
