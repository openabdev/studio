//! Regression tests for studio#111's "+ New fleet" first-creation path.
//!
//! When `provision_from_library` / `provision_agent` find no stored manifest
//! (`load_manifest` → `None`), they build a fresh one and persist+apply it
//! through the same `apply_manifests` path `deploy_apply` uses — not a
//! parallel mechanism. The manifest those calls produce is assembled by
//! `default_service_manifest` (the pure half of `build_default_manifest`);
//! these tests pin that its output is a valid input to that path:
//! `OABServiceManifest::validate()` (the gate `validate_apply_request`
//! enforces on every apply) plus a round-trip through
//! `oabctl::studio_api::parse_manifests`, the exact parser `deploy_apply`'s
//! `apply_deployment` entry point calls.

use std::collections::HashMap;
use studio_cp::default_service_manifest;

/// A plausible `default_networking` result — what the create path feeds in
/// after `oabctl::create::default_networking` resolves VPC/subnets/SG.
fn ecs_networking() -> oabctl::manifest::EcsNetworking {
    oabctl::manifest::EcsNetworking {
        subnets: vec!["subnet-aaa".to_string(), "subnet-bbb".to_string()],
        security_groups: vec!["sg-orca".to_string()],
        assign_public_ip: false,
    }
}

#[test]
fn first_provision_manifest_survives_the_deploy_apply_parse_and_validate_path() {
    let mut secrets = HashMap::new();
    // Same `aws-sm://oab/{ns}/{name}#OPENAB_ACP_AUTH_KEY` ref
    // `provision_acp_auth_secret` stores for an acp_enabled create.
    secrets.insert(
        "OPENAB_ACP_AUTH_KEY".to_string(),
        "aws-sm://oab/prod/orca#OPENAB_ACP_AUTH_KEY".to_string(),
    );
    let manifest = default_service_manifest(
        "prod",
        "orca",
        "ghcr.io/openabdev/openab:0.10.0-beta.4-claude",
        "s3://oab-control-plane-123/artifacts/prod/orca/config.toml".to_string(),
        secrets,
        ecs_networking(),
        true,
    );

    // The gate `apply_manifests` enforces (validate_apply_request →
    // manifest.validate()) — a manifest that fails this would only ever
    // surface as a live-deploy error without this pin.
    manifest
        .validate()
        .expect("first-provision manifest must satisfy OABServiceManifest::validate");

    // `provision_manifest` serializes with serde_yaml before `provision`
    // re-parses — and `deploy_apply` (studio-cp's `apply_deployment`) parses
    // caller YAML through the same `parse_manifests`. Round-trip through the
    // real serializer + the real parser so drift between the producer and the
    // consumer can't land silently.
    let yaml = serde_yaml::to_string(&manifest).expect("manifest serializes");
    let parsed = oabctl::studio_api::parse_manifests(&yaml)
        .expect("deploy_apply's parser must accept the generated manifest");
    assert_eq!(parsed.len(), 1);
    parsed[0]
        .validate()
        .expect("re-parsed manifest still validates");
    assert_eq!(parsed[0].metadata.name, "orca");
    assert_eq!(parsed[0].metadata.namespace, "prod");
}

#[test]
fn first_provision_manifest_carries_the_create_wizard_defaults() {
    let manifest = default_service_manifest(
        "prod",
        "orca",
        "ghcr.io/openabdev/openab:0.10.0-beta.4-claude",
        "s3://bucket/artifacts/prod/orca/config.toml".to_string(),
        HashMap::new(),
        ecs_networking(),
        false,
    );

    // Fresh first version — `apply_ecs` increments generation on store.
    assert_eq!(manifest.metadata.generation, 0);
    // `oabctl create`'s own interactive-wizard defaults, ported verbatim.
    assert_eq!(manifest.spec.resources.cpu, "256");
    assert_eq!(manifest.spec.resources.memory, "512");
    match &manifest.spec.runtime {
        oabctl::manifest::Runtime::Ecs(rt) => {
            assert_eq!(rt.capacity_provider, "FARGATE");
            assert_eq!(rt.architecture, "X86_64");
            assert_eq!(rt.networking.subnets, vec!["subnet-aaa", "subnet-bbb"]);
            assert_eq!(rt.networking.security_groups, vec!["sg-orca"]);
            assert!(!rt.networking.assign_public_ip);
            assert!(rt.task_role_arn.is_none());
        }
        other => panic!("expected an ECS runtime manifest, got {other:?}"),
    }
    // No ingress (outbound-only agent); `bundle_from` stays unset here — the
    // caller wires it to the uploaded bundle's prefix before applying.
    assert!(manifest.spec.ingress.is_none());
    assert!(manifest.spec.bundle_from.is_none());
    assert!(manifest.spec.bootstrap_from.is_none());
    assert_eq!(manifest.spec.acp_enabled, Some(false));
    assert!(manifest.spec.secrets.is_empty());
}

#[test]
fn first_provision_manifest_acp_enabled_carries_the_auth_key_secret() {
    // `Spec.acp_enabled`'s contract: Some(true) requires a matching
    // OPENAB_ACP_AUTH_KEY entry in `secrets` (the default 0.0.0.0 bind is not
    // loopback, so keyless fail-open doesn't apply). The pure builder takes
    // the already-provisioned secrets map verbatim — pin that wiring.
    let mut secrets = HashMap::new();
    secrets.insert(
        "OPENAB_ACP_AUTH_KEY".to_string(),
        "aws-sm://oab/prod/orca#OPENAB_ACP_AUTH_KEY".to_string(),
    );
    let manifest = default_service_manifest(
        "prod",
        "orca",
        "ghcr.io/openabdev/openab:0.10.0-beta.4-claude",
        "s3://bucket/artifacts/prod/orca/config.toml".to_string(),
        secrets,
        ecs_networking(),
        true,
    );
    assert_eq!(manifest.spec.acp_enabled, Some(true));
    assert_eq!(
        manifest
            .spec
            .secrets
            .get("OPENAB_ACP_AUTH_KEY")
            .map(String::as_str),
        Some("aws-sm://oab/prod/orca#OPENAB_ACP_AUTH_KEY")
    );
}
