mod mcp;

use mcp::McpClient;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

/// Default cluster for the desktop core, mirroring `oab-mcp`'s own default.
fn default_cluster() -> String {
    std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string())
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
/// sourced entirely through the bundled `oab-mcp` sidecar. Errors are surfaced
/// both to the caller and to the log pane.
#[tauri::command]
async fn deploy_list(
    client: tauri::State<'_, McpClient>,
    cluster: Option<String>,
) -> Result<Vec<Value>, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    match roster_over_mcp(client.inner(), &cluster).await {
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
            let handle = app.handle().clone();
            // First line the log pane shows — proves the window is alive even
            // before the core comes up.
            let _ = handle.emit(
                "app-log",
                json!({ "level": "info", "msg": "OAB Studio starting…" }),
            );
            // Start the control-plane core as a child and stash the MCP client
            // for the bridge command. Failure is logged (and shown in the pane)
            // rather than leaving a silently half-wired app.
            tauri::async_runtime::block_on(async move {
                match McpClient::spawn(&handle, &default_cluster()).await {
                    Ok(client) => {
                        handle.manage(client);
                    }
                    Err(e) => {
                        log::error!("failed to start oab-mcp core: {e}");
                        let _ = handle.emit(
                            "app-log",
                            json!({ "level": "error", "msg": format!("failed to start core: {e}") }),
                        );
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![deploy_list])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
