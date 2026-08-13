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

// ---- Fleet → managing-credential binding (ADR: Per-Fleet managing identity) --
//
// The *declarative* side of the loop: which credential should manage which
// fleet/cluster. Operator config, deliberately separate from the Fleet Store
// (observed membership/lease state); the two may fold together later. Selecting
// a binding is credential *selection*, not per-caller authz.

/// A declarative binding of a managed fleet/cluster to the credential that
/// should manage it. Profile-first (assume-role is later work).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FleetBinding {
    /// Fleet label (display / selection).
    #[serde(default)]
    pub name: String,
    /// ECS cluster this binding governs — the match key against a call's target.
    pub cluster: String,
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

/// Parsed fleet-binding file: a list of `[[fleet]]` tables.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct FleetBindings {
    #[serde(default, rename = "fleet")]
    pub fleets: Vec<FleetBinding>,
}

impl FleetBindings {
    /// The binding governing `cluster`, if any (first match wins).
    pub fn for_cluster(&self, cluster: &str) -> Option<&FleetBinding> {
        self.fleets.iter().find(|b| b.cluster == cluster)
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
}
