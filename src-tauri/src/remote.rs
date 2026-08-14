//! Remote reverse-MCP-over-ACP connection (reverse-MCP client ADR, Part B).
//!
//! Studio dials the `/acp` endpoint from [`RemoteConfig`], creates a session
//! declaring its `oab` server, and then **relays** the gateway's tunnelled MCP
//! (`initialize` / `tools/list` / `tools/call`) to the already-running `oab-mcp`
//! **sidecar** (`McpClient`) — no in-process aws link (ADR §4.3). Connection is
//! **operator-triggered** (the "Activate remote connection" button), not on boot.
//!
//! The exact ACP `initialize` / `session/new` params and the end-to-end behaviour
//! are validated against the live gateway once the §5 endpoint exists; this
//! module is structurally complete and compiles under `desktop.yml`.

use std::path::PathBuf;

use acp_tunnel as acp;
use acp_tunnel::config::RemoteConfig;
use acp_tunnel::{Inbound, Session};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::mcp::McpClient;

/// Managed state: the running connection task (abort to disconnect) plus the last
/// status string the UI renders.
#[derive(Default)]
pub struct Remote(pub AsyncMutex<RemoteState>);

#[derive(Default)]
pub struct RemoteState {
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// `"disconnected"` | `"connecting"` | `"connected"` | `"error: …"`.
    pub status: String,
}

/// `~/.config/oab-studio/remote.toml` — beside `fleets.toml`.
pub fn config_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("oab-studio").join("remote.toml"))
        .ok_or_else(|| "no config directory resolved".to_string())
}

/// Raw file text for the editor; a missing file is empty ("not configured").
pub fn read_config_text() -> Result<String, String> {
    let p = config_path()?;
    match std::fs::read_to_string(&p) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("read {}: {e}", p.display())),
    }
}

/// Persist edited text, **validating it parses first** (a bad edit never lands),
/// mirroring the fleets.toml editor.
pub fn write_config_text(text: &str) -> Result<(), String> {
    RemoteConfig::parse(text).map_err(|e| format!("invalid TOML: {e}"))?;
    let p = config_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(&p, text).map_err(|e| format!("write {}: {e}", p.display()))
}

fn load_config() -> Result<RemoteConfig, String> {
    RemoteConfig::parse(&read_config_text()?).map_err(|e| format!("invalid remote.toml: {e}"))
}

fn emit_status<R: Runtime>(app: &AppHandle<R>, status: &str) {
    let _ = app.emit("remote-status", json!({ "status": status }));
}

/// Activate the remote connection: validate config, spawn the connection task.
/// Idempotent — a no-op if already connected.
pub async fn connect<R: Runtime>(
    app: AppHandle<R>,
    remote: &Remote,
    client: McpClient,
) -> Result<(), String> {
    let cfg = load_config()?;
    cfg.validate()?;

    let mut guard = remote.0.lock().await;
    if guard.task.is_some() {
        return Ok(());
    }
    let app_task = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        run_reconnecting(app_task, cfg, client).await;
    });
    guard.task = Some(task);
    guard.status = "connecting".to_string();
    emit_status(&app, "connecting");
    Ok(())
}

/// Deactivate: abort the connection task.
pub async fn disconnect<R: Runtime>(app: &AppHandle<R>, remote: &Remote) {
    let mut guard = remote.0.lock().await;
    if let Some(t) = guard.task.take() {
        t.abort();
    }
    guard.status = "disconnected".to_string();
    emit_status(app, "disconnected");
}

/// Reconnect loop: one attempt, then back off and retry until the task is
/// aborted (by [`disconnect`]).
async fn run_reconnecting<R: Runtime>(app: AppHandle<R>, cfg: RemoteConfig, client: McpClient) {
    loop {
        if let Err(e) = run_once(&app, &cfg, &client).await {
            emit_status(&app, &format!("error: {e}"));
            let _ = app.emit(
                "app-log",
                json!({ "level": "error", "msg": format!("remote: {e}") }),
            );
        }
        emit_status(&app, "connecting");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// One connection: dial, run the `initialize` → `session/new` handshake, then
/// serve the gateway-initiated tunnel until the socket closes.
async fn run_once<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &RemoteConfig,
    client: &McpClient,
) -> Result<(), String> {
    let mut req = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad url: {e}"))?;
    // Bearer rides the WS sub-protocol offer (contract §1). Never logged.
    let proto = HeaderValue::from_str(&acp::bearer_subprotocol(&cfg.token))
        .map_err(|_| "invalid token in sub-protocol header".to_string())?;
    req.headers_mut().insert("Sec-WebSocket-Protocol", proto);

    let (ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("dial {}: {e}", cfg.url))?;
    let (mut write, mut read) = ws.split();

    // Per-connection server id (contract §6.1); the stable name is "oab" (D1).
    let conn_id = uuid::Uuid::new_v4().to_string();
    let mut session = Session::new(vec![acp::oab_server(&conn_id)]);

    // Drive the client handshake: initialize, then session/new declaring "oab".
    let (_id, init) = session.initialize(json!({
        "protocolVersion": "2024-11-05",
        "clientCapabilities": {},
        "clientInfo": { "name": "oab-studio", "version": env!("CARGO_PKG_VERSION") }
    }));
    send(&mut write, &init).await?;

    // Read loop. Handshake responses (to our initialize / session/new) advance
    // the session; gateway-initiated method frames are the tunnel.
    while let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| format!("ws read: {e}"))?;
        let text = match msg {
            WsMessage::Text(t) => t,
            WsMessage::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            WsMessage::Close(_) => return Ok(()),
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
        };
        if text.len() > acp::limits::MAX_FRAME_BYTES {
            return Err("inbound frame exceeds 8 MiB".to_string());
        }
        let frame: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue, // ignore non-JSON keepalives
        };

        // A response (no `method`) is the ack to one of our handshake requests.
        if frame.get("method").is_none() {
            if session.phase() == acp::Phase::Initializing {
                session.on_initialized();
                let (_id, new) = session.open_session(&cfg.cwd);
                send(&mut write, &new).await?;
            } else if session.phase() == acp::Phase::Initialized {
                let sid = frame
                    .get("result")
                    .and_then(|r| r.get("sessionId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                session.on_session_created(sid);
                emit_status(app, "connected");
                let _ = app.emit(
                    "app-log",
                    json!({ "level": "info", "msg": "remote: session active — oab tools published" }),
                );
            }
            continue;
        }

        // Otherwise it is a gateway-initiated tunnel frame.
        match acp::parse_inbound(&frame) {
            Inbound::Connect { id, .. } => {
                send(&mut write, &acp::connect_reply(id, &conn_id)).await?;
            }
            Inbound::Message {
                id,
                method,
                params,
                ..
            } => {
                if let Some(reply) = handle_inner(client, id, &method, params).await {
                    send(&mut write, &reply).await?;
                }
            }
            Inbound::Disconnect { id, .. } => {
                send(&mut write, &acp::disconnect_reply(id)).await?;
            }
            // Cancellation is best-effort; relays are short and not tracked here.
            Inbound::Cancel { .. } | Inbound::Other => {}
        }
    }
    Ok(())
}

/// Answer one inner MCP method. `initialize` is answered locally (the sidecar is
/// already initialized); `tools/list` / `tools/call` are relayed to the sidecar
/// and returned **verbatim**. Returns `None` for a notification (no reply owed).
async fn handle_inner(
    client: &McpClient,
    id: Option<Value>,
    method: &str,
    params: Value,
) -> Option<Value> {
    match method {
        "notifications/initialized" => None,
        "initialize" => id.map(|i| {
            acp::message_reply(
                i,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "oab-studio", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
        }),
        "tools/list" | "tools/call" => {
            let id = id?;
            Some(match client.request(method, params).await {
                Ok(result) => acp::message_reply(id, result),
                Err(e) => acp::message_error(id, -32000, &e),
            })
        }
        _ => id.map(|i| acp::message_error(i, -32601, &format!("method not found: {method}"))),
    }
}

async fn send<S>(write: &mut S, frame: &Value) -> Result<(), String>
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = serde_json::to_string(frame).map_err(|e| format!("encode: {e}"))?;
    write
        .send(WsMessage::Text(text))
        .await
        .map_err(|e| format!("ws write: {e}"))
}
