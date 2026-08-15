//! Persisted, **provider-tagged** connection target for the `oab-mcp` sidecar,
//! plus the **hermetic** child env used to spawn it.
//!
//! The desktop used to spawn the core with nothing but `OAB_CLUSTER`, so the
//! sidecar inherited whatever AWS credentials/region the host's *ambient* default
//! chain resolved — silently pointing Studio at the wrong account/region (the
//! `AccessDenied` drift). Two design choices close that:
//!
//! - **Provider-tagged.** The target is an enum (`Ecs` today; k8s etc. add a
//!   variant). The spawn path is provider-agnostic — it just asks the target for
//!   its env — so a new runtime is a new variant + its own `hermetic_env` arm, not
//!   a rewrite.
//! - **Hermetic env.** The child env is built **from empty**: only a small
//!   base allow-list of system vars is carried over (if present), then the target
//!   injects exactly its own vars. No ambient credential/region can leak in, and
//!   one provider's vars can never bleed into another provider's sidecar (a plain
//!   "strip `AWS_*`" blacklist would still leak a stale `KUBECONFIG`).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// Provider-tagged connection target. One variant today (ECS); adding k8s is a new
/// variant + its own `hermetic_env` arm — nothing else on the spawn path changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum McpTarget {
    /// AWS ECS. `profile` / `region` are optional; left unset the sidecar gets
    /// **no** AWS credentials in its (hermetic) env, which surfaces as an explicit
    /// auth error rather than silently drifting onto an ambient identity — exactly
    /// the drift this exists to close, so the UI nudges the user to set them.
    Ecs {
        cluster: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<String>,
    },
}

/// System env keys every sidecar needs regardless of provider, carried over from
/// the parent **only if present**. Everything else is dropped — this is what makes
/// the env hermetic: no ambient `AWS_*` / `KUBECONFIG` / credential var leaks in.
const BASE_ENV_ALLOW: &[&str] = &[
    "HOME",          // AWS SDK resolves ~/.aws/{config,credentials} via HOME
    "PATH",          // dylib / helper resolution
    "TMPDIR",        // temp files (macOS)
    "TZ",            // timestamps
    "LANG",          // locale
    "LC_ALL",        // locale
    "LC_CTYPE",      // locale
    "SSL_CERT_FILE", // TLS trust, if the host pins one
    "SSL_CERT_DIR",  // TLS trust, if the host pins one
];

impl McpTarget {
    /// First-run default: seed from the process env so behaviour is unchanged
    /// until the user saves an explicit target.
    pub fn env_seeded_default() -> Self {
        let non_empty = |s: String| Some(s).filter(|v| !v.is_empty());
        McpTarget::Ecs {
            cluster: std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".to_string()),
            profile: std::env::var("AWS_PROFILE").ok().and_then(non_empty),
            region: std::env::var("AWS_REGION")
                .ok()
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .and_then(non_empty),
        }
    }

    /// The cluster the roster / tool calls target.
    pub fn cluster(&self) -> &str {
        match self {
            McpTarget::Ecs { cluster, .. } => cluster,
        }
    }

    /// Build the sidecar's child env **from empty** (hermetic): carry only the base
    /// allow-list that exists in the parent, then inject exactly this provider's
    /// target vars. No ambient credential/region leaks; no cross-provider bleed.
    ///
    /// NOTE (desktop-only): the base list is a *desktop* app's needs. If this ever
    /// runs in a container, a k8s/EKS variant must also allow the container-cred
    /// vars its auth needs (e.g. `AWS_CONTAINER_CREDENTIALS_*` for EKS exec auth) —
    /// declared explicitly in that variant's arm, never inherited by accident.
    pub fn hermetic_env(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = BASE_ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v)))
            .collect();
        match self {
            McpTarget::Ecs {
                cluster,
                profile,
                region,
            } => {
                env.insert("OAB_CLUSTER".into(), cluster.clone());
                if let Some(p) = profile.as_deref().filter(|s| !s.is_empty()) {
                    env.insert("AWS_PROFILE".into(), p.to_string());
                }
                if let Some(r) = region.as_deref().filter(|s| !s.is_empty()) {
                    env.insert("AWS_REGION".into(), r.to_string());
                    env.insert("AWS_DEFAULT_REGION".into(), r.to_string());
                }
            }
        }
        env
    }
}

fn config_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    Ok(dir.join("mcp-target.json"))
}

/// Load the persisted target, or the env-seeded default if absent / unreadable.
/// Never fails: a corrupt file falls back to default rather than blocking boot.
pub fn load<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> McpTarget {
    let Ok(path) = config_path(app) else {
        return McpTarget::env_seeded_default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| McpTarget::env_seeded_default()),
        Err(_) => McpTarget::env_seeded_default(),
    }
}

/// Persist the target (creating the config dir if needed), validating it
/// round-trips through JSON first.
pub fn save<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &McpTarget,
) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create config dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(target).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}
