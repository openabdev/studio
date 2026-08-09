//! Studio-facing programmatic surface.
//!
//! Structured, non-interactive entry points over the CLI-oriented internals so
//! downstream crates (`studio-cp`, `oab-mcp`) call a library API instead of
//! shelling out to the `oabctl` binary. **Additive only** — this module adds no
//! behaviour to the existing CLI paths and changes no existing public type. See
//! `VENDORED.md`.

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

/// Immediate scale of an agent/service to `size` replicas (ECS `UpdateService`).
///
/// Thin wrapper over the CLI's scale path.
pub async fn scale(config: &aws_config::SdkConfig, alias: &str, size: i32) -> Result<()> {
    crate::scale::run(config, alias, size).await
}

/// Delete a control-plane resource (e.g. an `OABService`).
///
/// Thin wrapper over the CLI's delete path.
pub async fn delete(
    config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
) -> Result<()> {
    crate::delete::run(config, resource, name, cluster, namespace).await
}
