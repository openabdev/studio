//! studio#135 follow-up — `write_local_agent_config` mirrors a
//! wizard-generated config.toml to `<folder>/<name>/config.toml` for any
//! `deploy_provision_agent` caller (the console wizard *or* a direct
//! MCP/"admin agent" call — the tool's own doc comment names both). `name`
//! is caller-controlled and becomes a directory under the operator's Config
//! folder, so it must be a single path component: `..`, embedded
//! separators, or an absolute path would otherwise write `config.toml`
//! anywhere the sidecar can reach, outside the folder it promised.

use std::path::{Path, PathBuf};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oab-local-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn folder_str(dir: &Path) -> &str {
    dir.to_str().unwrap()
}

#[test]
fn plain_name_writes_config_toml_under_the_folder() {
    let base = scratch("plain");
    let folder = base.join("folder");
    studio_cp::write_local_agent_config(folder_str(&folder), "hera", b"[agent]\n").unwrap();
    assert_eq!(
        std::fs::read(folder.join("hera").join("config.toml")).unwrap(),
        b"[agent]\n"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn refuses_names_that_escape_the_config_folder() {
    let base = scratch("escape");
    let folder = base.join("folder");
    for name in ["..", "../escape", "a/../b", "a/b", "a\\b", "/abs", "", "."] {
        let result = studio_cp::write_local_agent_config(folder_str(&folder), name, b"x");
        assert!(result.is_err(), "name {name:?} must be refused");
    }
    // Nothing may have been written — neither outside the folder…
    assert!(!base.join("escape").join("config.toml").exists());
    assert!(!base.join("config.toml").exists());
    // …nor inside it: a refused name is a refused write, not a partial one.
    if folder.exists() {
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }
    let _ = std::fs::remove_dir_all(&base);
}
