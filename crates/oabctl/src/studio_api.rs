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
use aws_sdk_s3::primitives::ByteStream;

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

/// The `bundleFrom` S3 **prefix** URI an agent's composed bundle is uploaded to
/// and restored from at boot: `s3://{bucket}/artifacts/{namespace}/{name}/`
/// (trailing slash). Pairs with `studio_compose::Bundle::artifact_objects`, whose
/// keys are exactly `artifacts/{namespace}/{name}/{path}` under the same bucket.
pub fn bundle_from_uri(bucket: &str, namespace: &str, name: &str) -> String {
    format!("s3://{bucket}/artifacts/{namespace}/{name}/")
}

/// Outcome of pushing a bundle: which bucket it landed in and how many objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushBundleReport {
    pub bucket: String,
    pub objects: usize,
}

/// Upload a composed bundle's `(s3_key, bytes)` objects to the control-plane
/// bucket (agent deployment ADR, path A). Keys must already be under the agent's
/// artifacts prefix — produce them with `studio_compose::Bundle::artifact_objects`
/// so they line up with the `bundleFrom` the manifest carries. Puts are
/// idempotent overwrites, so re-provisioning simply replaces the prior bundle.
///
/// Config-free: the bucket is `control_plane_bucket`, else
/// `$OAB_CONTROL_PLANE_BUCKET`, else derived from the caller's account — never
/// `~/.oabctl/config.toml`.
pub async fn push_bundle(
    config: &aws_config::SdkConfig,
    control_plane_bucket: Option<&str>,
    objects: &[(String, Vec<u8>)],
) -> Result<PushBundleReport> {
    let bucket = crate::control_plane::resolve_bucket(config, control_plane_bucket).await?;
    let s3 = aws_sdk_s3::Client::new(config);
    for (key, bytes) in objects {
        s3.put_object()
            .bucket(&bucket)
            .key(key)
            .body(ByteStream::from(bytes.clone()))
            .send()
            .await
            .with_context(|| format!("failed to upload bundle object '{key}'"))?;
    }
    Ok(PushBundleReport {
        bucket,
        objects: objects.len(),
    })
}

/// Provision an agent from a composed bundle: **push the bundle** to the agent's
/// artifacts prefix, then **apply the manifest** (create/update the ECS service
/// at the manifest's chosen image tag). This is the ECS half of the deployment
/// ADR's provider-tagged driver (slice 2).
///
/// Order matters: the bundle is uploaded first, so the artifacts prefix the
/// service reads (`configFrom` / `bundleFrom`) exists before the task starts.
/// `manifest_yaml` is a rendered `OABService` (or `OABFleet`) whose `bundleFrom`
/// should be [`bundle_from_uri`] for the same `(bucket, namespace, name)`.
/// Config-free like the rest of this module.
pub async fn provision(
    config: &aws_config::SdkConfig,
    cluster: &str,
    manifest_yaml: &str,
    objects: &[(String, Vec<u8>)],
    control_plane_bucket: Option<&str>,
) -> Result<crate::apply::ApplyReport> {
    // 1. Bundle first — idempotent puts, so the file carrier is ready before ECS
    //    pulls the task up and reads config/persona/skills from it.
    push_bundle(config, control_plane_bucket, objects).await?;

    // 2. Apply the service manifest at its chosen image tag. `apply_manifests` is
    //    config-free and reconciles create-or-update.
    let manifests = parse_manifests(manifest_yaml)?;
    let mut opts = crate::apply::ApplyOptions::new(cluster);
    if let Some(bucket) = control_plane_bucket {
        opts = opts.with_control_plane_bucket(bucket);
    }
    crate::apply::apply_manifests(config, &manifests, &opts)
        .await
        .context("failed to apply manifest during provision")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_from_uri_is_a_trailing_slash_prefix() {
        assert_eq!(
            bundle_from_uri("oab-control-plane-123", "prod", "orca"),
            "s3://oab-control-plane-123/artifacts/prod/orca/"
        );
    }

    #[test]
    fn manifest_round_trips_bundle_from() {
        // A manifest carrying bundleFrom parses into spec.bundle_from; one without
        // it defaults to None (legacy config-only services stay unchanged).
        let with = r#"
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: orca
  namespace: prod
spec:
  image: ghcr.io/openabdev/openab:0.9.0-claude
  resources: { cpu: "256", memory: "512" }
  configFrom: s3://b/artifacts/prod/orca/config.toml
  bundleFrom: s3://b/artifacts/prod/orca/
  runtime:
    type: ecs
    networking: { subnets: ["subnet-1"], securityGroups: ["sg-1"] }
"#;
        let m = &parse_manifests(with).unwrap()[0];
        assert_eq!(
            m.spec.bundle_from.as_deref(),
            Some("s3://b/artifacts/prod/orca/")
        );

        let without = r#"
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: orca
  namespace: prod
spec:
  image: ghcr.io/openabdev/openab:0.9.0-claude
  resources: { cpu: "256", memory: "512" }
  configFrom: s3://b/artifacts/prod/orca/config.toml
  runtime:
    type: ecs
    networking: { subnets: ["subnet-1"], securityGroups: ["sg-1"] }
"#;
        assert_eq!(parse_manifests(without).unwrap()[0].spec.bundle_from, None);
    }
}
