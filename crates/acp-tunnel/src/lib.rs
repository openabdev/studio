//! Studio-side reverse-MCP-over-ACP: the `/acp` **session lifecycle** and tunnel
//! framing Studio speaks as an **ACP WebSocket client** (reverse-MCP client ADR,
//! Part B; upstream openab #1447 + the MCP-over-ACP tunnel contract).
//!
//! This crate is **pure protocol** — JSON in, JSON out, a small state machine —
//! with **no** transport, Tauri, or AWS dependency, so it is unit-tested in the
//! root workspace (`cargo test --workspace`) and reused by:
//!   - `src-tauri` — drives it over a real `tokio-tungstenite` `/acp` WS and
//!     relays tunnel tool calls to the running `oab-mcp` sidecar;
//!   - a future headless reverse-MCP client, if one is ever needed.
//!
//! **Scope of this module (slice 3): the session lifecycle** — dial-time framing
//! (the bearer sub-protocol), the `initialize` → `session/new` → (`session/resume`
//! on reconnect) handshake, and the `mcpServers` declaration that publishes
//! Studio's `oab-mcp` surface. The inbound tunnel dispatch (`mcp/connect` /
//! `mcp/message` / …) lands in slice 4.
//!
//! What is **contract-specified** (owned fully here): the `type:"acp"` server
//! declaration `{id, name}`, the JSON-RPC envelope, id correlation, method names,
//! and the [`limits`]. What is **standard ACP** and confirmed against the live
//! endpoint when the transport is wired: the exact `initialize` params — passed
//! through by the transport (see [`Session::initialize`]).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The ACP sub-protocol token the server echoes back on the `/acp` upgrade.
pub const ACP_SUBPROTOCOL: &str = "acp.v1";

/// The WebSocket `Sec-WebSocket-Protocol` offer for a bearer-authed `/acp` dial:
/// `openab.bearer.<token>, acp.v1` (tunnel contract §1). The server echoes only
/// `acp.v1`. The token is a secret — this returns it for the transport to place
/// in the handshake header; never log the result.
pub fn bearer_subprotocol(token: &str) -> String {
    format!("openab.bearer.{token}, {ACP_SUBPROTOCOL}")
}

/// Protocol limits Studio must respect (tunnel contract §7). The transport
/// enforces them; they live here so both the transport and its tests share one
/// source.
pub mod limits {
    /// One tunnelled `mcp/message` request; the server's default, under the ACP
    /// idle ceiling.
    pub const TUNNEL_REQUEST_TIMEOUT_SECS: u64 = 170;
    /// `mcp/connect` + the `initialize` that follows it.
    pub const CONNECT_HANDSHAKE_TIMEOUT_SECS: u64 = 30;
    /// `type:acp` entries accepted per `session/new`.
    pub const MAX_SERVERS_PER_SESSION: usize = 8;
    /// Any inbound frame; exceeding it closes the connection.
    pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
    /// A method-bearing frame (request or notification).
    pub const MAX_METHOD_FRAME_BYTES: usize = 1024 * 1024;
}

/// A `type:acp` MCP-server declaration Studio publishes on `session/new`
/// (tunnel contract §2). `id` is minted fresh **per connection** (a UUID, by the
/// transport) and used as the `acpId` in `mcp/connect`; `name` is **stable**
/// across reconnects and is what a tool prefix resolves by. For Studio there is
/// a single declaration, `name = "oab"` (reverse-MCP client ADR, D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerDecl {
    pub id: String,
    pub name: String,
}

impl ServerDecl {
    /// The wire form: `{ "type": "acp", "id": …, "name": … }`.
    pub fn to_json(&self) -> Value {
        json!({ "type": "acp", "id": self.id, "name": self.name })
    }
}

/// The single server Studio declares (D1): one `oab` server, fleet-parameterized
/// tools. `id` is the per-connection UUID the transport mints.
pub fn oab_server(connection_id: &str) -> ServerDecl {
    ServerDecl {
        id: connection_id.to_string(),
        name: "oab".to_string(),
    }
}

// ---- JSON-RPC 2.0 envelope helpers ------------------------------------------
// All `/acp` frames are JSON-RPC 2.0 (tunnel contract §1). Kept tiny and shared
// so request/response/notification framing is written once.

/// A JSON-RPC **request** (has an `id`, so a response is owed).
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A JSON-RPC **notification** (no `id`, fire-and-forget).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A successful JSON-RPC **response** to request `id`.
pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC **error** response to request `id`.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Where the session is in the `initialize → session/new` handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing sent yet (fresh, or after a disconnect).
    New,
    /// `initialize` sent, awaiting its result.
    Initializing,
    /// `initialize` acked; ready to open (or resume) a session.
    Initialized,
    /// A session exists; the `oab` server is declared and tools are reachable.
    SessionActive,
}

/// The client-side session state machine. It **produces** the outbound frames
/// (`initialize`, `session/new`, `session/resume`) and tracks the phase + the
/// declaration set; the transport owns the socket and feeds results back in.
/// One `Session` per logical connection; on reconnect the transport mints a new
/// connection id (hence a new [`ServerDecl::id`]) and calls [`Session::resume`].
#[derive(Debug, Clone)]
pub struct Session {
    servers: Vec<ServerDecl>,
    next_id: u64,
    session_id: Option<String>,
    phase: Phase,
}

impl Session {
    /// A fresh session that will declare `servers` (at most
    /// [`limits::MAX_SERVERS_PER_SESSION`]; Studio declares exactly one).
    pub fn new(servers: Vec<ServerDecl>) -> Self {
        Session {
            servers,
            next_id: 1,
            session_id: None,
            phase: Phase::New,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The declared servers in wire form — the `mcpServers` array reused by
    /// `session/new` and every `session/resume`.
    pub fn declarations(&self) -> Vec<Value> {
        self.servers.iter().map(ServerDecl::to_json).collect()
    }

    /// Allocate the next JSON-RPC request id (monotonic per session).
    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Build the `initialize` request and advance to [`Phase::Initializing`].
    /// `params` is the standard-ACP initialize payload (protocol version /
    /// client capabilities) the transport supplies against the ACP version in
    /// use — this crate owns the envelope + id + phase, not the ACP schema.
    /// Returns `(id, frame)`.
    pub fn initialize(&mut self, params: Value) -> (u64, Value) {
        let id = self.alloc_id();
        self.phase = Phase::Initializing;
        (id, request(id, "initialize", params))
    }

    /// Record a successful `initialize` result and advance to
    /// [`Phase::Initialized`].
    pub fn on_initialized(&mut self) {
        self.phase = Phase::Initialized;
    }

    /// Build the `session/new` request that **declares** Studio's servers
    /// (tunnel contract §2). Returns `(id, frame)`.
    pub fn open_session(&mut self, cwd: &str) -> (u64, Value) {
        let id = self.alloc_id();
        let frame = request(
            id,
            "session/new",
            json!({ "cwd": cwd, "mcpServers": self.declarations() }),
        );
        (id, frame)
    }

    /// Record the created session id and advance to [`Phase::SessionActive`].
    pub fn on_session_created(&mut self, session_id: impl Into<String>) {
        self.session_id = Some(session_id.into());
        self.phase = Phase::SessionActive;
    }

    /// Build the `session/resume` request after a reconnect. A resume
    /// **re-presents the whole declaration set** — anything omitted is withdrawn
    /// (tunnel contract §7) — so the transport must have refreshed the per-
    /// connection [`ServerDecl::id`]s (via [`Session::redeclare`]) first. Returns
    /// `None` if there is no session to resume yet.
    pub fn resume(&mut self, cwd: &str) -> Option<(u64, Value)> {
        let session_id = self.session_id.clone()?;
        let id = self.alloc_id();
        let frame = request(
            id,
            "session/resume",
            json!({
                "sessionId": session_id,
                "cwd": cwd,
                "mcpServers": self.declarations(),
            }),
        );
        Some((id, frame))
    }

    /// Replace the declaration set — used on reconnect to swap in fresh per-
    /// connection server ids before a [`Session::resume`], and to reset the phase
    /// to [`Phase::New`] so a full `initialize` runs on the new socket.
    pub fn redeclare(&mut self, servers: Vec<ServerDecl>) {
        self.servers = servers;
        self.phase = Phase::New;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_subprotocol_pairs_the_token_with_acp_v1() {
        assert_eq!(
            bearer_subprotocol("tok123"),
            "openab.bearer.tok123, acp.v1"
        );
    }

    #[test]
    fn server_decl_wire_form_is_type_acp() {
        let d = oab_server("conn-uuid");
        assert_eq!(
            d.to_json(),
            json!({ "type": "acp", "id": "conn-uuid", "name": "oab" })
        );
    }

    #[test]
    fn envelopes_match_jsonrpc_2_0() {
        assert_eq!(
            request(7, "initialize", json!({})),
            json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize", "params": {} })
        );
        // a notification carries no id
        let n = notification("notifications/initialized", json!({}));
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "notifications/initialized");
        assert!(n.get("id").is_none());
        assert_eq!(
            response(json!(3), json!({ "ok": true })),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "ok": true } })
        );
        assert_eq!(
            error_response(json!(3), -32601, "nope")["error"]["code"],
            json!(-32601)
        );
    }

    #[test]
    fn lifecycle_walks_new_to_session_active_with_monotonic_ids() {
        let mut s = Session::new(vec![oab_server("c1")]);
        assert_eq!(s.phase(), Phase::New);

        let (init_id, init) = s.initialize(json!({ "protocolVersion": "x" }));
        assert_eq!(init_id, 1);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["protocolVersion"], "x");
        assert_eq!(s.phase(), Phase::Initializing);

        s.on_initialized();
        assert_eq!(s.phase(), Phase::Initialized);

        let (new_id, frame) = s.open_session("/work");
        assert_eq!(new_id, 2); // monotonic
        assert_eq!(frame["method"], "session/new");
        assert_eq!(frame["params"]["cwd"], "/work");
        // the oab server is declared, in wire form
        assert_eq!(
            frame["params"]["mcpServers"],
            json!([{ "type": "acp", "id": "c1", "name": "oab" }])
        );

        s.on_session_created("sess-42");
        assert_eq!(s.phase(), Phase::SessionActive);
        assert_eq!(s.session_id(), Some("sess-42"));
    }

    #[test]
    fn resume_redeclares_the_whole_set_with_the_fresh_connection_id() {
        let mut s = Session::new(vec![oab_server("c1")]);
        s.initialize(json!({}));
        s.on_initialized();
        s.open_session("/w");
        s.on_session_created("sess-1");

        // reconnect: fresh per-connection id, phase resets so initialize re-runs
        s.redeclare(vec![oab_server("c2")]);
        assert_eq!(s.phase(), Phase::New);
        s.initialize(json!({}));
        s.on_initialized();

        let (_id, frame) = s.resume("/w").expect("a session exists to resume");
        assert_eq!(frame["method"], "session/resume");
        assert_eq!(frame["params"]["sessionId"], "sess-1");
        // the whole set is re-presented, with the NEW connection id
        assert_eq!(
            frame["params"]["mcpServers"],
            json!([{ "type": "acp", "id": "c2", "name": "oab" }])
        );
    }

    #[test]
    fn resume_is_none_before_a_session_exists() {
        let mut s = Session::new(vec![oab_server("c1")]);
        assert!(s.resume("/w").is_none());
    }

    #[test]
    fn studios_single_declaration_is_within_the_server_cap() {
        let s = Session::new(vec![oab_server("c1")]);
        assert!(s.declarations().len() <= limits::MAX_SERVERS_PER_SESSION);
    }
}
