use serde_json::{json, Value};

/// Bridge command: return the deployment roster as the console's read-model
/// (`Deployment[]` — name/namespace/desired/current/ready + per-instance
/// 6-state). Thin passthrough to `studio-cp`; the desktop core resolves AWS
/// credentials from the standard chain, like the CLI. Mirrors `oab-mcp`'s wire
/// shape so the web skin's `TauriSource` and `MockSource` are interchangeable.
#[tauri::command]
async fn deploy_list(cluster: Option<String>) -> Result<Vec<Value>, String> {
  let cluster = cluster
    .or_else(|| std::env::var("OAB_CLUSTER").ok())
    .unwrap_or_else(|| "oab".to_string());
  let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

  let services = studio_cp::observe_services(&aws, &cluster)
    .await
    .map_err(|e| e.to_string())?;

  let mut deployments = Vec::with_capacity(services.len());
  for svc in &services {
    if let Some(d) = studio_cp::observe_deployment(&aws, &cluster, &svc.name)
      .await
      .map_err(|e| e.to_string())?
    {
      deployments.push(json!({
        "name": d.name,
        "namespace": d.namespace,
        "desired": d.desired,
        "current": d.current,
        "ready": d.ready,
        "instances": d
          .instances
          .iter()
          .map(|i| json!({ "id": i.id, "state": format!("{:?}", i.phase) }))
          .collect::<Vec<_>>(),
      }));
    }
  }
  Ok(deployments)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![deploy_list])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
