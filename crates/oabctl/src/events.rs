//! ECS control-plane **event history** — the observable timeline that
//! `DescribeTasks` (a point-in-time snapshot) cannot give.
//!
//! ECS emits lifecycle events (Task State Change, Service Action, Deployment
//! State Change) to the default EventBridge bus. An EventBridge rule archives
//! them into a CloudWatch Logs group; this module reads that group back and
//! normalizes each EventBridge envelope into an [`EcsEvent`]. We **read the
//! store** rather than subscribe, so this stays a pull API — matching the MCP
//! surface, which is a short-lived stdio server with nowhere to receive a push.
//!
//! NOTE: a container `healthStatus` transition while its task stays `RUNNING`
//! (e.g. an agent going `Unhealthy` but not restarting) is **not** emitted by
//! ECS as an event — the field is absent from Task State Change events. Those
//! remain poll-only via `DescribeTasks`; only lifecycle/stop/service/deployment
//! transitions land here.

use anyhow::{Context, Result};

/// Default CloudWatch Logs group the EventBridge rule archives ECS events into.
/// Overridable (arg / `$OAB_EVENTS_LOG_GROUP`) so the name is not hard-wired to
/// one deployment.
pub const DEFAULT_EVENTS_LOG_GROUP: &str = "/oab/ecs-events";

/// One normalized ECS control-plane event: an EventBridge envelope distilled to
/// the fields an operator actually reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcsEvent {
    /// EventBridge `time` (RFC3339) — when the event fired.
    pub time: String,
    /// EventBridge `detail-type`, e.g. `"ECS Task State Change"`.
    pub detail_type: String,
    /// The OAB service (`oab-{ns}-{name}`) the event concerns, if resolvable.
    pub service: Option<String>,
    /// ECS `clusterArn` the event concerns, if present.
    pub cluster_arn: Option<String>,
    /// ECS `lastStatus` (task events).
    pub last_status: Option<String>,
    /// ECS `desiredStatus` (task events).
    pub desired_status: Option<String>,
    /// ECS `stopCode` when a task stopped.
    pub stop_code: Option<String>,
    /// Human-facing reason: `stoppedReason` (task) or `reason` (service action).
    pub reason: Option<String>,
}

/// Reduce a service reference to its bare agent name for comparison:
/// `oab-{ns}-{name}` → `{name}` (name may itself contain dashes); a bare name
/// passes through unchanged.
fn normalize_service(s: &str) -> String {
    match s.strip_prefix("oab-") {
        // "prod-mira" → "mira"; "prod-foo-bar" → "foo-bar" (name keeps dashes)
        Some(rest) => rest.split_once('-').map_or(rest, |(_, name)| name).to_string(),
        None => s.to_string(),
    }
}

/// Parse one CloudWatch Logs message (an EventBridge event envelope, JSON) into
/// a normalized [`EcsEvent`]. Returns `None` for non-ECS / unparseable lines so
/// a single bad record never sinks a batch.
pub fn parse_event(message: &str) -> Option<EcsEvent> {
    let v: serde_json::Value = serde_json::from_str(message).ok()?;
    if v.get("source").and_then(|s| s.as_str()) != Some("aws.ecs") {
        return None;
    }
    let detail_type = v.get("detail-type").and_then(|s| s.as_str())?.to_string();
    let time = v
        .get("time")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let detail = v.get("detail");

    // Which service: Task State Change carries detail.group =
    // "service:oab-{ns}-{name}"; service/deployment events carry the service
    // ARN in resources[] (…:service/{cluster}/oab-{ns}-{name}).
    let service = detail
        .and_then(|d| d.get("group"))
        .and_then(|g| g.as_str())
        .and_then(|g| g.strip_prefix("service:"))
        .map(str::to_string)
        .or_else(|| {
            v.get("resources")
                .and_then(|r| r.as_array())
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
                .filter_map(|arn| arn.rsplit('/').next())
                .find(|name| name.starts_with("oab-"))
                .map(str::to_string)
        });

    let dstr = |k: &str| {
        detail
            .and_then(|d| d.get(k))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };

    Some(EcsEvent {
        time,
        detail_type,
        service,
        cluster_arn: dstr("clusterArn"),
        last_status: dstr("lastStatus"),
        desired_status: dstr("desiredStatus"),
        stop_code: dstr("stopCode"),
        reason: dstr("stoppedReason").or_else(|| dstr("reason")),
    })
}

/// Fetch recent ECS events from the archive log group, **newest first**.
///
/// - `cluster`: when `Some`, keep only events whose `clusterArn` ends in
///   `/{cluster}` (defensive — the archiving rule is expected to be
///   cluster-scoped already).
/// - `service`: when `Some`, keep only events for that OAB service (accepts
///   `oab-{ns}-{name}` or the bare agent name).
/// - `since_ms`: CloudWatch time-window start (epoch millis).
/// - `limit`: max events returned (clamped to `1..=1000`).
pub async fn fetch_ecs_events(
    aws_config: &aws_config::SdkConfig,
    log_group: &str,
    cluster: Option<&str>,
    service: Option<&str>,
    since_ms: i64,
    limit: i32,
) -> Result<Vec<EcsEvent>> {
    let logs = aws_sdk_cloudwatchlogs::Client::new(aws_config);
    let limit = limit.clamp(1, 1000);

    // Collect raw (timestamp, message) pairs. Events are low-volume; page a
    // bounded number of times so a misconfigured window can't run away.
    let mut raw: Vec<(i64, String)> = Vec::new();
    let mut next_token = None;
    for _ in 0..20 {
        let mut req = logs
            .filter_log_events()
            .log_group_name(log_group)
            .start_time(since_ms.max(0))
            .limit(limit);
        if let Some(t) = &next_token {
            req = req.next_token(t);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("failed to read events log group {log_group}"))?;
        for e in resp.events() {
            if let Some(msg) = e.message() {
                raw.push((e.timestamp().unwrap_or(0), msg.to_string()));
            }
        }
        next_token = resp.next_token().map(str::to_string);
        if next_token.is_none() || raw.len() as i32 >= limit {
            break;
        }
    }

    // Newest first.
    raw.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));

    let want_service = service.map(normalize_service);
    let mut out = Vec::new();
    for (_, msg) in raw {
        let Some(ev) = parse_event(&msg) else { continue };
        if let Some(c) = cluster {
            match &ev.cluster_arn {
                Some(arn) if arn.ends_with(&format!("/{c}")) => {}
                // No clusterArn to check against → don't drop it.
                None => {}
                _ => continue,
            }
        }
        if let Some(ws) = &want_service {
            match &ev.service {
                Some(s) if &normalize_service(s) == ws => {}
                _ => continue,
            }
        }
        out.push(ev);
        if out.len() as i32 >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_oab_namespace_prefix() {
        assert_eq!(normalize_service("oab-prod-mira"), "mira");
        assert_eq!(normalize_service("oab-prod-foo-bar"), "foo-bar");
        assert_eq!(normalize_service("mira"), "mira");
    }

    #[test]
    fn parses_task_state_change_stop() {
        let msg = r#"{
            "source": "aws.ecs",
            "detail-type": "ECS Task State Change",
            "time": "2026-08-11T04:42:00Z",
            "resources": ["arn:aws:ecs:ap-east-2:504190915686:task/oab/abc"],
            "detail": {
                "clusterArn": "arn:aws:ecs:ap-east-2:504190915686:cluster/oab",
                "group": "service:oab-prod-mira",
                "lastStatus": "STOPPED",
                "desiredStatus": "STOPPED",
                "stopCode": "EssentialContainerExited",
                "stoppedReason": "Essential container in task exited"
            }
        }"#;
        let ev = parse_event(msg).expect("parses");
        assert_eq!(ev.detail_type, "ECS Task State Change");
        assert_eq!(ev.time, "2026-08-11T04:42:00Z");
        assert_eq!(ev.service.as_deref(), Some("oab-prod-mira"));
        assert_eq!(ev.last_status.as_deref(), Some("STOPPED"));
        assert_eq!(ev.stop_code.as_deref(), Some("EssentialContainerExited"));
        assert_eq!(
            ev.reason.as_deref(),
            Some("Essential container in task exited")
        );
    }

    #[test]
    fn parses_service_action_reason_and_resource_service() {
        let msg = r#"{
            "source": "aws.ecs",
            "detail-type": "ECS Service Action",
            "time": "2026-08-11T04:43:00Z",
            "resources": ["arn:aws:ecs:ap-east-2:504190915686:service/oab/oab-prod-mira"],
            "detail": {
                "clusterArn": "arn:aws:ecs:ap-east-2:504190915686:cluster/oab",
                "eventName": "SERVICE_TASK_START_IMPAIRED",
                "reason": "tasks are unable to consistently start and stay running"
            }
        }"#;
        let ev = parse_event(msg).expect("parses");
        assert_eq!(ev.detail_type, "ECS Service Action");
        // No detail.group → resolved from the resources[] service ARN.
        assert_eq!(ev.service.as_deref(), Some("oab-prod-mira"));
        assert_eq!(
            ev.reason.as_deref(),
            Some("tasks are unable to consistently start and stay running")
        );
    }

    #[test]
    fn non_ecs_and_garbage_lines_are_skipped() {
        assert!(parse_event(r#"{"source":"aws.ec2","detail-type":"x"}"#).is_none());
        assert!(parse_event("not json at all").is_none());
    }
}
