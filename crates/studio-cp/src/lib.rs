//! Studio control-plane library.
//!
//! This crate is the **seam** between the two halves of PR #2:
//!
//! - [`oabctl`] (vendored) is the management + status engine. It exposes live
//!   deployment status as data via [`oabctl::service_status`].
//! - [`agent_lifecycle`] is the canonical instance-lifecycle vocabulary (the
//!   6-state model + 4-axis discriminator).
//!
//! Studio consumes oabctl here and — in the next slice — maps per-task
//! observation onto [`AgentState`]. Every Studio front-end (CLI / TUI / GUI, and
//! an MCP surface later) is a downstream client of this crate, so oabctl stays
//! clean and upstream-contributable.

pub use agent_lifecycle::AgentState;
pub use oabctl::{EcsEvent, ServiceStatus, DEFAULT_EVENTS_LOG_GROUP};

/// Observe all OAB services in `cluster` — a thin passthrough over oabctl's
/// library status API.
///
/// This is the entry point the 6-state mapping and the MCP surface build on.
/// Per-instance [`AgentState`] derivation needs per-task observation
/// (`DescribeTasks`) and lands in the next slice; today this returns the
/// service-level [`ServiceStatus`] oabctl already produces.
pub async fn observe_services(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
) -> anyhow::Result<Vec<ServiceStatus>> {
    oabctl::service_status(aws_config, cluster).await
}

pub use oabctl::InstanceStatus;

/// Map an oabctl ECS [`InstanceStatus`] onto the canonical [`AgentState`]
/// (ADR-2 read model: `DescribeTasks` → 4 discriminators → `phase`).
///
/// Only the **ECS-observable** axes are derived here: `last_status` (drives the
/// `identity_verified` latch and desired) and `health_status`. Admission
/// (`accepting_work`) and the CP lease are **app/CP-level**, not ECS-observable
/// (ADR-1 F2 / ADR-2 N1), so they default to admitting / valid; the control
/// plane overrides them.
pub fn instance_phase(inst: &InstanceStatus, verified_before: bool) -> AgentState {
    use agent_lifecycle::ecs::{EcsDriver, EcsHealth, EcsLastStatus, EcsTask};
    use agent_lifecycle::RuntimeDriver;

    let last_status = match inst.last_status.as_str() {
        "PROVISIONING" => EcsLastStatus::Provisioning,
        "PENDING" => EcsLastStatus::Pending,
        "ACTIVATING" => EcsLastStatus::Activating,
        "RUNNING" => EcsLastStatus::Running,
        "DEACTIVATING" => EcsLastStatus::Deactivating,
        "STOPPING" => EcsLastStatus::Stopping,
        _ => EcsLastStatus::Stopped,
    };
    let health = match inst.health_status.as_str() {
        "HEALTHY" => EcsHealth::Healthy,
        "UNHEALTHY" => EcsHealth::Unhealthy,
        _ => EcsHealth::Unknown,
    };
    let task = EcsTask {
        last_status,
        desired_status_stopped: inst.desired_stopped,
        health,
        health_check_defined: inst.health_check_defined,
        lease_valid: true,    // CP-level, not ECS-observable
        accepting_work: true, // CP-level admission, not ECS-observable
    };
    EcsDriver.project(&task, verified_before).classify()
}

/// One Instance's identity + phase within a Deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePhase {
    pub id: String,
    pub phase: AgentState,
}

/// The generic Deployment read-model (ADR-2 §4): Deployment-level replica
/// **counters** + per-Instance `phase`. A Deployment has *counts*, **not** an
/// `AgentState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub name: String,
    pub namespace: String,
    /// Desired replica count.
    pub desired: i32,
    /// Instances currently observed.
    pub current: i32,
    /// Instances whose `phase` is `Running`.
    pub ready: i32,
    pub instances: Vec<InstancePhase>,
}

/// One-shot approximation of the `identity_verified` latch from the current
/// `last_status`. The real latch needs CP-persisted history; here an Instance
/// counts as verified once its ECS `lastStatus` is at or past `RUNNING`.
fn latched_verified(last_status: &str) -> bool {
    matches!(last_status, "RUNNING" | "DEACTIVATING" | "STOPPING")
}

/// Build the Deployment read-model from service-level + per-Instance status.
pub fn build_deployment(svc: &ServiceStatus, instances: &[InstanceStatus]) -> Deployment {
    let instances: Vec<InstancePhase> = instances
        .iter()
        .map(|i| InstancePhase {
            id: i.id.clone(),
            phase: instance_phase(i, latched_verified(&i.last_status)),
        })
        .collect();
    let ready = instances
        .iter()
        .filter(|p| p.phase == AgentState::Running)
        .count() as i32;
    Deployment {
        name: svc.name.clone(),
        namespace: svc.namespace.clone(),
        desired: svc.desired,
        current: instances.len() as i32,
        ready,
        instances,
    }
}

/// Resolve a caller's `service` selector to the matched service, accepting
/// **either** the full ECS name (`oab-{ns}-{name}`) **or** the display short
/// name (`{name}`). [`observe_deployment`] passes the resolved service's
/// `service_name` straight to `instance_status` with no further transformation,
/// so a test over this fully covers the "a short selector must query tasks by
/// the full ECS name" guarantee.
fn resolve_service<'a>(service: &str, services: &'a [ServiceStatus]) -> Option<&'a ServiceStatus> {
    services
        .iter()
        .find(|s| service == s.service_name || service == s.name)
}

/// Observe one Deployment end-to-end: service-level counters + per-Instance
/// phases. `service` may be the full ECS name (`oab-{namespace}-{name}`) **or**
/// the display short name (`{name}`) — both resolve to the same Deployment.
pub async fn observe_deployment(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
    service: &str,
) -> anyhow::Result<Option<Deployment>> {
    let services = oabctl::service_status(aws_config, cluster).await?;
    let Some(svc) = resolve_service(service, &services) else {
        return Ok(None);
    };
    // Query tasks by the authoritative ECS service name ECS handed back — never
    // the caller's (possibly short) selector, and never a `format!`-rebuilt name
    // (wrong for services that don't fit the `oab-<ns>-<name>` shape). A short
    // name reaching `ListTasks` is exactly what surfaced as
    // `ServiceNotFoundException`.
    let instances = oabctl::instance_status(aws_config, cluster, &svc.service_name).await?;
    Ok(Some(build_deployment(svc, &instances)))
}

/// Observe recent ECS control-plane **events** for the cluster (optionally one
/// service) — the lifecycle timeline `observe_deployment` cannot show, read
/// back from the EventBridge → CloudWatch Logs archive. Thin passthrough to
/// oabctl; newest first. See [`oabctl::fetch_ecs_events`] for the caveat that
/// container-health flips are not emitted by ECS.
pub async fn observe_events(
    aws_config: &aws_config::SdkConfig,
    log_group: &str,
    cluster: &str,
    service: Option<&str>,
    since_ms: i64,
    limit: i32,
) -> anyhow::Result<Vec<EcsEvent>> {
    oabctl::fetch_ecs_events(aws_config, log_group, Some(cluster), service, since_ms, limit).await
}

// ---- Effective runtime identity/context (ADR: Per-Fleet managing identity) --
//
// Read-only observation of *who this control plane is actually acting as*. The
// silent-fallback incident (a static `[default]` profile shadowing the intended
// task role, an IAM user in the wrong account) had no signal surfacing the
// resolved principal; this makes it observable — the value a Fleet↔account
// binding is later reconciled against.

/// The **effective** runtime identity/context a driver resolved. Generic and
/// read-only; vendor specifics (STS ARN / account / region) are produced here
/// and surfaced upward unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContext {
    /// Effective acting principal — the STS caller ARN.
    pub principal: String,
    /// Coarse kind of the principal: `"role"` (assumed-role / role) vs `"user"`
    /// (static IAM user) vs `"unknown"`. A `user/...` where a task role was
    /// expected is the exact shape of the credential-fallback incident.
    pub principal_kind: String,
    /// Account / project boundary — the AWS account id.
    pub scope: String,
    /// Region / zone the config resolved to (empty if unset).
    pub location: String,
    /// Best-effort hint of where the credential came from, inferred from the
    /// process environment. Not authoritative — richer provenance is later work.
    pub source: String,
    /// Opaque STS caller `UserId` (support/correlation).
    pub caller_id: String,
}

/// Classify a caller ARN as a role vs a static user vs unknown.
fn principal_kind(arn: &str) -> &'static str {
    if arn.contains(":assumed-role/") || arn.contains(":role/") {
        "role"
    } else if arn.contains(":user/") {
        "user"
    } else {
        "unknown"
    }
}

/// Best-effort classification of the credential source from the environment. An
/// honest hint only: the AWS SDK does not expose which chain provider won, so we
/// infer from the same signals that decide it.
fn credential_source_hint() -> String {
    if std::env::var_os("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
        || std::env::var_os("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some()
    {
        "container-credentials (task/pod role)".to_string()
    } else if std::env::var_os("AWS_ACCESS_KEY_ID").is_some() {
        "static keys (env)".to_string()
    } else if let Ok(profile) = std::env::var("AWS_PROFILE") {
        format!("named profile: {profile}")
    } else {
        "default credential chain".to_string()
    }
}

/// Observe the **effective** runtime identity/context the given AWS config
/// resolves to: a live STS `GetCallerIdentity`, the config's region, and an
/// environment-derived source hint. Read-only — makes the resolved principal
/// visible (the signal the silent-fallback incident lacked).
pub async fn observe_identity(
    aws_config: &aws_config::SdkConfig,
) -> anyhow::Result<RuntimeContext> {
    let ident = aws_sdk_sts::Client::new(aws_config)
        .get_caller_identity()
        .send()
        .await?;
    let principal = ident.arn().unwrap_or_default().to_string();
    Ok(RuntimeContext {
        principal_kind: principal_kind(&principal).to_string(),
        principal,
        scope: ident.account().unwrap_or_default().to_string(),
        location: aws_config
            .region()
            .map(|r| r.as_ref().to_string())
            .unwrap_or_default(),
        source: credential_source_hint(),
        caller_id: ident.user_id().unwrap_or_default().to_string(),
    })
}

/// Classify a k8s username the same way `principal_kind` classifies an AWS
/// ARN: `"service-account"` (`system:serviceaccount:<ns>:<name>`) vs
/// `"user"` (anything else non-empty) vs `"unknown"`.
fn k8s_principal_kind(username: &str) -> &'static str {
    if username.starts_with("system:serviceaccount:") {
        "service-account"
    } else if !username.is_empty() {
        "user"
    } else {
        "unknown"
    }
}

/// Observe the **effective** runtime identity/context a kubeconfig context
/// resolves to — the k8s counterpart to [`observe_identity`], same
/// `RuntimeContext` shape (ADR-19's AWS-driver/k8s-driver field mapping
/// table). `context = None` uses the kubeconfig's `current-context`, mirroring
/// `K8sDriver::from_context`'s "ambient default, explicit override" shape.
///
/// `principal`/`caller_id` come from a live `SelfSubjectReview`
/// (`authentication.k8s.io/v1`, stable since k8s 1.28) — the literal API
/// `kubectl auth whoami` calls, so this is the same "ask the server who it
/// thinks I am" check `observe_identity`'s STS `GetCallerIdentity` does, not
/// a value read out of the kubeconfig file (which only says who you *meant*
/// to authenticate as).
pub async fn observe_k8s_identity(context: Option<&str>) -> anyhow::Result<RuntimeContext> {
    use k8s_openapi::api::authentication::v1::SelfSubjectReview;
    use kube::api::{Api, PostParams};

    let kubeconfig = kube::config::Kubeconfig::read()
        .map_err(|e| anyhow::anyhow!("failed to read kubeconfig: {e}"))?;
    let context_name = context
        .map(str::to_string)
        .or_else(|| kubeconfig.current_context.clone())
        .unwrap_or_default();
    let named_context = kubeconfig.contexts.iter().find(|c| c.name == context_name);
    let ctx = named_context.and_then(|c| c.context.as_ref());
    let scope = match ctx {
        Some(c) => format!("{}/{}", c.cluster, c.namespace.as_deref().unwrap_or("default")),
        None => String::new(),
    };

    let client = k8s_client_for(context)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve kubeconfig context '{context_name}': {e}"))?;

    let api: Api<SelfSubjectReview> = Api::all(client);
    let review = api
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
        .map_err(|e| anyhow::anyhow!("SelfSubjectReview (kubectl auth whoami) failed: {e}"))?;
    let user_info = review.status.and_then(|s| s.user_info).unwrap_or_default();
    let principal = user_info.username.unwrap_or_default();

    Ok(RuntimeContext {
        principal_kind: k8s_principal_kind(&principal).to_string(),
        principal,
        scope,
        // k8s has no first-class region/zone concept the way AWS does — a
        // cluster's server URL isn't reliably a region, so this stays empty
        // rather than guessing at one (same "empty if unset" contract
        // observe_identity already has for location).
        location: String::new(),
        source: format!("kubeconfig context: {context_name}"),
        caller_id: user_info.uid.unwrap_or_default(),
    })
}

// ---- Discovery: local AWS profiles / kubeconfig contexts (studio#104) -------
//
// Backs the "+ New fleet" console wizard's provider-specific `<select>`
// fields — the desktop app spawns `oab-mcp` as a local sidecar (`src-tauri/src/
// mcp.rs`), so these read the *operator's own machine*, not a remote server.
// Deliberately hand-rolled (not `aws-config`'s internal profile-file parser,
// which isn't a stable public surface) — same "pure, regex/line-based, easy to
// unit-test" spirit as `fleetToml.ts`'s client-side TOML edits.

/// One AWS credential profile discovered on the local machine.
pub struct AwsProfile {
    pub name: String,
    pub region: Option<String>,
}

/// Result of scanning `~/.aws/config` (+ `~/.aws/credentials` for profiles that
/// only exist there). `exists=false` means neither file was found — the caller
/// (console) shows "run `aws configure`" guidance rather than a bare error.
/// `error` is set only when a file exists but couldn't be read/parsed.
pub struct AwsProfilesResult {
    pub profiles: Vec<AwsProfile>,
    pub source_path: String,
    pub exists: bool,
    pub error: Option<String>,
}

fn aws_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".aws"))
}

/// Parse `~/.aws/config`'s `[default]` / `[profile <name>]` sections, pulling
/// out `region` when present. Comments (`#`/`;`) and blank lines are ignored;
/// unrecognized keys are skipped (this isn't a general INI parser, just enough
/// to answer "what profiles exist, with what region").
fn parse_aws_config(text: &str) -> Vec<AwsProfile> {
    let mut profiles = Vec::new();
    let mut current: Option<AwsProfile> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(p) = current.take() {
                profiles.push(p);
            }
            let name = header.strip_prefix("profile ").unwrap_or(header).trim();
            current = Some(AwsProfile {
                name: name.to_string(),
                region: None,
            });
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "region" {
                if let Some(p) = current.as_mut() {
                    p.region = Some(value.trim().to_string());
                }
            }
        }
    }
    if let Some(p) = current.take() {
        profiles.push(p);
    }
    profiles
}

/// Profile names from `~/.aws/credentials`'s `[<name>]` sections (bare, no
/// `profile ` prefix there) — covers profiles that only carry credentials with
/// no matching `~/.aws/config` entry (no region info available for these).
fn parse_aws_credentials_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|raw_line| {
            let line = raw_line.trim();
            line.strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .map(|name| name.trim().to_string())
        })
        .collect()
}

/// List AWS profiles discoverable on this machine, merging `~/.aws/config`
/// (name + region) with any profile-only-in-`~/.aws/credentials` names.
pub fn list_aws_profiles() -> AwsProfilesResult {
    let Some(dir) = aws_dir() else {
        return AwsProfilesResult {
            profiles: Vec::new(),
            source_path: String::new(),
            exists: false,
            error: None,
        };
    };
    let config_path = dir.join("config");
    let source_path = config_path.display().to_string();

    let mut profiles = match std::fs::read_to_string(&config_path) {
        Ok(text) => parse_aws_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return AwsProfilesResult {
                profiles: Vec::new(),
                source_path,
                exists: true,
                error: Some(e.to_string()),
            };
        }
    };

    if let Ok(creds_text) = std::fs::read_to_string(dir.join("credentials")) {
        for name in parse_aws_credentials_names(&creds_text) {
            if !profiles.iter().any(|p| p.name == name) {
                profiles.push(AwsProfile { name, region: None });
            }
        }
    }

    let exists = config_path.exists() || dir.join("credentials").exists();
    AwsProfilesResult {
        profiles,
        source_path,
        exists,
        error: None,
    }
}

/// One kubeconfig context discovered on the local machine.
pub struct K8sContextInfo {
    pub name: String,
    pub cluster: String,
    pub namespace: Option<String>,
    pub user: Option<String>,
}

/// Result of reading the local kubeconfig. `exists=false` when no kubeconfig
/// file was found at all (`KUBECONFIG` unset, `~/.kube/config` missing) — the
/// caller shows "install OrbStack/kind/minikube, or merge your cluster's
/// kubeconfig" guidance rather than a bare error.
pub struct K8sContextsResult {
    pub contexts: Vec<K8sContextInfo>,
    pub current_context: Option<String>,
    pub exists: bool,
    pub error: Option<String>,
}

/// List kubeconfig contexts discoverable on this machine (same "ambient
/// default, explicit override" kubeconfig `Kubeconfig::read()` `K8sDriver::
/// from_context`/`observe_k8s_identity` already use).
pub fn list_k8s_contexts() -> K8sContextsResult {
    match kube::config::Kubeconfig::read() {
        Ok(kubeconfig) => {
            let contexts = kubeconfig
                .contexts
                .iter()
                .filter_map(|c| {
                    let ctx = c.context.as_ref()?;
                    Some(K8sContextInfo {
                        name: c.name.clone(),
                        cluster: ctx.cluster.clone(),
                        namespace: ctx.namespace.clone(),
                        user: ctx.user.clone(),
                    })
                })
                .collect();
            K8sContextsResult {
                contexts,
                current_context: kubeconfig.current_context,
                exists: true,
                error: None,
            }
        }
        Err(e) => {
            // `kube`'s Kubeconfig::read() reports a missing file the same way
            // as a malformed one (both surface as an Err), so distinguish by
            // checking the well-known path ourselves rather than string-
            // matching the error — a real parse failure still reports here,
            // just via the fallback `exists` probe.
            let path = std::env::var_os("KUBECONFIG")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|h| std::path::PathBuf::from(h).join(".kube").join("config"))
                });
            let exists = path.is_some_and(|p| p.exists());
            K8sContextsResult {
                contexts: Vec::new(),
                current_context: None,
                exists,
                error: if exists { Some(e.to_string()) } else { None },
            }
        }
    }
}

async fn k8s_client_for(context: Option<&str>) -> anyhow::Result<kube::Client> {
    let options = kube::config::KubeConfigOptions {
        context: context.map(str::to_string),
        ..Default::default()
    };
    let config = kube::Config::from_kubeconfig(&options)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve kubeconfig context: {e}"))?;
    kube::Client::try_from(config).map_err(|e| anyhow::anyhow!("failed to build k8s client: {e}"))
}

/// List namespaces in the cluster the given kubeconfig context (or the
/// ambient current-context) resolves to. Backs the New Fleet wizard's
/// namespace `<select>` — a manual-entry fallback covers a namespace that
/// doesn't exist yet (this can only list what's already there).
pub async fn list_namespaces(context: Option<&str>) -> anyhow::Result<Vec<String>> {
    use k8s_openapi::api::core::v1::Namespace;
    use kube::api::{Api, ListParams};

    let client = k8s_client_for(context).await?;
    let api: Api<Namespace> = Api::all(client);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list namespaces: {e}"))?;
    Ok(list.items.into_iter().filter_map(|ns| ns.metadata.name).collect())
}

/// List service accounts in one namespace of the given kubeconfig context.
/// Backs the New Fleet wizard's (optional) service-account `<select>` — the
/// caller falls back to leaving it unset (the namespace's `default` service
/// account applies) on any error here, including an RBAC-denied `list`, so
/// this deliberately doesn't distinguish failure reasons the way
/// `list_aws_profiles`/`list_k8s_contexts` do.
pub async fn list_service_accounts(context: Option<&str>, namespace: &str) -> anyhow::Result<Vec<String>> {
    use k8s_openapi::api::core::v1::ServiceAccount;
    use kube::api::{Api, ListParams};

    let client = k8s_client_for(context).await?;
    let api: Api<ServiceAccount> = Api::namespaced(client, namespace);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| anyhow::anyhow!("failed to list service accounts: {e}"))?;
    Ok(list.items.into_iter().filter_map(|sa| sa.metadata.name).collect())
}

// ---- Fleet → managing-credential binding (ADR: Per-Fleet managing identity) --
//
// The *declarative* side of the loop: which credential should manage which
// fleet/cluster. Operator config, deliberately separate from the Fleet Store
// (observed membership/lease state); the two may fold together later. Selecting
// a binding is credential *selection*, not per-caller authz.

/// A declarative binding of a managed fleet to the credential that should manage
/// it, plus the fleet's members. Profile-first (assume-role is later work).
///
/// A fleet groups agents by *usage*, decoupled from the physical cluster: two
/// fleets may share a `cluster` (and one credential) while listing different
/// `members`. `members` empty ⇒ the fleet covers the whole cluster (back-compat
/// with the old cluster-granular binding).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FleetBinding {
    /// Fleet name — its identity (the `[fleet.<name>]` key, or the `name` field
    /// of a legacy `[[fleet]]` entry).
    #[serde(default)]
    pub name: String,
    /// ECS cluster this fleet's members live in (drives the managing credential;
    /// credential resolution stays cluster/account-granular).
    pub cluster: String,
    /// Service names in this fleet (e.g. `oab-prod-orca`). Empty ⇒ the whole
    /// cluster (legacy behavior).
    #[serde(default)]
    pub members: Vec<String>,
    /// Region to pin for this fleet.
    #[serde(default)]
    pub region: Option<String>,
    /// Named AWS profile that supplies the managing credential (profile-first).
    #[serde(default)]
    pub profile: Option<String>,
    /// Expected effective principal ARN — reconciled against the resolved
    /// identity later (IdentityMismatch). Optional.
    #[serde(default)]
    pub expected_principal: Option<String>,
}

impl FleetBinding {
    /// Whether a service belongs to this fleet, matched by **either** its full
    /// ECS name (`oab-{ns}-{name}`) or its short agent name — mirroring
    /// [`resolve_service`], which accepts both forms. An **empty** member list ⇒
    /// the fleet covers the whole cluster (legacy semantics), so everything
    /// matches.
    pub fn includes(&self, service_name: &str, short_name: &str) -> bool {
        self.members.is_empty()
            || self
                .members
                .iter()
                .any(|m| m == service_name || m == short_name)
    }
}

/// The body of a `[fleet.<name>]` table — the fields of a [`FleetBinding`] minus
/// `name`, which is the table key.
#[derive(Debug, Clone, serde::Deserialize)]
struct FleetBody {
    cluster: String,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    expected_principal: Option<String>,
}

/// Accepts both the current `[fleet.<name>]` map form and the legacy `[[fleet]]`
/// array form, so an existing config keeps parsing unchanged.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum FleetsDoc {
    /// `[fleet.<name>]` — name is the table key.
    Named {
        #[serde(default)]
        fleet: std::collections::BTreeMap<String, FleetBody>,
    },
    /// `[[fleet]]` — name is a field (legacy).
    Array {
        #[serde(default)]
        fleet: Vec<FleetBinding>,
    },
}

impl From<FleetsDoc> for FleetBindings {
    fn from(doc: FleetsDoc) -> Self {
        let fleets = match doc {
            FleetsDoc::Named { fleet } => fleet
                .into_iter()
                .map(|(name, b)| FleetBinding {
                    name,
                    cluster: b.cluster,
                    members: b.members,
                    region: b.region,
                    profile: b.profile,
                    expected_principal: b.expected_principal,
                })
                .collect(),
            FleetsDoc::Array { fleet } => fleet,
        };
        FleetBindings { fleets }
    }
}

/// Parsed fleet-binding file, canonicalized to a list. Deserializes from either
/// `[fleet.<name>]` (current) or `[[fleet]]` (legacy).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(from = "FleetsDoc")]
pub struct FleetBindings {
    pub fleets: Vec<FleetBinding>,
}

impl FleetBindings {
    /// The fleet governing `cluster`, if any (first match) — used for credential
    /// resolution, which stays cluster/account-granular.
    pub fn for_cluster(&self, cluster: &str) -> Option<&FleetBinding> {
        self.fleets.iter().find(|b| b.cluster == cluster)
    }

    /// The fleet whose explicit `members` contain `service`, if any.
    pub fn fleet_for_service(&self, service: &str) -> Option<&FleetBinding> {
        self.fleets
            .iter()
            .find(|b| b.members.iter().any(|m| m == service))
    }

    /// A fleet by name.
    pub fn get(&self, name: &str) -> Option<&FleetBinding> {
        self.fleets.iter().find(|b| b.name == name)
    }
}

/// Default fleet-binding config path: `$OAB_FLEETS_CONFIG`, else
/// `<config-dir>/oab-studio/fleets.toml` (`~/.config/oab-studio/fleets.toml`).
pub fn default_bindings_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("OAB_FLEETS_CONFIG") {
        return Some(std::path::PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("oab-studio").join("fleets.toml"))
}

/// Load fleet bindings from `path`. A missing file is **not** an error — it
/// yields an empty set (every target falls back to the default credential
/// chain), so bindings are strictly opt-in.
pub fn load_bindings(path: &std::path::Path) -> anyhow::Result<FleetBindings> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(toml::from_str(&content)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FleetBindings::default()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve the AWS config a binding selects — its named profile (profile-first)
/// and/or pinned region, layered on the standard chain. This is the **switch**:
/// calls for this fleet act as the bound credential instead of whatever the
/// ambient `[default]` resolves first.
pub async fn resolve_binding_config(binding: &FleetBinding) -> aws_config::SdkConfig {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
    if let Some(profile) = &binding.profile {
        loader = loader.profile_name(profile.as_str());
    }
    if let Some(region) = &binding.region {
        loader = loader.region(aws_config::Region::new(region.clone()));
    }
    loader.load().await
}

/// Does the resolved caller `actual` principal satisfy the `expected` principal
/// a binding declares? The read-only **reconcile** check (IdentityMismatch) —
/// a warning signal, never an authz gate (ADR-2 authz stays deferred).
///
/// Handles the STS assumed-role vs IAM role shape and a trailing `*` wildcard:
/// an expected `arn:aws:iam::A:role/R` (or `…:assumed-role/R/*`) matches an
/// actual `arn:aws:sts::A:assumed-role/R/SESSION`.
pub fn principal_matches(expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    if let Some(prefix) = expected.strip_suffix('*') {
        if actual.starts_with(prefix) {
            return true;
        }
    }
    match (role_identity(expected), role_identity(actual)) {
        (Some(e), Some(a)) => e == a,
        _ => false,
    }
}

/// Extract `(account, role_name)` from an IAM role or STS assumed-role ARN;
/// `None` for anything else (e.g. a static `user/…` ARN — which therefore never
/// matches a role expectation, exactly the fallback we want flagged).
fn role_identity(arn: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = arn.split(':').collect();
    if parts.len() < 6 {
        return None;
    }
    let account = parts[4].to_string();
    let resource = parts[5..].join(":");
    let name = if let Some(r) = resource.strip_prefix("assumed-role/") {
        r.split('/').next()?.to_string()
    } else if let Some(r) = resource.strip_prefix("role/") {
        r.to_string()
    } else {
        return None;
    };
    Some((account, name))
}

// ---- K8s fleet binding (ADR #63 slice 3f) --------------------------------
//
// Parallel, additive config surface for k8s-driven fleets — a **separate
// file** (`fleets-k8s.toml`, not a second table in `fleets.toml`). Kept
// separate deliberately: `save_bindings_text`/`save_k8s_bindings_text` are
// both whole-file verbatim writes, so if AWS and k8s bindings shared one
// file, saving either one from the console would silently clobber the
// other's edits (e.g. a k8s-only save wiping Brett's existing prod
// `[fleet.*]` entries). One file per driver makes that class of bug
// structurally impossible instead of relying on callers to merge carefully.
//
// A fleet is either AWS-driven (`FleetBinding`, `fleets.toml`) or k8s-driven
// (`K8sFleetBinding`, `fleets-k8s.toml`); nothing infers one from the other,
// and nothing here reads or writes `fleets.toml`.

/// A declarative binding of a k8s-driven fleet to the kubeconfig context that
/// should manage it, plus the fleet's members. `context`+`namespace` stand in
/// for `FleetBinding`'s `cluster`+`profile` — there's no AWS account/region
/// here, just "which kubeconfig context, and which namespace within it"
/// (namespace is OAB's own `namespace`, which maps directly onto the k8s
/// namespace — see `k8s_driver`'s module docs upstream in `oabctl`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct K8sFleetBinding {
    /// Fleet name — the `[fleet.<name>]` key.
    #[serde(default)]
    pub name: String,
    /// Kubeconfig context name. `None` = the kubeconfig's current-context —
    /// same "ambient default, explicit override" shape `K8sDriver::from_context`
    /// and `observe_k8s_identity` already use.
    #[serde(default)]
    pub context: Option<String>,
    /// k8s namespace this fleet's members live in.
    pub namespace: String,
    /// Agent names in this fleet. Empty ⇒ the whole namespace (mirrors
    /// `FleetBinding`'s empty-members-means-everything convention).
    #[serde(default)]
    pub members: Vec<String>,
}

impl K8sFleetBinding {
    /// Whether `agent_name` belongs to this fleet. An **empty** member list ⇒
    /// the fleet covers the whole namespace (mirrors `FleetBinding::includes`).
    pub fn includes(&self, agent_name: &str) -> bool {
        self.members.is_empty() || self.members.iter().any(|m| m == agent_name)
    }
}

/// The body of a `[fleet.<name>]` table in `fleets-k8s.toml` — the fields of
/// a [`K8sFleetBinding`] minus `name`, which is the table key.
#[derive(Debug, Clone, serde::Deserialize)]
struct K8sFleetBody {
    #[serde(default)]
    context: Option<String>,
    namespace: String,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(serde::Deserialize)]
struct K8sFleetsDoc {
    #[serde(default)]
    fleet: std::collections::BTreeMap<String, K8sFleetBody>,
}

impl From<K8sFleetsDoc> for K8sFleetBindings {
    fn from(doc: K8sFleetsDoc) -> Self {
        K8sFleetBindings {
            fleets: doc
                .fleet
                .into_iter()
                .map(|(name, b)| K8sFleetBinding {
                    name,
                    context: b.context,
                    namespace: b.namespace,
                    members: b.members,
                })
                .collect(),
        }
    }
}

/// Parsed k8s-fleet-binding file, canonicalized to a list. Deserializes from
/// `[fleet.<name>]` (only form — no legacy array form, unlike `FleetBindings`,
/// since there's no pre-existing k8s config to stay compatible with).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(from = "K8sFleetsDoc")]
pub struct K8sFleetBindings {
    pub fleets: Vec<K8sFleetBinding>,
}

impl K8sFleetBindings {
    /// The fleet whose explicit `members` contain `agent_name`, if any.
    pub fn fleet_for_agent(&self, agent_name: &str) -> Option<&K8sFleetBinding> {
        self.fleets.iter().find(|b| b.includes(agent_name))
    }

    /// A fleet by name.
    pub fn get(&self, name: &str) -> Option<&K8sFleetBinding> {
        self.fleets.iter().find(|b| b.name == name)
    }
}

/// Default k8s-fleet-binding config path: `$OAB_K8S_FLEETS_CONFIG`, else
/// `<config-dir>/oab-studio/fleets-k8s.toml`. Deliberately a different file
/// from `default_bindings_path()` — see module docs above.
pub fn default_k8s_bindings_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("OAB_K8S_FLEETS_CONFIG") {
        return Some(std::path::PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("oab-studio").join("fleets-k8s.toml"))
}

/// Load k8s fleet bindings from `path`. A missing file is **not** an error —
/// it yields an empty set, so bindings are strictly opt-in (mirrors
/// `load_bindings`).
pub fn load_k8s_bindings(path: &std::path::Path) -> anyhow::Result<K8sFleetBindings> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(toml::from_str(&content)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(K8sFleetBindings::default()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve the k8s driver a binding selects — a `K8sDriver` bound to its
/// kubeconfig context. This is the k8s counterpart to
/// `resolve_binding_config`'s AWS `SdkConfig` resolution: the **switch**,
/// calls for this fleet act against the bound context instead of whatever
/// kubeconfig `current-context` happens to be ambient. Orbstack's local
/// cluster is targeted exactly this way — just a context name, no
/// special-casing.
pub async fn resolve_binding_driver(binding: &K8sFleetBinding) -> anyhow::Result<oabctl::K8sDriver> {
    oabctl::K8sDriver::from_context(binding.context.as_deref()).await
}

/// Validate `text` parses as a k8s-bindings file and, if so, persist it
/// verbatim to `fleets-k8s.toml` (never `fleets.toml` — see module docs
/// above), returning the parsed set. Mirrors `save_bindings_text`.
pub fn save_k8s_bindings_text(
    path: &std::path::Path,
    text: &str,
) -> anyhow::Result<K8sFleetBindings> {
    let parsed: K8sFleetBindings = toml::from_str(text)?;
    write_bindings_atomic(path, text)?;
    Ok(parsed)
}

// ---- Fleet-binding editing (raw-text, whole-file) ------------------------
//
// The write half of the config panel: the operator edits `fleets.toml` as text
// (a TOML editor in the UI), and we persist their exact bytes after validating
// they parse — so comments and layout are preserved trivially (verbatim write),
// with no format-preserving library. Still operator credential *selection*, not
// per-caller authz (ADR-2 authz stays deferred).

/// The raw text of the bindings file, or an empty string when it doesn't exist
/// yet (so the editor opens a blank buffer rather than erroring).
pub fn read_bindings_text(path: &std::path::Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Write `text` to `path` atomically: create the parent dir if needed, write a
/// sibling temp file, then rename over the target so a crash can't leave a
/// half-written config.
pub fn write_bindings_atomic(path: &std::path::Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("fleets.toml");
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Validate `text` parses as a bindings file and, if so, persist it verbatim,
/// returning the parsed set for the caller to hot-reload. A parse error is
/// returned **before** anything is written, so a bad edit never lands on disk.
pub fn save_bindings_text(
    path: &std::path::Path,
    text: &str,
) -> anyhow::Result<FleetBindings> {
    let parsed: FleetBindings = toml::from_str(text)?;
    write_bindings_atomic(path, text)?;
    Ok(parsed)
}

// ---- Write side (ADR-2 write model) ------------------------------------
//
// The read side above observes; these mutate. Each is a thin passthrough to
// oabctl's programmatic `studio_api` — Studio owns the vocabulary and the MCP
// surface, oabctl owns the AWS reconciliation.

pub use oabctl::{ApplyOptions, ApplyReport};

/// Apply one or more service manifests (create/update).
///
/// Parses the YAML document into manifests, then runs oabctl's **programmatic**
/// apply (no stdout/stderr side effects) and returns the structured
/// [`ApplyReport`]. An `OABFleet` document applies every expanded service.
pub async fn apply_deployment(
    aws_config: &aws_config::SdkConfig,
    manifest_yaml: &str,
    cluster: &str,
    wait: bool,
) -> anyhow::Result<ApplyReport> {
    let manifests = oabctl::studio_api::parse_manifests(manifest_yaml)?;
    let opts = ApplyOptions::new(cluster).with_wait(wait);
    oabctl::apply_manifests(aws_config, &manifests, &opts)
        .await
        .map_err(|e| anyhow::anyhow!("apply failed [{:?}]: {e}", e.kind))
}

pub use studio_compose::Library;

/// Structured outcome of a [`provision_from_library`] call — enough for the UI to
/// confirm what was deployed without holding the bundle bytes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvisionOutcome {
    /// The image tag the service was pointed at.
    pub image: String,
    /// Content-address of the composed bundle (`sha256:…`).
    pub digest: String,
    /// Number of bundle files uploaded to the agent's artifacts prefix.
    pub objects: usize,
    /// Number of ECS services reconciled (1 for a single agent).
    pub services_applied: usize,
    /// The reconcile action on the (first) service, e.g. `Created` / `Updated`.
    pub action: String,
}

/// Provision an agent from the compose **library**: compose `template ⊕ overlay`,
/// then **redeploy** — push the bundle to the agent's artifacts prefix and apply
/// its stored manifest at the chosen image tag (agent-deployment ADR slice 2,
/// path A). Networking/resources/secrets ride along from the stored manifest, so
/// this is the "update this agent to new persona / skills / image" path; the
/// agent must already have been `create`d.
///
/// `image_override` (when non-empty) wins over the bundle's own default image tag.
pub async fn provision_from_library(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
    namespace: &str,
    name: &str,
    library: &Library,
    template: &str,
    overlay: Option<&str>,
    image_override: Option<&str>,
) -> anyhow::Result<ProvisionOutcome> {
    let mut bundle = studio_compose::compose_named(library, template, overlay)
        .map_err(|e| anyhow::anyhow!("compose failed: {e}"))?;
    let image = image_override
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| bundle.image_tag.clone());

    // Resolve the bucket once here (rather than letting `redeploy` resolve it
    // internally, as before) so the zip's S3 URI can be computed and wired
    // into config.toml *before* upload. See oabctl::studio_api::bundle_zip_uri
    // for why this — not the loose-file artifact_objects prefix — is what
    // actually gets restored on the deployed agent at boot.
    //
    // Patch `bundle.files` itself (not a derived copy) *before* deriving
    // artifact_objects/zip_bytes/digest from it, so all three agree on the
    // same, actually-uploaded content — deriving them from the bundle
    // pre-patch (as an earlier version of this function did) meant the
    // uploaded zip's own config.toml lacked the hook, and the reported
    // digest didn't match what was actually deployed.
    let bucket = oabctl::resolve_bucket(aws_config, None).await?;
    let zip_uri = oabctl::studio_api::bundle_zip_uri(&bucket, namespace, name);
    match bundle.files.get_mut("config.toml") {
        Some(bytes) => *bytes = oabctl::studio_api::inject_pre_seed_hook(bytes, &zip_uri)?,
        None => anyhow::bail!("composed bundle for {namespace}/{name} has no config.toml — cannot wire hooks.pre_seed"),
    }

    let mut objects = bundle.artifact_objects(namespace, name);
    let zip_key = format!(
        "{}/{}",
        studio_compose::artifacts_prefix(namespace, name),
        oabctl::studio_api::BUNDLE_ZIP_FILENAME
    );
    objects.push((zip_key, bundle.zip_bytes()));

    let digest = bundle.digest();

    let report = oabctl::studio_api::redeploy(
        aws_config,
        cluster,
        namespace,
        name,
        Some(&image),
        &objects,
        Some(&bucket),
    )
    .await?;

    Ok(ProvisionOutcome {
        image,
        digest,
        objects: objects.len(),
        services_applied: report.services.len(),
        action: report
            .services
            .first()
            .map(|s| format!("{:?}", s.action))
            .unwrap_or_default(),
    })
}

/// Scale an OAB service to `size` replicas (0 = off, 1 = on).
///
/// Config-free: `cluster` / `namespace` are explicit (service = `oab-{namespace}-{name}`).
pub async fn scale_deployment(
    aws_config: &aws_config::SdkConfig,
    cluster: &str,
    namespace: &str,
    name: &str,
    size: i32,
) -> anyhow::Result<()> {
    oabctl::studio_api::scale(aws_config, cluster, namespace, name, size).await
}

/// Delete a control-plane resource (e.g. an `OABService`).
///
/// The control-plane bucket is resolved from the environment / account, not
/// from `~/.oabctl/config.toml`.
pub async fn delete_deployment(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: &str,
    cluster: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    oabctl::studio_api::delete(aws_config, resource, name, cluster, namespace, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(last: &str, health: &str, stopped: bool) -> InstanceStatus {
        InstanceStatus {
            id: "arn".into(),
            last_status: last.into(),
            health_status: health.into(),
            health_check_defined: false, // default: no ECS health check (common for OAB agents)
            desired_stopped: stopped,
            stop_code: None,
        }
    }

    #[test]
    fn activating_maps_to_starting() {
        assert_eq!(
            instance_phase(&inst("ACTIVATING", "UNKNOWN", false), false),
            AgentState::Starting
        );
    }

    #[test]
    fn running_healthy_maps_to_running() {
        assert_eq!(
            instance_phase(&inst("RUNNING", "HEALTHY", false), true),
            AgentState::Running
        );
    }

    #[test]
    fn running_unhealthy_after_verified_maps_to_unhealthy() {
        assert_eq!(
            instance_phase(&inst("RUNNING", "UNHEALTHY", false), true),
            AgentState::Unhealthy
        );
    }

    #[test]
    fn running_unknown_without_health_check_maps_to_running() {
        // The reported bug: orca/mira run fine but define no ECS health check, so
        // healthStatus is UNKNOWN forever — that must read as Running, not Unhealthy.
        assert_eq!(
            instance_phase(&inst("RUNNING", "UNKNOWN", false), true),
            AgentState::Running
        );
    }

    #[test]
    fn running_unknown_with_defined_health_check_maps_to_unhealthy() {
        // A *defined* check reporting UNKNOWN (lost signal) still fences.
        let i = InstanceStatus {
            health_check_defined: true,
            ..inst("RUNNING", "UNKNOWN", false)
        };
        assert_eq!(instance_phase(&i, true), AgentState::Unhealthy);
    }

    #[test]
    fn desired_stopped_maps_to_stopping() {
        assert_eq!(
            instance_phase(&inst("DEACTIVATING", "HEALTHY", true), true),
            AgentState::Stopping
        );
    }

    fn svc(desired: i32, running: i32) -> ServiceStatus {
        ServiceStatus {
            name: "orca".into(),
            namespace: "prod".into(),
            service_name: "oab-prod-orca".into(),
            cpu: "512".into(),
            memory: "1024".into(),
            capacity: "FARGATE".into(),
            running,
            desired,
            status: "ACTIVE".into(),
        }
    }

    fn svc_named(namespace: &str, name: &str) -> ServiceStatus {
        ServiceStatus {
            service_name: format!("oab-{namespace}-{name}"),
            name: name.into(),
            namespace: namespace.into(),
            ..svc(1, 1)
        }
    }

    #[test]
    fn observe_deployment_resolves_selector_to_full_ecs_service_name() {
        let services = vec![svc_named("prod", "orca"), svc_named("prod", "mira")];

        // `observe_deployment` passes the resolved service's `service_name`
        // verbatim to `instance_status`/`ListTasks`. Regression: a display short
        // name (`orca`) must resolve to the FULL ECS name, or ECS 404s with
        // `ServiceNotFoundException` (pre-fix it queried with the short name).
        assert_eq!(
            resolve_service("orca", &services)
                .expect("short name matches")
                .service_name,
            "oab-prod-orca"
        );

        // The full name resolves to itself.
        assert_eq!(
            resolve_service("oab-prod-mira", &services)
                .expect("full name matches")
                .service_name,
            "oab-prod-mira"
        );

        // An unknown selector is a clean miss — the Deployment then reports
        // not-found rather than issuing a doomed ECS query.
        assert!(resolve_service("nope", &services).is_none());
    }

    #[test]
    fn build_deployment_counts_ready_and_phases() {
        let insts = vec![
            inst("RUNNING", "HEALTHY", false),
            inst("ACTIVATING", "UNKNOWN", false),
        ];
        let d = build_deployment(&svc(2, 1), &insts);
        assert_eq!(d.desired, 2);
        assert_eq!(d.current, 2);
        assert_eq!(d.ready, 1); // one Running, one Starting
        assert_eq!(d.instances[0].phase, AgentState::Running);
        assert_eq!(d.instances[1].phase, AgentState::Starting);
    }

    #[test]
    fn principal_kind_distinguishes_role_from_static_user() {
        assert_eq!(
            principal_kind("arn:aws:sts::504190915686:assumed-role/openab-orca-task-role/sid"),
            "role"
        );
        assert_eq!(
            principal_kind("arn:aws:iam::916371022086:user/brett.chien"),
            "user"
        );
        assert_eq!(principal_kind("arn:aws:iam::1:root"), "unknown");
    }

    #[test]
    fn k8s_principal_kind_distinguishes_service_account_from_user() {
        assert_eq!(
            k8s_principal_kind("system:serviceaccount:prod:orca-sa"),
            "service-account"
        );
        assert_eq!(k8s_principal_kind("brett@example.com"), "user");
        assert_eq!(k8s_principal_kind(""), "unknown");
    }

    #[test]
    fn bindings_parse_and_match_by_cluster() {
        let doc = r#"
[[fleet]]
name = "prod"
cluster = "oab"
region = "ap-east-2"
profile = "orca-prod"

[[fleet]]
name = "sg"
cluster = "oab-sg"
profile = "appier-sg"
"#;
        let b: FleetBindings = toml::from_str(doc).expect("parse");
        assert_eq!(b.fleets.len(), 2);
        let prod = b.for_cluster("oab").expect("prod binding");
        assert_eq!(prod.profile.as_deref(), Some("orca-prod"));
        assert_eq!(prod.region.as_deref(), Some("ap-east-2"));
        assert!(b.for_cluster("nope").is_none());
        // legacy entries have no explicit members ⇒ whole-cluster fleet
        assert!(prod.members.is_empty());
    }

    #[test]
    fn named_fleets_parse_with_members_and_share_a_cluster() {
        // The new `[fleet.<name>]` form: orca and mira are two fleets on one
        // cluster, grouped by explicit members.
        let doc = r#"
[fleet.orca]
cluster = "oab"
region = "ap-east-2"
profile = "oab-fleet"
members = ["oab-prod-orca"]

[fleet.mira]
cluster = "oab"
region = "ap-east-2"
profile = "oab-fleet"
members = ["oab-prod-mira"]
"#;
        let b: FleetBindings = toml::from_str(doc).expect("parse named fleets");
        assert_eq!(b.fleets.len(), 2);
        let orca = b.get("orca").expect("orca fleet");
        assert_eq!(orca.cluster, "oab");
        assert_eq!(orca.members, vec!["oab-prod-orca".to_string()]);
        assert_eq!(orca.profile.as_deref(), Some("oab-fleet"));
        // both fleets resolve to the same cluster (shared credential)
        assert_eq!(b.get("mira").unwrap().cluster, "oab");
        // membership routing
        assert_eq!(
            b.fleet_for_service("oab-prod-mira").map(|f| f.name.as_str()),
            Some("mira")
        );
        assert!(b.fleet_for_service("oab-prod-nope").is_none());
        // credential resolution still finds a governing fleet by cluster
        assert!(b.for_cluster("oab").is_some());
    }

    #[test]
    fn binding_includes_matches_full_or_short_name_and_whole_cluster() {
        let orca = FleetBinding {
            name: "orca".into(),
            cluster: "oab".into(),
            members: vec!["oab-prod-orca".into()],
            region: None,
            profile: None,
            expected_principal: None,
        };
        // full ECS name and short agent name both match (mirrors resolve_service)
        assert!(orca.includes("oab-prod-orca", "orca"));
        // a co-located non-member is excluded — this is what keeps two fleets on
        // one cluster distinct
        assert!(!orca.includes("oab-prod-mira", "mira"));

        // empty members ⇒ whole cluster: everything matches (legacy semantics)
        let whole = FleetBinding {
            name: "prod".into(),
            cluster: "oab".into(),
            members: vec![],
            region: None,
            profile: None,
            expected_principal: None,
        };
        assert!(whole.includes("oab-prod-orca", "orca"));
        assert!(whole.includes("anything", "at-all"));
    }

    #[test]
    fn empty_config_and_no_fleet_key_parse_to_empty() {
        assert!(toml::from_str::<FleetBindings>("").unwrap().fleets.is_empty());
        assert!(toml::from_str::<FleetBindings>("# just a comment\n")
            .unwrap()
            .fleets
            .is_empty());
    }

    #[test]
    fn k8s_fleets_parse_with_context_and_members() {
        let doc = r#"
[fleet.orbstack-dev]
context = "orbstack"
namespace = "dev"
members = ["scratch-agent"]

[fleet.orca-k8s]
namespace = "prod"
"#;
        let b: K8sFleetBindings = toml::from_str(doc).expect("parse");
        assert_eq!(b.fleets.len(), 2);
        let dev = b.get("orbstack-dev").expect("orbstack-dev fleet");
        assert_eq!(dev.context.as_deref(), Some("orbstack"));
        assert_eq!(dev.namespace, "dev");
        assert_eq!(dev.members, vec!["scratch-agent".to_string()]);
        // context omitted ⇒ None (kubeconfig current-context), same as
        // K8sDriver::from_context's "ambient default" contract
        let prod = b.get("orca-k8s").expect("orca-k8s fleet");
        assert_eq!(prod.context, None);
    }

    #[test]
    fn k8s_binding_includes_matches_by_name_or_whole_namespace() {
        let scoped = K8sFleetBinding {
            name: "dev".into(),
            context: Some("orbstack".into()),
            namespace: "dev".into(),
            members: vec!["scratch-agent".into()],
        };
        assert!(scoped.includes("scratch-agent"));
        assert!(!scoped.includes("other-agent"));

        let whole = K8sFleetBinding {
            name: "prod".into(),
            context: None,
            namespace: "prod".into(),
            members: vec![],
        };
        assert!(whole.includes("anything"));
    }

    #[test]
    fn empty_k8s_config_and_no_fleet_key_parse_to_empty() {
        assert!(toml::from_str::<K8sFleetBindings>("").unwrap().fleets.is_empty());
        assert!(toml::from_str::<K8sFleetBindings>("# just a comment\n")
            .unwrap()
            .fleets
            .is_empty());
    }

    #[test]
    fn load_k8s_bindings_missing_file_is_empty() {
        let path = std::env::temp_dir().join("oab-k8s-fleets-does-not-exist-xyz.toml");
        let _ = std::fs::remove_file(&path);
        assert!(load_k8s_bindings(&path).unwrap().fleets.is_empty());
    }

    #[test]
    fn save_k8s_bindings_round_trips_and_preserves_text_verbatim() {
        let dir = std::env::temp_dir().join(format!("oab-k8s-fleets-save-{}", std::process::id()));
        let path = dir.join("fleets-k8s.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let text = "# my k8s fleets\n\n[fleet.dev]\ncontext = \"orbstack\"\nnamespace = \"dev\"\n";
        let parsed = save_k8s_bindings_text(&path, text).expect("save");
        assert_eq!(parsed.fleets.len(), 1);
        assert_eq!(parsed.get("dev").unwrap().namespace, "dev");
        assert_eq!(read_bindings_text(&path).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn k8s_bindings_file_is_separate_from_aws_bindings_file() {
        // Saving k8s bindings must never touch fleets.toml — the whole reason
        // this is a separate file (see module docs on K8sFleetBinding).
        let dir = std::env::temp_dir().join(format!("oab-separate-fleets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let aws_path = dir.join("fleets.toml");
        let k8s_path = dir.join("fleets-k8s.toml");

        let aws_text = "[fleet.prod]\ncluster = \"oab\"\nprofile = \"oab-fleet\"\n";
        save_bindings_text(&aws_path, aws_text).expect("save aws");
        let k8s_text = "[fleet.dev]\ncontext = \"orbstack\"\nnamespace = \"dev\"\n";
        save_k8s_bindings_text(&k8s_path, k8s_text).expect("save k8s");

        assert_eq!(read_bindings_text(&aws_path).unwrap(), aws_text);
        assert_eq!(read_bindings_text(&k8s_path).unwrap(), k8s_text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_bindings_round_trips_and_preserves_text_verbatim() {
        let dir = std::env::temp_dir().join(format!("oab-fleets-save-{}", std::process::id()));
        let path = dir.join("fleets.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let text = "# my fleets\n\n[[fleet]]\nname = \"prod\"\ncluster = \"oab\"\nprofile = \"orca-prod\"\n";
        let parsed = save_bindings_text(&path, text).expect("save");
        assert_eq!(parsed.fleets.len(), 1);
        assert_eq!(parsed.for_cluster("oab").unwrap().profile.as_deref(), Some("orca-prod"));
        // written verbatim — comment and layout preserved exactly
        assert_eq!(read_bindings_text(&path).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_bindings_rejects_invalid_toml_without_writing() {
        let dir = std::env::temp_dir().join(format!("oab-fleets-bad-{}", std::process::id()));
        let path = dir.join("fleets.toml");
        let _ = std::fs::remove_dir_all(&dir);
        // not-a-table for `fleet` — must fail to parse as FleetBindings
        let bad = "fleet = \"nope\"\n";
        assert!(save_bindings_text(&path, bad).is_err());
        // nothing was written
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_bindings_text_missing_file_is_empty() {
        let path = std::env::temp_dir().join("oab-fleets-does-not-exist-xyz.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_bindings_text(&path).unwrap(), "");
    }

    #[test]
    fn identity_mismatch_flags_static_user_and_wrong_account() {
        let expected_role = "arn:aws:iam::504190915686:role/openab-orca-task-role";
        let actual_assumed =
            "arn:aws:sts::504190915686:assumed-role/openab-orca-task-role/sess-abc";
        // role ARN expected, assumed-role ARN actual, same account+role → match
        assert!(principal_matches(expected_role, actual_assumed));
        // trailing-wildcard expectation
        let expected_wild = "arn:aws:sts::504190915686:assumed-role/openab-orca-task-role/*";
        assert!(principal_matches(expected_wild, actual_assumed));
        // the incident: expected a role, got a static IAM user → mismatch
        let brett = "arn:aws:iam::916371022086:user/brett.chien";
        assert!(!principal_matches(expected_role, brett));
        // right role name, wrong account → mismatch
        let other_acct = "arn:aws:sts::916371022086:assumed-role/openab-orca-task-role/x";
        assert!(!principal_matches(expected_role, other_acct));
        // exact match
        assert!(principal_matches(brett, brett));
    }

    #[test]
    fn bundle_zip_filename_constants_stay_in_sync() {
        // oabctl::studio_api::BUNDLE_ZIP_FILENAME and
        // studio_compose::Bundle::ZIP_FILENAME can't share a dependency edge
        // to enforce this with one constant (see provision_from_library) —
        // this is the cross-crate seam that catches drift instead.
        assert_eq!(oabctl::studio_api::BUNDLE_ZIP_FILENAME, studio_compose::Bundle::ZIP_FILENAME);
    }

    #[test]
    fn aws_config_parses_default_and_named_profiles_with_region() {
        let text = "\
[default]
region = us-east-1
output = json

[profile oab-fleet]
region = ap-east-2

[profile no-region]
";
        let profiles = parse_aws_config(text);
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0].name, "default");
        assert_eq!(profiles[0].region.as_deref(), Some("us-east-1"));
        assert_eq!(profiles[1].name, "oab-fleet");
        assert_eq!(profiles[1].region.as_deref(), Some("ap-east-2"));
        assert_eq!(profiles[2].name, "no-region");
        assert_eq!(profiles[2].region, None);
    }

    #[test]
    fn aws_config_ignores_comments_and_blank_lines() {
        let text = "\
# a comment
; also a comment

[default]
; region is commented out
# region = eu-west-1
region = us-west-2
";
        let profiles = parse_aws_config(text);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn aws_config_empty_text_yields_no_profiles() {
        assert!(parse_aws_config("").is_empty());
    }

    #[test]
    fn aws_credentials_names_are_bare_no_profile_prefix() {
        let text = "\
[default]
aws_access_key_id = AKIA...

[oab-fleet]
aws_access_key_id = AKIA...
";
        let names = parse_aws_credentials_names(text);
        assert_eq!(names, vec!["default".to_string(), "oab-fleet".to_string()]);
    }
}
