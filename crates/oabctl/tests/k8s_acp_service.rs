//! Integration tests for the k8s ACP Service (studio#155).
//!
//! These tests verify the Service shape, selector/port wiring, and the
//! apply-vs-prune decision without needing a live cluster — they only
//! exercise the pure functions in `k8s_driver.rs`.

use oabctl::k8s_driver::{acp_service_reconcile, build_acp_service, build_deployment, k8s_acp_url, k8s_deployment_name, AcpServiceReconcile};
use oabctl::manifest::{KubernetesRuntime, Metadata, Resources, OABServiceManifest, Runtime, Spec};

fn k8s_manifest(acp_enabled: Option<bool>) -> OABServiceManifest {
    OABServiceManifest {
        api_version: "oab.dev/v2".to_string(),
        kind: "OABService".to_string(),
        metadata: Metadata {
            name: "orca".to_string(),
            namespace: "prod".to_string(),
            generation: 0,
        },
        spec: Spec {
            image: "ghcr.io/openabdev/openab:latest".to_string(),
            resources: Resources {
                cpu: "500m".to_string(),
                memory: "512Mi".to_string(),
            },
            config_from: "s3://bucket/artifacts/prod/orca/config.toml".to_string(),
            bundle_from: None,
            bootstrap_from: None,
            secrets: std::collections::HashMap::new(),
            runtime: Runtime::Kubernetes(KubernetesRuntime {
                node_selector: Default::default(),
                service_account: None,
                tolerations: Vec::new(),
            }),
            ingress: None,
            acp_enabled,
        },
    }
}

#[test]
fn acp_service_exists_only_when_acp_enabled() {
    // acp_enabled = None → no Service
    let m = k8s_manifest(None);
    assert!(build_acp_service(&m).unwrap().is_none());

    // acp_enabled = Some(false) → no Service
    let m = k8s_manifest(Some(false));
    assert!(build_acp_service(&m).unwrap().is_none());

    // acp_enabled = Some(true) → Service exists
    let m = k8s_manifest(Some(true));
    let svc = build_acp_service(&m).unwrap().expect("acp_enabled=true → Service");
    assert_eq!(svc.metadata.name.as_deref(), Some("oab-orca"));
    assert_eq!(svc.metadata.namespace.as_deref(), Some("prod"));
}

#[test]
fn acp_service_is_clusterip_with_correct_port_and_selector() {
    let m = k8s_manifest(Some(true));
    let svc = build_acp_service(&m).unwrap().unwrap();

    let spec = svc.spec.unwrap();
    assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));

    let ports = spec.ports.unwrap();
    assert_eq!(ports.len(), 1);
    let port = &ports[0];
    assert_eq!(port.name.as_deref(), Some("acp"));
    assert_eq!(port.protocol.as_deref(), Some("TCP"));
    assert_eq!(port.port, 8080);
    assert_eq!(port.target_port, Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(8080)));

    // Selector matches the Deployment's pod template labels
    let selector = spec.selector.unwrap();
    assert_eq!(selector.get("app"), Some(&"oab-orca".to_string()));
    assert_eq!(selector.get("oab/name"), Some(&"orca".to_string()));
}

#[test]
fn acp_service_selector_matches_deployment_pod_labels() {
    let m = k8s_manifest(Some(true));
    let svc = build_acp_service(&m).unwrap().unwrap();
    let dep = build_deployment(&m).unwrap();

    let pod_labels = dep.spec.unwrap().template.metadata.unwrap().labels.unwrap();
    assert_eq!(svc.spec.unwrap().selector.unwrap(), pod_labels);
}

#[test]
fn acp_service_rejects_ecs_runtime() {
    let mut m = k8s_manifest(Some(true));
    m.spec.runtime = Runtime::Ecs(oabctl::manifest::EcsRuntime {
        capacity_provider: "FARGATE_SPOT".to_string(),
        architecture: "X86_64".to_string(),
        task_role_arn: None,
        networking: oabctl::manifest::EcsNetworking {
            subnets: vec!["subnet-1".to_string()],
            security_groups: vec!["sg-1".to_string()],
            assign_public_ip: false,
        },
    });
    let err = build_acp_service(&m).unwrap_err();
    assert!(err.to_string().contains("dispatch bug"));
}

#[test]
fn acp_off_reconcile_plans_prune_not_apply() {
    // acp_enabled = None → Prune
    let m = k8s_manifest(None);
    assert!(matches!(acp_service_reconcile(&m).unwrap(), AcpServiceReconcile::Prune));

    // acp_enabled = Some(false) → Prune
    let m = k8s_manifest(Some(false));
    assert!(matches!(acp_service_reconcile(&m).unwrap(), AcpServiceReconcile::Prune));

    // acp_enabled = Some(true) → Apply
    let m = k8s_manifest(Some(true));
    assert!(matches!(acp_service_reconcile(&m).unwrap(), AcpServiceReconcile::Apply(_)));
}

#[test]
fn k8s_acp_url_uses_ws_scheme_and_cluster_local_dns() {
    let url = k8s_acp_url("prod", "orca");
    assert_eq!(url, "ws://oab-orca.prod.svc.cluster.local:8080/acp");
}

#[test]
fn deployment_name_matches_service_name_for_acp() {
    // The Service shares the Deployment's `oab-<name>` slug
    assert_eq!(k8s_deployment_name("orca"), "oab-orca");
}