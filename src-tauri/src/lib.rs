mod compose;
mod config;
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
    // (the `core: spawning…` line from McpClient::spawn covers the "starting" beat)
    match McpClient::spawn(&app, &config::load(&app)).await {
        Ok(client) => {
            *guard = Some(client);
            Ok(())
        }
        Err(e) => {
            let _ = app.emit(
                "app-log",
                json!({ "level": "error", "msg": format!("core: failed to start — {e}") }),
            );
            Err(e)
        }
    }
}

/// The persisted (or env-seeded default) sidecar connection target, for the
/// Config tab to render/edit.
#[tauri::command]
async fn mcp_target_get(app: tauri::AppHandle) -> Result<config::McpTarget, String> {
    Ok(config::load(&app))
}

/// Persist a new sidecar target and **reload the core onto it** without an app
/// restart: kill the running sidecar, spawn a fresh one with the new hermetic env.
#[tauri::command]
async fn mcp_target_set(
    app: tauri::AppHandle,
    core: tauri::State<'_, Core>,
    target: config::McpTarget,
) -> Result<(), String> {
    config::save(&app, &target)?;
    let mut guard = core.0.lock().await;
    if let Some(old) = guard.take() {
        old.shutdown().await;
        let _ = app.emit(
            "app-log",
            json!({ "level": "info", "msg": "core: reloading onto new target…" }),
        );
    }
    match McpClient::spawn(&app, &target).await {
        Ok(client) => {
            *guard = Some(client);
            Ok(())
        }
        Err(e) => {
            let _ = app.emit(
                "app-log",
                json!({ "level": "error", "msg": format!("core: reload failed — {e}") }),
            );
            Err(e)
        }
    }
}

// ---- Bundle composition (ADR: agent deployment, slice 1) --------------------

/// The persisted template/overlay/skills library for the Compose panel to
/// render/edit. Empty on first run (or if the file is missing/corrupt).
#[tauri::command]
async fn compose_library_get(app: tauri::AppHandle) -> Result<studio_compose::Library, String> {
    Ok(compose::load(&app))
}

/// Persist the edited library and return it back (round-tripped through the same
/// serde the preview uses, so the editor and disk agree).
#[tauri::command]
async fn compose_library_set(
    app: tauri::AppHandle,
    library: studio_compose::Library,
) -> Result<studio_compose::Library, String> {
    compose::save(&app, &library)?;
    Ok(library)
}

/// Compose a preview of `template ⊕ overlay` from the (possibly unsaved) library
/// the editor currently holds — so the operator sees the effect of edits before
/// saving. Compose errors (unknown template/overlay/skill, missing image tag)
/// surface to the panel.
#[tauri::command]
async fn compose_preview(
    library: studio_compose::Library,
    template: String,
    overlay: Option<String>,
) -> Result<studio_compose::BundlePreview, String> {
    compose::preview(&library, &template, overlay.as_deref())
}

/// Provision an agent from the compose library over MCP (`deploy_provision`):
/// compose `template ⊕ overlay`, push the bundle to the agent's S3 artifacts
/// prefix, and either redeploy the ECS service (AWS, default) or apply a k8s
/// Deployment (studio#104: `provider = "k8s"`, applied through the given
/// `context` — see `t_provision`'s dispatch in `oab-mcp`) at the chosen image
/// tag. The heavy lifting (compose + S3 + apply) runs in the sidecar with its
/// hermetic AWS/kube env; this is a thin bridge like `deploy_scale`.
#[tauri::command]
async fn deploy_provision(
    core: tauri::State<'_, Core>,
    library: Value,
    template: String,
    overlay: Option<String>,
    name: String,
    namespace: Option<String>,
    image: Option<String>,
    cluster: Option<String>,
    provider: Option<String>,
    context: Option<String>,
    expected_principal: Option<String>,
) -> Result<Value, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    let mut params = json!({
        "library": library,
        "template": template,
        "name": name,
        "cluster": cluster,
    });
    if let Some(o) = overlay {
        params["overlay"] = json!(o);
    }
    if let Some(ns) = namespace {
        params["namespace"] = json!(ns);
    }
    if let Some(img) = image.filter(|s| !s.is_empty()) {
        params["image_tag"] = json!(img);
    }
    if let Some(p) = provider.filter(|s| !s.is_empty()) {
        params["provider"] = json!(p);
    }
    if let Some(c) = context.filter(|s| !s.is_empty()) {
        params["context"] = json!(c);
    }
    if let Some(ep) = expected_principal.filter(|s| !s.is_empty()) {
        params["expected_principal"] = json!(ep);
    }
    match client.call_tool("deploy_provision", params).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("deploy_provision: {e}"));
            Err(e)
        }
    }
}

/// [`deploy_provision`], but for `deploy_provision_agent` (studio#128) — the
/// New Fleet wizard's direct path. Structured fields, not a pre-rendered
/// `config_toml` — the sidecar renders it server-side (see the MCP tool's
/// own doc comment for why: single source of truth regardless of caller).
#[tauri::command]
async fn deploy_provision_agent(
    core: tauri::State<'_, Core>,
    image: String,
    name: String,
    namespace: Option<String>,
    api_key: Option<String>,
    chat_platform: Option<String>,
    chat_bot_token: Option<String>,
    chat_channel_secret: Option<String>,
    acp_enabled: Option<bool>,
    acp_token: Option<String>,
    local_config_folder: Option<String>,
    cluster: Option<String>,
    provider: Option<String>,
    context: Option<String>,
    expected_principal: Option<String>,
) -> Result<Value, String> {
    let cluster = cluster.unwrap_or_else(default_cluster);
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    let mut params = json!({
        "image": image,
        "name": name,
        "cluster": cluster,
    });
    if let Some(ns) = namespace {
        params["namespace"] = json!(ns);
    }
    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
        params["api_key"] = json!(k);
    }
    if let Some(p) = chat_platform.filter(|s| !s.is_empty()) {
        params["chat_platform"] = json!(p);
    }
    if let Some(t) = chat_bot_token.filter(|s| !s.is_empty()) {
        params["chat_bot_token"] = json!(t);
    }
    if let Some(s) = chat_channel_secret.filter(|s| !s.is_empty()) {
        params["chat_channel_secret"] = json!(s);
    }
    if let Some(a) = acp_enabled {
        params["acp_enabled"] = json!(a);
    }
    if let Some(t) = acp_token.filter(|s| !s.is_empty()) {
        params["acp_token"] = json!(t);
    }
    if let Some(f) = local_config_folder.filter(|s| !s.is_empty()) {
        params["local_config_folder"] = json!(f);
    }
    if let Some(p) = provider.filter(|s| !s.is_empty()) {
        params["provider"] = json!(p);
    }
    if let Some(c) = context.filter(|s| !s.is_empty()) {
        params["context"] = json!(c);
    }
    if let Some(ep) = expected_principal.filter(|s| !s.is_empty()) {
        params["expected_principal"] = json!(ep);
    }
    match client.call_tool("deploy_provision_agent", params).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("deploy_provision_agent: {e}"));
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

/// Bridge command: AWS profiles discovered on this machine (studio#104), via
/// the sidecar's `list_aws_profiles` tool — backs the New Fleet wizard's AWS
/// profile picker.
#[tauri::command]
async fn list_aws_profiles(core: tauri::State<'_, Core>) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client.call_tool("list_aws_profiles", json!({})).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("list_aws_profiles: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: kubeconfig contexts discovered on this machine (studio#104),
/// via the sidecar's `list_k8s_contexts` tool — backs the New Fleet wizard's
/// k8s context picker.
#[tauri::command]
async fn list_k8s_contexts(core: tauri::State<'_, Core>) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client.call_tool("list_k8s_contexts", json!({})).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("list_k8s_contexts: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: a vendor's real, currently-published Stable/Beta image
/// tags on GHCR (studio#128), via the sidecar's `resolve_vendor_image_tags`
/// tool — backs the New Fleet wizard's Vendor + Image tag fields.
#[tauri::command]
async fn resolve_vendor_image_tags(core: tauri::State<'_, Core>, vendor: String) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client
        .call_tool("resolve_vendor_image_tags", json!({ "vendor": vendor }))
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("resolve_vendor_image_tags: {e}"));
            Err(e)
        }
    }
}

/// Lists agent names under the local "Config folder" (studio#128) that
/// have a `config.toml` — backs the Debug drawer's "Agent configs" tab.
/// Pure local filesystem, no sidecar/AWS/k8s involved at all: the local
/// folder is Studio's own mirror the New Fleet wizard writes alongside its
/// S3 upload, not a read-through to the S3 source of truth (Brett:
/// "forget about s3 now" for this feature — see the Config folder
/// setting's own doc comment for the full reasoning).
#[tauri::command]
fn list_local_agent_configs(folder: String) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(&folder).map_err(|e| format!("read {folder}: {e}"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() && path.join("config.toml").is_file() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Reads one agent's `config.toml` from the local "Config folder" —
/// `<folder>/<agent>/config.toml`, the same layout
/// `list_local_agent_configs` scans.
#[tauri::command]
fn read_local_agent_config(folder: String, agent: String) -> Result<String, String> {
    let path = std::path::Path::new(&folder).join(&agent).join("config.toml");
    std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// Bridge command: namespaces in a kubeconfig context (studio#104), via the
/// sidecar's `list_namespaces` tool — backs the New Fleet wizard's namespace
/// field's autocomplete.
#[tauri::command]
async fn list_namespaces(core: tauri::State<'_, Core>, context: Option<String>) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    let mut params = json!({});
    if let Some(c) = context.filter(|s| !s.is_empty()) {
        params["context"] = json!(c);
    }
    match client.call_tool("list_namespaces", params).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("list_namespaces: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: service accounts in one namespace of a kubeconfig context
/// (studio#104), via the sidecar's `list_service_accounts` tool — backs the
/// New Fleet wizard's optional service-account picker. Per the tool's own
/// contract, a failure here should read to the caller as "leave it unset,"
/// not an error — this bridge doesn't editorialize that, it just forwards
/// whatever the sidecar returns (including an `Err`) and the console decides
/// how to treat it (deploy.ts's `loadK8sServiceAccounts` swallows failures).
#[tauri::command]
async fn list_service_accounts(
    core: tauri::State<'_, Core>,
    context: Option<String>,
    namespace: String,
) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    let mut params = json!({ "namespace": namespace });
    if let Some(c) = context.filter(|s| !s.is_empty()) {
        params["context"] = json!(c);
    }
    match client.call_tool("list_service_accounts", params).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("list_service_accounts: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: the declarative k8s fleet-binding config (studio#104, k8s
/// counterpart to `fleet_config`), sourced through the sidecar's
/// `k8s_fleet_config` tool. The New Fleet wizard's k8s submit path reads this
/// first to compute an appended `fleets-k8s.toml` block, since the write tool
/// takes the whole file's text.
#[tauri::command]
async fn k8s_fleet_config(core: tauri::State<'_, Core>) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client.call_tool("k8s_fleet_config", json!({})).await {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("k8s_fleet_config: {e}"));
            Err(e)
        }
    }
}

/// Bridge command: persist the edited `fleets-k8s.toml` text (studio#104,
/// k8s counterpart to `fleet_config_write`) via the sidecar's
/// `k8s_fleet_config_write` tool.
#[tauri::command]
async fn k8s_fleet_config_write(core: tauri::State<'_, Core>, text: String) -> Result<Value, String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet".to_string())?
    };
    match client
        .call_tool("k8s_fleet_config_write", json!({ "text": text }))
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            client.log("error", &format!("k8s_fleet_config_write: {e}"));
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
    // Status of the management connection — the one this legacy panel controls.
    // `RemoteState` is now keyed per agent, so look it up under the management
    // endpoint's name (the legacy `remote.toml` adopts the name "management").
    let mgmt_name = remote::load_registry()
        .ok()
        .and_then(|r| r.management().map(|e| e.name.clone()))
        .unwrap_or_else(|| remote::LEGACY_MANAGEMENT_NAME.to_string());
    let status = remote
        .0
        .lock()
        .await
        .get(&mgmt_name)
        .map(|st| st.status.clone())
        .unwrap_or_default();
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

/// The per-agent endpoint registry with each entry's live status — the data an
/// agent-console selector renders (ADR agent-consoles Parts B/C). Reads
/// `agents.toml` (or the adopted legacy `remote.toml`) and overlays each entry's
/// current connection status. Tokens are **not** included (secrets never cross
/// this bridge); only whether the entry is configured.
#[tauri::command]
async fn remote_agents(remote: tauri::State<'_, remote::Remote>) -> Result<Value, String> {
    let reg = remote::load_registry()?;
    let guard = remote.0.lock().await;
    let agents: Vec<Value> = reg
        .agents
        .iter()
        .map(|a| {
            let status = guard
                .get(&a.name)
                .map(|st| st.status.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "disconnected".to_string());
            json!({
                "name": a.name,
                "url": a.url,
                "cwd": a.cwd,
                "management": a.management,
                "configured": a.is_configured(),
                "status": status,
            })
        })
        .collect();
    Ok(json!({ "agents": agents }))
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

/// The registry file (`agents.toml`) as `{ path, text }` for the editor. When the
/// file is absent it is seeded with the adopted legacy `remote.toml`, so opening
/// the editor migrates a single-endpoint setup into the multi-agent format on the
/// first save (see [`remote::read_registry_text`]). Tokens live in the file, so
/// this text is editor-only — it is never mixed into the panel/selector views,
/// which stay token-free.
#[tauri::command]
async fn registry_config() -> Result<Value, String> {
    let text = remote::read_registry_text()?;
    let path = remote::registry_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(json!({ "path": path, "text": text }))
}

/// Persist the edited registry (`agents.toml`), **validating structure before it
/// writes** — parses, unique non-empty names, ≤1 `management` — so a bad edit
/// never lands. Returns the refreshed `{ path, text }`; the caller re-reads the
/// remote panel + agent selector to reflect the new registry.
#[tauri::command]
async fn registry_config_write(text: String) -> Result<Value, String> {
    remote::write_registry_text(&text)?;
    registry_config().await
}

/// Activate the remote connection (the explicit "Activate" button): dial `/acp`
/// using the saved config and publish the `oab` tools to the attached agent. The
/// core sidecar must be started first (it is what the tunnel relays to).
#[tauri::command]
async fn remote_connect(
    app: tauri::AppHandle,
    core: tauri::State<'_, Core>,
    remote: tauri::State<'_, remote::Remote>,
    agent: Option<String>,
) -> Result<(), String> {
    let client = {
        let guard = core.0.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| "core not started yet — start the core before connecting".to_string())?
    };
    // `None` ⇒ the management endpoint (legacy single-console behaviour); `Some`
    // ⇒ a specific agent console from the registry.
    let endpoint = remote::resolve_endpoint(agent.as_deref())?;
    remote::connect(app, &remote, client, endpoint).await
}

/// Deactivate a connection. `None` targets the management endpoint.
#[tauri::command]
async fn remote_disconnect(
    app: tauri::AppHandle,
    remote: tauri::State<'_, remote::Remote>,
    agent: Option<String>,
) -> Result<(), String> {
    let name = remote::resolve_name(agent.as_deref())?;
    remote::disconnect(&app, &remote, &name).await;
    Ok(())
}

/// Send a chat turn to an agent (ADR *agent-chat-panel*): pushes a `session/prompt`
/// onto that agent's live `/acp` session. The reply streams back as `agent-update`
/// events tagged with the agent name. `None` targets the management endpoint.
/// Errors if that agent's session is not active.
#[tauri::command]
async fn agent_prompt(
    remote: tauri::State<'_, remote::Remote>,
    agent: Option<String>,
    text: String,
) -> Result<(), String> {
    let name = remote::resolve_name(agent.as_deref())?;
    remote.send_prompt(&name, text).await
}

/// Abandon an agent's in-flight chat turn (`session/cancel`). Best-effort.
/// `None` targets the management endpoint.
#[tauri::command]
async fn agent_cancel(
    remote: tauri::State<'_, remote::Remote>,
    agent: Option<String>,
) -> Result<(), String> {
    let name = remote::resolve_name(agent.as_deref())?;
    remote.send_cancel(&name).await
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
        json!({ "level": "info", "msg": format!("update: downloading v{}…", update.version) }),
    );
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "app-log",
        json!({ "level": "info", "msg": "update: installed — restarting…" }),
    );
    app.restart();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
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
            mcp_target_get,
            mcp_target_set,
            compose_library_get,
            compose_library_set,
            compose_preview,
            deploy_provision,
            deploy_provision_agent,
            deploy_list,
            runtime_context,
            fleet_config,
            fleet_config_write,
            list_aws_profiles,
            list_k8s_contexts,
            resolve_vendor_image_tags,
            list_local_agent_configs,
            read_local_agent_config,
            list_namespaces,
            list_service_accounts,
            k8s_fleet_config,
            k8s_fleet_config_write,
            deploy_scale,
            remote_config,
            remote_agents,
            remote_config_write,
            registry_config,
            registry_config_write,
            remote_connect,
            remote_disconnect,
            agent_prompt,
            agent_cancel,
            check_update,
            install_update
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On app teardown, close the remote /acp session cleanly so the gateway
            // frees the session slot immediately instead of holding it for a resume
            // that will never come (until its TTL / liveness reaper fires). The
            // Disconnect button already does this; this covers Cmd-Q / window close.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let remote = app_handle.state::<remote::Remote>();
                tauri::async_runtime::block_on(remote::disconnect_all(app_handle, &remote));
            }
        });
}
