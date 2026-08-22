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

use crate::driver::ProvisionDriver;
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

/// The `bundleFrom` S3 **prefix** URI an agent's composed bundle is uploaded to:
/// `s3://{bucket}/artifacts/{namespace}/{name}/` (trailing slash). Pairs with
/// `studio_compose::Bundle::artifact_objects`, whose keys are exactly
/// `artifacts/{namespace}/{name}/{path}` under the same bucket.
///
/// Bookkeeping only — nothing downloads this prefix back (see
/// [`bundle_zip_uri`]'s doc for the mechanism that actually restores a
/// bundle). Kept for now as an informational record of where the loose files
/// landed; no manifest field or driver reads it.
pub fn bundle_from_uri(bucket: &str, namespace: &str, name: &str) -> String {
    format!("s3://{bucket}/artifacts/{namespace}/{name}/")
}

/// Filename (relative to the artifacts prefix) a bundle's zip archive uploads
/// to. Must match `studio_compose::Bundle::ZIP_FILENAME` — the two crates
/// don't share a dependency edge to enforce this with a shared constant, so
/// `studio-cp` (which depends on both) tests the two stay in sync.
pub const BUNDLE_ZIP_FILENAME: &str = "bundle.zip";

/// The S3 URI a bundle's zip archive (`studio_compose::Bundle::zip_bytes`)
/// uploads to: `s3://{bucket}/artifacts/{namespace}/{name}/bundle.zip`.
///
/// This — not [`bundle_from_uri`]'s loose-file prefix — is what actually gets
/// restored at boot: [`inject_pre_seed_hook`] wires this URI into the
/// deployed agent's own `config.toml` as a `[hooks.pre_seed]` source, and
/// `openab`'s `pre_seed` feature (already the mechanism that restores an
/// agent's own persistent state across restarts) downloads + extracts it into
/// `~` on every boot. Platform-agnostic by construction — `pre_seed` is pure
/// "S3 GetObject + extract," it doesn't know or care whether the process
/// booting is an ECS task or a k8s pod, so no k8s-specific bundle carrier is
/// needed (see studio#97's slice-3c investigation: `bundle_from_uri`'s prefix
/// was never actually consumed by anything on the ECS path either).
pub fn bundle_zip_uri(bucket: &str, namespace: &str, name: &str) -> String {
    format!("s3://{bucket}/artifacts/{namespace}/{name}/{BUNDLE_ZIP_FILENAME}")
}

/// Append a `[hooks.pre_seed]` section to `config_toml`'s bytes, pointing at
/// `zip_uri` — see [`bundle_zip_uri`] for why this is the actual bundle-restore
/// mechanism. A no-op (returns `config_toml` unchanged) if the config already
/// declares `[hooks.pre_seed]`: an operator who hand-authored one keeps
/// control, this never silently overrides it. Appends rather than
/// re-serializing the whole document, so existing comments/formatting in the
/// operator's authored `config.toml` survive untouched — same "verbatim,
/// don't round-trip through a parser" principle `save_bindings_text` already
/// applies to `fleets.toml`.
pub fn inject_pre_seed_hook(config_toml: &[u8], zip_uri: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(config_toml).context("config.toml is not valid UTF-8")?;
    let parsed: toml::Value = text.parse().context("config.toml is not valid TOML")?;
    if parsed.get("hooks").and_then(|h| h.get("pre_seed")).is_some() {
        return Ok(config_toml.to_vec());
    }
    let quoted_uri = toml::Value::String(zip_uri.to_string());
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n[hooks.pre_seed]\nsources = [{quoted_uri}]\n"));
    Ok(out.into_bytes())
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

    // 2. Apply the service manifest at its chosen image tag, through the
    //    provisioning driver seam (config-free, reconciles create-or-update).
    let manifests = parse_manifests(manifest_yaml)?;
    let opts = crate::driver::ProvisionOptions {
        control_plane_bucket: control_plane_bucket.map(str::to_string),
        wait: false,
    };
    crate::driver::EcsDriver { aws_config: config, cluster }
        .apply(&manifests, &opts)
        .await
        .context("failed to apply manifest during provision")
}

/// Load the desired `OABService` manifest oabctl persists at
/// `manifests/{namespace}/{name}.yaml` in the control-plane bucket. Returns
/// `Ok(None)` when the agent has no stored manifest yet (never applied); other
/// S3/parse errors propagate. This is the deploy config's single source of truth
/// — networking/resources/secrets already live here, so a redeploy reuses them
/// instead of re-collecting them.
pub async fn load_manifest(
    config: &aws_config::SdkConfig,
    namespace: &str,
    name: &str,
    control_plane_bucket: Option<&str>,
) -> Result<Option<OABServiceManifest>> {
    let bucket = crate::control_plane::resolve_bucket(config, control_plane_bucket).await?;
    let s3 = aws_sdk_s3::Client::new(config);
    let key = format!("manifests/{namespace}/{name}.yaml");
    match s3.get_object().bucket(&bucket).key(&key).send().await {
        Ok(resp) => {
            let bytes = resp
                .body
                .collect()
                .await
                .with_context(|| format!("failed to read stored manifest '{key}'"))?
                .into_bytes();
            let manifest: OABServiceManifest = serde_yaml::from_slice(&bytes)
                .with_context(|| format!("failed to parse stored manifest '{key}'"))?;
            Ok(Some(manifest))
        }
        // A missing object is the "not provisioned yet" signal, not an error.
        Err(err) if err.as_service_error().map(|e| e.is_no_such_key()).unwrap_or(false) => Ok(None),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("failed to fetch stored manifest '{key}'")))
        }
    }
}

/// Re-provision an already-created agent from a freshly composed bundle: load its
/// stored manifest, repoint it at `image` (when given) and the bundle prefix,
/// **push the bundle, then apply**. Networking/resources/secrets are untouched —
/// they ride along from the stored manifest — so a redeploy needs no infra input.
///
/// Errors if the agent has no stored manifest (it must be `create`d first). This
/// is the ECS "update this agent to a new image / persona / skills" path.
pub async fn redeploy(
    config: &aws_config::SdkConfig,
    cluster: &str,
    namespace: &str,
    name: &str,
    image: Option<&str>,
    objects: &[(String, Vec<u8>)],
    control_plane_bucket: Option<&str>,
) -> Result<crate::apply::ApplyReport> {
    // Resolve the bucket once and thread it through, so load/push/apply all agree.
    let bucket = crate::control_plane::resolve_bucket(config, control_plane_bucket).await?;
    let mut manifest = load_manifest(config, namespace, name, Some(&bucket))
        .await?
        .with_context(|| {
            format!("no stored manifest for {namespace}/{name} — create the agent before redeploying")
        })?;

    if let Some(img) = image.filter(|s| !s.is_empty()) {
        manifest.spec.image = img.to_string();
    }
    manifest.spec.bundle_from = Some(bundle_from_uri(&bucket, namespace, name));

    let yaml = serde_yaml::to_string(&manifest).context("failed to serialize patched manifest")?;
    provision(config, cluster, &yaml, objects, Some(&bucket)).await
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
    crate::driver::EcsDriver { aws_config: config, cluster }
        .scale(namespace, name, size)
        .await
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
    crate::driver::EcsDriver { aws_config: config, cluster }
        .delete(resource, name, namespace, &bucket)
        .await
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
    fn bundle_zip_uri_is_the_bundle_from_prefix_plus_zip_filename() {
        assert_eq!(
            bundle_zip_uri("oab-control-plane-123", "prod", "orca"),
            format!(
                "{}{BUNDLE_ZIP_FILENAME}",
                bundle_from_uri("oab-control-plane-123", "prod", "orca")
            )
        );
        assert_eq!(
            bundle_zip_uri("b", "prod", "orca"),
            "s3://b/artifacts/prod/orca/bundle.zip"
        );
    }

    #[test]
    fn inject_pre_seed_hook_appends_to_existing_config() {
        let config = b"[agent]\nname = \"orca\"\n";
        let out = inject_pre_seed_hook(config, "s3://bucket/artifacts/prod/orca/bundle.zip").unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("[agent]\nname = \"orca\"\n"));
        assert!(text.contains("[hooks.pre_seed]"));
        assert!(text.contains("s3://bucket/artifacts/prod/orca/bundle.zip"));
        // still valid TOML after injection
        text.parse::<toml::Value>().expect("valid toml");
    }

    #[test]
    fn inject_pre_seed_hook_is_a_noop_when_already_present() {
        let config = b"[hooks.pre_seed]\nsources = [\"s3://other/bucket.zip\"]\n";
        let out = inject_pre_seed_hook(config, "s3://bucket/artifacts/prod/orca/bundle.zip").unwrap();
        // unchanged, byte-for-byte — an operator's own hook is never overridden
        assert_eq!(out, config);
    }

    #[test]
    fn inject_pre_seed_hook_rejects_invalid_toml() {
        assert!(inject_pre_seed_hook(b"not = [valid", "s3://x/bundle.zip").is_err());
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
