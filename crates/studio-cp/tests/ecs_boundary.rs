//! studio#29 — every ECS / CloudWatch Logs boundary must query by the FULL
//! service name (`oab-{ns}-{name}`), never a caller's short/display selector.
//! These tests wire a recording client through the `observe_*_with` seams and
//! assert the exact `service` argument reaching `instance_status` (ListTasks)
//! and `fetch_ecs_events` (logs filter).

use std::sync::Mutex;

use studio_cp::{
    observe_deployment_with, observe_events_with, EcsEvent, EcsObserveReads, InstanceStatus,
    ServiceStatus,
};

fn svc(namespace: &str, name: &str) -> ServiceStatus {
    ServiceStatus {
        name: name.into(),
        namespace: namespace.into(),
        service_name: format!("oab-{namespace}-{name}"),
        cpu: "512".into(),
        memory: "1024".into(),
        capacity: "FARGATE".into(),
        running: 1,
        desired: 1,
        status: "ACTIVE".into(),
    }
}

/// Recording ECS/logs reader: canned `service_status` output plus captured
/// argument lists for the boundary calls under test.
#[derive(Default)]
struct RecordingReads {
    services: Vec<ServiceStatus>,
    instances: Vec<InstanceStatus>,
    events: Vec<EcsEvent>,
    instance_calls: Mutex<Vec<(String, String)>>,
    event_queries: Mutex<Vec<Option<String>>>,
    service_status_calls: Mutex<u32>,
}

impl EcsObserveReads for RecordingReads {
    async fn service_status(&self, _cluster: &str) -> anyhow::Result<Vec<ServiceStatus>> {
        *self.service_status_calls.lock().unwrap() += 1;
        Ok(self.services.clone())
    }

    async fn instance_status(
        &self,
        cluster: &str,
        service: &str,
    ) -> anyhow::Result<Vec<InstanceStatus>> {
        self.instance_calls
            .lock()
            .unwrap()
            .push((cluster.to_string(), service.to_string()));
        Ok(self.instances.clone())
    }

    async fn fetch_ecs_events(
        &self,
        _log_group: &str,
        _cluster: Option<&str>,
        service: Option<&str>,
        _since_ms: i64,
        _limit: i32,
    ) -> anyhow::Result<Vec<EcsEvent>> {
        self.event_queries
            .lock()
            .unwrap()
            .push(service.map(str::to_string));
        Ok(self.events.clone())
    }
}

/// `observe_deployment` must pass the resolved service's `service_name`
/// verbatim to `instance_status` — the pre-#28 bug sent the short selector and
/// ECS answered `ServiceNotFoundException`.
#[tokio::test]
async fn observe_deployment_lists_tasks_by_full_ecs_service_name() {
    let reads = RecordingReads {
        services: vec![svc("prod", "orca"), svc("prod", "mira")],
        ..Default::default()
    };

    let deployment = observe_deployment_with(&reads, "oab", "orca")
        .await
        .expect("observe succeeds")
        .expect("selector resolves");

    assert_eq!(deployment.name, "orca");
    assert_eq!(
        reads.instance_calls.lock().unwrap().as_slice(),
        [("oab".to_string(), "oab-prod-orca".to_string())]
    );
}

/// An unresolvable selector stays a clean miss — no ECS call is issued with a
/// name that was never confirmed to exist.
#[tokio::test]
async fn observe_deployment_unknown_selector_issues_no_ecs_call() {
    let reads = RecordingReads {
        services: vec![svc("prod", "orca")],
        ..Default::default()
    };

    let result = observe_deployment_with(&reads, "oab", "nope")
        .await
        .expect("observe succeeds");

    assert!(result.is_none());
    assert!(reads.instance_calls.lock().unwrap().is_empty());
}

/// `observe_events` accepts the same short-or-full selector as
/// `observe_deployment`, but the logs query must key on the resolved FULL name:
/// `prod/mira` and `dev/mira` share the bare name `mira`, so normalizing to a
/// bare name would leak the other namespace's events into the result.
#[tokio::test]
async fn observe_events_filters_logs_by_full_ecs_service_name() {
    let reads = RecordingReads {
        services: vec![svc("prod", "mira"), svc("dev", "mira")],
        ..Default::default()
    };

    let events = observe_events_with(&reads, "/oab/ecs-events", "oab", Some("mira"), 0, 50)
        .await
        .expect("observe_events succeeds");

    assert!(events.is_empty());
    assert_eq!(
        reads.event_queries.lock().unwrap().as_slice(),
        [Some("oab-prod-mira".to_string())]
    );
}

/// A selector already in `oab-{ns}-{name}` form queries verbatim even when the
/// service no longer appears in DescribeServices (e.g. deleted but still in the
/// events archive).
#[tokio::test]
async fn observe_events_full_name_selector_queries_verbatim() {
    let reads = RecordingReads {
        services: vec![svc("prod", "mira")],
        ..Default::default()
    };

    observe_events_with(
        &reads,
        "/oab/ecs-events",
        "oab",
        Some("oab-prod-gone"),
        0,
        50,
    )
    .await
    .expect("observe_events succeeds");

    assert_eq!(
        reads.event_queries.lock().unwrap().as_slice(),
        [Some("oab-prod-gone".to_string())]
    );
}

/// A bare selector matching no live service returns empty without issuing a
/// logs query — a short name must never reach the CloudWatch boundary.
#[tokio::test]
async fn observe_events_unknown_short_selector_issues_no_logs_query() {
    let reads = RecordingReads {
        services: vec![svc("prod", "mira")],
        ..Default::default()
    };

    let events = observe_events_with(&reads, "/oab/ecs-events", "oab", Some("nope"), 0, 50)
        .await
        .expect("observe_events succeeds");

    assert!(events.is_empty());
    assert!(reads.event_queries.lock().unwrap().is_empty());
}

/// Cluster-wide reads skip resolution entirely — no `service_status` call, the
/// logs query carries no service filter.
#[tokio::test]
async fn observe_events_without_service_skips_resolution() {
    let reads = RecordingReads::default();

    observe_events_with(&reads, "/oab/ecs-events", "oab", None, 0, 50)
        .await
        .expect("observe_events succeeds");

    assert_eq!(*reads.service_status_calls.lock().unwrap(), 0);
    assert_eq!(reads.event_queries.lock().unwrap().as_slice(), [None]);
}
