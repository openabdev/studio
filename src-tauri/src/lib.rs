mod mcp;

use mcp::McpClient;
use serde_json::{json, Value};

/// Default cluster for the desktop core, mirroring `oab-mcp`'s own default.
fn default_cluster() -> String {
    std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string())
}

/// Bridge command: the deployment roster in the console's read-model shape
/// (`Deployment[]` — name/namespace/desired/current/ready + per-instance
/// 6-state), sourced **entirely through the bundled `oab-mcp` sidecar** over the
/// MCP contract — no in-process core link.
///
/// `oab-mcp`'s `deploy_list` returns per-service counters only, so we list the
/// services then fetch each one's per-instance phases via `deploy_get` — the
/// same two-step the in-process bridge used, now over MCP.
#[tauri::command]
async fn deploy_list(
    client: tauri::State<'_, McpClient>,
    cluster: Option<String>,
) -> Result<Vec<Value>, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
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
            // Start the control-plane core as a child and stash the MCP client
            // for the bridge command. If it fails, the bridge surfaces a clear
            // "state not managed" error rather than the app silently half-working.
            let handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                match McpClient::spawn(&handle, &default_cluster()).await {
                    Ok(client) => {
                        handle.manage(client);
                    }
                    Err(e) => log::error!("failed to start oab-mcp core: {e}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![deploy_list])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
