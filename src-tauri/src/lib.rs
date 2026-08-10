mod mcp;

use mcp::McpClient;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tokio::sync::Mutex as AsyncMutex;

/// Holds the core client once the frontend has asked us to start it. Kept behind
/// an async mutex so `deploy_list` can wait for (and share) a single core.
#[derive(Default)]
struct Core(AsyncMutex<Option<McpClient>>);

/// Default cluster for the desktop core, mirroring `oab-mcp`'s own default.
fn default_cluster() -> String {
    std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string())
}

/// Start the core sidecar. Called by the frontend *after* it has subscribed to
/// the log streams, so no lifecycle line is emitted before anyone is listening.
/// Idempotent.
#[tauri::command]
async fn start_core(app: tauri::AppHandle, core: tauri::State<'_, Core>) -> Result<(), String> {
    let mut guard = core.0.lock().await;
    if guard.is_some() {
        return Ok(());
    }
    let _ = app.emit("app-log", json!({ "level": "info", "msg": "OAB Studio starting…" }));
    match McpClient::spawn(&app, &default_cluster()).await {
        Ok(client) => {
            *guard = Some(client);
            Ok(())
        }
        Err(e) => {
            let _ = app.emit(
                "app-log",
                json!({ "level": "error", "msg": format!("failed to start core: {e}") }),
            );
            Err(e)
        }
    }
}

/// List services (`deploy_list`) then fetch each one's per-instance 6-state
/// (`deploy_get`), all over MCP — the two-step the in-process bridge used,
/// now over the wire. Console view-model shape is unchanged.
async fn roster_over_mcp(client: &McpClient, cluster: &str) -> Result<Vec<Value>, String> {
    let listed = client
        .call_tool("deploy_list", json!({ "cluster": cluster }))
        .await?;
    let services = listed
        .get("deployments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut deployments = Vec::with_capacity(services.len());
    for svc in &services {
        let Some(name) = svc.get("name").and_then(Value::as_str) else {
            continue;
        };
        let got = client
            .call_tool("deploy_get", json!({ "service": name, "cluster": cluster }))
            .await?;
        // `deploy_get` returns `{ "found": false, .. }` for a vanished service.
        if got.get("found") == Some(&Value::Bool(false)) {
            continue;
        }
        deployments.push(got);
    }
    Ok(deployments)
}

/// Bridge command: the deployment roster in the console's read-model shape,
/// sourced through the bundled `oab-mcp` sidecar. Errors go to the caller and
/// the log pane.
#[tauri::command]
async fn deploy_list(
    core: tauri::State<'_, Core>,
    cluster: Option<String>,
) -> Result<Vec<Value>, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match roster_over_mcp(&client, &cluster).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("deploy_list: {e}"));
            Err(e)
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            app.manage(Core::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![start_core, deploy_list])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
