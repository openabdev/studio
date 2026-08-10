//! Persisted configuration for the `oab-mcp` sidecar's connection target.
//!
//! The desktop used to spawn the core with nothing but `OAB_CLUSTER`, so the
//! sidecar inherited whatever AWS credentials/region the host's *ambient*
//! default chain happened to resolve — which silently pointed Studio at the
//! wrong account/region. This module lets the user pin the target explicitly
//! (profile / region / cluster) and persists it to the app config dir, so each
//! oab-mcp instance is deterministically bound instead of drifting.
//!
//! v1 is a single target. The shape is intentionally room-to-grow toward a
//! multi-fleet registry (provider-tagged connections) — see the config-tab ADR.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;

/// The connection target for the `oab-mcp` sidecar. `profile` / `region` are
/// optional: left unset, the sidecar falls back to the host default chain (the
/// pre-config behaviour), which is exactly the drift this feature closes — so
/// the UI nudges the user to set them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpConfig {
    /// ECS cluster name, passed through as `OAB_CLUSTER`.
    pub cluster: String,
    /// Named AWS profile (`~/.aws/config`), exported as `AWS_PROFILE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// AWS region, exported as `AWS_REGION` + `AWS_DEFAULT_REGION`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl Default for McpConfig {
    /// First-run defaults seed from the process env so behaviour is unchanged
    /// until the user saves an explicit target.
    fn default() -> Self {
        let non_empty = |s: String| Some(s).filter(|v| !v.is_empty());
        Self {
            cluster: std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string()),
            profile: std::env::var("AWS_PROFILE").ok().and_then(non_empty),
            region: std::env::var("AWS_REGION")
                .ok()
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .and_then(non_empty),
        }
    }
}

fn config_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    Ok(dir.join("mcp-config.json"))
}

/// Load the persisted config, or the env-seeded default if absent/unreadable.
/// Never fails: a corrupt file falls back to default rather than blocking boot.
pub fn load<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> McpConfig {
    let Ok(path) = config_path(app) else {
        return McpConfig::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => McpConfig::default(),
    }
}

/// Persist the config as pretty JSON, creating the config dir if needed.
pub fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, cfg: &McpConfig) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write config {}: {e}", path.display()))
}
