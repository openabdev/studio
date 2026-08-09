//! Minimal MCP stdio client for the bundled `oab-mcp` sidecar.
//!
//! The desktop hosts the control-plane **core** as a child process: the skin
//! reaches the core over the MCP contract (`initialize` → `tools/call`), not via
//! an in-process crate link. We spawn `oab-mcp` once at startup, run the
//! handshake, then forward tool calls, correlating responses by JSON-RPC id.
//!
//! Transport is newline-delimited JSON-RPC on the sidecar's stdio (rmcp's stdio
//! server). One shared child; requests are multiplexed by a monotonic id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::async_runtime;
use tokio::sync::{oneshot, Mutex};

use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// A live connection to the `oab-mcp` sidecar. Cheap to clone (shared state
/// behind an `Arc`), so it can live in Tauri managed state.
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Inner>,
}

struct Inner {
    child: Mutex<CommandChild>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn the sidecar, wire up the stdout reader, and complete the MCP
    /// handshake. `cluster` is passed through as `OAB_CLUSTER` so the core
    /// defaults match the desktop's.
    pub async fn spawn<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        cluster: &str,
    ) -> Result<Self, String> {
        let (mut rx, child) = app
            .shell()
            .sidecar("oab-mcp")
            .map_err(|e| format!("locate oab-mcp sidecar: {e}"))?
            .env("OAB_CLUSTER", cluster)
            .spawn()
            .map_err(|e| format!("spawn oab-mcp: {e}"))?;

        let inner = Arc::new(Inner {
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        });

        // stdout reader: bytes may arrive un-framed, so buffer and split on '\n',
        // then resolve any pending request whose id matches.
        let reader = inner.clone();
        async_runtime::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        buf.extend_from_slice(&bytes);
                        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=nl).collect();
                            let line = &line[..line.len() - 1];
                            if line.is_empty() {
                                continue;
                            }
                            if let Ok(msg) = serde_json::from_slice::<Value>(line) {
                                if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                                    if let Some(tx) = reader.pending.lock().await.remove(&id) {
                                        let _ = tx.send(msg);
                                    }
                                }
                            }
                        }
                    }
                    CommandEvent::Terminated(_) => break,
                    _ => {}
                }
            }
            // Drain pending waiters so callers fail fast instead of hanging.
            reader.pending.lock().await.clear();
        });

        let client = McpClient { inner };
        client.handshake().await?;
        Ok(client)
    }

    async fn send(&self, msg: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.inner
            .child
            .lock()
            .await
            .write(&line)
            .map_err(|e| format!("write to oab-mcp: {e}"))
    }

    /// Send a request and await the correlated `result` (or a formatted error).
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let msg = rx
            .await
            .map_err(|_| "oab-mcp closed before responding".to_string())?;
        if let Some(err) = msg.get("error") {
            return Err(format!("oab-mcp error: {err}"));
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method })).await
    }

    async fn handshake(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "studio-desktop", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;
        self.notify("notifications/initialized").await?;
        Ok(())
    }

    /// Call an MCP tool and decode the JSON payload the server wraps in
    /// `result.content[0].text` (rmcp `CallToolResult::success`).
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let result = self
            .request("tools/call", json!({ "name": name, "arguments": arguments }))
            .await?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name}: unexpected tool result shape: {result}"))?;
        serde_json::from_str::<Value>(text).map_err(|e| format!("{name}: decode payload: {e}"))
    }
}
