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
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;
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
    /// The live outbound-chat channel, present only while a session is active.
    /// `agent_prompt` / `agent_cancel` push into it; `run_once` drains it onto the
    /// socket. `None` ⇒ nothing to prompt (disconnected / mid-handshake).
    prompt_tx: Option<mpsc::UnboundedSender<OutMsg>>,
}

/// A chat action the UI asks the live connection to perform (ADR
/// *agent-chat-panel*, Part B). Pushed by the `agent_prompt` / `agent_cancel`
/// commands into the per-connection channel that `run_once` drains onto the WS.
pub enum OutMsg {
    /// Send a chat turn (`session/prompt`).
    Prompt(String),
    /// Abandon the in-flight turn (`session/cancel`).
    Cancel,
}

impl Remote {
    /// Send a chat turn to the connected agent. Errors if no session is live.
    pub async fn send_prompt(&self, text: String) -> Result<(), String> {
        self.push(OutMsg::Prompt(text)).await
    }

    /// Cancel the in-flight turn (best-effort).
    pub async fn send_cancel(&self) -> Result<(), String> {
        self.push(OutMsg::Cancel).await
    }

    async fn push(&self, msg: OutMsg) -> Result<(), String> {
        let guard = self.0.lock().await;
        let tx = guard
            .prompt_tx
            .as_ref()
            .ok_or_else(|| "not connected — activate the remote connection first".to_string())?;
        // A live `prompt_tx` whose receiver has gone means the connection is
        // tearing down (the socket closed and `run_reconnecting` is about to
        // retract the channel). Report that as clearly as the missing-channel
        // case above rather than a bare "connection closed", which reads like a
        // different, harder failure (review #3).
        tx.send(msg)
            .map_err(|_| "not connected — the remote connection just closed; reactivate it".to_string())
    }
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
    guard.prompt_tx = None;
    emit_status(app, "disconnected");
}

/// Reconnect loop: one attempt, then back off and retry until the task is
/// aborted (by [`disconnect`]).
async fn run_reconnecting<R: Runtime>(app: AppHandle<R>, cfg: RemoteConfig, client: McpClient) {
    loop {
        let result = run_once(&app, &cfg, &client).await;
        // The socket is gone — retract the outbound-chat channel so a prompt
        // between attempts fails fast rather than dropping into a dead sink.
        app.state::<Remote>().0.lock().await.prompt_tx = None;
        if let Err(e) = result {
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
    // Native (non-browser) client: the bearer rides the `Authorization` header
    // (the gateway reads `ws_bearer_token` from it), and the WS sub-protocol
    // offers only `acp.v1`. This split matters: tokio-tungstenite 0.23 parses the
    // request's `Sec-WebSocket-Protocol` with `split(",")` and does *not* trim, so
    // a combined `openab.bearer.<token>, acp.v1` offer is stored as
    // [`openab.bearer.<token>`, ` acp.v1`] (note the leading space) and never
    // matches the server's echoed `acp.v1` — `Server sent an invalid subprotocol`.
    // `bearer_subprotocol()` remains the browser form (browsers can't set
    // `Authorization` on a WS upgrade). Both values carry secrets — never logged.
    req.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", cfg.token))
            .map_err(|_| "invalid token in Authorization header".to_string())?,
    );
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(acp::ACP_SUBPROTOCOL),
    );

    let (ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("dial {}: {e}", cfg.url))?;
    let (mut write, mut read) = ws.split();

    // Per-connection server id (contract §6.1); the stable name is "oab" (D1).
    let conn_id = uuid::Uuid::new_v4().to_string();
    let mut session = Session::new(vec![acp::oab_server(&conn_id)]);

    // Drive the client handshake: initialize, then session/new declaring "oab".
    // This is the outer **ACP** handshake — `protocolVersion` is a u16 integer
    // (the gateway deserializes it as `u16`). Do not copy MCP's date string here;
    // the MCP date string is correct only for the *tunnelled* inner `initialize`
    // in `handle_inner` below.
    let (_id, init) = session.initialize(json!({
        "protocolVersion": acp::ACP_PROTOCOL_VERSION,
        "clientCapabilities": {},
        "clientInfo": { "name": "oab-studio", "version": env!("CARGO_PKG_VERSION") }
    }));
    send(&mut write, &init).await?;

    // Outbound-chat channel: `agent_prompt`/`agent_cancel` push `OutMsg`s here and
    // the loop below drains them onto the socket. It is published into
    // `RemoteState.prompt_tx` only once the session is active (below), so the UI
    // can never prompt a half-open session; `run_reconnecting` retracts it on exit.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutMsg>();
    // The id of the in-flight `session/prompt`. A prompt result is method-less —
    // the same shape as a handshake ack — so the turn is ended by matching this id,
    // never by phase (ADR §3, "id correlation").
    let mut pending_prompt: Option<u64> = None;

    // Relay replies (tunnelled `tools/list` / `tools/call`) come back through this
    // channel instead of being sent inline, so a slow sidecar round-trip inside
    // `handle_inner` no longer stalls the loop — queued prompts, cancels and chat
    // chunks stay responsive while a `tools/call` is in flight (review #2). The
    // single writer stays in the loop, so frames are still serialized on the wire.
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Value>();

    // Keepalive: Cloudflare's tunnel idle-closes a WS with no traffic (~100s) and
    // tokio-tungstenite never pings on its own, so an idle `/acp` connection flaps
    // roughly every 2 min — and until `session/resume` lands, each reconnect opens
    // a fresh channel (lost agent context). A periodic WS Ping well inside that
    // window counts as traffic and keeps the tunnel open between prompts. `Skip`
    // missed-tick behaviour avoids a burst of pings if the loop was ever busy.
    let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(45));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Loop over BOTH inbound frames and outbound chat actions. `write` never leaves
    // the task; commands reach it only through `out_rx`.
    loop {
        tokio::select! {
            // Outbound: a queued chat action from the UI. The `Some` pattern
            // disables this arm if the channel ever closes, so a `None` can never
            // busy-spin the loop (review #4); in practice `out_tx` lives for the
            // whole run, so it stays open until teardown.
            Some(out) = out_rx.recv() => {
                match out {
                    OutMsg::Prompt(text) => {
                        // In-flight guard (review #1): single-shot turn model. A
                        // second prompt while one is pending would overwrite
                        // `pending_prompt` and orphan the first turn's `turn_end`
                        // (the panel spinner would hang). Reject rather than clobber
                        // — Part C's turn management gates this too, but the backend
                        // must not depend on the UI for its own correctness.
                        if pending_prompt.is_some() {
                            let _ = app.emit(
                                "app-log",
                                json!({ "level": "warn", "msg": "remote: prompt ignored — a turn is already in flight" }),
                            );
                        } else if let Some((id, frame)) = session.prompt(&text) {
                            pending_prompt = Some(id);
                            send(&mut write, &frame).await?;
                        }
                    }
                    OutMsg::Cancel => {
                        if let Some(frame) = session.cancel() {
                            send(&mut write, &frame).await?;
                        }
                    }
                }
            }

            // Outbound: a relay reply produced by a spawned `handle_inner` task
            // (review #2). Same `Some`-pattern guard against a closed channel.
            Some(reply) = reply_rx.recv() => {
                send(&mut write, &reply).await?;
            }

            // Keepalive tick: send a WS Ping directly on `write` (the `send` helper
            // only frames JSON Text). The read arm ignores the returning Pong
            // (`Ping | Pong | Frame => continue`), so this composes with the rest of
            // the loop without touching the inbound path.
            _ = keepalive.tick() => {
                write
                    .send(WsMessage::Ping(Vec::new()))
                    .await
                    .map_err(|e| format!("ws keepalive ping: {e}"))?;
            }

            // Inbound: a frame from the gateway.
            msg = read.next() => {
                let Some(msg) = msg else { break };
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

                // A response (no `method`) is a handshake ack or a prompt result.
                if frame.get("method").is_none() {
                    match session.phase() {
                        acp::Phase::Initializing => {
                            session.on_initialized();
                            let (_id, new) = session.open_session(&cfg.cwd);
                            send(&mut write, &new).await?;
                        }
                        acp::Phase::Initialized => {
                            let sid = frame
                                .get("result")
                                .and_then(|r| r.get("sessionId"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            session.on_session_created(sid);
                            // Now a turn can be sent — publish the outbound channel.
                            app.state::<Remote>().0.lock().await.prompt_tx = Some(out_tx.clone());
                            emit_status(app, "connected");
                            let _ = app.emit(
                                "app-log",
                                json!({ "level": "info", "msg": "remote: session active — oab tools published" }),
                            );
                        }
                        acp::Phase::SessionActive => {
                            // End the turn iff this is our in-flight prompt's result.
                            let fid = frame.get("id").and_then(Value::as_u64);
                            if fid.is_some() && fid == pending_prompt {
                                pending_prompt = None;
                                let stop = frame
                                    .get("result")
                                    .and_then(|r| r.get("stopReason"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("end_turn")
                                    .to_string();
                                let _ = app.emit(
                                    "agent-update",
                                    json!({ "kind": "turn_end", "stopReason": stop }),
                                );
                            }
                        }
                        acp::Phase::New => {}
                    }
                    continue;
                }

                // Otherwise it is a gateway-initiated frame: the reverse-MCP tunnel
                // or a streamed chat chunk.
                match acp::parse_inbound(&frame) {
                    Inbound::Connect { id, .. } => {
                        send(&mut write, &acp::connect_reply(id, &conn_id)).await?;
                    }
                    Inbound::Message { id, method, params, .. } => {
                        // Relay to the sidecar off the loop (review #2): a slow
                        // `tools/call` must not block queued prompts/cancels or
                        // streamed chat chunks. The reply returns via `reply_tx`
                        // and is written by the loop's single writer. `McpClient`
                        // is `Arc`-backed, so the clone is cheap; each reply carries
                        // its own id, so out-of-order completion is fine for MCP.
                        let client = client.clone();
                        let reply_tx = reply_tx.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Some(reply) = handle_inner(&client, id, &method, params).await {
                                let _ = reply_tx.send(reply);
                            }
                        });
                    }
                    Inbound::Disconnect { id, .. } => {
                        send(&mut write, &acp::disconnect_reply(id)).await?;
                    }
                    // A piece of the agent's chat reply → forward to the panel.
                    Inbound::AgentChunk { text } => {
                        let _ = app.emit("agent-update", json!({ "kind": "chunk", "text": text }));
                    }
                    Inbound::Cancel { .. } | Inbound::Other => {}
                }
            }
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
