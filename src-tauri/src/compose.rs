//! The **authoring + preview** side of the agent-deployment ADR (slice 1):
//! persist the operator's template/overlay/skills [`Library`] and compose a
//! preview of a concrete agent bundle from it.
//!
//! This is Studio-local and provider-neutral: composing produces a
//! `{path → bytes}` bundle ([`studio_compose::Bundle`]) and nothing more — no S3,
//! no ECS, no build. The provider drivers that land a bundle on a runtime's file
//! carrier are slice 2+. Persistence mirrors `config.rs`: one JSON document under
//! the app config dir, best-effort load (a corrupt/absent file → empty library so
//! the panel still opens), validated write.

use std::path::PathBuf;

use studio_compose::{compose_named, BundlePreview, Library};
use tauri::Manager;

fn library_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    Ok(dir.join("compose-library.json"))
}

/// Load the persisted library, or an empty one if absent / unreadable / corrupt.
/// Never fails: like `config::load`, a bad file falls back rather than blocking
/// the panel.
pub fn load<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Library {
    let Ok(path) = library_path(app) else {
        return Library::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Library::default(),
    }
}

/// Persist the library (creating the config dir if needed), pretty-printed so the
/// on-disk document is diff-friendly.
pub fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, library: &Library) -> Result<(), String> {
    let path = library_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create config dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(library).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Compose a preview of `template ⊕ overlay` from the given (possibly unsaved)
/// library. Pure passthrough to [`compose_named`]; the compose error's `Display`
/// is the operator-facing message (unknown template/overlay/skill, missing tag).
pub fn preview(
    library: &Library,
    template: &str,
    overlay: Option<&str>,
) -> Result<BundlePreview, String> {
    compose_named(library, template, overlay)
        .map(|bundle| bundle.preview())
        .map_err(|e| e.to_string())
}
