//! K8s implementation of `ProvisionDriver` (ADR #63 slice 3b).
//!
//! Deliberately narrow for this slice: `apply`/`scale`/`delete` against a k8s
//! `Deployment`, mirroring the shape `EcsDriver` already has. Two things a
//! manifest can carry are explicitly **not yet supported** and fail loudly
//! rather than silently mis-deploying:
//!
//! - `spec.bundleFrom` (the composed persona/skills bundle) — ECS gets this
//!   for free via its S3 file carrier; k8s needs a ConfigMap/volume carrier,
//!   tracked as sub-slice 3c.
//! - `spec.secrets` — ECS resolves these into `Secret.valueFrom` ARNs; a k8s
//!   target needs a different output shape (a Secret key selector), tracked
//!   as sub-slice 3d.
//!
//! Observing k8s state into the canonical 6-state (the `apply`/`scale`
//! counterpart to `status.rs`'s ECS `service_status`/`instance_status`) is
//! also out of scope here — it's substantial enough on its own (a new
//! `RuntimeDriver` impl in `agent-lifecycle`, per the ADR-2 6-state⇄k8s
//! mapping table) to land as its own follow-up rather than growing this PR
//! further.

use crate::apply::{ApplyAction, AppliedService, ApplyReport};
use crate::driver::{ProvisionDriver, ProvisionOptions};
use crate::manifest::{OABServiceManifest, Runtime};
use anyhow::{Context, Result};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, ResourceRequirements, Toleration,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::{Client, Config};
use std::collections::BTreeMap;

/// The k8s Deployment name for an OAB agent. OAB's own `metadata.namespace`
/// maps directly to the k8s namespace (a k8s namespace is already an
/// isolation/grouping boundary, same job OAB's `namespace` does for ECS
/// service naming) — so unlike `ecs_service_name` (`oab-{namespace}-{name}`,
/// flat because ECS has no per-namespace boundary), the Deployment only needs
/// `oab-{name}` within that namespace.
pub fn k8s_deployment_name(name: &str) -> String {
    format!("oab-{name}")
}

/// The k8s implementation. Bound to one kubeconfig context (and therefore one
/// cluster) at construction, same as `EcsDriver` is bound to one AWS
/// config+cluster — "which target" is a property of the driver instance, not
/// a per-call argument.
pub struct K8sDriver {
    client: Client,
}

impl K8sDriver {
    /// Build a client from a kubeconfig context. `context = None` uses
    /// whatever context is current in the resolved kubeconfig (`$KUBECONFIG`,
    /// falling back to `~/.kube/config`) — same "ambient default, explicit
    /// override" shape `aws_config` uses for region/profile resolution. This
    /// is exactly how orbstack's local cluster gets targeted: no special
    /// case, just another kubeconfig context.
    pub async fn from_context(context: Option<&str>) -> Result<Self> {
        let options = kube::config::KubeConfigOptions {
            context: context.map(str::to_string),
            ..Default::default()
        };
        let config = Config::from_kubeconfig(&options)
            .await
            .context("failed to resolve kubeconfig context")?;
        let client = Client::try_from(config).context("failed to build k8s client")?;
        Ok(Self { client })
    }
}

fn require_kubernetes_runtime(m: &OABServiceManifest) -> Result<&crate::manifest::KubernetesRuntime> {
    match &m.spec.runtime {
        Runtime::Kubernetes(rt) => Ok(rt),
        Runtime::Ecs(_) => anyhow::bail!(
            "K8sDriver got an ECS-runtime manifest ({}/{}) — dispatch bug, not a manifest error",
            m.metadata.namespace,
            m.metadata.name
        ),
    }
}

/// Reject the two not-yet-supported manifest features explicitly (see module
/// docs) instead of silently dropping them.
fn reject_unsupported(m: &OABServiceManifest) -> Result<()> {
    if m.spec.bundle_from.is_some() {
        anyhow::bail!(
            "k8s bundle carrier not implemented yet (studio#97 sub-slice 3c) — '{}/{}' has spec.bundleFrom set",
            m.metadata.namespace,
            m.metadata.name
        );
    }
    if !m.spec.secrets.is_empty() {
        anyhow::bail!(
            "k8s secret refs not implemented yet (studio#97 sub-slice 3d) — '{}/{}' has spec.secrets set",
            m.metadata.namespace,
            m.metadata.name
        );
    }
    Ok(())
}

fn resource_requirements(resources: &crate::manifest::Resources) -> ResourceRequirements {
    let mut quantities = BTreeMap::new();
    quantities.insert("cpu".to_string(), Quantity(resources.cpu.clone()));
    quantities.insert("memory".to_string(), Quantity(resources.memory.clone()));
    ResourceRequirements {
        limits: Some(quantities.clone()),
        requests: Some(quantities),
        ..Default::default()
    }
}

fn build_deployment(m: &OABServiceManifest) -> Result<Deployment> {
    let k8s_rt = require_kubernetes_runtime(m)?;
    let name = k8s_deployment_name(&m.metadata.name);

    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), name.clone());
    labels.insert("oab/name".to_string(), m.metadata.name.clone());

    let mut env = vec![
        EnvVar {
            name: "NAMESPACE".to_string(),
            value: Some(m.metadata.namespace.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "NAME".to_string(),
            value: Some(m.metadata.name.clone()),
            ..Default::default()
        },
    ];
    if let Some(bootstrap) = &m.spec.bootstrap_from {
        env.push(EnvVar {
            name: "BOOTSTRAP_FROM".to_string(),
            value: Some(bootstrap.clone()),
            ..Default::default()
        });
    }

    // Same convention as EcsDriver (apply.rs): the image's default CMD points
    // at a config.toml nothing populates, so override it to load configFrom
    // directly via openab's own s3:// support — no download step needed.
    let command = (!m.spec.config_from.is_empty()).then(|| {
        vec![
            "openab".to_string(),
            "run".to_string(),
            "-c".to_string(),
            m.spec.config_from.clone(),
        ]
    });

    let tolerations: Vec<Toleration> = k8s_rt
        .tolerations
        .iter()
        .map(|raw| {
            serde_yaml::from_value(raw.clone())
                .context("invalid runtime.tolerations entry — expected a k8s Toleration shape")
        })
        .collect::<Result<_>>()?;

    let container = Container {
        name: "openab".to_string(),
        image: Some(m.spec.image.clone()),
        command,
        env: Some(env),
        resources: Some(resource_requirements(&m.spec.resources)),
        ..Default::default()
    };

    let pod_spec = PodSpec {
        containers: vec![container],
        service_account_name: k8s_rt.service_account.clone(),
        node_selector: (!k8s_rt.node_selector.is_empty())
            .then(|| k8s_rt.node_selector.clone().into_iter().collect()),
        tolerations: (!tolerations.is_empty()).then_some(tolerations),
        ..Default::default()
    };

    Ok(Deployment {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(m.metadata.namespace.clone()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            // Same single-bot-token constraint as ECS: replicas is 0 or 1,
            // never more. `apply` always starts at 1; `scale` is the only
            // path that goes to 0.
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
            },
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[async_trait]
impl ProvisionDriver for K8sDriver {
    async fn apply(&self, manifests: &[OABServiceManifest], _opts: &ProvisionOptions) -> Result<ApplyReport> {
        let mut services = Vec::with_capacity(manifests.len());
        for m in manifests {
            reject_unsupported(m)?;
            let deployment = build_deployment(m)?;
            let name = k8s_deployment_name(&m.metadata.name);
            let api: Api<Deployment> = Api::namespaced(self.client.clone(), &m.metadata.namespace);

            let existed = api
                .get_opt(&name)
                .await
                .with_context(|| format!("failed to check for existing deployment '{name}'"))?
                .is_some();

            api.patch(
                &name,
                &PatchParams::apply("oabctl").force(),
                &Patch::Apply(&deployment),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to apply k8s deployment '{name}' in namespace '{}'",
                    m.metadata.namespace
                )
            })?;

            services.push(AppliedService {
                namespace: m.metadata.namespace.clone(),
                name: m.metadata.name.clone(),
                resource_name: name,
                action: if existed { ApplyAction::Updated } else { ApplyAction::Created },
                webhook_urls: Vec::new(),
                warnings: Vec::new(),
            });
        }
        Ok(ApplyReport { services })
    }

    async fn scale(&self, namespace: &str, name: &str, size: i32) -> Result<()> {
        if size != 0 && size != 1 {
            anyhow::bail!(
                "invalid size: {size}. OAB services scale only to 0 (off) or 1 (on) — \
                 each runs a single bot token and scaling above 1 duplicates responses."
            );
        }
        let dep_name = k8s_deployment_name(name);
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({ "spec": { "replicas": size } });
        api.patch(&dep_name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .with_context(|| format!("failed to scale k8s deployment '{dep_name}' to {size}"))?;
        Ok(())
    }

    async fn delete(&self, resource: &str, name: &str, namespace: &str, _control_plane_bucket: &str) -> Result<()> {
        if resource != "oabservice" {
            anyhow::bail!("unknown resource type: {resource}. Use 'oabservice'");
        }
        let dep_name = k8s_deployment_name(name);
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        match api.delete(&dep_name, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            // Delete is idempotent — already gone is success, not an error.
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to delete k8s deployment '{dep_name}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{KubernetesRuntime, Metadata, Resources, Spec};

    fn k8s_manifest(bundle_from: Option<&str>, secrets: &[(&str, &str)]) -> OABServiceManifest {
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
                bundle_from: bundle_from.map(str::to_string),
                bootstrap_from: None,
                secrets: secrets
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                runtime: Runtime::Kubernetes(KubernetesRuntime {
                    node_selector: Default::default(),
                    service_account: None,
                    tolerations: Vec::new(),
                }),
                ingress: None,
            },
        }
    }

    #[test]
    fn deployment_name_is_oab_prefixed_without_namespace() {
        assert_eq!(k8s_deployment_name("orca"), "oab-orca");
    }

    #[test]
    fn reject_unsupported_passes_a_plain_manifest() {
        let m = k8s_manifest(None, &[]);
        reject_unsupported(&m).unwrap();
    }

    #[test]
    fn reject_unsupported_bails_on_bundle_from() {
        let m = k8s_manifest(Some("s3://bucket/artifacts/prod/orca/"), &[]);
        let err = reject_unsupported(&m).unwrap_err();
        assert!(err.to_string().contains("3c"));
    }

    #[test]
    fn reject_unsupported_bails_on_secrets() {
        let m = k8s_manifest(None, &[("DISCORD_BOT_TOKEN", "aws-sm://oab/prod/orca#DISCORD_BOT_TOKEN")]);
        let err = reject_unsupported(&m).unwrap_err();
        assert!(err.to_string().contains("3d"));
    }

    #[test]
    fn build_deployment_sets_image_command_and_env() {
        let m = k8s_manifest(None, &[]);
        let dep = build_deployment(&m).unwrap();
        assert_eq!(dep.metadata.name.as_deref(), Some("oab-orca"));
        assert_eq!(dep.metadata.namespace.as_deref(), Some("prod"));

        let pod = dep.spec.unwrap().template.spec.unwrap();
        let container = &pod.containers[0];
        assert_eq!(container.image.as_deref(), Some("ghcr.io/openabdev/openab:latest"));
        assert_eq!(
            container.command.as_deref(),
            Some(
                [
                    "openab".to_string(),
                    "run".to_string(),
                    "-c".to_string(),
                    "s3://bucket/artifacts/prod/orca/config.toml".to_string(),
                ]
                .as_slice()
            )
        );
        let env_names: Vec<&str> = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(env_names, vec!["NAMESPACE", "NAME"]);
    }

    #[test]
    fn build_deployment_wires_service_account_and_node_selector() {
        let mut m = k8s_manifest(None, &[]);
        let Runtime::Kubernetes(rt) = &mut m.spec.runtime else { unreachable!() };
        rt.service_account = Some("orca-sa".to_string());
        rt.node_selector.insert("kubernetes.io/arch".to_string(), "arm64".to_string());

        let dep = build_deployment(&m).unwrap();
        let pod = dep.spec.unwrap().template.spec.unwrap();
        assert_eq!(pod.service_account_name.as_deref(), Some("orca-sa"));
        assert_eq!(
            pod.node_selector.unwrap().get("kubernetes.io/arch").map(String::as_str),
            Some("arm64")
        );
    }

    #[test]
    fn build_deployment_rejects_ecs_runtime() {
        let mut m = k8s_manifest(None, &[]);
        m.spec.runtime = Runtime::Ecs(crate::manifest::EcsRuntime {
            capacity_provider: "FARGATE_SPOT".to_string(),
            architecture: "X86_64".to_string(),
            task_role_arn: None,
            networking: crate::manifest::EcsNetworking {
                subnets: vec!["subnet-1".to_string()],
                security_groups: vec!["sg-1".to_string()],
                assign_public_ip: false,
            },
        });
        let err = build_deployment(&m).unwrap_err();
        assert!(err.to_string().contains("dispatch bug"));
    }
}
