//! studio#29 — the CloudWatch Logs event boundary must key on the FULL ECS
//! service name (`oab-{ns}-{name}`), the same discipline `instance_status`
//! already enforces for `ListTasks`. A short/display name is refused loudly at
//! the boundary rather than silently normalized.

#[tokio::test]
async fn fetch_ecs_events_rejects_short_service_names() {
    let aws = aws_config::SdkConfig::builder().build();
    let err = oabctl::fetch_ecs_events(&aws, "/oab/ecs-events", Some("oab"), Some("mira"), 0, 50)
        .await
        .expect_err("a short/display name must be refused at the boundary");
    assert!(
        err.to_string().contains("oab-<ns>-<name>"),
        "unexpected error: {err}"
    );
}
