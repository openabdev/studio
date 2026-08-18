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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acp_tunnel as acp;
use acp_tunnel::config::{AgentEndpoint, AgentRegistry, RemoteConfig};
use acp_tunnel::{DisconnectReason, Inbound, Session};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::mcp::McpClient;

/// How often the client probes an **idle** `/acp` socket for liveness. The probe
/// is a request the gateway answers itself (`-32601`), so it costs no agent turn.
/// Any inbound frame counts as liveness; two silent probe intervals in a row ⇒
/// the socket is half-open and we reconnect. Keeps the socket warm too, so an
/// intermediary's idle timeout can't reset it mid-think (the RST-churn source).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// Client-side ceiling on a single in-flight turn. Past this we stop trusting a
/// socket that has produced no result and force a reconnect — rather than waiting
/// indefinitely on a possibly-wedged peer (ADR browser-tunnel-liveness R3). Mirrors
/// katashiro's `ACP_PROMPT_TIMEOUT_MS`.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(600);

/// Managed state: a map of **per-agent** connections keyed by the endpoint name
/// (ADR agent-consoles Part B — `RemoteState` is no longer a singleton). Each
/// agent console dials its own endpoint; the management console is just the entry
/// keyed by the `management` endpoint's name. The map grows on connect and each
/// entry carries its own task/status/reconnect, so opening one console never
/// disturbs another.
#[derive(Default)]
pub struct Remote(pub AsyncMutex<HashMap<String, RemoteState>>);

#[derive(Default)]
pub struct RemoteState {
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// `"disconnected"` | `"connecting"` | `"connected"` | `"error: …"`.
    pub status: String,
    /// The live outbound-chat channel, present only while a session is active.
    /// `agent_prompt` / `agent_cancel` push into it; `run_once` drains it onto the
    /// socket. `None` ⇒ nothing to prompt (disconnected / mid-handshake).
    prompt_tx: Option<mpsc::UnboundedSender<OutMsg>>,
    /// Set by [`disconnect`] so [`run_reconnecting`] stops instead of reconnecting
    /// after the current attempt tears down. Shared with the reconnect task.
    stop: Option<Arc<AtomicBool>>,
}

/// A chat action the UI asks the live connection to perform (ADR
/// *agent-chat-panel*, Part B). Pushed by the `agent_prompt` / `agent_cancel`
/// commands into the per-connection channel that `run_once` drains onto the WS.
pub enum OutMsg {
    /// Send a chat turn (`session/prompt`).
    Prompt(String),
    /// Abandon the in-flight turn (`session/cancel`).
    Cancel,
    /// User is disconnecting: abandon any in-flight turn and close the socket
    /// **cleanly** (a WS Close frame) so the gateway can release the session slot
    /// immediately instead of waiting out its TTL / liveness reaper.
    Shutdown,
}

impl Remote {
    /// Send a chat turn to the named agent. Errors if that agent's session is not
    /// live (each agent console has its own connection, so the target is explicit).
    pub async fn send_prompt(&self, agent: &str, text: String) -> Result<(), String> {
        self.push(agent, OutMsg::Prompt(text)).await
    }

    /// Cancel the named agent's in-flight turn (best-effort).
    pub async fn send_cancel(&self, agent: &str) -> Result<(), String> {
        self.push(agent, OutMsg::Cancel).await
    }

    async fn push(&self, agent: &str, msg: OutMsg) -> Result<(), String> {
        let guard = self.0.lock().await;
        let tx = guard
            .get(agent)
            .and_then(|st| st.prompt_tx.as_ref())
            .ok_or_else(|| "not connected — activate the remote connection first".to_string())?;
        // A live `prompt_tx` whose receiver has gone means the connection is
        // tearing down (the socket closed and `run_reconnecting` is about to
        // retract the channel). Report that as clearly as the missing-channel
        // case above rather than a bare "connection closed", which reads like a
        // different, harder failure (review #3).
        tx.send(msg).map_err(|_| {
            "not connected — the remote connection just closed; reactivate it".to_string()
        })
    }
}

/// Legacy single-endpoint config `~/.config/oab-studio/remote.toml` — beside
/// `fleets.toml`. Still the file the current management-console editor writes; the
/// registry adopts it as one `management = true` entry when `agents.toml` is
/// absent (ADR agent-consoles Part B back-compat).
pub fn config_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("oab-studio").join("remote.toml"))
        .ok_or_else(|| "no config directory resolved".to_string())
}

/// The per-agent endpoint registry `~/.config/oab-studio/agents.toml`. When
/// present it is the source of truth; the legacy `remote.toml` is the fallback.
pub fn registry_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("oab-studio").join("agents.toml"))
        .ok_or_else(|| "no config directory resolved".to_string())
}

/// The stable name given to a legacy `remote.toml` when it is adopted into the
/// registry as the single management entry.
pub const LEGACY_MANAGEMENT_NAME: &str = "management";

/// Load the endpoint registry: prefer `agents.toml`; if it is missing or empty,
/// adopt the legacy `remote.toml` as one `management = true` entry so existing
/// single-endpoint setups keep working untouched.
pub fn load_registry() -> Result<AgentRegistry, String> {
    let rp = registry_path()?;
    match std::fs::read_to_string(&rp) {
        Ok(s) if !s.trim().is_empty() => {
            return AgentRegistry::parse(&s).map_err(|e| format!("invalid agents.toml: {e}"))
        }
        Ok(_) => {} // present but empty → fall through to legacy
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("read {}: {e}", rp.display())),
    }
    let legacy = load_config()?;
    if legacy.is_configured() {
        Ok(AgentRegistry::from_legacy(legacy, LEGACY_MANAGEMENT_NAME))
    } else {
        // Nothing configured anywhere → empty registry (the app shows
        // "not configured" rather than erroring).
        Ok(AgentRegistry::default())
    }
}

/// Resolve a command's optional `agent` argument to just the connection **key**
/// (the map name). `Some` is already the key; `None` resolves to the management
/// endpoint's name. Cheaper than [`resolve_endpoint`] when only the key is needed
/// (disconnect / prompt / cancel target an already-open connection).
pub fn resolve_name(agent: Option<&str>) -> Result<String, String> {
    match agent {
        Some(n) => Ok(n.to_string()),
        None => load_registry()?
            .management()
            .map(|e| e.name.clone())
            .ok_or_else(|| "no management agent configured".to_string()),
    }
}

/// Resolve a command's optional `agent` argument to a concrete endpoint. `None`
/// means the legacy single-endpoint commands — resolve to the management entry.
pub fn resolve_endpoint(agent: Option<&str>) -> Result<AgentEndpoint, String> {
    let reg = load_registry()?;
    match agent {
        Some(name) => reg
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no agent named {name:?} in the registry")),
        None => reg
            .management()
            .cloned()
            .ok_or_else(|| "no management agent configured".to_string()),
    }
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

/// Raw text for the **registry** editor (`agents.toml`). Prefer the file; when it
/// is absent or empty, seed the editor with the *adopted* registry — the legacy
/// `remote.toml` rendered as `agents.toml` — so opening the editor migrates an
/// old single-endpoint setup into the new multi-agent format on first save.
/// Nothing configured anywhere → empty ("not configured").
pub fn read_registry_text() -> Result<String, String> {
    let rp = registry_path()?;
    match std::fs::read_to_string(&rp) {
        Ok(s) if !s.trim().is_empty() => return Ok(s),
        Ok(_) => {} // present but empty → seed from legacy below
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("read {}: {e}", rp.display())),
    }
    let reg = load_registry()?;
    if reg.agents.is_empty() {
        Ok(String::new())
    } else {
        Ok(reg.to_toml())
    }
}

/// Persist the edited registry, **validating structure first** so a bad edit
/// never lands (mirroring the fleets.toml / remote.toml editors): it must parse,
/// every `[[agent]]` needs a unique non-empty name, and at most one may carry
/// `management = true`. Per-endpoint url/token completeness is deliberately *not*
/// enforced here — a half-filled entry can be saved and is only checked at dial
/// time, exactly as [`AgentRegistry::validate`] documents.
pub fn write_registry_text(text: &str) -> Result<(), String> {
    let reg = AgentRegistry::parse(text).map_err(|e| format!("invalid TOML: {e}"))?;
    reg.validate().map_err(|e| format!("invalid agents.toml: {e}"))?;
    let p = registry_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    std::fs::write(&p, text).map_err(|e| format!("write {}: {e}", p.display()))
}

fn load_config() -> Result<RemoteConfig, String> {
    RemoteConfig::parse(&read_config_text()?).map_err(|e| format!("invalid remote.toml: {e}"))
}

/// Emit a connection-status change for a specific agent. The `agent` field lets
/// the UI route the update to the matching console (the management console keys
/// off the management endpoint's name); a single-agent UI can ignore it.
fn emit_status<R: Runtime>(app: &AppHandle<R>, agent: &str, status: &str) {
    let _ = app.emit("remote-status", json!({ "agent": agent, "status": status }));
}

/// Activate a named agent's connection: validate the endpoint, spawn its
/// connection task keyed under the endpoint name. Idempotent — a no-op if that
/// agent is already connecting/connected. Reverse-MCP `oab` fleet-control tools
/// are published **only** when the endpoint is `management` (least privilege): an
/// ordinary agent console dials, chats, and (later) edits files without granting
/// the agent fleet control.
pub async fn connect<R: Runtime>(
    app: AppHandle<R>,
    remote: &Remote,
    client: McpClient,
    endpoint: AgentEndpoint,
) -> Result<(), String> {
    endpoint.validate()?;
    let agent = endpoint.name.clone();
    let cfg = endpoint.conn();
    let management = endpoint.management;

    let mut guard = remote.0.lock().await;
    let st = guard.entry(agent.clone()).or_default();
    if st.task.is_some() {
        return Ok(());
    }
    let app_task = app.clone();
    let agent_task = agent.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_task = stop.clone();
    let task = tauri::async_runtime::spawn(async move {
        run_reconnecting(app_task, agent_task, cfg, management, client, stop_task).await;
    });
    st.task = Some(task);
    st.stop = Some(stop);
    st.status = "connecting".to_string();
    emit_status(&app, &agent, "connecting");
    Ok(())
}

/// Deactivate. Unlike a bare `task.abort()`, this asks the live loop to close the
/// socket **cleanly** first (a `session/cancel` for any in-flight turn + a WS Close
/// frame) so the gateway releases the session slot immediately rather than holding
/// it for a resume that will never come (until its TTL / liveness reaper fires).
/// Safe to call on app teardown and from the Disconnect button; a no-op if idle.
pub async fn disconnect<R: Runtime>(app: &AppHandle<R>, remote: &Remote, agent: &str) {
    // Flag the reconnect loop to stop, and grab the pieces we need to tear down
    // outside the lock (so the loop can take the lock to retract `prompt_tx`).
    let (task, tx) = {
        let mut guard = remote.0.lock().await;
        let Some(st) = guard.get_mut(agent) else {
            return; // never connected — nothing to tear down
        };
        if let Some(stop) = st.stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        (st.task.take(), st.prompt_tx.take())
    };
    // Ask the running connection to flush a graceful close.
    if let Some(tx) = tx {
        let _ = tx.send(OutMsg::Shutdown);
    }
    // Give the loop a brief window to send the Close frame, then stop the task for
    // good. (`stop` already prevents a reconnect; the abort is the hard backstop in
    // case the loop is wedged mid-await.)
    if let Some(t) = task {
        tokio::time::sleep(Duration::from_millis(400)).await;
        t.abort();
    }
    if let Some(st) = remote.0.lock().await.get_mut(agent) {
        st.status = "disconnected".to_string();
        st.prompt_tx = None;
    }
    emit_status(app, agent, "disconnected");
    let _ = app.emit(
        "app-log",
        json!({ "level": "info", "msg": format!("remote: {agent} disconnected by user") }),
    );
}

/// Disconnect **every** live agent connection (app teardown): close each socket
/// cleanly so the gateway frees all held session slots at once.
pub async fn disconnect_all<R: Runtime>(app: &AppHandle<R>, remote: &Remote) {
    let agents: Vec<String> = remote.0.lock().await.keys().cloned().collect();
    for agent in agents {
        disconnect(app, remote, &agent).await;
    }
}

/// The reverse-MCP `oab` server declaration for a connection, **only** when this
/// endpoint is the management binding. A non-management agent console declares no
/// servers, so the gateway never tunnels Studio's fleet-control tools to it
/// (least privilege, ADR agent-consoles Part A).
fn servers_for(management: bool, conn_id: &str) -> Vec<acp::ServerDecl> {
    if management {
        vec![acp::oab_server(conn_id)]
    } else {
        vec![]
    }
}

/// Reconnect loop: one attempt, then back off and retry until the task is
/// aborted (by [`disconnect`]).
///
/// The [`Session`] persists **across** attempts: once it has a `session_id`, a
/// reconnect **resumes** that agent session instead of opening a brand-new one, so
/// a brief socket blip no longer stacks a fresh server-side session (and orphaned
/// turn) on every RST. Each attempt still mints a fresh per-connection server id
/// via [`Session::redeclare`], which also resets the phase so the new socket runs
/// a full `initialize` handshake.
async fn run_reconnecting<R: Runtime>(
    app: AppHandle<R>,
    agent: String,
    cfg: RemoteConfig,
    management: bool,
    client: McpClient,
    stop: Arc<AtomicBool>,
) {
    let mut session = Session::new(servers_for(
        management,
        &uuid::Uuid::new_v4().to_string(),
    ));
    // Consecutive-failure counter driving the reconnect backoff. Reset to 0 once an
    // attempt has held a live connection for a while (see below), so a long-running
    // session that blips reconnects promptly instead of at the capped delay.
    let mut attempt: u32 = 0;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let conn_id = uuid::Uuid::new_v4().to_string();
        // Fresh per-connection server id + phase reset; keeps `session_id` so
        // `run_once` picks the resume path when one exists. `servers_for` gates the
        // `oab` declaration on `management`, so a non-management console re-attaches
        // without ever republishing fleet-control tools.
        session.redeclare(servers_for(management, &conn_id));
        if session.session_id().is_some() {
            let _ = app.emit(
                "app-log",
                json!({ "level": "info", "msg": format!("remote: {agent} reconnecting — will resume the existing session") }),
            );
        }

        let started = Instant::now();
        let result = run_once(&app, &agent, &cfg, management, &client, &mut session, &conn_id).await;
        // The socket is gone — retract this agent's outbound-chat channel so a
        // prompt between attempts fails fast rather than dropping into a dead sink.
        if let Some(st) = app.state::<Remote>().0.lock().await.get_mut(&agent) {
            st.prompt_tx = None;
        }
        // User-initiated disconnect: stop here instead of reconnecting (and skip
        // the misleading "reconnecting…" log).
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // A drop after a decently long-lived connection is a fresh incident, not an
        // escalating failure — reset the backoff so we retry quickly. A fast failure
        // (bad dial / handshake) keeps escalating.
        if started.elapsed() >= Duration::from_secs(15) {
            attempt = 0;
        }
        if let Err(e) = result {
            // Classify so the status line says *why* (network / auth rejected /
            // server at capacity / protocol) instead of a raw error blob.
            let reason = DisconnectReason::classify(&e);
            emit_status(&app, &agent, &format!("error: {}", reason.label()));
            let _ = app.emit(
                "app-log",
                json!({ "level": "error", "msg": format!("remote: {agent} {} — {e}", reason.label()) }),
            );
        }
        // Exponential backoff (capped 30s) + per-connection jitter, so a flapping
        // link doesn't hammer the gateway — which only has a handful of session
        // slots — on a fixed cadence.
        let salt = conn_id.as_bytes().first().copied().unwrap_or(0);
        let delay = acp::backoff_delay(attempt, salt);
        attempt = attempt.saturating_add(1);
        emit_status(&app, &agent, "connecting");
        let _ = app.emit(
            "app-log",
            json!({ "level": "info", "msg": format!("remote: {agent} reconnecting in {}s…", delay.as_secs()) }),
        );
        tokio::time::sleep(delay).await;
    }
}

/// One connection: dial, run the `initialize` → `session/new` (or
/// `session/resume`) handshake, then serve the gateway-initiated tunnel until the
/// socket closes — or until the client heartbeat / turn ceiling decides the socket
/// is dead and returns `Err` so [`run_reconnecting`] reconnects.
async fn run_once<R: Runtime>(
    app: &AppHandle<R>,
    agent: &str,
    cfg: &RemoteConfig,
    management: bool,
    client: &McpClient,
    session: &mut Session,
    conn_id: &str,
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

    // The URL carries no secret (the bearer rides the Authorization header), so it
    // is safe to show which endpoint we're dialing — the reconnect cycle is
    // otherwise invisible in Activity until it succeeds or errors.
    let _ = app.emit(
        "app-log",
        json!({ "level": "info", "msg": format!("remote: {agent} dialing {}…", cfg.url) }),
    );
    let (ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("dial {}: {e}", cfg.url))?;
    let (mut write, mut read) = ws.split();

    // `session` and the per-connection server id (`conn_id`, contract §6.1; stable
    // name "oab", D1) are owned by `run_reconnecting` and threaded in, so the
    // session_id survives a reconnect and drives the resume path below.

    // Drive the client handshake: initialize, then session/resume (if we carry a
    // session id from a previous attempt) or session/new declaring "oab".
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

    // True once we've sent a `session/resume` (vs `session/new`) this connection,
    // so a handshake error can fall back to a fresh session and the "active" log
    // can say "resumed".
    let mut resume_attempted = false;
    // Liveness bookkeeping for the heartbeat. Any inbound frame refreshes
    // `last_activity` and clears `hb_outstanding`; a probe that goes a full
    // interval unanswered marks the socket dead.
    let mut last_activity = Instant::now();
    let mut hb_outstanding = false;
    // When the in-flight turn must be given up on (item 4). Set on send, cleared
    // when its result arrives.
    let mut prompt_deadline: Option<Instant> = None;
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Keepalive (#54): the heartbeat probe above only runs once the session is
    // active, but Cloudflare's tunnel idle-closes a WS with no traffic (~100s) and
    // tokio-tungstenite never pings on its own — so an idle `/acp` connection flaps
    // roughly every 2 min *during handshake*, before the heartbeat can cover it.
    // A periodic low-level WS Ping counts as traffic and keeps the tunnel open in
    // that window (and complements the JSON heartbeat once active). `Skip`
    // missed-tick behaviour avoids a burst of pings if the loop was ever busy.
    let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(45));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The reason this connection ended. `break 'conn` sets it; a clean EOF / close
    // leaves it `Ok`. Threaded out so the post-loop cleanup (abandoned-turn notice)
    // runs on every exit path.
    let mut outcome: Result<(), String> = Ok(());

    // Loop over inbound frames, outbound chat actions, and the liveness timers.
    // `write` never leaves the task; commands reach it only through `out_rx`.
    'conn: loop {
        tokio::select! {
            // Liveness + turn-ceiling timer. Fires every HEARTBEAT_INTERVAL.
            _ = heartbeat.tick() => {
                // Nothing to probe until the session is live.
                if session.phase() != acp::Phase::SessionActive {
                    continue;
                }
                // (item 4) The in-flight turn has run past the client ceiling with
                // no result: stop trusting this socket and reconnect.
                if let Some(deadline) = prompt_deadline {
                    if Instant::now() >= deadline {
                        let _ = app.emit(
                            "app-log",
                            json!({ "level": "warn", "msg": format!(
                                "remote: turn exceeded {}s with no result — dropping the socket and reconnecting",
                                PROMPT_TIMEOUT.as_secs()
                            ) }),
                        );
                        outcome = Err("session/prompt timed out — reconnecting".to_string());
                        break 'conn;
                    }
                }
                // (item 1) Recent inbound traffic ⇒ alive; nothing to do.
                if last_activity.elapsed() < HEARTBEAT_INTERVAL {
                    hb_outstanding = false;
                    continue;
                }
                if hb_outstanding {
                    // A probe sent a full interval ago drew no response of any kind:
                    // the socket is dead / half-open (the RST may never reach us).
                    let _ = app.emit(
                        "app-log",
                        json!({ "level": "warn", "msg": "remote: liveness probe unanswered — socket half-open, reconnecting" }),
                    );
                    outcome = Err("heartbeat timeout — no response to liveness probe".to_string());
                    break 'conn;
                }
                // Idle: send a probe the gateway answers itself (-32601). No agent
                // turn, no tokens; also keeps the socket warm against idle RSTs.
                let (_id, ping) = session.heartbeat();
                if let Err(e) = send(&mut write, &ping).await {
                    outcome = Err(e);
                    break 'conn;
                }
                hb_outstanding = true;
            }

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
                            if let Err(e) = send(&mut write, &frame).await {
                                outcome = Err(e);
                                break 'conn;
                            }
                            // Start the turn ceiling (item 4); cleared on its result.
                            prompt_deadline = Some(Instant::now() + PROMPT_TIMEOUT);
                        }
                    }
                    OutMsg::Cancel => {
                        // Record the operator action so the Activity log distinguishes an
                        // operator Stop from a gateway-side cancel or an error drop (the
                        // turn's `turn_end` carries stopReason `cancelled` regardless).
                        let _ = app.emit(
                            "app-log",
                            json!({ "level": "info", "msg": format!("remote: {agent} turn cancelled by operator") }),
                        );
                        if let Some(frame) = session.cancel() {
                            if let Err(e) = send(&mut write, &frame).await {
                                outcome = Err(e);
                                break 'conn;
                            }
                        }
                    }
                    OutMsg::Shutdown => {
                        // Best-effort graceful close: abandon any in-flight turn,
                        // then send a WS Close frame so the gateway sees an
                        // intentional close (not a resumable blip) and frees the
                        // session slot now. Errors are ignored — we're leaving.
                        if let Some(frame) = session.cancel() {
                            let _ = send(&mut write, &frame).await;
                        }
                        let _ = write.send(WsMessage::Close(None)).await;
                        let _ = write.flush().await;
                        outcome = Ok(());
                        break 'conn;
                    }
                }
            }

            // Outbound: a relay reply produced by a spawned `handle_inner` task
            // (review #2). Same `Some`-pattern guard against a closed channel.
            Some(reply) = reply_rx.recv() => {
                if let Err(e) = send(&mut write, &reply).await {
                    outcome = Err(e);
                    break 'conn;
                }
            }

            // Keepalive tick: send a WS Ping directly on `write` (the `send` helper
            // only frames JSON Text). The read arm ignores the returning Pong
            // (`Ping | Pong | Frame => continue`), so this composes with the rest of
            // the loop without touching the inbound path.
            _ = keepalive.tick() => {
                if let Err(e) = write.send(WsMessage::Ping(Vec::new())).await {
                    outcome = Err(format!("ws keepalive ping: {e}"));
                    break 'conn;
                }
                // Keepalive fires every ~45s — surface it at DEBUG so the Activity
                // pane isn't dominated by it. The operator can switch the pane to
                // DEBUG+ to see the tunnel being kept warm between prompts.
                let _ = app.emit(
                    "app-log",
                    json!({ "level": "debug", "msg": "remote: keepalive ping sent" }),
                );
            }

            // Inbound: a frame from the gateway.
            msg = read.next() => {
                let Some(msg) = msg else { break 'conn }; // stream ended cleanly
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        outcome = Err(format!("ws read: {e}"));
                        break 'conn;
                    }
                };
                // Any inbound frame — even a Ping/Pong or our own probe's reply —
                // proves the socket + gateway are alive (item 1).
                last_activity = Instant::now();
                hb_outstanding = false;
                let text = match msg {
                    WsMessage::Text(t) => t,
                    WsMessage::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    WsMessage::Close(_) => break 'conn, // clean close
                    WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
                };
                if text.len() > acp::limits::MAX_FRAME_BYTES {
                    outcome = Err("inbound frame exceeds 8 MiB".to_string());
                    break 'conn;
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
                            // Resume the existing agent session if we have one
                            // (item 2); otherwise open a fresh one.
                            let handshake = match session.resume(&cfg.cwd) {
                                Some((_id, resume)) => {
                                    resume_attempted = true;
                                    resume
                                }
                                None => session.open_session(&cfg.cwd).1,
                            };
                            if let Err(e) = send(&mut write, &handshake).await {
                                outcome = Err(e);
                                break 'conn;
                            }
                        }
                        acp::Phase::Initialized => {
                            // A handshake error here is either session/new failing
                            // (fatal) or session/resume rejected because the gateway
                            // already reaped the session — fall back to a fresh one.
                            if let Some(err) = frame.get("error") {
                                let emsg = err
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown error");
                                if resume_attempted {
                                    let _ = app.emit(
                                        "app-log",
                                        json!({ "level": "warn", "msg": format!(
                                            "remote: {agent} session/resume rejected ({emsg}) — opening a fresh session"
                                        ) }),
                                    );
                                    session.forget_session();
                                    resume_attempted = false;
                                    let (_id, new) = session.open_session(&cfg.cwd);
                                    if let Err(e) = send(&mut write, &new).await {
                                        outcome = Err(e);
                                        break 'conn;
                                    }
                                } else {
                                    outcome = Err(format!("session handshake rejected: {emsg}"));
                                    break 'conn;
                                }
                            } else {
                                let sid = frame
                                    .get("result")
                                    .and_then(|r| r.get("sessionId"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                session.on_session_created(sid);
                                // Now a turn can be sent — publish this agent's outbound channel.
                                if let Some(st) =
                                    app.state::<Remote>().0.lock().await.get_mut(agent)
                                {
                                    st.prompt_tx = Some(out_tx.clone());
                                    st.status = "connected".to_string();
                                }
                                emit_status(app, agent, "connected");
                                // Only the management binding declares the `oab` server; an
                                // agent console runs chat-only (least privilege). Wording:
                                // this is Studio *declaring* the server on session/new — the
                                // tools appear only once the agent connects back to it over
                                // the reverse-MCP tunnel (see the Inbound::Connect /
                                // tools/list logs below). "declared", not "published", so the
                                // log doesn't read as "the agent has them".
                                let tools = if management {
                                    if resume_attempted {
                                        " — oab server re-declared (awaiting agent connect)"
                                    } else {
                                        " — oab server declared (awaiting agent connect)"
                                    }
                                } else {
                                    ""
                                };
                                let verb = if resume_attempted { "resumed" } else { "active" };
                                let _ = app.emit(
                                    "app-log",
                                    json!({ "level": "info", "msg": format!("remote: {agent} session {verb}{tools}") }),
                                );
                            }
                        }
                        acp::Phase::SessionActive => {
                            // End the turn iff this is our in-flight prompt's result.
                            let fid = frame.get("id").and_then(Value::as_u64);
                            if fid.is_some() && fid == pending_prompt {
                                pending_prompt = None;
                                prompt_deadline = None;
                                let stop = frame
                                    .get("result")
                                    .and_then(|r| r.get("stopReason"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("end_turn")
                                    .to_string();
                                let _ = app.emit(
                                    "agent-update",
                                    json!({ "agent": agent, "kind": "turn_end", "stopReason": stop }),
                                );
                            }
                            // Any other method-less frame (e.g. a heartbeat probe's
                            // -32601 reply) is ignored — it already counted as
                            // liveness above.
                        }
                        acp::Phase::New => {}
                    }
                    continue;
                }

                // Otherwise it is a gateway-initiated frame: the reverse-MCP tunnel
                // or a streamed chat chunk.
                match acp::parse_inbound(&frame) {
                    Inbound::Connect { id, .. } => {
                        // The agent (via the gateway) opened the reverse-MCP tunnel to
                        // Studio's declared `oab` server — the proof the declaration was
                        // consumed. If this never logs after "oab server declared", the
                        // gateway/agent runtime isn't tunnelling the reverse direction
                        // (upstream), which is why the agent sees no oab tools.
                        let _ = app.emit(
                            "app-log",
                            json!({ "level": "info", "msg": format!("remote: {agent} reverse-MCP — agent connected to the oab server") }),
                        );
                        if let Err(e) = send(&mut write, &acp::connect_reply(id, conn_id)).await {
                            outcome = Err(e);
                            break 'conn;
                        }
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
                        let app = app.clone();
                        let agent = agent.to_string();
                        tauri::async_runtime::spawn(async move {
                            // Capture the tool name before `params` is moved into the relay.
                            let tool = if method == "tools/call" {
                                params
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string()
                            } else {
                                String::new()
                            };
                            // The client's requested MCP protocol version (initialize only),
                            // captured before `params` moves so we can compare it to what we
                            // answer — a mismatch is the suspected reason a connected agent
                            // never reaches tools/list.
                            let client_proto = if method == "initialize" {
                                params
                                    .get("protocolVersion")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string()
                            } else {
                                String::new()
                            };
                            // `notifications/initialized` is a notification (no reply), so it
                            // never reaches the match below — log it here. Seeing it means the
                            // agent accepted our initialize and completed the handshake; its
                            // absence right after an initialize points at a rejected handshake.
                            if method == "notifications/initialized" {
                                let _ = app.emit(
                                    "app-log",
                                    json!({ "level": "info", "msg": format!("remote: {agent} reverse-MCP — client sent initialized (handshake complete)") }),
                                );
                            }
                            if let Some(reply) = handle_inner(&client, id, &method, params).await {
                                // Observability: surface each reverse-MCP call the agent made
                                // against the oab server, and its outcome. `tools/list` is the
                                // definitive "the agent pulled N oab tools" signal; a failing
                                // `tools/call` is the usual troubleshooting case. Absence of any
                                // of these (with no Connect either) means the declaration was
                                // never consumed upstream, not that Studio failed to serve.
                                let err = reply
                                    .get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(Value::as_str);
                                let (level, msg): (&str, String) = match method.as_str() {
                                    "tools/list" => match (
                                        err,
                                        reply
                                            .get("result")
                                            .and_then(|r| r.get("tools"))
                                            .and_then(Value::as_array),
                                    ) {
                                        (Some(e), _) => ("error", format!(
                                            "remote: {agent} reverse-MCP tools/list failed — {e}"
                                        )),
                                        (None, Some(list)) => ("info", format!(
                                            "remote: {agent} reverse-MCP tools/list — served {} oab tool(s)",
                                            list.len()
                                        )),
                                        (None, None) => ("warn", format!(
                                            "remote: {agent} reverse-MCP tools/list — served (unexpected shape)"
                                        )),
                                    },
                                    "tools/call" => {
                                        if let Some(e) = err {
                                            ("error", format!(
                                                "remote: {agent} reverse-MCP tools/call {tool} failed — {e}"
                                            ))
                                        } else if reply
                                            .get("result")
                                            .and_then(|r| r.get("isError"))
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false)
                                        {
                                            ("warn", format!(
                                                "remote: {agent} reverse-MCP tools/call {tool} → tool reported an error"
                                            ))
                                        } else {
                                            ("info", format!(
                                                "remote: {agent} reverse-MCP tools/call {tool} → ok"
                                            ))
                                        }
                                    }
                                    // The reverse-MCP inner handshake. Log the negotiated
                                    // protocol versions so a mismatch (the suspected cause of a
                                    // connected agent never listing tools) is visible.
                                    "initialize" => {
                                        let server_proto = reply
                                            .get("result")
                                            .and_then(|r| r.get("protocolVersion"))
                                            .and_then(Value::as_str)
                                            .unwrap_or("?");
                                        if client_proto != "?"
                                            && server_proto != "?"
                                            && client_proto != server_proto
                                        {
                                            ("warn", format!(
                                                "remote: {agent} reverse-MCP initialize — PROTOCOL MISMATCH: client requested {client_proto}, server answered {server_proto}"
                                            ))
                                        } else {
                                            ("info", format!(
                                                "remote: {agent} reverse-MCP initialize — client requested {client_proto}, server answered {server_proto}"
                                            ))
                                        }
                                    }
                                    other => ("warn", format!(
                                        "remote: {agent} reverse-MCP: unsupported method {other} (replied -32601)"
                                    )),
                                };
                                if !level.is_empty() {
                                    let _ = app.emit("app-log", json!({ "level": level, "msg": msg }));
                                }
                                let _ = reply_tx.send(reply);
                            }
                        });
                    }
                    Inbound::Disconnect { id, .. } => {
                        // The agent tore down the reverse-MCP tunnel to the oab server —
                        // its oab tools go away until it reconnects. Symmetric with the
                        // Connect log; explains a "tools vanished" without an error.
                        let _ = app.emit(
                            "app-log",
                            json!({ "level": "info", "msg": format!("remote: {agent} reverse-MCP — agent disconnected from the oab server") }),
                        );
                        if let Err(e) = send(&mut write, &acp::disconnect_reply(id)).await {
                            outcome = Err(e);
                            break 'conn;
                        }
                    }
                    // A piece of the agent's chat reply → forward to the panel.
                    Inbound::AgentChunk { text } => {
                        let _ = app.emit("agent-update", json!({ "agent": agent, "kind": "chunk", "text": text }));
                    }
                    Inbound::Cancel { .. } | Inbound::Other => {}
                }
            }
        }
    }

    // (item 3) The socket ended with a turn still in flight — its result will never
    // arrive here. The chat panel already closes the spinner off `remote-status`,
    // but record *why* the turn ended so it's visible in the Activity panel rather
    // than a silent drop. (`session/resume` on the next attempt may reattach to the
    // same server-side turn if the gateway keeps it within its grace window.)
    if pending_prompt.is_some() {
        let _ = app.emit(
            "app-log",
            json!({ "level": "warn", "msg": format!("remote: {agent} connection dropped with a turn in flight — turn abandoned") }),
        );
    }
    outcome
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
            // Echo the client's requested protocol version. Our initialize is a
            // thin shim over the already-initialized oab-mcp sidecar, and
            // tools/list·call are forwarded verbatim (version-agnostic), so a
            // newer client (observed: 2025-06-18) must not see a downgrade to a
            // pinned 2024-11-05 and then decline to enumerate tools. Fall back to
            // a known version only when the client omits one.
            let proto = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            acp::message_reply(
                i,
                json!({
                    "protocolVersion": proto,
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

#[cfg(test)]
mod tests {
    use super::*;

    // `write_registry_text` validates before it ever resolves a path or touches
    // the filesystem, so the reject paths are safe to exercise in a unit test
    // (a green run proves a bad edit never lands).
    #[test]
    fn write_registry_text_rejects_bad_toml() {
        let err = write_registry_text("this is = not valid toml [[[").unwrap_err();
        assert!(err.contains("invalid TOML"), "got: {err}");
    }

    #[test]
    fn write_registry_text_rejects_two_managements() {
        let toml = r#"
[[agent]]
name = "a"
url = "wss://a/acp"
token = "t"
management = true

[[agent]]
name = "b"
url = "wss://b/acp"
token = "t"
management = true
"#;
        let err = write_registry_text(toml).unwrap_err();
        assert!(err.contains("management"), "got: {err}");
    }

    #[test]
    fn write_registry_text_rejects_duplicate_names() {
        let toml = r#"
[[agent]]
name = "dup"
url = "wss://a/acp"
token = "t"

[[agent]]
name = "dup"
url = "wss://b/acp"
token = "t"
"#;
        let err = write_registry_text(toml).unwrap_err();
        assert!(err.contains("unique") || err.contains("duplicate"), "got: {err}");
    }
}
