//! K8s implementation of `ProvisionDriver` (ADR #63 slice 3b).
//!
//! Deliberately narrow for this slice: `apply`/`scale`/`delete` against a k8s
//! `Deployment`, mirroring the shape `EcsDriver` already has.
//!
//! `spec.secrets` is wired (sub-slice 3d): each value must be
//! `k8s-secret://<secret-name>#<key>` (see `secrets::parse_k8s_secret_uri`)
//! and becomes an `env[].valueFrom.secretKeyRef` — the Secret object itself
//! must already exist in the target namespace; creating it is a separate
//! concern (same non-creating shape `aws-sm://` already has for ECS). ECS's
//! `aws-sm://`/raw-ARN values in a k8s-runtime manifest fail loudly at apply
//! time — a manifest error, not a silent no-op.
//!
//! `spec.configFrom` for k8s (studio#138 — Brett: "k8s does not need to
//! fetch from s3") is `k8s-configmap://<name>#<key>`: `build_deployment`
//! mounts that ConfigMap at `/etc/openab` and leaves the container command
//! unset entirely, so the image's own baked-in default CMD
//! (`openab run -c /etc/openab/config.toml`, confirmed straight from every
//! `Dockerfile.*` in openabdev/openab) does the reading — no S3, no AWS
//! credentials needed in the pod at all. This reverses the original
//! sub-slice 3c decision (which reused the S3-backed `hooks.pre_seed`
//! carrier verbatim from the ECS path, "orchestrator-agnostic by
//! construction") — that path remains supported for backward compatibility
//! (a `configFrom` still carrying a legacy `s3://`/`http(s)://` URI keeps
//! getting the command-override treatment) but is no longer what fresh k8s
//! deploys produce. `spec.bundleFrom` itself is still unused either way.
//!
//! There is no create-vs-redeploy manifest lookup for k8s (unlike ECS's
//! `redeploy()`, which reuses a stored desired-state YAML) — every field
//! `studio-cp::build_default_k8s_manifest` sets is already resent by its one
//! caller (the wizard) on every call, so a fresh manifest is rebuilt from
//! scratch every time; `apply()` below still reports Created-vs-Updated
//! correctly from the live Deployment's own existence, no stored manifest
//! needed for that either.
//!
//! studio#155: `apply` also creates a `ClusterIP` `Service` next to the
//! `Deployment` whenever `spec.acp_enabled` is set — the piece that makes the
//! pod's `/acp` port addressable at all (before this, a k8s agent got an ACP
//! auth key in its env but no reachable endpoint to pair with it; see
//! [`build_acp_service`] for the ClusterIP-vs-NodePort reasoning and
//! [`k8s_acp_url`] for the address `agents.toml` dials). `delete` removes the
//! Service alongside the Deployment so the endpoint doesn't dangle, and a
//! re-apply with `acp_enabled` off prunes the stale Service too.
//!
//! Observing k8s state into the canonical 6-state (the `apply`/`scale`
//! counterpart to `status.rs`'s ECS `service_status`/`instance_status`) is
//! also out of scope here — it's substantial enough on its own (a new
//! `RuntimeDriver` impl in `agent-lifecycle`, per the ADR-2 6-state⇄k8s
//! mapping table) to land as its own follow-up rather than growing this PR
//! further.

use crate::apply::{AppliedService, ApplyAction, ApplyReport};
use crate::driver::{ProvisionDriver, ProvisionOptions};
use crate::manifest::{OABServiceManifest, Runtime};
use anyhow::{Context, Result};
use async_trait::async_trait;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, Container, EnvVar, EnvVarSource, PodSpec, PodTemplateSpec,
    ResourceRequirements, SecretKeySelector, Service, ServicePort, ServiceSpec, Toleration, Volume,
    VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::{Client, Config};
use std::collections::BTreeMap;

/// Slugify an OAB agent name into a valid k8s object name (RFC 1123 lowercase
/// subdomain — `[a-z0-9]([-a-z0-9]*[a-z0-9])?`). The Agent name field is free
/// text (and its "suggest a Greek god name" default capitalizes — Brett hit
/// this directly: "Persephone-config" 422'd), so nothing upstream guarantees
/// it's already k8s-safe the way an ECS service name doesn't need to be.
/// Lowercases and maps any character outside `[a-z0-9-]` to `-`, then trims
/// leading/trailing `-` so the result still starts/ends alphanumeric (a
/// dash run in the middle is valid per the RFC, so no need to collapse
/// those).
pub fn k8s_safe_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// The k8s Deployment name for an OAB agent. OAB's own `metadata.namespace`
/// maps directly to the k8s namespace (a k8s namespace is already an
/// isolation/grouping boundary, same job OAB's `namespace` does for ECS
/// service naming) — so unlike `ecs_service_name` (`oab-{namespace}-{name}`,
/// flat because ECS has no per-namespace boundary), the Deployment only needs
/// `oab-{name}` within that namespace.
pub fn k8s_deployment_name(name: &str) -> String {
    format!("oab-{}", k8s_safe_name(name))
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

fn require_kubernetes_runtime(
    m: &OABServiceManifest,
) -> Result<&crate::manifest::KubernetesRuntime> {
    match &m.spec.runtime {
        Runtime::Kubernetes(rt) => Ok(rt),
        Runtime::Ecs(_) => anyhow::bail!(
            "K8sDriver got an ECS-runtime manifest ({}/{}) — dispatch bug, not a manifest error",
            m.metadata.namespace,
            m.metadata.name
        ),
    }
}

/// Build the `env[]` entries for `spec.secrets`: each value must be a
/// `k8s-secret://<secret-name>#<key>` ref, which becomes a `secretKeyRef` —
/// kubelet resolves it at pod-start time, no API call needed here (unlike
/// ECS's `aws-sm://`, which resolves to an ARN up front). Any other scheme
/// (an ECS `aws-sm://` ref left over from copy-pasting an ECS manifest, a
/// raw ARN, ...) is a manifest error, not silently dropped.
fn secret_env_vars(m: &OABServiceManifest) -> Result<Vec<EnvVar>> {
    m.spec
        .secrets
        .iter()
        .map(|(env_name, value)| {
            // parse_k8s_secret_uri returns Option<Result<..>>: None for the
            // wrong scheme, Some(Err(..)) for the right scheme but a
            // malformed body (e.g. missing '#'). anyhow's Context impl for
            // Option<T> only fires on None, so a chained `.with_context()??`
            // here would silently drop this context on the Some(Err(..))
            // path — attach it explicitly to both instead.
            let context = || {
                format!(
                    "spec.secrets['{env_name}'] for k8s runtime must use \
                     k8s-secret://<secret-name>#<key> (got '{value}') — '{}/{}'",
                    m.metadata.namespace, m.metadata.name
                )
            };
            let (secret_name, key) = crate::secrets::parse_k8s_secret_uri(value)
                .ok_or_else(|| anyhow::anyhow!(context()))?
                .with_context(context)?;
            Ok(EnvVar {
                name: env_name.clone(),
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: secret_name.to_string(),
                        key: key.to_string(),
                        optional: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            })
        })
        .collect()
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

/// The label set shared by the Deployment's `selector.matchLabels`, its pod
/// template, and the ACP `Service`'s selector — all three must agree or the
/// Service would select nothing (and the Deployment would violate its own
/// selector contract), so they come from one place.
fn pod_labels(m: &OABServiceManifest) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), k8s_deployment_name(&m.metadata.name));
    labels.insert("oab/name".to_string(), m.metadata.name.clone());
    labels
}

fn build_deployment(m: &OABServiceManifest) -> Result<Deployment> {
    let k8s_rt = require_kubernetes_runtime(m)?;
    let name = k8s_deployment_name(&m.metadata.name);

    let labels = pod_labels(m);

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
    // studio#119 follow-up: same as EcsDriver (apply.rs) — only ever push
    // "true", absence already means off (openab-gateway's own default).
    if m.spec.acp_enabled == Some(true) {
        env.push(EnvVar {
            name: "OPENAB_ACP_ENABLED".to_string(),
            value: Some("true".to_string()),
            ..Default::default()
        });
    }
    env.extend(secret_env_vars(m)?);

    // studio#138: a `k8s-configmap://<name>#<key>` configFrom mounts that
    // ConfigMap at /etc/openab and leaves the container command unset — the
    // image's own default CMD already reads /etc/openab/config.toml, so no
    // override, no S3, no AWS credentials needed in the pod. Anything else
    // non-empty (a legacy s3://... or http(s)://... configFrom) keeps the
    // old override-the-command behavior, same convention EcsDriver
    // (apply.rs) uses for its own s3:// support.
    let configmap_ref = match crate::secrets::parse_k8s_configmap_uri(&m.spec.config_from) {
        None => None,
        Some(Ok(v)) => Some(v),
        // Bake the agent name into the same message as the parse error —
        // anyhow's Display only surfaces the outermost `.with_context()`
        // frame, so a separate wrapper here would silently swallow the
        // parser's own detail (which scheme, which malformed part).
        Some(Err(e)) => {
            anyhow::bail!(
                "{e} — manifest '{}/{}'",
                m.metadata.namespace,
                m.metadata.name
            )
        }
    };
    let (command, volumes, volume_mounts) = if let Some((config_map_name, _key)) = configmap_ref {
        let volume = Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: config_map_name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mount = VolumeMount {
            name: "config".to_string(),
            mount_path: "/etc/openab".to_string(),
            read_only: Some(true),
            ..Default::default()
        };
        (None, Some(vec![volume]), Some(vec![mount]))
    } else {
        let command = (!m.spec.config_from.is_empty()).then(|| {
            vec![
                "openab".to_string(),
                "run".to_string(),
                "-c".to_string(),
                m.spec.config_from.clone(),
            ]
        });
        (command, None, None)
    };

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
        volume_mounts,
        ..Default::default()
    };

    let pod_spec = PodSpec {
        containers: vec![container],
        service_account_name: k8s_rt.service_account.clone(),
        node_selector: (!k8s_rt.node_selector.is_empty())
            .then(|| k8s_rt.node_selector.clone().into_iter().collect()),
        tolerations: (!tolerations.is_empty()).then_some(tolerations),
        volumes,
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

/// The `Service` exposing the pod's ACP/gateway port (studio#155). Without it
/// a k8s agent's `/acp` endpoint exists only inside the pod — nothing in the
/// cluster forwards traffic to it, so the `OPENAB_ACP_AUTH_KEY` the deploy
/// wires into env has no address to pair with.
///
/// `None` unless `spec.acp_enabled` is set: an ACP-less pod has no listener
/// behind the port, and a Service selecting it would be a dead endpoint that
/// still claims an address.
///
/// `ClusterIP`, deliberately not `NodePort`/`LoadBalancer` — the cluster-local
/// name `oab-<name>.<namespace>.svc.cluster.local` is already reachable from
/// the operator's machine on OrbStack (it routes `*.svc.cluster.local` and
/// ClusterIP addresses straight to the host) and everywhere else via
/// `kubectl port-forward svc/oab-<name> 8080:8080`, while NodePort/LB would
/// publish the bearer-authed port on every node IP or a provisioned LB — a
/// wider surface than a single-pod agent needs. A Studio-side dynamic
/// port-forward (the issue's candidate fix 2) stays a possible follow-up; the
/// Service also gives that tunnel a stable `svc/` target that survives pod
/// restarts, unlike `pod/<name>`.
pub fn build_acp_service(m: &OABServiceManifest) -> Result<Option<Service>> {
    require_kubernetes_runtime(m)?;
    if m.spec.acp_enabled != Some(true) {
        return Ok(None);
    }
    let labels = pod_labels(m);
    // The port the openab gateway listens on — the same value
    // `manifest::Ingress::container_port` defaults to on the ECS side; a k8s
    // manifest has no ingress block, so the image's own default is the only
    // value in play.
    let port = i32::from(crate::manifest::default_container_port());
    Ok(Some(Service {
        metadata: ObjectMeta {
            // Shares the Deployment's `oab-<name>` — different Kind, no
            // collision, and the name an operator guesses is the right one.
            name: Some(k8s_deployment_name(&m.metadata.name)),
            namespace: Some(m.metadata.namespace.clone()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".to_string()),
            selector: Some(labels),
            ports: Some(vec![ServicePort {
                name: Some("acp".to_string()),
                protocol: Some("TCP".to_string()),
                port,
                target_port: Some(IntOrString::Int(port)),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }))
}

/// What `apply` should do about the agent's ACP `Service` — the decision lives
/// in its own function so the contract is pinned by tests without needing a
/// live cluster.
enum AcpServiceReconcile {
    /// SSA-patch this Service and report its `webhook_urls` entry. `Box` keeps
    /// the enum small next to the data-less `Prune`.
    Apply(Box<Service>),
    /// Ensure the Service is absent. A redeploy that flipped
    /// `spec.acp_enabled` off must tear the stale Service down rather than
    /// leave it selecting pods that no longer listen on `/acp` — a dead
    /// endpoint that still claims a ClusterIP.
    Prune,
}

fn acp_service_reconcile(m: &OABServiceManifest) -> Result<AcpServiceReconcile> {
    match build_acp_service(m)? {
        Some(service) => Ok(AcpServiceReconcile::Apply(Box::new(service))),
        None => Ok(AcpServiceReconcile::Prune),
    }
}

/// The in-cluster `/acp` WebSocket URL the agent's Service exposes — the
/// address a `[[agent]].url` in `agents.toml` dials. `ws://` (not `wss://`):
/// service traffic inside the cluster is plaintext; the `openab.bearer.*`
/// sub-protocol carries the auth, and `RemoteConfig::validate` accepts only
/// ws/wss schemes anyway.
pub fn k8s_acp_url(namespace: &str, name: &str) -> String {
    format!(
        "ws://{}.{}.svc.cluster.local:{}/acp",
        k8s_deployment_name(name),
        namespace,
        crate::manifest::default_container_port()
    )
}

#[async_trait]
impl ProvisionDriver for K8sDriver {
    async fn apply(
        &self,
        manifests: &[OABServiceManifest],
        _opts: &ProvisionOptions,
    ) -> Result<ApplyReport> {
        let mut services = Vec::with_capacity(manifests.len());
        for m in manifests {
            let deployment = build_deployment(m)?;
            // Planned before any API call so a manifest error fails the whole
            // apply up front, not after a half-applied Deployment (studio#155).
            let acp_reconcile = acp_service_reconcile(m)?;
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

            // The Deployment exists now — reconcile its /acp exposure
            // (studio#155). Both arms are idempotent: server-side apply with
            // the Deployment's own field-manager, or a 404-tolerant delete for
            // a redeploy that turned acp off (and for never-acp manifests,
            // where the delete is just a no-op). The report's webhook_urls is
            // the "address next to the auth key" the issue was missing.
            let mut webhook_urls = Vec::new();
            let svc_api: Api<Service> = Api::namespaced(self.client.clone(), &m.metadata.namespace);
            match acp_reconcile {
                AcpServiceReconcile::Apply(service) => {
                    svc_api
                        .patch(
                            &name,
                            &PatchParams::apply("oabctl").force(),
                            &Patch::Apply(&service),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to apply k8s service '{name}' in namespace '{}'",
                                m.metadata.namespace
                            )
                        })?;
                    webhook_urls.push(k8s_acp_url(&m.metadata.namespace, &m.metadata.name));
                }
                AcpServiceReconcile::Prune => {
                    match svc_api.delete(&name, &DeleteParams::default()).await {
                        Ok(_) => {}
                        Err(kube::Error::Api(e)) if e.code == 404 => {}
                        Err(e) => {
                            return Err(e).with_context(|| {
                                format!(
                                    "failed to prune k8s service '{name}' in namespace '{}'",
                                    m.metadata.namespace
                                )
                            });
                        }
                    }
                }
            }

            services.push(AppliedService {
                namespace: m.metadata.namespace.clone(),
                name: m.metadata.name.clone(),
                resource_name: name,
                action: if existed {
                    ApplyAction::Updated
                } else {
                    ApplyAction::Created
                },
                webhook_urls,
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

    async fn delete(
        &self,
        resource: &str,
        name: &str,
        namespace: &str,
        _control_plane_bucket: &str,
    ) -> Result<()> {
        if resource != "oabservice" {
            anyhow::bail!("unknown resource type: {resource}. Use 'oabservice'");
        }
        let dep_name = k8s_deployment_name(name);
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        match api.delete(&dep_name, &DeleteParams::default()).await {
            Ok(_) => {}
            // Delete is idempotent — already gone is success, not an error.
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to delete k8s deployment '{dep_name}'"));
            }
        }
        // The ACP Service `apply` created next to the Deployment (studio#155) —
        // same name, same idempotent delete. Harmless when the agent never had
        // acp enabled (404 is success either way).
        let svc_api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        match svc_api.delete(&dep_name, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to delete k8s service '{dep_name}'")),
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
                acp_enabled: None,
            },
        }
    }

    #[test]
    fn deployment_name_is_oab_prefixed_without_namespace() {
        assert_eq!(k8s_deployment_name("orca"), "oab-orca");
    }

    #[test]
    fn deployment_name_lowercases_a_capitalized_agent_name() {
        // Brett hit this live: the Agent name field's "suggest a Greek god
        // name" default capitalizes ("Persephone"), which 422'd every k8s
        // object derived from it.
        assert_eq!(k8s_deployment_name("Persephone"), "oab-persephone");
    }

    #[test]
    fn k8s_safe_name_maps_invalid_characters_to_hyphens() {
        assert_eq!(k8s_safe_name("My Agent!"), "my-agent");
    }

    #[test]
    fn k8s_safe_name_trims_leading_and_trailing_hyphens() {
        assert_eq!(k8s_safe_name("-orca-"), "orca");
    }

    #[test]
    fn build_deployment_ignores_bundle_from_no_special_handling_needed() {
        // bundleFrom isn't consumed by the driver at all (see module docs) —
        // a manifest carrying it builds identically to one without.
        let with = k8s_manifest(Some("s3://bucket/artifacts/prod/orca/"), &[]);
        let without = k8s_manifest(None, &[]);
        assert_eq!(
            build_deployment(&with).unwrap(),
            build_deployment(&without).unwrap()
        );
    }

    #[test]
    fn build_deployment_wires_secret_key_ref() {
        let m = k8s_manifest(
            None,
            &[(
                "DISCORD_BOT_TOKEN",
                "k8s-secret://oab-orca#DISCORD_BOT_TOKEN",
            )],
        );
        let dep = build_deployment(&m).unwrap();
        let pod = dep.spec.unwrap().template.spec.unwrap();
        let env = pod.containers[0].env.as_ref().unwrap();
        let secret_env = env.iter().find(|e| e.name == "DISCORD_BOT_TOKEN").unwrap();
        let secret_ref = secret_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.name, "oab-orca");
        assert_eq!(secret_ref.key, "DISCORD_BOT_TOKEN");
    }

    #[test]
    fn build_deployment_rejects_non_k8s_secret_scheme() {
        let m = k8s_manifest(
            None,
            &[(
                "DISCORD_BOT_TOKEN",
                "aws-sm://oab/prod/orca#DISCORD_BOT_TOKEN",
            )],
        );
        let err = build_deployment(&m).unwrap_err();
        assert!(err.to_string().contains("k8s-secret://"));
    }

    #[test]
    fn build_deployment_attributes_malformed_k8s_secret_ref_to_its_env_var() {
        // Right scheme, malformed body (missing #key) — must still name the
        // offending spec.secrets entry, not just the bare parser error.
        let m = k8s_manifest(None, &[("DISCORD_BOT_TOKEN", "k8s-secret://oab-orca")]);
        let err = build_deployment(&m).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("DISCORD_BOT_TOKEN"),
            "must name the env var: {msg}"
        );
        assert!(msg.contains("prod/orca"), "must name the agent: {msg}");
    }

    #[test]
    fn build_deployment_mounts_k8s_native_configmap_and_skips_command_override() {
        let mut m = k8s_manifest(None, &[]);
        m.spec.config_from = "k8s-configmap://orca-config#config.toml".to_string();
        let dep = build_deployment(&m).unwrap();
        let pod = dep.spec.unwrap().template.spec.unwrap();

        // No command override — the image's own default CMD reads the
        // mounted /etc/openab/config.toml, no S3 involved.
        assert_eq!(pod.containers[0].command, None);

        let volumes = pod.volumes.unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "config");
        assert_eq!(volumes[0].config_map.as_ref().unwrap().name, "orca-config");

        let mounts = pod.containers[0].volume_mounts.as_ref().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "config");
        assert_eq!(mounts[0].mount_path, "/etc/openab");
        assert_eq!(mounts[0].read_only, Some(true));
    }

    #[test]
    fn build_deployment_rejects_malformed_k8s_configmap_ref() {
        let mut m = k8s_manifest(None, &[]);
        m.spec.config_from = "k8s-configmap://orca-config".to_string(); // missing #key
        let err = build_deployment(&m).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("k8s-configmap://"),
            "must name the scheme: {msg}"
        );
        assert!(msg.contains("prod/orca"), "must name the agent: {msg}");
    }

    #[test]
    fn build_deployment_sets_image_command_and_env() {
        let m = k8s_manifest(None, &[]);
        let dep = build_deployment(&m).unwrap();
        assert_eq!(dep.metadata.name.as_deref(), Some("oab-orca"));
        assert_eq!(dep.metadata.namespace.as_deref(), Some("prod"));

        let pod = dep.spec.unwrap().template.spec.unwrap();
        let container = &pod.containers[0];
        assert_eq!(
            container.image.as_deref(),
            Some("ghcr.io/openabdev/openab:latest")
        );
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
        let Runtime::Kubernetes(rt) = &mut m.spec.runtime else {
            unreachable!()
        };
        rt.service_account = Some("orca-sa".to_string());
        rt.node_selector
            .insert("kubernetes.io/arch".to_string(), "arm64".to_string());

        let dep = build_deployment(&m).unwrap();
        let pod = dep.spec.unwrap().template.spec.unwrap();
        assert_eq!(pod.service_account_name.as_deref(), Some("orca-sa"));
        assert_eq!(
            pod.node_selector
                .unwrap()
                .get("kubernetes.io/arch")
                .map(String::as_str),
            Some("arm64")
        );
    }

    #[test]
    fn acp_service_selector_matches_the_deployment_pod_labels() {
        // studio#155: the Service must select exactly the Deployment's pod
        // template labels — drift here silently produces a Service with zero
        // endpoints.
        let mut m = k8s_manifest(None, &[]);
        m.spec.acp_enabled = Some(true);
        let svc = build_acp_service(&m).unwrap().expect("acp on → Service");
        let dep = build_deployment(&m).unwrap();
        let pod_labels = dep.spec.unwrap().template.metadata.unwrap().labels.unwrap();
        assert_eq!(svc.spec.unwrap().selector.unwrap(), pod_labels);
    }

    #[test]
    fn acp_off_reconcile_plans_prune_not_apply() {
        // Review F2 (studio#155): a redeploy that turns acp off must produce a
        // Prune so apply tears down the stale Service — Apply would leave the
        // old selector routing to pods that no longer listen on /acp.
        let mut m = k8s_manifest(None, &[]);
        assert!(matches!(
            acp_service_reconcile(&m).unwrap(),
            AcpServiceReconcile::Prune
        ));
        m.spec.acp_enabled = Some(false);
        assert!(matches!(
            acp_service_reconcile(&m).unwrap(),
            AcpServiceReconcile::Prune
        ));
        m.spec.acp_enabled = Some(true);
        assert!(matches!(
            acp_service_reconcile(&m).unwrap(),
            AcpServiceReconcile::Apply(_)
        ));
    }

    #[test]
    fn acp_service_rejects_ecs_runtime_too() {
        let mut m = k8s_manifest(None, &[]);
        m.spec.acp_enabled = Some(true);
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
        let err = build_acp_service(&m).unwrap_err();
        assert!(err.to_string().contains("dispatch bug"));
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
