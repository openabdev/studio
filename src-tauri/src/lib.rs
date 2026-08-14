mod mcp;
mod remote;

use mcp::McpClient;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
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

/// Bridge command: the effective runtime identity/context for a cluster (ADR
/// #19), sourced through the bundled `oab-mcp` sidecar's `runtime_context` tool.
/// The console renders "who am I managing this cluster as, against what account".
#[tauri::command]
async fn runtime_context(
    core: tauri::State<'_, Core>,
    cluster: Option<String>,
) -> Result<Value, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client
        .call_tool("runtime_context", json!({ "cluster": cluster }))
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("runtime_context: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: the declarative fleet-binding config (ADR #19), sourced
/// through the bundled `oab-mcp` sidecar's `fleet_config` tool. The console
/// renders the configured fleets and lets the operator switch the active one.
#[tauri::command]
async fn fleet_config(core: tauri::State<'_, Core>) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client.call_tool("fleet_config", json!({})).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("fleet_config: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: persist the edited `fleets.toml` text (ADR #19 slice C) via
/// the sidecar's `fleet_config_write` tool, which validates + writes + hot-reloads
/// and returns the reloaded config. A parse error surfaces to the editor.
#[tauri::command]
async fn fleet_config_write(core: tauri::State<'_, Core>, text: String) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client
        .call_tool("fleet_config_write", json!({ "text": text }))
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("fleet_config_write: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: start (size 1) / stop (size 0) a deployment via the sidecar's
/// `deploy_scale` tool (ADR-2 write model — stop = scale→0, start = scale→1; the
/// Spec is kept by ECS, so it's reversible). An OAB service runs a single bot
/// token, so size is 0/1 only; `namespace` is required upstream to resolve the
/// service (`oab-{namespace}-{name}`) and the managing credential is per-cluster.
#[tauri::command]
async fn deploy_scale(
    core: tauri::State<'_, Core>,
    name: String,
    size: i64,
    namespace: Option<String>,
    cluster: Option<String>,
) -> Result<Value, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    let mut params = json!({ "name": name, "size": size, "cluster": cluster });
    if let Some(ns) = namespace {
        params["namespace"] = json!(ns);
    }
    match client.call_tool("deploy_scale", params).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("deploy_scale: {e}"));
            Err(e)
        }
    }
}

// ---- Remote reverse-MCP connection (reverse-MCP client ADR, Part B) ---------

/// The remote-connection view the panel renders: the config file path + raw text
/// (for the editor), the parsed URL, whether it is configured, and the live
/// connection status. Parse is best-effort so a malformed file still opens in the
/// editor (shown as not-configured) rather than blanking the panel.
async fn remote_view(remote: &remote::Remote) -> Result<Value, String> {
    let text = remote::read_config_text()?;
    let cfg = acp_tunnel::config::RemoteConfig::parse(&text).unwrap_or_default();
    let path = remote::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let status = remote.0.lock().await.status.clone();
    Ok(json!({
        "path": path,
        "text": text,
        "url": cfg.url,
        "configured": cfg.is_configured(),
        "status": if status.is_empty() { "disconnected".to_string() } else { status },
    }))
}

/// The remote-connection config + status (the "declare" side of the remote panel).
#[tauri::command]
async fn remote_config(remote: tauri::State<'_, remote::Remote>) -> Result<Value, String> {
    remote_view(&remote).await
}

/// Persist the edited `remote.toml` (validates it parses before writing) and
/// return the refreshed view.
#[tauri::command]
async fn remote_config_write(
    remote: tauri::State<'_, remote::Remote>,
    text: String,
) -> Result<Value, String> {
    remote::write_config_text(&text)?;
    remote_view(&remote).await
}

/// Activate the remote connection (the explicit "Activate" button): dial `/acp`
/// using the saved config and publish the `oab` tools to the attached agent. The
/// core sidecar must be started first (it is what the tunnel relays to).
#[tauri::command]
async fn remote_connect(
    app: tauri::AppHandle,
    core: tauri::State<'_, Core>,
    remote: tauri::State<'_, remote::Remote>,
) -> Result<(), String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet — start the core before connecting".to_string())?
    };
    remote::connect(app, &remote, client).await
}

/// Deactivate the remote connection.
#[tauri::command]
async fn remote_disconnect(
    app: tauri::AppHandle,
    remote: tauri::State<'_, remote::Remote>,
) -> Result<(), String> {
    remote::disconnect(&app, &remote).await;
    Ok(())
}

/// What the frontend needs to render the "update available" state: the version
/// on the release vs. what's running, plus the release notes.
#[derive(serde::Serialize)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: Option<String>,
}

/// Ask the release endpoint whether a newer signed build exists. Returns `None`
/// when we're already current. Drives the topbar "Check for updates" button.
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(Some(UpdateInfo {
            version: update.version.clone(),
            current: update.current_version.clone(),
            notes: update.body.clone(),
        })),
        None => Ok(None),
    }
}

/// Download + verify + install the pending update, then restart into it. The
/// bundle's minisign signature is checked against the embedded pubkey before it
/// is applied, so a tampered release can't be installed.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Err("no update available".to_string());
    };
    let _ = app.emit(
        "app-log",
        json!({ "level": "info", "msg": format!("downloading update v{}…", update.version) }),
    );
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "app-log",
        json!({ "level": "info", "msg": "update installed — restarting…" }),
    );
    app.restart();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            app.manage(Core::default());
            app.manage(remote::Remote::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_core,
            deploy_list,
            runtime_context,
            fleet_config,
            fleet_config_write,
            deploy_scale,
            remote_config,
            remote_config_write,
            remote_connect,
            remote_disconnect,
            check_update,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
